// SPDX-License-Identifier: Apache-2.0

use crate::{
    Error, Result,
    cancellation::Cancellation,
    core::{
        ActionRecord, ArtifactKind, CommandSpec, EnvironmentMode, EvaluationId, ExecutionRecord,
        ProcessStatus, Reality,
    },
    process::ProcessRunner,
};
use serde::{Deserialize, Serialize};
use std::{future::Future, path::Path, time::Duration};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluationSpec {
    pub checks: Vec<String>,
}

impl EvaluationSpec {
    pub fn validate(&self) -> Result<()> {
        if self
            .checks
            .iter()
            .any(|s| s.trim().is_empty() || s.contains('\0'))
        {
            return Err(Error::InvalidInput(
                "Checks must be nonempty shell scripts without NUL bytes".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationStatus {
    Completed,
    NotConfigured,
    Interrupted,
    TimedOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Passed,
    Failed,
    Interrupted,
    TimedOut,
    NotRun,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    pub command: String,
    pub status: CheckStatus,
    pub action: Option<ActionRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Evaluation {
    pub id: EvaluationId,
    pub spec: EvaluationSpec,
    pub status: EvaluationStatus,
    pub success: bool,
    pub checks: Vec<CheckResult>,
    pub summary: String,
}

pub trait Evaluator {
    fn evaluate(
        &self,
        reality: &Reality,
        execution: &ExecutionRecord,
        artifacts: &Path,
        cancel: &Cancellation,
    ) -> impl Future<Output = Result<Evaluation>>;
}

pub struct CommandEvaluator {
    pub spec: EvaluationSpec,
    pub timeout: Duration,
    pub environment: EnvironmentMode,
}

impl Evaluator for CommandEvaluator {
    async fn evaluate(
        &self,
        reality: &Reality,
        execution: &ExecutionRecord,
        artifacts: &Path,
        cancel: &Cancellation,
    ) -> Result<Evaluation> {
        self.spec.validate()?;
        let mut status = match execution.status {
            ProcessStatus::Interrupted => EvaluationStatus::Interrupted,
            ProcessStatus::TimedOut => EvaluationStatus::TimedOut,
            _ if self.spec.checks.is_empty() => EvaluationStatus::NotConfigured,
            _ => EvaluationStatus::Completed,
        };
        let mut checks = Vec::new();
        for (index, script) in self.spec.checks.iter().enumerate() {
            if cancel.is_cancelled() {
                status = EvaluationStatus::Interrupted;
            }
            let (check_status, action) = if status != EvaluationStatus::Completed {
                (CheckStatus::NotRun, None)
            } else {
                let (result, mut action) = ProcessRunner
                    .run(
                        &CommandSpec::shell(script, self.environment),
                        &reality.root,
                        &artifacts.join(format!("check-{index}")),
                        self.timeout,
                        cancel.cancelled(),
                    )
                    .await?;
                action.stdout.kind = ArtifactKind::EvaluationOutput;
                action.stderr.kind = ArtifactKind::EvaluationOutput;
                let check_status = match result {
                    ProcessStatus::Succeeded => CheckStatus::Passed,
                    ProcessStatus::Failed => CheckStatus::Failed,
                    ProcessStatus::Interrupted => {
                        status = EvaluationStatus::Interrupted;
                        CheckStatus::Interrupted
                    }
                    ProcessStatus::TimedOut => {
                        status = EvaluationStatus::TimedOut;
                        CheckStatus::TimedOut
                    }
                };
                (check_status, Some(action))
            };
            checks.push(CheckResult {
                name: format!("check-{index}"),
                command: script.clone(),
                status: check_status,
                action,
            });
        }
        let success = status == EvaluationStatus::Completed
            && !checks.is_empty()
            && checks.iter().all(|c| c.status == CheckStatus::Passed);
        let summary = match status {
            EvaluationStatus::NotConfigured => {
                "No checks configured; task success is unknown".into()
            }
            EvaluationStatus::Interrupted => {
                "Evaluation interrupted; remaining checks were not run".into()
            }
            EvaluationStatus::TimedOut => {
                "Execution or check timed out; remaining checks were not run".into()
            }
            EvaluationStatus::Completed => format!(
                "{}/{} required checks passed",
                checks
                    .iter()
                    .filter(|c| c.status == CheckStatus::Passed)
                    .count(),
                checks.len()
            ),
        };
        Ok(Evaluation {
            id: EvaluationId::new(),
            spec: self.spec.clone(),
            status,
            success,
            checks,
            summary,
        })
    }
}
