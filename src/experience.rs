// SPDX-License-Identifier: Apache-2.0

use crate::{
    Result,
    core::{
        ActionRecord, AgentIdentity, ArtifactRef, EnvironmentMode, ExecutionId, ExperienceId,
        ProcessStatus, RealityId, StateRef,
    },
    evaluation::{CheckStatus, Evaluation, EvaluationStatus},
    store::artifact,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

pub const MARKERS: &[&str] = &[
    "package.json",
    "pnpm-workspace.yaml",
    "Cargo.toml",
    "go.mod",
    "pyproject.toml",
    "requirements.txt",
    "pom.xml",
    "build.gradle",
    "hardknock-fixture.json",
];

pub fn controlled_environment(root: &Path) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("LANG".into(), "C".into()),
        ("LC_ALL".into(), "C".into()),
        ("TZ".into(), "UTC".into()),
        ("HOME".into(), root.to_string_lossy().into()),
        ("PWD".into(), root.to_string_lossy().into()),
    ])
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentContext {
    pub os: String,
    pub arch: String,
    pub cwd: PathBuf,
    pub mode: EnvironmentMode,
    pub facts: BTreeMap<String, String>,
    pub fingerprint: String,
}

impl EnvironmentContext {
    pub fn capture(root: &Path, mode: EnvironmentMode) -> Result<Self> {
        // Never persist arbitrary inherited values, which may contain credentials.
        let mut facts = if mode == EnvironmentMode::Controlled {
            controlled_environment(Path::new("$REALITY"))
        } else {
            BTreeMap::new()
        };
        facts.insert(
            "shell_blake3".into(),
            artifact(Path::new("/bin/sh"))?.blake3,
        );
        facts.insert(
            "environment_policy".into(),
            if mode == EnvironmentMode::Controlled {
                "controlled-v1"
            } else {
                "inherited-unverified"
            }
            .into(),
        );
        let os = std::env::consts::OS.to_owned();
        let arch = std::env::consts::ARCH.to_owned();
        let fingerprint = blake3::hash(&serde_json::to_vec(&(&os, &arch, &facts))?)
            .to_hex()
            .to_string();
        Ok(Self {
            os,
            arch,
            cwd: root.into(),
            mode,
            facts,
            fingerprint,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryContext {
    pub path: PathBuf,
    pub name: String,
    pub commit: String,
    pub branch: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperienceContext {
    pub repository: RepositoryContext,
    pub environment: EnvironmentContext,
    pub markers: Vec<String>,
    pub tags: Vec<String>,
}

impl ExperienceContext {
    pub fn capture(state: &StateRef, root: &Path, mode: EnvironmentMode) -> Result<Self> {
        let markers: Vec<String> = MARKERS
            .iter()
            .filter(|name| root.join(name).is_file())
            .map(|s| (*s).into())
            .collect();
        let mut tags: Vec<String> = markers.iter().map(|m| format!("marker:{m}")).collect();
        if let Ok(fixture) = fixture_metadata(root)
            && let Some(kind) = fixture["kind"].as_str()
            && matches!(
                kind,
                "pnpm-workspace-conflict"
                    | "pnpm-workspace-transfer"
                    | "pnpm-workspace-contradiction"
                    | "npm-ordinary"
                    | "retry-resilience"
                    | "stale-credential"
                    | "config-drift"
            )
        {
            tags.push(format!("fixture-kind:{kind}"));
            if fixture["version"] == 2
                && matches!(
                    kind,
                    "pnpm-workspace-conflict"
                        | "pnpm-workspace-transfer"
                        | "pnpm-workspace-contradiction"
                )
            {
                tags.push("fixture-family:pnpm-workspace-v2".into());
            }
        }
        Ok(Self {
            repository: RepositoryContext {
                path: state.repo_path.clone(),
                name: state
                    .repo_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into(),
                commit: state.git_commit.clone(),
                branch: None,
            },
            environment: EnvironmentContext::capture(root, mode)?,
            tags,
            markers,
        })
    }
}

/// Fixture metadata is small, local, and never read through a repository symlink.
pub fn fixture_metadata(root: &Path) -> Result<serde_json::Value> {
    let path = root.join("hardknock-fixture.json");
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 65536 {
        return Err(crate::Error::InvalidInput(
            "Fixture marker must be a regular file of at most 64 KiB".into(),
        ));
    }
    let mut bytes = Vec::new();
    File::open(path)?.take(65537).read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err(crate::Error::InvalidInput(
            "Fixture marker exceeded 64 KiB".into(),
        ));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    Failure,
    Inconclusive,
    Interrupted,
    TimedOut,
}

impl Outcome {
    pub fn from_evaluation(evaluation: &Evaluation) -> Self {
        match evaluation.status {
            EvaluationStatus::NotConfigured => Self::Inconclusive,
            EvaluationStatus::Interrupted => Self::Interrupted,
            EvaluationStatus::TimedOut => Self::TimedOut,
            EvaluationStatus::Completed if evaluation.success => Self::Success,
            EvaluationStatus::Completed => Self::Failure,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplaySpec {
    pub script: String,
    pub timeout_secs: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Perturbation {
    ReplaceCommand {
        from: String,
        to: String,
    },
    Local {
        perturbation_id: crate::core::PerturbationId,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureSource {
    Evaluator,
    AgentOutput,
    Rule,
    Manual,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FailureSignatureObservation {
    pub signature: String,
    pub source: SignatureSource,
    pub confidence: f64,
    pub artifacts: Vec<ArtifactRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvidenceBundle {
    pub artifacts: Vec<ArtifactRef>,
}

/// Immutable observation. Interpretations belong exclusively to Hypotheses/Lessons.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Experience {
    pub id: ExperienceId,
    pub created_at: DateTime<Utc>,
    pub goal: String,
    pub context: ExperienceContext,
    pub starting_state: StateRef,
    pub reality_id: RealityId,
    pub execution_id: ExecutionId,
    pub agent: AgentIdentity,
    pub actions: Vec<ActionRecord>,
    pub perturbations: Vec<Perturbation>,
    pub outcome: Outcome,
    pub evaluation: Evaluation,
    pub failure_signatures: Vec<FailureSignatureObservation>,
    pub evidence: EvidenceBundle,
    pub tags: Vec<String>,
    pub replay: Option<ReplaySpec>,
    #[serde(default)]
    pub lesson_applications: Vec<crate::application::LessonApplication>,
    #[serde(default)]
    pub relations: Vec<crate::application::ExperienceRelation>,
    #[serde(default)]
    pub repeated_mistakes: Vec<crate::application::RepeatedMistakeObservation>,
    #[serde(default)]
    pub observed_actions: Vec<crate::application::ObservedAction>,
    #[serde(default)]
    pub application_report_errors: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resilience: Option<crate::resilience::ResilienceObservation>,
}

impl Experience {
    pub fn exit_code(&self, process: ProcessStatus) -> u8 {
        match self.outcome {
            Outcome::Success => 0,
            Outcome::Failure | Outcome::TimedOut => 1,
            Outcome::Interrupted => 5,
            Outcome::Inconclusive => match process {
                ProcessStatus::Succeeded => 0,
                ProcessStatus::Interrupted => 5,
                _ => 1,
            },
        }
    }
}

/// Bounded, deterministic matching; never ingest unlimited process output into SQLite.
pub fn failure_signatures(
    evaluation: &Evaluation,
    agent: &ActionRecord,
) -> Result<Vec<FailureSignatureObservation>> {
    let mut found = Vec::new();
    for check in &evaluation.checks {
        if check.status == CheckStatus::Failed {
            found.push(FailureSignatureObservation {
                signature: "required_check_failed".into(),
                source: SignatureSource::Evaluator,
                confidence: 1.0,
                artifacts: check
                    .action
                    .iter()
                    .flat_map(|a| [a.stdout.clone(), a.stderr.clone()])
                    .collect(),
            });
        }
    }
    let sources = std::iter::once((agent, SignatureSource::AgentOutput)).chain(
        evaluation
            .checks
            .iter()
            .filter_map(|c| c.action.as_ref())
            .map(|a| (a, SignatureSource::Rule)),
    );
    for (action, source) in sources {
        for artifact in [&action.stdout, &action.stderr] {
            let mut bytes = Vec::new();
            File::open(&artifact.path)?
                .take(64 * 1024)
                .read_to_end(&mut bytes)?;
            let content = String::from_utf8_lossy(&bytes);
            for pattern in ["package_manager_conflict", "duplicate_lockfile"] {
                if content.contains(pattern) && !found.iter().any(|f| f.signature == pattern) {
                    found.push(FailureSignatureObservation {
                        signature: pattern.into(),
                        source,
                        confidence: 0.9,
                        artifacts: vec![artifact.clone()],
                    });
                }
            }
        }
    }
    Ok(found)
}
