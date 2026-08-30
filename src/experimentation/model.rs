// SPDX-License-Identifier: Apache-2.0
use crate::{
    budget::{ExperienceBudget, ExperienceUsage},
    core::{
        AgentIdentity, ArtifactRef, CandidateId, ExperienceId, ExperimentId, ExperimentRequestId,
        LessonId, ProcessStatus, RealityId, StateRef,
    },
    evaluation::{Evaluation, EvaluationSpec},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

/// Uses the existing Bridge's opaque, namespaced session identifiers.
pub type HardknockSessionId = String;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentRequest {
    pub id: ExperimentRequestId,
    pub session_id: HardknockSessionId,
    pub question: String,
    #[serde(default)]
    pub hypothesis: Option<String>,
    pub candidates: Vec<ExperimentCandidate>,
    pub starting_state: ExperimentStartingState,
    pub evaluator: EvaluationSpec,
    #[serde(default)]
    pub budget: ExperienceBudget,
    pub requested_by: AgentIdentity,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub criteria: ComparisonCriteria,
    #[serde(default)]
    pub origin: ExperimentOrigin,
    #[serde(default)]
    pub intent: ExperimentIntent,
    #[serde(default)]
    pub capabilities: ExperimentCapabilities,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentCandidate {
    pub id: CandidateId,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub execution: CandidateExecution,
    #[serde(default)]
    pub expected_outcome: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CandidateExecution {
    Shell {
        commands: Vec<String>,
    },
    AgentTask {
        prompt: String,
        #[serde(default)]
        agent: Option<AgentIdentity>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentStartingState {
    pub state_ref: StateRef,
    #[serde(default)]
    pub expected_fingerprint: Option<String>,
    #[serde(default)]
    pub parent_reality: Option<RealityId>,
    #[serde(default)]
    pub source: SnapshotSource,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotSource {
    #[default]
    RepositoryCommit,
    /// No live process, conversation, ignored files, or uncommitted edits are cloned.
    SessionCommitFallback,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartingStateProof {
    pub state_ref: StateRef,
    pub fingerprint: String,
    pub environment_fingerprint: String,
    /// Fixed runner and configured executor hashes, normalized independently of worktree path.
    pub runtime_fingerprints: BTreeMap<String, String>,
    pub scope: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ComparisonCriteria {
    pub require_success: bool,
    pub minimize_duration: bool,
    pub minimize_diff_size: bool,
    /// Reserved for explicit future policies; nonempty values are rejected in V0.4.
    pub custom_checks: Vec<String>,
}
impl Default for ComparisonCriteria {
    fn default() -> Self {
        Self {
            require_success: true,
            minimize_duration: false,
            minimize_diff_size: false,
            custom_checks: vec![],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentOrigin {
    #[default]
    User,
    Agent,
    ChaosEngine,
    LessonValidation,
    ReflexValidation,
    FederationReproduction,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentIntent {
    ResolveUncertainty,
    #[default]
    CompareStrategies,
    ValidateHypothesis,
    TestLesson,
    ValidateRecovery,
    MapBoundary,
    ReproduceFederatedExperience,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentQuality {
    Controlled,
    PartiallyControlled,
    Confounded,
    Invalid,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExperimentCapabilities {
    /// Empty means the managed Reality only. Arbitrary paths are unsupported.
    pub filesystem_scope: Vec<PathBuf>,
    pub allow_network: bool,
    pub allow_external_mutations: bool,
    /// Explicit effect declarations, such as send_email, cloud_mutation, git_push, payment.
    pub external_effects: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperimentVariable {
    pub name: String,
    pub values: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DiffSummary {
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub binary_files: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CandidateResult {
    pub candidate_id: CandidateId,
    pub name: String,
    pub reality_id: RealityId,
    pub experience_id: ExperienceId,
    pub execution_status: ProcessStatus,
    pub evaluation: Evaluation,
    pub diff_summary: Option<DiffSummary>,
    pub duration_ms: u64,
    pub artifacts: Vec<ArtifactRef>,
    pub starting_fingerprint: String,
    pub agent: AgentIdentity,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperimentComparison {
    pub policy: String,
    pub recommendation: Option<CandidateId>,
    pub reasons: Vec<String>,
    /// Qualitative indicator, never a calibrated probability or a universal causal claim.
    pub evidence_weight: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperimentResult {
    pub experiment_id: ExperimentId,
    pub question: String,
    pub candidates: Vec<CandidateResult>,
    pub comparison: ExperimentComparison,
    pub recommendation: Option<CandidateId>,
    pub confidence: Option<f64>,
    pub created_experience: Vec<ExperienceId>,
    pub candidate_lessons: Vec<LessonId>,
    pub quality: ExperimentQuality,
    pub changed_variables: Vec<ExperimentVariable>,
    pub starting_state: Option<StartingStateProof>,
    pub usage: ExperienceUsage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentStatus {
    Accepted,
    Running,
    Completed,
    Cancelled,
    Rejected,
    Failed,
}
impl ExperimentStatus {
    pub fn terminal(self) -> bool {
        !matches!(self, Self::Accepted | Self::Running)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StrategyExperiment {
    pub id: ExperimentId,
    pub request: ExperimentRequest,
    pub effective_budget: ExperienceBudget,
    pub status: ExperimentStatus,
    pub result: Option<ExperimentResult>,
    pub failure: Option<String>,
    pub notices: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentPhase {
    Preparing,
    Executing,
    Evaluating,
    Comparing,
    Learning,
    Completed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperimentProgress {
    pub experiment_id: ExperimentId,
    pub candidate: Option<CandidateId>,
    pub phase: ExperimentPhase,
    pub message: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentRelationType {
    Replay,
    Extension,
    Revalidation,
    Counterfactual,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperimentRelation {
    pub parent: ExperimentId,
    pub child: ExperimentId,
    pub relation: ExperimentRelationType,
}

/// Stored inside the immutable Experience, before its first insertion.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperimentEvidence {
    pub experiment_id: ExperimentId,
    pub candidate_id: CandidateId,
    pub starting_fingerprint: String,
}
