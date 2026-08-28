// SPDX-License-Identifier: Apache-2.0
//! Claude native command hooks. The adapter never reads transcripts or domain storage.
use crate::{
    Error, Result,
    bridge::{engine::session_key, protocol::*, transport::BridgeClient},
};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
fn required<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::InvalidInput(format!("Claude hook missing {key}")))
}
pub fn normalize(tool: &str, input: &Value, cwd: &str) -> Result<NormalizedAction> {
    Ok(match tool {
        "Bash" => NormalizedAction::Shell {
            command: required(input, "command")?.into(),
            cwd: cwd.into(),
        },
        "Write" | "Edit" => NormalizedAction::FileWrite {
            path: required(input, "file_path")?.into(),
        },
        "Read" => NormalizedAction::FileRead {
            path: required(input, "file_path")?.into(),
        },
        // No file contents, prompts or opaque tool arguments are retained.
        _ => NormalizedAction::ToolCall {
            tool: tool.into(),
            arguments: json!({"arguments_omitted":true}),
        },
    })
}
pub fn hook_response(decision: &ActionDecision) -> Value {
    let mut output = json!({"hookEventName":"PreToolUse"});
    match decision {
        ActionDecision::Continue => return json!({}),
        ActionDecision::Block {
            reason,
            authority: DecisionAuthority::UserPolicy | DecisionAuthority::ExternalPolicy,
        } => {
            output["permissionDecision"] = json!("deny");
            output["permissionDecisionReason"] = json!(reason);
        }
        ActionDecision::RequireApproval { reason, .. } => {
            output["permissionDecision"] = json!("ask");
            output["permissionDecisionReason"] = json!(reason);
        }
        _ => {
            output["additionalContext"] = json!(decision.message().unwrap_or("Hardknock advisory"));
        }
    }
    // Never emit permissionDecision=allow: that would bypass the user's native approval gate.
    json!({"hookSpecificOutput":output})
}
pub fn result(payload: &Value) -> ActionResult {
    let response = &payload["tool_response"];
    let exit_code = response["exit_code"]
        .as_i64()
        .or_else(|| response["exitCode"].as_i64())
        .and_then(|v| i32::try_from(v).ok());
    let failure = payload["hook_event_name"] == "PostToolUseFailure"
        || response["is_error"] == true
        || exit_code.is_some_and(|c| c != 0);
    ActionResult {
        success: !failure,
        exit_code,
        error_class: failure.then(|| "tool_failure".into()),
        output_summary: response["stderr"]
            .as_str()
            .or_else(|| payload["error"].as_str())
            .map(|s| crate::bridge::privacy::redact(s, MAX_OUTPUT_BYTES)),
        artifacts: vec![],
    }
}
pub async fn handle(home: &Path, payload: Value) -> Result<Value> {
    let event = required(&payload, "hook_event_name")?;
    let external = required(&payload, "session_id")?;
    let cwd = required(&payload, "cwd")?;
    let id = session_key("claude", external);
    let config = crate::bridge::config::Config::load(home)?;
    let mut client = BridgeClient::new(home);
    client.timeout = Duration::from_millis(config.bridge.timeout_ms);
    match event {
        "SessionStart" => {
            crate::cli::integrations::ensure_started(home).await?;
            client.timeout = Duration::from_secs(5);
            let response = client
                .request(AgentEvent::SessionStarted(SessionStarted {
                    session_id: external.into(),
                    agent: AgentIdentity::new("claude"),
                    cwd: cwd.into(),
                    repository: None,
                    task: None,
                    environment: Default::default(),
                }))
                .await?;
            Ok(
                json!({"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":response["context_document"].as_str().unwrap_or("")}}),
            )
        }
        "UserPromptSubmit" | "PostCompact" => {
            client.timeout = Duration::from_secs(2);
            // Prompt contents are omitted; never read transcript_path.
            let response = client
                .request(AgentEvent::ContextRequested(ContextRequested {
                    hardknock_session_id: id,
                    task: None,
                }))
                .await?;
            Ok(
                json!({"hookSpecificOutput":{"hookEventName":event,"additionalContext":response["context_document"].as_str().unwrap_or("")}}),
            )
        }
        "PreToolUse" => {
            let action = normalize(
                required(&payload, "tool_name")?,
                &payload["tool_input"],
                cwd,
            )?;
            let decision = client
                .request(AgentEvent::ActionProposed(ActionProposed {
                    hardknock_session_id: id,
                    action_id: required(&payload, "tool_use_id")?.into(),
                    action,
                    context: ActionContext {
                        can_intercept: true,
                        ..Default::default()
                    },
                }))
                .await?;
            Ok(hook_response(&serde_json::from_value(decision)?))
        }
        "PostToolUse" | "PostToolUseFailure" => {
            client
                .request(AgentEvent::ActionCompleted(ActionCompleted {
                    hardknock_session_id: id,
                    action_id: required(&payload, "tool_use_id")?.into(),
                    action: normalize(
                        required(&payload, "tool_name")?,
                        &payload["tool_input"],
                        cwd,
                    )?,
                    result: result(&payload),
                    duration_ms: payload["duration_ms"].as_u64().unwrap_or(0),
                }))
                .await?;
            Ok(json!({}))
        }
        "Stop" => {
            client.timeout = Duration::from_secs(2);
            // Claude does not expose a Stop turn id in all versions. Digest the final message
            // for idempotency without persisting it. Explicit native stop ids win when present.
            let run_id = payload["stop_id"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    format!(
                        "stop-{}",
                        blake3::hash(
                            payload["last_assistant_message"]
                                .as_str()
                                .unwrap_or("no-message")
                                .as_bytes()
                        )
                        .to_hex()
                    )
                });
            let response = client
                .request(AgentEvent::RunCompleted(RunCompleted {
                    termination: RunTermination::Completed,
                    hardknock_session_id: id.clone(),
                    run_id: run_id.clone(),
                    success: None,
                    final_message: None,
                    duration_ms: 0,
                    external_metadata: Value::Null,
                }))
                .await?;
            if config.bridge.max_verification_retries == 0 || payload["stop_hook_active"] == true {
                return Ok(json!({}));
            }
            let mut outcome = response;
            let deadline = tokio::time::Instant::now() + Duration::from_millis(2500);
            for _ in 0..25 {
                if outcome["status"] != "queued" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                outcome = match tokio::time::timeout_at(
                    deadline,
                    client.request(AgentEvent::RunStatus {
                        hardknock_session_id: id.clone(),
                        run_id: run_id.clone(),
                    }),
                )
                .await
                {
                    Ok(result) => result?,
                    Err(_) => return Ok(json!({})),
                };
            }
            if outcome["outcome"] == "failure" {
                Ok(
                    json!({"decision":"block","reason":"Hardknock evaluation failed. Reconsider the remaining failures. One verification continuation is permitted."}),
                )
            } else {
                Ok(json!({}))
            }
        }
        "SessionEnd" => {
            client
                .request(AgentEvent::SessionEnded(SessionEnded {
                    hardknock_session_id: id,
                }))
                .await?;
            Ok(json!({}))
        }
        _ => Err(Error::InvalidInput(
            "Unsupported Claude lifecycle hook".into(),
        )),
    }
}
