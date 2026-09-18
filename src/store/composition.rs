// SPDX-License-Identifier: Apache-2.0
use super::{Store, ToolStore};
use crate::{Error, Result, composition::*, core::*};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
impl Store {
    pub fn pin_composition_component(
        &self,
        component: &ComposableArtifactRef,
    ) -> Result<ComponentRevision> {
        let (revision, body) = match component {
            ComposableArtifactRef::Tool(id) => {
                let t = self.tool_definition(id)?;
                if t.disabled {
                    return Err(Error::Intervention("Composition tool is disabled".into()));
                }
                (t.version.clone(), serde_json::to_value(t)?)
            }
            ComposableArtifactRef::Skill(id) => {
                let s = self.skill(&id.to_string())?;
                let r = self
                    .skill_revisions(id)?
                    .into_iter()
                    .last()
                    .ok_or_else(|| {
                        Error::InvalidInput("Skill requires a stored procedure revision".into())
                    })?;
                if s.maturity == crate::curriculum::SkillMaturity::Retired {
                    return Err(Error::Intervention("Composition Skill is retired".into()));
                }
                (r.revision.to_string(), serde_json::to_value(r)?)
            }
            ComposableArtifactRef::Recovery(id) => {
                let r = self.recovery(id)?;
                if r.status == crate::resilience::RecoveryStatus::Retired {
                    return Err(Error::Intervention(
                        "Composition Recovery is retired".into(),
                    ));
                }
                (r.version.to_string(), serde_json::to_value(r)?)
            }
            ComposableArtifactRef::EffectPlan(id) => {
                let p = self.effect_plan(id)?;
                (composition_hash(&p)?, serde_json::to_value(p)?)
            }
        };
        Ok(ComponentRevision {
            component: component.clone(),
            revision,
            content_hash: composition_hash(&body)?,
        })
    }
    pub fn save_composition(&self, c: &Composition) -> Result<()> {
        ordered_steps(c)?;
        if !matches!(
            c.maturity,
            CompositionMaturity::Candidate | CompositionMaturity::Testable
        ) || !c.evidence.is_empty()
        {
            return Err(Error::InvalidInput("Definition import starts Candidate/Testable without self-assigned evidence or maturity".into()));
        }
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        for step in &c.steps {
            let current = self.pin_composition_component(&step.component.component)?;
            if composition_hash(&current)? != composition_hash(&step.component)? {
                return Err(Error::InvalidInput(
                    "Component revision is not current; explicitly rebase and test".into(),
                ));
            }
        }
        for plan in &c.recoveries {
            for recovery in &plan.recoveries {
                let pin = self.pin_composition_component(&ComposableArtifactRef::Recovery(
                    recovery.recovery.clone(),
                ))?;
                if pin.revision != recovery.revision.to_string() {
                    return Err(Error::InvalidInput("Recovery plan revision changed".into()));
                }
                tx.execute("INSERT INTO composition_components(key,data) VALUES(?1,?2) ON CONFLICT DO NOTHING", params![serde_json::to_string(&pin)?, serde_json::to_string(&self.current_composition_component_body(&pin.component)?)?])?;
            }
        }
        let rev = i64::try_from(c.revision)
            .map_err(|_| Error::InvalidInput("Composition revision overflow".into()))?;
        let data = serde_json::to_string(c)?;
        let n=tx.execute("INSERT INTO compositions(id,revision,name,data) VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,name=excluded.name,data=excluded.data WHERE compositions.revision=excluded.revision-1",params![c.id.to_string(),rev,c.name,data])?;
        if n != 1 {
            return Err(Error::InvalidInput(
                "Composition revision must advance exactly once".into(),
            ));
        }
        tx.execute(
            "INSERT INTO composition_revisions(id,revision,hash,data) VALUES(?1,?2,?3,?4)",
            params![c.id.to_string(), rev, composition_hash(c)?, data],
        )?;
        for step in &c.steps {
            let key = serde_json::to_string(&step.component)?;
            tx.execute(
                "INSERT INTO composition_components(key,data) VALUES(?1,?2) ON CONFLICT DO NOTHING",
                params![
                    key,
                    serde_json::to_string(
                        &self.current_composition_component_body(&step.component.component)?
                    )?
                ],
            )?;
        }
        self.composition_event(&c.id, "composition_revision_created", c)?;
        tx.commit()?;
        Ok(())
    }
    pub fn composition(&self, id_or_name: &str) -> Result<Composition> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM compositions WHERE id=?1 OR name=?1",
                [id_or_name],
                |r| r.get(0),
            )
            .optional()?;
        Ok(serde_json::from_str(&data.ok_or_else(|| {
            Error::NotFound("Composition not found".into())
        })?)?)
    }
    pub fn compositions(&self) -> Result<Vec<Composition>> {
        self.connection
            .prepare("SELECT data FROM compositions ORDER BY name")?
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub fn composition_revision(&self, id: &CompositionId, revision: u64) -> Result<Composition> {
        let (hash, data): (String, String) = self.connection.query_row(
            "SELECT hash,data FROM composition_revisions WHERE id=?1 AND revision=?2",
            params![
                id.to_string(),
                i64::try_from(revision)
                    .map_err(|_| Error::InvalidInput("Revision overflow".into()))?
            ],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let c: Composition = serde_json::from_str(&data)?;
        if composition_hash(&c)? != hash {
            return Err(Error::InvalidInput(
                "Composition revision integrity mismatch".into(),
            ));
        }
        Ok(c)
    }
    pub fn composition_event(
        &self,
        id: &CompositionId,
        kind: &str,
        value: &impl serde::Serialize,
    ) -> Result<()> {
        self.connection.execute(
            "INSERT INTO composition_events(composition,kind,data) VALUES(?1,?2,?3)",
            params![id.to_string(), kind, serde_json::to_string(value)?],
        )?;
        Ok(())
    }
    pub fn composition_history(&self, id: &CompositionId) -> Result<Vec<serde_json::Value>> {
        self.connection.prepare("SELECT kind,data,created_at FROM composition_events WHERE composition=?1 ORDER BY id")?.query_map([id.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.map(|r|{let(kind,data,at)=r?;Ok(serde_json::json!({"kind":kind,"data":serde_json::from_str::<serde_json::Value>(&data)?,"created_at":at}))}).collect()
    }
    pub fn composition_evidence(&self, id: &CompositionId) -> Result<Vec<CompositionEvidence>> {
        self.connection
            .prepare("SELECT data FROM composition_evidence WHERE composition=?1 ORDER BY rowid")?
            .query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub(crate) fn save_composition_evidence(&self, e: &CompositionEvidence) -> Result<()> {
        let c = self.composition_revision(&e.composition, e.composition_revision)?;
        if e.step_results.iter().any(|r| {
            !c.steps.iter().any(|s| {
                s.id == r.step
                    && composition_hash(&s.component).ok() == composition_hash(&r.component).ok()
            })
        }) {
            return Err(Error::InvalidInput(
                "Composition evidence component mismatch".into(),
            ));
        }
        self.connection.execute(
            "INSERT INTO composition_evidence(id,composition,revision,data) VALUES(?1,?2,?3,?4)",
            params![
                e.id.to_string(),
                e.composition.to_string(),
                i64::try_from(e.composition_revision)
                    .map_err(|_| Error::InvalidInput("Revision overflow".into()))?,
                serde_json::to_string(e)?
            ],
        )?;
        self.record_composition_epistemic(e)?;
        self.composition_event(&e.composition, "composition_evaluated", e)
    }
    pub fn composition_dependency_health(
        &self,
        c: &Composition,
    ) -> Result<CompositionDependencyHealth> {
        let mut components = c
            .steps
            .iter()
            .map(|step| {
                let (status, reason) =
                    match self.pin_composition_component(&step.component.component) {
                        Ok(current)
                            if composition_hash(&current).ok()
                                == composition_hash(&step.component).ok() =>
                        {
                            (
                                CompositionHealthStatus::Healthy,
                                "Exact component revision unchanged",
                            )
                        }
                        Ok(_) => (
                            CompositionHealthStatus::RevalidationRequired,
                            "Component revision changed",
                        ),
                        Err(_) => (
                            CompositionHealthStatus::Broken,
                            "Component unavailable or retired",
                        ),
                    };
                ComponentHealth {
                    component: step.component.clone(),
                    status,
                    reason: reason.into(),
                }
            })
            .collect::<Vec<_>>();
        for plan in &c.recoveries {
            for recovery in &plan.recoveries {
                let reference = ComposableArtifactRef::Recovery(recovery.recovery.clone());
                let pin = self.pin_composition_component(&reference);
                let status = match &pin {
                    Ok(p) if p.revision == recovery.revision.to_string() => {
                        CompositionHealthStatus::Healthy
                    }
                    Ok(_) => CompositionHealthStatus::RevalidationRequired,
                    Err(_) => CompositionHealthStatus::Broken,
                };
                components.push(ComponentHealth {
                    component: pin.unwrap_or(ComponentRevision {
                        component: reference,
                        revision: recovery.revision.to_string(),
                        content_hash: String::new(),
                    }),
                    status,
                    reason: "Composition Recovery dependency revision and retirement check".into(),
                });
            }
        }
        let status = if components
            .iter()
            .any(|c| c.status == CompositionHealthStatus::Broken)
        {
            CompositionHealthStatus::Broken
        } else if components
            .iter()
            .any(|c| c.status != CompositionHealthStatus::Healthy)
        {
            CompositionHealthStatus::RevalidationRequired
        } else {
            CompositionHealthStatus::Healthy
        };
        Ok(CompositionDependencyHealth { components, status })
    }
    pub fn composition_maturity(&self, c: &Composition) -> Result<CompositionMaturity> {
        if self.composition(&c.id.to_string())?.revision != c.revision
            || self.composition_dependency_health(c)?.status != CompositionHealthStatus::Healthy
        {
            return Ok(CompositionMaturity::Stale);
        }
        let evidence = self
            .composition_evidence(&c.id)?
            .into_iter()
            .filter(|e| {
                e.composition_revision == c.revision
                    && e.experiment_quality == crate::experimentation::ExperimentQuality::Controlled
            })
            .collect::<Vec<_>>();
        if evidence
            .iter()
            .any(|e| e.outcome == CompositionOutcome::Fail)
        {
            return Ok(CompositionMaturity::Degraded);
        }
        if evidence
            .iter()
            .any(|e| e.outcome == CompositionOutcome::Pass)
        {
            if self.composition_assurance(c)?["satisfied"] == true && self.connection.query_row("SELECT EXISTS(SELECT 1 FROM composite_skills WHERE composition=?1 AND revision=?2)", params![c.id.to_string(), i64::try_from(c.revision).map_err(|_| Error::InvalidInput("Revision overflow".into()))?], |r| r.get::<_,bool>(0))? { Ok(CompositionMaturity::Validated) } else { Ok(CompositionMaturity::Supported) }
        } else {
            Ok(c.maturity)
        }
    }
}

impl Store {
    pub fn assess_composition_step(
        &self,
        r: &CompositionRuntimeContext,
    ) -> Result<CompositionNextStepAssessment> {
        let c = self.composition_revision(&r.composition, r.revision)?;
        ordered_steps(&c)?;
        let current = self.composition(&r.composition.to_string())?;
        let health = self.composition_dependency_health(&c)?;
        let mut findings = vec![];
        let mut unknown = vec![];
        let mut active = vec![];
        if current.revision != r.revision || health.status != CompositionHealthStatus::Healthy {
            findings.push("Composition or component changed; revalidation required".into());
        }
        let step = c
            .steps
            .iter()
            .find(|s| s.id == r.step)
            .ok_or_else(|| Error::InvalidInput("Next composition step is missing".into()))?;
        if r.completed_steps.contains(&r.step)
            || r.completed_steps
                .iter()
                .any(|id| !c.steps.iter().any(|s| s.id == *id))
        {
            return Err(Error::InvalidInput(
                "Invalid completed composition steps".into(),
            ));
        }
        for relation in c
            .relations
            .iter()
            .filter(|e| e.to == r.step && e.kind.orders())
        {
            if !r.completed_steps.contains(&relation.from) {
                findings.push(format!(
                    "Required predecessor {} has not completed",
                    relation.from
                ));
            }
        }
        let now = chrono::Utc::now();
        let state = trusted_state(&r.state, &r.external_versions, now);
        for binding in &step.input_bindings {
            if let Some(from) = &binding.from {
                let handoff = r
                    .current_handoffs
                    .iter()
                    .find(|h| &h.from == from && h.to == r.step);
                if !handoff.is_some_and(|h| {
                    handoff_fresh(
                        h,
                        &StateFreshnessRequirement::BeforeNextMutation,
                        &r.step,
                        &r.external_versions,
                        now,
                    )
                }) {
                    unknown.push(format!(
                        "Refresh handoff {} before next mutation",
                        binding.key
                    ));
                }
            }
        }
        for reference in step.preconditions.iter().chain(&step.invariants) {
            if self.operational_revision(&reference.into()).is_err() {
                unknown.push(format!(
                    "Required operational revision {} is unavailable",
                    reference.id
                ));
            }
        }
        let conditions = c
            .cross_step_preconditions
            .iter()
            .filter(|p| p.required_by == r.step)
            .map(|p| &p.condition)
            .chain(
                c.assumptions
                    .iter()
                    .filter(|a| a.owner == step.component.component)
                    .map(|a| &a.condition),
            );
        for condition in conditions {
            match condition_value(condition, &state) {
                Some(true) => {}
                Some(false) => {
                    findings.push("Cross-step precondition or assumption violated".into())
                }
                None => {
                    unknown.push("Cross-step precondition needs authoritative observation".into())
                }
            }
        }
        for invariant in &c.contract.sequence_invariants {
            if invariant_active(
                &c,
                invariant,
                &r.step,
                SequenceEvaluationPhase::BeforeStep,
                &r.completed_steps,
                &r.commit_state,
            ) {
                active.push(invariant.id.clone());
                match condition_value(&invariant.condition, &state) {
                    Some(true) => {}
                    Some(false) => {
                        findings.push(format!("Sequence invariant {} violated", invariant.id))
                    }
                    None => unknown.push(format!("Sequence invariant {} unknown", invariant.id)),
                }
            }
        }
        Ok(CompositionNextStepAssessment {
            composition: r.composition.clone(),
            revision: r.revision,
            step: r.step.clone(),
            findings,
            unknown,
            active_invariants: active,
            component_health: health.status,
        })
    }
    pub fn attach_composition_knowledge(
        &self,
        context: &mut crate::runtime::RuntimeDecisionContext,
    ) -> Result<()> {
        let Some(composition) = context.composition.clone() else {
            context.composition_assessment = None;
            return Ok(());
        };
        self.validate_composition_responsibility(context, &composition)?;
        let state = trusted_state(
            &composition.state,
            &composition.external_versions,
            chrono::Utc::now(),
        );
        for (key, value) in state.values {
            context.context_observations.insert(
                key,
                vec![crate::knowledge_runtime::ContextValue {
                    value,
                    source: crate::knowledge_runtime::ContextValueSource::RuntimeObserved,
                }],
            );
        }
        context.context_observations.insert(
            "composition_id".into(),
            vec![crate::knowledge_runtime::ContextValue {
                value: crate::hierarchy::ScopeValue::String(composition.composition.to_string()),
                source: crate::knowledge_runtime::ContextValueSource::RuntimeObserved,
            }],
        );
        let c = self.composition_revision(&composition.composition, composition.revision)?;
        let state = trusted_state(
            &composition.state,
            &composition.external_versions,
            chrono::Utc::now(),
        );
        for invariant in &c.contract.sequence_invariants {
            let active = invariant_active(
                &c,
                invariant,
                &composition.step,
                SequenceEvaluationPhase::BeforeStep,
                &composition.completed_steps,
                &composition.commit_state,
            );
            if let Some(satisfied) = condition_value(&invariant.condition, &state) {
                context.context_observations.insert(
                    format!("violation.{}", invariant.id),
                    vec![crate::knowledge_runtime::ContextValue {
                        value: crate::hierarchy::ScopeValue::Boolean(active && !satisfied),
                        source: crate::knowledge_runtime::ContextValueSource::RuntimeObserved,
                    }],
                );
            }
        }
        context.composition_assessment = Some(self.assess_composition_step(&composition)?);
        Ok(())
    }
}

impl Store {
    pub fn composition_execution_tool(
        &self,
        step: &CompositionStep,
    ) -> Result<(ToolId, Option<String>)> {
        let current = self.pin_composition_component(&step.component.component)?;
        if composition_hash(&current)? != composition_hash(&step.component)? {
            return Err(Error::Intervention(
                "Component changed before execution".into(),
            ));
        }
        let commands = match &step.component.component {
            ComposableArtifactRef::Tool(id) => return Ok((id.clone(), None)),
            ComposableArtifactRef::Skill(id) => {
                let revision = self
                    .skill_revisions(id)?
                    .into_iter()
                    .find(|r| r.revision.to_string() == step.component.revision)
                    .ok_or_else(|| Error::NotFound("Pinned Skill revision missing".into()))?;
                revision
                    .procedure
                    .iter()
                    .map(|procedure| {
                        procedure.shell_script().map(str::to_string).ok_or_else(|| {
                            Error::InvalidInput(
                                "Non-shell Skill needs an explicit registered Tool".into(),
                            )
                        })
                    })
                    .collect::<Result<Vec<_>>>()?
            }
            ComposableArtifactRef::Recovery(id) => self
                .recovery(id)?
                .steps
                .iter()
                .map(|step| match step {
                    crate::resilience::RecoveryStep::ShellCommand { command } => {
                        if !command.environment_overrides.is_empty() {
                            return Err(Error::InvalidInput(
                                "Recovery environment needs explicit step capabilities".into(),
                            ));
                        }
                        Ok(std::iter::once(&command.program)
                            .chain(&command.args)
                            .map(|arg| format!("'{}'", arg.replace('\'', "'\\''")))
                            .collect::<Vec<_>>()
                            .join(" "))
                    }
                    _ => Err(Error::InvalidInput(
                        "Recovery action needs explicit tool/capability binding".into(),
                    )),
                })
                .collect::<Result<Vec<_>>>()?,
            ComposableArtifactRef::EffectPlan(_) => {
                return Err(Error::Intervention("Effect plans require externally governed execution; composition trials cannot grant commit authority".into()));
            }
        };
        if commands.is_empty() {
            return Err(Error::InvalidInput(
                "Composition procedure must have executable steps".into(),
            ));
        }
        Ok((
            self.tool_definition_by_name("composition-procedure")?.id,
            Some(format!("set -e\n{}", commands.join("\n"))),
        ))
    }
}

impl Store {
    fn current_composition_component_body(
        &self,
        component: &ComposableArtifactRef,
    ) -> Result<serde_json::Value> {
        Ok(match component {
            ComposableArtifactRef::Tool(id) => serde_json::to_value(self.tool_definition(id)?)?,
            ComposableArtifactRef::Skill(id) => serde_json::to_value(
                self.skill_revisions(id)?
                    .into_iter()
                    .last()
                    .ok_or_else(|| Error::NotFound("Skill revision missing".into()))?,
            )?,
            ComposableArtifactRef::Recovery(id) => serde_json::to_value(self.recovery(id)?)?,
            ComposableArtifactRef::EffectPlan(id) => serde_json::to_value(self.effect_plan(id)?)?,
        })
    }
    pub fn historical_composition(
        &self,
        id: &CompositionId,
        revision: u64,
    ) -> Result<serde_json::Value> {
        let c = self.composition_revision(id, revision)?;
        let mut components = vec![];
        for step in &c.steps {
            let data: String = self.connection.query_row(
                "SELECT data FROM composition_components WHERE key=?1",
                [serde_json::to_string(&step.component)?],
                |r| r.get(0),
            )?;
            let body: serde_json::Value = serde_json::from_str(&data)?;
            if composition_hash(&body)? != step.component.content_hash {
                return Err(Error::InvalidInput(
                    "Archived component body integrity mismatch".into(),
                ));
            }
            components.push(body);
        }
        Ok(
            serde_json::json!({"composition":c,"immutable_components":components,"evidence":self.composition_evidence(id)?.into_iter().filter(|e|e.composition_revision==revision).collect::<Vec<_>>()}),
        )
    }
    pub fn composition_assurance(&self, c: &Composition) -> Result<serde_json::Value> {
        use std::collections::BTreeSet;
        let evidence = self
            .composition_evidence(&c.id)?
            .into_iter()
            .filter(|e| e.composition_revision == c.revision)
            .collect::<Vec<_>>();
        let mut gaps = vec![];
        if self.composition_dependency_health(c)?.status != CompositionHealthStatus::Healthy {
            gaps.push("component revision revalidation".into())
        }
        if evidence
            .iter()
            .any(|e| e.outcome == CompositionOutcome::Fail)
        {
            gaps.push("contradictory composition evidence".into())
        }
        let passed = evidence
            .iter()
            .filter(|e| {
                e.outcome == CompositionOutcome::Pass
                    && e.experiment_quality == crate::experimentation::ExperimentQuality::Controlled
            })
            .collect::<Vec<_>>();
        if !passed.iter().any(|e| e.failure_points.is_empty()) {
            gaps.push("passing control".into())
        }
        for step in &c.steps {
            if !passed.iter().any(|e| {
                e.failure_points.contains(&step.id)
                    && matches!(
                        e.recovery_result,
                        Some(
                            CompositionRecoveryOutcome::FullyRecovered
                                | CompositionRecoveryOutcome::Compensated
                        )
                    )
            }) {
                gaps.push(format!("failure/recovery trial at {}", step.id))
            }
        }
        for invariant in &c.contract.sequence_invariants {
            if !passed.iter().any(|e| {
                e.invariant_results
                    .iter()
                    .any(|i| i.invariant == invariant.id && i.satisfied == Some(true))
            }) {
                gaps.push(format!("sequence invariant {} coverage", invariant.id))
            }
        }
        let evaluators: BTreeSet<_> = passed.iter().map(|e| &e.provenance.evaluator).collect();
        if evaluators.len() < 2 {
            gaps.push("independent evaluator specification".into())
        }
        if c.contract.postconditions.is_empty() && c.contract.sequence_invariants.is_empty() {
            gaps.push("explicit composition contract".into())
        }
        let preflight = DefaultCompositionPreflightAnalyzer.analyze(c, &Default::default())?;
        if preflight.findings.iter().any(|f| {
            matches!(
                f.kind,
                CompositionCompatibilityFindingKind::CapabilityConflict
                    | CompositionCompatibilityFindingKind::EffectConflict
                    | CompositionCompatibilityFindingKind::RecoveryConflict
                    | CompositionCompatibilityFindingKind::ConstraintConflict
            )
        }) {
            gaps.push("composition preflight conflict".into())
        }
        Ok(
            serde_json::json!({"profile":"composition-basic-v1","composition":c.id,"revision":c.revision,"satisfied":gaps.is_empty(),"gaps":gaps,"controlled_passes":passed.len(),"evaluator_specifications":evaluators.len(),"scope_limited":true}),
        )
    }
    pub fn promote_composite_skill(&self, id: &CompositionId) -> Result<CompositeSkill> {
        use crate::store::AssuranceStore;
        let c = self.composition(&id.to_string())?;
        let report = self.composition_assurance(&c)?;
        if report["satisfied"] != true {
            return Err(Error::Intervention(format!(
                "Composition validation incomplete: {}",
                report["gaps"]
            )));
        }
        let evidence = self
            .composition_evidence(id)?
            .into_iter()
            .filter(|e| e.composition_revision == c.revision)
            .collect::<Vec<_>>();
        let attestations = evidence
            .iter()
            .flat_map(|e| e.step_results.iter().filter_map(|s| s.attestation.clone()))
            .collect::<Vec<_>>();
        let mut manifest: crate::assurance::EvidenceManifest = serde_json::from_value(
            serde_json::json!({"id":EvidenceManifestId::new(),"subject":{"kind":"composition","subject":{"id":id,"revision":c.revision}},"generated_at":chrono::Utc::now(),"attestations":attestations,"policy_versions":crate::assurance::PolicyVersions::default(),"evidence_hash":""}),
        )?;
        manifest.seal()?;
        self.insert_evidence_manifest(&manifest)?;
        let composite = CompositeSkill {
            id: CompositeSkillId::new(),
            composition: id.clone(),
            revision: c.revision,
            contract: c.contract.clone(),
            maturity: CompositionMaturity::Validated,
            operating_envelope: c.scope.clone(),
            evidence_manifest: manifest,
        };
        self.connection.execute(
            "INSERT INTO composite_skills(id,composition,revision,data) VALUES(?1,?2,?3,?4)",
            params![
                composite.id.to_string(),
                id.to_string(),
                i64::try_from(c.revision)
                    .map_err(|_| Error::InvalidInput("Revision overflow".into()))?,
                serde_json::to_string(&composite)?
            ],
        )?;
        self.composition_event(id, "composite_skill_validated", &composite)?;
        Ok(composite)
    }
    pub fn composition_interactions(&self, id: &CompositionId) -> Result<Vec<InteractionFailure>> {
        self.connection.prepare("SELECT data FROM composition_interaction_failures WHERE composition=?1 ORDER BY rowid")?.query_map([id.to_string()],|r|r.get::<_,String>(0))?.map(|r|Ok(serde_json::from_str(&r?)?)).collect()
    }
    pub fn record_composition_interaction(
        &self,
        control: &CompositionEvidenceId,
        counterfactual: &CompositionEvidenceId,
        origin: &CompositionStepId,
        manifestation: &CompositionStepId,
    ) -> Result<InteractionFailure> {
        let load = |id: &CompositionEvidenceId| -> Result<CompositionEvidence> {
            let data: String = self.connection.query_row(
                "SELECT data FROM composition_evidence WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )?;
            Ok(serde_json::from_str(&data)?)
        };
        let a = load(control)?;
        let b = load(counterfactual)?;
        if a.composition != b.composition
            || a.composition_revision != b.composition_revision
            || a.starting_state != b.starting_state
            || a.provenance.evaluator != b.provenance.evaluator
            || a.outcome != CompositionOutcome::Fail
            || b.outcome != CompositionOutcome::Pass
            || a.experiment_quality != crate::experimentation::ExperimentQuality::Controlled
            || b.experiment_quality != crate::experimentation::ExperimentQuality::Controlled
            || a.test_input_hash == b.test_input_hash
        {
            return Err(Error::InvalidInput("Interaction candidate requires controlled fail/pass comparison with fixed revision, state and evaluator".into()));
        }
        let c = self.composition_revision(&a.composition, a.composition_revision)?;
        if !precedes(&c, origin, manifestation) {
            return Err(Error::InvalidInput(
                "Origin must precede manifestation".into(),
            ));
        }
        let interaction = InteractionFailure {
            id: InteractionFailureId::new(),
            composition: a.composition,
            origin_steps: vec![origin.clone()],
            manifestation_step: manifestation.clone(),
            kind: InteractionFailureKind::AssumptionInvalidation,
            failure_signature: crate::runtime::FailureSignatureRef {
                signature: "candidate-cross-step-assumption-invalidation".into(),
            },
            violated_invariants: a
                .invariant_results
                .iter()
                .filter(|i| i.satisfied == Some(false))
                .map(|i| i.invariant.clone())
                .collect(),
            evidence: vec![
                crate::epistemic::EvidenceRef {
                    kind: "composition_evidence".into(),
                    id: control.to_string(),
                },
                crate::epistemic::EvidenceRef {
                    kind: "composition_evidence".into(),
                    id: counterfactual.to_string(),
                },
            ],
        };
        self.connection.execute(
            "INSERT INTO composition_interaction_failures(id,composition,data) VALUES(?1,?2,?3)",
            params![
                interaction.id.to_string(),
                interaction.composition.to_string(),
                serde_json::to_string(&interaction)?
            ],
        )?;
        self.composition_event(
            &interaction.composition,
            "interaction_candidate_observed",
            &interaction,
        )?;
        Ok(interaction)
    }
}

impl Store {
    pub fn composition_opportunity(
        &self,
        c: &Composition,
        exposure: usize,
        severity: crate::curriculum::Severity,
        budget: &crate::budget::ExperienceBudget,
    ) -> Result<crate::economics::ExperienceOpportunity> {
        use crate::economics::*;
        let important = severity >= crate::curriculum::Severity::High;
        let band = if important {
            ValueBand::High
        } else {
            ValueBand::Low
        };
        let gap = ExperienceGap {
            kind: ExperienceOpportunityKind::ValidateComposition,
            target: ExperienceOpportunityTarget::RuntimeGap(c.id.to_string()),
            reasons: vec![OpportunityReason::Custom(
                "Composition interaction and recovery evidence gap".into(),
            )],
            severity,
            exposure: if exposure >= 10 {
                ExposureBand::Frequent
            } else {
                ExposureBand::Rare
            },
            mitigation_gap: MitigationGap::Significant,
            learning: LearningValueEstimate {
                band,
                possible_outcomes: vec![LearningOutcomeClass::ChangeRuntimeDecision],
                decision_changing_outcomes: 1,
                rationale: vec!["Component support does not establish sequence support".into()],
            },
            decision_relevance: DecisionRelevance {
                affected_decisions: exposure,
                affected_task_families: 1,
                current_runtime_use: if exposure >= 10 {
                    RuntimeUseBand::High
                } else {
                    RuntimeUseBand::Low
                },
                likely_decision_change: band,
            },
            reuse: ReusePotential::Reusable,
            novelty: EvidenceNovelty::ContextExtension,
            evidence: EvidenceSummary::default(),
            estimated_cost: ExperimentCost {
                trials: c.steps.len().min(budget.max_realities),
                ..Default::default()
            },
            risk: OpportunityRisk {
                trial_safety: crate::curriculum::TrialSafety::RequiresIsolation,
                external_effect_risk: crate::effects::EffectRisk::ReadOnly,
                isolation_required: crate::runtime::ExperimentCapabilitySummary::default()
                    .requirements,
                approval_required: false,
            },
            dependencies: vec![],
        };
        let opportunity = DeterministicExperienceOpportunityGenerator
            .generate(&ExperiencePlanningContext {
                gaps: vec![gap],
                completed_dependencies: Default::default(),
                objective: ExperiencePortfolioObjective::Balanced,
                now: chrono::Utc::now(),
            })?
            .pop()
            .ok_or_else(|| Error::InvalidInput("No composition opportunity generated".into()))?;
        self.save_experience_opportunity(&opportunity)?;
        Ok(opportunity)
    }
    pub fn investigate_composition_interaction(
        &self,
        id: &InteractionFailureId,
        mut input: crate::causal::CausalInvestigationInput,
    ) -> Result<crate::causal::CausalInvestigation> {
        let found: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM composition_interaction_failures WHERE id=?1)",
            [id.to_string()],
            |r| r.get(0),
        )?;
        if !found {
            return Err(Error::NotFound("Interaction failure missing".into()));
        }
        input.target = crate::causal::CausalTarget::CompositionInteraction(id.clone());
        self.create_causal_investigation(&input)
    }
    pub fn composition_invariant_knowledge(
        &self,
        c: &Composition,
        invariant: &SequenceInvariantId,
    ) -> Result<crate::hierarchy::KnowledgeHierarchy> {
        use crate::hierarchy::*;
        use chrono::Utc;
        let invariant = c
            .contract
            .sequence_invariants
            .iter()
            .find(|i| &i.id == invariant)
            .ok_or_else(|| Error::NotFound("Sequence invariant not found".into()))?;
        let evidence = self
            .composition_evidence(&c.id)?
            .into_iter()
            .filter(|e| {
                e.composition_revision == c.revision
                    && e.invariant_results
                        .iter()
                        .any(|r| r.invariant == invariant.id && r.satisfied == Some(true))
            })
            .map(|e| crate::epistemic::EvidenceRef {
                kind: "composition_evidence".into(),
                id: e.id.to_string(),
            })
            .collect::<Vec<_>>();
        if evidence.is_empty() {
            return Err(Error::InvalidInput(
                "Sequence invariant needs observed evidence".into(),
            ));
        }
        let validated = self.composition_assurance(c)?["satisfied"] == true;
        let id = KnowledgeNodeId::new();
        let artifact = KnowledgeArtifactRef {
            kind: KnowledgeArtifactKind::Constraint,
            id: invariant.id.to_string(),
            revision: c.revision,
        };
        let mut scope = c.scope.clone();
        scope.predicates.push(ScopePredicate::Equals {
            key: "composition_id".into(),
            value: ScopeValue::String(c.id.to_string()),
        });
        scope.predicates.push(ScopePredicate::Bool {
            key: format!("violation.{}", invariant.id),
            expected: true,
        });
        let now = Utc::now();
        let node = KnowledgeHierarchyNode {
            id: id.clone(),
            artifact: artifact.clone(),
            scope,
            maturity: if validated {
                KnowledgeMaturity::Validated
            } else {
                KnowledgeMaturity::Candidate
            },
            freshness: FreshnessStatus::Fresh,
            activation: KnowledgeActivationState::Active,
            provenance: KnowledgeProvenance {
                source_artifacts: vec![],
                source_contexts: vec![],
                evidence,
                root_origins: vec![c.id.to_string()],
                origin: crate::abstraction::KnowledgeOrigin::Local,
                generator: "composition-invariant-v1".into(),
                created_at: now,
            },
        };
        self.register_operational_revision(
            &crate::knowledge_runtime::OperationalKnowledgeRevision {
                knowledge: (&artifact).into(),
                statement: format!(
                    "Sequence requirement {:?}: {:?}",
                    invariant.scope, invariant.condition
                ),
                recovery: None,
            },
        )?;
        let h = KnowledgeHierarchy {
            id: KnowledgeHierarchyId::new(),
            name: format!("composition-invariant-{}", invariant.id),
            root_nodes: vec![id.clone()],
            nodes: std::collections::BTreeMap::from([(id, node)]),
            edges: vec![],
            revision: 1,
            created_at: now,
            updated_at: now,
        };
        self.save_knowledge_hierarchy(&h)?;
        Ok(h)
    }
    pub fn composition_guard_candidate(
        &self,
        c: &Composition,
        invariant: &SequenceInvariantId,
        guard: Option<crate::knowledge_runtime::GuardRef>,
    ) -> Result<crate::knowledge_runtime::GuardRevisionCandidate> {
        if self.composition_assurance(c)?["satisfied"] != true {
            return Err(Error::Intervention(
                "Sequence invariant requires composition assurance before Guard review".into(),
            ));
        }
        let h = self.composition_invariant_knowledge(c, invariant)?;
        let mut candidate =
            crate::knowledge_runtime::guard_revision_candidate(&h, &h.root_nodes[0], guard)?;
        candidate.reason =
            crate::knowledge_runtime::GuardRevisionReason::NewValidatedSequenceInvariant;
        self.save_guard_candidate(&candidate)?;
        Ok(candidate)
    }
}

impl Store {
    pub fn record_composition_epistemic(
        &self,
        e: &CompositionEvidence,
    ) -> Result<crate::core::ClaimId> {
        use crate::{epistemic::*, store::EpistemicStore};
        let statement = format!(
            "Composition {} revision {} satisfies its sequence contract under tested conditions",
            e.composition, e.composition_revision
        );
        let claim = if let Some(c) = self
            .claims()?
            .into_iter()
            .find(|c| c.statement == statement)
        {
            c
        } else {
            let c = Claim {
                id: ClaimId::new(),
                kind: ClaimKind::SkillBehavior,
                statement,
                scope: crate::lesson::ContextSelector {
                    repository: None,
                    required_markers: vec![],
                    tags: vec![],
                    os: None,
                    arch: None,
                },
                created_at: chrono::Utc::now(),
            };
            self.insert_claim(&c)?;
            c
        };
        let evaluator = EvaluatorIdentity {
            name: e.provenance.evaluator.clone(),
            version: "composition-state-v1".into(),
            kind: EvaluatorKind::BehavioralContract,
        };
        let path = EvidencePath {
            id: EvidencePathId::new(),
            claim: claim.id.clone().into(),
            source: EvidenceSource::StaticCheck {
                evaluator: e.provenance.evaluator.clone(),
            },
            context: EvidenceContext {
                evaluators: vec![evaluator.clone()],
                root_evidence_origins: vec![e.starting_state.fingerprint.clone()],
                ..Default::default()
            },
            dependencies: EpistemicDependencySet {
                evaluators: vec![
                    "composition-state-evaluator-v1".into(),
                    e.provenance.evaluator.clone(),
                ],
                evaluator_identities: vec![evaluator],
                environment_family: Some(e.provenance.environment.clone()),
                ..Default::default()
            },
            evidence_refs: vec![EvidenceRef {
                kind: "composition_evidence".into(),
                id: e.id.to_string(),
            }],
            outcome: match e.outcome {
                CompositionOutcome::Pass => EvidenceOutcome::Supports,
                CompositionOutcome::Fail => EvidenceOutcome::Contradicts,
                _ => EvidenceOutcome::Inconclusive,
            },
            created_at: e.created_at,
        };
        self.insert_evidence_path(&path)?;
        Ok(claim.id)
    }
    pub fn composition_effect_state(&self, c: &Composition) -> Result<serde_json::Value> {
        use crate::store::EffectStore;
        let mut receipts = vec![];
        let mut unresolved = vec![];
        for declared in &c.contract.effect_policy.effects {
            let effect = self.effect(&declared.effect)?;
            let receipt = self.commit_receipt_for_effect(&declared.effect)?;
            if let Some(receipt) = receipt {
                receipts.push(receipt)
            } else {
                unresolved.push(effect)
            }
        }
        Ok(
            serde_json::json!({"atomicity":c.contract.effect_policy.atomicity,"committed_receipts":receipts,"unresolved_effects":unresolved,"partial_commit":!receipts.is_empty()&&!unresolved.is_empty(),"rollback_inferred":false,"compensation_is_rollback":false}),
        )
    }
}

impl Store {
    pub fn start_composition_trajectory(
        &self,
        c: &Composition,
        session: HardknockSessionId,
    ) -> Result<crate::predictive::ExecutionTrajectory> {
        self.start_trajectory(crate::store::NewTrajectory {
            session_id: session,
            subject: Some(crate::predictive::TrajectorySubject::Composition(
                c.id.clone(),
            )),
            task_family: None,
            context: crate::predictive::TrajectoryContext {
                observability: vec![
                    "composition_step".into(),
                    "old_credential_valid".into(),
                    "rollback_window".into(),
                ],
                ..Default::default()
            },
        })
    }
    pub fn observe_composition_step(
        &self,
        trajectory: &TrajectoryId,
        step: &CompositionStepId,
        observations: &[StateClaim],
    ) -> Result<Vec<crate::predictive::FailureForecast>> {
        use crate::predictive::*;
        // Only normalized booleans/numbers cross into prediction; never credential material.
        let mut features = std::collections::BTreeMap::from([(
            "composition_step".into(),
            TrajectoryValue::Text(step.to_string()),
        )]);
        for claim in observations.iter().filter(|c| {
            c.source.trust() >= crate::knowledge_runtime::ContextValueSource::AdapterObserved
        }) {
            let value = match &claim.value {
                crate::hierarchy::ScopeValue::Boolean(v) => Some(TrajectoryValue::Boolean(*v)),
                crate::hierarchy::ScopeValue::Integer(v) => Some(TrajectoryValue::Integer(*v)),
                _ => None,
            };
            if let Some(value) = value {
                features.insert(claim.key.clone(), value);
            }
        }
        self.append_trajectory_event(
            trajectory,
            crate::store::NewTrajectoryEvent {
                kind: TrajectoryEventKind::StateObserved,
                observation: TrajectoryObservation { features },
                evidence: vec![],
            },
        )?;
        self.forecast_trajectory(trajectory)
    }
}

impl Store {
    /// Compile concrete, bounded composition requests into the existing Curriculum lifecycle.
    pub fn compile_composition_curriculum(
        &self,
        skill: &str,
        requests: &[CompositionExperimentRequest],
        kind: crate::curriculum::CurriculumGoalKind,
        trusted_host: bool,
        budget: &crate::budget::ExperienceBudget,
    ) -> Result<crate::curriculum::Curriculum> {
        use crate::curriculum::*;
        if requests.is_empty()
            || !matches!(
                kind,
                CurriculumGoalKind::ValidateComposition
                    | CurriculumGoalKind::FindInteractionFailure
                    | CurriculumGoalKind::ValidateSequenceInvariant
                    | CurriculumGoalKind::ValidateCompositionRecovery
                    | CurriculumGoalKind::ChallengeOrderingConstraint
                    | CurriculumGoalKind::MinimizeCompositionAuthority
            )
        {
            return Err(Error::InvalidInput(
                "Composition curriculum needs concrete requests and a composition goal".into(),
            ));
        }
        let skill = self.skill(skill)?;
        let goal = CurriculumGoal {
            id: CurriculumGoalId::new(),
            kind,
            description: "Validate an explicit composition under supplied interventions".into(),
            priority: Priority::High,
            score: PriorityScore {
                score: 80,
                priority: Priority::High,
                explanation: "Cross-step evidence gap".into(),
            },
            evidence_gap: EvidenceGap {
                dimension: "composition".into(),
                known_values: vec![],
                unknown_values: vec!["sequence validity".into()],
                rationale: "Component evidence does not establish composition validity".into(),
            },
            status: GoalStatus::Planned,
            decision: CurriculumDecision::Approved,
            reason: "Bounded isolated validation".into(),
            severity: Severity::High,
            safety: TrialSafety::RequiresIsolation,
        };
        let trials = requests.iter().map(|request| {
            let c = self.composition_revision(&request.composition, request.revision)?;
            ordered_steps(&c)?;
            Ok(CurriculumTrial { id: CurriculumTrialId::new(), goal_id: goal.id.clone(), skill_id: skill.id.clone(), condition: format!("composition:{}:{}", c.id, c.revision), fingerprint: composition_hash(request)?, intent: TrialIntent::Revalidation, execution: TrialExecution::Composition { request: Box::new(request.clone()), trusted_host }, result: None, learning_outcome: None, status: GoalStatus::Planned, estimated_budget: crate::budget::ExperienceUsage { realities: c.steps.len(), ..Default::default() }, expected_value: "Observe sequence and recovery behavior without transferring execution authority".into(), required_isolation: RealityCapabilities::default(), round: 1 })
        }).collect::<Result<Vec<_>>>()?;
        let now = chrono::Utc::now();
        let curriculum = Curriculum {
            id: CurriculumId::new(),
            target: CurriculumTarget::Skill(skill.id),
            profile: "composition".into(),
            goals: vec![goal],
            trials,
            budget: budget.clone(),
            usage: Default::default(),
            reserved: Default::default(),
            trials_executed: 0,
            status: CurriculumStatus::Planned,
            created_at: now,
            updated_at: now,
            rounds: 0,
            max_rounds: 1,
            revision: 1,
            before: vec![],
            after: vec![],
            stop_reason: None,
            session_id: None,
            quality: CurriculumQuality::Medium,
        };
        let config = crate::bridge::config::Config::load(&self.home)?;
        CurriculumExecutor {
            store: self,
            config: &config,
        }
        .validate(&curriculum)?;
        crate::store::CurriculumStore::insert(self, &curriculum)?;
        self.composition_event(
            &requests[0].composition,
            "composition_curriculum_created",
            &curriculum.id,
        )?;
        Ok(curriculum)
    }
}
