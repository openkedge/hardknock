// SPDX-License-Identifier: Apache-2.0
use super::{AssuranceStore, EpistemicStore, RuntimeStore, Store, ToolStore};
use crate::{
    Error, Result,
    budget::ExperienceBudget,
    causal::CausalHypothesisStatus,
    curriculum::{Severity, TrialSafety},
    economics::*,
    effects::EffectRisk,
    epistemic::ClaimKind,
    federation::FederatedExperienceState,
    lesson::{EvidenceRelationship, LessonStatus},
    predictive::{ForecastHealth, Forecastability, RiskIndicatorStatus},
    resilience::{RecoveryStatus, ReflexStatus, SkillStatus},
};
use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use std::{collections::BTreeSet, time::Duration};

fn json(value: &impl serde::Serialize) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}

fn name(value: &impl serde::Serialize) -> Result<String> {
    Ok(serde_json::to_value(value)?
        .as_str()
        .unwrap_or("custom")
        .to_owned())
}

fn target_parts(target: &ExperienceOpportunityTarget) -> Result<(String, String)> {
    let value = serde_json::to_value(target)?;
    Ok((
        value
            .get("target")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        value
            .get("id")
            .map(serde_json::Value::to_string)
            .unwrap_or_else(|| "null".into()),
    ))
}

fn event(tx: &Transaction<'_>, subject: &str, kind: &str, data: serde_json::Value) -> Result<()> {
    tx.execute(
        "INSERT INTO experience_economics_events(subject,kind,data) VALUES(?1,?2,?3)",
        params![subject, kind, data.to_string()],
    )?;
    tx.execute(
        "INSERT INTO bridge_events(session_id,kind,data) VALUES('experience-economics',?1,?2)",
        params![kind, serde_json::json!({"subject":subject}).to_string()],
    )?;
    Ok(())
}

fn exposure(count: u64) -> ExposureBand {
    match count {
        0 | 1 => ExposureBand::Rare,
        2..=4 => ExposureBand::Occasional,
        5..=19 => ExposureBand::Frequent,
        _ => ExposureBand::VeryFrequent,
    }
}

fn learning(
    band: ValueBand,
    outcomes: Vec<LearningOutcomeClass>,
    rationale: &str,
) -> LearningValueEstimate {
    let decision_changing_outcomes = outcomes
        .iter()
        .filter(|outcome| {
            !matches!(
                outcome,
                LearningOutcomeClass::Strengthen | LearningOutcomeClass::NoMaterialChange
            )
        })
        .count();
    LearningValueEstimate {
        band,
        possible_outcomes: outcomes,
        decision_changing_outcomes,
        rationale: vec![rationale.into()],
    }
}

fn relevance(decisions: usize, families: usize) -> DecisionRelevance {
    DecisionRelevance {
        affected_decisions: decisions,
        affected_task_families: families,
        current_runtime_use: match decisions {
            0 => RuntimeUseBand::None,
            1..=4 => RuntimeUseBand::Low,
            5..=19 => RuntimeUseBand::Medium,
            _ => RuntimeUseBand::High,
        },
        likely_decision_change: match decisions {
            0 => ValueBand::Low,
            1..=4 => ValueBand::Medium,
            5..=19 => ValueBand::High,
            _ => ValueBand::Critical,
        },
    }
}

fn evidence(
    observations: usize,
    replications: usize,
    contexts: usize,
    counterfactuals: usize,
    diversity: usize,
    contradictions: usize,
    fresh: bool,
) -> EvidenceSummary {
    EvidenceSummary {
        observations,
        equivalent_replications: replications,
        distinct_contexts: contexts,
        counterfactuals,
        diversity_domains: diversity,
        contradictions,
        fresh,
    }
}

fn risk(safety: TrialSafety, effect: EffectRisk, approval_required: bool) -> OpportunityRisk {
    OpportunityRisk {
        trial_safety: safety,
        external_effect_risk: effect,
        approval_required,
        ..default_opportunity_risk()
    }
}

impl Store {
    /// Project current persisted evidence gaps into the V0.16 planning model.
    /// The projection is deterministic and does not schedule work.
    pub fn experience_planning_context(
        &self,
        objective: ExperiencePortfolioObjective,
    ) -> Result<ExperiencePlanningContext> {
        let runtime_decisions = self.runtime_decisions()?;
        let mut gaps = Vec::new();

        for gap in self.runtime_gaps()? {
            gaps.push(ExperienceGap {
                kind: ExperienceOpportunityKind::ResolveRuntimeUnknown,
                target: ExperienceOpportunityTarget::RuntimeGap(gap.context_hash.clone()),
                reasons: vec![if matches!(
                    gap.decision,
                    crate::runtime::RuntimeDecisionKind::Abstain
                ) {
                    OpportunityReason::RuntimeAbstention
                } else {
                    OpportunityReason::RuntimeUnknown
                }],
                severity: if matches!(gap.decision, crate::runtime::RuntimeDecisionKind::Abstain) {
                    Severity::High
                } else {
                    Severity::Medium
                },
                exposure: exposure(gap.occurrences),
                mitigation_gap: MitigationGap::Unknown,
                learning: learning(
                    ValueBand::High,
                    vec![
                        LearningOutcomeClass::ChangeRuntimeDecision,
                        LearningOutcomeClass::NoMaterialChange,
                    ],
                    "A bounded experiment can resolve a runtime unknown or justify abstention",
                ),
                decision_relevance: relevance(
                    usize::try_from(gap.occurrences).unwrap_or(usize::MAX),
                    usize::from(gap.task_family.is_some()),
                ),
                reuse: if gap.task_family.is_some() {
                    ReusePotential::Reusable
                } else {
                    ReusePotential::Narrow
                },
                novelty: EvidenceNovelty::ContextExtension,
                evidence: evidence(0, 0, 0, 0, 0, 0, true),
                estimated_cost: ExperimentCost {
                    trials: 2,
                    ..Default::default()
                },
                risk: risk(TrialSafety::RequiresIsolation, EffectRisk::ReadOnly, false),
                dependencies: Vec::new(),
            });
        }

        let misses = self.forecast_misses()?;
        for miss in &misses {
            let count = misses
                .iter()
                .filter(|item| item.failure == miss.failure)
                .count();
            gaps.push(ExperienceGap {
                kind: ExperienceOpportunityKind::InvestigateForecastMiss,
                target: ExperienceOpportunityTarget::FailureSignature(miss.failure.clone()),
                reasons: vec![
                    OpportunityReason::ForecastMiss,
                    OpportunityReason::RepeatedFailure,
                ],
                severity: Severity::High,
                exposure: exposure(u64::try_from(count).unwrap_or(u64::MAX)),
                mitigation_gap: if miss.forecastability
                    == Forecastability::InsufficientObservability
                {
                    MitigationGap::Unmitigated
                } else {
                    MitigationGap::Significant
                },
                learning: learning(
                    ValueBand::High,
                    vec![
                        LearningOutcomeClass::DiscoverFailureBoundary,
                        LearningOutcomeClass::ChangeRuntimeDecision,
                    ],
                    "A missed failure may reveal a new trajectory basin or an observability gap",
                ),
                decision_relevance: relevance(count, 1),
                reuse: ReusePotential::Reusable,
                novelty: EvidenceNovelty::NewFailureClass,
                evidence: evidence(count, 0, 1, 0, 1, 0, true),
                estimated_cost: ExperimentCost {
                    trials: 3,
                    ..Default::default()
                },
                risk: risk(TrialSafety::RequiresIsolation, EffectRisk::ReadOnly, false),
                dependencies: Vec::new(),
            });
        }

        for warning in self.warning_signatures()? {
            let health = self.forecast_health(&warning.id)?;
            let (kind, reasons, novelty) = match (warning.status, health.health) {
                (RiskIndicatorStatus::Candidate | RiskIndicatorStatus::Supported, _) => (
                    ExperienceOpportunityKind::ValidateEarlyWarning,
                    vec![OpportunityReason::ForecastMiss],
                    EvidenceNovelty::Replication,
                ),
                (RiskIndicatorStatus::Stale, _) | (_, ForecastHealth::Stale) => (
                    ExperienceOpportunityKind::RevalidateStaleExperience,
                    vec![OpportunityReason::EvidenceStale],
                    EvidenceNovelty::ContextExtension,
                ),
                (RiskIndicatorStatus::Noisy | RiskIndicatorStatus::Contradicted, _)
                | (_, ForecastHealth::Degrading | ForecastHealth::Contradicted) => (
                    ExperienceOpportunityKind::ReduceForecastFalsePositives,
                    vec![OpportunityReason::ForecastNoisy],
                    EvidenceNovelty::MechanismChallenge,
                ),
                _ => continue,
            };
            let evidence_count = warning.evidence.len();
            gaps.push(ExperienceGap {
                kind,
                target: ExperienceOpportunityTarget::EarlyWarning(warning.id),
                reasons,
                severity: Severity::High,
                exposure: ExposureBand::Frequent,
                mitigation_gap: MitigationGap::Partial,
                learning: learning(
                    ValueBand::High,
                    vec![
                        LearningOutcomeClass::Validate,
                        LearningOutcomeClass::NarrowScope,
                        LearningOutcomeClass::Retire,
                    ],
                    "Positive, negative-control, and held-out paths can change warning behavior",
                ),
                decision_relevance: relevance(evidence_count, 1),
                reuse: ReusePotential::Reusable,
                novelty,
                evidence: evidence(
                    evidence_count,
                    evidence_count.saturating_sub(1),
                    1,
                    warning.causal_basis.len(),
                    1,
                    usize::from(warning.status == RiskIndicatorStatus::Contradicted),
                    warning.status != RiskIndicatorStatus::Stale,
                ),
                estimated_cost: ExperimentCost {
                    trials: 3,
                    ..Default::default()
                },
                risk: risk(TrialSafety::RequiresIsolation, EffectRisk::ReadOnly, false),
                dependencies: Vec::new(),
            });
        }

        for hypothesis in self.causal_hypotheses()? {
            if hypothesis.status.supported()
                || matches!(
                    hypothesis.status,
                    CausalHypothesisStatus::Retired | CausalHypothesisStatus::Untestable
                )
            {
                continue;
            }
            let contradicted = hypothesis.status == CausalHypothesisStatus::Contradicted;
            gaps.push(ExperienceGap {
                kind: if contradicted {
                    ExperienceOpportunityKind::ResolveContradiction
                } else {
                    ExperienceOpportunityKind::DiscriminateCausalHypotheses
                },
                target: ExperienceOpportunityTarget::CausalHypothesis(hypothesis.id),
                reasons: vec![if contradicted {
                    OpportunityReason::EvidenceContradicted
                } else {
                    OpportunityReason::CausalMechanismUnknown
                }],
                severity: if contradicted {
                    Severity::High
                } else {
                    Severity::Medium
                },
                exposure: ExposureBand::Occasional,
                mitigation_gap: MitigationGap::Unknown,
                learning: learning(
                    if contradicted {
                        ValueBand::Critical
                    } else {
                        ValueBand::High
                    },
                    vec![
                        LearningOutcomeClass::Contradict,
                        LearningOutcomeClass::Strengthen,
                        LearningOutcomeClass::NarrowScope,
                    ],
                    "A controlled intervention can discriminate explicit causal predictions",
                ),
                decision_relevance: relevance(hypothesis.evidence.len(), 1),
                reuse: ReusePotential::Reusable,
                novelty: EvidenceNovelty::MechanismChallenge,
                evidence: evidence(
                    hypothesis.evidence.len(),
                    hypothesis.evidence.len(),
                    1,
                    hypothesis.evidence.len(),
                    1,
                    usize::from(contradicted),
                    true,
                ),
                estimated_cost: ExperimentCost {
                    trials: 2,
                    ..Default::default()
                },
                risk: risk(TrialSafety::RequiresIsolation, EffectRisk::ReadOnly, false),
                dependencies: Vec::new(),
            });
        }

        for lesson in self.all_lessons()? {
            let influenced = runtime_decisions
                .iter()
                .filter(|decision| {
                    decision
                        .context
                        .relevant_experience
                        .lessons
                        .iter()
                        .any(|item| item.lesson.id == lesson.id)
                })
                .count();
            let contradictions = lesson
                .evidence
                .iter()
                .filter(|item| {
                    matches!(
                        item,
                        crate::lesson::EvidenceRef::Experience {
                            relationship: EvidenceRelationship::Contradicts,
                            ..
                        } | crate::lesson::EvidenceRef::Trial {
                            relationship: EvidenceRelationship::Contradicts,
                            ..
                        }
                    )
                })
                .count();
            if lesson.status != LessonStatus::Candidate
                && lesson.status != LessonStatus::Contradicted
                && contradictions == 0
            {
                continue;
            }
            let contradicted = lesson.status == LessonStatus::Contradicted || contradictions > 0;
            let mut reasons = vec![if contradicted {
                OpportunityReason::EvidenceContradicted
            } else {
                OpportunityReason::HighUsageSkillGap
            }];
            if influenced >= 5 {
                reasons.push(OpportunityReason::LargeBlastRadius);
            }
            gaps.push(ExperienceGap {
                kind: if contradicted {
                    ExperienceOpportunityKind::ResolveContradiction
                } else {
                    ExperienceOpportunityKind::ValidateLesson
                },
                target: ExperienceOpportunityTarget::Lesson(lesson.id),
                reasons,
                severity: if contradicted {
                    Severity::High
                } else {
                    Severity::Medium
                },
                exposure: exposure(u64::try_from(influenced).unwrap_or(u64::MAX)),
                mitigation_gap: MitigationGap::Partial,
                learning: learning(
                    if contradicted {
                        ValueBand::Critical
                    } else {
                        ValueBand::High
                    },
                    vec![
                        LearningOutcomeClass::Validate,
                        LearningOutcomeClass::NarrowScope,
                        LearningOutcomeClass::Retire,
                    ],
                    "The result can change a persisted Lesson and every decision that consumes it",
                ),
                decision_relevance: relevance(influenced, 1),
                reuse: if influenced >= 5 {
                    ReusePotential::Broad
                } else {
                    ReusePotential::Narrow
                },
                novelty: if contradicted {
                    EvidenceNovelty::MechanismChallenge
                } else {
                    EvidenceNovelty::Replication
                },
                evidence: evidence(
                    lesson.evidence.len(),
                    lesson.evidence.len(),
                    1,
                    usize::from(lesson.status == LessonStatus::CounterfactuallySupported),
                    lesson.discovered_by.len(),
                    contradictions,
                    true,
                ),
                estimated_cost: ExperimentCost {
                    trials: 2,
                    ..Default::default()
                },
                risk: risk(TrialSafety::RequiresIsolation, EffectRisk::ReadOnly, false),
                dependencies: Vec::new(),
            });
        }

        for recovery in self.recoveries()? {
            if matches!(
                recovery.status,
                RecoveryStatus::Validated | RecoveryStatus::Retired
            ) {
                continue;
            }
            let contradicted = recovery.status == RecoveryStatus::Contradicted;
            gaps.push(ExperienceGap {
                kind: if contradicted {
                    ExperienceOpportunityKind::ResolveContradiction
                } else {
                    ExperienceOpportunityKind::ValidateRecovery
                },
                target: ExperienceOpportunityTarget::Recovery(recovery.id),
                reasons: vec![if contradicted {
                    OpportunityReason::EvidenceContradicted
                } else {
                    OpportunityReason::RecoveryMissing
                }],
                severity: Severity::High,
                exposure: ExposureBand::Frequent,
                mitigation_gap: if contradicted {
                    MitigationGap::Unmitigated
                } else {
                    MitigationGap::Significant
                },
                learning: learning(
                    ValueBand::High,
                    vec![
                        LearningOutcomeClass::Validate,
                        LearningOutcomeClass::DiscoverRecovery,
                        LearningOutcomeClass::Contradict,
                    ],
                    "A controlled recovery trial can close a recurring mitigation gap",
                ),
                decision_relevance: relevance(recovery.evidence.len(), 1),
                reuse: ReusePotential::Reusable,
                novelty: EvidenceNovelty::Replication,
                evidence: evidence(
                    recovery.evidence.len(),
                    recovery.evidence.len(),
                    1,
                    0,
                    1,
                    usize::from(contradicted),
                    true,
                ),
                estimated_cost: ExperimentCost {
                    trials: 2,
                    ..Default::default()
                },
                risk: risk(TrialSafety::RequiresIsolation, EffectRisk::ReadOnly, false),
                dependencies: Vec::new(),
            });
        }

        for reflex in self.reflexes()? {
            if matches!(reflex.status, ReflexStatus::Active | ReflexStatus::Retired) {
                continue;
            }
            gaps.push(ExperienceGap {
                kind: ExperienceOpportunityKind::ValidateReflex,
                target: ExperienceOpportunityTarget::Reflex(reflex.id),
                reasons: vec![if reflex.status == ReflexStatus::Disabled {
                    OpportunityReason::ReflexNoisy
                } else {
                    OpportunityReason::HighUsageSkillGap
                }],
                severity: Severity::Medium,
                exposure: ExposureBand::Occasional,
                mitigation_gap: MitigationGap::Partial,
                learning: learning(
                    ValueBand::Medium,
                    vec![LearningOutcomeClass::Validate, LearningOutcomeClass::Retire],
                    "Positive and false-positive controls can change Reflex activation",
                ),
                decision_relevance: relevance(reflex.evidence.len(), 1),
                reuse: ReusePotential::Reusable,
                novelty: EvidenceNovelty::Replication,
                evidence: evidence(
                    reflex.evidence.len(),
                    reflex.evidence.len(),
                    1,
                    0,
                    1,
                    0,
                    true,
                ),
                estimated_cost: ExperimentCost {
                    trials: 2,
                    ..Default::default()
                },
                risk: risk(TrialSafety::RequiresIsolation, EffectRisk::ReadOnly, false),
                dependencies: Vec::new(),
            });
        }

        for claim in self.claims()? {
            let report = self.epistemic_report(&claim.id)?;
            if report.gaps.is_empty() {
                continue;
            }
            let contexts = report
                .paths
                .iter()
                .map(|path| path.context.fingerprint.hash.clone())
                .collect::<BTreeSet<_>>()
                .len();
            let contradictions = report
                .paths
                .iter()
                .filter(|path| path.outcome == crate::epistemic::EvidenceOutcome::Contradicts)
                .count();
            let severity = if matches!(
                claim.kind,
                ClaimKind::FailureCause
                    | ClaimKind::RecoveryClaim
                    | ClaimKind::RuntimeDecisionClaim
            ) {
                Severity::High
            } else {
                Severity::Medium
            };
            gaps.push(ExperienceGap {
                kind: ExperienceOpportunityKind::IncreaseEvidenceDiversity,
                target: ExperienceOpportunityTarget::AssuranceGap(format!(
                    "epistemic:{}",
                    claim.id
                )),
                reasons: vec![OpportunityReason::LowEvidenceDiversity],
                severity,
                exposure: exposure(u64::try_from(report.paths.len()).unwrap_or(u64::MAX)),
                mitigation_gap: MitigationGap::Unknown,
                learning: learning(
                    ValueBand::High,
                    vec![
                        LearningOutcomeClass::Strengthen,
                        LearningOutcomeClass::Contradict,
                        LearningOutcomeClass::ChangeRuntimeDecision,
                    ],
                    "A controlled or independent evidence path can close a named diversity gap",
                ),
                decision_relevance: relevance(report.paths.len(), 1),
                reuse: ReusePotential::Reusable,
                novelty: EvidenceNovelty::ContextExtension,
                evidence: evidence(
                    report.paths.len(),
                    report.paths.len(),
                    contexts,
                    usize::from(report.gaps.iter().any(|gap| gap.contains("controlled"))),
                    report.diversity.source_type_count,
                    contradictions,
                    true,
                ),
                estimated_cost: ExperimentCost {
                    trials: 1,
                    agent_runs: 1,
                    ..Default::default()
                },
                risk: risk(TrialSafety::RequiresIsolation, EffectRisk::ReadOnly, false),
                dependencies: Vec::new(),
            });
        }

        for skill in self.skills()? {
            if skill.status == SkillStatus::Supported {
                gaps.push(ExperienceGap {
                    kind: ExperienceOpportunityKind::HardenSkill,
                    target: ExperienceOpportunityTarget::Skill(skill.id.clone()),
                    reasons: vec![OpportunityReason::HighUsageSkillGap],
                    severity: Severity::Medium,
                    exposure: ExposureBand::Occasional,
                    mitigation_gap: MitigationGap::Partial,
                    learning: learning(
                        ValueBand::Medium,
                        vec![
                            LearningOutcomeClass::Validate,
                            LearningOutcomeClass::NarrowScope,
                        ],
                        "Curriculum can test a supported Skill across named conditions",
                    ),
                    decision_relevance: relevance(skill.evidence.len(), 1),
                    reuse: ReusePotential::Reusable,
                    novelty: EvidenceNovelty::ContextExtension,
                    evidence: evidence(
                        skill.evidence.len(),
                        skill.evidence.len(),
                        1,
                        0,
                        1,
                        0,
                        true,
                    ),
                    estimated_cost: ExperimentCost {
                        trials: 3,
                        ..Default::default()
                    },
                    risk: risk(TrialSafety::RequiresIsolation, EffectRisk::ReadOnly, false),
                    dependencies: Vec::new(),
                });
            }
            if matches!(
                skill.status,
                SkillStatus::Supported | SkillStatus::Validated
            ) && skill.operating_envelope.is_none()
            {
                gaps.push(ExperienceGap {
                    kind: ExperienceOpportunityKind::ExploreOperatingEnvelope,
                    target: ExperienceOpportunityTarget::Skill(skill.id.clone()),
                    reasons: vec![OpportunityReason::OperatingEnvelopeUnknown],
                    severity: Severity::Medium,
                    exposure: ExposureBand::Occasional,
                    mitigation_gap: MitigationGap::Unknown,
                    learning: learning(
                        ValueBand::High,
                        vec![
                            LearningOutcomeClass::DiscoverFailureBoundary,
                            LearningOutcomeClass::NarrowScope,
                        ],
                        "Boundary trials can replace an unknown operating envelope with tested points",
                    ),
                    decision_relevance: relevance(skill.evidence.len(), 1),
                    reuse: ReusePotential::Reusable,
                    novelty: EvidenceNovelty::ContextExtension,
                    evidence: evidence(skill.evidence.len(), 0, 1, 0, 1, 0, true),
                    estimated_cost: ExperimentCost {
                        trials: 3,
                        ..Default::default()
                    },
                    risk: risk(TrialSafety::RequiresIsolation, EffectRisk::ReadOnly, false),
                    dependencies: Vec::new(),
                });
            }
            if matches!(
                skill.status,
                SkillStatus::Supported | SkillStatus::Validated
            ) && self.skill_certifications(&skill.id)?.is_empty()
            {
                gaps.push(ExperienceGap {
                    kind: ExperienceOpportunityKind::CloseAssuranceGap,
                    target: ExperienceOpportunityTarget::AssuranceGap(format!(
                        "skill:{}:local-certification",
                        skill.id
                    )),
                    reasons: vec![OpportunityReason::AssuranceBlocked],
                    severity: Severity::High,
                    exposure: exposure(u64::try_from(skill.evidence.len()).unwrap_or(u64::MAX)),
                    mitigation_gap: MitigationGap::Significant,
                    learning: learning(
                        ValueBand::High,
                        vec![
                            LearningOutcomeClass::ChangeAssurance,
                            LearningOutcomeClass::NarrowScope,
                        ],
                        "A named certification curriculum can close the local assurance gap",
                    ),
                    decision_relevance: relevance(skill.evidence.len(), 1),
                    reuse: ReusePotential::Broad,
                    novelty: EvidenceNovelty::ContextExtension,
                    evidence: evidence(skill.evidence.len(), 0, 1, 0, 1, 0, true),
                    estimated_cost: ExperimentCost {
                        trials: 2,
                        ..Default::default()
                    },
                    risk: risk(TrialSafety::RequiresIsolation, EffectRisk::ReadOnly, false),
                    dependencies: Vec::new(),
                });
            }
        }

        let attestations = self.execution_attestations(None)?;
        for tool in self.tool_definitions(false)? {
            if tool.capabilities.network.mode == crate::capability::NetworkMode::None {
                continue;
            }
            let uses = attestations
                .iter()
                .filter(|attestation| attestation.tool.id == tool.id)
                .count();
            gaps.push(ExperienceGap {
                kind: ExperienceOpportunityKind::MinimizeCapability,
                target: ExperienceOpportunityTarget::Tool(tool.id),
                reasons: vec![OpportunityReason::CapabilityOverprovisioned],
                severity: Severity::High,
                exposure: exposure(u64::try_from(uses).unwrap_or(u64::MAX)),
                mitigation_gap: MitigationGap::Significant,
                learning: learning(
                    ValueBand::High,
                    vec![
                        LearningOutcomeClass::ChangeAssurance,
                        LearningOutcomeClass::NoMaterialChange,
                    ],
                    "A no-network replay can test whether broad authority is unnecessary",
                ),
                decision_relevance: relevance(uses, 1),
                reuse: ReusePotential::Broad,
                novelty: EvidenceNovelty::MechanismChallenge,
                evidence: evidence(uses, 0, 1, 0, 1, 0, true),
                estimated_cost: ExperimentCost::default(),
                risk: risk(TrialSafety::RequiresIsolation, EffectRisk::ReadOnly, false),
                dependencies: Vec::new(),
            });
        }

        for object in self.federated_objects()? {
            if !matches!(
                object.state,
                FederatedExperienceState::ContextMatched
                    | FederatedExperienceState::ReproductionRecommended
            ) {
                continue;
            }
            gaps.push(ExperienceGap {
                kind: ExperienceOpportunityKind::ReproduceFederatedExperience,
                target: ExperienceOpportunityTarget::FederatedObject(
                    crate::epistemic::FederatedObjectRef {
                        node_id: object.identity.origin_node,
                        object_id: object.identity.origin_object_id,
                    },
                ),
                reasons: vec![OpportunityReason::FederatedEvidenceUnreproduced],
                severity: Severity::Medium,
                exposure: ExposureBand::Occasional,
                mitigation_gap: MitigationGap::Unknown,
                learning: learning(
                    ValueBand::High,
                    vec![LearningOutcomeClass::Validate, LearningOutcomeClass::Contradict],
                    "Local reproduction is cheaper than rediscovery while preserving advisory trust",
                ),
                decision_relevance: relevance(1, 1),
                reuse: ReusePotential::Reusable,
                novelty: EvidenceNovelty::ContextExtension,
                evidence: evidence(1, 0, 1, 0, 1, 0, true),
                estimated_cost: ExperimentCost {
                    trials: 2,
                    ..Default::default()
                },
                risk: risk(TrialSafety::RequiresIsolation, EffectRisk::ReadOnly, false),
                dependencies: Vec::new(),
            });
        }

        Ok(ExperiencePlanningContext {
            gaps,
            completed_dependencies: self
                .opportunity_results()?
                .into_iter()
                .filter(|result| {
                    matches!(
                        result.outcome,
                        OpportunityOutcome::MaterialLearning
                            | OpportunityOutcome::StrengthenedExistingEvidence
                            | OpportunityOutcome::NoMaterialChange
                    )
                })
                .map(|result| result.opportunity)
                .collect(),
            objective,
            now: Utc::now(),
        })
    }

    pub fn generate_experience_opportunities(
        &self,
        context: &ExperiencePlanningContext,
    ) -> Result<Vec<ExperienceOpportunity>> {
        let generated = DeterministicExperienceOpportunityGenerator.generate(context)?;
        for opportunity in &generated {
            self.save_experience_opportunity(opportunity)?;
        }
        generated
            .iter()
            .map(|opportunity| self.experience_opportunity(&opportunity.id))
            .collect()
    }

    pub fn save_experience_opportunity(&self, opportunity: &ExperienceOpportunity) -> Result<()> {
        let mut opportunity = opportunity.clone();
        let (target_kind, target_id) = target_parts(&opportunity.target)?;
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT data FROM experience_opportunities WHERE id=?1",
                [opportunity.id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            let current: ExperienceOpportunity = serde_json::from_str(&existing)?;
            if matches!(
                current.status,
                ExperienceOpportunityStatus::Running
                    | ExperienceOpportunityStatus::Completed
                    | ExperienceOpportunityStatus::Invalidated
            ) {
                return Ok(());
            }
            opportunity.created_at = current.created_at;
            tx.execute(
                "UPDATE experience_opportunities SET kind=?2,target_kind=?3,target_id=?4,status=?5,data=?6 WHERE id=?1",
                params![opportunity.id.to_string(),name(&opportunity.kind)?,target_kind,target_id,name(&opportunity.status)?,json(&opportunity)?],
            )?;
            tx.execute(
                "DELETE FROM experience_opportunity_reasons WHERE opportunity_id=?1",
                [opportunity.id.to_string()],
            )?;
        } else {
            tx.execute(
                "INSERT INTO experience_opportunities(id,kind,target_kind,target_id,status,created_at,data) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![opportunity.id.to_string(),name(&opportunity.kind)?,target_kind,target_id,name(&opportunity.status)?,opportunity.created_at.to_rfc3339(),json(&opportunity)?],
            )?;
            event(
                &tx,
                &opportunity.id.to_string(),
                "experience_opportunity_created",
                serde_json::json!({"kind":opportunity.kind,"target":opportunity.target}),
            )?;
        }
        for (position, reason) in opportunity.rationale.iter().enumerate() {
            tx.execute(
                "INSERT INTO experience_opportunity_reasons(opportunity_id,position,reason) VALUES(?1,?2,?3)",
                params![opportunity.id.to_string(),i64::try_from(position).map_err(|_|Error::InvalidInput("Opportunity reason overflow".into()))?,name(reason)?],
            )?;
        }
        tx.execute(
            "INSERT INTO experiment_cost_estimates(opportunity_id,data) VALUES(?1,?2) ON CONFLICT(opportunity_id) DO UPDATE SET data=excluded.data",
            params![opportunity.id.to_string(), json(&opportunity.estimated_cost)?],
        )?;
        tx.execute(
            "INSERT INTO evidence_saturation_records(opportunity_id,observed_at,saturation,data) VALUES(?1,?2,?3,?4)",
            params![opportunity.id.to_string(),Utc::now().to_rfc3339(),name(&opportunity.saturation)?,json(&opportunity.marginal_value)?],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn experience_opportunity(
        &self,
        id: &crate::core::ExperienceOpportunityId,
    ) -> Result<ExperienceOpportunity> {
        self.get(
            "SELECT data FROM experience_opportunities WHERE id=?1",
            &id.to_string(),
        )
    }

    pub fn experience_opportunities(&self) -> Result<Vec<ExperienceOpportunity>> {
        self.list("SELECT data FROM experience_opportunities ORDER BY created_at,id")
    }

    pub fn invalidate_experience_opportunity(
        &self,
        id: &crate::core::ExperienceOpportunityId,
        reason: &str,
    ) -> Result<ExperienceOpportunity> {
        if reason.trim().is_empty() || reason.len() > 2048 {
            return Err(Error::InvalidInput(
                "Opportunity invalidation requires a nonempty reason of at most 2048 bytes".into(),
            ));
        }
        let mut opportunity = self.experience_opportunity(id)?;
        if opportunity.status == ExperienceOpportunityStatus::Completed {
            return Err(Error::Intervention(
                "A completed opportunity result is immutable; create a new opportunity instead"
                    .into(),
            ));
        }
        opportunity.status = ExperienceOpportunityStatus::Invalidated;
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE experience_opportunities SET status='invalidated',data=?2 WHERE id=?1",
            params![id.to_string(), json(&opportunity)?],
        )?;
        event(
            &tx,
            &id.to_string(),
            "experience_opportunity_invalidated",
            serde_json::json!({"reason":reason}),
        )?;
        tx.commit()?;
        Ok(opportunity)
    }

    pub fn create_experience_portfolio(
        &self,
        opportunities: &[ExperienceOpportunity],
        budget: &ExperienceBudget,
        context: &ExperiencePlanningContext,
    ) -> Result<ExperiencePortfolio> {
        for opportunity in opportunities {
            self.save_experience_opportunity(opportunity)?;
        }
        let portfolio =
            DeterministicExperienceAllocationPolicy.allocate(opportunities, budget, context)?;
        self.persist_experience_portfolio(&portfolio, true)?;
        Ok(portfolio)
    }

    fn persist_experience_portfolio(
        &self,
        portfolio: &ExperiencePortfolio,
        created: bool,
    ) -> Result<()> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        if created {
            tx.execute(
                "INSERT INTO experience_portfolios(id,revision,objective,created_at,data) VALUES(?1,?2,?3,?4,?5)",
                params![portfolio.id.to_string(),i64::try_from(portfolio.revision).map_err(|_|Error::InvalidInput("Portfolio revision overflow".into()))?,name(&portfolio.objective)?,portfolio.created_at.to_rfc3339(),json(portfolio)?],
            )?;
        } else {
            let changed = tx.execute(
                "UPDATE experience_portfolios SET revision=?2,objective=?3,data=?4 WHERE id=?1 AND revision=?5",
                params![portfolio.id.to_string(),i64::try_from(portfolio.revision).map_err(|_|Error::InvalidInput("Portfolio revision overflow".into()))?,name(&portfolio.objective)?,json(portfolio)?,i64::try_from(portfolio.revision.saturating_sub(1)).map_err(|_|Error::InvalidInput("Portfolio revision overflow".into()))?],
            )?;
            if changed != 1 {
                return Err(Error::Intervention(
                    "Portfolio changed concurrently; recompute before persisting".into(),
                ));
            }
        }
        tx.execute(
            "INSERT INTO experience_portfolio_revisions(portfolio_id,revision,reason,created_at,data) VALUES(?1,?2,?3,?4,?5)",
            params![portfolio.id.to_string(),i64::try_from(portfolio.revision).map_err(|_|Error::InvalidInput("Portfolio revision overflow".into()))?,portfolio.revision_reason.as_ref().map(name).transpose()?,Utc::now().to_rfc3339(),json(portfolio)?],
        )?;
        for selection in &portfolio.selected {
            tx.execute(
                "INSERT INTO portfolio_selections(portfolio_id,revision,opportunity_id,priority,data) VALUES(?1,?2,?3,?4,?5)",
                params![portfolio.id.to_string(),i64::try_from(portfolio.revision).map_err(|_|Error::InvalidInput("Portfolio revision overflow".into()))?,selection.opportunity.to_string(),i64::try_from(selection.priority).map_err(|_|Error::InvalidInput("Portfolio priority overflow".into()))?,json(selection)?],
            )?;
            let mut opportunity = self.experience_opportunity(&selection.opportunity)?;
            opportunity.status = ExperienceOpportunityStatus::Selected;
            tx.execute(
                "UPDATE experience_opportunities SET status='selected',data=?2 WHERE id=?1 AND status NOT IN ('completed','invalidated')",
                params![selection.opportunity.to_string(),json(&opportunity)?],
            )?;
            event(
                &tx,
                &selection.opportunity.to_string(),
                "experience_opportunity_selected",
                serde_json::json!({"portfolio":portfolio.id,"revision":portfolio.revision}),
            )?;
        }
        for deferral in &portfolio.deferred {
            tx.execute(
                "INSERT INTO portfolio_deferrals(portfolio_id,revision,opportunity_id,data) VALUES(?1,?2,?3,?4)",
                params![portfolio.id.to_string(),i64::try_from(portfolio.revision).map_err(|_|Error::InvalidInput("Portfolio revision overflow".into()))?,deferral.opportunity.to_string(),json(deferral)?],
            )?;
            let mut opportunity = self.experience_opportunity(&deferral.opportunity)?;
            if !matches!(
                opportunity.status,
                ExperienceOpportunityStatus::Completed
                    | ExperienceOpportunityStatus::Invalidated
                    | ExperienceOpportunityStatus::Saturated
            ) {
                opportunity.status = ExperienceOpportunityStatus::Deferred;
                tx.execute(
                    "UPDATE experience_opportunities SET status='deferred',data=?2 WHERE id=?1",
                    params![deferral.opportunity.to_string(), json(&opportunity)?],
                )?;
            }
            event(
                &tx,
                &deferral.opportunity.to_string(),
                "experience_opportunity_deferred",
                serde_json::json!({"portfolio":portfolio.id,"reasons":deferral.reasons}),
            )?;
        }
        tx.execute(
            "INSERT INTO budget_ledgers(id,portfolio_id,revision,data) VALUES(?1,?2,?3,?4)",
            params![
                portfolio.ledger.id.to_string(),
                portfolio.id.to_string(),
                i64::try_from(portfolio.revision)
                    .map_err(|_| Error::InvalidInput("Portfolio revision overflow".into()))?,
                json(&portfolio.ledger)?
            ],
        )?;
        event(
            &tx,
            &portfolio.id.to_string(),
            if created {
                "experience_portfolio_created"
            } else {
                "experience_portfolio_replanned"
            },
            serde_json::json!({"revision":portfolio.revision,"selected":portfolio.selected.len(),"deferred":portfolio.deferred.len()}),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn experience_portfolio(
        &self,
        id: &crate::core::ExperiencePortfolioId,
    ) -> Result<ExperiencePortfolio> {
        self.get(
            "SELECT data FROM experience_portfolios WHERE id=?1",
            &id.to_string(),
        )
    }

    pub fn experience_portfolios(&self) -> Result<Vec<ExperiencePortfolio>> {
        self.list("SELECT data FROM experience_portfolios ORDER BY created_at,id")
    }

    pub fn latest_experience_portfolio(&self) -> Result<Option<ExperiencePortfolio>> {
        let value: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM experience_portfolios ORDER BY created_at DESC,id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        value
            .map(|value| Ok(serde_json::from_str(&value)?))
            .transpose()
    }

    pub fn experience_portfolio_history(
        &self,
        id: &crate::core::ExperiencePortfolioId,
    ) -> Result<Vec<ExperiencePortfolio>> {
        let mut statement = self.connection.prepare(
            "SELECT data FROM experience_portfolio_revisions WHERE portfolio_id=?1 ORDER BY revision",
        )?;
        statement
            .query_map([id.to_string()], |row| row.get::<_, String>(0))?
            .map(|value| Ok(serde_json::from_str(&value?)?))
            .collect()
    }

    pub fn begin_experience_portfolio(
        &self,
        id: &crate::core::ExperiencePortfolioId,
    ) -> Result<Vec<(crate::core::ExperienceOpportunityId, ExecutableLearningPlan)>> {
        let portfolio = self.experience_portfolio(id)?;
        let compiler = DeterministicExperienceOpportunityCompiler;
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let mut plans = Vec::new();
        for selection in &portfolio.selected {
            let mut opportunity = self.experience_opportunity(&selection.opportunity)?;
            if matches!(
                opportunity.status,
                ExperienceOpportunityStatus::Running | ExperienceOpportunityStatus::Completed
            ) {
                continue;
            }
            let plan = compiler.compile(&opportunity)?;
            opportunity.status = ExperienceOpportunityStatus::Running;
            tx.execute(
                "UPDATE experience_opportunities SET status='running',data=?2 WHERE id=?1 AND status IN ('eligible','selected','deferred')",
                params![opportunity.id.to_string(),json(&opportunity)?],
            )?;
            event(
                &tx,
                &opportunity.id.to_string(),
                "experience_budget_reserved",
                serde_json::json!({"portfolio":id,"cost":selection.reserved_cost}),
            )?;
            plans.push((opportunity.id, plan));
        }
        tx.commit()?;
        Ok(plans)
    }

    pub fn opportunity_result(
        &self,
        id: &crate::core::ExperienceOpportunityId,
    ) -> Result<ExperienceOpportunityResult> {
        self.get(
            "SELECT data FROM opportunity_results WHERE opportunity_id=?1",
            &id.to_string(),
        )
    }

    pub fn opportunity_results(&self) -> Result<Vec<ExperienceOpportunityResult>> {
        self.list("SELECT data FROM opportunity_results ORDER BY completed_at,opportunity_id")
    }

    pub fn complete_opportunity_and_replan(
        &self,
        portfolio_id: &crate::core::ExperiencePortfolioId,
        result: ExperienceOpportunityResult,
        additional: &[ExperienceOpportunity],
    ) -> Result<ExperiencePortfolio> {
        let mut previous = self.experience_portfolio(portfolio_id)?;
        let selection = previous
            .selected
            .iter()
            .find(|selection| selection.opportunity == result.opportunity)
            .ok_or_else(|| {
                Error::InvalidInput("Opportunity is not reserved in this portfolio".into())
            })?
            .clone();
        let opportunity = self.experience_opportunity(&result.opportunity)?;
        if opportunity.status == ExperienceOpportunityStatus::Completed {
            return Err(Error::InvalidInput(
                "Opportunity result is append-only and already recorded".into(),
            ));
        }
        let approval_reserved = usize::from(opportunity.risk.approval_required);
        previous.ledger.consume(
            &selection.reserved_cost,
            approval_reserved,
            &result.actual_cost,
        );
        for remaining in previous
            .selected
            .iter()
            .filter(|item| item.opportunity != result.opportunity)
        {
            let item = self.experience_opportunity(&remaining.opportunity)?;
            previous.ledger.release(
                &remaining.reserved_cost,
                usize::from(item.risk.approval_required),
            );
        }

        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO opportunity_results(opportunity_id,outcome,completed_at,data) VALUES(?1,?2,?3,?4)",
            params![result.opportunity.to_string(),name(&result.outcome)?,result.completed_at.to_rfc3339(),json(&result)?],
        )?;
        tx.execute(
            "INSERT INTO experiment_actual_costs(opportunity_id,data) VALUES(?1,?2)",
            params![result.opportunity.to_string(), json(&result.actual_cost)?],
        )?;
        let mut completed = opportunity;
        completed.status = ExperienceOpportunityStatus::Completed;
        tx.execute(
            "UPDATE experience_opportunities SET status='completed',data=?2 WHERE id=?1",
            params![result.opportunity.to_string(), json(&completed)?],
        )?;
        event(
            &tx,
            &result.opportunity.to_string(),
            "experience_opportunity_completed",
            serde_json::json!({"outcome":result.outcome,"actual_cost":result.actual_cost}),
        )?;
        tx.commit()?;

        for item in additional {
            self.save_experience_opportunity(item)?;
        }
        let mut candidates = previous
            .opportunities
            .iter()
            .filter(|id| **id != result.opportunity)
            .filter_map(|id| self.experience_opportunity(id).ok())
            .filter(|item| item.status != ExperienceOpportunityStatus::Completed)
            .collect::<Vec<_>>();
        candidates.extend_from_slice(additional);
        candidates.sort_by_key(|item| item.id.clone());
        candidates.dedup_by(|left, right| left.id == right.id);
        let context = ExperiencePlanningContext {
            gaps: Vec::new(),
            completed_dependencies: self
                .opportunity_results()?
                .into_iter()
                .filter(|item| {
                    matches!(
                        item.outcome,
                        OpportunityOutcome::MaterialLearning
                            | OpportunityOutcome::StrengthenedExistingEvidence
                            | OpportunityOutcome::NoMaterialChange
                    )
                })
                .map(|item| item.opportunity)
                .collect(),
            objective: previous.objective,
            now: Utc::now(),
        };
        let reason = if result.outcome == OpportunityOutcome::ContradictedExistingEvidence {
            PortfolioRevisionReason::Contradiction
        } else {
            PortfolioRevisionReason::OpportunityCompleted
        };
        let revised = DeterministicExperienceAllocationPolicy.replan(
            &previous,
            &candidates,
            &context,
            reason,
        )?;
        self.persist_experience_portfolio(&revised, false)?;
        Ok(revised)
    }

    pub fn replay_experience_portfolio(
        &self,
        id: &crate::core::ExperiencePortfolioId,
    ) -> Result<ExperiencePortfolio> {
        let portfolio = self.experience_portfolio(id)?;
        let opportunities = portfolio
            .opportunities
            .iter()
            .map(|id| self.experience_opportunity(id))
            .collect::<Result<Vec<_>>>()?;
        let context = ExperiencePlanningContext {
            gaps: Vec::new(),
            completed_dependencies: self
                .opportunity_results()?
                .into_iter()
                .filter(|item| {
                    matches!(
                        item.outcome,
                        OpportunityOutcome::MaterialLearning
                            | OpportunityOutcome::StrengthenedExistingEvidence
                            | OpportunityOutcome::NoMaterialChange
                    )
                })
                .map(|item| item.opportunity)
                .collect(),
            objective: portfolio.objective,
            now: portfolio.created_at,
        };
        DeterministicExperienceAllocationPolicy.allocate(
            &opportunities,
            &portfolio.budget,
            &context,
        )
    }

    pub fn experience_debt(&self) -> Result<Vec<ExperienceDebtItem>> {
        let now = Utc::now();
        let mut debt = self
            .experience_opportunities()?
            .into_iter()
            .filter(|item| {
                !matches!(
                    item.status,
                    ExperienceOpportunityStatus::Completed
                        | ExperienceOpportunityStatus::Invalidated
                        | ExperienceOpportunityStatus::Saturated
                ) && item.risk_reduction.failure_severity >= Severity::Medium
                    && !matches!(
                        item.risk_reduction.occurrence_exposure,
                        ExposureBand::Rare | ExposureBand::Unknown
                    )
                    && matches!(
                        item.risk_reduction.mitigation_gap,
                        MitigationGap::Partial
                            | MitigationGap::Significant
                            | MitigationGap::Unmitigated
                            | MitigationGap::Unknown
                    )
            })
            .map(|item| ExperienceDebtItem {
                target: item.target,
                severity: item.risk_reduction.failure_severity,
                exposure: item.risk_reduction.occurrence_exposure,
                age: now
                    .signed_duration_since(item.created_at)
                    .to_std()
                    .unwrap_or(Duration::ZERO),
                reason: item
                    .rationale
                    .into_iter()
                    .next()
                    .unwrap_or(OpportunityReason::Custom("known evidence gap".into())),
            })
            .collect::<Vec<_>>();
        debt.sort_by_key(|item| {
            (
                std::cmp::Reverse(item.severity),
                format!("{:?}", item.target),
            )
        });
        Ok(debt)
    }

    pub fn experience_economics_report(&self) -> Result<serde_json::Value> {
        let portfolios = self.experience_portfolios()?;
        let mut composition = std::collections::BTreeMap::<String, usize>::new();
        if let Some(portfolio) = portfolios.last() {
            for selection in &portfolio.selected {
                *composition.entry(name(&selection.category)?).or_default() +=
                    selection.reserved_cost.trials;
            }
        }
        let results = self.opportunity_results()?;
        let completed = results.len();
        let material = results
            .iter()
            .filter(|result| {
                matches!(
                    result.outcome,
                    OpportunityOutcome::MaterialLearning
                        | OpportunityOutcome::StrengthenedExistingEvidence
                        | OpportunityOutcome::ContradictedExistingEvidence
                )
            })
            .count();
        let no_change = results
            .iter()
            .filter(|result| result.outcome == OpportunityOutcome::NoMaterialChange)
            .count();
        let trials = results
            .iter()
            .map(|result| result.actual_cost.trials)
            .sum::<usize>();
        let agent_runs = results
            .iter()
            .map(|result| result.actual_cost.agent_runs)
            .sum::<usize>();
        let estimated_trials = results
            .iter()
            .filter_map(|result| self.experience_opportunity(&result.opportunity).ok())
            .map(|item| item.estimated_cost.trials)
            .sum::<usize>();
        let yield_summary = self.experience_learning_yield()?;
        let cost_by_opportunity = results
            .iter()
            .filter_map(|result| {
                self.experience_opportunity(&result.opportunity)
                    .ok()
                    .map(|opportunity| {
                        serde_json::json!({
                            "opportunity":result.opportunity,
                            "target":opportunity.target,
                            "estimated":opportunity.estimated_cost,
                            "actual":result.actual_cost,
                            "outcome":result.outcome,
                        })
                    })
            })
            .collect::<Vec<_>>();
        let saturated_evidence_spend = results
            .iter()
            .filter_map(|result| {
                self.experience_opportunity(&result.opportunity)
                    .ok()
                    .filter(|item| item.saturation == EvidenceSaturation::Saturated)
                    .map(|_| result.actual_cost.trials)
            })
            .sum::<usize>();
        Ok(serde_json::json!({
            "portfolios":portfolios.len(),
            "latest_portfolio_composition_trials":composition,
            "completed_opportunities":completed,
            "material_learning":{"count":material,"rate":(completed>0).then_some(material as f64/completed as f64),"sample_count":completed},
            "no_material_change":no_change,
            "actual_cost":{"trials":trials,"agent_runs":agent_runs},
            "estimated_cost":{"trials":estimated_trials},
            "estimate_delta_trials":i128::try_from(trials).unwrap_or(i128::MAX)-i128::try_from(estimated_trials).unwrap_or(i128::MAX),
            "learning_yield":yield_summary,
            "cost_by_opportunity":cost_by_opportunity,
            "saturated_evidence_spend":saturated_evidence_spend,
            "wasted_trial_rate":{"value":null,"reason":"Equivalent duplicate purpose is not recorded per trial; inconclusive work is not classified as wasted"},
            "experience_debt":self.experience_debt()?,
            "principle":"Unused experiment budget is better than low-value experimentation"
        }))
    }

    pub fn experience_learning_yield(&self) -> Result<LearningYield> {
        let results = self.opportunity_results()?;
        let mut summary = LearningYield::default();
        for result in results {
            for outcome in &result.learning_outcomes {
                summary.material_updates = summary
                    .material_updates
                    .saturating_add(outcome.lessons_created.len())
                    .saturating_add(outcome.lessons_updated.len())
                    .saturating_add(outcome.reflexes_created.len())
                    .saturating_add(outcome.recoveries_created.len())
                    .saturating_add(outcome.envelope_updates.len());
                summary.failures_mitigated = summary
                    .failures_mitigated
                    .saturating_add(outcome.recoveries_created.len());
            }
            if matches!(
                result.outcome,
                OpportunityOutcome::MaterialLearning
                    | OpportunityOutcome::StrengthenedExistingEvidence
                    | OpportunityOutcome::ContradictedExistingEvidence
            ) {
                summary.gaps_closed = summary.gaps_closed.saturating_add(1);
            }
            if result.outcome == OpportunityOutcome::ContradictedExistingEvidence {
                summary.contradictions_resolved = summary.contradictions_resolved.saturating_add(1);
            }
        }
        Ok(summary)
    }

    pub fn experience_acquisition_summary(&self) -> Result<ExperienceAcquisitionSummary> {
        let opportunities = self.experience_opportunities()?;
        let open = opportunities
            .iter()
            .filter(|item| {
                !matches!(
                    item.status,
                    ExperienceOpportunityStatus::Completed
                        | ExperienceOpportunityStatus::Invalidated
                        | ExperienceOpportunityStatus::Saturated
                )
            })
            .collect::<Vec<_>>();
        let results = self.opportunity_results()?;
        let trials_consumed = results
            .iter()
            .map(|result| result.actual_cost.trials)
            .sum::<usize>();
        let material_outcomes = results
            .iter()
            .filter(|result| {
                matches!(
                    result.outcome,
                    OpportunityOutcome::MaterialLearning
                        | OpportunityOutcome::ContradictedExistingEvidence
                        | OpportunityOutcome::StrengthenedExistingEvidence
                )
            })
            .count();
        let critical_gaps_closed = results
            .iter()
            .filter(|result| {
                self.experience_opportunity(&result.opportunity)
                    .is_ok_and(|item| {
                        item.risk_reduction.failure_severity == Severity::Critical
                            && !matches!(
                                result.outcome,
                                OpportunityOutcome::FailedToExecute
                                    | OpportunityOutcome::Cancelled
                                    | OpportunityOutcome::Inconclusive
                            )
                    })
            })
            .count();
        let early_stop_trial_savings = results
            .iter()
            .filter_map(|result| {
                self.experience_opportunity(&result.opportunity)
                    .ok()
                    .map(|item| {
                        item.estimated_cost
                            .trials
                            .saturating_sub(result.actual_cost.trials)
                    })
            })
            .sum();
        Ok(ExperienceAcquisitionSummary {
            open_opportunities: open.len(),
            critical_opportunities: open
                .iter()
                .filter(|item| item.risk_reduction.failure_severity == Severity::Critical)
                .count(),
            high_opportunities: open
                .iter()
                .filter(|item| item.risk_reduction.failure_severity == Severity::High)
                .count(),
            last_portfolio: self.latest_experience_portfolio()?.map(|item| item.id),
            trials_consumed,
            material_outcomes,
            critical_gaps_closed,
            early_stop_trial_savings,
        })
    }

    pub fn experience_economics_events(&self) -> Result<Vec<serde_json::Value>> {
        let mut statement = self.connection.prepare(
            "SELECT json_object('sequence',sequence,'subject',subject,'kind',kind,'created_at',created_at,'data',json(data)) FROM experience_economics_events ORDER BY sequence",
        )?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .map(|value| Ok(serde_json::from_str(&value?)?))
            .collect()
    }
}
