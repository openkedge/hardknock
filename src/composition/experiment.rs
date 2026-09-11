// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{
    Error, Result,
    core::*,
    dojo::{GitRealityProvider, RealityProvider},
    experimentation::ExperimentQuality,
    hierarchy::ScopeValue,
    store::{CapabilityStore, RuntimeStore, Store, ToolStore},
    tool::{ToolExecutionStatus, ToolRegistry},
    tool_runtime::{MicroSandboxProvider, ToolRouter},
};
use chrono::Utc;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Bounded test coordinator. All processes, sandboxes, attestations and Realities
/// are provided by existing execution infrastructure. Never used as a live scheduler.
pub struct CompositionExperimentEngine<'a, P> {
    pub store: &'a Store,
    pub router: ToolRouter<P>,
}
impl<'a, P: MicroSandboxProvider> CompositionExperimentEngine<'a, P> {
    pub fn new(store: &'a Store, provider: P) -> Result<Self> {
        if store
            .tool_definition_by_name("composition-procedure")
            .is_err()
        {
            let mut tool = crate::tool::builtin_tools()
                .into_iter()
                .find(|t| t.name == "run-tests")
                .expect("builtin");
            tool.name = "composition-procedure".into();
            tool.invocation = crate::tool::ToolInvocation::NativeBinary {
                executable: "/bin/sh".into(),
                args_template: vec![
                    "-c".into(),
                    "{command}".into(),
                    "composition".into(),
                    "{state_input}".into(),
                ],
            };
            tool.integrity.manifest_hash = tool.manifest_hash()?;
            store.insert_tool_definition(&tool)?;
        }
        let wrapper = store.tool_definition_by_name("composition-procedure")?;
        let expected = crate::tool::ToolInvocation::NativeBinary {
            executable: "/bin/sh".into(),
            args_template: vec![
                "-c".into(),
                "{command}".into(),
                "composition".into(),
                "{state_input}".into(),
            ],
        };
        if wrapper.disabled
            || serde_json::to_value(&wrapper.invocation)? != serde_json::to_value(expected)?
        {
            return Err(Error::Intervention(
                "Composition procedure wrapper does not match the registered execution contract"
                    .into(),
            ));
        }
        let mut registry = ToolRegistry::new();
        for tool in store.tool_definitions(false)? {
            registry.register(tool)?;
        }
        Ok(Self {
            store,
            router: ToolRouter::new(registry, provider),
        })
    }
    pub async fn run(
        &self,
        request: &CompositionExperimentRequest,
        cancel: &crate::cancellation::Cancellation,
    ) -> Result<CompositionEvidence> {
        let c = self
            .store
            .composition_revision(&request.composition, request.revision)?;
        let order = ordered_steps(&c)?;
        let starting_state = composition_starting_proof(&c, &request.starting_state.state_ref)?;
        if starting_state != request.starting_state {
            return Err(Error::InvalidInput(
                "Starting proof differs from the pinned composition and controlled environment"
                    .into(),
            ));
        }
        if request
            .failure_injections
            .iter()
            .any(|i| i.facts.is_empty() || i.perturbation.trim().is_empty())
        {
            return Err(Error::InvalidInput(
                "Failure injections need an explicit perturbation and state transition".into(),
            ));
        }
        if request.evaluation.required_failure_points.iter().any(|id| {
            !request
                .failure_injections
                .iter()
                .any(|i| &i.before_step == id)
        }) {
            return Err(Error::InvalidInput(
                "Required failure point has no injection".into(),
            ));
        }
        if request.step_inputs.values().any(|v| !v.is_object()) {
            return Err(Error::InvalidInput("Step inputs must be objects".into()));
        }
        if self.store.composition_dependency_health(&c)?.status != CompositionHealthStatus::Healthy
        {
            return Err(Error::Intervention(
                "Component revision changed; composition revalidation must pin current revisions"
                    .into(),
            ));
        }
        if order.len() > request.budget.max_realities || request.evaluation.checks.is_empty() {
            return Err(Error::InvalidInput("Composition test requires a concrete evaluator and a Reality budget for every step".into()));
        }
        if request
            .failure_injections
            .iter()
            .any(|i| !order.contains(&i.before_step))
        {
            return Err(Error::InvalidInput(
                "Failure injection step is missing".into(),
            ));
        }
        let report = DefaultCompositionPreflightAnalyzer.analyze(
            &c,
            &trusted_state(&request.initial_state, &BTreeMap::new(), Utc::now()),
        )?;
        if report.findings.iter().any(|f| {
            matches!(
                f.kind,
                CompositionCompatibilityFindingKind::CapabilityConflict
                    | CompositionCompatibilityFindingKind::EffectConflict
            )
        }) {
            return Err(Error::Intervention(
                "Resolve composition authority/effect conflicts before testing".into(),
            ));
        }
        // Only actual portable Tool definitions run here. Skill/Recovery procedures can
        // use their registered tool wrappers; unsupported references fail before creating a Reality.
        for step in &c.steps {
            self.store.composition_execution_tool(step)?;
        }
        let mut state = request.initial_state.clone();
        let mut results = vec![];
        let mut invariants = vec![];
        let mut completed = vec![];
        let mut commit = CompositionCommitState::default();
        let mut evidence_refs = vec![];
        let mut outcome = CompositionOutcome::Pass;
        let start = std::time::Instant::now();
        self.store
            .composition_event(&c.id, "composition_experiment_started", request)?;
        let session = HardknockSessionId::new();
        let trajectory = self
            .store
            .start_composition_trajectory(&c, session.clone())?;
        'steps: for id in order {
            if cancel.is_cancelled()
                || request
                    .budget
                    .max_duration_ms
                    .is_some_and(|limit| start.elapsed().as_millis() > u128::from(limit))
            {
                outcome = CompositionOutcome::Inconclusive;
                break;
            }
            let step = c.steps.iter().find(|s| s.id == id).expect("ordered");
            for injection in request
                .failure_injections
                .iter()
                .filter(|i| i.before_step == id)
            {
                for fact in &injection.facts {
                    state.retain(|s| s.key != fact.key);
                    state.push(fact.clone());
                }
            }
            let runtime_context = composition_runtime_context(
                self.store,
                &c,
                &request.starting_state.state_ref,
                &session,
                &id,
                &completed,
                &commit,
                &state,
            )?;
            let runtime = self
                .store
                .record_runtime_decision(&runtime_context, Default::default())?;
            evaluate_invariants(
                &c,
                &id,
                SequenceEvaluationPhase::BeforeStep,
                &completed,
                &commit,
                &state,
                &mut invariants,
            );
            let mut input = request
                .step_inputs
                .get(&id)
                .cloned()
                .unwrap_or_else(|| json!({}));
            if !input.is_object() {
                return Err(Error::InvalidInput("Step input must be an object".into()));
            }
            let observed = trusted_state(&state, &BTreeMap::new(), Utc::now());
            for binding in &step.input_bindings {
                if !binding.allowed {
                    return Err(Error::Intervention("Unapproved state handoff".into()));
                }
                let Some(value) = observed.values.get(&binding.source_key) else {
                    outcome = CompositionOutcome::Inconclusive;
                    self.store.composition_event(
                        &c.id,
                        "composition_handoff_unavailable",
                        &binding.source_key,
                    )?;
                    break 'steps;
                };
                if let Some(from) = &binding.from
                    && !state.iter().any(|s| {
                        s.key == binding.source_key && s.freshness.step.as_ref() == Some(from)
                    })
                {
                    return Err(Error::Intervention(
                        "Handoff producer provenance mismatch".into(),
                    ));
                }
                input[&binding.key] = scope_json(value);
            }
            let (tool, script) = self.store.composition_execution_tool(step)?;
            if let Some(script) = script {
                input = json!({"command":script,"state_input":input.to_string()});
            }
            let provider = GitRealityProvider::new(self.store);
            let mut reality = provider.create(&request.starting_state.state_ref)?;
            provider.verify_start(&reality)?;
            self.store
                .insert_capability_manifest(&reality.id, &step.capabilities)?;
            let remaining = request
                .budget
                .max_duration_ms
                .unwrap_or(300_000)
                .saturating_sub(start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
            let tool_name = tool.to_string();
            let execution = self
                .router
                .execute_controlled(
                    &reality,
                    &step.capabilities,
                    &tool_name,
                    input,
                    &[],
                    Some((cancel, std::time::Duration::from_millis(remaining))),
                )
                .await;
            // Destroy each Reality before the next step; only approved typed facts cross.
            let cleanup = provider.discard(&mut reality);
            let run = match execution {
                Ok(run) => run,
                Err(error) => {
                    cleanup?;
                    self.store.composition_event(
                        &c.id,
                        "composition_step_interrupted",
                        &error.to_string(),
                    )?;
                    outcome = CompositionOutcome::Inconclusive;
                    break;
                }
            };
            cleanup?;
            self.router.persist_run(self.store, &run)?;
            evidence_refs.push(crate::epistemic::EvidenceRef {
                kind: "execution_attestation".into(),
                id: run.attestation.id.to_string(),
            });
            let step_outcome = match run.result.status {
                ToolExecutionStatus::Success => CompositionOutcome::Pass,
                ToolExecutionStatus::RuntimeFailure => CompositionOutcome::Inconclusive,
                _ => CompositionOutcome::Fail,
            };
            let mut observations = vec![];
            if let Ok(Value::Object(values)) = serde_json::from_str::<Value>(&run.result.stdout) {
                for (key, value) in values {
                    if let Some(value) = json_scope(value) {
                        let claim = StateClaim {
                            key: key.clone(),
                            value,
                            source: StateClaimSource::ToolAttestation,
                            freshness: StateFreshness {
                                observed_at: Utc::now(),
                                expires_at: None,
                                step: Some(id.clone()),
                                external_version: None,
                            },
                        };
                        state.retain(|s| s.key != key);
                        state.push(claim.clone());
                        observations.push(claim);
                    }
                }
            }
            self.store
                .observe_composition_step(&trajectory.id, &id, &observations)?;
            results.push(CompositionStepResult {
                step: id.clone(),
                component: step.component.clone(),
                outcome: step_outcome,
                observations,
                attestation: Some(run.attestation.id),
                runtime_decision: Some(runtime.id),
            });
            evaluate_invariants(
                &c,
                &id,
                SequenceEvaluationPhase::AfterStep,
                &completed,
                &commit,
                &state,
                &mut invariants,
            );
            for point in c
                .contract
                .effect_policy
                .commit_points
                .iter()
                .filter(|p| p.after_step == id)
            {
                evaluate_invariants(
                    &c,
                    &id,
                    SequenceEvaluationPhase::CommitPoint,
                    &completed,
                    &commit,
                    &state,
                    &mut invariants,
                );
                if point.irreversible_effects.is_empty() {
                    commit.reached.push(point.id.clone());
                }
            }
            completed.push(id.clone());
            if step_outcome == CompositionOutcome::Inconclusive {
                outcome = CompositionOutcome::Inconclusive;
                break;
            }
            if step_outcome == CompositionOutcome::Fail {
                outcome = CompositionOutcome::Fail;
                if c.relations
                    .iter()
                    .any(|r| r.from == id && r.kind == CompositionRelationKind::RequiresSuccessOf)
                {
                    break;
                }
            }
        }
        if let Some(last) = completed.last() {
            evaluate_invariants(
                &c,
                last,
                SequenceEvaluationPhase::Completion,
                &completed,
                &commit,
                &state,
                &mut invariants,
            );
        }
        let initial = trusted_state(&request.initial_state, &BTreeMap::new(), Utc::now());
        if c.contract
            .preconditions
            .iter()
            .any(|p| condition_value(p, &initial) != Some(true))
        {
            outcome = CompositionOutcome::Inconclusive;
        }
        let final_state = trusted_state(&state, &BTreeMap::new(), Utc::now());
        let checks = request
            .evaluation
            .checks
            .iter()
            .chain(&c.contract.postconditions)
            .map(|condition| condition_value(condition, &final_state))
            .collect::<Vec<_>>();
        if invariants.iter().any(|i| i.satisfied == Some(false))
            || checks.contains(&Some(false))
            || c.contract
                .forbidden_outcomes
                .iter()
                .any(|f| condition_value(&f.detector, &final_state) == Some(true))
        {
            outcome = CompositionOutcome::Fail
        } else if outcome == CompositionOutcome::Pass
            && (invariants.iter().any(|i| i.satisfied.is_none())
                || checks.contains(&None)
                || c.contract
                    .forbidden_outcomes
                    .iter()
                    .any(|f| condition_value(&f.detector, &final_state).is_none())
                || completed.len() != c.steps.len())
        {
            outcome = CompositionOutcome::Inconclusive
        }
        let recovery_steps = results
            .iter()
            .filter(|r| matches!(r.component.component, ComposableArtifactRef::Recovery(_)))
            .collect::<Vec<_>>();
        let recovery_result = if recovery_steps.is_empty() {
            None
        } else {
            let global_checks = checks
                .iter()
                .copied()
                .chain(invariants.iter().map(|i| i.satisfied))
                .collect::<Vec<_>>();
            Some(composition_recovery_outcome(
                recovery_steps
                    .iter()
                    .all(|r| r.outcome == CompositionOutcome::Pass),
                &global_checks,
                false,
            ))
        };
        let evidence = CompositionEvidence {
            failure_points: request
                .failure_injections
                .iter()
                .map(|i| i.before_step.clone())
                .collect(),
            test_input_hash: composition_hash(&(
                &request.initial_state,
                &request.step_inputs,
                &request.failure_injections,
            ))?,
            id: CompositionEvidenceId::new(),
            composition: c.id.clone(),
            composition_revision: c.revision,
            starting_state: request.starting_state.clone(),
            step_results: results,
            invariant_results: invariants,
            effect_results: commit.committed_effects,
            recovery_result,
            outcome,
            experiment_quality: if outcome == CompositionOutcome::Inconclusive {
                ExperimentQuality::PartiallyControlled
            } else {
                ExperimentQuality::Controlled
            },
            provenance: CompositionEvidenceProvenance {
                evidence: evidence_refs,
                evaluator: composition_hash(&("composition-state-v1", &request.evaluation.checks))?,
                environment: request.starting_state.environment_fingerprint.clone(),
                intervention: if request.failure_injections.is_empty() {
                    None
                } else {
                    Some(composition_hash(&request.failure_injections)?)
                },
            },
            created_at: Utc::now(),
        };
        self.store.save_composition_evidence(&evidence)?;
        Ok(evidence)
    }
}
fn evaluate_invariants(
    c: &Composition,
    step: &CompositionStepId,
    phase: SequenceEvaluationPhase,
    completed: &[CompositionStepId],
    commit: &CompositionCommitState,
    state: &[StateClaim],
    results: &mut Vec<SequenceInvariantEvaluation>,
) {
    let context = trusted_state(state, &BTreeMap::new(), Utc::now());
    for invariant in &c.contract.sequence_invariants {
        if invariant_active(c, invariant, step, phase, completed, commit) {
            results.push(SequenceInvariantEvaluation {
                invariant: invariant.id.clone(),
                step: step.clone(),
                phase,
                satisfied: condition_value(&invariant.condition, &context),
                reason: "Evaluated from trusted current step observations; unknown is not success"
                    .into(),
            });
        }
    }
}
pub fn scope_json(value: &ScopeValue) -> Value {
    match value {
        ScopeValue::String(v) | ScopeValue::Version(v) => json!(v),
        ScopeValue::Integer(v) => json!(v),
        ScopeValue::Boolean(v) => json!(v),
    }
}
pub fn json_scope(value: Value) -> Option<ScopeValue> {
    match value {
        Value::String(v) => Some(ScopeValue::String(v)),
        Value::Bool(v) => Some(ScopeValue::Boolean(v)),
        Value::Number(v) => v.as_i64().map(ScopeValue::Integer),
        _ => None,
    }
}

/// Capture the same controlled environment used by the experiment substrate.
/// The proof binds exact component definitions, not caller-supplied evidence labels.
pub fn composition_starting_proof(
    c: &Composition,
    state: &StateRef,
) -> Result<crate::experimentation::StartingStateProof> {
    let environment = crate::experience::EnvironmentContext::capture(
        &state.repo_path,
        crate::core::EnvironmentMode::Controlled,
    )?;
    let runtimes = c
        .steps
        .iter()
        .map(|s| (s.id.to_string(), s.component.content_hash.clone()))
        .collect::<BTreeMap<_, _>>();
    Ok(crate::experimentation::StartingStateProof {
        state_ref: state.clone(), fingerprint: composition_hash(&("composition-start-v1", state, &environment.fingerprint, &runtimes))?,
        environment_fingerprint: environment.fingerprint, runtime_fingerprints: runtimes,
        scope: "Pinned commit/tree, component manifests and controlled shell environment; no live services or process snapshot".into(),
    })
}

#[allow(clippy::too_many_arguments)]
fn composition_runtime_context(
    store: &Store,
    c: &Composition,
    start: &StateRef,
    session: &HardknockSessionId,
    step: &CompositionStepId,
    completed: &[CompositionStepId],
    commit: &CompositionCommitState,
    state: &[StateClaim],
) -> Result<crate::runtime::RuntimeDecisionContext> {
    use crate::runtime::*;
    let experience = crate::experience::ExperienceContext::capture(
        start,
        &start.repo_path,
        EnvironmentMode::Controlled,
    )?;
    let mut context = RuntimeContextSynthesizer { store }.synthesize(RuntimeContextRequest {
        external_session_id: session.to_string(),
        agent: AgentIdentity {
            kind: "composition-test".into(),
            executable: "hardknock".into(),
            version: Some(env!("CARGO_PKG_VERSION").into()),
            model: None,
        },
        task: TaskDescriptor {
            description: format!("Validate composition {} step {}", c.name, step),
            family: Some("composition-validation".into()),
            tags: vec![],
        },
        query_context: crate::retrieval::QueryContext::new(&experience, &c.name, vec![]),
        proposed_action: None,
        proposed_effect: None,
        risk: None,
        capability_context: Default::default(),
        failure_signature: None,
        consecutive_failures: 0,
        no_state_change: false,
        config_changed: false,
        candidate_strategies: vec![],
        experiment_capability: Default::default(),
        known_unknowns: vec![],
        externally_supported: false,
        envelope_position: None,
    })?;
    context.session_id = session.clone();
    context.composition = Some(CompositionRuntimeContext {
        composition: c.id.clone(),
        revision: c.revision,
        step: step.clone(),
        completed_steps: completed.to_vec(),
        current_handoffs: vec![],
        active_sequence_invariants: vec![],
        commit_state: commit.clone(),
        state: state.to_vec(),
        external_versions: BTreeMap::new(),
        assessed_at: Utc::now(),
    });
    Ok(context)
}
