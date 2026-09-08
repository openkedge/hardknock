// SPDX-License-Identifier: Apache-2.0

use chrono::Utc;
use clap::Subcommand;
use serde_json::{Value, json};

use super::{Cli, Commands};
use crate::{
    Error, Result,
    abstraction::{
        CandidateAbstraction, DefaultAbstractionPromotionPolicy,
        DeterministicTransferContextPlanner, KnowledgeMaturity, PromotionDecision, TransferContext,
        TransferContextPlanner, TransferContextRole, TransferEvaluationSet, TransferExpectation,
        TransferHypothesis, TransferHypothesisStatus, narrow_generalization_boundary,
    },
    budget::ExperienceBudget,
    core::{AbstractKnowledgeId, ExperiencePatternId, TransferHypothesisId},
    store::Store,
};

#[derive(Debug, Subcommand)]
pub enum PatternCommand {
    /// List persisted structural pattern candidates.
    List,
    /// Show one pattern, its members, structure, scope, and evidence.
    Show { id: ExperiencePatternId },
    /// Discover and persist candidates from explicit shared structure.
    Candidates,
    /// Explain why a structural pattern was formed.
    Explain { id: ExperiencePatternId },
}

#[derive(Debug, Subcommand)]
pub enum AbstractCommand {
    /// List abstract knowledge and maturity without expanding every member.
    List,
    /// Show provenance, transfer evidence, boundary, specializations, and exceptions.
    Show { id: AbstractKnowledgeId },
    /// Propose abstractions from persisted structurally compatible knowledge.
    Propose,
    /// Evaluate held-out evidence and negative controls under the promotion policy.
    Validate { id: AbstractKnowledgeId },
    /// Create a held-out transfer hypothesis and compile a paired Experiment plan.
    TestTransfer {
        id: AbstractKnowledgeId,
        #[arg(long)]
        target_tag: Option<String>,
        #[arg(long)]
        negative_control: bool,
    },
    /// Show included, excluded, and unknown applicability clauses.
    Boundary { id: AbstractKnowledgeId },
    /// Show immutable abstraction revisions and lifecycle events.
    History { id: AbstractKnowledgeId },
    /// Show downstream runtime, Skill, Constraint, Recovery, Guard, and assurance impact.
    Impact { id: AbstractKnowledgeId },
    /// Run the deterministic specific-vs-naive-vs-empirical benchmark.
    Benchmark,
}

pub fn handles(command: &Commands) -> bool {
    matches!(
        command,
        Commands::Pattern { .. } | Commands::Abstract { .. }
    )
}

fn show(store: &Store, id: &AbstractKnowledgeId) -> Result<Value> {
    let knowledge = store.abstract_knowledge(id)?;
    let evidence = store.transfer_evidence_for(id)?;
    let hypotheses = store.transfer_hypotheses_for(id)?;
    let specializations = store.knowledge_specializations_for(id)?;
    let exceptions = store.knowledge_exceptions_for(id)?;
    let guard = crate::abstraction::assess_guard_candidate(&knowledge, &evidence);
    Ok(json!({
        "kind":"abstract_knowledge_detail",
        "knowledge":knowledge,
        "source_context_count":knowledge.provenance.source_contexts.len(),
        "source_artifact_count":knowledge.provenance.source_artifacts.len(),
        "held_out_support":evidence.iter().filter(|item|item.context_role==TransferContextRole::HeldOut && item.outcome==crate::abstraction::TransferEvidenceOutcome::Supports).count(),
        "negative_controls":evidence.iter().filter(|item|item.context_role==TransferContextRole::NegativeControl).count(),
        "transfer_hypotheses":hypotheses,
        "transfer_evidence":evidence,
        "specializations":specializations,
        "exceptions":exceptions,
        "guard_candidate_assessment":guard,
        "notice":"Abstraction compresses guidance, never source evidence; external governance is required for enforcement"
    }))
}

pub fn execute(cli: &Cli, store: &Store) -> Result<Value> {
    match &cli.command {
        Commands::Pattern { command } => match command {
            PatternCommand::List => Ok(json!({
                "kind":"experience_patterns",
                "patterns":store.experience_patterns()?
            })),
            PatternCommand::Show { id } => Ok(json!({
                "kind":"experience_pattern",
                "pattern":store.experience_pattern(id)?
            })),
            PatternCommand::Candidates => {
                let candidates = store.discover_abstraction_candidates()?;
                Ok(json!({
                    "kind":"abstraction_candidates",
                    "candidates":candidates,
                    "provider":crate::abstraction::ABSTRACTION_PROVIDER_VERSION,
                    "semantic_similarity_establishes_support":false
                }))
            }
            PatternCommand::Explain { id } => {
                let pattern = store.experience_pattern(id)?;
                Ok(json!({
                    "kind":"experience_pattern_explanation",
                    "id":pattern.id,
                    "members":pattern.members,
                    "shared_structure":pattern.structure,
                    "scope":pattern.scope,
                    "status":pattern.status,
                    "basis":"Explicit shared trigger, action/effect semantics, outcome, causal mechanism, Recovery mechanism, or FailureTrajectory; prose similarity is excluded from candidate identity"
                }))
            }
        },
        Commands::Abstract { command } => match command {
            AbstractCommand::List => Ok(json!({
                "kind":"abstract_knowledge",
                "items":store.abstract_knowledge_items()?,
                "remote_constraints_are_advisory":true
            })),
            AbstractCommand::Show { id } => show(store, id),
            AbstractCommand::Propose => {
                let candidates = store.discover_abstraction_candidates()?;
                Ok(json!({
                    "kind":"abstract_knowledge_proposals",
                    "proposals":candidates.iter().map(|item|&item.knowledge).collect::<Vec<_>>(),
                    "patterns":candidates.iter().map(|item|&item.pattern).collect::<Vec<_>>(),
                    "notice":"Proposals are candidate-only until controlled held-out transfer and required negative controls are recorded"
                }))
            }
            AbstractCommand::Validate { id } => {
                let knowledge = store.abstract_knowledge(id)?;
                let evidence = store.transfer_evidence_for(id)?;
                let controls = evidence
                    .iter()
                    .filter(|item| item.context_role == TransferContextRole::NegativeControl)
                    .cloned()
                    .collect::<Vec<_>>();
                let policy = DefaultAbstractionPromotionPolicy::default();
                let assessment = policy.assess(&knowledge, &evidence, &controls);
                let mut revised = None;
                let mut exceptions = Vec::new();
                match assessment.decision {
                    PromotionDecision::Promote => {
                        let mut next = knowledge.clone();
                        next.revision = next.revision.saturating_add(1);
                        next.maturity = KnowledgeMaturity::Validated;
                        next.updated_at = Utc::now();
                        next.transfer_evidence = evidence
                            .iter()
                            .map(|item| crate::abstraction::TransferEvidenceRef {
                                id: item.id.clone(),
                            })
                            .collect();
                        store.revise_abstract_knowledge(
                            &next,
                            "Held-out transfer and negative-control promotion gates satisfied",
                            "abstract_knowledge_promoted",
                        )?;
                        revised = Some(next);
                    }
                    PromotionDecision::NarrowScope => {
                        let (next, created) = narrow_generalization_boundary(&knowledge, &evidence);
                        store.revise_abstract_knowledge(
                            &next,
                            "Transfer evidence narrowed the generalization boundary",
                            "abstraction_scope_narrowed",
                        )?;
                        for exception in &created {
                            store.save_knowledge_exception(exception)?;
                        }
                        exceptions = created;
                        revised = Some(next);
                    }
                    PromotionDecision::MoreEvidenceRequired | PromotionDecision::Reject => {}
                }
                Ok(json!({
                    "kind":"abstraction_validation",
                    "id":id,
                    "policy":crate::abstraction::PROMOTION_POLICY_VERSION,
                    "assessment":assessment,
                    "revision":revised,
                    "exceptions_created":exceptions,
                    "automatic_guard":false
                }))
            }
            AbstractCommand::TestTransfer {
                id,
                target_tag,
                negative_control,
            } => {
                let knowledge = store.abstract_knowledge(id)?;
                let pattern_id = knowledge.supporting_patterns.first().ok_or_else(|| {
                    Error::InvalidInput("Abstraction has no supporting pattern".into())
                })?;
                let pattern = store.experience_pattern(pattern_id)?;
                let target = crate::lesson::ContextSelector {
                    repository: None,
                    required_markers: Vec::new(),
                    tags: vec![
                        target_tag
                            .clone()
                            .unwrap_or_else(|| format!("held-out:{}", id)),
                    ],
                    os: None,
                    arch: None,
                };
                let hypothesis = TransferHypothesis {
                    id: TransferHypothesisId::new(),
                    abstract_knowledge: crate::abstraction::AbstractKnowledgeRef {
                        id: id.clone(),
                        revision: knowledge.revision,
                    },
                    source_contexts: knowledge.provenance.source_contexts.clone(),
                    target_context: target.clone(),
                    expected_behavior: TransferExpectation::ImproveOutcome,
                    status: TransferHypothesisStatus::Testable,
                    evidence: Vec::new(),
                    created_at: Utc::now(),
                };
                store.save_transfer_hypothesis(&hypothesis)?;
                let set = TransferEvaluationSet {
                    hypothesis: hypothesis.id.clone(),
                    source_contexts: hypothesis.source_contexts.clone(),
                    held_out_contexts: if *negative_control {
                        Vec::new()
                    } else {
                        vec![target.clone()]
                    },
                    negative_controls: if *negative_control {
                        vec![target.clone()]
                    } else {
                        Vec::new()
                    },
                };
                // The target is held out in both cases; a negative control is also
                // retained in its explicit role rather than being a source example.
                let persisted_set = TransferEvaluationSet {
                    held_out_contexts: vec![target.clone()],
                    ..set
                };
                store.save_transfer_evaluation_set(&persisted_set)?;
                let candidate = CandidateAbstraction {
                    pattern,
                    knowledge,
                    varying_dimensions: Vec::new(),
                    rationale: vec!["Existing persisted abstraction".into()],
                };
                let plan = DeterministicTransferContextPlanner.plan(
                    &candidate,
                    &[TransferContext {
                        selector: target,
                        variables: Vec::new(),
                        role: if *negative_control {
                            TransferContextRole::NegativeControl
                        } else {
                            TransferContextRole::HeldOut
                        },
                    }],
                    &ExperienceBudget {
                        max_realities: 2,
                        max_agent_runs: 0,
                        max_duration_ms: Some(300_000),
                        max_commands_per_reality: None,
                        max_curriculum_trials: Some(2),
                        max_parallel_trials: Some(2),
                        max_human_approvals: Some(0),
                        allowed_effect_risk: crate::effects::EffectRisk::ReadOnly,
                    },
                )?;
                Ok(json!({
                    "kind":"abstraction_transfer_plan",
                    "hypothesis":hypothesis,
                    "evaluation_set":persisted_set,
                    "plan":plan,
                    "execution_engine":"experiment",
                    "notice":"The plan is a typed baseline/transfer handoff to the existing Experiment engine; it grants no capability or Effect authority"
                }))
            }
            AbstractCommand::Boundary { id } => {
                let knowledge = store.abstract_knowledge(id)?;
                Ok(json!({
                    "kind":"generalization_boundary",
                    "abstract":id,
                    "included":knowledge.generalization_boundary.included,
                    "excluded":knowledge.generalization_boundary.excluded,
                    "unknown":knowledge.generalization_boundary.unknown,
                    "evidence":knowledge.generalization_boundary.evidence
                }))
            }
            AbstractCommand::History { id } => Ok(json!({
                "kind":"abstract_knowledge_history",
                "revisions":store.abstract_knowledge_history(id)?,
                "events":store.abstraction_events(Some(&id.to_string()))?
            })),
            AbstractCommand::Impact { id } => Ok(json!({
                "kind":"abstraction_impact",
                "abstract":id,
                "impact":store.abstraction_impact(id)?,
                "notice":"Contradiction requires dependent guidance review; it never silently changes OpenKedge policy"
            })),
            AbstractCommand::Benchmark => Ok(json!({
                "kind":"abstraction_benchmark",
                "report":crate::abstraction::benchmark::run()?
            })),
        },
        _ => Err(Error::InvalidInput("Abstraction dispatch failed".into())),
    }
}
