// SPDX-License-Identifier: Apache-2.0
use super::{EffectStore, EpistemicStore, RuntimeStore, Store, ToolStore};
use crate::{
    Error, Result,
    composition::{ComposableArtifactRef, StateClaimSource, composition_hash, trusted_state},
    core::*,
    plan::*,
    runtime::RuntimeDecisionContext,
};
use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
fn revision(n: u64) -> Result<i64> {
    i64::try_from(n).map_err(|_| Error::InvalidInput("Plan revision overflow".into()))
}
impl Store {
    pub fn save_execution_plan(
        &self,
        input: &ExecutionPlan,
        reason: PlanRevisionReason,
    ) -> Result<ExecutionPlan> {
        validate_plan(input)?;
        if input.status != PlanStatus::Proposed
            || input
                .steps
                .iter()
                .any(|s| s.status != PlanStepStatus::Pending)
        {
            return Err(Error::InvalidInput(
                "Imported plan definitions begin Proposed with Pending steps".into(),
            ));
        }
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let mut plan = input.clone();
        for step in &plan.steps {
            let pin = match &step.kind {
                PlanStepKind::Skill(id) => Some(PlanComponentRevision::Executable {
                    revision: self
                        .pin_composition_component(&ComposableArtifactRef::Skill(id.clone()))?,
                }),
                PlanStepKind::Tool(id) => Some(PlanComponentRevision::Executable {
                    revision: self
                        .pin_composition_component(&ComposableArtifactRef::Tool(id.clone()))?,
                }),
                PlanStepKind::Effect(id) => Some(PlanComponentRevision::Executable {
                    revision: self.pin_composition_component(
                        &ComposableArtifactRef::EffectPlan(id.clone()),
                    )?,
                }),
                PlanStepKind::Composition(id) => {
                    let c = self.composition(&id.to_string())?;
                    Some(PlanComponentRevision::Composition {
                        id: id.clone(),
                        revision: c.revision,
                        hash: composition_hash(&c)?,
                    })
                }
                _ => None,
            };
            if let Some(pin) = pin {
                if plan
                    .component_revisions
                    .get(&step.id)
                    .is_some_and(|old| composition_hash(old).ok() != composition_hash(&pin).ok())
                {
                    return Err(Error::Intervention(
                        "Plan component pin changed; explicitly revise before use".into(),
                    ));
                }
                plan.component_revisions.insert(step.id.clone(), pin);
            }
        }
        let previous: Option<i64> = self
            .connection
            .query_row(
                "SELECT revision FROM execution_plans WHERE id=?1",
                [plan.id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if previous.map_or(plan.revision != 1, |r| {
            r.checked_add(1) != i64::try_from(plan.revision).ok()
        }) {
            return Err(Error::InvalidInput(
                "Plan revisions must advance exactly once from revision 1".into(),
            ));
        }
        tx.execute("INSERT INTO execution_plans(id,revision,name,data) VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,name=excluded.name,data=excluded.data",params![plan.id.to_string(),revision(plan.revision)?,plan.goal.description,serde_json::to_string(&plan)?])?;
        tx.execute(
            "INSERT INTO plan_revisions(id,revision,hash,data) VALUES(?1,?2,?3,?4)",
            params![
                plan.id.to_string(),
                revision(plan.revision)?,
                composition_hash(&plan)?,
                serde_json::to_string(&plan)?
            ],
        )?;
        if let Some(parent) = previous {
            self.plan_event(
                &plan.id,
                None,
                "plan_revised",
                &PlanRevisionRelation {
                    parent: PlanRevisionRef {
                        plan: plan.id.clone(),
                        revision: u64::try_from(parent).map_err(|_| {
                            Error::InvalidInput("Invalid stored plan revision".into())
                        })?,
                    },
                    child: PlanRevisionRef {
                        plan: plan.id.clone(),
                        revision: plan.revision,
                    },
                    reason,
                },
            )?;
        } else {
            self.plan_event(&plan.id, None, "plan_created", &plan)?;
        }
        tx.commit()?;
        Ok(plan)
    }
    pub fn execution_plan(&self, id: &ExecutionPlanId) -> Result<ExecutionPlan> {
        let data: String = self.connection.query_row(
            "SELECT data FROM execution_plans WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&data)?)
    }
    pub fn execution_plans(&self) -> Result<Vec<ExecutionPlan>> {
        self.connection
            .prepare("SELECT data FROM execution_plans ORDER BY name,id")?
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub fn plan_revision(&self, id: &ExecutionPlanId, rev: u64) -> Result<ExecutionPlan> {
        let (hash, data): (String, String) = self.connection.query_row(
            "SELECT hash,data FROM plan_revisions WHERE id=?1 AND revision=?2",
            params![id.to_string(), revision(rev)?],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let plan: ExecutionPlan = serde_json::from_str(&data)?;
        if composition_hash(&plan)? != hash {
            return Err(Error::InvalidInput(
                "Plan revision integrity mismatch".into(),
            ));
        }
        Ok(plan)
    }
    pub fn plan_event(
        &self,
        plan: &ExecutionPlanId,
        run: Option<&PlanRunId>,
        kind: &str,
        value: &impl serde::Serialize,
    ) -> Result<()> {
        self.connection.execute(
            "INSERT INTO plan_events(plan,run,kind,data) VALUES(?1,?2,?3,?4)",
            params![
                plan.to_string(),
                run.map(ToString::to_string),
                kind,
                serde_json::to_string(value)?
            ],
        )?;
        Ok(())
    }
    pub fn plan_history(&self, id: &ExecutionPlanId) -> Result<Vec<Value>> {
        self.connection
            .prepare("SELECT kind,data,created_at FROM plan_events WHERE plan=?1 ORDER BY id")?
            .query_map([id.to_string()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .map(|r| {
                let (k, d, t) = r?;
                Ok(json!({"kind":k,"data":serde_json::from_str::<Value>(&d)?,"created_at":t}))
            })
            .collect()
    }
    pub fn start_plan_run(&self, id: &ExecutionPlanId) -> Result<PlanRun> {
        let plan = self.execution_plan(id)?;
        let trajectory = self.start_trajectory(crate::store::NewTrajectory {
            session_id: HardknockSessionId::new(),
            subject: Some(crate::predictive::TrajectorySubject::Plan(plan.id.clone())),
            task_family: None,
            context: Default::default(),
        })?;
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let run = PlanRun {
            trajectory: Some(trajectory.id),
            id: PlanRunId::new(),
            plan: PlanRevisionRef {
                plan: id.clone(),
                revision: plan.revision,
            },
            state: PlanState::initial(&plan),
            started_at: Utc::now(),
            finished_at: None,
            outcome: None,
        };
        self.connection.execute(
            "INSERT INTO plan_runs(id,plan,data) VALUES(?1,?2,?3)",
            params![
                run.id.to_string(),
                id.to_string(),
                serde_json::to_string(&run)?
            ],
        )?;
        self.plan_event(id, Some(&run.id), "plan_started", &run)?;
        tx.commit()?;
        Ok(run)
    }
    pub fn plan_run(&self, id: &PlanRunId) -> Result<PlanRun> {
        let data: String = self.connection.query_row(
            "SELECT data FROM plan_runs WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&data)?)
    }
    fn update_plan_run(&self, run: &PlanRun) -> Result<()> {
        self.connection.execute(
            "UPDATE plan_runs SET data=?2 WHERE id=?1",
            params![run.id.to_string(), serde_json::to_string(run)?],
        )?;
        Ok(())
    }
    pub fn record_plan_observation(
        &self,
        id: &PlanRunId,
        observation: &PlanObservation,
    ) -> Result<Vec<AssumptionDrift>> {
        if observation.claims.is_empty()
            || observation
                .claims
                .iter()
                .map(|c| &c.key)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != observation.claims.len()
            || observation.claims.len() > 1000
            || observation.captured_at > Utc::now()
        {
            return Err(Error::InvalidInput(
                "Plan observation must contain bounded current claims".into(),
            ));
        }
        if matches!(observation.source, StateClaimSource::ToolAttestation) {
            let attestation = observation.attestation.as_ref().ok_or_else(|| {
                Error::InvalidInput("Tool observation requires an attestation".into())
            })?;
            self.execution_attestation(attestation)?;
            return Err(Error::InvalidInput("Attestation observations require an adapter that binds claims to verified output artifacts".into()));
        }
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let mut run = self.plan_run(id)?;
        if run.finished_at.is_some() {
            return Err(Error::Intervention("Plan run is terminal".into()));
        }
        let previous = run.state.clone();
        for claim in &observation.claims {
            if claim.source.trust() > observation.source.trust()
                || claim.freshness.observed_at > observation.captured_at
            {
                return Err(Error::InvalidInput(
                    "Observation cannot elevate claim provenance or use future facts".into(),
                ));
            }
            if run.state.observations.iter().any(|old| {
                old.key == claim.key
                    && old.source.trust() >= claim.source.trust()
                    && old.freshness.observed_at > claim.freshness.observed_at
            }) {
                return Err(Error::InvalidInput(
                    "Observation cannot replace newer authoritative state".into(),
                ));
            }
            // Lower-trust reports never erase authoritative observations.
            run.state
                .observations
                .retain(|old| old.key != claim.key || old.source.trust() > claim.source.trust());
            run.state.observations.push(claim.clone());
            if claim.source.trust() >= crate::knowledge_runtime::ContextValueSource::AdapterObserved
                && !run
                    .state
                    .observations
                    .iter()
                    .any(|old| old.key == claim.key && old.source.trust() > claim.source.trust())
            {
                run.state
                    .observation_epochs
                    .insert(claim.key.clone(), run.state.mutation_epoch);
                if let Some(version) = &claim.freshness.external_version {
                    run.state
                        .external_versions
                        .insert(version.resource.clone(), version.version.clone());
                }
            }
        }
        let plan = self.plan_revision(&run.plan.plan, run.plan.revision)?;
        let drift = detect_assumption_drift(&plan, &previous, &run.state, Utc::now());
        self.connection.execute(
            "INSERT INTO plan_observations(id,run,data) VALUES(?1,?2,?3)",
            params![
                observation.id.to_string(),
                id.to_string(),
                serde_json::to_string(observation)?
            ],
        )?;
        self.update_plan_run(&run)?;
        self.plan_event(&plan.id, Some(id), "plan_assumption_observed", observation)?;
        for d in &drift {
            self.plan_event(&plan.id, Some(id), "plan_assumption_drift", d)?;
        }
        tx.commit()?;
        if let Some(trajectory) = &run.trajectory {
            use crate::predictive::*;
            let features = std::collections::BTreeMap::from([
                (
                    "plan_drift_count".into(),
                    TrajectoryValue::Integer(drift.len() as i64),
                ),
                (
                    "plan_mutation_epoch".into(),
                    TrajectoryValue::Integer(run.state.mutation_epoch as i64),
                ),
            ]);
            self.append_trajectory_event(
                trajectory,
                crate::store::NewTrajectoryEvent {
                    kind: TrajectoryEventKind::StateObserved,
                    observation: TrajectoryObservation { features },
                    evidence: vec![],
                },
            )?;
        }

        Ok(drift)
    }
    pub fn plan_evaluation_inputs(
        &self,
        plan: &ExecutionPlan,
        state: &PlanState,
    ) -> Result<PlanEvaluationInputs> {
        let mut input = PlanEvaluationInputs {
            now: Some(Utc::now()),
            ..Default::default()
        };
        let current = self.execution_plan(&plan.id)?;
        if current.revision != plan.revision {
            input
                .invalid_components
                .extend(plan.steps.iter().map(|s| s.id.clone()));
        }
        for (step, pin) in &plan.component_revisions {
            let valid = match pin {
                PlanComponentRevision::Executable { revision } => self
                    .pin_composition_component(&revision.component)
                    .is_ok_and(|p| composition_hash(&p).ok() == composition_hash(revision).ok()),
                PlanComponentRevision::Composition { id, revision, hash } => {
                    self.composition(&id.to_string()).is_ok_and(|c| {
                        c.revision == *revision
                            && composition_hash(&c).is_ok_and(|h| &h == hash)
                            && self.composition_dependency_health(&c).is_ok_and(|h| {
                                h.status == crate::composition::CompositionHealthStatus::Healthy
                            })
                    })
                }
            };
            if !valid {
                input.invalid_components.insert(step.clone());
            }
            if let PlanComponentRevision::Composition { id, revision, .. } = pin
                && state.current_step.as_ref() == Some(step)
            {
                let c = self.composition_revision(id, *revision)?;
                let age = if c.contract.effect_policy.commit_points.is_empty() {
                    plan.freshness_policy.critical_max_age
                } else {
                    input.nested_commitment_steps.insert(step.clone());
                    plan.freshness_policy.commitment_point_max_age
                };
                for assumption in &c.assumptions {
                    if assess_predicate(
                        &assumption.condition,
                        &crate::composition::StateFreshnessRequirement::None,
                        state,
                        Utc::now(),
                        age,
                    ) != AssumptionValidity::Supported
                    {
                        input
                            .nested_blockers
                            .entry(step.clone())
                            .or_default()
                            .push(format!(
                                "Nested composition assumption {} requires revalidation",
                                assumption.id
                            ));
                    }
                }
                for invariant in &c.contract.sequence_invariants {
                    if matches!(
                        invariant.scope,
                        crate::composition::SequenceScope::EntireComposition
                    ) && assess_predicate(
                        &invariant.condition,
                        &crate::composition::StateFreshnessRequirement::None,
                        state,
                        Utc::now(),
                        age,
                    ) != AssumptionValidity::Supported
                    {
                        input
                            .nested_blockers
                            .entry(step.clone())
                            .or_default()
                            .push(format!(
                                "Nested invariant {} is not supported",
                                invariant.id
                            ));
                    }
                }
            }
        }
        let hierarchies = self.knowledge_hierarchies()?;
        for dependency in &plan.knowledge_dependencies {
            let valid = hierarchies.iter().flat_map(|h| h.nodes.values()).any(|n| {
                n.artifact == dependency.knowledge.artifact
                    && n.artifact.revision == dependency.knowledge.revision
                    && n.activation == crate::hierarchy::KnowledgeActivationState::Active
                    && n.freshness == crate::hierarchy::FreshnessStatus::Fresh
            });
            if !valid {
                input
                    .changed_knowledge
                    .extend(dependency.dependent_steps.iter().cloned());
            }
        }
        for recovery in self.recoveries()? {
            if matches!(
                recovery.status,
                crate::resilience::RecoveryStatus::Supported
                    | crate::resilience::RecoveryStatus::Validated
            ) && !plan.commitment_points.iter().any(|p| {
                state.crossed_commitment_points.contains(&p.id)
                    && p.recoveries_lost.contains(&recovery.id)
            }) {
                input.available_recoveries.insert(recovery.id);
            }
        }
        let requirements = plan
            .commitment_gates
            .iter()
            .flat_map(|g| g.required_approvals.iter())
            .chain(plan.steps.iter().filter_map(|s| {
                if let PlanStepKind::Approval(a) = &s.kind {
                    Some(a)
                } else {
                    None
                }
            }));
        for requirement in requirements {
            let effects = requirement
                .effects
                .iter()
                .map(|id| self.effect(id))
                .collect::<Result<Vec<_>>>()?;
            for id in &state.authorizations {
                let data: Option<String> = self
                    .connection
                    .query_row(
                        "SELECT data FROM commit_authorizations WHERE id=?1",
                        [id.to_string()],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(data) = data {
                    let auth: crate::effects::CommitAuthorization = serde_json::from_str(&data)?;
                    if !effects.is_empty()
                        && auth.granted_at <= Utc::now()
                        && auth.validate(&effects, Utc::now()).is_ok()
                        && requirement.max_age.is_none_or(|age| {
                            Utc::now()
                                .signed_duration_since(auth.granted_at)
                                .to_std()
                                .is_ok_and(|a| a <= age)
                        })
                    {
                        input.valid_approvals.insert(requirement.id.clone());
                    }
                }
            }
        }
        for gate in &plan.commitment_gates {
            if let Some(minimum) = gate.minimum_diversity
                && !gate.diversity_claim.as_ref().is_some_and(|c| {
                    self.epistemic_report(c)
                        .is_ok_and(|r| r.diversity.diversity_class.satisfies(minimum))
                })
            {
                input.low_diversity.insert(gate.commitment_point.clone());
            }
        }
        Ok(input)
    }
    pub fn assess_plan_run(
        &self,
        id: &PlanRunId,
        context: &RuntimeDecisionContext,
        persist: bool,
    ) -> Result<PlanValidityAssessment> {
        let tx = if persist {
            Some(Transaction::new_unchecked(
                &self.connection,
                TransactionBehavior::Immediate,
            )?)
        } else {
            None
        };
        let mut run = self.plan_run(id)?;
        let plan = self.plan_revision(&run.plan.plan, run.plan.revision)?;
        let mut assessment = DeterministicPlanValidityEvaluator {
            inputs: self.plan_evaluation_inputs(&plan, &run.state)?,
        }
        .evaluate(&plan, &run.state, context)?;
        if run.outcome == Some(PlanRunOutcome::Failure) {
            assessment.status = PlanValidityStatus::RecoveryRequired;
            assessment.recommendations = vec![PlanValidityRecommendation::Abstain];
            assessment
                .reasons
                .push("Reconciled failure requires recovery or an explicit revised plan".into());
        }
        if persist {
            self.connection.execute(
                "INSERT INTO plan_validity_assessments(id,run,data) VALUES(?1,?2,?3)",
                params![
                    assessment.id.to_string(),
                    id.to_string(),
                    serde_json::to_string(&assessment)?
                ],
            )?;
            run.state.assumption_states = assessment
                .assumptions
                .iter()
                .map(|a| (a.assumption.clone(), a.validity))
                .collect();
            run.state.invariant_states = assessment
                .invariants
                .iter()
                .map(|i| (i.invariant.clone(), i.clone()))
                .collect();
            self.update_plan_run(&run)?;
            self.plan_event(&plan.id, Some(id), "plan_validity_changed", &assessment)?;
        }
        if let Some(tx) = tx {
            tx.commit()?;
        }
        Ok(assessment)
    }
    pub fn attach_plan_identity(&self, context: &mut RuntimeDecisionContext) -> Result<()> {
        let Some(plan) = &context.plan else {
            return Ok(());
        };
        let run = self.plan_run(&plan.run)?;
        if run.plan.plan != plan.plan
            || run.plan.revision != plan.revision
            || run.state.current_step.as_ref() != Some(&plan.next_step)
        {
            return Err(Error::Intervention(
                "Plan runtime identity differs from the recorded next step".into(),
            ));
        }
        let definition = self.plan_revision(&run.plan.plan, run.plan.revision)?;
        // Remove old plan-owned facts before resolving current observations, including
        // facts invalidated at a commitment and absent from the current state.
        for key in definition
            .assumptions
            .iter()
            .filter_map(|a| predicate_key(&a.predicate))
            .chain(
                definition
                    .invariants
                    .iter()
                    .filter_map(|i| predicate_key(&i.condition)),
            )
            .chain(run.state.observations.iter().map(|c| c.key.as_str()))
        {
            context.context_observations.remove(key);
        }
        for (key, value) in trusted_state(
            &run.state.observations,
            &run.state.external_versions,
            Utc::now(),
        )
        .values
        {
            let source = run
                .state
                .observations
                .iter()
                .filter(|c| {
                    c.key == key
                        && trusted_state(
                            std::slice::from_ref(c),
                            &run.state.external_versions,
                            Utc::now(),
                        )
                        .values
                        .get(&key)
                            == Some(&value)
                })
                .map(|c| c.source.trust())
                .max()
                .unwrap_or(crate::knowledge_runtime::ContextValueSource::AgentReported);
            context.context_observations.insert(
                key,
                vec![crate::knowledge_runtime::ContextValue { value, source }],
            );
        }
        context.context_observations.insert(
            "plan_id".into(),
            vec![crate::knowledge_runtime::ContextValue {
                value: crate::hierarchy::ScopeValue::String(plan.plan.to_string()),
                source: crate::knowledge_runtime::ContextValueSource::RuntimeObserved,
            }],
        );
        Ok(())
    }
    pub fn attach_plan_validity(
        &self,
        context: &mut RuntimeDecisionContext,
        persist: bool,
    ) -> Result<()> {
        let Some(plan) = context.plan.clone() else {
            return Ok(());
        };
        let assessment = self.assess_plan_run(&plan.run, context, persist)?;
        let run = self.plan_run(&plan.run)?;
        context.plan = Some(PlanRuntimeContext {
            validity: Some(assessment),
            crossed_commitments: run.state.crossed_commitment_points,
            ..plan
        });
        Ok(())
    }
    pub fn plan_runtime_context(
        &self,
        id: &PlanRunId,
        mut context: RuntimeDecisionContext,
    ) -> Result<RuntimeDecisionContext> {
        let run = self.plan_run(id)?;
        let next = run
            .state
            .current_step
            .clone()
            .ok_or_else(|| Error::Intervention("Plan has no next step".into()))?;
        context.plan = Some(PlanRuntimeContext {
            run: id.clone(),
            plan: run.plan.plan,
            revision: run.plan.revision,
            next_step: next,
            validity: None,
            crossed_commitments: run.state.crossed_commitment_points,
        });
        self.attach_plan_identity(&mut context)?;
        Ok(context)
    }
    pub fn plan_step_tool(
        &self,
        plan: &ExecutionPlan,
        step: &PlanStep,
    ) -> Result<(ToolId, Option<Value>)> {
        let component = match &step.kind {
            PlanStepKind::Tool(id) => ComposableArtifactRef::Tool(id.clone()),
            PlanStepKind::Skill(id) => ComposableArtifactRef::Skill(id.clone()),
            _ => {
                return Err(Error::InvalidInput(
                    "Step is not a Tool or shell Skill".into(),
                ));
            }
        };
        let current = self.pin_composition_component(&component)?;
        if !matches!(plan.component_revisions.get(&step.id), Some(PlanComponentRevision::Executable {revision})
            if composition_hash(revision)? == composition_hash(&current)?)
        {
            return Err(Error::Intervention(
                "Plan executable revision changed".into(),
            ));
        }
        match component {
            ComposableArtifactRef::Tool(id) => Ok((id, None)),
            ComposableArtifactRef::Skill(id) => {
                let revision = self
                    .skill_revisions(&id)?
                    .into_iter()
                    .find(|r| r.revision.to_string() == current.revision)
                    .ok_or_else(|| Error::NotFound("Pinned Skill revision missing".into()))?;
                let commands = revision
                    .procedure
                    .iter()
                    .map(|p| {
                        p.shell_script().map(str::to_string).ok_or_else(|| {
                            Error::InvalidInput("Non-shell Skill requires a registered Tool".into())
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                if commands.is_empty() {
                    return Err(Error::InvalidInput(
                        "Skill has no executable procedure".into(),
                    ));
                }
                let tool = self.tool_definition_by_name("composition-procedure")?;
                let expected = crate::tool::ToolInvocation::NativeBinary {
                    executable: "/bin/sh".into(),
                    args_template: vec![
                        "-c".into(),
                        "{command}".into(),
                        "composition".into(),
                        "{state_input}".into(),
                    ],
                };
                if serde_json::to_value(&tool.invocation)? != serde_json::to_value(expected)?
                    || tool.disabled
                {
                    return Err(Error::Intervention(
                        "Procedure Tool must use the verified shell wrapper".into(),
                    ));
                }
                Ok((
                    tool.id,
                    Some(
                        json!({"command":format!("set -e\n{}",commands.join("\n")),"state_input":"{}"}),
                    ),
                ))
            }
            _ => unreachable!(),
        }
    }
    pub fn complete_plan_step(&self, record: &PlanStepRun) -> Result<PlanRun> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let mut run = self.plan_run(&record.run)?;
        if run.finished_at.is_some() || run.state.current_step.as_ref() != Some(&record.step) {
            return Err(Error::Intervention("Plan step is not current".into()));
        }
        if record.completed_at > Utc::now() {
            return Err(Error::InvalidInput(
                "Step completion cannot be in the future".into(),
            ));
        }
        let used: bool = self.connection.query_row("SELECT EXISTS(SELECT 1 FROM plan_step_runs WHERE json_extract(data,'$.decision')=?1 OR (?2 IS NOT NULL AND json_extract(data,'$.composition_evidence')=?2) OR (?3 IS NOT NULL AND json_extract(data,'$.experiment')=?3))",
            params![record.decision.to_string(),record.composition_evidence.as_ref().map(ToString::to_string),record.experiment.as_ref().map(ToString::to_string)], |r| r.get(0))?;
        if used {
            return Err(Error::Intervention(
                "Plan execution evidence cannot complete multiple steps".into(),
            ));
        }
        let decision = self.runtime_decision(&record.decision)?;
        if decision.decision.kind() != crate::runtime::RuntimeDecisionKind::Act
            || !decision
                .context
                .plan
                .as_ref()
                .is_some_and(|p| p.run == record.run && p.next_step == record.step)
        {
            return Err(Error::Intervention(
                "Plan step needs its recorded ACT decision".into(),
            ));
        }
        let assessment = self.assess_plan_run(&record.run, &decision.context, false)?;
        if !matches!(
            assessment.status,
            PlanValidityStatus::Valid | PlanValidityStatus::ValidWithWarnings
        ) {
            return Err(Error::Intervention(
                "Plan changed before step completion; reconcile the outcome explicitly".into(),
            ));
        }
        let plan = self.plan_revision(&run.plan.plan, run.plan.revision)?;
        let step = plan
            .steps
            .iter()
            .find(|s| s.id == record.step)
            .expect("checked");
        match &step.kind {
            PlanStepKind::Tool(_) | PlanStepKind::Skill(_) => {
                let (tool, input) = self.plan_step_tool(&plan, step)?;
                let id = record.attestation.as_ref().ok_or_else(|| {
                    Error::InvalidInput("Executable step requires attestation".into())
                })?;
                let attestation = self.execution_attestation(id)?;
                if let Some(input) = input
                    && !attestation
                        .input_hashes
                        .contains(&composition_hash(&input)?)
                {
                    return Err(Error::Intervention(
                        "Skill attestation does not bind its pinned procedure input".into(),
                    ));
                }

                let used: bool = self.connection.query_row("SELECT EXISTS(SELECT 1 FROM plan_step_runs WHERE json_extract(data,'$.attestation')=?1)", [id.to_string()], |r| r.get(0))?;
                if used
                    || attestation.tool_manifest_hash
                        != self.tool_definition(&tool)?.manifest_hash()?
                    || attestation.result != crate::tool::ToolExecutionStatus::Success
                    || attestation.tool.id != tool
                    || attestation.started_at < decision.created_at
                    || attestation.completed_at > record.completed_at
                    || record.completed_at > Utc::now()
                {
                    return Err(Error::Intervention(
                        "Step attestation did not succeed".into(),
                    ));
                }
            }
            PlanStepKind::Observe(spec) => {
                if assess_predicate(
                    &spec.condition,
                    &spec.freshness,
                    &run.state,
                    Utc::now(),
                    plan.freshness_policy.critical_max_age,
                ) != AssumptionValidity::Supported
                {
                    return Err(Error::Intervention(
                        "Observation step is not satisfied".into(),
                    ));
                }
            }
            PlanStepKind::Effect(id) => {
                let effect_plan = self.effect_plan(id)?;
                for effect in &effect_plan.effects {
                    let receipt = self.commit_receipt_for_effect(effect)?.ok_or_else(|| {
                        Error::Intervention("Effect step requires real receipts".into())
                    })?;
                    if !record.receipts.contains(&receipt.id) {
                        return Err(Error::InvalidInput(
                            "Effect receipt absent from step record".into(),
                        ));
                    }
                }
            }
            PlanStepKind::Approval(_) => {}
            PlanStepKind::Composition(id) => {
                let evidence_id = record.composition_evidence.as_ref().ok_or_else(|| {
                    Error::InvalidInput("Nested step requires recorded composition evidence".into())
                })?;
                let evidence = self
                    .composition_evidence(id)?
                    .into_iter()
                    .find(|e| &e.id == evidence_id)
                    .ok_or_else(|| Error::NotFound("Composition evidence missing".into()))?;
                let revision = match plan.component_revisions.get(&step.id) {
                    Some(PlanComponentRevision::Composition { revision, .. }) => *revision,
                    _ => return Err(Error::InvalidInput("Nested composition pin missing".into())),
                };
                if evidence.composition_revision != revision
                    || evidence.outcome != crate::composition::CompositionOutcome::Pass
                    || evidence.created_at < decision.created_at
                    || evidence.created_at > record.completed_at
                    || evidence.experiment_quality
                        != crate::experimentation::ExperimentQuality::Controlled
                {
                    return Err(Error::Intervention(
                        "Nested composition needs a current controlled passing execution".into(),
                    ));
                }
            }
            PlanStepKind::Experiment(template) => {
                let id = record.experiment.as_ref().ok_or_else(|| {
                    Error::InvalidInput("Experiment step requires a recorded experiment".into())
                })?;
                let experiment = self.strategy_experiment(id)?;
                let request = template.request.as_ref().ok_or_else(|| {
                    Error::InvalidInput("Experiment template must bind a concrete request".into())
                })?;
                if composition_hash(&experiment.request)? != composition_hash(request.as_ref())?
                    || experiment.status != crate::experimentation::ExperimentStatus::Completed
                    || !experiment.result.as_ref().is_some_and(|r| {
                        r.quality == crate::experimentation::ExperimentQuality::Controlled
                    })
                {
                    return Err(Error::Intervention(
                        "Experiment result does not bind this step's controlled request".into(),
                    ));
                }
                for candidate in &experiment.result.as_ref().expect("checked").candidates {
                    let experience = self.experience(&candidate.experience_id)?;
                    if experience.actions.is_empty()
                        || experience
                            .actions
                            .iter()
                            .any(|a| a.started_at < decision.created_at)
                        || experience.created_at > record.completed_at
                    {
                        return Err(Error::Intervention(
                            "Experiment execution must follow the recorded step decision".into(),
                        ));
                    }
                }
            }
            PlanStepKind::Custom(_) => {
                return Err(Error::Intervention(
                    "Nested executions require their explicit engine result reconciliation".into(),
                ));
            }
        }
        for id in &record.receipts {
            let receipt = self.commit_receipt(id)?;
            if !run.state.committed_effects.contains(&receipt.effect_id) {
                run.state.committed_effects.push(receipt.effect_id);
            }
        }
        run.state.completed_steps.push(step.id.clone());
        if !matches!(
            step.kind,
            PlanStepKind::Observe(_) | PlanStepKind::Approval(_)
        ) {
            run.state.mutation_epoch += 1;
        }
        for point in plan
            .commitment_points
            .iter()
            .filter(|p| p.after_step == step.id)
        {
            for consequence in &point.consequences {
                let receipt = self
                    .commit_receipt_for_effect(&consequence.effect)?
                    .ok_or_else(|| {
                        Error::Intervention(
                            "Commitment cannot be inferred without a receipt".into(),
                        )
                    })?;
                if !record.receipts.contains(&receipt.id) {
                    return Err(Error::InvalidInput("Commitment receipt missing".into()));
                }
            }
            for assumption in plan
                .assumptions
                .iter()
                .filter(|a| point.assumptions_invalidated.contains(&a.id))
            {
                if let Some(key) = predicate_key(&assumption.predicate) {
                    run.state.observations.retain(|c| c.key != key);
                    run.state.observation_epochs.remove(key);
                }
            }
            run.state.crossed_commitment_points.push(point.id.clone());
            self.plan_event(&plan.id, Some(&run.id), "plan_commitment_crossed", point)?;
        }
        run.state.current_step = plan
            .steps
            .iter()
            .find(|s| {
                !run.state.completed_steps.contains(&s.id)
                    && s.dependencies
                        .iter()
                        .all(|d| run.state.completed_steps.contains(d))
            })
            .map(|s| s.id.clone());
        if run.state.completed_steps.len() == plan.steps.len() {
            run.finished_at = Some(Utc::now());
            run.outcome = Some(PlanRunOutcome::Success);
        }
        self.connection.execute(
            "INSERT INTO plan_step_runs(run,step,data) VALUES(?1,?2,?3)",
            params![
                record.run.to_string(),
                record.step.to_string(),
                serde_json::to_string(record)?
            ],
        )?;
        self.update_plan_run(&run)?;
        self.plan_event(&plan.id, Some(&run.id), "plan_step_completed", record)?;
        tx.commit()?;
        Ok(run)
    }
    /// Continue from recorded effects and completed steps under an explicitly saved revision.
    pub fn replan_run(&self, id: &PlanRunId, reason: PlanRevisionReason) -> Result<PlanRun> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let mut previous = self.plan_run(id)?;
        if previous.finished_at.is_some() {
            return Err(Error::Intervention("Plan run is terminal".into()));
        }
        let old = self.plan_revision(&previous.plan.plan, previous.plan.revision)?;
        let next = self.execution_plan(&previous.plan.plan)?;
        if next.revision <= old.revision {
            return Err(Error::InvalidInput(
                "Replanning requires an explicit newer revision".into(),
            ));
        }
        for completed in &previous.state.completed_steps {
            let a = old.steps.iter().find(|s| &s.id == completed);
            let b = next.steps.iter().find(|s| &s.id == completed);
            if b.is_none() || composition_hash(&a)? != composition_hash(&b)? {
                return Err(Error::Intervention(
                    "Replan cannot rewrite completed steps".into(),
                ));
            }
        }
        for crossed in &previous.state.crossed_commitment_points {
            let a = old.commitment_points.iter().find(|p| &p.id == crossed);
            let b = next.commitment_points.iter().find(|p| &p.id == crossed);
            if b.is_none() || composition_hash(&a)? != composition_hash(&b)? {
                return Err(Error::Intervention(
                    "Replan must preserve crossed commitments".into(),
                ));
            }
        }
        let recovery = self.plan_recovery_context(id)?;
        previous.state.committed_effects = recovery.committed_effects;
        previous.state.nested_commitments = recovery.plan_state.nested_commitments;
        let mut run = previous.clone();
        run.id = PlanRunId::new();
        run.outcome = None;
        run.finished_at = None;
        run.plan.revision = next.revision;
        run.state.revision = next.revision;
        run.started_at = Utc::now();
        run.state.current_step = next
            .steps
            .iter()
            .find(|s| {
                !run.state.completed_steps.contains(&s.id)
                    && s.dependencies
                        .iter()
                        .all(|d| run.state.completed_steps.contains(d))
            })
            .map(|s| s.id.clone());
        run.state.assumption_states.clear();
        run.state.invariant_states.clear();
        run.state.reached_checkpoints.clear();
        run.state.authorizations.clear();
        previous.finished_at = Some(run.started_at);
        // Appropriateness needs a controlled counterfactual; chronology alone is insufficient.
        if previous.outcome != Some(PlanRunOutcome::Failure) {
            previous.outcome = Some(PlanRunOutcome::Inconclusive);
        }
        self.connection.execute(
            "INSERT INTO plan_runs(id,plan,data) VALUES(?1,?2,?3)",
            params![
                run.id.to_string(),
                run.plan.plan.to_string(),
                serde_json::to_string(&run)?
            ],
        )?;
        self.update_plan_run(&previous)?;
        self.plan_event(
            &run.plan.plan,
            Some(&run.id),
            "plan_replanned",
            &PlanReplanEvent {
                from: previous.plan,
                to: run.plan.clone(),
                trigger: reason,
                evidence: vec![],
                created_at: run.started_at,
            },
        )?;
        tx.commit()?;
        Ok(run)
    }
    /// Record failure from the actual effect ledger, including effects committed before a step finished.
    pub fn reconcile_plan_failure(
        &self,
        id: &PlanRunId,
        reason: &str,
    ) -> Result<PlanRecoveryContext> {
        if reason.trim().is_empty() {
            return Err(Error::InvalidInput("Failure needs a reason".into()));
        }
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let mut run = self.plan_run(id)?;
        if run.finished_at.is_some() {
            return Err(Error::Intervention("Plan run is terminal".into()));
        }
        let plan = self.plan_revision(&run.plan.plan, run.plan.revision)?;
        for point in &plan.commitment_points {
            let committed = point
                .consequences
                .iter()
                .map(|c| self.commit_receipt_for_effect(&c.effect))
                .collect::<Result<Vec<_>>>()?
                .iter()
                .any(Option::is_some);
            if committed && !run.state.crossed_commitment_points.contains(&point.id) {
                run.state.crossed_commitment_points.push(point.id.clone());
                for assumption in plan
                    .assumptions
                    .iter()
                    .filter(|a| point.assumptions_invalidated.contains(&a.id))
                {
                    if let Some(key) = predicate_key(&assumption.predicate) {
                        run.state.observations.retain(|c| c.key != key);
                        run.state.observation_epochs.remove(key);
                    }
                }
            }
        }
        run.outcome = Some(PlanRunOutcome::Failure);
        self.update_plan_run(&run)?;
        let recovery = self.plan_recovery_context(id)?;
        run.state.committed_effects = recovery.committed_effects.clone();
        run.state.nested_commitments = recovery.plan_state.nested_commitments.clone();
        self.update_plan_run(&run)?;
        self.plan_event(
            &plan.id,
            Some(id),
            "plan_failure_reconciled",
            &json!({"reason":reason,"recovery":recovery}),
        )?;
        tx.commit()?;
        Ok(recovery)
    }
    pub fn investigate_plan_drift(
        &self,
        id: &PlanRunId,
        mut input: crate::causal::CausalInvestigationInput,
    ) -> Result<crate::causal::CausalInvestigation> {
        let run = self.plan_run(id)?;
        input.target = crate::causal::CausalTarget::Outcome(format!("plan-run:{}", run.id));
        let investigation = self.create_causal_investigation(&input)?;
        self.plan_event(
            &run.plan.plan,
            Some(id),
            "plan_drift_investigation",
            &investigation.id,
        )?;
        Ok(investigation)
    }
    pub fn plan_forecasts(
        &self,
        id: &PlanRunId,
    ) -> Result<Vec<crate::predictive::FailureForecast>> {
        let run = self.plan_run(id)?;
        match run.trajectory {
            Some(id) => self.forecast_trajectory(&id),
            None => Ok(vec![]),
        }
    }
    /// Capture a verified boundary without granting execution authority.
    pub fn capture_plan_checkpoint(
        &self,
        id: &PlanRunId,
        checkpoint: &PlanCheckpointId,
        context: &RuntimeDecisionContext,
    ) -> Result<PlanCheckpointSnapshot> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let mut run = self.plan_run(id)?;
        let plan = self.plan_revision(&run.plan.plan, run.plan.revision)?;
        let definition = plan
            .checkpoints
            .iter()
            .find(|c| &c.id == checkpoint)
            .ok_or_else(|| Error::InvalidInput("Unknown plan checkpoint".into()))?;
        if run.finished_at.is_some()
            || definition
                .after_step
                .as_ref()
                .is_some_and(|s| !run.state.completed_steps.contains(s))
        {
            return Err(Error::Intervention(
                "Checkpoint boundary has not been reached".into(),
            ));
        }
        let assessment = self.assess_plan_run(id, context, false)?;
        if !matches!(
            assessment.status,
            PlanValidityStatus::Valid | PlanValidityStatus::ValidWithWarnings
        ) || !assessment
            .checkpoints
            .iter()
            .any(|c| &c.checkpoint == checkpoint && c.status == CheckpointStatus::Satisfied)
        {
            return Err(Error::Intervention(
                "Checkpoint requires current verified evidence".into(),
            ));
        }
        run.state.reached_checkpoints.push(checkpoint.clone());
        let recovery = self.plan_recovery_context(id)?;
        run.state.nested_commitments = recovery.plan_state.nested_commitments.clone();
        run.state.committed_effects = recovery.committed_effects.clone();
        let snapshot = PlanCheckpointSnapshot {
            id: PlanCheckpointSnapshotId::new(),
            checkpoint: checkpoint.clone(),
            plan_revision: plan.revision,
            state_claims: run.state.observations.clone(),
            knowledge_snapshot: context
                .operational_knowledge
                .as_ref()
                .map(|k| k.snapshot.id.clone()),
            effect_state: recovery.committed_effects,
            state: run.state.clone(),
            captured_at: Utc::now(),
        };
        self.connection.execute(
            "INSERT INTO plan_checkpoint_snapshots(id,run,data) VALUES(?1,?2,?3)",
            params![
                snapshot.id.to_string(),
                id.to_string(),
                serde_json::to_string(&snapshot)?
            ],
        )?;
        self.update_plan_run(&run)?;
        self.plan_event(&plan.id, Some(id), "plan_checkpoint", &snapshot)?;
        tx.commit()?;
        Ok(snapshot)
    }
    pub fn plan_checkpoint_snapshot(
        &self,
        id: &PlanCheckpointSnapshotId,
    ) -> Result<PlanCheckpointSnapshot> {
        let data: String = self.connection.query_row(
            "SELECT data FROM plan_checkpoint_snapshots WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&data)?)
    }
    /// Resume assesses the live run; a snapshot never overwrites subsequently observed effects.
    pub fn resume_plan_checkpoint(
        &self,
        id: &PlanRunId,
        snapshot: &PlanCheckpointSnapshotId,
        context: &RuntimeDecisionContext,
    ) -> Result<PlanValidityAssessment> {
        let saved = self.plan_checkpoint_snapshot(snapshot)?;
        let owner: String = self.connection.query_row(
            "SELECT run FROM plan_checkpoint_snapshots WHERE id=?1",
            [snapshot.to_string()],
            |r| r.get(0),
        )?;
        let run = self.plan_run(id)?;
        if owner != id.to_string()
            || saved.state.plan != run.plan.plan
            || saved.plan_revision != run.plan.revision
            || run.finished_at.is_some()
        {
            return Err(Error::Intervention(
                "Checkpoint does not belong to this active run revision".into(),
            ));
        }
        let assessment = self.assess_plan_run(id, context, true)?;
        self.plan_event(
            &run.plan.plan,
            Some(id),
            "plan_resume_assessed",
            &assessment,
        )?;
        Ok(assessment)
    }
    pub fn plan_recovery_context(&self, id: &PlanRunId) -> Result<PlanRecoveryContext> {
        let mut run = self.plan_run(id)?;
        let plan = self.plan_revision(&run.plan.plan, run.plan.revision)?;
        let mut effects = std::collections::BTreeSet::new();
        for effect in &run.state.committed_effects {
            if self.commit_receipt_for_effect(effect)?.is_some() {
                effects.insert(effect.clone());
            }
        }
        for step in &plan.steps {
            if let PlanStepKind::Effect(id) = &step.kind {
                for effect in self.effect_plan(id)?.effects {
                    if self.commit_receipt_for_effect(&effect)?.is_some() {
                        effects.insert(effect);
                    }
                }
            }
        }
        for pin in plan.component_revisions.values() {
            if let PlanComponentRevision::Composition { id, revision, .. } = pin {
                let composition = self.composition_revision(id, *revision)?;
                let mut nested = run
                    .state
                    .nested_commitments
                    .get(id)
                    .cloned()
                    .unwrap_or_default();
                for effect in &composition.contract.effect_policy.effects {
                    if self.commit_receipt_for_effect(&effect.effect)?.is_some() {
                        if !nested.committed_effects.contains(&effect.effect) {
                            nested.committed_effects.push(effect.effect.clone());
                        }
                        effects.insert(effect.effect.clone());
                    }
                }
                for point in &composition.contract.effect_policy.commit_points {
                    if point
                        .irreversible_effects
                        .iter()
                        .any(|e| nested.committed_effects.contains(e))
                        && !nested.reached.contains(&point.id)
                    {
                        nested.reached.push(point.id.clone());
                    }
                }
                run.state.nested_commitments.insert(id.clone(), nested);
            }
        }
        for point in &plan.commitment_points {
            for consequence in &point.consequences {
                if self
                    .commit_receipt_for_effect(&consequence.effect)?
                    .is_some()
                {
                    effects.insert(consequence.effect.clone());
                }
            }
        }
        Ok(PlanRecoveryContext {
            failure_step: run.state.current_step.clone(),
            crossed_commitments: run.state.crossed_commitment_points.clone(),
            committed_effects: effects.into_iter().collect(),
            plan_state: run.state,
        })
    }
    pub fn bind_plan_authorization(
        &self,
        id: &PlanRunId,
        authorization: &CommitAuthorizationId,
    ) -> Result<()> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let mut run = self.plan_run(id)?;
        if run.finished_at.is_some() {
            return Err(Error::Intervention("Plan run is terminal".into()));
        }
        let exists: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM commit_authorizations WHERE id=?1)",
            [authorization.to_string()],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::InvalidInput("Unknown external authorization".into()));
        }
        if !run.state.authorizations.contains(authorization) {
            run.state.authorizations.push(authorization.clone());
        }
        self.update_plan_run(&run)?;
        self.plan_event(
            &run.plan.plan,
            Some(id),
            "plan_authorization_bound",
            authorization,
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn plan_replay(&self, id: &PlanRunId) -> Result<Value> {
        let run = self.plan_run(id)?;
        let plan = self.plan_revision(&run.plan.plan, run.plan.revision)?;
        let mut statements = self
            .connection
            .prepare("SELECT data FROM plan_step_runs WHERE run=?1 ORDER BY rowid")?;
        let steps: Vec<PlanStepRun> = statements
            .query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect::<Result<_>>()?;
        let decisions = steps
            .iter()
            .map(|s| self.runtime_decision(&s.decision))
            .collect::<Result<Vec<_>>>()?;
        Ok(
            json!({"original_plan":plan,"run":run,"steps":steps,"original_decisions":decisions,"history":self.plan_history(&run.plan.plan)?}),
        )
    }
    pub fn compile_plan_curriculum(
        &self,
        plan: &PlanRevisionRef,
        skill: &str,
        requests: &[crate::experimentation::ExperimentRequest],
        kind: crate::curriculum::CurriculumGoalKind,
        budget: &crate::budget::ExperienceBudget,
    ) -> Result<crate::curriculum::Curriculum> {
        use crate::curriculum::*;
        if requests.is_empty()
            || !matches!(
                kind,
                CurriculumGoalKind::ValidatePlan
                    | CurriculumGoalKind::ValidateCheckpoint
                    | CurriculumGoalKind::ValidateCommitmentGate
                    | CurriculumGoalKind::ValidatePlanRecovery
                    | CurriculumGoalKind::ReduceUnnecessaryReplans
            )
        {
            return Err(Error::InvalidInput(
                "Plan curriculum needs concrete requests and a plan goal".into(),
            ));
        }
        let definition = self.plan_revision(&plan.plan, plan.revision)?;
        let skill = self.skill(skill)?;
        let goal = CurriculumGoal {
            id: CurriculumGoalId::new(),
            kind,
            description: "Validate an explicit plan under supplied interventions".into(),
            priority: Priority::High,
            score: PriorityScore {
                score: 80,
                priority: Priority::High,
                explanation: "Cross-step evidence gap".into(),
            },
            evidence_gap: EvidenceGap {
                dimension: "plan-continuity".into(),
                known_values: vec![],
                unknown_values: vec!["sequence validity".into()],
                rationale: "Initial evidence does not establish continued validity".into(),
            },
            status: GoalStatus::Planned,
            decision: CurriculumDecision::Approved,
            reason: "Bounded isolated validation".into(),
            severity: Severity::High,
            safety: TrialSafety::RequiresIsolation,
        };
        let trials = requests
            .iter()
            .map(|request| {
                if request.candidates.iter().any(|c| {
                    !matches!(
                        c.execution,
                        crate::experimentation::CandidateExecution::Shell { .. }
                    )
                }) {
                    return Err(Error::InvalidInput(
                        "Plan validation requires explicit local controlled commands".into(),
                    ));
                }
                Ok(CurriculumTrial {
                    id: CurriculumTrialId::new(),
                    goal_id: goal.id.clone(),
                    skill_id: skill.id.clone(),
                    condition: format!("plan:{}:{}", definition.id, definition.revision),
                    fingerprint: composition_hash(request)?,
                    intent: TrialIntent::Revalidation,
                    execution: TrialExecution::Experiment {
                        request: Box::new(request.clone()),
                    },
                    result: None,
                    learning_outcome: None,
                    status: GoalStatus::Planned,
                    estimated_budget: crate::budget::ExperienceUsage {
                        realities: request.candidates.len(),
                        ..Default::default()
                    },
                    expected_value: "Compare continuation and revalidation under controlled drift"
                        .into(),
                    required_isolation: RealityCapabilities::default(),
                    round: 1,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let now = chrono::Utc::now();
        let curriculum = Curriculum {
            id: CurriculumId::new(),
            target: CurriculumTarget::Skill(skill.id),
            profile: "plan-continuity".into(),
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
        self.plan_event(&plan.plan, None, "plan_curriculum_created", &curriculum.id)?;
        Ok(curriculum)
    }
    pub fn plan_opportunity(
        &self,
        c: &ExecutionPlan,
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
            kind: ExperienceOpportunityKind::ValidatePlan,
            target: ExperienceOpportunityTarget::RuntimeGap(c.id.to_string()),
            reasons: vec![OpportunityReason::Custom(
                "Plan drift and recovery evidence gap".into(),
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
                rationale: vec!["Initial support does not establish continued validity".into()],
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
            .ok_or_else(|| Error::InvalidInput("No plan opportunity generated".into()))?;
        self.save_experience_opportunity(&opportunity)?;
        Ok(opportunity)
    }
}
