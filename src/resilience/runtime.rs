// SPDX-License-Identifier: Apache-2.0
use super::{
    reflex::{DeterministicReflexMatcher, ReflexMatcher, fixture_action},
    *,
};
use crate::{
    Error, Result,
    cancellation::Cancellation,
    core::*,
    experience::{ExperienceContext, FailureSignatureObservation, SignatureSource},
    perturbation::{
        AppliedPerturbations, LocalPerturbationProvider, Perturbation, PerturbationProvider,
        scoped_path, validate_environment,
    },
    process::ProcessRunner,
};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::Path,
    time::{Duration, Instant},
};

#[derive(Clone, Default)]
pub struct RunResilienceOptions {
    pub origin: Option<ChaosOrigin>,
    pub perturbations: Vec<Perturbation>,
    pub fixture: Option<FixtureKind>,
    pub reflexes: Vec<Reflex>,
    pub testing_reflex: bool,
    pub recovery: Option<Recovery>,
    pub baseline: Option<TrialMetrics>,
}
pub struct RuntimeResult {
    pub status: ProcessStatus,
    pub actions: Vec<ActionRecord>,
    pub observation: ResilienceObservation,
    pub signatures: Vec<FailureSignatureObservation>,
    pub environment: BTreeMap<String, String>,
}
pub fn apply(reality: &Reality, options: &RunResilienceOptions) -> Result<AppliedPerturbations> {
    let mut handles = AppliedPerturbations::default();
    for perturbation in &options.perturbations {
        handles
            .0
            .push(LocalPerturbationProvider.apply(reality, perturbation)?);
    }
    Ok(handles)
}
fn read_small(root: &Path, name: &str) -> Result<Vec<u8>> {
    let path = scoped_path(root, Path::new(name))?;
    match fs::File::open(path) {
        Ok(file) => {
            let mut bytes = Vec::new();
            file.take(65537).read_to_end(&mut bytes)?;
            if bytes.len() > 65536 {
                return Err(Error::InvalidInput("Fixture state exceeded 64 KiB".into()));
            }
            Ok(bytes)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(e.into()),
    }
}
fn state_hash(root: &Path) -> Result<String> {
    let bytes = serde_json::to_vec(&[
        read_small(root, "generation")?,
        read_small(root, "plan-generation")?,
        read_small(root, "result")?,
        read_small(root, "token")?,
    ])?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}
fn config_changed(root: &Path, fixture: FixtureKind) -> Result<bool> {
    Ok(fixture == FixtureKind::ConfigDrift
        && read_small(root, "generation")? != read_small(root, "plan-generation")?)
}
fn signature(action: &ActionRecord) -> Result<Option<String>> {
    let mut bytes = Vec::new();
    fs::File::open(&action.stdout.path)?
        .take(65536)
        .read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).lines().find_map(|line| {
        line.strip_prefix("HK_SIGNATURE ")
            .filter(|s| {
                [
                    "retry_exhaustion",
                    "stale_credential",
                    "configuration_stale",
                    "transient_command_failure",
                ]
                .contains(s)
            })
            .map(str::to_owned)
    }))
}
struct Runner<'a> {
    root: &'a Path,
    artifacts: &'a Path,
    deadline: Instant,
    cancel: &'a Cancellation,
    actions: Vec<ActionRecord>,
}
impl Runner<'_> {
    async fn run(
        &mut self,
        mut command: CommandSpec,
        environment: &BTreeMap<String, String>,
    ) -> Result<ProcessStatus> {
        command.environment_overrides.extend(environment.clone());
        let (status, action) = ProcessRunner
            .run(
                &command,
                self.root,
                &self.artifacts.join(format!("agent-{}", self.actions.len())),
                self.deadline.saturating_duration_since(Instant::now()),
                self.cancel.cancelled(),
            )
            .await?;
        self.actions.push(action);
        Ok(status)
    }
    async fn script(
        &mut self,
        script: &str,
        environment: &BTreeMap<String, String>,
    ) -> Result<ProcessStatus> {
        self.run(
            CommandSpec::shell(script, EnvironmentMode::Controlled),
            environment,
        )
        .await
    }
}
pub async fn execute(
    reality: &Reality,
    context: &ExperienceContext,
    request: &crate::workflow::RunRequest,
    artifacts: &Path,
    cancel: &Cancellation,
    options: &RunResilienceOptions,
    handles: &AppliedPerturbations,
) -> Result<RuntimeResult> {
    let command = &request.command;
    let timeout = Duration::from_secs(request.timeout_secs);
    let started = Instant::now();
    let mut runner = Runner {
        root: &reality.root,
        artifacts,
        deadline: started + timeout,
        cancel,
        actions: vec![],
    };
    let mut environment = command.environment_overrides.clone();
    let mut delay = 0;
    let mut failures = 0;
    let mut fail_exit = 17;
    for handle in &handles.0 {
        environment.extend(handle.environment.clone());
        delay += handle.command_delay_ms;
        if let Some((n, code)) = handle.command_failure {
            failures += n;
            fail_exit = code;
        }
    }
    let mut metrics = TrialMetrics::default();
    let mut temporal = Vec::new();
    let mut reflex_matches = Vec::new();
    let mut recovery_attempt = None;
    let mut final_signature = None;
    let mut signature_artifacts = vec![];
    let mut status;
    if let Some(fixture) = options.fixture {
        let marker = crate::experience::fixture_metadata(&reality.root)?;
        if marker["kind"] != fixture.name()
            || marker["runtime"] != super::fixture::RUNTIME_VERSION
            || marker["version"] != 1
        {
            return Err(Error::Intervention(
                "Fixture runtime/version mismatch".into(),
            ));
        }
        environment.insert("HK_DELAY_MS".into(), delay.to_string());
        environment.insert("HK_FAILURES".into(), failures.to_string());
        environment.insert("HK_FAILURE_EXIT".into(), fail_exit.to_string());
        metrics.simulated_duration_ms = Some(0);
        let mut consecutive = 0;
        let mut unchanged = true;
        status = ProcessStatus::Failed;
        for attempt in 1..=6 {
            let action_context = ActionContext {
                context: context.clone(),
                proposed_action: fixture_action(),
                consecutive_failures: consecutive,
                no_state_change: unchanged,
                config_changed: config_changed(&reality.root, fixture)?,
                elapsed_ms: delay * metrics.attempts as u64,
                state_fingerprint: state_hash(&reality.root)?,
            };
            let mut candidates = options.reflexes.clone();
            if options.testing_reflex {
                for r in &mut candidates {
                    if r.status != ReflexStatus::Retired {
                        r.status = ReflexStatus::Active;
                    }
                }
            }
            let mut matched = DeterministicReflexMatcher.evaluate(&action_context, &candidates)?;
            for m in &mut matched {
                m.test_only = options.testing_reflex;
            }
            let replan = matched.iter().any(|m| m.response == ReflexResponse::Replan);
            reflex_matches.extend(matched);
            if replan {
                metrics.replans += 1;
                environment.insert("HK_ATTEMPT".into(), attempt.to_string());
                status = runner.script("/bin/sh ./replan.sh", &environment).await?;
                break;
            }
            let before = state_hash(&reality.root)?;
            environment.insert("HK_ATTEMPT".into(), attempt.to_string());
            status = runner
                .script("/bin/sh ./operation.sh", &environment)
                .await?;
            metrics.attempts += 1;
            metrics.retries = metrics.attempts.saturating_sub(1);
            metrics.simulated_duration_ms = Some(delay * metrics.attempts as u64);
            let after = state_hash(&reality.root)?;
            unchanged = before == after;
            if status == ProcessStatus::Failed {
                consecutive += 1;
                metrics.failed_attempts += 1;
            } else {
                consecutive = 0;
            }
            let action = runner.actions.last().expect("action was recorded");
            final_signature = signature(action)?;
            signature_artifacts = vec![action.stdout.clone(), action.stderr.clone()];
            temporal.push(TemporalObservation {
                attempt,
                failed: status == ProcessStatus::Failed,
                consecutive_failures: consecutive,
                no_state_change: unchanged,
                config_changed: config_changed(&reality.root, fixture)?,
                elapsed_ms: delay * metrics.attempts as u64,
                action: fixture_action(),
                artifacts: signature_artifacts.clone(),
                state_before: before,
                state_after: after,
            });
            if status == ProcessStatus::Failed && metrics.failure_detection_ms.is_none() {
                metrics.failure_detection_ms = Some(delay * metrics.attempts as u64);
            }
            if status != ProcessStatus::Failed {
                break;
            }
        }
        if let Some(recovery) = &options.recovery {
            let mut observation = RecoveryAttempt {
                recovery_id: recovery.id.clone(),
                recovery_version: recovery.version,
                reproduced_failure: false,
                failure_signature: final_signature.clone(),
                attempted: false,
                succeeded: false,
                time_to_recovery_ms: 0,
                steps_executed: 0,
            };
            if status == ProcessStatus::Failed
                && final_signature.as_deref() == Some(&recovery.failure_signature.signature)
                && recovery.context.matches(context)
            {
                // Verify a failed task in this exact Reality before attempting restoration.
                let precheck = runner.script("/bin/sh ./test.sh", &environment).await?;
                observation.reproduced_failure = precheck == ProcessStatus::Failed;
                if observation.reproduced_failure {
                    let recovery_start = Instant::now();
                    observation.attempted = true;
                    for step in &recovery.steps {
                        if cancel.is_cancelled() {
                            status = ProcessStatus::Interrupted;
                            break;
                        }
                        if Instant::now() >= runner.deadline {
                            status = ProcessStatus::TimedOut;
                            break;
                        }
                        observation.steps_executed += 1;
                        status = match step {
                            RecoveryStep::ShellCommand { command } => {
                                runner.run(command.clone(), &environment).await?
                            }
                            RecoveryStep::SetEnvironmentVariable { key, value } => {
                                validate_environment(key, value)?;
                                environment.insert(key.clone(), value.clone());
                                ProcessStatus::Succeeded
                            }
                            RecoveryStep::Replan => {
                                metrics.replans += 1;
                                runner.script("/bin/sh ./replan.sh", &environment).await?
                            }
                        };
                        if status != ProcessStatus::Succeeded {
                            break;
                        }
                    }
                    observation.time_to_recovery_ms = recovery_start.elapsed().as_millis() as u64;
                } else if matches!(
                    precheck,
                    ProcessStatus::Interrupted | ProcessStatus::TimedOut
                ) {
                    status = precheck;
                }
            }
            recovery_attempt = Some(observation);
        }
    } else {
        if !options.reflexes.is_empty() || options.recovery.is_some() {
            return Err(Error::InvalidInput(
                "Reflex/recovery hooks require the deterministic fixture runtime".into(),
            ));
        }
        let mut adjusted = command.clone();
        if failures > 0 {
            adjusted = CommandSpec::shell(
                &format!("printf '%s\\n' 'Injected local command failure'; exit {fail_exit}"),
                command.environment,
            );
        }
        if delay > 0 {
            let invocation = std::iter::once(&adjusted.program)
                .chain(&adjusted.args)
                .map(|s| shell_words::quote(s))
                .collect::<Vec<_>>()
                .join(" ");
            adjusted = CommandSpec::shell(
                &format!(
                    "/bin/sleep {}.{:03}; exec {invocation}",
                    delay / 1000,
                    delay % 1000
                ),
                command.environment,
            );
        }
        status = runner.run(adjusted, &environment).await?;
        metrics.attempts = 1;
        metrics.failed_attempts = u32::from(status == ProcessStatus::Failed);
    }
    metrics.duration_ms = started.elapsed().as_millis() as u64;
    if let Some(baseline) = &options.baseline {
        let values = if let Some(observed) = metrics.simulated_duration_ms {
            vec![(
                "simulated_duration_ms",
                baseline.simulated_duration_ms.unwrap_or(0) as f64,
                observed as f64,
                observed >= 1000,
            )]
        } else {
            vec![(
                "duration_ms",
                baseline.duration_ms as f64,
                metrics.duration_ms as f64,
                metrics.duration_ms >= baseline.duration_ms.saturating_mul(2).saturating_add(100),
            )]
        };
        for (metric, base, observed, degraded) in values {
            if degraded {
                metrics.degradations.push(DegradationObservation {
                    metric: metric.into(),
                    baseline: base,
                    observed,
                    ratio: (base > 0.0).then_some(observed / base),
                });
            }
        }
        if metrics.retries > baseline.retries {
            metrics.degradations.push(DegradationObservation {
                metric: "retries".into(),
                baseline: baseline.retries as f64,
                observed: metrics.retries as f64,
                ratio: (baseline.retries > 0)
                    .then_some(metrics.retries as f64 / baseline.retries as f64),
            });
        }
    }
    let signatures = final_signature
        .into_iter()
        .map(|signature| FailureSignatureObservation {
            signature,
            source: SignatureSource::Rule,
            confidence: 1.0,
            artifacts: signature_artifacts.clone(),
        })
        .collect();
    Ok(RuntimeResult {
        status,
        actions: runner.actions,
        observation: ResilienceObservation {
            origin: options.origin.clone(),
            perturbation_ids: options.perturbations.iter().map(|p| p.id.clone()).collect(),
            outcome: ChaosTrialOutcome::Inconclusive,
            metrics,
            temporal,
            reflex_matches,
            recovery_attempt,
        },
        signatures,
        environment,
    })
}

pub fn finish(result: &mut RuntimeResult, evaluation: &crate::evaluation::Evaluation) {
    result.observation.outcome = match (result.status, evaluation.status, evaluation.success) {
        (ProcessStatus::Interrupted | ProcessStatus::TimedOut, _, _)
        | (
            _,
            crate::evaluation::EvaluationStatus::Interrupted
            | crate::evaluation::EvaluationStatus::TimedOut
            | crate::evaluation::EvaluationStatus::NotConfigured,
            _,
        ) => ChaosTrialOutcome::Inconclusive,
        (ProcessStatus::Succeeded, _, true)
            if result.observation.metrics.degradations.is_empty() =>
        {
            ChaosTrialOutcome::Pass
        }
        (ProcessStatus::Succeeded, _, true) => ChaosTrialOutcome::Degraded,
        _ => ChaosTrialOutcome::Fail,
    };
    if let Some(attempt) = &mut result.observation.recovery_attempt {
        attempt.succeeded = attempt.attempted
            && attempt.reproduced_failure
            && result.status == ProcessStatus::Succeeded
            && evaluation.success;
    }
}
