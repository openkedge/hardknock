// SPDX-License-Identifier: Apache-2.0
use crate::{
    budget::{ExperienceBudget, ExperienceUsage},
    core::*,
    experience::{Experience, RepositoryContext},
    lesson::{ContextSelector, Lesson},
    perturbation::Perturbation,
    resilience::*,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillMaturity {
    #[default]
    Observed,
    Supported,
    Validated,
    Hardened,
    Degraded,
    Retired,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SkillCoverage {
    pub profile: Option<String>,
    pub dimensions: Vec<CoverageDimension>,
    pub tested_conditions: usize,
    pub configured_conditions: usize,
    /// Only a fraction of a finite, named catalog; never universal correctness.
    pub profile_coverage: Option<f64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CoverageDimension {
    pub name: String,
    pub tested: Vec<ConditionObservation>,
    pub unknown: Vec<String>,
    pub coverage_score: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConditionObservation {
    pub condition: String,
    pub outcome: ChaosTrialOutcome,
    pub experience_id: ExperienceId,
    pub trial_id: Option<CurriculumTrialId>,
    pub observed_at: DateTime<Utc>,
    pub fingerprint: String,
    pub severity: Severity,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "target", rename_all = "snake_case")]
pub enum CurriculumTarget {
    Skill(SkillId),
    Lesson(LessonId),
    Agent(AgentIdentity),
    Repository(RepositoryContext),
    TaskFamily(TaskFamilyId),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurriculumStatus {
    Planned,
    Running,
    Completed,
    PartiallyCompleted,
    Cancelled,
}
impl CurriculumStatus {
    pub fn terminal(self) -> bool {
        !matches!(self, Self::Planned | Self::Running)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurriculumGoalKind {
    ValidateSkill,
    DiscoverFailureBoundary,
    TestRecovery,
    ValidateReflex,
    ExploreUnknownCondition,
    RevalidateOldExperience,
    ResolveContradiction,
    MinimizeCapability,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Planned,
    Running,
    Completed,
    Deferred,
    Rejected,
    Inconclusive,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Informational,
    Low,
    Medium,
    High,
    Critical,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    Medium,
    High,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PriorityScore {
    pub score: u64,
    pub priority: Priority,
    pub explanation: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvidenceGap {
    pub dimension: String,
    pub known_values: Vec<String>,
    pub unknown_values: Vec<String>,
    pub rationale: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CurriculumGoal {
    pub id: CurriculumGoalId,
    pub kind: CurriculumGoalKind,
    pub description: String,
    pub priority: Priority,
    pub score: PriorityScore,
    pub evidence_gap: EvidenceGap,
    pub status: GoalStatus,
    pub decision: CurriculumDecision,
    pub reason: String,
    pub severity: Severity,
    pub safety: TrialSafety,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrialIntent {
    NovelExploration,
    Replication,
    Revalidation,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurriculumDecision {
    Approved,
    Reduced,
    RequiresApproval,
    Rejected,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrialSafety {
    Safe,
    RequiresIsolation,
    RequiresApproval,
    Unsupported,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationLevel {
    Shared,
    Partial,
    Isolated,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RealityCapabilities {
    pub filesystem_isolation: IsolationLevel,
    pub process_isolation: IsolationLevel,
    pub network_isolation: IsolationLevel,
    pub external_effect_isolation: IsolationLevel,
    #[serde(default)]
    pub external_effects: crate::effects::ExternalEffectCapabilities,
}
impl Default for RealityCapabilities {
    fn default() -> Self {
        Self {
            filesystem_isolation: IsolationLevel::Partial,
            process_isolation: IsolationLevel::Shared,
            network_isolation: IsolationLevel::Shared,
            external_effect_isolation: IsolationLevel::Shared,
            external_effects: Default::default(),
        }
    }
}

/// Dispatch to existing engines. This is a plan, not a new process/trial runner.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "engine", rename_all = "snake_case")]
pub enum TrialExecution {
    Experiment {
        request: Box<crate::experimentation::ExperimentRequest>,
    },
    Chaos {
        plan: Box<CampaignPlan>,
    },
    Recovery {
        recovery_id: RecoveryId,
        version: u32,
    },
    Reflex {
        reflex_id: ReflexId,
        version: u32,
        conditions: Vec<Perturbation>,
    },
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TrialEvidence {
    pub experiment_id: Option<ExperimentId>,
    pub campaign_id: Option<ChaosCampaignId>,
    pub resilience_test_id: Option<ResilienceTestId>,
    pub experiences: Vec<ExperienceId>,
    pub outcome: Option<ChaosTrialOutcome>,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CurriculumTrial {
    pub id: CurriculumTrialId,
    pub goal_id: CurriculumGoalId,
    pub skill_id: SkillId,
    pub condition: String,
    pub fingerprint: String,
    pub intent: TrialIntent,
    pub execution: TrialExecution,
    pub result: Option<TrialEvidence>,
    pub learning_outcome: Option<LearningOutcome>,
    pub status: GoalStatus,
    pub estimated_budget: ExperienceUsage,
    pub expected_value: String,
    pub required_isolation: RealityCapabilities,
    pub round: usize,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LearningOutcome {
    pub new_experiences: Vec<ExperienceId>,
    pub lessons_created: Vec<LessonId>,
    pub lessons_updated: Vec<LessonId>,
    pub reflexes_created: Vec<ReflexId>,
    pub recoveries_created: Vec<RecoveryId>,
    pub envelope_updates: Vec<OperatingEnvelopeId>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Curriculum {
    pub id: CurriculumId,
    pub target: CurriculumTarget,
    pub profile: String,
    pub goals: Vec<CurriculumGoal>,
    pub trials: Vec<CurriculumTrial>,
    pub budget: ExperienceBudget,
    pub usage: ExperienceUsage,
    pub reserved: ExperienceUsage,
    pub trials_executed: usize,
    pub status: CurriculumStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub rounds: usize,
    pub max_rounds: usize,
    pub revision: u32,
    pub before: Vec<ExperiencePackage>,
    pub after: Vec<ExperiencePackage>,
    pub stop_reason: Option<String>,
    pub session_id: Option<String>,
    pub quality: CurriculumQuality,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurriculumQuality {
    High,
    Medium,
    Low,
    Invalid,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UsageStatistics {
    pub execution_count: u64,
    pub recent_execution_count: u64,
    pub failure_count: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskFamily {
    pub id: TaskFamilyId,
    pub name: String,
    pub selector: ContextSelector,
    pub examples: Vec<ExperienceId>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvidenceFreshness {
    pub last_supported_at: DateTime<Utc>,
    pub environment_version: Option<String>,
    pub stale: bool,
    pub reasons: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillEvidenceSummary {
    pub usage: UsageStatistics,
    pub base_successes: usize,
    pub base_failed: bool,
    pub tested_dimensions: usize,
    pub unresolved_critical: usize,
    pub high_failure_recovery_gaps: Vec<String>,
    pub reflex_check_gaps: Vec<ReflexId>,
    pub freshness: EvidenceFreshness,
}
/// Items remain local IDs resolving to immutable Experiences and versioned artifacts.
/// No import/trust semantics are implied by serialization.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperiencePackage {
    pub skill: SkillId,
    pub operating_envelope: Option<OperatingEnvelopeId>,
    pub operating_envelopes: Vec<OperatingEnvelopeId>,
    pub lessons: Vec<LessonId>,
    pub reflexes: Vec<ReflexId>,
    pub recoveries: Vec<RecoveryId>,
    pub coverage: SkillCoverage,
    pub maturity: SkillMaturity,
    pub evidence: SkillEvidenceSummary,
    pub provenance: Vec<PackageProvenance>,
    pub generated_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackageProvenance {
    pub kind: String,
    pub id: String,
    pub version: Option<u32>,
    pub evidence: Vec<crate::lesson::EvidenceRef>,
}
#[derive(Clone, Debug)]
pub struct CurriculumContext {
    pub experiments: Vec<crate::experiment::Experiment>,
    pub skills: Vec<Skill>,
    pub experiences: Vec<Experience>,
    pub lessons: Vec<Lesson>,
    pub envelopes: Vec<OperatingEnvelope>,
    pub reflexes: Vec<Reflex>,
    pub recoveries: Vec<Recovery>,
    pub tests: Vec<ResilienceTest>,
    pub history: Vec<Curriculum>,
    pub packages: Vec<ExperiencePackage>,
    pub profile: super::PerturbationProfile,
    pub capabilities: RealityCapabilities,
    pub now: DateTime<Utc>,
    pub config: super::CurriculumConfig,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CurriculumSuggestion {
    pub condition: String,
    pub rationale: String,
}
pub trait CurriculumSuggestionProvider {
    fn suggest(&self, context: &CurriculumContext) -> crate::Result<Vec<CurriculumSuggestion>>;
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CurriculumRecommendation {
    pub target: CurriculumTarget,
    pub gaps: Vec<EvidenceGap>,
    pub rationale: String,
    pub auto_run: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CurriculumEvent {
    pub event: String,
    pub curriculum_id: CurriculumId,
    pub trial_id: Option<CurriculumTrialId>,
    pub message: String,
    pub created_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Default)]
pub struct CurriculumQuery {
    pub session_id: Option<String>,
}
