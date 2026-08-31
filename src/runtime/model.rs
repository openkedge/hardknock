// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    assurance::{AssuranceGap, AssuranceProfileRef, CertificationFreshness},
    bridge::protocol::NormalizedAction,
    capability::{ExecutionCapability, IsolationLevel, RealityRequirements},
    core::{
        AgentIdentity, HardknockSessionId, LessonId, OperatingEnvelopeId, RecoveryId, ReflexId,
        RuntimeDecisionId, SkillCertificationId, SkillId,
    },
    curriculum::Severity,
    development::ActiveExperienceSet,
    effects::{EffectRequest, EffectRisk, ExternalityClass, ReversibilityClass},
    lesson::{ActionPattern, ConfidenceScore},
    retrieval::QueryContext,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDescriptor {
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillRef {
    pub id: SkillId,
    pub revision: u64,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LessonRef {
    pub id: LessonId,
    pub version: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflexRef {
    pub id: ReflexId,
    pub version: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryRef {
    pub id: RecoveryId,
    pub version: u32,
    pub failure_signature: String,
    pub confidence: ConfidenceScore,
    pub fresh: bool,
    pub scope_matches: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureSignatureRef {
    pub signature: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvelopePosition {
    KnownSafe,
    KnownDegraded,
    KnownFailure,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatingEnvelopeRef {
    pub id: OperatingEnvelopeId,
    pub version: u32,
    pub position: EnvelopePosition,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeState {
    KnownSupported,
    KnownContradicted,
    KnownStale,
    Unknown,
    OutOfScope,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeSignals {
    pub local_supported: bool,
    pub local_contradicted: bool,
    pub evidence_stale: bool,
    pub context_in_scope: bool,
    pub validated_skill: bool,
    pub applicable_lesson: bool,
    /// A current, locally supported Lesson explicitly identifies the proposed
    /// action as one to avoid in this context.
    #[serde(default)]
    pub known_failure_precursor: bool,
    pub remote_advisory_only: bool,
    pub known_gap_matches: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssuranceRuntimeStatus {
    Current,
    ReviewRecommended,
    Expired,
    Invalidated,
    OutOfScope,
    Missing,
    Inconclusive,
}

impl From<CertificationFreshness> for AssuranceRuntimeStatus {
    fn from(value: CertificationFreshness) -> Self {
        match value {
            CertificationFreshness::Current => Self::Current,
            CertificationFreshness::ReviewRecommended => Self::ReviewRecommended,
            CertificationFreshness::Expired => Self::Expired,
            CertificationFreshness::Invalidated => Self::Invalidated,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssuranceSummary {
    pub status: AssuranceRuntimeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certification: Option<SkillCertificationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<AssuranceProfileRef>,
    #[serde(default)]
    pub reasons: Vec<String>,
}

impl Default for AssuranceSummary {
    fn default() -> Self {
        Self {
            status: AssuranceRuntimeStatus::Missing,
            certification: None,
            profile: None,
            reasons: vec!["No applicable local certification was found".into()],
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssuranceApplicability {
    pub applicable: bool,
    #[serde(default)]
    pub reasons: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CertificationStatusRequirement {
    Current,
    CurrentOrReviewRecommended,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeAssuranceRequirement {
    pub action_pattern: ActionPattern,
    pub minimum_profile: AssuranceProfileRef,
    pub required_status: CertificationStatusRequirement,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AssuranceContext {
    pub summary: AssuranceSummary,
    pub applicability: AssuranceApplicability,
    #[serde(default)]
    pub requirements: Vec<RuntimeAssuranceRequirement>,
    #[serde(default)]
    pub gaps: Vec<AssuranceGap>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceContext {
    pub hard_policy_blocked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_reason: Option<String>,
    pub approval_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_reason: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityContext {
    #[serde(default)]
    pub available: Vec<ExecutionCapability>,
    #[serde(default)]
    pub missing: Vec<ExecutionCapability>,
    pub required_available: bool,
    pub commit_authority: bool,
    pub effect_adapter_available: bool,
    pub isolation_sufficient: bool,
    pub isolation_level: IsolationLevel,
    #[serde(default)]
    pub governance: GovernanceContext,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeRiskAssessment {
    pub severity: Severity,
    pub reversibility: ReversibilityClass,
    pub externality: ExternalityClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assurance_requirement: Option<AssuranceProfileRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_risk: Option<EffectRisk>,
    #[serde(default)]
    pub rationale: Vec<String>,
}

impl Default for RuntimeRiskAssessment {
    fn default() -> Self {
        Self {
            severity: Severity::Low,
            reversibility: ReversibilityClass::NaturallyReversible,
            externality: ExternalityClass::RealityLocal,
            assurance_requirement: None,
            effect_risk: None,
            rationale: vec!["No consequential external effect was identified".into()],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UncertaintyLevel {
    Low,
    Medium,
    High,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum UncertaintyReason {
    AgentReported(String),
    MultipleStrategies,
    MissingExperience,
    ContradictoryEvidence,
    OutsideEnvelope,
    FailedPrediction,
    KnownGap(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrategyCandidate {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<NormalizedAction>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCandidate {
    pub name: String,
    pub satisfies_task: bool,
    pub current_assurance: bool,
    /// Count of explicitly declared capability dimensions; a transparent
    /// ordering aid, not a universal safety score.
    pub capability_width: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeUncertainty {
    pub level: UncertaintyLevel,
    #[serde(default)]
    pub reasons: Vec<UncertaintyReason>,
    #[serde(default)]
    pub candidate_strategies: Vec<StrategyCandidate>,
}

impl Default for RuntimeUncertainty {
    fn default() -> Self {
        Self {
            level: UncertaintyLevel::Unknown,
            reasons: vec![UncertaintyReason::MissingExperience],
            candidate_strategies: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentMode {
    Off,
    #[default]
    Suggest,
    Automatic,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperimentCapabilitySummary {
    pub mode: ExperimentMode,
    pub safe_reality_available: bool,
    pub effect_safe: bool,
    pub budget: crate::budget::ExperienceBudget,
    pub budget_remaining: bool,
    pub requirements: RealityRequirements,
}

impl Default for ExperimentCapabilitySummary {
    fn default() -> Self {
        Self {
            mode: ExperimentMode::Suggest,
            safe_reality_available: false,
            effect_safe: false,
            budget: Default::default(),
            budget_remaining: true,
            requirements: RealityRequirements {
                filesystem_isolation: IsolationLevel::Cooperative,
                process_isolation: IsolationLevel::None,
                network_isolation: IsolationLevel::None,
                credential_isolation: IsolationLevel::None,
                effect_gating: false,
            },
        }
    }
}

impl ExperimentCapabilitySummary {
    pub fn can_experiment(&self) -> bool {
        self.mode != ExperimentMode::Off
            && self.safe_reality_available
            && self.effect_safe
            && self.budget_remaining
            && self.budget.max_realities > 0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeDecisionContext {
    pub session_id: HardknockSessionId,
    pub agent: AgentIdentity,
    pub task: TaskDescriptor,
    pub query_context: QueryContext,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_action: Option<NormalizedAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_effect: Option<EffectRequest>,
    pub relevant_experience: ActiveExperienceSet,
    pub assurance: AssuranceContext,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operating_envelope: Option<OperatingEnvelopeRef>,
    pub capability_context: CapabilityContext,
    pub risk: RuntimeRiskAssessment,
    pub uncertainty: RuntimeUncertainty,
    #[serde(default)]
    pub available_recovery: Vec<RecoveryRef>,
    pub available_experiments: ExperimentCapabilitySummary,
    #[serde(default)]
    pub knowledge_signals: KnowledgeSignals,
    #[serde(default)]
    pub applicable_skills: Vec<SkillRef>,
    #[serde(default)]
    pub matched_reflexes: Vec<ReflexRef>,
    #[serde(default)]
    pub advisory_reflexes: Vec<ReflexRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_signature: Option<FailureSignatureRef>,
    #[serde(default)]
    pub known_unknowns: Vec<String>,
    #[serde(default)]
    pub externally_supported: bool,
    #[serde(default)]
    pub tool_candidates: Vec<ToolCandidate>,
}

impl RuntimeDecisionContext {
    pub fn context_hash(&self) -> crate::Result<String> {
        Ok(blake3::hash(&serde_json::to_vec(self)?)
            .to_hex()
            .to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum EvidenceRef {
    Skill(SkillRef),
    Lesson(LessonRef),
    Reflex(ReflexRef),
    Recovery {
        id: RecoveryId,
        version: u32,
    },
    Certification(SkillCertificationId),
    OperatingEnvelope {
        id: OperatingEnvelopeId,
        version: u32,
    },
    ExternalAdvisory(String),
    Experience(String),
    Custom(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum DecisionReason {
    ValidatedSkillApplicable,
    InsideOperatingEnvelope,
    RelevantLessonMatched,
    ReflexMatched,
    RecoveryAvailable,
    EvidenceInsufficient,
    EvidenceContradicted,
    EvidenceStale,
    OutsideOperatingEnvelope,
    UnknownOperatingRegion,
    AssuranceCurrent,
    AssuranceExpired,
    CriticalInvariantRisk,
    SafeExperimentAvailable,
    ExperimentBudgetUnavailable,
    CapabilityUnavailable,
    CommitAuthorityRequired,
    HighRiskEffect,
    ExternalEvidenceAdvisoryOnly,
    HardPolicyPrecedence,
    Custom(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum DecisionBlocker {
    HardPolicyProhibition(String),
    MissingCapability(String),
    MissingCommitAuthority,
    InsufficientIsolation,
    CriticalAssuranceGap(String),
    UnresolvedContradiction,
    UnsafeExperiment,
    ExhaustedBudget,
    UnsupportedEffectAdapter,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActDecision {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended_action: Option<NormalizedAction>,
    #[serde(default)]
    pub applicable_skills: Vec<SkillRef>,
    #[serde(default)]
    pub relevant_lessons: Vec<LessonRef>,
    pub assurance: AssuranceSummary,
    #[serde(default)]
    pub evidence: Vec<EvidenceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended_tool: Option<String>,
    pub warning: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperimentDecision {
    pub reason: String,
    pub question: String,
    #[serde(default)]
    pub candidates: Vec<StrategyCandidate>,
    pub budget: crate::budget::ExperienceBudget,
    pub requirements: RealityRequirements,
    pub automatic: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplanDecision {
    pub reason: String,
    #[serde(default)]
    pub matched_reflexes: Vec<ReflexRef>,
    #[serde(default)]
    pub relevant_lessons: Vec<LessonRef>,
    #[serde(default)]
    pub excluded_actions: Vec<ActionPattern>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoverDecision {
    pub recovery: RecoveryRef,
    pub failure_signature: FailureSignatureRef,
    pub confidence: ConfidenceScore,
    #[serde(default)]
    pub evidence: Vec<EvidenceRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum RequestedAuthority {
    CommitEffect,
    ExecuteCapability(String),
    UserApproval,
    ExternalPolicy(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeAlternative {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApprovalDecision {
    pub reason: String,
    pub requested_authority: RequestedAuthority,
    pub evidence_summary: String,
    pub risk: RuntimeRiskAssessment,
    #[serde(default)]
    pub alternatives: Vec<RuntimeAlternative>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbstentionReason {
    CriticalUnknown,
    UnsupportedEffect,
    InsufficientIsolation,
    NoCommitAuthority,
    UnresolvedContradiction,
    NoValidatedRecovery,
    InconclusiveAssurance,
    BudgetExhausted,
    UnsafeToExperiment,
    ExternalPolicyProhibition,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AbstentionDecision {
    pub reason: AbstentionReason,
    #[serde(default)]
    pub missing_evidence: Vec<AssuranceGap>,
    #[serde(default)]
    pub unresolved_risks: Vec<DecisionBlocker>,
    #[serde(default)]
    pub possible_next_steps: Vec<RuntimeAlternative>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "decision", content = "detail", rename_all = "snake_case")]
pub enum RuntimeDecision {
    Act(ActDecision),
    Experiment(ExperimentDecision),
    Replan(ReplanDecision),
    Recover(RecoverDecision),
    RequireApproval(ApprovalDecision),
    Abstain(AbstentionDecision),
}

impl RuntimeDecision {
    pub fn kind(&self) -> RuntimeDecisionKind {
        match self {
            Self::Act(_) => RuntimeDecisionKind::Act,
            Self::Experiment(_) => RuntimeDecisionKind::Experiment,
            Self::Replan(_) => RuntimeDecisionKind::Replan,
            Self::Recover(_) => RuntimeDecisionKind::Recover,
            Self::RequireApproval(_) => RuntimeDecisionKind::RequireApproval,
            Self::Abstain(_) => RuntimeDecisionKind::Abstain,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeDecisionKind {
    Act,
    Experiment,
    Replan,
    Recover,
    RequireApproval,
    Abstain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GovernanceDisposition {
    RuntimeRecommendation,
    ApprovalOverride,
    SecurityBlocked,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeDecisionEvaluation {
    pub decision: RuntimeDecision,
    #[serde(default)]
    pub reasons: Vec<DecisionReason>,
    #[serde(default)]
    pub evidence: Vec<EvidenceRef>,
    #[serde(default)]
    pub blockers: Vec<DecisionBlocker>,
    pub knowledge: KnowledgeState,
    pub policy_version: String,
    pub governance: GovernanceDisposition,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeControlEventKind {
    RuntimeDecisionRequested,
    RuntimeDecisionMade,
    ExperimentSuggested,
    ExperimentStarted,
    RecoverySelected,
    ReplanRequested,
    ApprovalRequired,
    Abstained,
    AgentDisagreed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeDecisionRecord {
    pub id: RuntimeDecisionId,
    pub session_id: HardknockSessionId,
    pub context_hash: String,
    pub context: RuntimeDecisionContext,
    pub decision: RuntimeDecision,
    pub evaluation: RuntimeDecisionEvaluation,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionOutcome {
    Successful,
    Failed,
    AvoidedFailure,
    UnnecessaryIntervention,
    Inconclusive,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeDecisionFeedback {
    pub decision_id: RuntimeDecisionId,
    pub outcome: DecisionOutcome,
    #[serde(default)]
    pub evidence: Vec<EvidenceRef>,
    pub observed_at: DateTime<Utc>,
    #[serde(default)]
    pub agent_disagreed: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "kebab-case")]
pub enum RuntimePolicyProfile {
    Developer,
    #[default]
    Balanced,
    Conservative,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "kebab-case")]
pub enum RuntimeAutonomy {
    Observe,
    #[default]
    Advise,
    Adaptive,
    Governed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalExperienceRuntimePolicy {
    pub advisory_can_warn: bool,
    pub advisory_can_trigger_experiment: bool,
    pub advisory_can_trigger_replan: bool,
    pub advisory_can_authorize_act: bool,
}

impl Default for ExternalExperienceRuntimePolicy {
    fn default() -> Self {
        Self {
            advisory_can_warn: true,
            advisory_can_trigger_experiment: true,
            advisory_can_trigger_replan: false,
            advisory_can_authorize_act: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimePolicyConfig {
    pub profile: RuntimePolicyProfile,
    pub autonomy: RuntimeAutonomy,
    pub experiment_mode: ExperimentMode,
    pub external_experience: ExternalExperienceRuntimePolicy,
    pub version: String,
}

impl Default for RuntimePolicyConfig {
    fn default() -> Self {
        Self {
            profile: RuntimePolicyProfile::Balanced,
            autonomy: RuntimeAutonomy::Advise,
            experiment_mode: ExperimentMode::Suggest,
            external_experience: Default::default(),
            version: super::RUNTIME_POLICY_VERSION.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedLearningValue {
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeAudit {
    pub decisions: BTreeMap<RuntimeDecisionKind, u64>,
    pub outcomes: BTreeMap<DecisionOutcome, u64>,
    pub total: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeGap {
    pub context_hash: String,
    pub task_family: Option<String>,
    pub knowledge: KnowledgeState,
    pub decision: RuntimeDecisionKind,
    pub occurrences: u64,
    pub reasons: Vec<String>,
    pub curriculum_recommendation: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeDevelopmentMetrics {
    pub decisions: BTreeMap<RuntimeDecisionKind, u64>,
    pub avoided_failures: u64,
    pub unnecessary_interventions: u64,
    pub unnecessary_intervention_rate: Option<f64>,
    pub experiments_per_task: Option<f64>,
    pub recovery_success_rate: Option<f64>,
}
