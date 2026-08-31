// SPDX-License-Identifier: Apache-2.0

use std::{fmt, path::PathBuf, str::FromStr};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Error, Result};

macro_rules! identifier {
    ($name:ident, $prefix:literal) => {
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn new() -> Self {
                Self(format!("{}{}", $prefix, Uuid::new_v4()))
            }
        }
        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl FromStr for $name {
            type Err = Error;
            fn from_str(value: &str) -> Result<Self> {
                let valid = value
                    .strip_prefix($prefix)
                    .and_then(|s| Uuid::parse_str(s).ok());
                match valid {
                    Some(uuid) if value == format!("{}{}", $prefix, uuid) => Ok(Self(value.into())),
                    _ => Err(Error::InvalidInput(format!(
                        "Expected {}<canonical UUID>",
                        $prefix
                    ))),
                }
            }
        }
        impl TryFrom<String> for $name {
            type Error = Error;
            fn try_from(value: String) -> Result<Self> {
                value.parse()
            }
        }
        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

identifier!(RealityId, "r-");
identifier!(ExecutionId, "exec-");
identifier!(ExperienceId, "exp-");
identifier!(LessonId, "lesson-");
identifier!(ExperimentId, "experiment-");
identifier!(ExperimentRequestId, "request-");
identifier!(CandidateId, "candidate-");
identifier!(HypothesisId, "hypothesis-");
identifier!(TrialId, "trial-");
identifier!(EvaluationId, "eval-");
identifier!(ApplicationId, "application-");
identifier!(ReflexId, "reflex-");
identifier!(RecoveryId, "recovery-");
identifier!(PerturbationId, "perturb-");
identifier!(ChaosCampaignId, "chaos-");
identifier!(ChaosTrialId, "chaos-trial-");
identifier!(OperatingEnvelopeId, "envelope-");
identifier!(SkillId, "skill-");
identifier!(ResilienceTestId, "resilience-test-");
identifier!(CurriculumId, "curriculum-");
identifier!(CurriculumGoalId, "curriculum-goal-");
identifier!(CurriculumTrialId, "curriculum-trial-");
identifier!(TaskFamilyId, "task-family-");
identifier!(ExperienceProfileId, "profile-");
identifier!(ProfileSnapshotId, "profile-snapshot-");
identifier!(DevelopmentEpisodeId, "episode-");
identifier!(ExperiencePackageId, "package-");
identifier!(RevalidationId, "revalidation-");
identifier!(BenchmarkRunId, "benchmark-");
identifier!(FederatedObjectId, "federated-");
identifier!(FederatedConflictId, "conflict-");
identifier!(FederationAuditId, "federation-audit-");
identifier!(FederationReproductionId, "reproduction-");
identifier!(EffectId, "effect-");
identifier!(PreparedEffectId, "prepared-");
identifier!(CommitReceiptId, "receipt-");
identifier!(CompensationReceiptId, "compensation-");
identifier!(EffectLedgerId, "effect-ledger-");
identifier!(EffectEventId, "effect-event-");
identifier!(EffectPlanId, "effect-plan-");
identifier!(EffectGroupId, "effect-group-");
identifier!(EffectInvariantId, "effect-invariant-");
identifier!(CommitAuthorizationId, "authorization-");
identifier!(ExternalStateSnapshotId, "snapshot-");
identifier!(ReconciliationAttemptId, "reconcile-");
identifier!(CapabilityManifestId, "capability-manifest-");
identifier!(CapabilityEventId, "capability-event-");
identifier!(CapabilityTokenId, "capability-token-");
identifier!(CredentialId, "credential-");
identifier!(CapabilityEscalationId, "capability-escalation-");
identifier!(ToolId, "tool-");
identifier!(MicroSandboxId, "micro-sandbox-");
identifier!(ExecutionAttestationId, "attestation-");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateRef {
    pub repo_path: PathBuf,
    pub git_commit: String,
    pub tree_hash: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealityStatus {
    /// An external workspace observed by the Bridge, never owned or cleaned up by the Dojo.
    Observed,
    Created,
    Running,
    Completed,
    Failed,
    Discarded,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reality {
    pub id: RealityId,
    pub parent: Option<RealityId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_reason: Option<ForkReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment_id: Option<ExperimentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_id: Option<CandidateId>,
    /// The append-only external effect history associated with this Reality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_ledger: Option<EffectLedgerId>,
    /// Truthful execution-boundary metadata. Older Realities deserialize as a
    /// cooperative Git worktree rather than being upgraded to a stronger claim.
    #[serde(default)]
    pub execution_boundary: crate::capability::ExecutionBoundary,
    pub root: PathBuf,
    pub starting_state: StateRef,
    pub created_at: DateTime<Utc>,
    pub status: RealityStatus,
    /// Only automatic-run worktrees are eligible for orphan cleanup.
    pub ephemeral: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForkReason {
    Counterfactual,
    AgentExperiment,
    Chaos,
    Retry,
}

pub use crate::budget::ExperienceBudget;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub environment: EnvironmentMode,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub environment_overrides: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentMode {
    #[default]
    Inherited,
    Controlled,
}

impl CommandSpec {
    pub fn shell(script: &str, environment: EnvironmentMode) -> Self {
        Self {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            environment,
            environment_overrides: Default::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentIdentity {
    pub kind: String,
    pub executable: String,
    pub version: Option<String>,
    pub model: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub path: PathBuf,
    pub blake3: String,
    pub bytes: u64,
    #[serde(default)]
    pub kind: ArtifactKind,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Stdout,
    Stderr,
    Diff,
    EvaluationOutput,
    Metadata,
    #[default]
    Other,
}

impl ArtifactRef {
    pub fn with_kind(mut self, kind: ArtifactKind) -> Self {
        self.kind = kind;
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStatus {
    Succeeded,
    Failed,
    Interrupted,
    TimedOut,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionRecord {
    pub command: CommandSpec,
    pub cwd: PathBuf,
    pub started_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: ArtifactRef,
    pub stderr: ArtifactRef,
}

/// A process observation, not an evaluated task outcome or an Experience.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub id: ExecutionId,
    pub reality_id: RealityId,
    pub starting_state: StateRef,
    pub task: String,
    pub agent: AgentIdentity,
    pub status: ProcessStatus,
    pub action: ActionRecord,
    pub diff: ArtifactRef,
}

impl ExecutionRecord {
    pub fn exit_code(&self) -> u8 {
        match self.status {
            ProcessStatus::Succeeded => 0,
            ProcessStatus::Failed | ProcessStatus::TimedOut => 1,
            ProcessStatus::Interrupted => 5,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_typed_and_cannot_be_paths() {
        let id = RealityId::new();
        assert_eq!(id, id.to_string().parse().unwrap());
        assert!(id.to_string().parse::<LessonId>().is_err());
        assert!("r-../../somewhere".parse::<RealityId>().is_err());
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(id, serde_json::from_str::<RealityId>(&json).unwrap());
        assert!(serde_json::from_str::<RealityId>("\"../../bad\"").is_err());
    }

    #[test]
    fn states_have_stable_json_names() {
        assert_eq!(
            serde_json::to_string(&RealityStatus::Discarded).unwrap(),
            "\"discarded\""
        );
        assert_eq!(
            serde_json::to_string(&ProcessStatus::TimedOut).unwrap(),
            "\"timed_out\""
        );
    }
}
