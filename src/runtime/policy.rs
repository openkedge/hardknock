// SPDX-License-Identifier: Apache-2.0

use crate::{
    Error, Result,
    assurance::AssuranceGapKind,
    curriculum::Severity,
    effects::{EffectKind, EffectOperation, EffectRisk, ExternalityClass, ReversibilityClass},
    lesson::ActionPattern,
};

use super::*;

pub trait RuntimeController {
    fn decide(&self, context: &RuntimeDecisionContext) -> Result<RuntimeDecision>;
}

pub trait RuntimeDecisionPolicy {
    fn evaluate(&self, context: &RuntimeDecisionContext) -> Result<RuntimeDecisionEvaluation>;
}

pub trait RuntimeRiskPolicy {
    fn assess(&self, context: &RuntimeDecisionContext) -> RuntimeRiskAssessment;
}

pub trait KnowledgeClassifier {
    fn classify(&self, context: &RuntimeDecisionContext) -> KnowledgeState;
}

pub fn expected_learning_value(context: &RuntimeDecisionContext) -> ExpectedLearningValue {
    let reusable = context.task.family.is_some();
    let material_gap = !context.assurance.gaps.is_empty()
        || context.knowledge_signals.local_contradicted
        || context.knowledge_signals.evidence_stale
        || !context.knowledge_signals.local_supported;
    let expensive = context
        .available_experiments
        .budget
        .max_duration_ms
        .is_some_and(|duration| duration > 300_000)
        || context.available_experiments.budget.max_realities > 3;
    match (
        context.uncertainty.level,
        context.risk.severity >= Severity::Medium,
        reusable,
        material_gap,
        expensive,
    ) {
        (UncertaintyLevel::High | UncertaintyLevel::Unknown, true, true, true, false) => {
            ExpectedLearningValue::High
        }
        (_, _, _, false, _) | (UncertaintyLevel::Low, false, false, _, _) => {
            ExpectedLearningValue::Low
        }
        _ => ExpectedLearningValue::Medium,
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicKnowledgeClassifier;

impl KnowledgeClassifier for DeterministicKnowledgeClassifier {
    fn classify(&self, context: &RuntimeDecisionContext) -> KnowledgeState {
        let signals = &context.knowledge_signals;
        if !signals.context_in_scope {
            KnowledgeState::OutOfScope
        } else if signals.local_supported && signals.local_contradicted {
            KnowledgeState::KnownContradicted
        } else if signals.local_supported && signals.evidence_stale {
            KnowledgeState::KnownStale
        } else if (signals.local_supported || signals.validated_skill || signals.applicable_lesson)
            && !signals.remote_advisory_only
            && !signals.known_gap_matches
        {
            KnowledgeState::KnownSupported
        } else {
            KnowledgeState::Unknown
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicRuntimeRiskPolicy;

impl RuntimeRiskPolicy for DeterministicRuntimeRiskPolicy {
    fn assess(&self, context: &RuntimeDecisionContext) -> RuntimeRiskAssessment {
        let Some(effect) = &context.proposed_effect else {
            return context.risk.clone();
        };
        let (externality, reversibility) = match effect.kind {
            EffectKind::Message => (
                ExternalityClass::HumanVisible,
                ReversibilityClass::Irreversible,
            ),
            EffectKind::Deployment | EffectKind::CloudResource | EffectKind::HttpApi => (
                ExternalityClass::ExternalSystem,
                ReversibilityClass::Compensatable,
            ),
            EffectKind::Database => (
                ExternalityClass::ExternalSystem,
                ReversibilityClass::Compensatable,
            ),
            EffectKind::Filesystem | EffectKind::Process => (
                ExternalityClass::HostLocal,
                ReversibilityClass::NaturallyReversible,
            ),
            EffectKind::Custom => (ExternalityClass::Unknown, ReversibilityClass::Unknown),
        };
        let effect_risk = match effect.operation {
            EffectOperation::Read => EffectRisk::ReadOnly,
            EffectOperation::Create | EffectOperation::Update | EffectOperation::Post => {
                EffectRisk::Medium
            }
            EffectOperation::Delete | EffectOperation::Dispatch | EffectOperation::Promote => {
                EffectRisk::High
            }
            EffectOperation::Custom(_) => EffectRisk::Critical,
        };
        let severity = match effect_risk {
            EffectRisk::ReadOnly => Severity::Low,
            EffectRisk::Low => Severity::Low,
            EffectRisk::Medium => Severity::Medium,
            EffectRisk::High => Severity::High,
            EffectRisk::Critical => Severity::Critical,
        };
        RuntimeRiskAssessment {
            severity,
            reversibility,
            externality,
            assurance_requirement: context.risk.assurance_requirement.clone(),
            effect_risk: Some(effect_risk),
            rationale: vec![format!(
                "Structured {:?} {:?} effect targets {}",
                effect.kind, effect.operation, effect.target.uri
            )],
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct DeterministicRuntimeController {
    pub policy: DeterministicRuntimeDecisionPolicy,
}

impl DeterministicRuntimeController {
    pub fn with_config(mut config: RuntimePolicyConfig) -> Result<Self> {
        config.refresh_version();
        config.validate()?;
        Ok(Self {
            policy: DeterministicRuntimeDecisionPolicy { config },
        })
    }

    pub fn evaluate(&self, context: &RuntimeDecisionContext) -> Result<RuntimeDecisionEvaluation> {
        self.policy.evaluate(context)
    }
}

impl RuntimeController for DeterministicRuntimeController {
    fn decide(&self, context: &RuntimeDecisionContext) -> Result<RuntimeDecision> {
        Ok(self.evaluate(context)?.decision)
    }
}

#[derive(Clone, Debug, Default)]
pub struct DeterministicRuntimeDecisionPolicy {
    pub config: RuntimePolicyConfig,
}

impl RuntimePolicyConfig {
    pub fn refresh_version(&mut self) {
        let material = format!(
            "{:?}|{:?}|{:?}|{}|{}|{}|{}",
            self.profile,
            self.autonomy,
            self.experiment_mode,
            self.external_experience.advisory_can_warn,
            self.external_experience.advisory_can_trigger_experiment,
            self.external_experience.advisory_can_trigger_replan,
            self.external_experience.advisory_can_authorize_act,
        );
        self.version = format!(
            "{}:{}",
            super::RUNTIME_POLICY_VERSION,
            &blake3::hash(material.as_bytes()).to_hex()[..16]
        );
    }

    pub fn validate(&self) -> Result<()> {
        if self.version.trim().is_empty() || self.version.len() > 128 {
            return Err(Error::InvalidInput(
                "Runtime policy version must be bounded and nonempty".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatrixDisposition {
    Act,
    ActWithWarning,
    Experiment,
    ApprovalOrExperiment,
    Abstain,
}

fn matrix(
    profile: RuntimePolicyProfile,
    knowledge: KnowledgeState,
    severity: Severity,
) -> MatrixDisposition {
    use KnowledgeState::*;
    use MatrixDisposition::*;
    use RuntimePolicyProfile::*;
    use Severity::*;
    match (profile, knowledge, severity) {
        (_, KnownSupported, Informational | Low | Medium) => Act,
        (_, KnownSupported, High | Critical) => ApprovalOrExperiment,

        (Developer, Unknown | KnownStale | OutOfScope, Informational | Low | Medium) => {
            ActWithWarning
        }
        (Developer, Unknown | KnownStale | OutOfScope, High) => Experiment,
        (Developer, Unknown | KnownStale | OutOfScope, Critical) => ApprovalOrExperiment,

        (Balanced, Unknown, Informational | Low) => ActWithWarning,
        (Balanced, Unknown | KnownStale | OutOfScope, Medium) => Experiment,
        (Balanced, Unknown | KnownStale | OutOfScope, High | Critical) => ApprovalOrExperiment,
        (Balanced, KnownStale | OutOfScope, Informational | Low) => ActWithWarning,

        (Conservative, Unknown | KnownStale | OutOfScope, Informational | Low) => ActWithWarning,
        (Conservative, Unknown | KnownStale | OutOfScope, Medium) => Experiment,
        (Conservative, Unknown | KnownStale | OutOfScope, High | Critical) => Abstain,

        (_, KnownContradicted, Informational | Low | Medium) => Experiment,
        (_, KnownContradicted, High | Critical) => Abstain,
    }
}

fn lesson_refs(context: &RuntimeDecisionContext) -> Vec<LessonRef> {
    context
        .relevant_experience
        .lessons
        .iter()
        .map(|item| LessonRef {
            id: item.lesson.id.clone(),
            version: item.lesson.version,
        })
        .collect()
}

fn evidence(context: &RuntimeDecisionContext) -> Vec<EvidenceRef> {
    let mut evidence = Vec::new();
    evidence.extend(
        context
            .applicable_skills
            .iter()
            .cloned()
            .map(EvidenceRef::Skill),
    );
    evidence.extend(lesson_refs(context).into_iter().map(EvidenceRef::Lesson));
    evidence.extend(
        context
            .matched_reflexes
            .iter()
            .cloned()
            .map(EvidenceRef::Reflex),
    );
    evidence.extend(
        context
            .advisory_reflexes
            .iter()
            .cloned()
            .map(EvidenceRef::Reflex),
    );
    if let Some(certification) = &context.assurance.summary.certification {
        evidence.push(EvidenceRef::Certification(certification.clone()));
    }
    if let Some(envelope) = &context.operating_envelope {
        evidence.push(EvidenceRef::OperatingEnvelope {
            id: envelope.id.clone(),
            version: envelope.version,
        });
    }
    if context.externally_supported {
        evidence.push(EvidenceRef::ExternalAdvisory(
            "compatible remote evidence remains locally unvalidated".into(),
        ));
    }
    evidence
}

fn alternatives() -> Vec<RuntimeAlternative> {
    vec![
        RuntimeAlternative {
            name: "inspect".into(),
            description: "Inspect the evidence, risk dimensions, and requested authority".into(),
        },
        RuntimeAlternative {
            name: "experiment".into(),
            description: "Use a disposable Reality if the effect can be virtualized safely".into(),
        },
        RuntimeAlternative {
            name: "narrow".into(),
            description: "Choose a reversible action with less authority".into(),
        },
    ]
}

fn experiment(context: &RuntimeDecisionContext, reason: impl Into<String>) -> RuntimeDecision {
    let mut budget = context.available_experiments.budget.clone();
    let desired = match (
        context.uncertainty.level,
        context.risk.severity,
        context.task.family.is_some(),
    ) {
        (UncertaintyLevel::High | UncertaintyLevel::Unknown, Severity::High, true) => 3,
        (UncertaintyLevel::Low, _, _) => 1,
        _ => 2,
    };
    budget.max_realities = budget.max_realities.min(desired);
    let reason = reason.into();
    RuntimeDecision::Experiment(ExperimentDecision {
        question: format!("Which bounded strategy best resolves: {reason}?"),
        reason,
        candidates: context
            .uncertainty
            .candidate_strategies
            .iter()
            .take(3)
            .cloned()
            .collect(),
        budget,
        requirements: context.available_experiments.requirements.clone(),
        automatic: context.available_experiments.mode == ExperimentMode::Automatic,
    })
}

fn abstain(
    context: &RuntimeDecisionContext,
    reason: AbstentionReason,
    blockers: Vec<DecisionBlocker>,
) -> RuntimeDecision {
    RuntimeDecision::Abstain(AbstentionDecision {
        reason,
        missing_evidence: context.assurance.gaps.clone(),
        unresolved_risks: blockers,
        possible_next_steps: alternatives(),
    })
}

fn approval(context: &RuntimeDecisionContext, reason: impl Into<String>) -> RuntimeDecision {
    RuntimeDecision::RequireApproval(ApprovalDecision {
        reason: reason.into(),
        requested_authority: if context.proposed_effect.is_some() {
            RequestedAuthority::CommitEffect
        } else {
            RequestedAuthority::UserApproval
        },
        evidence_summary: format!(
            "knowledge={:?}; assurance={:?}; local evidence items={}",
            DeterministicKnowledgeClassifier.classify(context),
            context.assurance.summary.status,
            context.relevant_experience.lessons.len() + context.applicable_skills.len()
        ),
        risk: context.risk.clone(),
        alternatives: alternatives(),
    })
}

fn act(context: &RuntimeDecisionContext, warning: Option<String>) -> RuntimeDecision {
    let refs = evidence(context);
    let recommended_tool = context
        .tool_candidates
        .iter()
        .filter(|candidate| candidate.satisfies_task)
        .min_by_key(|candidate| {
            (
                !candidate.current_assurance,
                candidate.capability_width,
                &candidate.name,
            )
        })
        .map(|candidate| candidate.name.clone());
    RuntimeDecision::Act(ActDecision {
        recommended_action: context.proposed_action.clone(),
        applicable_skills: context.applicable_skills.clone(),
        relevant_lessons: lesson_refs(context),
        assurance: context.assurance.summary.clone(),
        evidence: refs,
        recommended_tool,
        warning,
    })
}

impl RuntimeDecisionPolicy for DeterministicRuntimeDecisionPolicy {
    fn evaluate(&self, context: &RuntimeDecisionContext) -> Result<RuntimeDecisionEvaluation> {
        self.config.validate()?;
        let knowledge = DeterministicKnowledgeClassifier.classify(context);
        let mut reasons = Vec::new();
        let mut blockers = Vec::new();
        let collected_evidence = evidence(context);
        let governance = &context.capability_context.governance;

        if governance.hard_policy_blocked {
            let reason = governance
                .block_reason
                .clone()
                .unwrap_or_else(|| "External hard policy prohibits the action".into());
            reasons.push(DecisionReason::HardPolicyPrecedence);
            blockers.push(DecisionBlocker::HardPolicyProhibition(reason.clone()));
            let decision = abstain(
                context,
                AbstentionReason::ExternalPolicyProhibition,
                blockers.clone(),
            );
            return Ok(self.finish(
                decision,
                knowledge,
                reasons,
                collected_evidence,
                blockers,
                GovernanceDisposition::SecurityBlocked,
            ));
        }

        if !context.capability_context.required_available
            || !context.capability_context.missing.is_empty()
        {
            reasons.push(DecisionReason::CapabilityUnavailable);
            blockers.extend(
                context.capability_context.missing.iter().map(|capability| {
                    DecisionBlocker::MissingCapability(format!("{capability:?}"))
                }),
            );
            let decision = abstain(
                context,
                AbstentionReason::InsufficientIsolation,
                blockers.clone(),
            );
            return Ok(self.finish(
                decision,
                knowledge,
                reasons,
                collected_evidence,
                blockers,
                GovernanceDisposition::SecurityBlocked,
            ));
        }

        if !context.capability_context.isolation_sufficient
            && context.risk.severity >= Severity::High
        {
            reasons.push(DecisionReason::CapabilityUnavailable);
            blockers.push(DecisionBlocker::InsufficientIsolation);
            let decision = abstain(
                context,
                AbstentionReason::InsufficientIsolation,
                blockers.clone(),
            );
            return Ok(self.finish(
                decision,
                knowledge,
                reasons,
                collected_evidence,
                blockers,
                GovernanceDisposition::SecurityBlocked,
            ));
        }

        if context.proposed_effect.is_some() && !context.capability_context.effect_adapter_available
        {
            reasons.push(DecisionReason::CapabilityUnavailable);
            blockers.push(DecisionBlocker::UnsupportedEffectAdapter);
            if context.risk.severity >= Severity::High {
                let decision = abstain(
                    context,
                    AbstentionReason::UnsupportedEffect,
                    blockers.clone(),
                );
                return Ok(self.finish(
                    decision,
                    knowledge,
                    reasons,
                    collected_evidence,
                    blockers,
                    GovernanceDisposition::SecurityBlocked,
                ));
            }
        }

        if let Some(signature) = &context.failure_signature
            && let Some(recovery) = context.available_recovery.iter().find(|recovery| {
                recovery.failure_signature == signature.signature && recovery.scope_matches
            })
        {
            reasons.push(DecisionReason::RecoveryAvailable);
            if recovery.fresh {
                let decision = RuntimeDecision::Recover(RecoverDecision {
                    recovery: recovery.clone(),
                    failure_signature: signature.clone(),
                    confidence: recovery.confidence,
                    evidence: vec![EvidenceRef::Recovery {
                        id: recovery.id.clone(),
                        version: recovery.version,
                    }],
                });
                return Ok(self.finish(
                    decision,
                    knowledge,
                    reasons,
                    collected_evidence,
                    blockers,
                    GovernanceDisposition::RuntimeRecommendation,
                ));
            }
            reasons.push(DecisionReason::EvidenceStale);
            if context.available_experiments.can_experiment() {
                reasons.push(DecisionReason::SafeExperimentAvailable);
                let decision = experiment(context, "the matching Recovery evidence is stale");
                return Ok(self.finish(
                    decision,
                    KnowledgeState::KnownStale,
                    reasons,
                    collected_evidence,
                    blockers,
                    GovernanceDisposition::RuntimeRecommendation,
                ));
            }
        }

        if !context.matched_reflexes.is_empty() {
            reasons.push(DecisionReason::ReflexMatched);
            let decision = RuntimeDecision::Replan(ReplanDecision {
                reason: "A supported or active failure precursor matches the proposed action"
                    .into(),
                matched_reflexes: context.matched_reflexes.clone(),
                relevant_lessons: lesson_refs(context),
                excluded_actions: proposed_action_patterns(context),
            });
            return Ok(self.finish(
                decision,
                knowledge,
                reasons,
                collected_evidence,
                blockers,
                GovernanceDisposition::RuntimeRecommendation,
            ));
        }

        if context.knowledge_signals.known_failure_precursor {
            reasons.push(DecisionReason::RelevantLessonMatched);
            let decision = RuntimeDecision::Replan(ReplanDecision {
                reason: "A current, locally supported Lesson identifies the proposed action as a failure precursor"
                    .into(),
                matched_reflexes: Vec::new(),
                relevant_lessons: lesson_refs(context),
                excluded_actions: proposed_action_patterns(context),
            });
            return Ok(self.finish(
                decision,
                knowledge,
                reasons,
                collected_evidence,
                blockers,
                GovernanceDisposition::RuntimeRecommendation,
            ));
        }

        match context
            .operating_envelope
            .as_ref()
            .map(|envelope| envelope.position)
            .unwrap_or(EnvelopePosition::Unknown)
        {
            EnvelopePosition::KnownSafe => reasons.push(DecisionReason::InsideOperatingEnvelope),
            EnvelopePosition::KnownDegraded => {
                reasons.push(DecisionReason::OutsideOperatingEnvelope);
                if context.risk.severity >= Severity::Medium {
                    let decision = RuntimeDecision::Replan(ReplanDecision {
                        reason: "Current conditions are in a tested degraded region".into(),
                        matched_reflexes: Vec::new(),
                        relevant_lessons: lesson_refs(context),
                        excluded_actions: proposed_action_patterns(context),
                    });
                    return Ok(self.finish(
                        decision,
                        knowledge,
                        reasons,
                        collected_evidence,
                        blockers,
                        GovernanceDisposition::RuntimeRecommendation,
                    ));
                }
            }
            EnvelopePosition::KnownFailure => {
                reasons.push(DecisionReason::OutsideOperatingEnvelope);
                blockers.push(DecisionBlocker::CriticalAssuranceGap(
                    "Current conditions match a tested failure region".into(),
                ));
                if context.risk.severity >= Severity::High {
                    let decision = abstain(
                        context,
                        AbstentionReason::NoValidatedRecovery,
                        blockers.clone(),
                    );
                    return Ok(self.finish(
                        decision,
                        knowledge,
                        reasons,
                        collected_evidence,
                        blockers,
                        GovernanceDisposition::RuntimeRecommendation,
                    ));
                }
                let decision = RuntimeDecision::Replan(ReplanDecision {
                    reason: "Do not continue into a tested failure region".into(),
                    matched_reflexes: Vec::new(),
                    relevant_lessons: lesson_refs(context),
                    excluded_actions: proposed_action_patterns(context),
                });
                return Ok(self.finish(
                    decision,
                    knowledge,
                    reasons,
                    collected_evidence,
                    blockers,
                    GovernanceDisposition::RuntimeRecommendation,
                ));
            }
            EnvelopePosition::Unknown => reasons.push(DecisionReason::UnknownOperatingRegion),
        }

        match knowledge {
            KnowledgeState::KnownSupported => {
                reasons.push(DecisionReason::ValidatedSkillApplicable)
            }
            KnowledgeState::KnownContradicted => {
                reasons.push(DecisionReason::EvidenceContradicted);
                blockers.push(DecisionBlocker::UnresolvedContradiction);
            }
            KnowledgeState::KnownStale => reasons.push(DecisionReason::EvidenceStale),
            KnowledgeState::Unknown => reasons.push(DecisionReason::EvidenceInsufficient),
            KnowledgeState::OutOfScope => reasons.push(DecisionReason::OutsideOperatingEnvelope),
        }
        if context.externally_supported || context.knowledge_signals.remote_advisory_only {
            reasons.push(DecisionReason::ExternalEvidenceAdvisoryOnly);
        }
        match context.assurance.summary.status {
            AssuranceRuntimeStatus::Current => reasons.push(DecisionReason::AssuranceCurrent),
            AssuranceRuntimeStatus::Expired
            | AssuranceRuntimeStatus::Invalidated
            | AssuranceRuntimeStatus::ReviewRecommended => {
                reasons.push(DecisionReason::AssuranceExpired)
            }
            _ => {}
        }
        if context.risk.severity >= Severity::High {
            reasons.push(DecisionReason::HighRiskEffect);
        }

        let experiment_available = context.available_experiments.can_experiment();
        if experiment_available {
            reasons.push(DecisionReason::SafeExperimentAvailable);
        } else if !context.available_experiments.budget_remaining {
            reasons.push(DecisionReason::ExperimentBudgetUnavailable);
            blockers.push(DecisionBlocker::ExhaustedBudget);
        } else if !context.available_experiments.effect_safe {
            blockers.push(DecisionBlocker::UnsafeExperiment);
        }

        let assurance_sufficient = matches!(
            context.assurance.summary.status,
            AssuranceRuntimeStatus::Current
        ) && context.assurance.applicability.applicable;
        let high_risk = context.risk.severity >= Severity::High;
        let no_commit_authority =
            context.proposed_effect.is_some() && !context.capability_context.commit_authority;

        let mut disposition = matrix(self.config.profile, knowledge, context.risk.severity);
        if context
            .operating_envelope
            .as_ref()
            .is_some_and(|envelope| envelope.position == EnvelopePosition::Unknown)
            && context.risk.severity >= Severity::Medium
            && knowledge != KnowledgeState::KnownSupported
        {
            disposition = MatrixDisposition::Experiment;
        }

        let (decision, disposition_source) = match disposition {
            MatrixDisposition::Act if high_risk && !assurance_sufficient => {
                blockers.extend(critical_assurance_blockers(context));
                if experiment_available {
                    (
                        experiment(context, "high-risk action lacks current applicable assurance"),
                        GovernanceDisposition::RuntimeRecommendation,
                    )
                } else {
                    (
                        abstain(
                            context,
                            AbstentionReason::InconclusiveAssurance,
                            blockers.clone(),
                        ),
                        GovernanceDisposition::RuntimeRecommendation,
                    )
                }
            }
            MatrixDisposition::Act if no_commit_authority || governance.approval_required => {
                reasons.push(DecisionReason::CommitAuthorityRequired);
                blockers.push(DecisionBlocker::MissingCommitAuthority);
                (
                    approval(
                        context,
                        governance.approval_reason.clone().unwrap_or_else(|| {
                            "Empirical support is sufficient to prepare, but commit authority is external"
                                .into()
                        }),
                    ),
                    GovernanceDisposition::ApprovalOverride,
                )
            }
            MatrixDisposition::Act => (
                act(context, None),
                GovernanceDisposition::RuntimeRecommendation,
            ),
            MatrixDisposition::ActWithWarning => (
                act(
                    context,
                    Some(
                        "Proceed only as a low-risk reversible action; applicable evidence is incomplete"
                            .into(),
                    ),
                ),
                GovernanceDisposition::RuntimeRecommendation,
            ),
            MatrixDisposition::Experiment if experiment_available => (
                experiment(context, knowledge_reason(knowledge)),
                GovernanceDisposition::RuntimeRecommendation,
            ),
            MatrixDisposition::Experiment => (
                abstain(
                    context,
                    if !context.available_experiments.budget_remaining {
                        AbstentionReason::BudgetExhausted
                    } else {
                        AbstentionReason::UnsafeToExperiment
                    },
                    blockers.clone(),
                ),
                GovernanceDisposition::RuntimeRecommendation,
            ),
            MatrixDisposition::ApprovalOrExperiment
                if knowledge != KnowledgeState::KnownSupported && experiment_available =>
            {
                (
                    experiment(context, knowledge_reason(knowledge)),
                    GovernanceDisposition::RuntimeRecommendation,
                )
            }
            MatrixDisposition::ApprovalOrExperiment
                if assurance_sufficient
                    && context.capability_context.effect_adapter_available
                    && (no_commit_authority || governance.approval_required) =>
            {
                reasons.push(DecisionReason::CommitAuthorityRequired);
                blockers.push(DecisionBlocker::MissingCommitAuthority);
                (
                    approval(
                        context,
                        "Strong applicable evidence supports preparation; external authority must approve commit",
                    ),
                    GovernanceDisposition::ApprovalOverride,
                )
            }
            MatrixDisposition::ApprovalOrExperiment if assurance_sufficient && !no_commit_authority => {
                (
                    act(context, None),
                    GovernanceDisposition::RuntimeRecommendation,
                )
            }
            MatrixDisposition::ApprovalOrExperiment => {
                blockers.extend(critical_assurance_blockers(context));
                (
                    abstain(
                        context,
                        if knowledge == KnowledgeState::KnownContradicted {
                            AbstentionReason::UnresolvedContradiction
                        } else {
                            AbstentionReason::CriticalUnknown
                        },
                        blockers.clone(),
                    ),
                    GovernanceDisposition::RuntimeRecommendation,
                )
            }
            MatrixDisposition::Abstain => {
                if assurance_sufficient
                    && context.capability_context.effect_adapter_available
                    && no_commit_authority
                    && knowledge != KnowledgeState::KnownContradicted
                {
                    reasons.push(DecisionReason::CommitAuthorityRequired);
                    blockers.push(DecisionBlocker::MissingCommitAuthority);
                    (
                        approval(
                            context,
                            "The action is prepared and supported, but commit authority remains external",
                        ),
                        GovernanceDisposition::ApprovalOverride,
                    )
                } else {
                    (
                        abstain(
                            context,
                            if knowledge == KnowledgeState::KnownContradicted {
                                AbstentionReason::UnresolvedContradiction
                            } else {
                                AbstentionReason::CriticalUnknown
                            },
                            blockers.clone(),
                        ),
                        GovernanceDisposition::RuntimeRecommendation,
                    )
                }
            }
        };

        Ok(self.finish(
            decision,
            knowledge,
            reasons,
            collected_evidence,
            blockers,
            disposition_source,
        ))
    }
}

impl DeterministicRuntimeDecisionPolicy {
    fn finish(
        &self,
        decision: RuntimeDecision,
        knowledge: KnowledgeState,
        reasons: Vec<DecisionReason>,
        evidence: Vec<EvidenceRef>,
        blockers: Vec<DecisionBlocker>,
        governance: GovernanceDisposition,
    ) -> RuntimeDecisionEvaluation {
        RuntimeDecisionEvaluation {
            decision,
            reasons,
            evidence,
            blockers,
            knowledge,
            policy_version: self.config.version.clone(),
            governance,
        }
    }
}

fn knowledge_reason(knowledge: KnowledgeState) -> &'static str {
    match knowledge {
        KnowledgeState::KnownSupported => "supported evidence needs a bounded high-risk check",
        KnowledgeState::KnownContradicted => "compatible evidence materially contradicts itself",
        KnowledgeState::KnownStale => "applicable evidence is stale in the current environment",
        KnowledgeState::Unknown => "applicable empirical evidence is insufficient",
        KnowledgeState::OutOfScope => {
            "current context is outside the evidence or certification scope"
        }
    }
}

fn critical_assurance_blockers(context: &RuntimeDecisionContext) -> Vec<DecisionBlocker> {
    if context.assurance.gaps.is_empty() {
        return vec![DecisionBlocker::CriticalAssuranceGap(
            "No current applicable assurance supports this consequential action".into(),
        )];
    }
    context
        .assurance
        .gaps
        .iter()
        .filter(|gap| {
            gap.severity
                .is_some_and(|severity| severity >= Severity::High)
                || matches!(
                    gap.kind,
                    AssuranceGapKind::ContractInconclusive
                        | AssuranceGapKind::ContradictoryEvidence
                        | AssuranceGapKind::UnsupportedEffect
                        | AssuranceGapKind::UnsupportedIsolation
                )
        })
        .map(|gap| DecisionBlocker::CriticalAssuranceGap(gap.description.clone()))
        .collect()
}

fn proposed_action_patterns(context: &RuntimeDecisionContext) -> Vec<ActionPattern> {
    match &context.proposed_action {
        Some(crate::bridge::protocol::NormalizedAction::Shell { command, .. }) => {
            vec![ActionPattern::shell(command)]
        }
        Some(crate::bridge::protocol::NormalizedAction::FileWrite { path })
        | Some(crate::bridge::protocol::NormalizedAction::FileDelete { path }) => {
            vec![ActionPattern::FileOperation {
                pattern: path.clone(),
            }]
        }
        Some(action) => vec![ActionPattern::Custom {
            kind: "normalized_action".into(),
            value: serde_json::to_string(action).unwrap_or_else(|_| "unavailable".into()),
        }],
        None => Vec::new(),
    }
}
