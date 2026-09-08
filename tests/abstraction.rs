// SPDX-License-Identifier: Apache-2.0

mod support;

use std::{path::Path, time::Instant};

use chrono::Utc;
use hardknock::{
    abstraction::{self, *},
    bridge::config::Config,
    budget::ExperienceBudget,
    core::*,
    curriculum::CurriculumGoalKind,
    economics::{
        DeterministicExperienceOpportunityCompiler, ExecutableLearningPlan,
        ExperienceOpportunityCompiler, ExperienceOpportunityGenerator, ExperienceOpportunityKind,
        ExperienceOpportunityTarget, ExperiencePortfolioObjective,
    },
    effects::EffectRisk,
    epistemic::{DiversityClass, EvidenceRef},
    experimentation::{ExperimentIntent, ExperimentQuality},
    federation::{FederationService, LocalFederationService},
    lesson::{ActionPattern, ContextSelector},
    runtime::RuntimeScenario,
    store::Store,
};
use support::Fixture;

fn scope(tag: &str) -> ContextSelector {
    ContextSelector {
        repository: None,
        required_markers: Vec::new(),
        tags: vec![tag.into()],
        os: None,
        arch: None,
    }
}

fn variable(
    name: &str,
    kind: ContextVariableKind,
    value: VariableValue,
    relevance: ContextRelevance,
) -> ContextVariable {
    ContextVariable {
        name: name.into(),
        kind,
        value,
        relevance,
    }
}

fn structure(mechanism: &CausalHypothesisId, resource: &str) -> PatternStructure {
    PatternStructure {
        trigger: Some(PatternPredicate {
            variable: "outcome_uncertain".into(),
            operator: PredicateOperator::Equals,
            value: Some(VariableValue::Boolean(true)),
        }),
        context_variables: vec![
            variable(
                "externality",
                ContextVariableKind::Externality,
                VariableValue::Text("external_mutation".into()),
                ContextRelevance::Required,
            ),
            variable(
                "action_semantics",
                ContextVariableKind::ActionSemantics,
                VariableValue::Text("state_dependent_follow_up".into()),
                ContextRelevance::Required,
            ),
            variable(
                "idempotent",
                ContextVariableKind::Idempotency,
                VariableValue::Boolean(false),
                ContextRelevance::Required,
            ),
            variable(
                "resource",
                ContextVariableKind::Resource,
                VariableValue::Text(resource.into()),
                ContextRelevance::Varies,
            ),
        ],
        action_pattern: Some(ActionPattern::Custom {
            kind: "mutation".into(),
            value: "retry_without_reconciliation".into(),
        }),
        outcome_pattern: Some(OutcomePattern {
            classification: "failure".into(),
            observable: "stale state-dependent mutation fails".into(),
        }),
        causal_mechanisms: vec![CausalHypothesisRef {
            id: mechanism.clone(),
        }],
        required_conditions: vec![PatternPredicate {
            variable: "authoritative_state_reconciled".into(),
            operator: PredicateOperator::Equals,
            value: Some(VariableValue::Boolean(false)),
        }],
    }
}

fn artifact(
    kind: KnowledgeArtifactKind,
    statement: &str,
    context: &str,
    mechanism: &CausalHypothesisId,
    root: &str,
) -> KnowledgeArtifact {
    KnowledgeArtifact {
        artifact: KnowledgeArtifactRef {
            kind,
            id: format!("{context}-artifact"),
            revision: 1,
        },
        statement: statement.into(),
        scope: scope(context),
        structure: structure(mechanism, context),
        evidence: vec![EvidenceRef {
            kind: "experiment".into(),
            id: format!("{context}-experiment"),
        }],
        root_origins: vec![root.into()],
        updated_at: Utc::now(),
    }
}

fn candidate(kind: KnowledgeArtifactKind) -> CandidateAbstraction {
    let mechanism = CausalHypothesisId::new();
    let artifacts = vec![
        artifact(
            kind,
            "Postgres needs a row refresh",
            "database",
            &mechanism,
            "db-root",
        ),
        artifact(
            kind,
            "Fetch the new resource version",
            "deployment",
            &mechanism,
            "deployment-root",
        ),
        artifact(
            kind,
            "Refetch before the next push",
            "git",
            &mechanism,
            "git-root",
        ),
    ];
    DeterministicAbstractionCandidateProvider
        .propose(&artifacts, &AbstractionContext::default())
        .unwrap()
        .remove(0)
}

fn quality() -> TransferQuality {
    TransferQuality {
        context_distance: ContextDifferenceSummary {
            changed_dimensions: vec![ContextVariableKind::Resource],
            preserved_dimensions: vec![ContextVariableKind::ActionSemantics],
            unknown_dimensions: Vec::new(),
        },
        analogy_mapping_complete: true,
        experiment_quality: ExperimentQuality::Controlled,
        evidence_diversity: DiversityClass::High,
    }
}

fn trial() -> AbstractionTrialRef {
    AbstractionTrialRef {
        experiment_id: ExperimentId::new(),
        trial_id: TrialId::new(),
    }
}

fn transfer(
    knowledge: &AbstractKnowledge,
    hypothesis: &TransferHypothesisId,
    role: TransferContextRole,
    outcome: TransferEvidenceOutcome,
    expected_applicable: bool,
    triggered: bool,
    tag: &str,
) -> TransferEvidence {
    TransferEvidence {
        id: TransferEvidenceId::new(),
        hypothesis: hypothesis.clone(),
        source_artifact: AbstractKnowledgeRef {
            id: knowledge.id.clone(),
            revision: knowledge.revision,
        },
        target_context: scope(tag),
        context_role: role,
        expected_applicable,
        application_triggered: triggered,
        baseline_trial: trial(),
        transfer_trial: trial(),
        outcome,
        quality: quality(),
        observable_behavior: "paired evaluator outcome".into(),
        boundary_clause: None,
        local: true,
        created_at: Utc::now(),
    }
}

fn budget(trials: usize) -> ExperienceBudget {
    ExperienceBudget {
        max_realities: trials,
        max_agent_runs: 0,
        max_duration_ms: Some(60_000),
        max_commands_per_reality: None,
        max_curriculum_trials: Some(trials),
        max_parallel_trials: Some(2),
        max_human_approvals: Some(0),
        allowed_effect_risk: EffectRisk::ReadOnly,
    }
}

#[test]
fn structural_discovery_prefers_shared_mechanism_over_shared_wording() {
    let shared = CausalHypothesisId::new();
    let artifacts = vec![
        artifact(
            KnowledgeArtifactKind::Lesson,
            "Completely different words A",
            "database",
            &shared,
            "a",
        ),
        artifact(
            KnowledgeArtifactKind::Lesson,
            "Completely different words B",
            "deployment",
            &shared,
            "b",
        ),
        artifact(
            KnowledgeArtifactKind::Lesson,
            "retry failed",
            "rate-limit",
            &CausalHypothesisId::new(),
            "c",
        ),
        artifact(
            KnowledgeArtifactKind::Lesson,
            "retry failed",
            "authorization",
            &CausalHypothesisId::new(),
            "d",
        ),
    ];
    let candidates = DeterministicAbstractionCandidateProvider
        .propose(&artifacts, &AbstractionContext::default())
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].pattern.members.len(), 2);
    assert!(
        candidates[0]
            .rationale
            .iter()
            .any(|item| item.contains("statements were not compared"))
    );
}

#[test]
fn every_abstract_kind_preserves_its_semantic_type() {
    for (artifact_kind, expected) in [
        (
            KnowledgeArtifactKind::Lesson,
            AbstractKnowledgeKind::AbstractLesson,
        ),
        (
            KnowledgeArtifactKind::Skill,
            AbstractKnowledgeKind::AbstractSkill,
        ),
        (
            KnowledgeArtifactKind::Constraint,
            AbstractKnowledgeKind::AbstractConstraint,
        ),
        (
            KnowledgeArtifactKind::AntiPattern,
            AbstractKnowledgeKind::AbstractAntiPattern,
        ),
        (
            KnowledgeArtifactKind::Recovery,
            AbstractKnowledgeKind::AbstractRecovery,
        ),
    ] {
        assert_eq!(candidate(artifact_kind).knowledge.kind, expected);
    }
}

#[test]
fn promotion_requires_local_held_out_behavior_and_distinct_origins() {
    let candidate = candidate(KnowledgeArtifactKind::Lesson);
    let hypothesis = TransferHypothesisId::new();
    let supported = transfer(
        &candidate.knowledge,
        &hypothesis,
        TransferContextRole::HeldOut,
        TransferEvidenceOutcome::Supports,
        true,
        true,
        "object-store",
    );
    let policy = DefaultAbstractionPromotionPolicy::default();
    assert_eq!(
        policy.evaluate(&candidate.knowledge, std::slice::from_ref(&supported), &[]),
        PromotionDecision::Promote
    );
    let mut remote = candidate.knowledge.clone();
    remote.provenance.origin = KnowledgeOrigin::FederatedAdvisory;
    let mut external = supported;
    external.local = false;
    assert_eq!(
        policy.evaluate(&remote, &[external], &[]),
        PromotionDecision::MoreEvidenceRequired
    );
}

#[test]
fn abstract_constraint_requires_negative_control_and_detects_false_constraint() {
    let mut candidate = candidate(KnowledgeArtifactKind::Constraint).knowledge;
    candidate.kind = AbstractKnowledgeKind::AbstractConstraint;
    let hypothesis = TransferHypothesisId::new();
    let support = transfer(
        &candidate,
        &hypothesis,
        TransferContextRole::HeldOut,
        TransferEvidenceOutcome::Supports,
        true,
        true,
        "object-store",
    );
    let policy = DefaultAbstractionPromotionPolicy::default();
    assert_eq!(
        policy.evaluate(&candidate, std::slice::from_ref(&support), &[]),
        PromotionDecision::MoreEvidenceRequired
    );
    let clean_control = transfer(
        &candidate,
        &hypothesis,
        TransferContextRole::NegativeControl,
        TransferEvidenceOutcome::Supports,
        false,
        false,
        "idempotent-api",
    );
    assert_eq!(
        policy.evaluate(
            &candidate,
            &[support.clone(), clean_control.clone()],
            &[clean_control]
        ),
        PromotionDecision::Promote
    );
    let false_constraint = transfer(
        &candidate,
        &hypothesis,
        TransferContextRole::NegativeControl,
        TransferEvidenceOutcome::NarrowsScope,
        false,
        true,
        "idempotent-api",
    );
    assert_eq!(
        policy.evaluate(
            &candidate,
            &[support, false_constraint.clone()],
            &[false_constraint]
        ),
        PromotionDecision::NarrowScope
    );
}

#[test]
fn narrowing_creates_exception_and_preserves_original_sources() {
    let candidate = candidate(KnowledgeArtifactKind::Constraint).knowledge;
    let original_sources = candidate.provenance.source_artifacts.clone();
    let mut control = transfer(
        &candidate,
        &TransferHypothesisId::new(),
        TransferContextRole::NegativeControl,
        TransferEvidenceOutcome::NarrowsScope,
        false,
        true,
        "idempotent-api",
    );
    control.boundary_clause = Some(ApplicabilityClause {
        variable: ContextVariableKind::Idempotency,
        name: "idempotent".into(),
        operator: PredicateOperator::Equals,
        value: Some(VariableValue::Boolean(true)),
        rationale: "Exact replay is guaranteed".into(),
    });
    let (revised, exceptions) = narrow_generalization_boundary(&candidate, &[control]);
    assert_eq!(revised.provenance.source_artifacts, original_sources);
    assert_eq!(revised.maturity, KnowledgeMaturity::Overgeneralized);
    assert_eq!(revised.generalization_boundary.excluded.len(), 1);
    assert_eq!(exceptions.len(), 1);
    assert_eq!(exceptions[0].reason, ExceptionReason::IdempotentSemantics);
}

#[test]
fn specific_exception_beats_broader_constraint_at_runtime() {
    let parent = AbstractKnowledgeId::new();
    let mut scenario = RuntimeScenario::default();
    scenario
        .context
        .facts
        .insert("operation".into(), "external_mutation".into());
    scenario
        .context
        .facts
        .insert("idempotency".into(), "exact".into());
    let context = scenario.decision_context().unwrap();
    let empty_scope = ContextSelector {
        repository: None,
        required_markers: Vec::new(),
        tags: Vec::new(),
        os: None,
        arch: None,
    };
    let required_operation = ApplicabilityClause {
        variable: ContextVariableKind::ActionSemantics,
        name: "operation".into(),
        operator: PredicateOperator::Equals,
        value: Some(VariableValue::Text("external_mutation".into())),
        rationale: "test".into(),
    };
    let abstract_item = KnowledgeCandidateRef {
        reference: parent.to_string(),
        abstract_parent: Some(parent.clone()),
        level: ResolutionLevel::Abstract,
        statement: "Reconcile before retry".into(),
        scope: empty_scope.clone(),
        applicability: ApplicabilityPredicate {
            all_of: vec![required_operation.clone()],
        },
        boundary: GeneralizationBoundary::default(),
        maturity: KnowledgeMaturity::Validated,
        quality: ExperimentQuality::Controlled,
        updated_at: Utc::now(),
    };
    let exception = KnowledgeCandidateRef {
        reference: KnowledgeExceptionId::new().to_string(),
        abstract_parent: Some(parent),
        level: ResolutionLevel::Exception,
        statement: "Exact idempotency-key replay is safe".into(),
        scope: empty_scope,
        applicability: ApplicabilityPredicate {
            all_of: vec![
                required_operation,
                ApplicabilityClause {
                    variable: ContextVariableKind::Idempotency,
                    name: "idempotency".into(),
                    operator: PredicateOperator::Equals,
                    value: Some(VariableValue::Text("exact".into())),
                    rationale: "test".into(),
                },
            ],
        },
        boundary: GeneralizationBoundary::default(),
        maturity: KnowledgeMaturity::Validated,
        quality: ExperimentQuality::Controlled,
        updated_at: Utc::now(),
    };
    let resolution = DeterministicKnowledgeResolutionPolicy
        .resolve(&context, &[abstract_item, exception])
        .unwrap();
    assert_eq!(resolution.selected.len(), 1);
    assert_eq!(resolution.selected[0].level, ResolutionLevel::Exception);
}

#[test]
fn distillation_is_reversible_and_reactivates_specific_knowledge() {
    let mut knowledge = candidate(KnowledgeArtifactKind::Lesson).knowledge;
    knowledge.maturity = KnowledgeMaturity::Validated;
    let (distillation, represented) = distill(&knowledge, EvidenceManifestId::new()).unwrap();
    assert_eq!(distillation.inputs, knowledge.provenance.source_artifacts);
    assert_eq!(represented.len(), 3);
    assert!(represented.iter().all(|item| matches!(
        item.state,
        KnowledgeRepresentationState::RepresentedByAbstract(_)
    )));
    let reactivated = reactivate_represented_members(&knowledge, &represented);
    assert_eq!(reactivated.len(), represented.len());
    assert!(
        reactivated
            .iter()
            .all(|item| item.state == KnowledgeRepresentationState::DirectActive)
    );
}

#[test]
fn store_preserves_revisions_held_out_sets_and_append_only_transfer_evidence() {
    let fixture = Fixture::new();
    let store = Store::open(&fixture.home).unwrap();
    let candidate = candidate(KnowledgeArtifactKind::Lesson);
    store.save_experience_pattern(&candidate.pattern).unwrap();
    store
        .create_abstract_knowledge(&candidate.knowledge, "initial candidate")
        .unwrap();
    let hypothesis = TransferHypothesis {
        id: TransferHypothesisId::new(),
        abstract_knowledge: AbstractKnowledgeRef {
            id: candidate.knowledge.id.clone(),
            revision: 1,
        },
        source_contexts: candidate.knowledge.provenance.source_contexts.clone(),
        target_context: scope("object-store"),
        expected_behavior: TransferExpectation::ImproveOutcome,
        status: TransferHypothesisStatus::Testable,
        evidence: Vec::new(),
        created_at: Utc::now(),
    };
    store.save_transfer_hypothesis(&hypothesis).unwrap();
    store
        .save_transfer_evaluation_set(&TransferEvaluationSet {
            hypothesis: hypothesis.id.clone(),
            source_contexts: hypothesis.source_contexts.clone(),
            held_out_contexts: vec![scope("object-store")],
            negative_controls: vec![scope("idempotent-api")],
        })
        .unwrap();
    let evidence = transfer(
        &candidate.knowledge,
        &hypothesis.id,
        TransferContextRole::HeldOut,
        TransferEvidenceOutcome::Supports,
        true,
        true,
        "object-store",
    );
    store.record_transfer_evidence(&evidence).unwrap();
    assert_eq!(
        store
            .transfer_evidence_for(&candidate.knowledge.id)
            .unwrap()
            .len(),
        1
    );
    let mut revised = candidate.knowledge.clone();
    revised.revision = 2;
    revised.maturity = KnowledgeMaturity::Supported;
    revised.updated_at = Utc::now();
    store
        .revise_abstract_knowledge(&revised, "held-out support", "transfer_supported")
        .unwrap();
    assert_eq!(
        store.abstract_knowledge_history(&revised.id).unwrap().len(),
        2
    );
    let db = rusqlite::Connection::open(fixture.home.join("hardknock.db")).unwrap();
    assert!(
        db.execute("UPDATE transfer_evidence SET outcome='contradicts'", [])
            .is_err()
    );
}

#[test]
fn transfer_planner_excludes_sources_and_requires_a_paired_budget() {
    let candidate = candidate(KnowledgeArtifactKind::Lesson);
    let held_out = TransferContext {
        selector: scope("object-store"),
        variables: vec![variable(
            "resource",
            ContextVariableKind::Resource,
            VariableValue::Text("object-store".into()),
            ContextRelevance::Varies,
        )],
        role: TransferContextRole::HeldOut,
    };
    assert!(
        DeterministicTransferContextPlanner
            .plan(&candidate, std::slice::from_ref(&held_out), &budget(1))
            .is_err()
    );
    let far_context = TransferContext {
        selector: scope("different-provider-and-consistency-model"),
        variables: vec![
            variable(
                "provider",
                ContextVariableKind::Environment,
                VariableValue::Text("different".into()),
                ContextRelevance::Varies,
            ),
            variable(
                "consistency",
                ContextVariableKind::Custom("consistency".into()),
                VariableValue::Text("eventual".into()),
                ContextRelevance::Varies,
            ),
        ],
        role: TransferContextRole::HeldOut,
    };
    let plan = DeterministicTransferContextPlanner
        .plan(&candidate, &[far_context, held_out.clone()], &budget(2))
        .unwrap();
    assert_eq!(plan.intent, ExperimentIntent::ValidateTransfer);
    assert!(plan.requires_equivalent_start);
    assert_eq!(plan.candidates.len(), 2);
    assert_eq!(plan.contexts[0].selector, held_out.selector);
}

#[test]
fn partial_staleness_and_contradiction_remain_explicit() {
    let artifact = KnowledgeArtifactRef {
        kind: KnowledgeArtifactKind::Lesson,
        id: "lesson-a".into(),
        revision: 1,
    };
    let partial = assess_abstraction_freshness(
        vec![
            MemberKnowledgeHealth {
                artifact: artifact.clone(),
                maturity: KnowledgeMaturity::Stale,
            },
            MemberKnowledgeHealth {
                artifact: KnowledgeArtifactRef {
                    id: "lesson-b".into(),
                    ..artifact.clone()
                },
                maturity: KnowledgeMaturity::Validated,
            },
        ],
        Vec::new(),
    );
    assert_eq!(partial.status, AbstractionFreshnessStatus::PartiallyStale);
    let contradicted = assess_abstraction_freshness(
        vec![MemberKnowledgeHealth {
            artifact,
            maturity: KnowledgeMaturity::Contradicted,
        }],
        Vec::new(),
    );
    assert_eq!(
        contradicted.status,
        AbstractionFreshnessStatus::Contradicted
    );
}

#[test]
fn abstract_antipattern_forecast_transfer_stays_an_inactive_candidate() {
    let mut knowledge = candidate(KnowledgeArtifactKind::AntiPattern).knowledge;
    knowledge.maturity = KnowledgeMaturity::Validated;
    let source = hardknock::predictive::EarlyWarningSignature {
        id: EarlyWarningSignatureId::new(),
        failure: hardknock::runtime::FailureSignatureRef {
            signature: "stale-state-mutation".into(),
        },
        ordered_conditions: vec![],
        failure_trajectory: None,
        horizon: hardknock::predictive::ForecastHorizon::NextAction,
        scope: scope("database"),
        evidence: vec![],
        status: hardknock::predictive::RiskIndicatorStatus::Validated,
        revision: 3,
        origin: hardknock::predictive::PredictiveOrigin::Local,
        causal_basis: vec![],
        required_runtime_version: None,
        precision: None,
        recall: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let transferred =
        transfer_early_warning_candidate(&knowledge, &source, scope("object-store")).unwrap();
    assert_ne!(transferred.id, source.id);
    assert_eq!(
        transferred.status,
        hardknock::predictive::RiskIndicatorStatus::Candidate
    );
    assert!(transferred.evidence.iter().any(|item| matches!(
        item,
        hardknock::predictive::TrajectoryEvidenceRef::Custom(value)
            if value.contains(&knowledge.id.to_string())
    )));
}

fn persist_candidate(store: &Store, member_count: usize, label: &str) -> AbstractKnowledgeId {
    let mut candidate = candidate(KnowledgeArtifactKind::Lesson);
    candidate.pattern.id = ExperiencePatternId::new();
    candidate.pattern.name = label.into();
    candidate.pattern.members = (0..member_count)
        .map(|index| KnowledgeArtifactRef {
            kind: KnowledgeArtifactKind::Lesson,
            id: format!("{label}-{index}"),
            revision: 1,
        })
        .collect();
    candidate.knowledge.id = AbstractKnowledgeId::new();
    candidate.knowledge.supporting_patterns = vec![candidate.pattern.id.clone()];
    candidate.knowledge.provenance.source_artifacts = candidate.pattern.members.clone();
    candidate.knowledge.provenance.source_contexts = (0..member_count)
        .map(|index| scope(&format!("{label}-{index}")))
        .collect();
    candidate.knowledge.provenance.root_origins = (0..member_count)
        .map(|index| format!("{label}-root-{index}"))
        .collect();
    let id = candidate.knowledge.id.clone();
    store.save_experience_pattern(&candidate.pattern).unwrap();
    store
        .create_abstract_knowledge(&candidate.knowledge, "economics candidate")
        .unwrap();
    id
}

#[test]
fn experience_economics_prioritizes_high_reuse_abstraction_at_equal_cost() {
    let fixture = Fixture::new();
    let store = Store::open(&fixture.home).unwrap();
    let broad = persist_candidate(&store, 15, "broad");
    let narrow = persist_candidate(&store, 2, "narrow");
    let context = store
        .experience_planning_context(ExperiencePortfolioObjective::Balanced)
        .unwrap();
    let opportunities = store.generate_experience_opportunities(&context).unwrap();
    let broad_opportunity = opportunities
        .iter()
        .find(|item| item.target == ExperienceOpportunityTarget::AbstractKnowledge(broad.clone()))
        .unwrap();
    let narrow_opportunity = opportunities
        .iter()
        .find(|item| item.target == ExperienceOpportunityTarget::AbstractKnowledge(narrow.clone()))
        .unwrap();
    assert_eq!(
        broad_opportunity.estimated_cost.trials,
        narrow_opportunity.estimated_cost.trials
    );
    let portfolio = store
        .create_experience_portfolio(&opportunities, &budget(2), &context)
        .unwrap();
    assert_eq!(portfolio.selected.len(), 1);
    assert_eq!(portfolio.selected[0].opportunity, broad_opportunity.id);
}

#[test]
fn abstraction_opportunities_compile_to_existing_engines() {
    let candidate = candidate(KnowledgeArtifactKind::Lesson);
    let context = hardknock::economics::ExperiencePlanningContext {
        gaps: Vec::new(),
        completed_dependencies: Default::default(),
        objective: ExperiencePortfolioObjective::Balanced,
        now: Utc::now(),
    };
    let mut opportunity = hardknock::economics::DeterministicExperienceOpportunityGenerator
        .generate(&hardknock::economics::ExperiencePlanningContext {
            gaps: vec![hardknock::economics::ExperienceGap {
                kind: ExperienceOpportunityKind::ValidateTransfer,
                target: ExperienceOpportunityTarget::AbstractKnowledge(candidate.knowledge.id),
                reasons: vec![
                    hardknock::economics::OpportunityReason::HighReuseKnowledgeFragmentation,
                ],
                severity: hardknock::curriculum::Severity::High,
                exposure: hardknock::economics::ExposureBand::Frequent,
                mitigation_gap: hardknock::economics::MitigationGap::Unknown,
                learning: hardknock::economics::LearningValueEstimate {
                    band: hardknock::economics::ValueBand::High,
                    possible_outcomes: vec![hardknock::economics::LearningOutcomeClass::Validate],
                    decision_changing_outcomes: 1,
                    rationale: vec!["test".into()],
                },
                decision_relevance: hardknock::economics::DecisionRelevance {
                    affected_decisions: 4,
                    affected_task_families: 2,
                    current_runtime_use: hardknock::economics::RuntimeUseBand::Medium,
                    likely_decision_change: hardknock::economics::ValueBand::High,
                },
                reuse: hardknock::economics::ReusePotential::Broad,
                novelty: hardknock::economics::EvidenceNovelty::ContextExtension,
                evidence: Default::default(),
                estimated_cost: Default::default(),
                risk: hardknock::economics::default_opportunity_risk(),
                dependencies: Vec::new(),
            }],
            ..context
        })
        .unwrap()
        .remove(0);
    assert!(matches!(
        DeterministicExperienceOpportunityCompiler
            .compile(&opportunity)
            .unwrap(),
        ExecutableLearningPlan::Experiment {
            intent: ExperimentIntent::ValidateTransfer,
            ..
        }
    ));
    opportunity.kind = ExperienceOpportunityKind::ChallengeAbstraction;
    assert!(matches!(
        DeterministicExperienceOpportunityCompiler
            .compile(&opportunity)
            .unwrap(),
        ExecutableLearningPlan::Curriculum {
            goal: CurriculumGoalKind::ChallengeAbstraction,
            ..
        }
    ));
}

#[test]
fn comparative_benchmark_supports_only_its_deterministic_claim() {
    let report = abstraction::benchmark::run().unwrap();
    assert!(report.deterministic);
    assert_eq!(report.model_calls, 0);
    assert_eq!(report.network_calls, 0);
    assert_eq!(report.families.len(), 3);
    assert!(report.scientific_hypothesis_supported);
    let specific = &report.results[0].metrics;
    let naive = &report.results[1].metrics;
    let empirical = &report.results[2].metrics;
    assert!(empirical.held_out_transfer_successes > specific.held_out_transfer_successes);
    assert!(empirical.negative_transfers < naive.negative_transfers);
    assert!(empirical.runtime_knowledge_items_injected < specific.runtime_knowledge_items_injected);
}

#[test]
fn runtime_resolution_is_model_free_and_bounded() {
    let context = RuntimeScenario::default().decision_context().unwrap();
    let started = Instant::now();
    for _ in 0..1_000 {
        let resolution = DeterministicKnowledgeResolutionPolicy
            .resolve(&context, &[])
            .unwrap();
        assert!(resolution.selected.is_empty());
    }
    assert!(started.elapsed().as_secs() < 2);
}

#[test]
fn federation_exports_only_validated_local_abstractions_as_advisory_objects() {
    let fixture = Fixture::new();
    let store = Store::open(&fixture.home).unwrap();
    let mut candidate = candidate(KnowledgeArtifactKind::Lesson);
    candidate.knowledge.maturity = KnowledgeMaturity::Validated;
    candidate.knowledge.provenance.origin = KnowledgeOrigin::Local;
    store.save_experience_pattern(&candidate.pattern).unwrap();
    store
        .create_abstract_knowledge(&candidate.knowledge, "validated locally")
        .unwrap();
    let config = Config::load(&fixture.home).unwrap();
    let service = LocalFederationService {
        store: &store,
        config: &config,
    };
    let bundle = service
        .export_abstraction(&candidate.knowledge.id, vec!["abstraction".into()])
        .unwrap();
    assert_eq!(bundle.bundle.abstract_knowledge.len(), 1);
    assert_eq!(
        bundle.bundle.abstract_knowledge[0].source_maturity,
        KnowledgeMaturity::Validated
    );
    assert_eq!(bundle.bundle.object_count(), 1);

    let mut advisory = candidate.knowledge;
    advisory.id = AbstractKnowledgeId::new();
    advisory.provenance.origin = KnowledgeOrigin::FederatedAdvisory;
    store
        .create_abstract_knowledge(&advisory, "remote advisory")
        .unwrap();
    assert!(service.export_abstraction(&advisory.id, vec![]).is_err());
}

#[test]
fn cli_exposes_required_pattern_and_abstraction_commands() {
    let fixture = Fixture::new();
    let list = fixture.cli(&["pattern", "list"], 0);
    assert_eq!(list["result"]["kind"], "experience_patterns");
    let benchmark = fixture.cli(&["abstract", "benchmark"], 0);
    assert_eq!(benchmark["result"]["kind"], "abstraction_benchmark");
    assert_eq!(benchmark["result"]["report"]["model_calls"], 0);
}

#[test]
fn fixture_library_covers_required_abstraction_families() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/abstraction");
    for name in [
        "authoritative-state",
        "quorum-availability",
        "misleading-surface-similarity",
        "specific-exception",
        "recovery-transfer",
        "failed-recovery-transfer",
        "skill-transfer",
        "staleness",
        "federated-advisory",
        "correlated-origins",
        "retrieval-compression",
        "counterexample-boundary",
    ] {
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join(name).join("fixture.json")).unwrap())
                .unwrap();
        assert_eq!(value["version"], 1, "fixture {name}");
    }
}
