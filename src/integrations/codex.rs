// SPDX-License-Identifier: Apache-2.0
//! Codex App Server v2 JSONL adapter; protocol assumptions live only here.
use crate::{
    Error, Result,
    bridge::{protocol::*, transport::BridgeClient},
    cancellation::Cancellation,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    ffi::OsStr,
    path::Path,
    process::{ExitStatus, Stdio},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};
const TESTED_VERSION: &str = "codex-cli 0.149.1";
const CORE_SCHEMA_COMPATIBLE_UNTESTED: &str = "core-schema-compatible-untested";
const MAX_CODEX_SCHEMA_BYTES: u64 = 2 * 1024 * 1024;
const MAX_CODEX_VERSION_BYTES: usize = 4096;
const MAX_CODEX_PENDING_BYTES: usize = 16 * 1024 * 1024;
const MAX_CODEX_FRAME_BYTES: usize = 8 * 1024 * 1024;
fn invalid(s: &str) -> Error {
    Error::InvalidInput(s.into())
}

#[derive(Clone, Debug, Serialize)]
pub struct CodexCompatibility {
    pub adapter_version: String,
    pub external_version: String,
    pub tested_version: String,
    pub supported: bool,
    pub schema_verified: bool,
    pub approval_schema_verified: bool,
    pub conformance_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

pub struct CodexAppServerClient {
    child: Child,
    group: Option<nix::unistd::Pid>,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    pending: VecDeque<(Value, usize)>,
    pending_bytes: usize,
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
            pending_bytes: 0,
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
            if data.len() + n > MAX_CODEX_FRAME_BYTES {
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
                let event_bytes = serde_json::to_vec(&value)?.len();
                if self.pending.len() >= 256
                    || self.pending_bytes.saturating_add(event_bytes) > MAX_CODEX_PENDING_BYTES
                {
                    return Err(invalid(
                        "App Server pending event count or byte limit exceeded",
                    ));
                }
                self.pending_bytes += event_bytes;
                self.pending.push_back((value, event_bytes));
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
        if let Some((v, bytes)) = self.pending.pop_front() {
            self.pending_bytes = self.pending_bytes.saturating_sub(bytes);
            Ok(v)
        } else {
            self.read().await
        }
    }
    pub async fn close(&mut self) -> Result<()> {
        let leader_reaped = self.child.try_wait()?.is_some();
        let group_result = kill_process_group(self.group, leader_reaped);
        if !leader_reaped {
            let _ = self.child.start_kill();
            self.child.wait().await?;
        }
        self.group = None;
        group_result?;
        Ok(())
    }
}
impl Drop for CodexAppServerClient {
    fn drop(&mut self) {
        let leader_reaped = self.child.try_wait().ok().flatten().is_some();
        let _ = kill_process_group(self.group, leader_reaped);
        if !leader_reaped {
            let _ = self.child.start_kill();
        }
    }
}

fn kill_process_group(group: Option<nix::unistd::Pid>, leader_reaped: bool) -> Result<()> {
    let Some(group) = group else {
        return Ok(());
    };
    match nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGKILL) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
        Err(nix::errno::Errno::EPERM) if leader_reaped => Ok(()),
        Err(error) => Err(std::io::Error::from_raw_os_error(error as i32).into()),
    }
}

async fn stop_failed_command(
    child: &mut Child,
    group: Option<nix::unistd::Pid>,
    leader_reaped: bool,
) {
    if let Err(error) = kill_process_group(group, leader_reaped) {
        tracing::warn!(%error, "Could not stop Codex command process group");
    }
    if !leader_reaped {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}

async fn read_bounded<R: AsyncRead + Unpin>(mut reader: R, limit: usize) -> Result<Vec<u8>> {
    let mut output = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            return Ok(output);
        }
        if output.len().saturating_add(read) > limit {
            return Err(invalid("Codex command output exceeds its limit"));
        }
        output.extend_from_slice(&buffer[..read]);
    }
}

async fn bounded_command_output(
    executable: &str,
    args: &[&OsStr],
    timeout: Duration,
    output_limit: usize,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
    let mut child = Command::new(executable)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()?;
    let group = child.id().map(|pid| nix::unistd::Pid::from_raw(pid as i32));
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| invalid("Codex command stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| invalid("Codex command stderr unavailable"))?;
    let result = tokio::time::timeout(timeout, async {
        tokio::try_join!(
            async { child.wait().await.map_err(Error::Io) },
            read_bounded(stdout, output_limit),
            read_bounded(stderr, output_limit)
        )
    })
    .await;
    match result {
        Ok(Ok((status, stdout, stderr))) => Ok((status, stdout, stderr)),
        Ok(Err(error)) => {
            let leader_reaped = child.try_wait()?.is_some();
            stop_failed_command(&mut child, group, leader_reaped).await;
            Err(error)
        }
        Err(_) => {
            let leader_reaped = child.try_wait()?.is_some();
            stop_failed_command(&mut child, group, leader_reaped).await;
            Err(invalid("Codex command timeout"))
        }
    }
}

async fn command_status(mut command: Command, timeout: Duration) -> Result<ExitStatus> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let group = child.id().map(|pid| nix::unistd::Pid::from_raw(pid as i32));
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(result) => Ok(result?),
        Err(_) => {
            let leader_reaped = child.try_wait()?.is_some();
            stop_failed_command(&mut child, group, leader_reaped).await;
            Err(invalid("Codex command timeout"))
        }
    }
}

pub fn version_supported(version: &str) -> bool {
    version.trim() == TESTED_VERSION
}

#[derive(Clone, Copy)]
enum SchemaKind {
    Array,
    Boolean,
    Object,
    String,
}

impl SchemaKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Array => "array",
            Self::Boolean => "boolean",
            Self::Object => "object",
            Self::String => "string",
        }
    }
}

struct FieldRequirement {
    path: &'static [&'static str],
    kind: SchemaKind,
    required: bool,
}

struct SchemaRequirement {
    file: &'static str,
    fields: &'static [FieldRequirement],
}

const SCHEMA_REQUIREMENTS: &[SchemaRequirement] = &[
    SchemaRequirement {
        file: "v1/InitializeParams.json",
        fields: &[
            FieldRequirement {
                path: &["clientInfo"],
                kind: SchemaKind::Object,
                required: false,
            },
            FieldRequirement {
                path: &["clientInfo", "name"],
                kind: SchemaKind::String,
                required: false,
            },
            FieldRequirement {
                path: &["clientInfo", "title"],
                kind: SchemaKind::String,
                required: false,
            },
            FieldRequirement {
                path: &["clientInfo", "version"],
                kind: SchemaKind::String,
                required: false,
            },
            FieldRequirement {
                path: &["capabilities"],
                kind: SchemaKind::Object,
                required: false,
            },
            FieldRequirement {
                path: &["capabilities", "experimentalApi"],
                kind: SchemaKind::Boolean,
                required: false,
            },
        ],
    },
    SchemaRequirement {
        file: "v2/ThreadStartParams.json",
        fields: &[
            FieldRequirement {
                path: &["cwd"],
                kind: SchemaKind::String,
                required: false,
            },
            FieldRequirement {
                path: &["developerInstructions"],
                kind: SchemaKind::String,
                required: false,
            },
            FieldRequirement {
                path: &["model"],
                kind: SchemaKind::String,
                required: false,
            },
        ],
    },
    SchemaRequirement {
        file: "v2/ThreadStartResponse.json",
        fields: &[
            FieldRequirement {
                path: &["thread"],
                kind: SchemaKind::Object,
                required: true,
            },
            FieldRequirement {
                path: &["thread", "id"],
                kind: SchemaKind::String,
                required: true,
            },
        ],
    },
    SchemaRequirement {
        file: "v2/ThreadResumeParams.json",
        fields: &[
            FieldRequirement {
                path: &["threadId"],
                kind: SchemaKind::String,
                required: false,
            },
            FieldRequirement {
                path: &["cwd"],
                kind: SchemaKind::String,
                required: false,
            },
            FieldRequirement {
                path: &["model"],
                kind: SchemaKind::String,
                required: false,
            },
        ],
    },
    SchemaRequirement {
        file: "v2/ThreadResumeResponse.json",
        fields: &[
            FieldRequirement {
                path: &["thread"],
                kind: SchemaKind::Object,
                required: true,
            },
            FieldRequirement {
                path: &["thread", "id"],
                kind: SchemaKind::String,
                required: true,
            },
        ],
    },
    SchemaRequirement {
        file: "v2/TurnStartParams.json",
        fields: &[
            FieldRequirement {
                path: &["threadId"],
                kind: SchemaKind::String,
                required: false,
            },
            FieldRequirement {
                path: &["input"],
                kind: SchemaKind::Array,
                required: false,
            },
            FieldRequirement {
                path: &["input", "[]"],
                kind: SchemaKind::Object,
                required: false,
            },
            FieldRequirement {
                path: &["input", "[]", "type"],
                kind: SchemaKind::String,
                required: false,
            },
            FieldRequirement {
                path: &["input", "[]", "text"],
                kind: SchemaKind::String,
                required: false,
            },
            FieldRequirement {
                path: &["input", "[]", "text_elements"],
                kind: SchemaKind::Array,
                required: false,
            },
        ],
    },
    SchemaRequirement {
        file: "v2/TurnStartResponse.json",
        fields: &[
            FieldRequirement {
                path: &["turn"],
                kind: SchemaKind::Object,
                required: true,
            },
            FieldRequirement {
                path: &["turn", "id"],
                kind: SchemaKind::String,
                required: true,
            },
        ],
    },
    SchemaRequirement {
        file: "v2/ItemStartedNotification.json",
        fields: &[
            FieldRequirement {
                path: &["threadId"],
                kind: SchemaKind::String,
                required: true,
            },
            FieldRequirement {
                path: &["turnId"],
                kind: SchemaKind::String,
                required: true,
            },
            FieldRequirement {
                path: &["item"],
                kind: SchemaKind::Object,
                required: true,
            },
            FieldRequirement {
                path: &["item", "id"],
                kind: SchemaKind::String,
                required: true,
            },
            FieldRequirement {
                path: &["item", "type"],
                kind: SchemaKind::String,
                required: true,
            },
        ],
    },
    SchemaRequirement {
        file: "v2/ItemCompletedNotification.json",
        fields: &[
            FieldRequirement {
                path: &["threadId"],
                kind: SchemaKind::String,
                required: true,
            },
            FieldRequirement {
                path: &["turnId"],
                kind: SchemaKind::String,
                required: true,
            },
            FieldRequirement {
                path: &["item"],
                kind: SchemaKind::Object,
                required: true,
            },
            FieldRequirement {
                path: &["item", "id"],
                kind: SchemaKind::String,
                required: true,
            },
            FieldRequirement {
                path: &["item", "type"],
                kind: SchemaKind::String,
                required: true,
            },
        ],
    },
    SchemaRequirement {
        file: "v2/TurnCompletedNotification.json",
        fields: &[
            FieldRequirement {
                path: &["threadId"],
                kind: SchemaKind::String,
                required: true,
            },
            FieldRequirement {
                path: &["turn"],
                kind: SchemaKind::Object,
                required: true,
            },
            FieldRequirement {
                path: &["turn", "id"],
                kind: SchemaKind::String,
                required: true,
            },
            FieldRequirement {
                path: &["turn", "status"],
                kind: SchemaKind::String,
                required: true,
            },
        ],
    },
];

fn resolve_ref<'a>(root: &'a Value, value: &'a Value) -> Option<&'a Value> {
    let reference = value.get("$ref")?.as_str()?;
    reference
        .strip_prefix('#')
        .and_then(|pointer| root.pointer(pointer))
}

fn find_property<'a>(
    root: &'a Value,
    value: &'a Value,
    name: &str,
    depth: usize,
) -> Option<&'a Value> {
    if depth > 32 {
        return None;
    }
    if let Some(resolved) = resolve_ref(root, value) {
        return find_property(root, resolved, name, depth + 1);
    }
    if let Some(property) = value.get("properties").and_then(|v| v.get(name)) {
        return Some(property);
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(property) = value
            .get(keyword)
            .and_then(Value::as_array)
            .and_then(|variants| {
                variants
                    .iter()
                    .find_map(|variant| find_property(root, variant, name, depth + 1))
            })
        {
            return Some(property);
        }
    }
    None
}

fn field_required(root: &Value, value: &Value, name: &str, depth: usize) -> bool {
    if depth > 32 {
        return false;
    }
    if let Some(resolved) = resolve_ref(root, value) {
        return field_required(root, resolved, name, depth + 1);
    }
    if value
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|required| required.iter().any(|field| field == name))
    {
        return true;
    }
    if value
        .get("allOf")
        .and_then(Value::as_array)
        .is_some_and(|variants| {
            variants
                .iter()
                .any(|variant| field_required(root, variant, name, depth + 1))
        })
    {
        return true;
    }
    ["anyOf", "oneOf"].iter().any(|keyword| {
        value
            .get(keyword)
            .and_then(Value::as_array)
            .is_some_and(|variants| {
                !variants.is_empty()
                    && variants
                        .iter()
                        .all(|variant| field_required(root, variant, name, depth + 1))
            })
    })
}

fn field_at_path<'a>(root: &'a Value, path: &[&str]) -> Option<(&'a Value, &'a Value)> {
    let mut owner = root;
    let mut field = None;
    for name in path {
        let value = if *name == "[]" {
            let mut resolved = owner;
            for _ in 0..=32 {
                let Some(next) = resolve_ref(root, resolved) else {
                    break;
                };
                resolved = next;
            }
            resolved.get("items")?
        } else {
            find_property(root, owner, name, 0)?
        };
        field = Some((owner, value));
        owner = value;
    }
    field
}

fn accepts_kind(root: &Value, value: &Value, kind: SchemaKind, depth: usize) -> bool {
    if depth > 32 {
        return false;
    }
    if let Some(resolved) = resolve_ref(root, value) {
        return accepts_kind(root, resolved, kind, depth + 1);
    }
    if value
        .get("type")
        .is_some_and(|schema_type| match schema_type {
            Value::String(value) => value == kind.name(),
            Value::Array(values) => values.iter().any(|value| value == kind.name()),
            _ => false,
        })
    {
        return true;
    }
    if value
        .get("const")
        .is_some_and(|constant| value_has_kind(constant, kind))
        || value
            .get("enum")
            .and_then(Value::as_array)
            .is_some_and(|values| values.iter().any(|value| value_has_kind(value, kind)))
    {
        return true;
    }
    if matches!(kind, SchemaKind::Object) && value.get("properties").is_some() {
        return true;
    }
    ["allOf", "anyOf", "oneOf"].iter().any(|keyword| {
        value
            .get(keyword)
            .and_then(Value::as_array)
            .is_some_and(|variants| {
                variants
                    .iter()
                    .any(|variant| accepts_kind(root, variant, kind, depth + 1))
            })
    })
}

fn guarantees_kind(root: &Value, value: &Value, kind: SchemaKind, depth: usize) -> bool {
    if depth > 32 {
        return false;
    }
    if let Some(resolved) = resolve_ref(root, value) {
        return guarantees_kind(root, resolved, kind, depth + 1);
    }
    if let Some(schema_type) = value.get("type") {
        return match schema_type {
            Value::String(value) => value == kind.name(),
            Value::Array(values) => {
                !values.is_empty() && values.iter().all(|value| value == kind.name())
            }
            _ => false,
        };
    }
    if let Some(constant) = value.get("const") {
        return value_has_kind(constant, kind);
    }
    if let Some(values) = value.get("enum").and_then(Value::as_array) {
        return !values.is_empty() && values.iter().all(|value| value_has_kind(value, kind));
    }
    if value
        .get("allOf")
        .and_then(Value::as_array)
        .is_some_and(|variants| {
            variants
                .iter()
                .any(|variant| guarantees_kind(root, variant, kind, depth + 1))
        })
    {
        return true;
    }
    ["anyOf", "oneOf"].iter().any(|keyword| {
        value
            .get(keyword)
            .and_then(Value::as_array)
            .is_some_and(|variants| {
                !variants.is_empty()
                    && variants
                        .iter()
                        .all(|variant| guarantees_kind(root, variant, kind, depth + 1))
            })
    })
}

fn value_has_kind(value: &Value, kind: SchemaKind) -> bool {
    matches!(
        (value, kind),
        (Value::Array(_), SchemaKind::Array)
            | (Value::Bool(_), SchemaKind::Boolean)
            | (Value::Object(_), SchemaKind::Object)
            | (Value::String(_), SchemaKind::String)
    )
}

fn verify_schema_bundle(root: &Path) -> Result<()> {
    for requirement in SCHEMA_REQUIREMENTS {
        let path = root.join(requirement.file);
        let metadata = std::fs::symlink_metadata(&path).map_err(|_| {
            invalid(&format!(
                "Codex App Server schema lacks required method contract {}",
                requirement.file
            ))
        })?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_CODEX_SCHEMA_BYTES
        {
            return Err(invalid(&format!(
                "Codex App Server schema {} is unsafe or exceeds the size limit",
                requirement.file
            )));
        }
        let data = std::fs::read(&path)?;
        let schema: Value = serde_json::from_slice(&data)?;
        for requirement in requirement.fields {
            let Some((owner, field)) = field_at_path(&schema, requirement.path) else {
                return Err(invalid(&format!(
                    "Codex App Server schema {} lacks required field {}",
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("schema"),
                    requirement.path.join(".")
                )));
            };
            let compatible_kind = if requirement.required {
                guarantees_kind(&schema, field, requirement.kind, 0)
            } else {
                accepts_kind(&schema, field, requirement.kind, 0)
            };
            if !compatible_kind {
                return Err(invalid(&format!(
                    "Codex App Server schema {} has unsafe type for {}; expected {}",
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("schema"),
                    requirement.path.join("."),
                    requirement.kind.name()
                )));
            }
            let name = requirement
                .path
                .last()
                .copied()
                .ok_or_else(|| invalid("empty Codex schema field requirement"))?;
            if requirement.required && !field_required(&schema, owner, name, 0) {
                return Err(invalid(&format!(
                    "Codex App Server schema {} no longer requires {}; compatibility cannot be established",
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("schema"),
                    requirement.path.join(".")
                )));
            }
        }
    }
    Ok(())
}

pub async fn check(executable: &str, allow_untested: bool) -> Result<CodexCompatibility> {
    let (version_status, version_stdout, _) = bounded_command_output(
        executable,
        &[OsStr::new("--version")],
        Duration::from_secs(5),
        MAX_CODEX_VERSION_BYTES,
    )
    .await
    .map_err(|error| invalid(&format!("Codex version check failed: {error}")))?;
    if !version_status.success() {
        return Err(invalid("Codex --version failed"));
    }
    let version = String::from_utf8(version_stdout)
        .map_err(|_| invalid("Codex --version returned invalid UTF-8"))?
        .trim()
        .to_string();
    if version.is_empty() || version.chars().any(char::is_control) {
        return Err(invalid("Codex --version returned an invalid version"));
    }
    let tested = version_supported(&version);
    let schema = tempfile::tempdir()?;
    let mut schema_command = Command::new(executable);
    schema_command
        .args(["app-server", "generate-json-schema", "--out"])
        .arg(schema.path());
    let status = command_status(schema_command, Duration::from_secs(20))
        .await
        .map_err(|error| invalid(&format!("Codex schema detection failed: {error}")))?;
    if !status.success() {
        return Err(invalid("Codex App Server cannot generate its schema"));
    }
    verify_schema_bundle(schema.path())?;
    let mut client = CodexAppServerClient::launch(executable).await?;
    let initialized = client.initialize().await;
    let closed = client.close().await;
    initialized?;
    closed?;
    let warning = (!tested).then(|| {
        let acknowledgement = if allow_untested {
            " The explicit --allow-untested acknowledgement was supplied."
        } else {
            ""
        };
        format!(
            "Codex {version} is not the fixture-tested {TESTED_VERSION}; required core App Server schemas and initialization conform. Approval schema compatibility is not claimed, and unsupported inbound requests fail closed.{acknowledgement}"
        )
    });
    if let Some(warning) = &warning {
        eprintln!("Hardknock Codex compatibility warning: {warning}");
    }
    Ok(CodexCompatibility {
        adapter_version: env!("CARGO_PKG_VERSION").into(),
        external_version: version,
        tested_version: TESTED_VERSION.into(),
        supported: true,
        schema_verified: true,
        approval_schema_verified: tested,
        conformance_status: if tested {
            "tested".into()
        } else {
            CORE_SCHEMA_COMPATIBLE_UNTESTED.into()
        },
        warning,
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
