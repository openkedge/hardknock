// SPDX-License-Identifier: Apache-2.0
//! Thin native adapters. Domain repositories must stay behind the Bridge.
pub mod claude;
pub mod codex;
pub mod install;
use crate::{
    Result,
    bridge::{protocol::AgentEvent, transport::BridgeClient},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentCapabilities {
    pub session_hooks: bool,
    pub context_injection: bool,
    pub pre_action_interception: bool,
    pub post_action_observation: bool,
    pub structured_tool_events: bool,
    pub run_completion_hook: bool,
    pub native_reality_support: bool,
}
pub fn capabilities() -> Value {
    json!({"agents":[
        {"agent":"claude","mode":"hooks","capabilities":native(),"pre_action_scope":"PreToolUse; advisory by default"},
        {"agent":"codex","mode":"app-server","capabilities":AgentCapabilities{pre_action_interception:false,..native()},"pre_action_scope":"approval requests only; item/started is observation, not interception"},
        {"agent":"hermes","mode":"plugin","capabilities":native(),"pre_action_scope":"pre_tool_call; warnings delivered through session context"},
        {"agent":"openclaw","mode":"plugin","capabilities":native(),"pre_action_scope":"typed before_tool_call; learning advice queued for next prompt"}
    ]})
}
fn native() -> AgentCapabilities {
    AgentCapabilities {
        session_hooks: true,
        context_injection: true,
        pre_action_interception: true,
        post_action_observation: true,
        structured_tool_events: true,
        run_completion_hook: true,
        native_reality_support: false,
    }
}
pub async fn status(home: &Path, doctor: bool) -> Result<Value> {
    let bridge = BridgeClient::new(home)
        .request(AgentEvent::Status)
        .await
        .ok();
    let mut agents = Vec::new();
    for mut item in capabilities()["agents"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        let name = item["agent"].as_str().unwrap_or_default().to_owned();
        let executable = install::find_executable(&name);
        item["executable_found"] = json!(executable.is_some());
        item["installed"] = json!(install::installed(&name, home));
        item["connected"] = json!(
            bridge
                .as_ref()
                .and_then(|b| b["adapters"].as_array())
                .is_some_and(|a| a.contains(&json!(name)))
        );
        if doctor {
            if name == "codex" {
                item["compatibility"] = if let Some(path) = executable {
                    match codex::check(&path.to_string_lossy(), false).await {
                        Ok(compatibility) => serde_json::to_value(compatibility)?,
                        Err(error) => {
                            json!({"supported":false,"error":crate::bridge::privacy::redact(&error.to_string(),512)})
                        }
                    }
                } else {
                    json!({"supported":false,"error":"Codex executable not found"})
                };
            } else {
                item["native_host_load_verified"] = Value::Null;
                item["host_check"] = json!(
                    "Managed files checked; native host enablement/loading requires a live session"
                );
            }
        }
        agents.push(item);
    }
    let configuration = if doctor {
        match crate::bridge::config::Config::load(home) {
            Ok(_) => json!({"valid":true}),
            Err(error) => {
                json!({"valid":false,"error":crate::bridge::privacy::redact(&error.to_string(),512)})
            }
        }
    } else {
        Value::Null
    };
    Ok(
        json!({"doctor":doctor,"configuration":configuration,"bridge_reachable":bridge.is_some(),"agents":agents,"note":"connected means an unended registered session; not a live heartbeat"}),
    )
}
