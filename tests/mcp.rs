// SPDX-License-Identifier: Apache-2.0

use hardknock::{
    bridge::{
        protocol::AgentEvent,
        transport::{self, BridgeClient},
    },
    cancellation::Cancellation,
    mcp::MCP_PROTOCOL_VERSION,
};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

fn metadata() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": MCP_PROTOCOL_VERSION,
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": {
            "name": "hardknock-cli-conformance",
            "version": "1.0.0"
        }
    })
}

fn request(id: u64, method: &str, params: Value) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    }))
    .unwrap()
}

fn exchange(home: &Path, workspace: &Path, requests: &[String]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_hardknock"))
        .env("HOME", home.parent().unwrap())
        .args([
            "--home",
            home.to_str().unwrap(),
            "mcp",
            "serve",
            "--stdio",
            "--workspace",
            workspace.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    for request in requests {
        writeln!(input, "{request}").unwrap();
    }
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect()
}

#[test]
fn stdio_cli_discovers_and_lists_tools_without_trailing_output_or_state() {
    let temporary = tempfile::tempdir().unwrap();
    let home = temporary.path().join("hardknock-home");
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();

    let lines = exchange(
        &home,
        &workspace,
        &[
            request(1, "server/discover", json!({"_meta":metadata()})),
            request(2, "tools/list", json!({"_meta":metadata()})),
        ],
    );
    assert_eq!(lines.len(), 2);
    let discovered = lines.iter().find(|line| line["id"] == 1).unwrap();
    let listed = lines.iter().find(|line| line["id"] == 2).unwrap();
    assert!(
        discovered["result"]["supportedVersions"]
            .as_array()
            .unwrap()
            .contains(&json!(MCP_PROTOCOL_VERSION))
    );
    assert_eq!(discovered["result"]["resultType"], "complete");
    assert_eq!(discovered["result"]["ttlMs"], 300_000);
    assert_eq!(discovered["result"]["cacheScope"], "public");
    assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 3);
    assert_eq!(listed["result"]["resultType"], "complete");
    assert_eq!(listed["result"]["ttlMs"], 300_000);
    assert_eq!(listed["result"]["cacheScope"], "public");
    assert!(!home.exists());
}

#[tokio::test]
async fn stdio_cli_exits_cleanly_on_interrupt_without_waiting_for_eof() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let temporary = tempfile::tempdir().unwrap();
    let home = temporary.path().join("hardknock-home");
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_hardknock"))
        .env("HOME", temporary.path())
        .args([
            "--home",
            home.to_str().unwrap(),
            "mcp",
            "serve",
            "--stdio",
            "--workspace",
            workspace.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut open_stdin = child.stdin.take().unwrap();
    open_stdin
        .write_all(
            format!(
                "{}\n",
                request(1, "server/discover", json!({"_meta":metadata()}))
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    open_stdin.flush().await.unwrap();
    let mut stdout = tokio::io::BufReader::new(child.stdout.take().unwrap());
    let mut response = String::new();
    tokio::time::timeout(Duration::from_secs(3), stdout.read_line(&mut response))
        .await
        .expect("MCP process did not become ready")
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&response).unwrap()["id"],
        json!(1)
    );
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id().unwrap() as i32),
        nix::sys::signal::Signal::SIGINT,
    )
    .unwrap();
    let output = tokio::time::timeout(Duration::from_secs(3), child.wait_with_output())
        .await
        .expect("MCP process ignored SIGINT")
        .unwrap();
    assert!(
        output.status.success(),
        "status={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    drop(open_stdin);
    assert!(!home.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stateless_stdio_processes_share_an_explicit_bridge_session() {
    let temporary = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let socket_probe = temporary.path().join("socket-probe");
    match std::os::unix::net::UnixListener::bind(&socket_probe) {
        Ok(listener) => {
            drop(listener);
            std::fs::remove_file(&socket_probe).unwrap();
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("skipping live Bridge fixture: local sockets are blocked");
            return;
        }
        Err(error) => panic!("local socket capability probe failed: {error}"),
    }
    let home = temporary.path().join("hardknock-home");
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("README.md"), "fixture\n").unwrap();
    for args in [
        vec!["init"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["config", "user.name", "Hardknock Fixture"],
        vec!["add", "README.md"],
        vec!["commit", "-m", "fixture"],
    ] {
        let output = Command::new("git")
            .args(args)
            .current_dir(&workspace)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let cancellation = Cancellation::default();
    let bridge_cancellation = cancellation.clone();
    let bridge_home = home.clone();
    let mut bridge = Some(tokio::spawn(async move {
        transport::serve(&bridge_home, None, &bridge_cancellation).await
    }));
    let mut readiness = BridgeClient::new(&home);
    readiness.timeout = Duration::from_millis(250);
    let mut ready = false;
    for _ in 0..100 {
        if readiness.request(AgentEvent::Status).await.is_ok() {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    if !ready {
        cancellation.cancel();
        let result = bridge.take().unwrap().await;
        panic!("Bridge did not become ready: {result:?}");
    }

    let query = exchange(
        &home,
        &workspace,
        &[request(
            1,
            "tools/call",
            json!({
                "_meta":metadata(),
                "name":"hardknock_query_context",
                "arguments":{"task":"Inspect the fixture"}
            }),
        )],
    );
    assert!(
        query[0].get("result").is_some(),
        "unexpected MCP query response: {}",
        query[0]
    );
    let query_text = query[0]["result"]["content"][0]["text"].as_str().unwrap();
    let query_result: Value = serde_json::from_str(query_text).unwrap();
    let session = query_result["hardknock_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(query_result["context"].is_object());

    let recorded = exchange(
        &home,
        &workspace,
        &[request(
            2,
            "tools/call",
            json!({
                "_meta":metadata(),
                "name":"hardknock_record_outcome",
                "arguments":{
                    "hardknock_session_id":session,
                    "run_id":"mcp-live-fixture",
                    "success":true,
                    "summary":"fixture completed",
                    "duration_ms":1
                }
            }),
        )],
    );
    assert_eq!(recorded[0]["result"]["isError"], false);
    let client = BridgeClient::new(&home);
    let status = client
        .request(AgentEvent::RunStatus {
            hardknock_session_id: session,
            run_id: "mcp-live-fixture".into(),
        })
        .await
        .unwrap();
    assert!(matches!(
        status["status"].as_str(),
        Some("queued" | "completed")
    ));

    cancellation.cancel();
    bridge.take().unwrap().await.unwrap().unwrap();
}
