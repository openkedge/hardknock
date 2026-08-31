// SPDX-License-Identifier: Apache-2.0
use crate::{
    core::*,
    evaluation::EvaluationSpec,
    experience::ExperienceContext,
    lesson::{ActionPattern, ConfidenceScore, ContextSelector, EvidenceRef},
    perturbation::Perturbation,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum FixtureKind {
    RetryResilience,
    StaleCredential,
    ConfigDrift,
    SkillHardening,
    SkillHardeningTransfer,
}
impl FixtureKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::RetryResilience => "retry-resilience",
            Self::StaleCredential => "stale-credential",
            Self::ConfigDrift => "config-drift",
            Self::SkillHardening => "skill-hardening",
            Self::SkillHardeningTransfer => "skill-hardening-transfer",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ChaosTarget {
    Task(String),
    Command(CommandSpec),
    Skill(SkillId),
}
pub type EnvelopeTarget = ChaosTarget;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CampaignPlan {
    pub target: ChaosTarget,
    pub starting_state: StateRef,
    pub goal: String,
    pub command: CommandSpec,
    pub evaluation: EvaluationSpec,
    pub agent: AgentIdentity,
    pub fixture: Option<FixtureKind>,
    pub perturbations: Vec<Vec<Perturbation>>,
    /// Perturbed runs only; the mandatory control costs one additional run.
    pub trial_budget: usize,
    pub timeout_secs: u64,
    pub max_duration_secs: u64,
    pub environment: crate::experience::EnvironmentContext,
    pub hardknock_version: String,
    pub runtime_version: String,
    pub fixture_version: Option<String>,
    pub active_reflexes: Vec<Reflex>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignStatus {
    Running,
    Completed,
    UnhealthyControl,
    BudgetExhausted,
    Interrupted,
    Failed,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChaosCampaign {
    pub id: ChaosCampaignId,
    pub plan: CampaignPlan,
    pub control: Option<ChaosTrial>,
    pub trials: Vec<ChaosTrial>,
    pub result: CampaignStatus,
    pub stop_reason: Option<String>,
    pub envelope_id: Option<OperatingEnvelopeId>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChaosTrialOutcome {
    Pass,
    Degraded,
    Fail,
    Inconclusive,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChaosTrial {
    pub id: ChaosTrialId,
    pub campaign_id: ChaosCampaignId,
    pub is_control: bool,
    pub index: usize,
    pub reality_id: RealityId,
    pub experience_id: ExperienceId,
    pub execution_id: ExecutionId,
    pub evaluation_id: EvaluationId,
    pub perturbations: Vec<Perturbation>,
    pub outcome: ChaosTrialOutcome,
    pub metrics: TrialMetrics,
    #[serde(default)]
    pub failure_signatures: Vec<String>,
    pub lessons: Vec<LessonId>,
    pub reflexes: Vec<ReflexId>,
    pub recoveries: Vec<RecoveryId>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TrialMetrics {
    pub attempts: u32,
    pub retries: u32,
    pub failed_attempts: u32,
    pub duration_ms: u64,
    /// Fixture logical time is not measured network latency.
    pub simulated_duration_ms: Option<u64>,
    pub failure_detection_ms: Option<u64>,
    pub replans: u32,
    pub degradations: Vec<DegradationObservation>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DegradationObservation {
    pub metric: String,
    pub baseline: f64,
    pub observed: f64,
    pub ratio: Option<f64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TemporalObservation {
    pub attempt: u32,
    pub failed: bool,
    pub consecutive_failures: u32,
    pub no_state_change: bool,
    pub config_changed: bool,
    pub elapsed_ms: u64,
    pub action: ActionPattern,
    pub artifacts: Vec<ArtifactRef>,
    pub state_before: String,
    pub state_after: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChaosOrigin {
    pub campaign_id: ChaosCampaignId,
    pub trial_id: ChaosTrialId,
    pub control: Option<ExperienceId>,
    pub index: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResilienceObservation {
    pub origin: Option<ChaosOrigin>,
    pub perturbation_ids: Vec<PerturbationId>,
    pub outcome: ChaosTrialOutcome,
    pub metrics: TrialMetrics,
    pub temporal: Vec<TemporalObservation>,
    pub reflex_matches: Vec<ReflexMatch>,
    pub recovery_attempt: Option<RecoveryAttempt>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnvelopeCondition {
    pub perturbations: Vec<Perturbation>,
    pub trial_id: ChaosTrialId,
    pub experience_id: ExperienceId,
    pub outcome: ChaosTrialOutcome,
}
/// Deliberately point-valued: no interval interpolation or extrapolation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConditionRange {
    TestedPoint { trial_id: ChaosTrialId },
    AllUntestedConditions,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperatingEnvelope {
    pub id: OperatingEnvelopeId,
    pub version: u32,
    pub target: EnvelopeTarget,
    pub campaign_id: ChaosCampaignId,
    pub tested_conditions: Vec<EnvelopeCondition>,
    pub safe_regions: Vec<ConditionRange>,
    pub degraded_regions: Vec<ConditionRange>,
    pub failure_regions: Vec<ConditionRange>,
    pub unknown_regions: Vec<ConditionRange>,
    pub evidence: Vec<EvidenceRef>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReflexStatus {
    Candidate,
    Supported,
    Active,
    Disabled,
    Retired,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReflexResponse {
    Advise,
    Warn,
    Replan,
    Block,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TriggerPattern {
    pub context: ContextSelector,
    pub proposed_action: ActionPattern,
    pub repeated_failures: Option<u32>,
    pub no_state_change: bool,
    pub config_changed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reflex {
    pub id: ReflexId,
    pub version: u32,
    pub source_lessons: Vec<LessonId>,
    pub source_trial: ChaosTrialId,
    pub trigger: TriggerPattern,
    pub response: ReflexResponse,
    pub confidence: ConfidenceScore,
    pub status: ReflexStatus,
    pub evidence: Vec<EvidenceRef>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionContext {
    pub context: ExperienceContext,
    pub proposed_action: ActionPattern,
    pub consecutive_failures: u32,
    pub no_state_change: bool,
    pub config_changed: bool,
    pub elapsed_ms: u64,
    pub state_fingerprint: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReflexMatch {
    pub reflex_id: ReflexId,
    pub reflex_version: u32,
    pub trigger: TriggerPattern,
    pub response: ReflexResponse,
    pub confidence: ConfidenceScore,
    pub source_lessons: Vec<LessonId>,
    pub source_trial: ChaosTrialId,
    pub observed: ActionContext,
    pub test_only: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryStatus {
    Candidate,
    Supported,
    Validated,
    Contradicted,
    Retired,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FailureSignaturePattern {
    pub signature: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecoveryStep {
    ShellCommand { command: CommandSpec },
    SetEnvironmentVariable { key: String, value: String },
    Replan,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Recovery {
    pub id: RecoveryId,
    pub version: u32,
    pub source_trial: ChaosTrialId,
    pub failure_signature: FailureSignaturePattern,
    pub context: ContextSelector,
    pub steps: Vec<RecoveryStep>,
    pub status: RecoveryStatus,
    pub confidence: ConfidenceScore,
    pub evidence: Vec<EvidenceRef>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryAttempt {
    pub recovery_id: RecoveryId,
    pub recovery_version: u32,
    pub reproduced_failure: bool,
    pub failure_signature: Option<String>,
    pub attempted: bool,
    pub succeeded: bool,
    pub time_to_recovery_ms: u64,
    pub steps_executed: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillStatus {
    Candidate,
    Supported,
    Validated,
    Retired,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Skill {
    pub id: SkillId,
    pub name: String,
    pub description: String,
    pub context: ContextSelector,
    pub procedure: Vec<ActionPattern>,
    pub evidence: Vec<EvidenceRef>,
    pub status: SkillStatus,
    pub operating_envelope: Option<OperatingEnvelopeId>,
    pub source_experience: ExperienceId,
    #[serde(default)]
    pub maturity: crate::curriculum::SkillMaturity,
    #[serde(default)]
    pub coverage: crate::curriculum::SkillCoverage,
    /// The current contract binding is revisioned separately. Historical Skill
    /// and certification records retain the exact contract revision they used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavioral_contract: Option<crate::assurance::BehavioralContractRef>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResilienceTestStatus {
    Running,
    Supported,
    Contradicted,
    FalsePositive,
    NegativeControlPassed,
    Inconclusive,
    Failed,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResilienceTest {
    pub id: ResilienceTestId,
    pub reflex_id: Option<ReflexId>,
    pub recovery_id: Option<RecoveryId>,
    pub source_trial: ChaosTrialId,
    pub perturbations: Vec<Perturbation>,
    pub without: Option<ExperienceId>,
    pub with: Option<ExperienceId>,
    pub status: ResilienceTestStatus,
    pub false_positive: Option<bool>,
    pub created_at: DateTime<Utc>,
    pub reason: String,
}
