// SPDX-License-Identifier: Apache-2.0
mod support;
use hardknock::{
    bridge::{
        protocol::*,
        transport::{self, BridgeClient},
    },
    cancellation::Cancellation,
    cli::integrations::AdapterCommand,
    integrations::{claude, codex},
};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, time::Duration};
use support::Fixture;
fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
#[test]
fn claude_fixture_normalization_preserves_permissions_and_privacy() {
    let fixtures: Vec<Value> =
        serde_json::from_str(include_str!("../integrations/claude-code/fixtures.json")).unwrap();
    assert!(matches!(
        claude::normalize("Bash", &fixtures[1]["tool_input"], "/fixture").unwrap(),
        NormalizedAction::Shell { .. }
    ));
    let write = serde_json::to_string(
        &claude::normalize("Write", &fixtures[2]["tool_input"], "/fixture").unwrap(),
    )
    .unwrap();
    assert!(!write.contains("not retained"));
    assert!(claude::result(&fixtures[3]).success);
    assert!(!claude::result(&fixtures[4]).success);
    let advice = ActionDecision::Replan {
        reason: "Try the supported alternative".into(),
        evidence: vec![],
    };
    let response = claude::hook_response(&advice);
    assert!(
        response["hookSpecificOutput"]
            .get("permissionDecision")
            .is_none()
    );
    assert_eq!(
        claude::hook_response(&ActionDecision::Block {
            reason: "policy".into(),
            authority: DecisionAuthority::UserPolicy
        })["hookSpecificOutput"]["permissionDecision"],
        "deny"
    );
    assert!(claude::normalize("Bash", &json!({}), "/fixture").is_err());
}
#[test]
fn installers_are_idempotent_and_preserve_unrelated_settings() {
    let f = Fixture::new();
    let config = f.temp.path().join("claude settings.json");
    fs::write(&config,serde_json::to_vec(&json!({"model":"keep","hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"echo user-hook"}]}]}})).unwrap()).unwrap();
    for _ in 0..2 {
        hardknock::integrations::install::manage(
            "claude",
            &f.home,
            &AdapterCommand::Install {
                config: Some(config.clone()),
            },
        )
        .unwrap();
    }
    let installed: Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
    assert_eq!(
        installed["hooks"]["PreToolUse"].as_array().unwrap().len(),
        2
    );
    assert_eq!(installed["model"], "keep");
    hardknock::integrations::install::manage(
        "claude",
        &f.home,
        &AdapterCommand::Uninstall {
            config: Some(config.clone()),
        },
    )
    .unwrap();
    let removed: Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
    assert_eq!(removed["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
    assert_eq!(
        removed["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "echo user-hook"
    );
    for agent in ["hermes", "openclaw"] {
        let path = f.temp.path().join(agent);
        hardknock::integrations::install::manage(
            agent,
            &f.home,
            &AdapterCommand::Install {
                config: Some(path.clone()),
            },
        )
        .unwrap();
        fs::write(path.join("user.txt"), "retain").unwrap();
        hardknock::integrations::install::manage(
            agent,
            &f.home,
            &AdapterCommand::Uninstall {
                config: Some(path.clone()),
            },
        )
        .unwrap();
        assert_eq!(fs::read_to_string(path.join("user.txt")).unwrap(), "retain");
    }
}
#[test]
fn codex_fixture_types_approvals_and_version_pinning() {
    let events: Vec<Value> = include_str!("../integrations/codex/fixtures/lifecycle.jsonl")
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert!(matches!(
        codex::normalize_item(&events[4]["params"]["item"]).unwrap(),
        Some(NormalizedAction::Shell { .. })
    ));
    assert!(codex::normalize_result(&events[6]["params"]["item"]).success);
    let evidence = ActionDecision::Replan {
        reason: "Experience".into(),
        evidence: vec![],
    };
    assert_eq!(
        codex::approval_response(&events[5], &evidence, Some(true))["result"]["decision"],
        "accept"
    );
    assert_eq!(
        codex::approval_response(&events[5], &evidence, None)["result"]["decision"],
        "cancel"
    );
    assert!(!codex::version_supported("codex-cli 0.999.0"));
    assert!(codex::version_supported("codex-cli 0.149.1"));
    assert!(
        codex::normalize_item(&json!({"type":"future"}))
            .unwrap()
            .is_none()
    );
    assert!(codex::normalize_item(&json!({"type":"commandExecution"})).is_err());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_bidirectional_client_completes_a_model_free_fixture() {
    let f = Fixture::new();
    let cancel = Cancellation::default();
    let c = cancel.clone();
    let home = f.home.clone();
    let server = tokio::spawn(async move { transport::serve(&home, None, &c).await });
    for _ in 0..100 {
        if f.home.join("run/bridge-endpoint.json").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let executable = fixture_root().join("integrations/codex/fixtures/fake_app_server.py");
    let result = codex::run(
        &f.home,
        &f.repo,
        codex::RunOptions {
            executable: executable.to_str().unwrap(),
            allow_untested: false,
            resume: None,
            model: None,
            timeout: Duration::from_secs(10),
            task: "Protocol fixture, no model execution",
        },
        &cancel,
    )
    .await;
    let client = BridgeClient::new(&f.home);
    if let Ok(result) = &result {
        for _ in 0..100 {
            let run = client
                .request(AgentEvent::RunStatus {
                    hardknock_session_id: result["hardknock_session_id"].as_str().unwrap().into(),
                    run_id: result["turn_id"].as_str().unwrap().into(),
                })
                .await
                .unwrap();
            if run["status"] == "completed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    cancel.cancel();
    server.await.unwrap().unwrap();
    let result = result.unwrap();
    assert_eq!(result["thread_id"], "thread-fixture");
    assert_eq!(result["compatibility"]["supported"], true);
}
#[test]
fn plugin_mock_hosts_are_network_and_model_free() {
    for (program, args) in [
        ("python3", vec!["integrations/hermes/test_plugin.py"]),
        (
            "node",
            vec!["--test", "integrations/openclaw/hooks.test.mjs"],
        ),
    ] {
        let output = std::process::Command::new(program)
            .args(args)
            .current_dir(fixture_root())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
#[test]
fn doctor_checks_app_server_compatibility_without_a_model() {
    let f = Fixture::new();
    let bin = f.temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    std::os::unix::fs::symlink(
        fixture_root().join("integrations/codex/fixtures/fake_app_server.py"),
        bin.join("codex"),
    )
    .unwrap();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let output = f
        .command()
        .args(["integrate", "doctor"])
        .env("PATH", std::env::join_paths(paths).unwrap())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let codex = report["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["agent"] == "codex")
        .unwrap();
    assert_eq!(codex["compatibility"]["schema_verified"], true);
    assert_eq!(report["configuration"]["valid"], true);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_timeout_reaps_server_and_records_incomplete_run() {
    let f = Fixture::new();
    let cancel = Cancellation::default();
    let c = cancel.clone();
    let home = f.home.clone();
    let server = tokio::spawn(async move { transport::serve(&home, None, &c).await });
    for _ in 0..100 {
        if f.home.join("run/bridge-endpoint.json").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let executable = fixture_root().join("integrations/codex/fixtures/fake_app_server.py");
    let result = codex::run(
        &f.home,
        &f.repo,
        codex::RunOptions {
            executable: executable.to_str().unwrap(),
            allow_untested: false,
            resume: None,
            model: None,
            timeout: Duration::from_millis(500),
            task: "fixture-stall",
        },
        &Cancellation::default(),
    )
    .await;
    cancel.cancel();
    server.await.unwrap().unwrap();
    assert!(result.unwrap_err().to_string().contains("timed out"));
    let pid: i32 = fs::read_to_string(f.repo.join("fixture-server.pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
        Err(nix::errno::Errno::ESRCH)
    );
    let store = hardknock::store::Store::open(&f.home).unwrap();
    let sessions = store.bridge_sessions().unwrap();
    assert!(sessions[0].ended);
    let runs = store.bridge_runs(&sessions[0].id).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "completed");
    assert_eq!(runs[0].outcome.as_deref(), Some("timed_out"));
}
#[tokio::test]
async fn codex_advisory_mode_continues_when_bridge_is_absent() {
    let f = Fixture::new();
    let executable = fixture_root().join("integrations/codex/fixtures/fake_app_server.py");
    let result = codex::run(
        &f.home,
        &f.repo,
        codex::RunOptions {
            executable: executable.to_str().unwrap(),
            allow_untested: false,
            resume: None,
            model: None,
            timeout: Duration::from_secs(2),
            task: "Private prompt must not be recorded",
        },
        &Cancellation::default(),
    )
    .await
    .unwrap();
    assert_eq!(result["thread_id"], "thread-fixture");
    assert!(result["hardknock_session_id"].is_null());
    assert_eq!(result["recording"]["status"], "unavailable");
    assert!(!f.home.join("hardknock.db").exists());
}
#[tokio::test]
#[ignore = "requires a locally installed Codex; no model call"]
async fn real_codex_app_server_handshake() {
    let executable = std::env::var("HARDKNOCK_TEST_CODEX").unwrap_or_else(|_| "codex".into());
    let compatibility = codex::check(&executable, false).await.unwrap();
    assert!(compatibility.schema_verified);
    println!("{}", serde_json::to_string(&compatibility).unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "optional real Codex model run; uses configured local account and a disposable fixture"]
async fn real_codex_model_lifecycle_smoke() {
    let f = Fixture::new();
    fs::create_dir_all(&f.home).unwrap();
    let mut config = hardknock::bridge::config::Config::default();
    config.bridge.evaluators.insert(
        f.repo.canonicalize().unwrap().display().to_string(),
        vec!["test -f hardknock-smoke.txt && test \"$(cat hardknock-smoke.txt)\" = ok".into()],
    );
    fs::write(
        f.home.join("config.toml"),
        toml::to_string(&config).unwrap(),
    )
    .unwrap();
    let cancel = Cancellation::default();
    let c = cancel.clone();
    let home = f.home.clone();
    let server = tokio::spawn(async move { transport::serve(&home, None, &c).await });
    for _ in 0..100 {
        if f.home.join("run/bridge-endpoint.json").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let executable = std::env::var("HARDKNOCK_TEST_CODEX").unwrap_or_else(|_| "codex".into());
    let result=codex::run(&f.home,&f.repo,codex::RunOptions{executable:&executable,allow_untested:false,resume:None,model:None,timeout:Duration::from_secs(90),task:"This is a disposable Hardknock integration smoke fixture. Use a shell tool to write exactly ok to hardknock-smoke.txt in the current working directory. Do not read other directories or use the network. Then finish briefly."},&cancel).await;
    let mut recorded = None;
    if let Ok(result) = &result {
        let mut client = BridgeClient::new(&f.home);
        client.timeout = Duration::from_secs(2);
        for _ in 0..100 {
            let run = client
                .request(AgentEvent::RunStatus {
                    hardknock_session_id: result["hardknock_session_id"].as_str().unwrap().into(),
                    run_id: result["turn_id"].as_str().unwrap().into(),
                })
                .await;
            if let Ok(run) = run
                && run["status"] != "queued"
            {
                recorded = Some(run);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    cancel.cancel();
    server.await.unwrap().unwrap();
    let result = result.unwrap();
    let recorded = recorded.unwrap();
    println!(
        "REAL_CODEX_SMOKE {}",
        json!({"compatibility":result["compatibility"],"outcome":recorded["outcome"],"approval_required":result["approval_required"]})
    );
    if result["approval_required"] == true {
        assert!(
            matches!(
                recorded["outcome"].as_str(),
                Some("failure" | "interrupted")
            ),
            "Unapproved work must remain failed or interrupted, got {recorded}"
        );
    } else {
        assert_eq!(recorded["outcome"], "success");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_hooks_drive_bridge_and_bound_verification_continuations() {
    let f = Fixture::new();
    fs::create_dir_all(&f.home).unwrap();
    let mut config = hardknock::bridge::config::Config::default();
    config.bridge.autostart = false;
    config.bridge.evaluators.insert(
        f.repo.canonicalize().unwrap().display().to_string(),
        vec!["exit 1".into()],
    );
    fs::write(
        f.home.join("config.toml"),
        toml::to_string(&config).unwrap(),
    )
    .unwrap();
    let cancel = Cancellation::default();
    let c = cancel.clone();
    let home = f.home.clone();
    let server = tokio::spawn(async move { transport::serve(&home, None, &c).await });
    for _ in 0..100 {
        if f.home.join("run/bridge-endpoint.json").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let result=async{
        let mut fixtures:Vec<Value>=serde_json::from_str(include_str!("../integrations/claude-code/fixtures.json")).unwrap();for fxt in &mut fixtures{fxt["cwd"]=json!(f.repo);}
        assert_eq!(claude::handle(&f.home,fixtures[0].clone()).await?["hookSpecificOutput"]["hookEventName"],"SessionStart");
        claude::handle(&f.home,fixtures[1].clone()).await?;claude::handle(&f.home,fixtures[3].clone()).await?;
        let stop=claude::handle(&f.home,fixtures[5].clone()).await?;assert_eq!(stop["decision"],"block");
        fixtures[5]["stop_hook_active"]=json!(true);assert!(claude::handle(&f.home,fixtures[5].clone()).await?.get("decision").is_none());
        Ok::<_,hardknock::Error>(())
    }.await;
    cancel.cancel();
    server.await.unwrap().unwrap();
    result.unwrap();
    let mut cmd = f.command();
    use std::process::Stdio;
    let mut child = cmd
        .args(["integration-event", "--agent", "claude"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(b"malformed").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({})
    );
}
