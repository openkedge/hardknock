// SPDX-License-Identifier: Apache-2.0
use crate::{
    Result,
    causal::CausalHypothesisRef,
    core::*,
    curriculum::Severity,
    effects::{ExternalityClass, ReversibilityClass},
    lesson::ContextSelector,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::Duration};

pub type TrajectoryEventRef = TrajectoryEventId;
pub type TrajectoryPointRef = (TrajectoryId, u64);
pub type RiskSignalRef = RiskSignalId;
pub type RiskIndicatorRef = RiskIndicatorId;
pub type EarlyWarningSignatureRef = EarlyWarningSignatureId;
pub type FailureForecastRef = FailureForecastId;
pub type PreventiveInterventionRef = PreventiveInterventionId;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "subject", content = "id", rename_all = "snake_case")]
pub enum TrajectorySubject {
    Task(TaskId),
    Skill(SkillId),
    EffectPlan(EffectPlanId),
    Recovery(RecoveryId),
    RuntimeDecision(RuntimeDecisionId),
}
impl Default for TrajectorySubject {
    fn default() -> Self {
        Self::Task(TaskId::new())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum TrajectoryValue {
    Boolean(bool),
    Integer(i64),
    Decimal(String),
    Text(String),
}
impl TrajectoryValue {
    pub fn normalized(&self) -> String {
        match self {
            Self::Boolean(v) => v.to_string(),
            Self::Integer(v) => v.to_string(),
            Self::Decimal(v) | Self::Text(v) => v.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrajectoryObservation {
    /// Only normalized operational observations belong here; secrets and raw transcripts do not.
    pub features: BTreeMap<String, TrajectoryValue>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrajectoryEventKind {
    ActionProposed,
    ActionCompleted,
    ToolStarted,
    ToolCompleted,
    EvaluationObserved,
    FailureObserved,
    RecoveryAttempted,
    EffectPrepared,
    EffectCommitAttempted,
    EffectCommitted,
    EffectUnknown,
    Retry,
    StateRefresh,
    StateObserved,
    CapabilityDenied,
    RuntimeDecision,
    Custom(String),
}

pub type ObservedValue = TrajectoryValue;
pub type RiskSignalValue = TrajectoryValue;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateObservation {
    pub variables: BTreeMap<String, ObservedValue>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskSignalKind {
    RetryCount,
    StateAge,
    VersionMismatch,
    Latency,
    FailureFrequency,
    RepeatedAction,
    EnvelopeBoundaryDistance,
    CredentialAge,
    ResourceContention,
    EffectConflict,
    UnknownOutcome,
    Custom(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RiskSignal {
    pub id: RiskSignalId,
    pub kind: RiskSignalKind,
    pub value: RiskSignalValue,
    #[serde(default)]
    pub evidence: Vec<TrajectoryEvidenceRef>,
    pub observed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrajectoryPoint {
    pub index: u64,
    pub timestamp: DateTime<Utc>,
    pub event: TrajectoryEventKind,
    pub state: StateObservation,
    #[serde(default)]
    pub derived_signals: Vec<RiskSignal>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum TrajectoryEvidenceRef {
    Trajectory(TrajectoryId),
    Experience(ExperienceId),
    Execution(ExecutionId),
    RuntimeDecision(RuntimeDecisionId),
    Effect(EffectId),
    CausalEvidence(CausalEvidenceId),
    ExternalAdvisory(String),
    Custom(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrajectoryEvent {
    pub id: TrajectoryEventId,
    pub trajectory_id: TrajectoryId,
    pub sequence: u64,
    pub timestamp: DateTime<Utc>,
    pub kind: TrajectoryEventKind,
    pub observation: TrajectoryObservation,
    #[serde(default)]
    pub evidence: Vec<TrajectoryEvidenceRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", content = "failure", rename_all = "snake_case")]
pub enum TrajectoryOutcome {
    Success,
    Degraded,
    Failure(crate::runtime::FailureSignatureRef),
    Aborted,
    Abstained,
    Unknown,
}
impl TrajectoryOutcome {
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failure(_))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrajectoryContext {
    pub scope: ContextSelector,
    #[serde(default)]
    pub runtime_version: Option<String>,
    #[serde(default)]
    pub tool_versions: BTreeMap<String, String>,
    /// Declared feature names observable in this context.
    #[serde(default)]
    pub observability: Vec<String>,
}
impl Default for TrajectoryContext {
    fn default() -> Self {
        Self {
            scope: ContextSelector {
                repository: None,
                required_markers: Vec::new(),
                tags: Vec::new(),
                os: None,
                arch: None,
            },
            runtime_version: None,
            tool_versions: BTreeMap::new(),
            observability: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrajectoryFingerprint {
    pub hash: String,
    pub event_kinds: Vec<TrajectoryEventKind>,
    pub salient_features: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionTrajectory {
    pub id: TrajectoryId,
    pub session_id: HardknockSessionId,
    #[serde(default)]
    pub subject: TrajectorySubject,
    pub task_family: Option<TaskFamilyId>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub events: Vec<TrajectoryEventRef>,
    #[serde(default)]
    pub points: Vec<TrajectoryPoint>,
    pub outcome: Option<TrajectoryOutcome>,
    pub context: TrajectoryContext,
    #[serde(default)]
    pub fingerprint: TrajectoryFingerprint,
}

pub trait TrajectoryStore {
    fn append_point(
        &self,
        trajectory: &TrajectoryId,
        event: TrajectoryEventKind,
        state: StateObservation,
        evidence: Vec<TrajectoryEvidenceRef>,
    ) -> Result<TrajectoryPoint>;

    fn get(&self, trajectory: &TrajectoryId) -> Result<ExecutionTrajectory>;

    fn list(&self) -> Result<Vec<ExecutionTrajectory>>;

    fn find_by_failure(
        &self,
        failure: &crate::runtime::FailureSignatureRef,
    ) -> Result<Vec<ExecutionTrajectory>>;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrajectoryWindow {
    pub max_events: usize,
    pub max_duration: Option<Duration>,
}
impl Default for TrajectoryWindow {
    fn default() -> Self {
        Self {
            max_events: 20,
            max_duration: Some(Duration::from_secs(300)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonOperator {
    Equals,
    NotEquals,
    AtLeast,
    AtMost,
    GreaterThan,
    LessThan,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeaturePredicate {
    pub feature: String,
    pub operator: ComparisonOperator,
    pub value: TrajectoryValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventPredicate {
    pub kind: TrajectoryEventKind,
    #[serde(default)]
    pub feature: Option<FeaturePredicate>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrajectoryCondition {
    EventObserved {
        predicate: EventPredicate,
    },
    FeatureCondition {
        predicate: FeaturePredicate,
    },
    Before {
        first: Box<Self>,
        second: Box<Self>,
    },
    After {
        first: Box<Self>,
        second: Box<Self>,
    },
    Repeated {
        condition: Box<Self>,
        minimum: usize,
    },
    Within {
        condition: Box<Self>,
        duration: Duration,
    },
}

pub type StatePredicate = FeaturePredicate;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventPattern {
    pub kind: TrajectoryEventKind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemporalConstraint {
    pub max_events_between: Option<u32>,
    pub max_duration_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrajectoryPatternStep {
    pub event_pattern: Option<EventPattern>,
    #[serde(default)]
    pub state_predicates: Vec<StatePredicate>,
    #[serde(default)]
    pub causal_relevance: Option<CausalHypothesisRef>,
    #[serde(default)]
    pub temporal: Option<TemporalConstraint>,
    /// Canonical condition retained when the pattern originated from an
    /// EarlyWarningSignature. This supports compound conditions without
    /// forcing them into a lossy event/state shape.
    #[serde(default)]
    pub condition: Option<TrajectoryCondition>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureTrajectoryStatus {
    Candidate,
    Supported,
    Validated,
    Stale,
    Contradicted,
    Retired,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FailureTrajectory {
    pub id: FailureTrajectoryId,
    pub failure_signature: crate::runtime::FailureSignatureRef,
    pub causal_model: Option<CausalModelId>,
    pub sequence: Vec<TrajectoryPatternStep>,
    pub scope: ContextSelector,
    #[serde(default)]
    pub evidence: Vec<TrajectoryEvidenceRef>,
    pub status: FailureTrajectoryStatus,
    pub origin: PredictiveOrigin,
    pub required_runtime_version: Option<String>,
    pub revision: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FailureTrajectoryFamily {
    pub id: FailureTrajectoryFamilyId,
    pub failure_signature: crate::runtime::FailureSignatureRef,
    pub trajectories: Vec<FailureTrajectoryId>,
    pub shared_precursors: Vec<TrajectoryPatternStep>,
    pub divergent_precursors: Vec<TrajectoryPatternStep>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrajectoryMatchStatus {
    Weak,
    Partial,
    Strong,
    Terminal,
    Contradicted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FailureTrajectoryMatch {
    pub failure_trajectory: FailureTrajectoryId,
    pub matched_steps: usize,
    pub total_steps: usize,
    pub matched: Vec<usize>,
    pub missing: Vec<usize>,
    pub contradicted: Vec<usize>,
    pub status: TrajectoryMatchStatus,
    pub last_match_index: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CausalPrecursor {
    pub hypothesis: CausalHypothesisRef,
    pub current_state: crate::causal::VariableValue,
    pub predicted_transition: Option<crate::causal::VariableValue>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvelopeProximity {
    Interior,
    NearBoundary,
    AtBoundary,
    OutsideKnownSafeRegion,
    Unknown,
}
impl Default for EnvelopeProximity {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskIndicatorStatus {
    Candidate,
    Supported,
    Validated,
    Noisy,
    Stale,
    Contradicted,
    Retired,
}

/// V0.15 uses the same evidence lifecycle for risk indicators and warning signatures.
pub type EarlyWarningStatus = RiskIndicatorStatus;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndicatorCondition {
    pub conditions: Vec<TrajectoryCondition>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RiskIndicator {
    pub id: RiskIndicatorId,
    pub name: String,
    pub condition: IndicatorCondition,
    pub associated_failures: Vec<crate::runtime::FailureSignatureRef>,
    pub scope: ContextSelector,
    #[serde(default)]
    pub evidence: Vec<TrajectoryEvidenceRef>,
    pub status: RiskIndicatorStatus,
    pub origin: PredictiveOrigin,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "horizon", content = "value", rename_all = "snake_case")]
pub enum ForecastHorizon {
    NextAction,
    Actions(u32),
    NextNEvents(u32),
    BeforeEffectCommit,
    BeforeTaskCompletion,
    Duration(Duration),
    BeforeMilestone(String),
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EmpiricalRate {
    pub value: f64,
    pub sample_count: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EarlyWarningSignature {
    pub id: EarlyWarningSignatureId,
    pub failure: crate::runtime::FailureSignatureRef,
    pub ordered_conditions: Vec<TrajectoryCondition>,
    #[serde(default)]
    pub failure_trajectory: Option<FailureTrajectoryId>,
    pub horizon: ForecastHorizon,
    pub scope: ContextSelector,
    #[serde(default)]
    pub evidence: Vec<TrajectoryEvidenceRef>,
    pub status: RiskIndicatorStatus,
    pub revision: u64,
    pub origin: PredictiveOrigin,
    #[serde(default)]
    pub causal_basis: Vec<CausalHypothesisRef>,
    #[serde(default)]
    pub required_runtime_version: Option<String>,
    #[serde(default)]
    pub precision: Option<EmpiricalRate>,
    #[serde(default)]
    pub recall: Option<EmpiricalRate>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ForecastSignatureRevision {
    pub id: ForecastRevisionId,
    pub signature_id: EarlyWarningSignatureId,
    pub revision: u64,
    pub conditions: Vec<TrajectoryCondition>,
    pub evidence: Vec<TrajectoryEvidenceRef>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredictiveOrigin {
    Local,
    FederatedAdvisory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForecastStrength {
    InsufficientEvidence,
    Weak,
    Moderate,
    Strong,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForecastEvidenceKind {
    Correlational,
    Causal,
    Mixed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForecastStatus {
    Watch,
    Elevated,
    Actionable,
    Imminent,
    Cleared,
    Materialized,
    FalsePositive,
    /// Legacy V0.15 draft states retained for decoding existing local stores.
    Active,
    ResolvedFailureOccurred,
    ResolvedAvoided,
    ResolvedFalseAlarm,
    Expired,
    Inconclusive,
}
impl ForecastStatus {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Watch | Self::Elevated | Self::Actionable | Self::Imminent | Self::Active
        )
    }
    pub fn is_actionable(self) -> bool {
        matches!(self, Self::Actionable | Self::Imminent | Self::Active)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForecastLead {
    pub event_distance: Option<u32>,
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedSignal {
    pub description: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FailureForecast {
    pub id: FailureForecastId,
    pub trajectory_id: TrajectoryId,
    pub failure: crate::runtime::FailureSignatureRef,
    pub matched_trajectory: Option<FailureTrajectoryId>,
    pub horizon: ForecastHorizon,
    #[serde(default)]
    pub evidence: Vec<TrajectoryEvidenceRef>,
    pub indicators: Vec<RiskIndicatorRef>,
    #[serde(default)]
    pub matched_signals: Vec<RiskSignalRef>,
    #[serde(default)]
    pub missing_signals: Vec<ExpectedSignal>,
    #[serde(default)]
    pub recommended_interventions: Vec<PreventiveInterventionRef>,
    pub signature: EarlyWarningSignatureRef,
    pub causal_basis: Vec<CausalHypothesisRef>,
    pub evidence_kind: ForecastEvidenceKind,
    pub strength: ForecastStrength,
    pub status: ForecastStatus,
    pub historical_matches: Vec<TrajectoryId>,
    pub warning_sequence: u64,
    #[serde(default)]
    pub lead: ForecastLead,
    #[serde(default)]
    pub matcher_version: String,
    #[serde(default)]
    pub policy_version: String,
    #[serde(default)]
    pub warning_revision: u64,
    pub causal_model_revision: Option<u64>,
    #[serde(default)]
    pub advisory: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCompatibility {
    Exact,
    Compatible,
    OutOfScope,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrajectoryMatch {
    pub trajectory: TrajectoryId,
    pub matched_events: usize,
    pub matching_indicators: Vec<RiskIndicatorRef>,
    pub context_compatibility: ContextCompatibility,
    pub outcome: TrajectoryOutcome,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EarlyWarningMatch {
    pub signature: EarlyWarningSignatureRef,
    pub matched_conditions: Vec<String>,
    pub unmatched_conditions: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ForecastContext {
    pub events: Vec<TrajectoryEvent>,
    pub signatures: Vec<EarlyWarningSignature>,
    pub indicators: Vec<RiskIndicator>,
    pub history: Vec<ExecutionTrajectory>,
    pub supported_causal_hypotheses: Vec<CausalHypothesisRef>,
    #[serde(default)]
    pub causal_precursors: Vec<CausalPrecursor>,
    #[serde(default)]
    pub envelope_proximity: EnvelopeProximity,
    #[serde(default = "default_forecast_risk")]
    pub risk: Severity,
    #[serde(default)]
    pub failure_trajectories: Vec<FailureTrajectory>,
    #[serde(default)]
    pub interventions: Vec<PreventiveIntervention>,
    #[serde(default)]
    pub policy: ForecastPolicyConfig,
    pub window: TrajectoryWindow,
}

fn default_forecast_risk() -> Severity {
    Severity::Medium
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForecastPolicyProfile {
    PredictiveObserve,
    PredictiveBalanced,
    PredictiveConservative,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForecastPolicyConfig {
    pub version: String,
    pub profile: ForecastPolicyProfile,
    pub elevated_matched_steps: usize,
    pub actionable_matched_steps: usize,
}
impl Default for ForecastPolicyConfig {
    fn default() -> Self {
        Self {
            version: "predictive-policy-v1".into(),
            profile: ForecastPolicyProfile::PredictiveObserve,
            elevated_matched_steps: 2,
            actionable_matched_steps: 3,
        }
    }
}
impl ForecastPolicyConfig {
    pub fn for_profile(profile: ForecastPolicyProfile) -> Self {
        match profile {
            ForecastPolicyProfile::PredictiveObserve => Self::default(),
            ForecastPolicyProfile::PredictiveBalanced => Self {
                version: "predictive-policy-balanced-v1".into(),
                profile,
                elevated_matched_steps: 2,
                actionable_matched_steps: 3,
            },
            ForecastPolicyProfile::PredictiveConservative => Self {
                version: "predictive-policy-conservative-v1".into(),
                profile,
                elevated_matched_steps: 3,
                actionable_matched_steps: 4,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", content = "detail", rename_all = "snake_case")]
pub enum InterventionAction {
    Observe,
    Warn(String),
    RefreshAuthoritativeState,
    RefreshCredential,
    Replan,
    ReprepareEffect,
    ReconcileEffect,
    ReduceCapability,
    DelayAction,
    AbortPreparedEffect,
    ApplyRecoveryEarly(RecoveryId),
    RunExperiment(String),
    SwitchTool(String),
    Custom(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterventionDisruption {
    Minimal,
    Moderate,
    High,
}
impl Default for InterventionDisruption {
    fn default() -> Self {
        Self::Moderate
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterventionWindow {
    pub opens_at: TrajectoryPointRef,
    pub closes_at: Option<TrajectoryPointRef>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterventionCost {
    Negligible,
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreventiveInterventionStatus {
    Candidate,
    Supported,
    Validated,
    Contradicted,
    Retired,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreventiveIntervention {
    pub id: PreventiveInterventionId,
    pub forecast: Option<FailureForecastRef>,
    pub signature: EarlyWarningSignatureRef,
    pub action: InterventionAction,
    pub target_failure: crate::runtime::FailureSignatureRef,
    pub evidence: Vec<TrajectoryEvidenceRef>,
    pub status: PreventiveInterventionStatus,
    #[serde(default)]
    pub disruption: InterventionDisruption,
    pub cost: InterventionCost,
    pub reversibility: ReversibilityClass,
    pub externality: ExternalityClass,
    pub requires_commit_authority: bool,
    pub scope: ContextSelector,
    pub origin: PredictiveOrigin,
    #[serde(default)]
    pub window: Option<InterventionWindow>,
    #[serde(default)]
    pub mechanism: Option<CausalHypothesisRef>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreventiveEvidenceOutcome {
    AvoidedFailure,
    NoEffect,
    Harmful,
    Inconclusive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForecastOutcome {
    FailureMaterialized,
    FailureAvoided,
    FalsePositive,
    ClearedWithoutIntervention,
    Inconclusive,
    InterventionWindowMissed,
}
impl Default for ForecastOutcome {
    fn default() -> Self {
        Self::Inconclusive
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreventiveCounterfactual {
    pub id: PreventiveCounterfactualId,
    pub forecast: FailureForecastRef,
    pub intervention: PreventiveInterventionRef,
    pub control: crate::causal::TrialRef,
    pub intervention_trial: crate::causal::TrialRef,
    pub result: PreventiveEvidenceOutcome,
    pub starting_fingerprint: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ForecastFeedback {
    pub id: ForecastFeedbackId,
    pub forecast_id: FailureForecastId,
    pub status: ForecastStatus,
    #[serde(default)]
    pub outcome: ForecastOutcome,
    pub observed_outcome: TrajectoryOutcome,
    pub intervention: Option<PreventiveInterventionRef>,
    pub evidence: Vec<TrajectoryEvidenceRef>,
    pub warning_lead_actions: Option<u64>,
    #[serde(default)]
    pub lead: ForecastLead,
    pub intervention_feasible: Option<bool>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ForecastConflict {
    pub forecasts: Vec<FailureForecastId>,
    pub interventions: Vec<PreventiveInterventionId>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ForecastQualitySummary {
    pub resolved_forecasts: usize,
    pub failures_observed: usize,
    pub forecasted_failures: usize,
    pub precision: Option<f64>,
    pub recall: Option<f64>,
    pub false_positive_rate: Option<f64>,
    pub false_negative_rate: Option<f64>,
    pub median_warning_lead_actions: Option<f64>,
    pub avoided_failure_rate: Option<f64>,
    pub unnecessary_preventive_intervention_rate: Option<f64>,
    pub sufficient_samples: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Forecastability {
    InsufficientObservability,
    NotYetPredictable,
    WeaklyPredictable,
    Predictable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForecastHealth {
    Healthy,
    Degrading,
    Stale,
    Contradicted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ForecastHealthAssessment {
    pub signature: EarlyWarningSignatureId,
    pub health: ForecastHealth,
    pub reason: String,
    pub revalidation_recommended: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PredictiveExperienceSummary {
    pub early_warning_signatures: usize,
    pub validated_signatures: usize,
    pub forecast_quality: ForecastQualitySummary,
    pub validated_preventive_interventions: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreventivePolicyContext {
    pub runtime: crate::runtime::RuntimeDecisionContext,
    pub failure_severity: Severity,
    pub available_interventions: Vec<PreventiveIntervention>,
    pub false_positive_rate: Option<f64>,
    pub adequate_evidence_diversity: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", content = "detail", rename_all = "snake_case")]
pub enum PreventiveDecision {
    Observe,
    Warn(String),
    Intervene(PreventiveInterventionRef),
    Experiment,
    RequireApproval(PreventiveInterventionRef),
    Abstain,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InterventionSelection {
    pub selected: Option<PreventiveInterventionId>,
    pub considered: Vec<PreventiveInterventionId>,
    pub decision: PreventiveDecision,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ForecastMiss {
    pub trajectory: TrajectoryId,
    pub failure: crate::runtime::FailureSignatureRef,
    pub forecastability: Forecastability,
    pub observability_gap: Option<String>,
    pub created_at: DateTime<Utc>,
}
