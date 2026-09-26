// SPDX-License-Identifier: Apache-2.0
//! Bounded, local-only MCP facade over the authenticated Hardknock Bridge.
//!
//! The public surface intentionally contains no effect commit, approval,
//! filesystem, or unrestricted command-execution capability.

use crate::{
    Error, Result,
    bridge::{
        privacy::{redact, redact_value},
        protocol::{
            AgentEvent, AgentIdentity, ContextRequested, EnvironmentSummary, MAX_EVENT_BYTES,
            MAX_OUTPUT_BYTES, RunCompleted, RunTermination, SessionStarted,
        },
        transport::BridgeClient,
    },
    cancellation::Cancellation,
    core::ExperimentId,
};
use async_trait::async_trait;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{self, Read as _},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::task::{AbortHandle, JoinSet};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf},
    sync::mpsc,
};

pub const MCP_PROTOCOL_VERSION: &str = "2026-07-28";
pub const SERVER_NAME: &str = "hardknock";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortableToolEffect {
    ReadOnly,
    RecordsEvidence,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PortableToolDescriptor {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub capability: &'static str,
    pub capability_description: &'static str,
    pub effect: PortableToolEffect,
}

pub const PORTABLE_TOOLS: &[PortableToolDescriptor] = &[
    PortableToolDescriptor {
        name: "hardknock_query_context",
        title: "Query Hardknock Context",
        description: "Retrieve bounded, scoped operational context for the current workspace.",
        capability: "context.query",
        capability_description: "Read scoped evidence-backed context.",
        effect: PortableToolEffect::ReadOnly,
    },
    PortableToolDescriptor {
        name: "hardknock_record_outcome",
        title: "Record Hardknock Outcome",
        description: "Record a bounded execution outcome as local evidence.",
        capability: "outcome.record",
        capability_description: "Record bounded execution outcomes.",
        effect: PortableToolEffect::RecordsEvidence,
    },
    PortableToolDescriptor {
        name: "hardknock_experiment_status",
        title: "Get Hardknock Experiment Status",
        description: "Read bounded progress for an experiment belonging to this MCP session.",
        capability: "experiment.status",
        capability_description: "Read managed experiment status.",
        effect: PortableToolEffect::ReadOnly,
    },
];

const LEGACY_PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const MAX_SUMMARY_BYTES: usize = 2048;
const MAX_IN_FLIGHT_REQUESTS: usize = 32;
const STDIN_CHUNK_BYTES: usize = 8192;
const STDIN_CHANNEL_DEPTH: usize = 8;
#[cfg(not(test))]
const EOF_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(test)]
const EOF_DRAIN_TIMEOUT: Duration = Duration::from_millis(50);

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
const PROTOCOL_VERSION_MISMATCH: i64 = -32022;

#[async_trait]
pub trait McpBackend: Send + Sync {
    async fn request(&self, event: AgentEvent) -> Result<Value>;
}

#[async_trait]
impl McpBackend for BridgeClient {
    async fn request(&self, event: AgentEvent) -> Result<Value> {
        BridgeClient::request(self, event).await
    }
}

#[derive(Clone)]
pub struct LocalMcpBackend {
    home: PathBuf,
    client: BridgeClient,
}

#[async_trait]
impl McpBackend for LocalMcpBackend {
    async fn request(&self, event: AgentEvent) -> Result<Value> {
        crate::cli::integrations::ensure_started(&self.home).await?;
        self.client.request(event).await
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestMode {
    Modern,
    Legacy,
}

/// A stateless modern MCP dispatcher with bounded legacy lifecycle support.
/// Bridge session handles are explicit in modern tool arguments.
pub struct McpServer<B = LocalMcpBackend> {
    backend: Arc<B>,
    cwd: PathBuf,
    legacy: Arc<Mutex<LegacyState>>,
}

impl<B> Clone for McpServer<B> {
    fn clone(&self) -> Self {
        Self {
            backend: Arc::clone(&self.backend),
            cwd: self.cwd.clone(),
            legacy: Arc::clone(&self.legacy),
        }
    }
}

struct LegacyState {
    external_session_id: String,
    legacy_hardknock_session_id: Option<String>,
    legacy_protocol_version: Option<String>,
}

impl McpServer<LocalMcpBackend> {
    pub fn new(home: &Path, cwd: &Path) -> Result<Self> {
        let cwd = cwd.canonicalize()?;
        if !cwd.is_dir() {
            return Err(Error::InvalidInput(
                "MCP workspace must be an existing directory".into(),
            ));
        }
        let mut client = BridgeClient::new(home);
        client.timeout = Duration::from_secs(5);
        let backend = LocalMcpBackend {
            home: home.to_path_buf(),
            client,
        };
        Ok(Self::with_backend(backend, cwd))
    }
}

impl<B> McpServer<B>
where
    B: McpBackend + 'static,
{
    /// Dependency-injection boundary for conformance tests and future
    /// transports. Production callers should normally use [`McpServer::new`].
    pub fn with_backend(backend: B, cwd: PathBuf) -> Self {
        Self {
            backend: Arc::new(backend),
            cwd,
            legacy: Arc::new(Mutex::new(LegacyState {
                external_session_id: format!("mcp-{}", uuid::Uuid::new_v4()),
                legacy_hardknock_session_id: None,
                legacy_protocol_version: None,
            })),
        }
    }

    pub async fn serve<R, W>(
        &self,
        mut reader: R,
        mut writer: W,
        cancel: &Cancellation,
    ) -> io::Result<()>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let mut tasks = JoinSet::new();
        let mut active: HashMap<RequestKey, ActiveRequest> = HashMap::new();
        let mut input_done = false;
        let mut shutdown_requested = false;
        let mut drain_deadline = None;
        let suppress_all_responses = Arc::new(AtomicBool::new(false));
        while !input_done || !tasks.is_empty() {
            tokio::select! {
                _ = cancel.cancelled(), if !shutdown_requested => {
                    shutdown_requested = true;
                    input_done = true;
                    drain_deadline = None;
                    suppress_all_responses.store(true, Ordering::Release);
                    active.clear();
                    tasks.abort_all();
                }
                line = read_bounded_line(&mut reader), if !input_done => match line? {
                    LineRead::Eof => {
                        input_done = true;
                        if !tasks.is_empty() {
                            drain_deadline =
                                Some(tokio::time::Instant::now() + EOF_DRAIN_TIMEOUT);
                        }
                    }
                    LineRead::Oversized => {
                        write_response(
                            &mut writer,
                            &error_response(
                                Value::Null,
                                INVALID_REQUEST,
                                "JSON-RPC request exceeds the protocol limit",
                            ),
                        ).await?;
                    }
                    LineRead::Line(line) if line.iter().all(u8::is_ascii_whitespace) => {}
                    LineRead::Line(line) => {
                        if let Some(cancelled) = cancellation_request_key(&line) {
                            if let Some(request) = active.remove(&cancelled) {
                                request.cancelled.store(true, Ordering::Release);
                                request.abort.abort();
                            }
                            continue;
                        }
                        let identity = request_identity(&line);
                        if let Some((key, id)) = &identity
                            && active.contains_key(key)
                        {
                            write_response(
                                &mut writer,
                                &error_response(
                                    id.clone(),
                                    INVALID_REQUEST,
                                    "JSON-RPC request id is already in flight",
                                ),
                            ).await?;
                            continue;
                        }
                        if tasks.len() >= MAX_IN_FLIGHT_REQUESTS {
                            let id = identity
                                .as_ref()
                                .map(|(_, id)| id.clone())
                                .unwrap_or(Value::Null);
                            write_response(
                                &mut writer,
                                &error_response(
                                    id,
                                    INTERNAL_ERROR,
                                    "Too many MCP requests are in flight",
                                ),
                            ).await?;
                            continue;
                        }
                        let key = identity.map(|(key, _)| key);
                        let task_key = key.clone();
                        let server = self.clone();
                        let request_cancelled = Arc::new(AtomicBool::new(false));
                        let task_cancelled = Arc::clone(&request_cancelled);
                        let abort = tasks.spawn(async move {
                            (task_key, task_cancelled, server.handle_line(&line).await)
                        });
                        if let Some(key) = key {
                            active.insert(
                                key,
                                ActiveRequest {
                                    abort,
                                    cancelled: request_cancelled,
                                },
                            );
                        }
                    }
                },
                _ = tokio::time::sleep_until(
                    drain_deadline.unwrap_or_else(|| {
                        tokio::time::Instant::now() + Duration::from_secs(86_400)
                    })
                ), if drain_deadline.is_some() => {
                    drain_deadline = None;
                    suppress_all_responses.store(true, Ordering::Release);
                    active.clear();
                    tasks.abort_all();
                }
                joined = tasks.join_next(), if !tasks.is_empty() => {
                    match joined {
                        Some(Ok((key, request_cancelled, response))) => {
                            if let Some(key) = key {
                                active.remove(&key);
                            }
                            if !request_cancelled.load(Ordering::Acquire)
                                && !suppress_all_responses.load(Ordering::Acquire)
                                && let Some(response) = response
                            {
                                write_response(&mut writer, &response).await?;
                            }
                        }
                        Some(Err(error)) if error.is_cancelled() => {}
                        Some(Err(_)) => {
                            return Err(io::Error::other("MCP request task failed"));
                        }
                        None => {}
                    }
                }
            }
        }
        Ok(())
    }

    async fn handle_line(&self, line: &[u8]) -> Option<Value> {
        if line.len() > MAX_EVENT_BYTES {
            return Some(error_response(
                Value::Null,
                INVALID_REQUEST,
                "JSON-RPC request exceeds the protocol limit",
            ));
        }
        let request: JsonRpcRequest = match serde_json::from_slice(line) {
            Ok(request) => request,
            Err(_) => {
                return Some(error_response(
                    Value::Null,
                    PARSE_ERROR,
                    "Malformed JSON-RPC request",
                ));
            }
        };
        let id = match request.valid_id() {
            Ok(id) => id,
            Err(message) => {
                return Some(error_response(Value::Null, INVALID_REQUEST, message));
            }
        };

        // The legacy notification is accepted as a no-op. Other notifications
        // are deliberately not used for state-changing tool calls.
        let id = id?;
        Some(match self.dispatch(&request.method, request.params).await {
            Ok(result) => success_response(id, result),
            Err(error) => failure_response(id, error),
        })
    }

    async fn dispatch(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> std::result::Result<Value, RpcFailure> {
        match method {
            "server/discover" => {
                let parsed: MetadataParams = parse_params(params)?;
                validate_modern_metadata(parsed.meta.as_ref())?;
                Ok(discover_result())
            }
            "initialize" => {
                let (protocol_version, result) = initialize_legacy(params)?;
                self.legacy
                    .lock()
                    .expect("MCP legacy state lock")
                    .legacy_protocol_version = Some(protocol_version);
                Ok(result)
            }
            "ping" => {
                let parsed: MetadataParams = parse_params(params)?;
                self.require_request_mode(parsed.meta.as_ref())?;
                Ok(complete_result(json!({})))
            }
            "tools/list" => {
                let parsed: ListToolsParams = parse_params(params)?;
                self.require_request_mode(parsed.meta.as_ref())?;
                let _ = &parsed.cursor;
                Ok(cacheable_result(json!({"tools": tool_definitions()})))
            }
            "tools/call" => {
                let call: CallToolParams = parse_params(params)?;
                let mode = self.require_request_mode(call.meta.as_ref())?;
                self.call_tool(call, mode).await
            }
            _ => Err(RpcFailure::new(
                METHOD_NOT_FOUND,
                "JSON-RPC method is not supported",
            )),
        }
    }

    fn require_request_mode(
        &self,
        meta: Option<&Value>,
    ) -> std::result::Result<RequestMode, RpcFailure> {
        if meta.is_some() {
            validate_modern_metadata(meta)?;
            Ok(RequestMode::Modern)
        } else if self
            .legacy
            .lock()
            .expect("MCP legacy state lock")
            .legacy_protocol_version
            .is_some()
        {
            Ok(RequestMode::Legacy)
        } else {
            Err(RpcFailure::new(
                INVALID_PARAMS,
                "Modern MCP requests require protocol and client-capability metadata",
            ))
        }
    }

    async fn call_tool(
        &self,
        call: CallToolParams,
        mode: RequestMode,
    ) -> std::result::Result<Value, RpcFailure> {
        let value = match call.name.as_str() {
            "hardknock_query_context" => {
                let args: QueryContextArgs = parse_arguments(call.arguments)?;
                args.validate()?;
                let session = self
                    .resolve_session(
                        mode,
                        args.hardknock_session_id.as_deref(),
                        args.task.as_deref(),
                        true,
                    )
                    .await?;
                let context = self
                    .bridge_request(AgentEvent::ContextRequested(ContextRequested {
                        hardknock_session_id: session.clone(),
                        task: args.task,
                    }))
                    .await?;
                json!({
                    "hardknock_session_id": session,
                    "context": context
                })
            }
            "hardknock_record_outcome" => {
                let args: RecordOutcomeArgs = parse_arguments(call.arguments)?;
                args.validate()?;
                let session = self
                    .resolve_session(mode, args.hardknock_session_id.as_deref(), None, false)
                    .await?;
                let outcome = self
                    .bridge_request(AgentEvent::RunCompleted(RunCompleted {
                        hardknock_session_id: session.clone(),
                        run_id: args
                            .run_id
                            .unwrap_or_else(|| format!("mcp-run-{}", uuid::Uuid::new_v4())),
                        success: args.success,
                        final_message: args
                            .summary
                            .map(|summary| redact(&summary, MAX_SUMMARY_BYTES)),
                        duration_ms: args.duration_ms,
                        termination: args.termination.into(),
                        external_metadata: Value::Null,
                    }))
                    .await?;
                json!({"hardknock_session_id":session,"outcome":outcome})
            }
            "hardknock_experiment_status" => {
                let args: ExperimentStatusArgs = parse_arguments(call.arguments)?;
                let session = self
                    .resolve_session(mode, Some(&args.hardknock_session_id), None, false)
                    .await?;
                let status = self
                    .bridge_request(AgentEvent::ExperimentProgress {
                        hardknock_session_id: session.clone(),
                        experiment_id: args.experiment_id,
                        after: args.after,
                    })
                    .await?;
                json!({"hardknock_session_id":session,"status":status})
            }
            _ => {
                return Err(RpcFailure::new(INVALID_PARAMS, "Unknown Hardknock tool"));
            }
        };
        tool_result(value)
    }

    async fn resolve_session(
        &self,
        mode: RequestMode,
        requested: Option<&str>,
        task: Option<&str>,
        create_for_modern: bool,
    ) -> std::result::Result<String, RpcFailure> {
        if let Some(session) = requested {
            validate_session_handle(session)?;
            self.verify_session_binding(session).await?;
            return Ok(session.to_owned());
        }
        if mode == RequestMode::Modern && !create_for_modern {
            return Err(RpcFailure::new(
                INVALID_PARAMS,
                "hardknock_session_id is required for this stateless MCP tool call",
            ));
        }
        if mode == RequestMode::Legacy {
            let existing = self
                .legacy
                .lock()
                .expect("MCP legacy state lock")
                .legacy_hardknock_session_id
                .clone();
            if let Some(session) = existing {
                return Ok(session);
            }
        }
        let external_session_id = if mode == RequestMode::Legacy {
            self.legacy
                .lock()
                .expect("MCP legacy state lock")
                .external_session_id
                .clone()
        } else {
            format!("mcp-{}", uuid::Uuid::new_v4())
        };
        let response = self
            .bridge_request(AgentEvent::SessionStarted(SessionStarted {
                session_id: external_session_id,
                agent: AgentIdentity::new("mcp"),
                cwd: self.cwd.to_string_lossy().into_owned(),
                repository: None,
                task: task.map(|value| redact(value, 512)),
                environment: EnvironmentSummary::default(),
            }))
            .await?;
        let session = response
            .get("hardknock_session_id")
            .and_then(Value::as_str)
            .filter(|value| validate_session_handle(value).is_ok())
            .ok_or_else(|| {
                RpcFailure::new(
                    INTERNAL_ERROR,
                    "Hardknock Bridge returned an invalid session",
                )
            })?
            .to_owned();
        if mode == RequestMode::Legacy {
            self.legacy
                .lock()
                .expect("MCP legacy state lock")
                .legacy_hardknock_session_id = Some(session.clone());
        }
        Ok(session)
    }

    async fn verify_session_binding(&self, session: &str) -> std::result::Result<(), RpcFailure> {
        let inspected = self
            .backend
            .request(AgentEvent::Inspect {
                hardknock_session_id: session.to_owned(),
            })
            .await
            .map_err(|_| {
                RpcFailure::new(
                    INVALID_PARAMS,
                    "Hardknock session is unavailable for this MCP workspace",
                )
            })?;
        let summary = inspected.get("session").and_then(Value::as_object);
        let bound = summary.is_some_and(|summary| {
            summary.get("id").and_then(Value::as_str) == Some(session)
                && summary.get("agent").and_then(Value::as_str) == Some("mcp")
                && summary
                    .get("cwd")
                    .and_then(Value::as_str)
                    .is_some_and(|cwd| Path::new(cwd) == self.cwd)
                && summary.get("ended").and_then(Value::as_bool) == Some(false)
        });
        if !bound {
            return Err(RpcFailure::new(
                INVALID_PARAMS,
                "Hardknock session does not belong to this MCP workspace",
            ));
        }
        Ok(())
    }

    async fn bridge_request(&self, event: AgentEvent) -> std::result::Result<Value, RpcFailure> {
        self.backend
            .request(event)
            .await
            .map_err(|_| RpcFailure::new(INTERNAL_ERROR, "Hardknock Bridge request failed"))
    }
}

struct ActiveRequest {
    abort: AbortHandle,
    cancelled: Arc<AtomicBool>,
}

pub async fn serve_stdio(home: &Path, cwd: &Path, cancel: &Cancellation) -> Result<()> {
    let server = McpServer::new(home, cwd)?;
    let stdin = CancellableStdin::spawn()?;
    server
        .serve(
            tokio::io::BufReader::new(stdin),
            tokio::io::stdout(),
            cancel,
        )
        .await
        .map_err(Error::Io)
}

/// Adapts blocking process stdin without enrolling the read in Tokio's
/// blocking pool. Dropping the receiver on cancellation lets the runtime
/// terminate immediately even when the peer keeps stdin open.
struct CancellableStdin {
    receiver: mpsc::Receiver<io::Result<Vec<u8>>>,
    pending: Option<PendingChunk>,
}

struct PendingChunk {
    bytes: Vec<u8>,
    offset: usize,
}

impl CancellableStdin {
    fn spawn() -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel(STDIN_CHANNEL_DEPTH);
        std::thread::Builder::new()
            .name("hardknock-mcp-stdin".into())
            .spawn(move || {
                let stdin = io::stdin();
                let mut stdin = stdin.lock();
                loop {
                    let mut chunk = vec![0_u8; STDIN_CHUNK_BYTES];
                    match stdin.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(length) => {
                            chunk.truncate(length);
                            if sender.blocking_send(Ok(chunk)).is_err() {
                                break;
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error) => {
                            let _ = sender.blocking_send(Err(error));
                            break;
                        }
                    }
                }
            })?;
        Ok(Self {
            receiver,
            pending: None,
        })
    }
}

impl AsyncRead for CancellableStdin {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if buffer.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        loop {
            if self.pending.is_some() {
                let complete = {
                    let pending = self.pending.as_mut().expect("pending stdin chunk");
                    let remaining = &pending.bytes[pending.offset..];
                    let length = remaining.len().min(buffer.remaining());
                    buffer.put_slice(&remaining[..length]);
                    pending.offset += length;
                    pending.offset == pending.bytes.len()
                };
                if complete {
                    self.pending = None;
                }
                return Poll::Ready(Ok(()));
            }
            match Pin::new(&mut self.receiver).poll_recv(context) {
                Poll::Ready(Some(Ok(bytes))) if bytes.is_empty() => {}
                Poll::Ready(Some(Ok(bytes))) => {
                    self.pending = Some(PendingChunk { bytes, offset: 0 });
                }
                Poll::Ready(Some(Err(error))) => return Poll::Ready(Err(error)),
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRpcRequest {
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RequestKey(String);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CancellationParams {
    request_id: Value,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default, rename = "_meta")]
    meta: Option<Value>,
}

impl JsonRpcRequest {
    fn valid_id(&self) -> std::result::Result<Option<Value>, &'static str> {
        if self.jsonrpc != "2.0" || self.method.is_empty() {
            return Err("Expected a JSON-RPC 2.0 request with a method");
        }
        if self
            .id
            .as_ref()
            .is_some_and(|id| !matches!(id, Value::String(_) | Value::Number(_)))
        {
            return Err("JSON-RPC id must be a string or number");
        }
        Ok(self.id.clone())
    }
}

fn request_key(id: &Value) -> Option<RequestKey> {
    matches!(id, Value::String(_) | Value::Number(_))
        .then(|| serde_json::to_string(id).ok().map(RequestKey))
        .flatten()
}

fn request_identity(line: &[u8]) -> Option<(RequestKey, Value)> {
    let request: JsonRpcRequest = serde_json::from_slice(line).ok()?;
    let id = request.valid_id().ok()??;
    request_key(&id).map(|key| (key, id))
}

fn cancellation_request_key(line: &[u8]) -> Option<RequestKey> {
    let request: JsonRpcRequest = serde_json::from_slice(line).ok()?;
    if request.method != "notifications/cancelled" || request.valid_id().ok()?.is_some() {
        return None;
    }
    let params: CancellationParams =
        serde_json::from_value(request.params.unwrap_or_else(|| json!({}))).ok()?;
    if params.reason.as_ref().is_some_and(|reason| {
        reason.len() > 1024 || reason.chars().any(|character| character.is_control())
    }) {
        return None;
    }
    let _ = params.meta;
    request_key(&params.request_id)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InitializeParams {
    protocol_version: String,
    capabilities: Value,
    client_info: ClientInfo,
    #[serde(default, rename = "_meta")]
    meta: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataParams {
    #[serde(default, rename = "_meta")]
    meta: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientInfo {
    name: String,
    version: String,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListToolsParams {
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default, rename = "_meta")]
    meta: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CallToolParams {
    name: String,
    #[serde(default = "empty_object")]
    arguments: Value,
    #[serde(default, rename = "_meta")]
    meta: Option<Value>,
}

fn empty_object() -> Value {
    json!({})
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryContextArgs {
    #[serde(default)]
    hardknock_session_id: Option<String>,
    #[serde(default)]
    task: Option<String>,
}

impl QueryContextArgs {
    fn validate(&self) -> std::result::Result<(), RpcFailure> {
        if self.task.as_ref().is_some_and(|task| {
            task.len() > 512 || task.chars().any(|character| character.is_control())
        }) || self
            .hardknock_session_id
            .as_deref()
            .is_some_and(|session| validate_session_handle(session).is_err())
        {
            return Err(RpcFailure::new(
                INVALID_PARAMS,
                "Context task or session handle is invalid",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordOutcomeArgs {
    #[serde(default)]
    hardknock_session_id: Option<String>,
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    duration_ms: u64,
    #[serde(default)]
    termination: OutcomeTermination,
}

impl RecordOutcomeArgs {
    fn validate(&self) -> std::result::Result<(), RpcFailure> {
        if self
            .hardknock_session_id
            .as_deref()
            .is_some_and(|session| validate_session_handle(session).is_err())
            || self.run_id.as_ref().is_some_and(|value| {
                value.is_empty() || value.len() > 256 || value.chars().any(char::is_control)
            })
            || self
                .summary
                .as_ref()
                .is_some_and(|value| value.len() > MAX_SUMMARY_BYTES)
        {
            return Err(RpcFailure::new(
                INVALID_PARAMS,
                "Outcome identifiers or summary exceed their limits",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OutcomeTermination {
    #[default]
    Completed,
    Interrupted,
    TimedOut,
}

impl From<OutcomeTermination> for RunTermination {
    fn from(value: OutcomeTermination) -> Self {
        match value {
            OutcomeTermination::Completed => Self::Completed,
            OutcomeTermination::Interrupted => Self::Interrupted,
            OutcomeTermination::TimedOut => Self::TimedOut,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExperimentStatusArgs {
    hardknock_session_id: String,
    experiment_id: ExperimentId,
    #[serde(default)]
    after: u64,
}

#[derive(Debug)]
struct RpcFailure {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl RpcFailure {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: redact(&message.into(), 256),
            data: None,
        }
    }

    fn version_mismatch(requested: &str) -> Self {
        Self {
            code: PROTOCOL_VERSION_MISMATCH,
            message: "Unsupported MCP protocol version".into(),
            data: Some(json!({
                "supported": supported_versions(),
                "requested": requested
            })),
        }
    }
}

fn initialize_legacy(params: Option<Value>) -> std::result::Result<(String, Value), RpcFailure> {
    let params: InitializeParams = parse_params(params)?;
    if params.client_info.name.is_empty()
        || params.client_info.name.len() > 128
        || params.client_info.version.len() > 128
        || params
            .client_info
            .title
            .as_ref()
            .is_some_and(|title| title.len() > 128)
    {
        return Err(RpcFailure::new(
            INVALID_PARAMS,
            "MCP client metadata is invalid",
        ));
    }
    let protocol_version = if LEGACY_PROTOCOL_VERSIONS.contains(&params.protocol_version.as_str()) {
        params.protocol_version.as_str()
    } else {
        return Err(RpcFailure::version_mismatch(&params.protocol_version));
    };
    let _ = (&params.capabilities, &params.meta);
    Ok((
        protocol_version.to_owned(),
        json!({
            "protocolVersion": protocol_version,
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": server_info(),
            "instructions": "Use the three bounded Hardknock tools for context, outcomes, and read-only experiment status."
        }),
    ))
}

fn validate_modern_metadata(meta: Option<&Value>) -> std::result::Result<(), RpcFailure> {
    let meta = meta
        .and_then(Value::as_object)
        .ok_or_else(|| RpcFailure::new(INVALID_PARAMS, "MCP request metadata is required"))?;
    let protocol_version = meta
        .get("io.modelcontextprotocol/protocolVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            RpcFailure::new(
                INVALID_PARAMS,
                "MCP request metadata must include a protocol version",
            )
        })?;
    if protocol_version != MCP_PROTOCOL_VERSION {
        return Err(RpcFailure::version_mismatch(protocol_version));
    }
    if !meta
        .get("io.modelcontextprotocol/clientCapabilities")
        .is_some_and(Value::is_object)
    {
        return Err(RpcFailure::new(
            INVALID_PARAMS,
            "MCP request metadata must include client capabilities",
        ));
    }
    if let Some(client_info) = meta.get("io.modelcontextprotocol/clientInfo") {
        let client = client_info.as_object().ok_or_else(|| {
            RpcFailure::new(INVALID_PARAMS, "MCP client information must be an object")
        })?;
        for field in ["name", "version"] {
            if !client
                .get(field)
                .and_then(Value::as_str)
                .is_some_and(|value| {
                    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
                })
            {
                return Err(RpcFailure::new(
                    INVALID_PARAMS,
                    "MCP client name and version must be bounded strings",
                ));
            }
        }
    }
    Ok(())
}

fn server_info() -> Value {
    json!({
        "name": SERVER_NAME,
        "title": "Hardknock",
        "version": env!("CARGO_PKG_VERSION")
    })
}

fn discover_result() -> Value {
    cacheable_result(json!({
        "supportedVersions": supported_versions(),
        "capabilities": {"tools": {"listChanged": false}}
    }))
}

fn supported_versions() -> Vec<&'static str> {
    std::iter::once(MCP_PROTOCOL_VERSION)
        .chain(LEGACY_PROTOCOL_VERSIONS.iter().copied())
        .collect()
}

fn complete_result(mut value: Value) -> Value {
    let object = value
        .as_object_mut()
        .expect("MCP result payloads are always objects");
    object.insert("resultType".into(), Value::String("complete".into()));
    object.insert(
        "_meta".into(),
        json!({"io.modelcontextprotocol/serverInfo":server_info()}),
    );
    value
}

fn cacheable_result(mut value: Value) -> Value {
    let object = value
        .as_object_mut()
        .expect("MCP cacheable result payloads are always objects");
    object.insert("ttlMs".into(), json!(300_000));
    object.insert("cacheScope".into(), Value::String("public".into()));
    complete_result(value)
}

fn validate_session_handle(session: &str) -> std::result::Result<(), RpcFailure> {
    if session.is_empty()
        || session.len() > 256
        || session.chars().any(char::is_control)
        || !session.starts_with("hk-s-")
    {
        return Err(RpcFailure::new(
            INVALID_PARAMS,
            "Hardknock session handle is invalid",
        ));
    }
    Ok(())
}

fn parse_params<T: DeserializeOwned>(params: Option<Value>) -> std::result::Result<T, RpcFailure> {
    serde_json::from_value(params.unwrap_or_else(|| json!({})))
        .map_err(|error| RpcFailure::new(INVALID_PARAMS, format!("Invalid parameters: {error}")))
}

fn parse_arguments<T: DeserializeOwned>(arguments: Value) -> std::result::Result<T, RpcFailure> {
    if match serde_json::to_vec(&arguments) {
        Ok(bytes) => bytes.len() > MAX_EVENT_BYTES,
        Err(_) => true,
    } {
        return Err(RpcFailure::new(
            INVALID_PARAMS,
            "Tool arguments exceed the protocol limit",
        ));
    }
    serde_json::from_value(arguments).map_err(|error| {
        RpcFailure::new(INVALID_PARAMS, format!("Invalid tool arguments: {error}"))
    })
}

fn tool_result(mut value: Value) -> std::result::Result<Value, RpcFailure> {
    redact_value(&mut value);
    let text = bounded_json(value)?;
    Ok(complete_result(json!({
        "content": [{"type": "text", "text": text}],
        "isError": false
    })))
}

fn bounded_json(value: Value) -> std::result::Result<String, RpcFailure> {
    let encoded = serde_json::to_string(&value)
        .map_err(|_| RpcFailure::new(INTERNAL_ERROR, "Could not encode tool result"))?;
    if encoded.len() <= MAX_OUTPUT_BYTES {
        return Ok(encoded);
    }
    let marker = json!({
        "truncated": true,
        "summary": redact(&encoded, MAX_OUTPUT_BYTES.saturating_sub(1024))
    });
    let encoded = serde_json::to_string(&marker)
        .map_err(|_| RpcFailure::new(INTERNAL_ERROR, "Could not bound tool result"))?;
    if encoded.len() > MAX_OUTPUT_BYTES {
        return Err(RpcFailure::new(
            INTERNAL_ERROR,
            "Tool result exceeded the protocol limit",
        ));
    }
    Ok(encoded)
}

fn success_response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": redact(message, 256)}
    })
}

fn failure_response(id: Value, failure: RpcFailure) -> Value {
    let mut response = error_response(id, failure.code, &failure.message);
    if let Some(data) = failure.data {
        response["error"]["data"] = data;
    }
    response
}

async fn write_response(
    writer: &mut (impl AsyncWrite + Unpin),
    response: &Value,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(response)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if bytes.len() > MAX_EVENT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "MCP response exceeds the protocol limit",
        ));
    }
    writer.write_all(&bytes).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await
}

enum LineRead {
    Eof,
    Line(Vec<u8>),
    Oversized,
}

async fn read_bounded_line(reader: &mut (impl AsyncBufRead + Unpin)) -> io::Result<LineRead> {
    let mut line = Vec::new();
    let mut oversized = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if oversized {
                Ok(LineRead::Oversized)
            } else if line.is_empty() {
                Ok(LineRead::Eof)
            } else {
                Ok(LineRead::Line(line))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |position| position + 1);
        if !oversized {
            let payload_len = newline.unwrap_or(available.len());
            if line.len().saturating_add(payload_len) > MAX_EVENT_BYTES {
                oversized = true;
                line.clear();
            } else {
                line.extend_from_slice(&available[..payload_len]);
            }
        }
        reader.consume(consumed);
        if newline.is_some() {
            return if oversized {
                Ok(LineRead::Oversized)
            } else {
                Ok(LineRead::Line(line))
            };
        }
    }
}

fn tool_definitions() -> Vec<Value> {
    PORTABLE_TOOLS
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "title": tool.title,
                "description": tool.description,
                "inputSchema": tool_input_schema(tool.name)
            })
        })
        .collect()
}

fn tool_input_schema(name: &str) -> Value {
    match name {
        "hardknock_query_context" => json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "hardknock_session_id": {
                    "type": "string",
                    "pattern": "^hk-s-",
                    "maxLength": 256,
                    "description": "Optional existing session handle; omitted to create a new scoped session."
                },
                "task": {"type": "string", "maxLength": 512}
            }
        }),
        "hardknock_record_outcome" => json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["hardknock_session_id"],
            "properties": {
                "hardknock_session_id": {
                    "type": "string",
                    "pattern": "^hk-s-",
                    "maxLength": 256
                },
                "run_id": {"type": "string", "maxLength": 256},
                "success": {"type": ["boolean", "null"]},
                "summary": {"type": "string", "maxLength": MAX_SUMMARY_BYTES},
                "duration_ms": {"type": "integer", "minimum": 0},
                "termination": {
                    "type": "string",
                    "enum": ["completed", "interrupted", "timed_out"],
                    "default": "completed"
                }
            }
        }),
        "hardknock_experiment_status" => json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["hardknock_session_id", "experiment_id"],
            "properties": {
                "hardknock_session_id": {
                    "type": "string",
                    "pattern": "^hk-s-",
                    "maxLength": 256
                },
                "experiment_id": {
                    "type": "string",
                    "pattern": "^experiment-[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
                },
                "after": {"type": "integer", "minimum": 0}
            }
        }),
        _ => unreachable!("portable tool descriptor must have an input schema"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::AsyncReadExt;

    #[derive(Clone, Default)]
    struct FakeBackend {
        events: Arc<Mutex<Vec<AgentEvent>>>,
    }

    #[async_trait]
    impl McpBackend for FakeBackend {
        async fn request(&self, event: AgentEvent) -> Result<Value> {
            let response = match &event {
                AgentEvent::Inspect {
                    hardknock_session_id,
                } => {
                    let (agent, cwd) = match hardknock_session_id.as_str() {
                        "hk-s-foreign-agent" => ("claude", "/tmp/hardknock-mcp-test"),
                        "hk-s-foreign-workspace" => ("mcp", "/tmp/other-workspace"),
                        _ => ("mcp", "/tmp/hardknock-mcp-test"),
                    };
                    json!({
                        "session": {
                            "id": hardknock_session_id,
                            "agent": agent,
                            "cwd": cwd,
                            "ended": false
                        }
                    })
                }
                _ => json!({"hardknock_session_id": "hk-s-test", "accepted": true}),
            };
            self.events.lock().expect("events lock").push(event);
            Ok(response)
        }
    }

    fn server() -> McpServer<FakeBackend> {
        McpServer::with_backend(
            FakeBackend::default(),
            PathBuf::from("/tmp/hardknock-mcp-test"),
        )
    }

    fn request(id: u64, method: &str, params: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))
        .unwrap()
    }

    fn modern_metadata() -> Value {
        json!({
            "io.modelcontextprotocol/protocolVersion": MCP_PROTOCOL_VERSION,
            "io.modelcontextprotocol/clientCapabilities": {},
            "io.modelcontextprotocol/clientInfo": {
                "name": "hardknock-test-client",
                "version": "1.0.0"
            }
        })
    }

    fn modern_request(id: u64, method: &str, mut params: Value) -> Vec<u8> {
        params
            .as_object_mut()
            .expect("modern params object")
            .insert("_meta".into(), modern_metadata());
        request(id, method, params)
    }

    #[tokio::test]
    async fn discover_and_list_expose_only_the_three_safe_tools() {
        let server = server();
        let discovered = server
            .handle_line(&modern_request(1, "server/discover", json!({})))
            .await
            .unwrap();
        assert_eq!(
            discovered["result"]["supportedVersions"],
            json!(supported_versions())
        );
        assert_eq!(discovered["result"]["resultType"], "complete");
        assert_eq!(discovered["result"]["ttlMs"], 300_000);
        assert_eq!(discovered["result"]["cacheScope"], "public");
        assert_eq!(
            discovered["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            SERVER_NAME
        );

        let listed = server
            .handle_line(&modern_request(2, "tools/list", json!({})))
            .await
            .unwrap();
        assert_eq!(listed["result"]["resultType"], "complete");
        assert_eq!(listed["result"]["ttlMs"], 300_000);
        assert_eq!(listed["result"]["cacheScope"], "public");
        let tools = listed["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
        let names = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "hardknock_query_context",
                "hardknock_record_outcome",
                "hardknock_experiment_status"
            ]
        );
        for tool in tools {
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        }
        let encoded = serde_json::to_string(tools).unwrap();
        for unsafe_name in [
            "effect_commit",
            "effect_discard",
            "approval",
            "filesystem_read",
            "filesystem_write",
            "hardknock_request_experiment",
            "run_command",
            "shell_command",
        ] {
            assert!(!encoded.contains(unsafe_name), "{unsafe_name} was exposed");
        }
    }

    #[tokio::test]
    async fn modern_requests_require_metadata_and_legacy_initialize_remains_bounded() {
        let server = server();
        let ping = server
            .handle_line(&modern_request(0, "ping", json!({})))
            .await
            .unwrap();
        assert_eq!(ping["result"]["resultType"], "complete");

        let missing_metadata = server
            .handle_line(&request(1, "tools/list", json!({})))
            .await
            .unwrap();
        assert_eq!(missing_metadata["error"]["code"], INVALID_PARAMS);

        let without_optional_client_info = server
            .handle_line(&request(
                10,
                "tools/list",
                json!({
                    "_meta": {
                        "io.modelcontextprotocol/protocolVersion": MCP_PROTOCOL_VERSION,
                        "io.modelcontextprotocol/clientCapabilities": {}
                    }
                }),
            ))
            .await
            .unwrap();
        assert!(without_optional_client_info.get("result").is_some());

        let initialized = server
            .handle_line(&request(
                2,
                "initialize",
                json!({
                    "protocolVersion": LEGACY_PROTOCOL_VERSIONS[0],
                    "capabilities": {},
                    "clientInfo": {"name": "legacy-test", "version": "1"}
                }),
            ))
            .await
            .unwrap();
        assert_eq!(
            initialized["result"]["protocolVersion"],
            LEGACY_PROTOCOL_VERSIONS[0]
        );
        let listed = server
            .handle_line(&request(3, "tools/list", json!({})))
            .await
            .unwrap();
        assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 3);
        let notification = br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert!(server.handle_line(notification).await.is_none());

        let current_initialize = server
            .handle_line(&request(
                4,
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "wrong-lifecycle", "version": "1"}
                }),
            ))
            .await
            .unwrap();
        assert_eq!(
            current_initialize["error"]["code"],
            PROTOCOL_VERSION_MISMATCH
        );
        assert_eq!(
            current_initialize["error"]["data"]["requested"],
            MCP_PROTOCOL_VERSION
        );
        assert!(
            current_initialize["error"]["data"]["supported"]
                .as_array()
                .unwrap()
                .contains(&json!(MCP_PROTOCOL_VERSION))
        );
    }

    #[tokio::test]
    async fn malformed_oversized_and_unknown_requests_return_standard_errors() {
        let server = server();
        let malformed = server.handle_line(b"{").await.unwrap();
        assert_eq!(malformed["error"]["code"], PARSE_ERROR);

        let oversized = vec![b'x'; MAX_EVENT_BYTES + 1];
        let response = server.handle_line(&oversized).await.unwrap();
        assert_eq!(response["error"]["code"], INVALID_REQUEST);

        let unknown = server
            .handle_line(&modern_request(
                2,
                "tools/call",
                json!({"name": "hardknock_commit_effect", "arguments": {}}),
            ))
            .await
            .unwrap();
        assert_eq!(unknown["error"]["code"], INVALID_PARAMS);
    }

    #[tokio::test]
    async fn typed_arguments_reject_unknown_fields_before_bridge_delivery() {
        let server = server();
        let response = server
            .handle_line(&modern_request(
                1,
                "tools/call",
                json!({
                    "name": "hardknock_query_context",
                    "arguments": {"task": "inspect", "command": "rm -rf /"}
                }),
            ))
            .await
            .unwrap();
        assert_eq!(response["error"]["code"], INVALID_PARAMS);
        assert!(
            server
                .backend
                .events
                .lock()
                .expect("events lock")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn modern_tool_calls_use_explicit_stateless_session_handles() {
        let server = server();
        let first = server
            .handle_line(&modern_request(
                1,
                "tools/call",
                json!({
                    "name": "hardknock_query_context"
                }),
            ))
            .await
            .unwrap();
        let text = first["result"]["content"][0]["text"].as_str().unwrap();
        let result: Value = serde_json::from_str(text).unwrap();
        let session = result["hardknock_session_id"].as_str().unwrap();
        let second = server
            .handle_line(&modern_request(
                2,
                "tools/call",
                json!({
                    "name": "hardknock_query_context",
                    "arguments": {
                        "hardknock_session_id": session,
                        "task": "inspect state again"
                    }
                }),
            ))
            .await
            .unwrap();
        assert!(second.get("result").is_some());
        let events = server.backend.events.lock().expect("events lock");
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::SessionStarted(_)))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::ContextRequested(_)))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn state_changing_modern_tools_require_a_session_handle() {
        let server = server();
        let response = server
            .handle_line(&modern_request(
                1,
                "tools/call",
                json!({
                    "name": "hardknock_record_outcome",
                    "arguments": {"success": true}
                }),
            ))
            .await
            .unwrap();
        assert_eq!(response["error"]["code"], INVALID_PARAMS);
        assert!(
            server
                .backend
                .events
                .lock()
                .expect("events lock")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn explicit_session_handles_are_bound_to_agent_and_workspace() {
        let server = server();
        for session in ["hk-s-foreign-agent", "hk-s-foreign-workspace"] {
            let response = server
                .handle_line(&modern_request(
                    1,
                    "tools/call",
                    json!({
                        "name": "hardknock_query_context",
                        "arguments": {"hardknock_session_id": session}
                    }),
                ))
                .await
                .unwrap();
            assert_eq!(response["error"]["code"], INVALID_PARAMS);
        }
        assert!(
            server
                .backend
                .events
                .lock()
                .expect("events lock")
                .iter()
                .all(|event| matches!(event, AgentEvent::Inspect { .. }))
        );
    }

    #[tokio::test]
    async fn cancellation_notifications_abort_in_flight_requests_without_a_response() {
        #[derive(Clone)]
        struct SlowBackend;

        #[async_trait]
        impl McpBackend for SlowBackend {
            async fn request(&self, _event: AgentEvent) -> Result<Value> {
                std::future::pending().await
            }
        }

        let server = McpServer::with_backend(SlowBackend, PathBuf::from("/tmp/hardknock-mcp-test"));
        let mut input =
            modern_request(42, "tools/call", json!({"name": "hardknock_query_context"}));
        input.push(b'\n');
        input.extend_from_slice(
            br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":42,"reason":"client stopped waiting"}}"#,
        );
        input.push(b'\n');
        let cancel = Cancellation::default();
        let (writer, mut captured_reader) = tokio::io::duplex(1024);
        tokio::time::timeout(
            Duration::from_secs(1),
            server.serve(tokio::io::BufReader::new(&input[..]), writer, &cancel),
        )
        .await
        .expect("cancelled request should not hold stdio open")
        .unwrap();
        let mut captured = Vec::new();
        captured_reader.read_to_end(&mut captured).await.unwrap();
        assert!(captured.is_empty());
    }

    #[tokio::test]
    async fn eof_aborts_a_backend_that_does_not_finish_within_the_drain_window() {
        #[derive(Clone)]
        struct StuckBackend;

        #[async_trait]
        impl McpBackend for StuckBackend {
            async fn request(&self, _event: AgentEvent) -> Result<Value> {
                std::future::pending().await
            }
        }

        let server =
            McpServer::with_backend(StuckBackend, PathBuf::from("/tmp/hardknock-mcp-test"));
        let mut input =
            modern_request(42, "tools/call", json!({"name": "hardknock_query_context"}));
        input.push(b'\n');
        let cancel = Cancellation::default();
        let (writer, mut captured_reader) = tokio::io::duplex(1024);
        tokio::time::timeout(
            Duration::from_secs(1),
            server.serve(tokio::io::BufReader::new(&input[..]), writer, &cancel),
        )
        .await
        .expect("EOF drain must be bounded")
        .unwrap();
        let mut captured = Vec::new();
        captured_reader.read_to_end(&mut captured).await.unwrap();
        assert!(captured.is_empty());
    }

    #[tokio::test]
    async fn bounded_reader_discards_an_oversized_line_and_recovers() {
        let mut input = vec![b'x'; MAX_EVENT_BYTES + 1];
        input.extend_from_slice(b"\n{}\n");
        let mut reader = tokio::io::BufReader::new(&input[..]);
        assert!(matches!(
            read_bounded_line(&mut reader).await.unwrap(),
            LineRead::Oversized
        ));
        match read_bounded_line(&mut reader).await.unwrap() {
            LineRead::Line(line) => assert_eq!(line, b"{}"),
            _ => panic!("expected next bounded line"),
        }
    }
}
