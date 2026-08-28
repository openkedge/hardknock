// SPDX-License-Identifier: Apache-2.0
//! Public wire DTOs. All names, units and tags are independent of Rust/domain types.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: &str = "hardknock.bridge.v1";
pub const MAX_EVENT_BYTES: usize = 1024 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 8 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeEnvelope<T> {
    pub protocol_version: String,
    pub request_id: String,
    pub token: String,
    pub payload: T,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeResponse {
    pub protocol_version: String,
    pub request_id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<BridgeError>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeError {
    pub code: String,
    pub message: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentIdentity {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    pub adapter_version: String,
}
impl AgentIdentity {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            version: None,
            model: None,
            adapter_version: env!("CARGO_PKG_VERSION").into(),
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentSummary {
    #[serde(default)]
    pub os: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
    /// Explicit nonsecret version labels only; never an environment-variable dump.
    #[serde(default)]
    pub versions: std::collections::BTreeMap<String, String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryContext {
    pub path: String,
    #[serde(default)]
    pub commit: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionStarted {
    pub session_id: String,
    pub agent: AgentIdentity,
    pub cwd: String,
    #[serde(default)]
    pub repository: Option<RepositoryContext>,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub environment: EnvironmentSummary,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextRequested {
    pub hardknock_session_id: String,
    #[serde(default)]
    pub task: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum NormalizedAction {
    Shell { command: String, cwd: String },
    FileRead { path: String },
    FileWrite { path: String },
    FileDelete { path: String },
    ToolCall { tool: String, arguments: Value },
    Network { method: String, target: String },
    Custom { kind: String, payload: Value },
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionContext {
    #[serde(default)]
    pub no_state_change: bool,
    #[serde(default)]
    pub config_changed: bool,
    /// False for observation-only notifications (e.g. Codex item/started).
    #[serde(default)]
    pub can_intercept: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionProposed {
    pub hardknock_session_id: String,
    pub action_id: String,
    pub action: NormalizedAction,
    #[serde(default)]
    pub context: ActionContext,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionAuthority {
    Experience,
    Reflex,
    UserPolicy,
    ExternalPolicy,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRef {
    pub id: String,
    pub kind: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActionDecision {
    Continue,
    Advise {
        message: String,
        evidence: Vec<EvidenceRef>,
    },
    Warn {
        message: String,
        evidence: Vec<EvidenceRef>,
    },
    Replan {
        reason: String,
        evidence: Vec<EvidenceRef>,
    },
    RequireApproval {
        reason: String,
        evidence: Vec<EvidenceRef>,
    },
    Block {
        reason: String,
        authority: DecisionAuthority,
    },
}
impl ActionDecision {
    pub fn references_lesson(&self, id: &str) -> bool {
        match self {
            Self::Advise { evidence, .. }
            | Self::Warn { evidence, .. }
            | Self::Replan { evidence, .. }
            | Self::RequireApproval { evidence, .. } => evidence
                .iter()
                .any(|reference| reference.kind == "lesson" && reference.id == id),
            Self::Continue | Self::Block { .. } => false,
        }
    }
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::Continue => None,
            Self::Advise { message, .. } | Self::Warn { message, .. } => Some(message),
            Self::Replan { reason, .. }
            | Self::RequireApproval { reason, .. }
            | Self::Block { reason, .. } => Some(reason),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    pub uri: String,
    #[serde(default)]
    pub description: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionResult {
    pub success: bool,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub error_class: Option<String>,
    #[serde(default)]
    pub output_summary: Option<String>,
    #[serde(default)]
    pub artifacts: Vec<ArtifactRef>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionCompleted {
    pub hardknock_session_id: String,
    pub action_id: String,
    pub action: NormalizedAction,
    pub result: ActionResult,
    pub duration_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCompleted {
    pub hardknock_session_id: String,
    /// Stable per turn, including retries of delivery. Never an agent success assertion.
    pub run_id: String,
    #[serde(default)]
    pub success: Option<bool>,
    #[serde(default)]
    pub final_message: Option<String>,
    pub duration_ms: u64,
    #[serde(default)]
    pub termination: RunTermination,
    #[serde(default)]
    pub external_metadata: Value,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunTermination {
    #[default]
    Completed,
    Interrupted,
    TimedOut,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectionReason {
    ContextMismatch,
    EnvironmentChanged,
    ContradictedByObservation,
    AlternativeUnavailable,
    Other,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LessonFeedback {
    pub hardknock_session_id: String,
    pub lesson_id: String,
    pub reason: RejectionReason,
    #[serde(default)]
    pub detail: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMessage {
    pub hardknock_session_id: String,
    /// An explicit conclusion/summary, not a prompt, transcript or reasoning trace.
    pub summary: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionEnded {
    pub hardknock_session_id: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperienceBudget {
    pub max_trials: usize,
    pub max_duration_ms: Option<u64>,
    pub max_agent_runs: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentRequested {
    pub hardknock_session_id: String,
    pub lesson_id: String,
    pub budget: ExperienceBudget,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "event",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum AgentEvent {
    SessionStarted(SessionStarted),
    ContextRequested(ContextRequested),
    ActionProposed(ActionProposed),
    ActionCompleted(ActionCompleted),
    AgentMessage(AgentMessage),
    RunCompleted(RunCompleted),
    SessionEnded(SessionEnded),
    LessonRejected(LessonFeedback),
    ExperimentRequested(ExperimentRequested),
    Status,
    Sessions,
    Inspect {
        hardknock_session_id: String,
    },
    RunStatus {
        hardknock_session_id: String,
        run_id: String,
    },
    Events {
        #[serde(default)]
        after: u64,
    },
    RefreshCache,
    Shutdown,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperienceBrief {
    pub id: String,
    pub kind: String,
    pub summary: String,
    pub confidence: f64,
    pub relevance: f64,
    pub scope: String,
    pub evidence_count: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionStartResponse {
    pub hardknock_session_id: String,
    pub relevant_experience: Vec<ExperienceBrief>,
    pub context_document: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdapterCompatibility {
    pub adapter_version: String,
    pub external_version: String,
    pub supported: bool,
    pub schema_verified: bool,
}
