// SPDX-License-Identifier: Apache-2.0
use crate::{
    Error, Result,
    causal::*,
    core::*,
    epistemic::{
        DeterministicEvidenceDiversityPolicy, EpistemicDependencySet, EvidenceContext,
        EvidenceDiversityPolicy, EvidenceOutcome, EvidencePath, EvidenceSource,
    },
    experimentation::StrategyExperiment,
    store::Store,
};
use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

fn json(v: &impl serde::Serialize) -> Result<String> {
    Ok(serde_json::to_string(v)?)
}
fn event(tx: &Transaction<'_>, subject: &str, kind: CausalEventKind) -> Result<()> {
    tx.execute(
        "INSERT INTO bridge_events(session_id,kind,data) VALUES('causal-local',?1,?2)",
        params![
            serde_json::to_value(kind)?
                .as_str()
                .unwrap_or("causal_event"),
            json(&serde_json::json!({"subject":subject}))?
        ],
    )?;
    tx.execute(
        "INSERT INTO causal_events(subject,kind) VALUES(?1,?2)",
        params![subject, json(&kind)?],
    )?;
    Ok(())
}
fn revision(tx: &Transaction<'_>, h: &CausalHypothesis) -> Result<()> {
    tx.execute(
        "UPDATE causal_hypotheses SET status=?2,data=?3 WHERE id=?1",
        params![h.id.to_string(), json(&h.status)?, json(h)?],
    )?;
    tx.execute("INSERT INTO causal_hypothesis_revisions(hypothesis_id,revision,data) SELECT ?1,COALESCE(MAX(revision),0)+1,?2 FROM causal_hypothesis_revisions WHERE hypothesis_id=?1",params![h.id.to_string(),json(h)?])?;
    Ok(())
}
impl Store {
    pub fn causal_observations(
        &self,
        id: &CausalInvestigationId,
    ) -> Result<Vec<CausalObservation>> {
        let mut statement = self.connection.prepare(
            "SELECT data FROM causal_observations WHERE investigation_id=?1 ORDER BY rowid",
        )?;
        statement
            .query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub fn causal_observations_for_hypothesis(
        &self,
        id: &CausalHypothesisId,
    ) -> Result<Vec<CausalObservation>> {
        let mut result = vec![];
        for inv in self.causal_investigations()? {
            if inv.hypotheses.contains(id) {
                result.extend(self.causal_observations(&inv.id)?);
            }
        }
        result.sort_by_key(|o| o.experience.clone());
        result.dedup_by_key(|o| o.experience.clone());
        Ok(result)
    }
    pub fn causal_runtime_guidance(
        &self,
        query: &crate::retrieval::QueryContext,
        failure: Option<&str>,
    ) -> Result<CausalRuntimeGuidance> {
        let mut guidance = CausalRuntimeGuidance::default();
        let Some(failure) = failure else {
            return Ok(guidance);
        };
        let mut statement=self.connection.prepare("SELECT data FROM causal_investigations WHERE json_extract(data,'$.target.kind')='failure_signature' AND json_extract(data,'$.target.target')=?1 ORDER BY rowid")?;
        let investigations = statement
            .query_map([failure], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str::<CausalInvestigation>(&r?)?))
            .collect::<Result<Vec<_>>>()?;
        for inv in investigations {
            let spec = self.causal_test_spec(&inv.id)?;
            if !spec.scope.matches(&query.experience_context()) {
                continue;
            }
            let exact = spec.starting_state.state_ref.git_commit == query.repository.commit
                && spec.baseline.iter().all(|(id, value)| {
                    spec.variables
                        .iter()
                        .find(|v| v.id == *id)
                        .is_some_and(|v| {
                            query.environment.facts.get(&v.name) == Some(&value.literal())
                        })
                });
            for h in self.causal_investigation_hypotheses(&inv.id)? {
                guidance.applicable_hypotheses.push(h.id.clone());
                if h.status.supported() && exact {
                    let evidence = self.causal_evidence(&h.id)?;
                    if let Some(e) = evidence.iter().rev().find(|e| {
                        e.outcome == CausalEvidenceOutcome::Supports
                            && e.conditions == spec.baseline
                            && e.context == spec.scope
                            && self.causal_run(&e.intervention).is_ok_and(|run| {
                                run.request.starting_state.state_ref.git_commit
                                    == query.repository.commit
                            })
                    }) {
                        let run = self.causal_run(&e.intervention)?;
                        let deps = self.causal_dependencies(&h.id)?;
                        let recovery = deps.into_iter().find_map(|d| {
                            if d.intervention.as_ref() == Some(&e.intervention) {
                                if let CausalArtifact::Recovery(id) = d.artifact {
                                    Some(id)
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        });
                        guidance
                            .supported_interventions
                            .push(InterventionRecommendation {
                                hypothesis: h.id,
                                intervention: run.discrimination.intervention,
                                controlled_pairs: evidence
                                    .iter()
                                    .filter(|e| e.outcome == CausalEvidenceOutcome::Supports)
                                    .count(),
                                recovery,
                            });
                    } else {
                        guidance.causal_gaps.push(CausalGap {
                            description:
                                "Supported elsewhere but untested at these exact input values"
                                    .into(),
                            related_variables: vec![h.cause],
                            reason: CausalGapReason::Untested,
                        });
                    }
                } else {
                    guidance.causal_gaps.push(CausalGap {description:format!("{}: {:?}; runtime requires matching input values and repository version; revalidate after drift",h.statement,h.status),related_variables:vec![h.cause],reason:if h.status==CausalHypothesisStatus::Contradicted {CausalGapReason::Contradictory} else {CausalGapReason::InsufficientEvidence}});
                }
            }
        }
        guidance
            .supported_interventions
            .sort_by_key(|i| (i.recovery.is_none(), i.hypothesis.clone()));
        Ok(guidance)
    }
    pub fn causal_envelope_observations(
        &self,
        id: &CausalInvestigationId,
    ) -> Result<Vec<EnvelopeObservation>> {
        let spec = self.causal_test_spec(id)?;
        let mut observations = vec![];
        let mut seen = std::collections::BTreeSet::new();
        for h in self.causal_investigation_hypotheses(id)? {
            for e in self.causal_evidence(&h.id)? {
                if !seen.insert(e.pair.clone()) {
                    continue;
                }
                let run = self.causal_run(&e.intervention)?;
                let names =
                    |values: &std::collections::BTreeMap<CausalVariableId, VariableValue>| {
                        values
                            .iter()
                            .filter_map(|(id, value)| {
                                spec.variables
                                    .iter()
                                    .find(|v| v.id == *id)
                                    .map(|v| (v.name.clone(), value.clone()))
                            })
                            .collect()
                    };
                if let Some(baseline) = e.baseline_trial {
                    observations.push(EnvelopeObservation {
                        conditions: names(&run.baseline),
                        outcome: e.baseline_outcome,
                        evidence: vec![baseline],
                    });
                }
                observations.push(EnvelopeObservation {
                    conditions: names(&run.changed),
                    outcome: e.intervention_outcome,
                    evidence: vec![e.intervention_trial],
                });
            }
        }
        Ok(observations)
    }
    pub fn causal_curriculum_goals(
        &self,
        id: &CausalInvestigationId,
    ) -> Result<Vec<crate::curriculum::CurriculumGoal>> {
        use crate::curriculum::*;
        Ok(self.causal_investigation_hypotheses(id)?.into_iter().filter(|h|!h.status.supported()).map(|h|{
            let contradiction=h.status==CausalHypothesisStatus::Contradicted;
            CurriculumGoal {id:CurriculumGoalId::new(),kind:if contradiction {CurriculumGoalKind::ResolveCausalContradiction} else {CurriculumGoalKind::DiscriminateHypotheses},description:h.statement,priority:Priority::High,score:PriorityScore {score:if contradiction {90} else {70},priority:Priority::High,explanation:"Unresolved scoped failure mechanism; prefer discriminating controlled interventions".into()},evidence_gap:EvidenceGap {dimension:h.cause.to_string(),known_values:vec![],unknown_values:vec!["controlled mechanism".into()],rationale:"No rhetorical voting; causal planner supplies explicit tests".into()},status:GoalStatus::Planned,decision:CurriculumDecision::RequiresApproval,reason:"Recommendation only; existing ExperienceBudget and Reality capability checks apply".into(),severity:Severity::High,safety:TrialSafety::Safe}
        }).collect())
    }
    pub fn causal_hypothesis(&self, id: &CausalHypothesisId) -> Result<CausalHypothesis> {
        self.get(
            "SELECT data FROM causal_hypotheses WHERE id=?1",
            &id.to_string(),
        )
    }
    pub fn causal_hypotheses(&self) -> Result<Vec<CausalHypothesis>> {
        self.list("SELECT data FROM causal_hypotheses ORDER BY id")
    }
    pub fn causal_evidence(&self, id: &CausalHypothesisId) -> Result<Vec<CausalEvidence>> {
        let mut stmt = self
            .connection
            .prepare("SELECT data FROM causal_evidence WHERE hypothesis_id=?1 ORDER BY rowid")?;
        stmt.query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub fn causal_investigation(&self, id: &CausalInvestigationId) -> Result<CausalInvestigation> {
        self.get(
            "SELECT data FROM causal_investigations WHERE id=?1",
            &id.to_string(),
        )
    }
    pub fn causal_investigations(&self) -> Result<Vec<CausalInvestigation>> {
        self.list("SELECT data FROM causal_investigations ORDER BY rowid")
    }
    pub fn causal_test_spec(&self, id: &CausalInvestigationId) -> Result<CausalTestSpec> {
        self.get(
            "SELECT spec FROM causal_investigations WHERE id=?1",
            &id.to_string(),
        )
    }
    pub fn causal_investigation_hypotheses(
        &self,
        id: &CausalInvestigationId,
    ) -> Result<Vec<CausalHypothesis>> {
        self.causal_investigation(id)?
            .hypotheses
            .iter()
            .map(|id| self.causal_hypothesis(id))
            .collect()
    }
    pub fn causal_run(&self, id: &InterventionId) -> Result<CausalRun> {
        self.get(
            "SELECT data FROM interventions WHERE id=?1",
            &id.to_string(),
        )
    }
    pub fn causal_model(&self, id: &CausalModelId) -> Result<ContextualCausalModel> {
        self.get(
            "SELECT data FROM causal_models WHERE id=?1 ORDER BY revision DESC LIMIT 1",
            &id.to_string(),
        )
    }
    pub fn causal_models(&self) -> Result<Vec<ContextualCausalModel>> {
        self.list("SELECT data FROM causal_models m WHERE revision=(SELECT MAX(revision) FROM causal_models WHERE id=m.id) ORDER BY id")
    }
    pub fn causal_model_history(&self, id: &CausalModelId) -> Result<Vec<ContextualCausalModel>> {
        let mut stmt = self
            .connection
            .prepare("SELECT data FROM causal_models WHERE id=?1 ORDER BY revision")?;
        stmt.query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }

    pub fn create_causal_investigation(
        &self,
        input: &CausalInvestigationInput,
    ) -> Result<CausalInvestigation> {
        validate_spec(&input.spec)?;
        if input.hypotheses.is_empty() || input.hypotheses.len() > 64 {
            return Err(Error::InvalidInput(
                "Supply 1..64 explicit hypotheses".into(),
            ));
        }
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        for v in &input.spec.variables {
            let prior: Option<String> = tx
                .query_row(
                    "SELECT data FROM causal_variables WHERE id=?1",
                    [v.id.to_string()],
                    |r| r.get(0),
                )
                .optional()?;
            if prior
                .as_ref()
                .is_some_and(|p| p != &json(v).unwrap_or_default())
            {
                return Err(Error::InvalidInput(
                    "Variable identity reused with changed domain or semantics".into(),
                ));
            }
            tx.execute(
                "INSERT OR IGNORE INTO causal_variables(id,data) VALUES(?1,?2)",
                params![v.id.to_string(), json(v)?],
            )?;
        }
        let mut ids = vec![];
        for proposed in &input.hypotheses {
            if proposed.statement.trim().is_empty()
                || proposed.cause == proposed.effect
                || proposed.scope != input.spec.scope
                || !input.spec.variables.iter().any(|v| v.id == proposed.cause)
                || !input
                    .spec
                    .variables
                    .iter()
                    .any(|v| v.id == proposed.effect && v.kind == CausalVariableKind::Outcome)
                || proposed
                    .conditions
                    .iter()
                    .any(|c| !input.spec.variables.iter().any(|v| v.id == c.variable))
            {
                return Err(Error::InvalidInput("Hypothesis needs known cause/outcome variables and the exact explicit test scope".into()));
            }
            let hash = blake3::hash(&serde_json::to_vec(&(
                &proposed.claim,
                &proposed.cause,
                &proposed.effect,
                &proposed.scope,
                &proposed.conditions,
                proposed.baseline_prediction,
                proposed.intervention_prediction,
            ))?)
            .to_hex()
            .to_string();
            let existing: Option<String> = tx
                .query_row(
                    "SELECT id FROM causal_hypotheses WHERE canonical_hash=?1",
                    [&hash],
                    |r| r.get(0),
                )
                .optional()?;
            let id = if let Some(id) = existing {
                id.parse()?
            } else {
                let mut h = proposed.clone();
                h.status = CausalHypothesisStatus::Candidate;
                h.evidence.clear();
                h.updated_at = Utc::now();
                tx.execute("INSERT INTO causal_hypotheses(id,canonical_hash,status,data) VALUES(?1,?2,?3,?4)",params![h.id.to_string(),hash,json(&h.status)?,json(&h)?])?;
                revision(&tx, &h)?;
                for (p, c) in h.conditions.iter().enumerate() {
                    tx.execute("INSERT INTO causal_conditions(hypothesis_id,position,data) VALUES(?1,?2,?3)",params![h.id.to_string(),p as i64,json(c)?])?;
                }
                event(
                    &tx,
                    &h.id.to_string(),
                    CausalEventKind::CausalHypothesisCreated,
                )?;
                h.id
            };
            if proposed.remote_origin.is_some() {
                tx.execute(
                    "INSERT INTO causal_remote_claims(hypothesis_id,data) VALUES(?1,?2)",
                    params![id.to_string(), json(proposed)?],
                )?;
            }
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        let inv = CausalInvestigation {
            id: CausalInvestigationId::new(),
            target: input.target.clone(),
            hypotheses: ids,
            interventions: vec![],
            evidence: vec![],
            status: InvestigationStatus::Open,
        };
        tx.execute(
            "INSERT INTO causal_investigations(id,model_id,data,spec) VALUES(?1,?2,?3,?4)",
            params![
                inv.id.to_string(),
                CausalModelId::new().to_string(),
                json(&inv)?,
                json(&input.spec)?
            ],
        )?;
        for id in &input.source_experiences {
            let experience = self.experience(id)?;
            if !input.spec.scope.matches(&experience.context) {
                return Err(Error::InvalidInput(
                    "Source observation is outside investigation scope".into(),
                ));
            }
            tx.execute("INSERT OR IGNORE INTO causal_observations(investigation_id,experience_id,data) VALUES(?1,?2,?3)",params![inv.id.to_string(),id.to_string(),json(&extract_causal_observation(&experience,&input.spec.variables))?])?;
        }
        tx.commit()?;
        self.revise_causal_model(&inv.id)?;
        Ok(inv)
    }
    pub fn plan_causal_investigation(
        &self,
        id: &CausalInvestigationId,
        config: &crate::bridge::config::Config,
    ) -> Result<InterventionPlan> {
        let spec = self.causal_test_spec(id)?;
        let context = planning_context(&spec, self, config)?;
        let hypotheses = self.causal_investigation_hypotheses(id)?;
        let plan = DeterministicInterventionPlanner {
            budget: spec.causal_budget.clone(),
        }
        .plan(&hypotheses, &context, &spec.budget)?;
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        for mut h in hypotheses {
            if matches!(
                h.status,
                CausalHypothesisStatus::Candidate
                    | CausalHypothesisStatus::Untestable
                    | CausalHypothesisStatus::Testable
            ) {
                h.status = if plan.untestable.contains(&h.id) {
                    CausalHypothesisStatus::Untestable
                } else {
                    CausalHypothesisStatus::Testable
                };
                h.updated_at = Utc::now();
                revision(&tx, &h)?;
            }
        }
        for experiment in &plan.experiments {
            event(
                &tx,
                &experiment.intervention.id.to_string(),
                CausalEventKind::CausalInterventionPlanned,
            )?;
        }
        tx.commit()?;
        Ok(plan)
    }
    pub(crate) fn start_causal_run(&self, run: &CausalRun) -> Result<()> {
        let spec = self.causal_test_spec(&run.investigation)?;
        let compiled = compile_intervention(&run.investigation, &spec, &run.discrimination)?;
        if json(&compiled.baseline)? != json(&run.baseline)?
            || json(&compiled.changed)? != json(&run.changed)?
            || json(&compiled.known_confounders)? != json(&run.known_confounders)?
            || compiled.scope != run.scope
            || compiled
                .request
                .candidates
                .iter()
                .zip(&run.request.candidates)
                .any(|(a, b)| json(&a.execution).ok() != json(&b.execution).ok())
            || run.request.candidates.len() != 2
            || compiled.request.evaluator != run.request.evaluator
            || json(&compiled.request.starting_state.state_ref)?
                != json(&run.request.starting_state.state_ref)?
            || compiled.request.budget != run.request.budget
            || compiled.request.origin != run.request.origin
            || compiled.request.intent != run.request.intent
            || json(&compiled.request.capabilities)? != json(&run.request.capabilities)?
            || run.request.capabilities.allow_external_mutations
            || run.request.capabilities.allow_network
        {
            return Err(Error::InvalidInput(
                "Causal run differs from its registered, controlled fixture adapter".into(),
            ));
        }
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let mut inv = self.causal_investigation(&run.investigation)?;
        if inv.interventions.len() >= spec.causal_budget.max_interventions {
            return Err(Error::Intervention("Causal intervention budget exhausted; create a new explicit investigation or replay".into()));
        }
        tx.execute(
            "INSERT INTO interventions(id,investigation_id,request_id,data) VALUES(?1,?2,?3,?4)",
            params![
                run.discrimination.intervention.id.to_string(),
                inv.id.to_string(),
                run.request.id.to_string(),
                json(run)?
            ],
        )?;
        inv.interventions
            .push(run.discrimination.intervention.id.clone());
        inv.status = InvestigationStatus::Testing;
        tx.execute(
            "UPDATE causal_investigations SET data=?2 WHERE id=?1",
            params![inv.id.to_string(), json(&inv)?],
        )?;
        event(
            &tx,
            &run.discrimination.intervention.id.to_string(),
            CausalEventKind::CausalInterventionPlanned,
        )?;
        event(
            &tx,
            &run.discrimination.intervention.id.to_string(),
            CausalEventKind::CausalInterventionStarted,
        )?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn finish_causal_run(
        &self,
        run: &CausalRun,
        e: &StrategyExperiment,
        pair: &CounterfactualPair,
    ) -> Result<Vec<CausalEvidence>> {
        let mut inv = self.causal_investigation(&run.investigation)?;
        let mut evidence = vec![];
        let result = e
            .result
            .as_ref()
            .ok_or_else(|| Error::InvalidInput("Missing experiment result".into()))?;
        let baseline = result
            .candidates
            .iter()
            .find(|c| c.candidate_id == pair.baseline.candidate)
            .ok_or_else(|| Error::InvalidInput("Missing baseline".into()))?;
        let changed = result
            .candidates
            .iter()
            .find(|c| c.candidate_id == pair.intervention.candidate)
            .ok_or_else(|| Error::InvalidInput("Missing intervention".into()))?;
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO counterfactual_pairs(id,intervention_id,experiment_id,data) VALUES(?1,?2,?3,?4)",params![pair.id.to_string(),run.discrimination.intervention.id.to_string(),e.id.to_string(),json(pair)?])?;
        for mut h in self.causal_investigation_hypotheses(&inv.id)? {
            if h.cause != run.discrimination.intervention.variable
                || h.scope != run.scope
                || !h.conditions.iter().all(|c| {
                    run.baseline
                        .get(&c.variable)
                        .is_some_and(|v| c.predicate.matches(v))
                })
            {
                continue;
            }
            let item = CausalEvidence {
                id: CausalEvidenceId::new(),
                hypothesis_id: h.id.clone(),
                intervention: run.discrimination.intervention.id.clone(),
                baseline_trial: Some(pair.baseline.clone()),
                intervention_trial: pair.intervention.clone(),
                pair: pair.id.clone(),
                outcome: classify_evidence(
                    &h,
                    &run.discrimination.intervention,
                    trial_outcome(baseline),
                    trial_outcome(changed),
                    pair.quality.quality,
                ),
                kind: CausalEvidenceKind::Interventional,
                experiment_quality: pair.quality.quality,
                baseline_outcome: trial_outcome(baseline),
                intervention_outcome: trial_outcome(changed),
                context: run.scope.clone(),
                conditions: run.baseline.clone(),
                dependencies: EpistemicDependencySet {
                    evaluators: vec![
                        blake3::hash(json(&e.request.evaluator)?.as_bytes())
                            .to_hex()
                            .to_string(),
                    ],
                    environment_family: Some(format!(
                        "{}:fixture:{}",
                        pair.starting_state.environment_fingerprint,
                        pair.starting_state.state_ref.git_commit
                    )),
                    agent_runtime: Some("hardknock-local-shell".into()),
                    ..Default::default()
                },
                created_at: Utc::now(),
            };
            tx.execute(
                "INSERT INTO causal_evidence(id,hypothesis_id,pair_id,data) VALUES(?1,?2,?3,?4)",
                params![
                    item.id.to_string(),
                    h.id.to_string(),
                    pair.id.to_string(),
                    json(&item)?
                ],
            )?;
            let mut all = self.causal_evidence(&h.id)?;
            // Same connection sees the transaction's inserted evidence.
            if !all.iter().any(|x| x.id == item.id) {
                all.push(item.clone());
            }
            let paths: Vec<_> = all
                .iter()
                .filter(|e| e.outcome == CausalEvidenceOutcome::Supports)
                .map(|e| EvidencePath {
                    id: EvidencePathId::new(),
                    claim: ClaimId::new().into(),
                    source: EvidenceSource::Experiment {
                        experiment_id: e.intervention_trial.experiment.clone(),
                    },
                    context: EvidenceContext {
                        root_evidence_origins: vec![e.intervention_trial.experiment.to_string()],
                        fingerprint: crate::epistemic::context_fingerprint(&e.dependencies)
                            .unwrap_or_default(),
                        ..Default::default()
                    },
                    dependencies: e.dependencies.clone(),
                    evidence_refs: vec![],
                    outcome: EvidenceOutcome::Supports,
                    created_at: e.created_at,
                })
                .collect();
            let diversity = DeterministicEvidenceDiversityPolicy.assess(&paths);
            h.status =
                DeterministicCausalSupportPolicy::default().evaluate(&h, &all, Some(&diversity));
            h.evidence.push(item.id.clone());
            h.updated_at = Utc::now();
            revision(&tx, &h)?;
            event(
                &tx,
                &h.id.to_string(),
                CausalEventKind::CausalEvidenceRecorded,
            )?;
            if h.status.supported() {
                event(
                    &tx,
                    &h.id.to_string(),
                    CausalEventKind::CausalHypothesisSupported,
                )?;
            }
            if h.status == CausalHypothesisStatus::Contradicted {
                event(
                    &tx,
                    &h.id.to_string(),
                    CausalEventKind::CausalHypothesisContradicted,
                )?;
                for dep in self.causal_dependencies(&h.id)? {
                    let review=CausalRevalidation { hypothesis:h.id.clone(), artifact:dep.artifact.clone(), reason:"Controlled causal contradiction; review scope and revalidate derived behavior".into(), automatic_guidance_quarantined:true, created_at:Utc::now() };
                    tx.execute("INSERT INTO causal_revalidations(hypothesis_id,artifact_id,data) VALUES(?1,?2,?3)",params![h.id.to_string(),dep.artifact.key(),json(&review)?])?;
                    event(
                        &tx,
                        &dep.artifact.key(),
                        CausalEventKind::CausalArtifactRevalidationRequested,
                    )?;
                }
            }
            inv.evidence.push(item.id.clone());
            evidence.push(item);
        }
        let statuses: Vec<_> = inv
            .hypotheses
            .iter()
            .map(|id| self.causal_hypothesis(id))
            .collect::<Result<_>>()?;
        inv.status = if statuses.iter().any(|h| h.status.supported())
            && statuses
                .iter()
                .all(|h| h.status.supported() || h.status == CausalHypothesisStatus::Contradicted)
        {
            InvestigationStatus::ResolvedUnderScope
        } else if statuses.iter().any(|h| h.status.supported()) {
            InvestigationStatus::Narrowed
        } else {
            InvestigationStatus::Inconclusive
        };
        tx.execute(
            "UPDATE causal_investigations SET data=?2 WHERE id=?1",
            params![inv.id.to_string(), json(&inv)?],
        )?;
        tx.commit()?;
        for affected in self.causal_investigations()? {
            if affected
                .hypotheses
                .iter()
                .any(|h| inv.hypotheses.contains(h))
            {
                self.revise_causal_model(&affected.id)?;
            }
        }
        Ok(evidence)
    }
    pub fn revise_causal_model(&self, id: &CausalInvestigationId) -> Result<ContextualCausalModel> {
        let spec = self.causal_test_spec(id)?;
        let model_id: String = self.connection.query_row(
            "SELECT model_id FROM causal_investigations WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )?;
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let rev: i64 = tx.query_row(
            "SELECT COALESCE(MAX(revision),0)+1 FROM causal_models WHERE id=?1",
            [&model_id],
            |r| r.get(0),
        )?;
        let hypotheses = self.causal_investigation_hypotheses(id)?;
        let mut model = ContextualCausalModel {
            id: model_id.parse()?,
            scope: spec.scope,
            variables: spec.variables,
            edges: vec![],
            known_unknowns: vec![],
            revision: rev as u64,
        };
        for h in hypotheses {
            model.edges.push(CausalEdge {
                tested_inputs: self
                    .causal_evidence(&h.id)?
                    .into_iter()
                    .filter(|e| {
                        e.experiment_quality
                            == crate::experimentation::ExperimentQuality::Controlled
                    })
                    .map(|e| e.conditions)
                    .collect(),
                hypothesis: h.id.clone(),
                cause: h.cause.clone(),
                effect: h.effect.clone(),
                conditions: h.conditions.clone(),
                status: h.status,
                evidence: h.evidence.clone(),
            });
            if !h.status.supported() {
                model.known_unknowns.push(CausalGap {
                    description: format!(
                        "{}: {:?}; contextual contradictions require explicit scope refinement",
                        h.statement, h.status
                    ),
                    related_variables: vec![h.cause],
                    reason: match h.status {
                        CausalHypothesisStatus::Contradicted => CausalGapReason::Contradictory,
                        CausalHypothesisStatus::Untestable => CausalGapReason::Unintervenable,
                        _ => CausalGapReason::InsufficientEvidence,
                    },
                });
            }
        }
        tx.execute(
            "INSERT INTO causal_models(id,revision,data) VALUES(?1,?2,?3)",
            params![model_id, rev, json(&model)?],
        )?;
        for edge in &model.edges {
            tx.execute("INSERT INTO causal_model_edges(model_id,revision,hypothesis_id,data) VALUES(?1,?2,?3,?4)",params![model_id,rev,edge.hypothesis.to_string(),json(edge)?])?;
        }
        for (p, gap) in model.known_unknowns.iter().enumerate() {
            tx.execute(
                "INSERT INTO causal_gaps(model_id,revision,position,data) VALUES(?1,?2,?3,?4)",
                params![model_id, rev, p as i64, json(gap)?],
            )?;
        }
        event(&tx, &model_id, CausalEventKind::CausalModelRevised)?;
        tx.commit()?;
        Ok(model)
    }
    pub fn causal_dependencies(
        &self,
        id: &CausalHypothesisId,
    ) -> Result<Vec<CausalArtifactDependency>> {
        let mut stmt = self
            .connection
            .prepare("SELECT data FROM causal_artifact_dependencies WHERE hypothesis_id=?1")?;
        stmt.query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub fn link_causal_artifact(&self, dep: &CausalArtifactDependency) -> Result<()> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let h = self.causal_hypothesis(&dep.hypothesis)?;
        if !h.status.supported() {
            return Err(Error::InvalidInput(
                "Only locally supported mechanisms can guide learning artifacts".into(),
            ));
        }
        if let Some(id) = &dep.intervention {
            let run = self.causal_run(id)?;
            if run.discrimination.intervention.variable != h.cause
                || !self
                    .causal_evidence(&h.id)?
                    .iter()
                    .any(|e| e.intervention == *id && e.outcome == CausalEvidenceOutcome::Supports)
            {
                return Err(Error::InvalidInput(
                    "Intervention has no supporting evidence for this mechanism".into(),
                ));
            }
        }
        match &dep.artifact {
            CausalArtifact::Lesson(id) => {
                self.lesson(id)?;
            }
            CausalArtifact::Reflex(id) => {
                self.reflex(id)?;
            }
            CausalArtifact::Recovery(id) => {
                self.recovery(id)?;
            }
            CausalArtifact::Skill(id) => {
                self.skill(&id.to_string())?;
            }
            CausalArtifact::RuntimeDecision(id) => {
                use crate::store::RuntimeStore;
                self.runtime_decision(id)?;
            }
            CausalArtifact::Certification(id) => {
                use crate::store::AssuranceStore;
                self.skill_certification(id)?;
            }
        }
        tx.execute("INSERT INTO causal_artifact_dependencies(hypothesis_id,artifact_id,data) VALUES(?1,?2,?3)",params![h.id.to_string(),dep.artifact.key(),json(dep)?])?;
        tx.commit()?;
        Ok(())
    }
    pub fn causal_artifact_quarantined(&self, id: &str) -> Result<bool> {
        Ok(self.connection.query_row("SELECT EXISTS(SELECT 1 FROM causal_artifact_dependencies d JOIN causal_hypotheses h ON h.id=d.hypothesis_id WHERE d.artifact_id=?1 AND h.status NOT IN ('\"supported\"','\"strongly_supported\"'))",[id],|r|r.get(0))?)
    }
    pub fn causal_impact(&self, id: &CausalHypothesisId) -> Result<serde_json::Value> {
        let mut stmt = self.connection.prepare(
            "SELECT data FROM causal_revalidations WHERE hypothesis_id=?1 ORDER BY sequence",
        )?;
        let reviews: Vec<CausalRevalidation> = stmt
            .query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect::<Result<_>>()?;
        Ok(
            serde_json::json!({"hypothesis":self.causal_hypothesis(id)?,"dependencies":self.causal_dependencies(id)?,"revalidations":reviews}),
        )
    }
    pub fn propose_causal_refinement(
        &self,
        id: &CausalInvestigationId,
        hypothesis: &CausalHypothesisId,
    ) -> Result<LessonRevisionCandidate> {
        let inv = self.causal_investigation(id)?;
        let spec = self.causal_test_spec(id)?;
        let h = self.causal_hypothesis(hypothesis)?;
        if !inv.hypotheses.contains(hypothesis) || !h.status.supported() {
            return Err(Error::InvalidInput(
                "Refinement requires supported mechanism in this investigation".into(),
            ));
        }
        let evidence = self
            .causal_evidence(hypothesis)?
            .into_iter()
            .find(|e| {
                e.outcome == CausalEvidenceOutcome::Supports
                    && e.conditions == spec.baseline
                    && e.context == spec.scope
                    && self.causal_run(&e.intervention).is_ok_and(|run| {
                        run.request.starting_state.state_ref.git_commit
                            == spec.starting_state.state_ref.git_commit
                    })
            })
            .ok_or_else(|| Error::InvalidInput("No controlled support".into()))?;
        let run = self.causal_run(&evidence.intervention)?;
        let name = self
            .causal_test_spec(id)?
            .variables
            .into_iter()
            .find(|v| v.id == h.cause)
            .map(|v| v.name)
            .unwrap_or_else(|| h.cause.to_string());
        let guidance = format!(
            "Set {name}={} under the tested scope and conditions: {}",
            run.discrimination.intervention.to.literal(),
            h.statement
        );
        let candidate = LessonRevisionCandidate {
            investigation: id.clone(),
            hypothesis: hypothesis.clone(),
            intervention: evidence.intervention,
            scope: h.scope,
            conditions: h.conditions,
            tested_inputs: evidence.conditions,
            lesson_guidance: guidance.clone(),
            reflex_guidance: format!(
                "On matching failure and unchanged {} input, REPLAN: {guidance}",
                name
            ),
            recovery_guidance: guidance,
            requires_existing_validation: true,
        };
        self.connection.execute("INSERT OR IGNORE INTO causal_refinements(investigation_id,hypothesis_id,data) VALUES(?1,?2,?3)",params![id.to_string(),hypothesis.to_string(),json(&candidate)?])?;
        Ok(candidate)
    }
}
