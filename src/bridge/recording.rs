// SPDX-License-Identifier: Apache-2.0
//! Convert observable lifecycle evidence into the existing immutable Experience model.
use super::{
    config::BridgeConfig,
    engine::{RunRecord, Session},
    privacy::redact,
    protocol::EnvironmentSummary,
};
use crate::{
    Result,
    application::{
        ApplicationVerification, ExperienceRelation, LessonApplication, LessonInfluence,
        ObservedAction,
    },
    cancellation::Cancellation,
    core::*,
    evaluation::{CommandEvaluator, EvaluationSpec, Evaluator},
    experience::{EvidenceBundle, Experience, ExperienceContext, Outcome},
    lesson::ActionPattern,
    store::{ExperienceStore, Store, artifact},
};
use chrono::Utc;
use std::os::unix::process::CommandExt;
use std::{
    fs,
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "--no-pager",
            "-C",
        ])
        .arg(cwd)
        .args(args);
    // Bound synchronous Git capture too: no unbounded output allocation or stuck helper.
    let scratch = tempfile::NamedTempFile::new().ok()?;
    let mut child = command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(scratch.reopen().ok()?)
        .process_group(0)
        .spawn()
        .ok()?;
    let pid = nix::unistd::Pid::from_raw(child.id() as i32);
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Err(_) => break None,
            Ok(None) => {}
        }
        if Instant::now() >= deadline
            || scratch
                .as_file()
                .metadata()
                .map_or(true, |m| m.len() > 1024 * 1024)
        {
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let _ = nix::sys::signal::killpg(pid, nix::sys::signal::Signal::SIGKILL);
    let _ = child.wait();
    if !status?.success() || scratch.as_file().metadata().ok()?.len() > 1024 * 1024 {
        return None;
    }
    let mut bytes = Vec::new();
    scratch
        .reopen()
        .ok()?
        .take(1024 * 1024)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).trim_end().into())
}
pub fn capture_context(
    cwd: &Path,
    environment: &EnvironmentSummary,
) -> Result<(StateRef, ExperienceContext, bool)> {
    let commit = git(cwd, &["rev-parse", "HEAD^{commit}"]).unwrap_or_else(|| "unversioned".into());
    let tree_hash = git(cwd, &["rev-parse", "HEAD^{tree}"]).unwrap_or_else(|| "unversioned".into());
    let clean = commit != "unversioned"
        && git(cwd, &["status", "--porcelain", "--untracked-files=normal"])
            .is_some_and(|s| s.is_empty());
    let root = git(cwd, &["rev-parse", "--show-toplevel"])
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| cwd.into());
    let state = StateRef {
        repo_path: root,
        git_commit: commit,
        tree_hash,
    };
    let mut context = ExperienceContext::capture(&state, cwd, EnvironmentMode::Inherited)?;
    for (key, value) in environment.versions.iter().take(32) {
        if !super::privacy::sensitive_key(key) {
            context
                .environment
                .facts
                .insert(format!("version:{}", redact(key, 64)), redact(value, 128));
        }
    }
    context.environment.fingerprint =
        blake3::hash(&serde_json::to_vec(&context.environment.facts)?)
            .to_hex()
            .to_string();
    // OS/arch and repository facts come from the local runtime, never an adapter assertion.
    Ok((state, context, clean))
}
fn save(path: &Path, data: &str, kind: ArtifactKind) -> Result<ArtifactRef> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(data.as_bytes())?;
    Ok(artifact(path)?.with_kind(kind))
}
fn sanitized_copy(source: &ArtifactRef, target: &Path) -> Result<ArtifactRef> {
    let mut bytes = Vec::new();
    fs::File::open(&source.path)?
        .take(1024 * 1024)
        .read_to_end(&mut bytes)?;
    save(
        target,
        &redact(&String::from_utf8_lossy(&bytes), 8192),
        source.kind,
    )
}
pub fn record(
    store: &Store,
    session: &Session,
    run: &RunRecord,
    config: &BridgeConfig,
    cancel: &Cancellation,
) -> Result<Experience> {
    let id: ExperienceId = run.experience_id.parse()?;
    if let Some(exp) = ExperienceStore::get(store, &id)? {
        return Ok(exp);
    }
    // A deterministic output directory identifies incomplete recording after a crash.
    let directory = store.home.join("artifacts").join(&run.experience_id);
    fs::create_dir(&directory)?;
    let actions = &session.actions[run.action_start..run.action_end];
    let trace = save(
        &directory.join("actions.json"),
        &serde_json::to_string(actions)?,
        ArtifactKind::Metadata,
    )?;
    let empty = save(&directory.join("empty.txt"), "", ArtifactKind::Stdout)?;
    // This is the final workspace diff relative to HEAD at registration, not exclusive agent attribution.
    // Untracked contents and binary data are not ingested; dirty starts cannot validate transfer.
    let diff = git(
        &session.cwd,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            &session.starting_state.git_commit,
            "--",
        ],
    );
    let diff = save(
        &directory.join("workspace.patch"),
        &redact(
            diff.as_deref().unwrap_or(
                "Diff unavailable (unversioned workspace, Git failure, or capture limit)",
            ),
            32768,
        ),
        ArtifactKind::Diff,
    )?;
    let reality = Reality {
        fork_reason: None,
        experiment_id: None,
        candidate_id: None,
        id: RealityId::new(),
        parent: None,
        root: session.cwd.clone(),
        starting_state: session.starting_state.clone(),
        created_at: session.started_at,
        status: RealityStatus::Observed,
        ephemeral: false,
    };
    let agent = crate::core::AgentIdentity {
        kind: session.agent.name.clone(),
        executable: format!("bridge:{}", session.agent.name),
        version: session.agent.version.clone(),
        model: session.agent.model.clone(),
    };
    // The adapter did not execute an aggregate shell command. Preserve that distinction explicitly.
    let aggregate = ActionRecord {
        command: CommandSpec {
            program: "bridge-observation".into(),
            args: vec![run.run_id.clone()],
            environment: EnvironmentMode::Inherited,
            environment_overrides: Default::default(),
        },
        cwd: session.cwd.clone(),
        started_at: session.started_at,
        duration_ms: run.duration_ms,
        exit_code: None,
        signal: None,
        stdout: trace.clone(),
        stderr: empty.clone(),
    };
    let execution = ExecutionRecord {
        id: ExecutionId::new(),
        reality_id: reality.id.clone(),
        starting_state: session.starting_state.clone(),
        task: session.task.clone(),
        agent: agent.clone(),
        status: match run.termination {
            super::protocol::RunTermination::TimedOut => ProcessStatus::TimedOut,
            super::protocol::RunTermination::Interrupted => ProcessStatus::Interrupted,
            // Intercepted proposals may be abandoned after advice; they are not executions.
            _ if actions
                .iter()
                .any(|a| a.result.is_none() && !a.can_intercept) =>
            {
                ProcessStatus::Interrupted
            }
            _ if actions
                .iter()
                .any(|a| a.result.as_ref().is_some_and(|r| !r.success)) =>
            {
                ProcessStatus::Failed
            }
            _ => ProcessStatus::Succeeded,
        },
        action: aggregate.clone(),
        diff: diff.clone(),
    };
    let temporary = tempfile::tempdir()?;
    let evaluator = CommandEvaluator {
        spec: EvaluationSpec {
            checks: config
                .evaluators
                .get(&session.cwd.to_string_lossy().to_string())
                .cloned()
                .unwrap_or_default(),
        },
        timeout: Duration::from_secs(config.evaluator_timeout_secs),
        environment: EnvironmentMode::Inherited,
        environment_overrides: Default::default(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut evaluation =
        runtime.block_on(evaluator.evaluate(&reality, &execution, temporary.path(), cancel))?;
    let mut artifacts = vec![trace.clone(), empty, diff];
    for (index, check) in evaluation.checks.iter_mut().enumerate() {
        check.command = redact(&check.command, 8192);
        if let Some(action) = &mut check.action {
            action.stdout = sanitized_copy(
                &action.stdout,
                &directory.join(format!("check-{index}.out")),
            )?;
            action.stderr = sanitized_copy(
                &action.stderr,
                &directory.join(format!("check-{index}.err")),
            )?;
            action.command.args = action
                .command
                .args
                .iter()
                .map(|a| redact(a, 8192))
                .collect();
            artifacts.extend([action.stdout.clone(), action.stderr.clone()]);
        }
    }
    evaluation.spec.checks = evaluation
        .spec
        .checks
        .iter()
        .map(|c| redact(c, 8192))
        .collect();
    let mut applications = Vec::new();
    let mut observed_actions = Vec::new();
    for action in actions.iter().filter(|a| a.result.is_some()) {
        if let super::protocol::NormalizedAction::Shell { command, cwd } = &action.action
            && Path::new(cwd) == session.cwd
        {
            observed_actions.push(ObservedAction {
                action: ActionPattern::shell(command),
                observer: "bridge-lifecycle-v1".into(),
                artifact: trace.clone(),
            });
        }
    }
    let mut relations = Vec::new();
    for retrieved in &session.delivered {
        let lesson = &retrieved.lesson;
        let observed = lesson.prefer.as_ref().is_some_and(|prefer| {
            actions.iter().any(|a| {
                if !a.result.as_ref().is_some_and(|r| r.success) {
                    return false;
                }
                match &a.action {
                    super::protocol::NormalizedAction::Shell { command, cwd } => {
                        Path::new(cwd) == session.cwd && prefer.matches_shell(command)
                    }
                    _ => false,
                }
            })
        });
        let rejected = session.rejections.get(&lesson.id.to_string());
        let applied = session.clean_start && observed && rejected.is_none();
        applications.push(LessonApplication {
            id: ApplicationId::new(),
            lesson_id: lesson.id.clone(),
            lesson_version: lesson.version,
            experience_id: id.clone(),
            created_at: Utc::now(),
            relevance: retrieved.relevance,
            context_matches: retrieved.matched_context.clone(),
            delivered: true,
            influence: if rejected.is_some() {
                LessonInfluence::Rejected
            } else if applied {
                LessonInfluence::Applied
            } else {
                LessonInfluence::Retrieved
            },
            verification: if applied {
                ApplicationVerification::Observed
            } else {
                ApplicationVerification::Unconfirmed
            },
            resulting_action: if applied { lesson.prefer.clone() } else { None },
            reason: if let Some(f) = rejected {
                format!("Agent rejected: {:?}", f.reason)
            } else if applied {
                "Preferred action observed through native lifecycle; no causal attribution implied"
                    .into()
            } else {
                "Context delivered; application not established".into()
            },
            artifacts: vec![trace.clone()],
        });
        if applied && lesson.source_experience != id {
            let relation = ExperienceRelation::TransferFrom(lesson.source_experience.clone());
            if !relations.contains(&relation) {
                relations.push(relation);
            }
        }
    }
    let failure_signatures = crate::experience::failure_signatures(&evaluation, &aggregate)?;
    let experience = Experience {
        experiment: None,
        id,
        created_at: Utc::now(),
        goal: session.task.clone(),
        context: session.context.clone(),
        starting_state: session.starting_state.clone(),
        reality_id: reality.id.clone(),
        execution_id: execution.id.clone(),
        agent,
        actions: vec![aggregate],
        perturbations: vec![],
        outcome: Outcome::from_evaluation(&evaluation),
        evaluation,
        failure_signatures,
        evidence: EvidenceBundle { artifacts },
        tags: vec![
            "bridge-lifecycle-v1".into(),
            "external-workspace-not-isolated".into(),
            if session.clean_start {
                "bridge-clean-start-v1".into()
            } else {
                "bridge-dirty-start-v1".into()
            },
        ],
        replay: None,
        lesson_applications: applications,
        relations,
        repeated_mistakes: vec![],
        observed_actions,
        application_report_errors: vec![],
        resilience: None,
    };
    store.persist_bridge_experience(&reality, &execution, &experience, &session.id, run)?;
    Ok(experience)
}
