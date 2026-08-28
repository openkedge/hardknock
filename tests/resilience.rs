// SPDX-License-Identifier: Apache-2.0
mod support;
use hardknock::{
    core::*,
    dojo::{GitRealityProvider, RealityProvider, capture_state},
    experience::{ExperienceContext, Outcome},
    perturbation::{
        AppliedPerturbations, LocalPerturbationProvider, Perturbation, PerturbationParameters,
        PerturbationProvider,
    },
    resilience::{
        reflex::{DeterministicReflexMatcher, ReflexMatcher, fixture_action},
        runtime::{RunResilienceOptions, apply},
        *,
    },
    store::{Store, artifact},
};
use serde_json::Value;
use std::{collections::HashSet, fs, os::unix::fs::symlink};
use support::{Fixture, git};

fn run(f: &Fixture, profile: &str) -> ChaosCampaign {
    serde_json::from_value(
        f.cli(
            &[
                "chaos",
                "run",
                "--agent",
                "test-agent",
                "--profile",
                profile,
            ],
            0,
        )["result"]["campaign"]
            .clone(),
    )
    .unwrap()
}
fn test_result(value: Value) -> ResilienceTest {
    serde_json::from_value(value["result"]["test"].clone()).unwrap()
}
fn clean(f: &Fixture) {
    f.assert_source_unchanged();
    let store = Store::open(&f.home).unwrap();
    assert!(
        store
            .realities()
            .unwrap()
            .iter()
            .all(|r| r.status == RealityStatus::Discarded)
    );
    assert_eq!(fs::read_dir(f.home.join("realities")).unwrap().count(), 0);
    let db = rusqlite::Connection::open(f.home.join("hardknock.db")).unwrap();
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        !db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_some()
    );
}

#[test]
fn boundary_sweep_records_every_experience_and_only_tested_points() {
    let f = Fixture::from_fixture("retry-resilience");
    let c = run(&f, "latency");
    assert_eq!(c.result, CampaignStatus::Completed);
    assert_eq!(c.control.as_ref().unwrap().outcome, ChaosTrialOutcome::Pass);
    assert_eq!(
        c.trials.iter().map(|t| t.outcome).collect::<Vec<_>>(),
        vec![
            ChaosTrialOutcome::Pass,
            ChaosTrialOutcome::Pass,
            ChaosTrialOutcome::Pass,
            ChaosTrialOutcome::Degraded,
            ChaosTrialOutcome::Fail
        ]
    );
    let store = Store::open(&f.home).unwrap();
    let envelope = store.envelope(c.envelope_id.as_ref().unwrap()).unwrap();
    assert_eq!(envelope.tested_conditions.len(), 5);
    assert_eq!(envelope.safe_regions.len(), 3);
    assert_eq!(envelope.degraded_regions.len(), 1);
    assert_eq!(envelope.failure_regions.len(), 1);
    assert!(matches!(
        envelope.unknown_regions.as_slice(),
        [ConditionRange::AllUntestedConditions]
    ));
    let mut realities = HashSet::new();
    for t in c.control.iter().chain(&c.trials) {
        assert!(realities.insert(t.reality_id.clone()));
        let e = store.experience(&t.experience_id).unwrap();
        let r = e.resilience.as_ref().unwrap();
        assert_eq!(r.origin.as_ref().unwrap().campaign_id, c.id);
        assert_eq!(r.origin.as_ref().unwrap().trial_id, t.id);
        assert_eq!(r.perturbation_ids.len(), usize::from(!t.is_control));
        for artifact_ref in &e.evidence.artifacts {
            assert_eq!(
                artifact(&artifact_ref.path).unwrap().blake3,
                artifact_ref.blake3
            );
        }
        if !t.is_control {
            assert!(e.relations.contains(
                &hardknock::application::ExperienceRelation::ChaosVariantOf(
                    c.control.as_ref().unwrap().experience_id.clone()
                )
            ));
        }
    }
    let last = c.trials.last().unwrap();
    let e = store.experience(&last.experience_id).unwrap();
    assert_eq!(e.outcome, Outcome::Failure);
    assert!(
        e.failure_signatures
            .iter()
            .any(|s| s.signature == "retry_exhaustion")
    );
    assert_eq!(last.metrics.retries, 5);
    assert_eq!(e.resilience.unwrap().temporal.len(), 6);
    assert_eq!(
        store.lesson(&last.lessons[0]).unwrap().status,
        hardknock::lesson::LessonStatus::Candidate
    );
    assert_eq!(
        store.reflex(&last.reflexes[0]).unwrap().status,
        ReflexStatus::Candidate
    );
    let report = f.cli(&["chaos", "report", &c.id.to_string()], 0);
    assert_eq!(report["result"]["metrics"]["envelope_tested_points"], 5);
    assert!(report["result"]["metrics"]["false_positive_reflex_rate"].is_null());
    assert!(c.plan.agent.model.is_none());
    assert_eq!(c.plan.runtime_version, "local-resilience-v1");
    clean(&f);
}

#[test]
fn reflex_pair_support_activation_and_why_preserve_provenance() {
    let f = Fixture::from_fixture("retry-resilience");
    let c = run(&f, "latency");
    let id = &c.trials.last().unwrap().reflexes[0];
    let rejected = f
        .command()
        .args(["reflex", "enable", &id.to_string()])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    let t = test_result(f.cli(&["reflex", "test", &id.to_string()], 0));
    assert_eq!(t.status, ResilienceTestStatus::Supported);
    assert_eq!(t.false_positive, Some(false));
    let store = Store::open(&f.home).unwrap();
    let before = store.experience(t.without.as_ref().unwrap()).unwrap();
    let after = store.experience(t.with.as_ref().unwrap()).unwrap();
    assert_eq!(before.outcome, Outcome::Failure);
    assert_eq!(after.outcome, Outcome::Success);
    assert_eq!(before.starting_state, after.starting_state);
    let observation = after.resilience.as_ref().unwrap();
    assert_eq!(observation.metrics.attempts, 3);
    assert_eq!(observation.metrics.replans, 1);
    assert!(observation.reflex_matches[0].test_only);
    assert_eq!(store.reflex(id).unwrap().status, ReflexStatus::Supported);
    let why = store.explain(Some(&after.id)).unwrap();
    assert_eq!(why.reflexes[0].source_campaign, c.id);
    assert_eq!(why.reflexes[0].source_trial.id, c.trials.last().unwrap().id);
    assert_eq!(store.explain(None).unwrap().experience_id, after.id);
    let enabled = f.cli(&["reflex", "enable", &id.to_string()], 0);
    assert_eq!(enabled["result"]["reflex"]["status"], "active");
    let active = run(&f, "latency");
    let e = store
        .experience(&active.trials.last().unwrap().experience_id)
        .unwrap();
    assert_eq!(e.outcome, Outcome::Success);
    assert!(!e.resilience.unwrap().reflex_matches[0].test_only);
    f.cli(&["reflex", "disable", &id.to_string()], 0);
    assert_eq!(store.reflex(id).unwrap().status, ReflexStatus::Disabled);
    assert_eq!(
        store.explain(Some(&after.id)).unwrap().reflexes[0]
            .matched
            .reflex_version,
        1
    );
    clean(&f);
}

#[test]
fn scope_and_proposed_action_gate_reflexes_even_when_precursor_matches() {
    let f = Fixture::from_fixture("retry-resilience");
    let c = run(&f, "latency");
    let store = Store::open(&f.home).unwrap();
    let id = &c.trials.last().unwrap().reflexes[0];
    let mut reflex = store.reflex(id).unwrap();
    reflex.status = ReflexStatus::Active;
    let mut context = ActionContext {
        context: store
            .experience(&c.trials.last().unwrap().experience_id)
            .unwrap()
            .context,
        proposed_action: fixture_action(),
        consecutive_failures: 3,
        no_state_change: true,
        config_changed: false,
        elapsed_ms: 6000,
        state_fingerprint: "scope-matcher-test".into(),
    };
    assert_eq!(
        DeterministicReflexMatcher
            .evaluate(&context, &[reflex.clone()])
            .unwrap()
            .len(),
        1
    );
    let other = Fixture::from_fixture("stale-credential");
    context.context = ExperienceContext::capture(
        &capture_state(&other.repo).unwrap(),
        &other.repo,
        EnvironmentMode::Controlled,
    )
    .unwrap();
    assert!(
        DeterministicReflexMatcher
            .evaluate(&context, &[reflex.clone()])
            .unwrap()
            .is_empty()
    );
    context.context = store
        .experience(&c.trials.last().unwrap().experience_id)
        .unwrap()
        .context;
    context.proposed_action = hardknock::lesson::ActionPattern::shell("./unrelated-operation.sh");
    assert!(
        DeterministicReflexMatcher
            .evaluate(&context, &[reflex.clone()])
            .unwrap()
            .is_empty()
    );
    context.proposed_action = fixture_action();
    context.no_state_change = false;
    assert!(
        DeterministicReflexMatcher
            .evaluate(&context, &[reflex.clone()])
            .unwrap()
            .is_empty()
    );
    context.no_state_change = true;
    reflex.status = ReflexStatus::Candidate;
    assert!(
        DeterministicReflexMatcher
            .evaluate(&context, &[reflex])
            .unwrap()
            .is_empty()
    );
    clean(&f);
    other.assert_source_unchanged();
}

#[test]
fn false_positive_records_successful_original_action_and_prevents_activation() {
    let f = Fixture::from_fixture("retry-resilience");
    let c = run(&f, "latency");
    let id = &c.trials.last().unwrap().reflexes[0];
    f.cli(&["reflex", "test", &id.to_string()], 0);
    f.cli(&["reflex", "enable", &id.to_string()], 0);
    let t = test_result(f.cli(
        &[
            "reflex",
            "test",
            &id.to_string(),
            "--perturb",
            "command-failure:3",
        ],
        0,
    ));
    assert_eq!(t.status, ResilienceTestStatus::FalsePositive);
    assert_eq!(t.false_positive, Some(true));
    let store = Store::open(&f.home).unwrap();
    let original = store.experience(t.without.as_ref().unwrap()).unwrap();
    assert_eq!(original.outcome, Outcome::Success);
    assert_eq!(original.resilience.unwrap().temporal[3].attempt, 4);
    assert_eq!(store.reflex(id).unwrap().status, ReflexStatus::Disabled);
    assert!(
        !f.command()
            .args(["reflex", "enable", &id.to_string()])
            .output()
            .unwrap()
            .status
            .success()
    );
    // A later successful replay must not erase the negative evidence.
    f.cli(&["reflex", "test", &id.to_string()], 0);
    assert_eq!(store.reflex(id).unwrap().status, ReflexStatus::Disabled);
    let report = f.cli(&["chaos", "report", &c.id.to_string()], 0);
    assert_eq!(report["result"]["metrics"]["false_positive_reflexes"], 1);
    assert_eq!(report["result"]["metrics"]["paired_reflex_firings"], 3);
    clean(&f);
}

#[test]
fn stale_credential_recovery_reproduces_failure_before_typed_steps() {
    let f = Fixture::from_fixture("stale-credential");
    let c = run(&f, "credential");
    let id = &c.trials[0].recoveries[0];
    let store = Store::open(&f.home).unwrap();
    assert_eq!(
        store.recovery(id).unwrap().status,
        RecoveryStatus::Candidate
    );
    let t = test_result(f.cli(&["recovery", "test", &id.to_string()], 0));
    assert_eq!(t.status, ResilienceTestStatus::Supported);
    let exp = store.experience(t.with.as_ref().unwrap()).unwrap();
    assert_eq!(exp.outcome, Outcome::Success);
    let attempt = exp
        .resilience
        .as_ref()
        .unwrap()
        .recovery_attempt
        .as_ref()
        .unwrap();
    assert!(attempt.reproduced_failure && attempt.attempted && attempt.succeeded);
    assert_eq!(
        attempt.failure_signature.as_deref(),
        Some("stale_credential")
    );
    assert_eq!(attempt.steps_executed, 4);
    assert!(exp.actions.iter().take(6).all(|a| a.exit_code == Some(21)));
    assert_ne!(exp.actions[6].exit_code, Some(0)); // Pre-recovery check in this exact Reality.
    assert!(
        exp.relations
            .contains(&hardknock::application::ExperienceRelation::RecoveryOf(
                t.without.unwrap()
            ))
    );
    assert_eq!(
        store.recovery(id).unwrap().status,
        RecoveryStatus::Supported
    );
    f.cli(&["recovery", "test", &id.to_string()], 0);
    assert_eq!(
        store.recovery(id).unwrap().status,
        RecoveryStatus::Supported
    );
    let report = f.cli(&["chaos", "report", &c.id.to_string()], 0);
    assert_eq!(report["result"]["metrics"]["recovery_success_rate"], 1.0);
    clean(&f);
}

#[test]
fn configuration_drift_is_a_stale_plan_and_replanning_reads_current_generation() {
    let f = Fixture::from_fixture("config-drift");
    let c = run(&f, "config-drift");
    let store = Store::open(&f.home).unwrap();
    let failed = store.experience(&c.trials[0].experience_id).unwrap();
    assert!(
        failed
            .failure_signatures
            .iter()
            .any(|s| s.signature == "configuration_stale")
    );
    assert!(
        failed
            .resilience
            .unwrap()
            .temporal
            .iter()
            .all(|t| t.config_changed)
    );
    let t = test_result(f.cli(&["reflex", "test", &c.trials[0].reflexes[0].to_string()], 0));
    assert_eq!(t.status, ResilienceTestStatus::Supported);
    let exp = store.experience(t.with.as_ref().unwrap()).unwrap();
    let r = exp.resilience.as_ref().unwrap();
    assert_eq!(r.metrics.attempts, 0);
    assert!(r.reflex_matches[0].observed.config_changed);
    let diff = exp
        .evidence
        .artifacts
        .iter()
        .find(|a| a.path.file_name().is_some_and(|n| n == "diff.patch"))
        .unwrap();
    let diff = fs::read_to_string(&diff.path).unwrap();
    assert!(diff.contains("plan-generation"));
    assert!(diff.contains("+2"));
    let t = test_result(f.cli(
        &["recovery", "test", &c.trials[0].recoveries[0].to_string()],
        0,
    ));
    assert_eq!(t.status, ResilienceTestStatus::Supported);
    assert_eq!(
        fs::read_to_string(f.repo.join("generation")).unwrap(),
        "1\n"
    );
    clean(&f);
}

#[test]
fn failing_control_prevents_all_perturbation_trials_and_derived_claims() {
    let f = Fixture::new();
    let response = f.cli(
        &[
            "chaos",
            "run",
            "--command",
            "exit 1",
            "--check",
            "exit 1",
            "--profile",
            "latency",
        ],
        3,
    );
    let c: ChaosCampaign = serde_json::from_value(response["result"]["campaign"].clone()).unwrap();
    assert_eq!(c.result, CampaignStatus::UnhealthyControl);
    assert!(c.trials.is_empty());
    assert!(c.envelope_id.is_none());
    let store = Store::open(&f.home).unwrap();
    assert!(store.reflexes().unwrap().is_empty());
    assert!(store.envelopes().unwrap().is_empty());
    assert_eq!(store.executions().unwrap().len(), 1);
    clean(&f);
}

#[test]
fn trial_budget_is_hard_and_bundled_fixture_command_works_without_source_changes() {
    let f = Fixture::new();
    let result = f.cli(
        &[
            "chaos",
            "run",
            "--fixture",
            "retry-resilience",
            "--perturb-sweep",
            "delay=0,100,500,1000,2000",
            "--trials",
            "2",
        ],
        3,
    );
    let c: ChaosCampaign = serde_json::from_value(result["result"]["campaign"].clone()).unwrap();
    assert_eq!(c.result, CampaignStatus::BudgetExhausted);
    assert_eq!(c.trials.len(), 2);
    assert_ne!(c.plan.starting_state.repo_path, f.repo);
    assert!(
        c.plan
            .starting_state
            .repo_path
            .starts_with(f.home.canonicalize().unwrap())
    );
    assert!(
        git(&c.plan.starting_state.repo_path, &["status", "--porcelain"])
            .stdout
            .is_empty()
    );
    assert_eq!(Store::open(&f.home).unwrap().executions().unwrap().len(), 3);
    clean(&f);
}

#[test]
fn known_successful_skill_can_be_registered_and_targeted_by_name() {
    let f = Fixture::from_fixture("retry-resilience");
    let c = run(&f, "latency");
    let skill = f.cli(
        &[
            "skill",
            "register",
            "retry-operation",
            "--experience",
            &c.control.unwrap().experience_id.to_string(),
        ],
        0,
    );
    let id = skill["result"]["skill"]["id"].as_str().unwrap();
    let next = f.cli(
        &[
            "chaos",
            "run",
            "--skill",
            "retry-operation",
            "--perturb",
            "delay:500ms",
        ],
        0,
    );
    assert_eq!(
        next["result"]["campaign"]["plan"]["target"]["kind"],
        "skill"
    );
    assert_eq!(next["result"]["campaign"]["plan"]["target"]["value"], id);
    let store = Store::open(&f.home).unwrap();
    assert!(
        store
            .register_skill("failed-skill", &c.trials.last().unwrap().experience_id)
            .is_err()
    );
    clean(&f);
}

#[test]
fn generic_commands_apply_environment_file_failure_and_real_local_delay() {
    let f = Fixture::new();
    let env = f.cli(
        &[
            "chaos",
            "run",
            "--command",
            "test -z \"${HK_EXPERIMENT:-}\" && touch result",
            "--check",
            "test -f result",
            "--perturb",
            "env:HK_EXPERIMENT=broken",
        ],
        0,
    );
    assert_eq!(env["result"]["campaign"]["control"]["outcome"], "pass");
    assert_eq!(env["result"]["campaign"]["trials"][0]["outcome"], "fail");
    let file = f.cli(
        &[
            "chaos",
            "run",
            "--command",
            "test \"$(cat tracked.txt)\" = original && touch result",
            "--check",
            "test -f result",
            "--perturb",
            "file:tracked.txt=drift",
        ],
        0,
    );
    assert_eq!(file["result"]["campaign"]["trials"][0]["outcome"], "fail");
    let fail = f.cli(
        &[
            "chaos",
            "run",
            "--command",
            "touch result",
            "--check",
            "test -f result",
            "--perturb",
            "command-failure:once",
        ],
        0,
    );
    assert_eq!(fail["result"]["campaign"]["trials"][0]["outcome"], "fail");
    let delay = f.cli(
        &[
            "chaos",
            "run",
            "--command",
            "touch result",
            "--check",
            "test -f result",
            "--perturb",
            "delay:100ms",
        ],
        0,
    );
    assert!(
        delay["result"]["campaign"]["trials"][0]["metrics"]["duration_ms"]
            .as_u64()
            .unwrap()
            >= 90
    );
    clean(&f);
}

#[test]
fn perturbation_handles_restore_files_and_reject_escape_paths_and_source_checkout() {
    let f = Fixture::new();
    let store = Store::open(&f.home).unwrap();
    let provider = GitRealityProvider::new(&store);
    let mut r = provider.create(&capture_state(&f.repo).unwrap()).unwrap();
    let mutation = |path: &str, content: &str| {
        Perturbation::new(PerturbationParameters::FileMutation {
            path: path.into(),
            content: content.into(),
        })
    };
    let p = LocalPerturbationProvider;
    {
        let mut handles = AppliedPerturbations::default();
        handles
            .0
            .push(p.apply(&r, &mutation("tracked.txt", "one")).unwrap());
        handles
            .0
            .push(p.apply(&r, &mutation("tracked.txt", "two")).unwrap());
        assert_eq!(
            fs::read_to_string(r.root.join("tracked.txt")).unwrap(),
            "two"
        );
    }
    assert_eq!(
        fs::read_to_string(r.root.join("tracked.txt")).unwrap(),
        "original\n"
    );
    {
        let handle = p.apply(&r, &mutation("new.txt", "temporary")).unwrap();
        p.remove(handle).unwrap();
    }
    assert!(!r.root.join("new.txt").exists());
    symlink(&f.repo, r.root.join("escape")).unwrap();
    fs::hard_link(f.repo.join("tracked.txt"), r.root.join("hardlink.txt")).unwrap();
    assert!(p.apply(&r, &mutation("hardlink.txt", "bad")).is_err());
    for path in [
        "../tracked.txt",
        "/tmp/outside",
        "escape/tracked.txt",
        ".git/config",
    ] {
        assert!(p.apply(&r, &mutation(path, "bad")).is_err());
    }
    let mut source = r.clone();
    source.root = f.repo.clone();
    assert!(p.apply(&source, &mutation("tracked.txt", "bad")).is_err());
    let options = RunResilienceOptions {
        perturbations: vec![
            mutation("tracked.txt", "changed"),
            mutation("escape/tracked.txt", "bad"),
        ],
        ..Default::default()
    };
    assert!(apply(&r, &options).is_err());
    assert_eq!(
        fs::read_to_string(r.root.join("tracked.txt")).unwrap(),
        "original\n"
    );
    let env = p
        .apply(
            &r,
            &Perturbation::new(PerturbationParameters::EnvironmentVariable {
                key: "HK_TEST_ONLY".into(),
                value: "value".into(),
            }),
        )
        .unwrap();
    assert_eq!(env.environment["HK_TEST_ONLY"], "value");
    assert!(std::env::var_os("HK_TEST_ONLY").is_none());
    p.remove(env).unwrap();
    provider.discard(&mut r).unwrap();
    clean(&f);
}

#[test]
fn immutable_resilience_evidence_and_versioned_interpretations_are_enforced() {
    let f = Fixture::from_fixture("stale-credential");
    let c = run(&f, "credential");
    let id = &c.trials[0].recoveries[0];
    f.cli(&["recovery", "test", &id.to_string()], 0);
    let db = rusqlite::Connection::open(f.home.join("hardknock.db")).unwrap();
    for table in [
        "chaos_trials",
        "perturbations",
        "operating_envelopes",
        "operating_envelope_observations",
        "recovery_steps",
        "recovery_versions",
        "recovery_attempts",
        "experience_perturbations",
    ] {
        let column = if table == "experience_perturbations" {
            "experience_id"
        } else {
            "data"
        };
        assert!(
            db.execute(&format!("UPDATE {table} SET {column}={column}"), [])
                .is_err()
        );
        assert!(db.execute(&format!("DELETE FROM {table}"), []).is_err());
    }
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM recovery_versions WHERE recovery_id=?1",
            [id.to_string()],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    assert!(
        db.execute("UPDATE chaos_campaigns SET data=data", [])
            .is_err()
    );
    assert!(
        db.execute("UPDATE resilience_tests SET data=data", [])
            .is_err()
    );
    clean(&f);
}

#[test]
fn failed_recovery_is_contradiction_and_not_validation() {
    let f = Fixture::from_fixture("stale-credential");
    fs::write(f.repo.join("refresh-token.sh"), "#!/bin/sh\nexit 19\n").unwrap();
    git(&f.repo, &["add", "."]);
    git(
        &f.repo,
        &["commit", "-m", "Simulate unavailable refresh operation"],
    );
    let c = run(&f, "credential");
    let id = &c.trials[0].recoveries[0];
    let test = test_result(f.cli(&["recovery", "test", &id.to_string()], 0));
    assert_eq!(test.status, ResilienceTestStatus::Contradicted);
    let store = Store::open(&f.home).unwrap();
    let e = store.experience(test.with.as_ref().unwrap()).unwrap();
    let a = e.resilience.unwrap().recovery_attempt.unwrap();
    assert!(a.reproduced_failure && a.attempted && !a.succeeded);
    assert_eq!(a.steps_executed, 1);
    assert_eq!(
        store.recovery(id).unwrap().status,
        RecoveryStatus::Contradicted
    );
    clean(&f);
}

#[test]
fn active_reflex_does_not_fire_in_a_different_fixture_campaign() {
    let f = Fixture::new();
    let retry = f.cli(
        &[
            "chaos",
            "run",
            "--fixture",
            "retry-resilience",
            "--profile",
            "latency",
        ],
        0,
    );
    let id = retry["result"]["campaign"]["trials"][4]["reflexes"][0]
        .as_str()
        .unwrap();
    f.cli(&["reflex", "test", id], 0);
    f.cli(&["reflex", "enable", id], 0);
    let stale = f.cli(
        &[
            "chaos",
            "run",
            "--fixture",
            "stale-credential",
            "--profile",
            "credential",
        ],
        0,
    );
    let id = stale["result"]["campaign"]["trials"][0]["experience_id"]
        .as_str()
        .unwrap();
    let e = Store::open(&f.home)
        .unwrap()
        .experience(&id.parse().unwrap())
        .unwrap();
    assert_eq!(e.outcome, Outcome::Failure);
    assert!(e.resilience.unwrap().reflex_matches.is_empty());
    clean(&f);
}

#[test]
fn interrupted_campaign_persists_inconclusive_trial_and_cleans_realities() {
    use nix::{
        sys::signal::{Signal, kill},
        unistd::Pid,
    };
    use std::{
        process::Stdio,
        time::{Duration, Instant},
    };
    let f = Fixture::new();
    let child = f
        .command()
        .arg("--json")
        .args([
            "chaos",
            "run",
            "--command",
            "touch result",
            "--check",
            "test -f result",
            "--perturb",
            "delay:10000ms",
            "--perturb",
            "delay:500ms",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    loop {
        let trial_started = fs::read_dir(f.home.join("artifacts"))
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join("agent-0").exists())
            .count()
            >= 2;
        if trial_started {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(20));
    }
    kill(Pid::from_raw(child.id() as i32), Signal::SIGINT).unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data: Value = serde_json::from_slice(&output.stdout).unwrap();
    let campaign = &data["result"]["campaign"];
    assert_eq!(campaign["result"], "interrupted");
    assert_eq!(campaign["trials"].as_array().unwrap().len(), 1);
    assert_eq!(campaign["trials"][0]["outcome"], "inconclusive");
    clean(&f);
}

#[test]
fn v4_migration_preserves_nonempty_relation_and_lesson_evidence_tables() {
    let source = Fixture::pnpm();
    source.cli(
        &[
            "run",
            "--agent",
            "test-agent",
            "--check",
            "./test.sh",
            "fix workspace",
        ],
        1,
    );
    let legacy = Fixture::new();
    fs::create_dir(&legacy.home).unwrap();
    let db = rusqlite::Connection::open(legacy.home.join("hardknock.db")).unwrap();
    db.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); INSERT INTO schema_migrations(version) VALUES(1),(2),(3),(4);").unwrap();
    for sql in [
        include_str!("../migrations/001_substrate.sql"),
        include_str!("../migrations/002_experiences.sql"),
        include_str!("../migrations/003_learning.sql"),
        include_str!("../migrations/004_transfer.sql"),
    ] {
        db.execute_batch(sql).unwrap();
    }
    db.execute(
        "ATTACH DATABASE ?1 AS seed",
        [source.home.join("hardknock.db").to_str().unwrap()],
    )
    .unwrap();
    for table in [
        "realities",
        "executions",
        "evaluations",
        "experiences",
        "experience_artifacts",
        "hypotheses",
        "lessons",
        "lesson_versions",
        "experiments",
        "trials",
        "trial_artifacts",
        "lesson_evidence",
        "experience_relations",
    ] {
        db.execute(
            &format!("INSERT INTO {table} SELECT * FROM seed.{table}"),
            [],
        )
        .unwrap();
    }
    let before: String = db
        .query_row("SELECT group_concat(data) FROM experiences", [], |r| {
            r.get(0)
        })
        .unwrap();
    let relations: i64 = db
        .query_row("SELECT COUNT(*) FROM experience_relations", [], |r| {
            r.get(0)
        })
        .unwrap();
    let evidence: i64 = db
        .query_row("SELECT COUNT(*) FROM lesson_evidence", [], |r| r.get(0))
        .unwrap();
    assert!(relations > 0 && evidence > 0);
    drop(db);
    let _store = Store::open(&legacy.home).unwrap();
    let db = rusqlite::Connection::open(legacy.home.join("hardknock.db")).unwrap();
    assert_eq!(
        before,
        db.query_row("SELECT group_concat(data) FROM experiences", [], |r| r
            .get::<_, String>(
            0
        ))
        .unwrap()
    );
    assert_eq!(
        relations,
        db.query_row("SELECT COUNT(*) FROM experience_relations", [], |r| r
            .get::<_, i64>(0))
            .unwrap()
    );
    assert_eq!(
        evidence,
        db.query_row("SELECT COUNT(*) FROM lesson_evidence", [], |r| r
            .get::<_, i64>(0))
            .unwrap()
    );
    assert!(
        db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
    assert!(db.execute("DELETE FROM experience_relations", []).is_err());
    assert!(db.execute("DELETE FROM lesson_evidence", []).is_err());
    source.assert_source_unchanged();
    legacy.assert_source_unchanged();
}

#[test]
fn concurrent_reflex_tests_append_evidence_without_lost_revisions() {
    let f = Fixture::from_fixture("retry-resilience");
    let c = run(&f, "latency");
    let id = &c.trials.last().unwrap().reflexes[0];
    let mut a = f.command();
    let mut b = f.command();
    let args = ["--json", "reflex", "test", &id.to_string()];
    a.args(args);
    b.args(args);
    let a = std::thread::spawn(move || a.output().unwrap());
    let b = std::thread::spawn(move || b.output().unwrap());
    for output in [a.join().unwrap(), b.join().unwrap()] {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["result"]["test"]["status"], "supported");
    }
    let store = Store::open(&f.home).unwrap();
    let reflex = store.reflex(id).unwrap();
    assert_eq!(reflex.version, 3);
    assert_eq!(reflex.evidence.len(), 5);
    assert_eq!(store.resilience_tests().unwrap().len(), 2);
    clean(&f);
}
