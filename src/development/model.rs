// SPDX-License-Identifier: Apache-2.0
use crate::{
    core::*,
    curriculum::{LearningOutcome, SkillCoverage, SkillMaturity},
    experience::{ExperienceContext, Outcome},
    lesson::{ActionPattern, ContextSelector, EvidenceRef},
    resilience::RecoveryAttempt,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ProfileScope {
    LocalStore,
    Repository(PathBuf),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSubject {
    pub agent_kind: String,
    pub agent_version: Option<String>,
    pub model: Option<String>,
    /// A measured environment/configuration fingerprint, not a free-form claim.
    pub configuration: Option<String>,
    pub profile_scope: ProfileScope,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ExperienceSubject {
    Agent(AgentSubject),
    Repository(PathBuf),
    TaskFamily(TaskFamilyId),
    SharedLocal,
    Workspace(PathBuf),
    OrganizationScope(String),
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ProfileWindow {
    #[default]
    AllTime,
    Since(DateTime<Utc>),
    LastDays(u32),
    LastExperiences(u64),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricConfidence {
    InsufficientEvidence,
    Low,
    Medium,
    High,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MetricValue {
    pub value: Option<f64>,
    pub numerator: Option<u64>,
    pub sample_count: u64,
    pub period: ProfileWindow,
    pub confidence: MetricConfidence,
    pub definition: String,
}
impl MetricValue {
    pub fn ratio(n: u64, d: u64, period: &ProfileWindow, definition: &str) -> Self {
        Self {
            value: (d > 0).then(|| n as f64 / d as f64),
            numerator: (d > 0).then_some(n),
            sample_count: d,
            period: period.clone(),
            confidence: if d == 0 {
                MetricConfidence::InsufficientEvidence
            } else if d < 20 {
                MetricConfidence::Low
            } else {
                MetricConfidence::Medium
            },
            definition: definition.into(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DevelopmentMetricKind {
    TaskSuccessRate,
    RepeatedMistakeRate,
    RepeatedFailureRate,
    RecoverySuccessRate,
    ExperienceTransferRate,
    LessonPrecision,
    ReflexFalsePositiveRate,
    ExperimentSuccessRate,
    CurriculumYield,
    ExperiencePortabilityRate,
}
impl DevelopmentMetricKind {
    pub const ALL: [Self; 10] = [
        Self::TaskSuccessRate,
        Self::RepeatedMistakeRate,
        Self::RepeatedFailureRate,
        Self::RecoverySuccessRate,
        Self::ExperienceTransferRate,
        Self::LessonPrecision,
        Self::ReflexFalsePositiveRate,
        Self::ExperimentSuccessRate,
        Self::CurriculumYield,
        Self::ExperiencePortabilityRate,
    ];
    pub fn lower_is_better(self) -> bool {
        matches!(
            self,
            Self::RepeatedMistakeRate | Self::RepeatedFailureRate | Self::ReflexFalsePositiveRate
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DevelopmentMetrics {
    pub task_success_rate: MetricValue,
    pub repeated_mistake_rate: MetricValue,
    pub repeated_failure_rate: MetricValue,
    pub recovery_success_rate: MetricValue,
    pub median_time_to_recovery_ms: Option<u64>,
    pub recovery_latency_samples: u64,
    pub experience_transfer_rate: MetricValue,
    pub lesson_precision: MetricValue,
    pub reflex_false_positive_rate: MetricValue,
    pub experiment_success_rate: MetricValue,
    pub curriculum_yield: MetricValue,
    pub hardened_skill_count: u64,
    pub experience_portability_rate: MetricValue,
}
impl DevelopmentMetrics {
    pub fn metric(&self, k: DevelopmentMetricKind) -> &MetricValue {
        match k {
            DevelopmentMetricKind::TaskSuccessRate => &self.task_success_rate,
            DevelopmentMetricKind::RepeatedMistakeRate => &self.repeated_mistake_rate,
            DevelopmentMetricKind::RepeatedFailureRate => &self.repeated_failure_rate,
            DevelopmentMetricKind::RecoverySuccessRate => &self.recovery_success_rate,
            DevelopmentMetricKind::ExperienceTransferRate => &self.experience_transfer_rate,
            DevelopmentMetricKind::LessonPrecision => &self.lesson_precision,
            DevelopmentMetricKind::ReflexFalsePositiveRate => &self.reflex_false_positive_rate,
            DevelopmentMetricKind::ExperimentSuccessRate => &self.experiment_success_rate,
            DevelopmentMetricKind::CurriculumYield => &self.curriculum_yield,
            DevelopmentMetricKind::ExperiencePortabilityRate => &self.experience_portability_rate,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceState {
    Fresh,
    Aging,
    Stale,
    Superseded,
    Contradicted,
    Retired,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExperienceHealth {
    pub fresh: u64,
    pub aging: u64,
    pub stale: u64,
    pub superseded: u64,
    pub contradicted: u64,
    pub retired: u64,
    pub needs_revalidation: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperienceRef {
    pub kind: String,
    pub id: String,
    pub revision: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceTrust {
    LocalObserved,
    LocalExperiment,
    Imported,
    UnverifiedExternal,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperienceScope {
    Session,
    Agent,
    Repository,
    Workspace,
    Shared,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArtifactSummary {
    pub item: ExperienceRef,
    pub scope: ExperienceScope,
    pub status: String,
    pub confidence: Option<f64>,
    pub context: ContextSelector,
    pub source_experience: ExperienceId,
    pub origin_agent: AgentIdentity,
    pub state: EvidenceState,
    pub last_supported_at: DateTime<Utc>,
    pub reasons: Vec<String>,
    pub trust: EvidenceTrust,
}
pub type LessonSummary = ArtifactSummary;
pub type ReflexSummary = ArtifactSummary;
pub type RecoverySummary = ArtifactSummary;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillSummary {
    pub skill_id: SkillId,
    pub revision: u64,
    pub name: String,
    pub maturity: SkillMaturity,
    pub source_experience: ExperienceId,
    pub coverage: SkillCoverage,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExperienceCoverage {
    pub skills: Vec<(SkillId, SkillCoverage)>,
    pub known_unknowns: Vec<String>,
    pub note: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapabilityProfile {
    pub task_family: TaskFamilyId,
    pub matching_tasks: u64,
    pub task_success: MetricValue,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LearningEfficiency {
    pub artifact: ExperienceRef,
    pub experiences_to_validation: Option<u64>,
    pub definition: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperienceProfile {
    pub id: ExperienceProfileId,
    pub subject: ExperienceSubject,
    pub window: ProfileWindow,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub experience_count: u64,
    pub task_count: u64,
    pub skills: Vec<SkillSummary>,
    pub lessons: Vec<LessonSummary>,
    pub reflexes: Vec<ReflexSummary>,
    pub recoveries: Vec<RecoverySummary>,
    pub capabilities: Vec<CapabilityProfile>,
    pub metrics: DevelopmentMetrics,
    pub coverage: ExperienceCoverage,
    pub freshness: ExperienceHealth,
    pub efficiency: Vec<LearningEfficiency>,
    pub evidence_ids: Vec<ExperienceId>,
    pub policy_versions: BTreeMap<String, String>,
    pub policy_hash: String,
    pub contributing_agents: Vec<AgentIdentity>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExperienceArtifactCounts {
    pub skills: u64,
    pub lessons: u64,
    pub reflexes: u64,
    pub recoveries: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProfileSnapshot {
    pub id: ProfileSnapshotId,
    pub profile_id: ExperienceProfileId,
    pub subject: ExperienceSubject,
    pub captured_at: DateTime<Utc>,
    pub window: ProfileWindow,
    pub metrics: DevelopmentMetrics,
    pub coverage: ExperienceCoverage,
    pub artifact_counts: ExperienceArtifactCounts,
    pub evidence_ids: Vec<ExperienceId>,
    pub policy_hash: String,
    pub policy_versions: BTreeMap<String, String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricTrend {
    Improving,
    Stable,
    Regressing,
    InsufficientEvidence,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MetricComparison {
    pub metric: DevelopmentMetricKind,
    pub previous: MetricValue,
    pub current: MetricValue,
    pub delta: Option<f64>,
    pub trend: MetricTrend,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DevelopmentRegression {
    pub metric: DevelopmentMetricKind,
    pub previous: MetricValue,
    pub current: MetricValue,
    pub detected_at: DateTime<Utc>,
    pub recommendation: String,
    pub auto_run: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GrowthReport {
    pub from: ProfileSnapshotId,
    pub to: ProfileSnapshotId,
    pub comparisons: Vec<MetricComparison>,
    pub regressions: Vec<DevelopmentRegression>,
    pub median_recovery_ms: NumericChange,
    pub hardened_skills: NumericChange,
    pub note: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NumericChange {
    pub previous: Option<f64>,
    pub current: Option<f64>,
    pub delta: Option<f64>,
    pub previous_samples: u64,
    pub current_samples: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DevelopmentEpisode {
    pub id: DevelopmentEpisodeId,
    pub name: String,
    pub subject: ExperienceSubject,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub task_family: Option<TaskFamilyId>,
    pub experiences: Vec<ExperienceId>,
    pub learning_artifacts: LearningOutcome,
    pub profile_before: Option<ProfileSnapshotId>,
    pub profile_after: Option<ProfileSnapshotId>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillRevision {
    pub skill_id: SkillId,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    pub procedure: Vec<ActionPattern>,
    pub context: ContextSelector,
    pub evidence: Vec<EvidenceRef>,
    pub parent_revision: Option<u64>,
    pub source_experience: ExperienceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavioral_contract: Option<crate::assurance::BehavioralContractRef>,
}
pub type LessonRevision = crate::lesson::Lesson;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperiencePackageRevision {
    pub package_id: ExperiencePackageId,
    pub skill_id: SkillId,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    pub skill_revision: u64,
    pub items: Vec<ExperienceRef>,
    pub package: crate::curriculum::ExperiencePackage,
    pub evidence_hash: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevalidationReason {
    Stale,
    EnvironmentChanged,
    Contradicted,
    AgentRejected,
    LowConfidence,
    ScopeChanged,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RevalidationItem {
    pub id: RevalidationId,
    pub item: ExperienceRef,
    pub reason: RevalidationReason,
    pub explanation: String,
    pub context: ExperienceContext,
    pub created_at: DateTime<Utc>,
    pub status: String,
    pub experiment_id: Option<ExperimentId>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MaintenanceReport {
    pub health: ExperienceHealth,
    pub revalidation: Vec<RevalidationItem>,
    pub possible_duplicates: Vec<Vec<LessonId>>,
    pub auto_run: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TimelineEvent {
    pub at: DateTime<Utc>,
    pub kind: String,
    pub id: String,
    pub revision: Option<u64>,
    pub experience_id: Option<ExperienceId>,
    pub description: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ActiveExperienceSet {
    pub lessons: Vec<crate::retrieval::RetrievedLesson>,
    pub reflexes: Vec<ExperienceRef>,
    pub recoveries: Vec<ExperienceRef>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperienceContextBundle {
    pub relevant: ActiveExperienceSet,
    pub known_unknowns: Vec<String>,
    pub stale_items: Vec<ExperienceRef>,
    pub contradictions: Vec<ExperienceRef>,
    pub recommendations: Vec<String>,
    pub auto_run: bool,
}

/// Compact SQL projection of canonical Experience JSON. It excludes raw outputs,
/// artifact paths, environment values, commands and transcripts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DevelopmentObservation {
    pub id: ExperienceId,
    pub created_at: DateTime<Utc>,
    pub agent: AgentIdentity,
    pub context: ExperienceContext,
    pub outcome: Outcome,
    pub goal: String,
    pub tree_hash: String,
    pub task: bool,
    pub perturbed: bool,
    pub audited: bool,
    pub repeated_mistake: bool,
    pub failure_signatures: Vec<String>,
    pub applications: Vec<ApplicationObservation>,
    pub recovery: Option<RecoveryAttempt>,
    pub reflex_firings: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApplicationObservation {
    pub lesson_id: LessonId,
    pub lesson_version: u32,
    pub influence: crate::application::LessonInfluence,
    pub verification: crate::application::ApplicationVerification,
    pub delivered: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BenchmarkTask {
    pub episode: u32,
    pub index: u32,
    pub arm: String,
    pub task_kind: String,
    pub experience_id: ExperienceId,
    pub success: bool,
    pub repeated_mistake: bool,
    pub repeated_failure: bool,
    pub recovery_attempted: bool,
    pub recovery_succeeded: bool,
    pub time_to_recovery_ms: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BenchmarkResult {
    pub id: BenchmarkRunId,
    pub created_at: DateTime<Utc>,
    pub status: String,
    pub metadata: serde_json::Value,
    pub tasks: Vec<BenchmarkTask>,
    pub metrics: serde_json::Value,
    pub stale_rule: serde_json::Value,
    pub portability: serde_json::Value,
    pub profiles: Vec<ExperienceProfileId>,
    pub snapshots: Vec<ProfileSnapshotId>,
    pub stop_reason: Option<String>,
}
