// SPDX-License-Identifier: Apache-2.0
//! Codex App Server v2 JSONL adapter; protocol assumptions live only here.
use crate::{
    Error, Result,
    bridge::{protocol::*, transport::BridgeClient},
    cancellation::Cancellation,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};
const TESTED_VERSION: &str = "codex-cli 0.149.1";
fn invalid(s: &str) -> Error {
    Error::InvalidInput(s.into())
}

pub struct CodexAppServerClient {
    child: Child,
    group: Option<nix::unistd::Pid>,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    pending: VecDeque<Value>,
}
impl CodexAppServerClient {
    pub async fn launch(executable: &str) -> Result<Self> {
        let mut child = Command::new(executable)
            .args(["app-server", "--listen", "stdio://"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| invalid("App Server stdin unavailable"))?;
        let stdout = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| invalid("App Server stdout unavailable"))?,
        );
        Ok(Self {
            group: child.id().map(|pid| nix::unistd::Pid::from_raw(pid as i32)),
            child,
            stdin,
            stdout,
            next_id: 0,
            pending: VecDeque::new(),
        })
    }
    pub async fn send(&mut self, message: Value) -> Result<()> {
        let mut data = serde_json::to_vec(&message)?;
        data.push(b'\n');
        self.stdin.write_all(&data).await?;
        Ok(())
    }
    async fn read(&mut self) -> Result<Value> {
        let mut data = Vec::new();
        loop {
            let buffer = self.stdout.fill_buf().await?;
            if buffer.is_empty() {
                return Err(invalid("App Server disconnected"));
            }
            let n = buffer
                .iter()
                .position(|b| *b == b'\n')
                .map(|i| i + 1)
                .unwrap_or(buffer.len());
            if data.len() + n > 8 * 1024 * 1024 {
                return Err(invalid("App Server frame exceeds 8 MiB"));
            }
            let done = buffer[n - 1] == b'\n';
            data.extend_from_slice(&buffer[..n]);
            self.stdout.consume(n);
            if done {
                return Ok(serde_json::from_slice(&data)?);
            }
        }
    }
    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"id":id,"method":method,"params":params}))
            .await?;
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let value = self.read().await?;
                if value["id"] == id && value.get("method").is_none() {
                    if value.get("error").is_some() {
                        return Err(invalid(&format!(
                            "App Server {method} rejected: {}",
                            crate::bridge::privacy::redact(&value["error"].to_string(), 512)
                        )));
                    }
                    return Ok(value["result"].clone());
                }
                if self.pending.len() >= 256 {
                    return Err(invalid("App Server pending event limit exceeded"));
                }
                self.pending.push_back(value);
            }
        })
        .await
        .map_err(|_| invalid("App Server request timeout"))?
    }
    pub async fn initialize(&mut self) -> Result<Value> {
        let response=self.request("initialize",json!({"clientInfo":{"name":"hardknock","title":"Hardknock","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":false}})).await?;
        self.send(json!({"method":"initialized","params":{}}))
            .await?;
        Ok(response)
    }
    pub async fn next_event(&mut self) -> Result<Value> {
        if let Some(v) = self.pending.pop_front() {
            Ok(v)
        } else {
            self.read().await
        }
    }
    pub async fn close(&mut self) -> Result<()> {
        self.kill_group()?;
        self.child.wait().await?;
        self.group = None;
        Ok(())
    }
    fn kill_group(&self) -> Result<()> {
        if let Some(pid) = self.group {
            match nix::sys::signal::killpg(pid, nix::sys::signal::Signal::SIGKILL) {
                Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
                Err(error) => return Err(std::io::Error::from_raw_os_error(error as i32).into()),
            }
        }
        Ok(())
    }
}
impl Drop for CodexAppServerClient {
    fn drop(&mut self) {
        let _ = self.kill_group();
    }
}
pub fn version_supported(version: &str) -> bool {
    version.trim() == TESTED_VERSION
}
pub async fn check(executable: &str, allow_untested: bool) -> Result<AdapterCompatibility> {
    let version = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new(executable)
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| invalid("Codex version check timeout"))??;
    if !version.status.success() {
        return Err(invalid("Codex --version failed"));
    }
    let version = String::from_utf8_lossy(&version.stdout).trim().to_string();
    let supported = version_supported(&version);
    if !supported && !allow_untested {
        return Err(invalid(&format!(
            "Codex {version} is untested; adapter fixtures target {TESTED_VERSION}. Use --allow-untested explicitly for compatibility mode."
        )));
    }
    let schema = tempfile::tempdir()?;
    let status = tokio::time::timeout(
        Duration::from_secs(20),
        Command::new(executable)
            .args(["app-server", "generate-json-schema", "--out"])
            .arg(schema.path())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| invalid("Codex schema detection timeout"))??;
    if !status.status.success() {
        return Err(invalid("Codex App Server cannot generate its schema"));
    }
    for (file, fields) in [
        ("v1/InitializeParams.json", vec!["clientInfo"]),
        (
            "v2/ThreadStartParams.json",
            vec!["cwd", "developerInstructions"],
        ),
        ("v2/TurnStartParams.json", vec!["threadId", "input"]),
        (
            "v2/ItemStartedNotification.json",
            vec!["item", "threadId", "turnId"],
        ),
    ] {
        let value: Value = serde_json::from_slice(&std::fs::read(schema.path().join(file))?)?;
        if fields
            .iter()
            .any(|field| value["properties"].get(field).is_none())
        {
            return Err(invalid(
                "Codex App Server schema lacks a required field; compatibility mode cannot override this",
            ));
        }
    }
    let mut client = CodexAppServerClient::launch(executable).await?;
    let initialized = client.initialize().await;
    let closed = client.close().await;
    initialized?;
    closed?;
    Ok(AdapterCompatibility {
        adapter_version: env!("CARGO_PKG_VERSION").into(),
        external_version: version,
        supported,
        schema_verified: true,
    })
}
pub fn normalize_item(item: &Value) -> Result<Option<NormalizedAction>> {
    let required = |key: &str| {
        item[key]
            .as_str()
            .ok_or_else(|| invalid(&format!("Codex item missing {key}")))
    };
    Ok(match item["type"].as_str() {
        Some("commandExecution") => Some(NormalizedAction::Shell {
            command: required("command")?.into(),
            cwd: required("cwd")?.into(),
        }),
        Some("fileChange") => Some(NormalizedAction::Custom {
            kind: "file_changes".into(),
            payload: json!({"paths":item["changes"].as_array().into_iter().flatten().filter_map(|c|c["path"].as_str()).collect::<Vec<_>>()}),
        }),
        Some("mcpToolCall") => Some(NormalizedAction::ToolCall {
            tool: format!("{}:{}", required("server")?, required("tool")?),
            arguments: json!({"arguments_omitted":true}),
        }),
        _ => None,
    })
}
pub fn normalize_result(item: &Value) -> ActionResult {
    let exit_code = item["exitCode"]
        .as_i64()
        .and_then(|c| i32::try_from(c).ok());
    let success = item["status"] == "completed"
        && exit_code.is_none_or(|c| c == 0)
        && item["error"].is_null();
    ActionResult {
        success,
        exit_code,
        error_class: (!success).then(|| "tool_failure".into()),
        output_summary: item["aggregatedOutput"]
            .as_str()
            .map(|s| crate::bridge::privacy::redact(s, MAX_OUTPUT_BYTES)),
        artifacts: vec![],
    }
}
pub fn approval_response(
    request: &Value,
    decision: &ActionDecision,
    user_approved: Option<bool>,
) -> Value {
    let policy_block = matches!(
        decision,
        ActionDecision::Block {
            authority: DecisionAuthority::UserPolicy | DecisionAuthority::ExternalPolicy,
            ..
        }
    );
    let response = if policy_block {
        "decline"
    } else {
        match user_approved {
            Some(true) => "accept",
            Some(false) => "decline",
            None => "cancel",
        }
    };
    json!({"id":request["id"],"result":{"decision":response}})
}
pub struct RunOptions<'a> {
    pub executable: &'a str,
    pub allow_untested: bool,
    pub resume: Option<&'a str>,
    pub model: Option<&'a str>,
    pub timeout: Duration,
    pub task: &'a str,
}
async fn advisory_event(client: &BridgeClient, session: &str, event: AgentEvent) -> Option<Value> {
    if session.is_empty() {
        return None;
    }
    match client.request(event).await {
        Ok(value) => Some(value),
        Err(_) => {
            eprintln!(
                "Hardknock advisory/recording unavailable (payload omitted); native Codex permissions still apply"
            );
            None
        }
    }
}
pub async fn run(
    home: &Path,
    repo: &Path,
    options: RunOptions<'_>,
    cancel: &Cancellation,
) -> Result<Value> {
    let compatibility = check(options.executable, options.allow_untested).await?;
    let cwd = repo.canonicalize()?;
    let mut bridge = BridgeClient::new(home);
    bridge.timeout = Duration::from_secs(5);
    let external = options
        .resume
        .map(str::to_owned)
        .unwrap_or_else(|| format!("codex-run-{}", uuid::Uuid::new_v4()));
    let started = advisory_event(
        &bridge,
        "registering",
        AgentEvent::SessionStarted(SessionStarted {
            session_id: external,
            agent: AgentIdentity {
                name: "codex".into(),
                version: Some(compatibility.external_version.clone()),
                model: options.model.map(str::to_owned),
                adapter_version: env!("CARGO_PKG_VERSION").into(),
            },
            cwd: cwd.to_string_lossy().into(),
            repository: None,
            // The submitted prompt is not a task summary, even when it is short.
            task: None,
            environment: Default::default(),
        }),
    )
    .await
    .unwrap_or(Value::Null);
    let session = started["hardknock_session_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    bridge.timeout = Duration::from_millis(200);
    let mut server = match CodexAppServerClient::launch(options.executable).await {
        Ok(server) => server,
        Err(error) => {
            let _ = advisory_event(
                &bridge,
                &session,
                AgentEvent::SessionEnded(SessionEnded {
                    hardknock_session_id: session.clone(),
                }),
            )
            .await;
            return Err(error);
        }
    };
    let start = Instant::now();
    let mut observed_turn = None;
    let execution = async {
        server.initialize().await?;
        let mut params = json!({"cwd":cwd});
        if let Some(model) = options.model {
            params["model"] = json!(model);
        }
        // Omit sandbox/approval settings: preserve the user's configured Codex boundaries.
        let method = if let Some(resume) = options.resume {
            params["threadId"] = json!(resume);
            "thread/resume"
        } else {
            "thread/start"
        };
        let thread = server.request(method, params).await?;
        let thread_id = thread["thread"]["id"]
            .as_str()
            .ok_or_else(|| invalid("App Server thread id missing"))?
            .to_string();
        // Add evidence as turn context without replacing configured developer/base instructions.
        let mut input = Vec::new();
        if let Some(context) = started["context_document"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            input.push(json!({"type":"text","text":context,"text_elements":[]}));
        }
        input.push(json!({"type":"text","text":options.task,"text_elements":[]}));
        let turn = server
            .request("turn/start", json!({"threadId":thread_id,"input":input}))
            .await?;
        let turn_id = turn["turn"]["id"]
            .as_str()
            .ok_or_else(|| invalid("App Server turn id missing"))?
            .to_owned();
        observed_turn = Some(turn_id.clone());
        let mut actions = HashMap::new();
        let mut approval_required = false;
        loop {
            let event = server.next_event().await?;
            let method = event["method"].as_str().unwrap_or("");
            let p = &event["params"];
            if event.get("id").is_some() && event.get("method").is_some() {
                if matches!(
                    method,
                    "item/commandExecution/requestApproval" | "item/fileChange/requestApproval"
                ) {
                    let action_id = p["itemId"]
                        .as_str()
                        .ok_or_else(|| invalid("Approval item id missing"))?
                        .to_owned();
                    let action = actions.get(&action_id).cloned().or_else(|| {
                        p["command"]
                            .as_str()
                            .map(|command| NormalizedAction::Shell {
                                command: command.into(),
                                cwd: p["cwd"]
                                    .as_str()
                                    .unwrap_or(cwd.to_str().unwrap_or("/"))
                                    .into(),
                            })
                    });
                    let decision = if let Some(action) = action {
                        advisory_event(
                            &bridge,
                            &session,
                            AgentEvent::ActionProposed(ActionProposed {
                                hardknock_session_id: session.clone(),
                                action_id,
                                action,
                                context: ActionContext {
                                    can_intercept: true,
                                    ..Default::default()
                                },
                            }),
                        )
                        .await
                        .and_then(|v| serde_json::from_value(v).ok())
                        .unwrap_or(ActionDecision::Continue)
                    } else {
                        ActionDecision::Continue
                    };
                    eprintln!(
                        "Codex needs user approval. Hardknock evidence: {}. This noninteractive runner does not grant approval.",
                        decision.message().unwrap_or("none")
                    );
                    approval_required = true;
                    server
                        .send(approval_response(&event, &decision, None))
                        .await?;
                } else {
                    server.send(json!({"id":event["id"],"error":{"code":-32601,"message":"Hardknock client does not implement this request; no approval granted"}})).await?;
                }
                continue;
            }
            if p["threadId"].as_str().is_some_and(|id| id != thread_id)
                || p["turnId"].as_str().is_some_and(|id| id != turn_id)
            {
                continue;
            }
            match method {
                "item/started" | "item/completed" => {
                    let item = &p["item"];
                    if let Some(action) = normalize_item(item)? {
                        let id = item["id"]
                            .as_str()
                            .ok_or_else(|| invalid("Tool item id missing"))?
                            .to_owned();
                        if !actions.contains_key(&id) {
                            let decision = advisory_event(
                                &bridge,
                                &session,
                                AgentEvent::ActionProposed(ActionProposed {
                                    hardknock_session_id: session.clone(),
                                    action_id: id.clone(),
                                    action: action.clone(),
                                    context: Default::default(),
                                }),
                            )
                            .await
                            .unwrap_or_else(|| json!({"decision":"continue"}));
                            if decision["decision"] != "continue" {
                                eprintln!(
                                    "Hardknock observed-action advisory: {}",
                                    crate::bridge::privacy::redact(&decision.to_string(), 1024)
                                );
                            }
                            actions.insert(id.clone(), action.clone());
                        }
                        if method == "item/completed" {
                            advisory_event(
                                &bridge,
                                &session,
                                AgentEvent::ActionCompleted(ActionCompleted {
                                    hardknock_session_id: session.clone(),
                                    action_id: id.clone(),
                                    action: actions[&id].clone(),
                                    result: normalize_result(item),
                                    duration_ms: item["durationMs"].as_u64().unwrap_or(0),
                                }),
                            )
                            .await;
                        }
                    } else if method == "item/completed" && item["type"] == "agentMessage" {
                        // Observe existence, never retain the complete model output.
                        advisory_event(
                            &bridge,
                            &session,
                            AgentEvent::AgentMessage(AgentMessage {
                                hardknock_session_id: session.clone(),
                                summary: "Codex emitted an agent message (content omitted)".into(),
                            }),
                        )
                        .await;
                    }
                }
                "turn/completed" => {
                    if p["turn"]["id"] != turn_id {
                        continue;
                    }
                    let completed = advisory_event(
                        &bridge,
                        &session,
                        AgentEvent::RunCompleted(RunCompleted {
                            termination: if p["turn"]["status"] == "interrupted" {
                                RunTermination::Interrupted
                            } else {
                                RunTermination::Completed
                            },
                            hardknock_session_id: session.clone(),
                            run_id: turn_id.clone(),
                            success: Some(p["turn"]["status"] == "completed"),
                            final_message: None,
                            duration_ms: start.elapsed().as_millis() as u64,
                            external_metadata: Value::Null,
                        }),
                    )
                    .await
                    .unwrap_or_else(|| json!({"status":"unavailable","experience_id":null}));
                    return Ok(
                        json!({"thread_id":thread_id,"turn_id":turn_id,"hardknock_session_id":if session.is_empty() { None } else { Some(&session) },"approval_required":approval_required,"recording":completed,"compatibility":compatibility}),
                    );
                }
                // Diffs are observed but not copied from provider messages; Bridge captures bounded local Git diff.
                "turn/diff/updated" => {}
                // Includes all reasoning events: never request or store chain of thought.
                _ => {}
            }
        }
    };
    let mut termination = RunTermination::Interrupted;
    let result = tokio::select! {
        _ = cancel.cancelled() => Err(invalid("Codex run interrupted")),
        result = tokio::time::timeout(options.timeout, execution) => match result {
            Ok(result) => result,
            Err(_) => { termination = RunTermination::TimedOut; Err(invalid("Codex run timed out")) },
        }
    };
    let close = server.close().await;
    if result.is_err() {
        let _ = advisory_event(
            &bridge,
            &session,
            AgentEvent::RunCompleted(RunCompleted {
                hardknock_session_id: session.clone(),
                run_id: observed_turn
                    .unwrap_or_else(|| format!("aborted-{}", uuid::Uuid::new_v4())),
                success: Some(false),
                final_message: None,
                duration_ms: start.elapsed().as_millis() as u64,
                termination,
                external_metadata: Value::Null,
            }),
        )
        .await;
    }
    let _ = advisory_event(
        &bridge,
        &session,
        AgentEvent::SessionEnded(SessionEnded {
            hardknock_session_id: session.clone(),
        }),
    )
    .await;
    let value = result?;
    close?;
    Ok(value)
}
