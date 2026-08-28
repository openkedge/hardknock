// SPDX-License-Identifier: Apache-2.0
use super::{Bridge, protocol::*};
use crate::{Error, Result, cancellation::Cancellation, store::Store};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    net::{Ipv4Addr, SocketAddr},
    os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream, UnixListener, UnixStream},
    sync::Semaphore,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case", deny_unknown_fields)]
pub enum Endpoint {
    Unix { path: PathBuf },
    Tcp { address: SocketAddr },
}
trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}
type BoxStream = Box<dyn Stream>;
fn invalid(s: &str) -> Error {
    Error::InvalidInput(s.into())
}
fn private_read(path: &Path) -> Result<String> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file()
        || meta.file_type().is_symlink()
        || meta.permissions().mode() & 0o077 != 0
        || meta.len() > 8192
    {
        return Err(invalid(
            "Bridge runtime file must be a private regular file (0600)",
        ));
    }
    Ok(fs::read_to_string(path)?)
}
fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(invalid("Refusing symlink runtime file"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("Missing runtime parent"))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| Error::Io(e.error))?;
    Ok(())
}
pub struct RuntimeFiles {
    home: PathBuf,
    _lock: fs::File,
}
impl Drop for RuntimeFiles {
    fn drop(&mut self) {
        for name in ["hardknock.sock", "bridge-token", "bridge-endpoint.json"] {
            let _ = fs::remove_file(self.home.join("run").join(name));
        }
    }
}
pub async fn serve(home: &Path, tcp: Option<u16>, cancel: &Cancellation) -> Result<()> {
    let store = Store::open(home)?;
    let home = store.home.clone();
    drop(store);
    let run = home.join("run");
    fs::set_permissions(&run, fs::Permissions::from_mode(0o700))?;
    let lock_path = run.join("bridge.lock");
    if fs::symlink_metadata(&lock_path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(invalid("Refusing symlink Bridge lock"));
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(lock_path)?;
    lock.try_lock_exclusive()
        .map_err(|_| invalid("Bridge already running (runtime lock held)"))?;
    let socket_path = run.join("hardknock.sock");
    if let Ok(meta) = fs::symlink_metadata(&socket_path) {
        if !meta.file_type().is_socket() {
            return Err(invalid("Refusing to replace non-socket runtime path"));
        }
        fs::remove_file(&socket_path)?;
    }
    for name in ["bridge-token", "bridge-endpoint.json"] {
        if fs::symlink_metadata(run.join(name))
            .is_ok_and(|m| !m.is_file() || m.file_type().is_symlink())
        {
            return Err(invalid("Refusing unsafe Bridge runtime file"));
        }
    }
    let guard = RuntimeFiles {
        home: home.clone(),
        _lock: lock,
    };
    enum Listener {
        Unix(UnixListener),
        Tcp(TcpListener),
    }
    let (listener, endpoint) = if let Some(port) = tcp {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
        let address = listener.local_addr()?;
        (Listener::Tcp(listener), Endpoint::Tcp { address })
    } else {
        let listener = UnixListener::bind(&socket_path)?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
        (
            Listener::Unix(listener),
            Endpoint::Unix { path: socket_path },
        )
    };
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    private_write(&run.join("bridge-token"), token.as_bytes())?;
    let (bridge, worker) = Bridge::open(&home)?;
    private_write(
        &run.join("bridge-endpoint.json"),
        &serde_json::to_vec(&endpoint)?,
    )?;
    let semaphore = Arc::new(Semaphore::new(32));
    let mut clients = tokio::task::JoinSet::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut refresh = tokio::time::interval(Duration::from_secs(2));
    loop {
        if bridge.stopping.load(Ordering::Relaxed) {
            break;
        }
        tokio::select! {
            _=cancel.cancelled()=>break,
            _=tick.tick()=>{},
            _=refresh.tick()=>{let b=bridge.clone();tokio::task::spawn_blocking(move||b.refresh());},
            accepted=async { match &listener {Listener::Unix(l)=>l.accept().await.map(|(s,_)|Box::new(s)as BoxStream),Listener::Tcp(l)=>l.accept().await.map(|(s,_)|Box::new(s)as BoxStream)} }=>{
                let stream=accepted?;
                let Ok(permit)=semaphore.clone().try_acquire_owned()else{drop(stream);continue;};
                let b=bridge.clone();let t=token.clone();
                clients.spawn(async move{let _permit=permit;let _=tokio::time::timeout(Duration::from_secs(10),connection(stream,b,&t)).await;});
            }
            Some(_)=clients.join_next()=>{},
        }
    }
    bridge.stopping.store(true, Ordering::Relaxed);
    bridge.learning_cancel.cancel();
    bridge.experiments.cancel_all();
    clients.abort_all();
    while clients.join_next().await.is_some() {}
    let b = bridge.clone();
    let flushed = tokio::task::spawn_blocking(move || b.flush())
        .await
        .map_err(|_| invalid("Writer join failed"))?;
    drop(bridge);
    tokio::task::spawn_blocking(move || worker.join())
        .await
        .map_err(|_| invalid("Worker join failed"))?
        .map_err(|_| invalid("Worker panicked"))?;
    drop(guard);
    flushed
}
async fn frame<R: AsyncRead + Unpin>(reader: &mut BufReader<R>) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            return Err(invalid("Incomplete JSONL frame"));
        }
        let count = buffer
            .iter()
            .position(|b| *b == b'\n')
            .map(|i| i + 1)
            .unwrap_or(buffer.len());
        if line.len() + count > MAX_EVENT_BYTES {
            return Err(invalid("Bridge message exceeds 1 MiB"));
        }
        let complete = buffer[count - 1] == b'\n';
        line.extend_from_slice(&buffer[..count]);
        reader.consume(count);
        if complete {
            return Ok(line);
        }
    }
}
fn token_matches(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |diff, (x, y)| diff | (x ^ y))
            == 0
}
async fn connection(stream: BoxStream, bridge: Arc<Bridge>, token: &str) -> Result<()> {
    let mut stream = BufReader::new(stream);
    let bytes = frame(&mut stream).await?;
    let raw: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| invalid("Malformed JSON"))?;
    let request_id = raw["request_id"]
        .as_str()
        .filter(|s| s.len() <= 128)
        .unwrap_or("")
        .to_owned();
    let result = if !token_matches(raw["token"].as_str().unwrap_or(""), token) {
        Err(("unauthorized", "Bridge authentication failed".into()))
    } else if raw["protocol_version"] != PROTOCOL_VERSION {
        Err((
            "unsupported_protocol",
            "Expected hardknock.bridge.v1".into(),
        ))
    } else if request_id.is_empty() {
        Err((
            "invalid_request",
            "Request id required (maximum 128 bytes)".into(),
        ))
    } else {
        match serde_json::from_value::<BridgeEnvelope<AgentEvent>>(raw) {
            Err(_) => Err(("invalid_event", "Malformed or unknown event fields".into())),
            Ok(envelope) => tokio::task::spawn_blocking(move || bridge.handle(envelope.payload))
                .await
                .map_err(|_| invalid("Bridge handler failed"))?
                .map_err(|e| ("rejected", super::privacy::redact(&e.to_string(), 512))),
        }
    };
    let (payload, error) = match result {
        Ok(value) => (Some(value), None),
        Err((code, message)) => (
            None,
            Some(BridgeError {
                code: code.into(),
                message,
            }),
        ),
    };
    let response = BridgeResponse {
        protocol_version: PROTOCOL_VERSION.into(),
        request_id,
        ok: error.is_none(),
        payload,
        error,
    };
    let mut bytes = serde_json::to_vec(&response)?;
    bytes.push(b'\n');
    stream.get_mut().write_all(&bytes).await?;
    Ok(())
}
#[derive(Clone)]
pub struct BridgeClient {
    pub home: PathBuf,
    pub timeout: Duration,
}
impl BridgeClient {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.into(),
            timeout: Duration::from_millis(200),
        }
    }
    pub async fn request(&self, payload: AgentEvent) -> Result<serde_json::Value> {
        tokio::time::timeout(self.timeout, self.request_inner(payload))
            .await
            .map_err(|_| invalid("Bridge timeout; advisory unavailable"))?
    }
    async fn request_inner(&self, payload: AgentEvent) -> Result<serde_json::Value> {
        let run = self.home.canonicalize()?.join("run");
        let endpoint: Endpoint =
            serde_json::from_str(&private_read(&run.join("bridge-endpoint.json"))?)?;
        let token = private_read(&run.join("bridge-token"))?;
        let stream: BoxStream = match endpoint {
            Endpoint::Unix { path } => {
                if path != run.join("hardknock.sock") {
                    return Err(invalid("Unexpected Bridge socket path"));
                }
                Box::new(UnixStream::connect(path).await?)
            }
            Endpoint::Tcp { address } => {
                if address.ip() != std::net::IpAddr::V4(Ipv4Addr::LOCALHOST) {
                    return Err(invalid("Bridge TCP must bind 127.0.0.1"));
                }
                Box::new(TcpStream::connect(address).await?)
            }
        };
        let request_id = uuid::Uuid::new_v4().to_string();
        let envelope = BridgeEnvelope {
            protocol_version: PROTOCOL_VERSION.into(),
            request_id: request_id.clone(),
            token,
            payload,
        };
        let mut bytes = serde_json::to_vec(&envelope)?;
        bytes.push(b'\n');
        if bytes.len() > MAX_EVENT_BYTES {
            return Err(invalid("Bridge request too large"));
        }
        let mut stream = BufReader::new(stream);
        stream.get_mut().write_all(&bytes).await?;
        let response: BridgeResponse = serde_json::from_slice(&frame(&mut stream).await?)?;
        if response.protocol_version != PROTOCOL_VERSION || response.request_id != request_id {
            return Err(invalid("Bridge response correlation mismatch"));
        }
        if !response.ok {
            return Err(invalid(
                &response
                    .error
                    .map(|e| format!("{}: {}", e.code, e.message))
                    .unwrap_or_else(|| "Bridge request failed".into()),
            ));
        }
        response
            .payload
            .ok_or_else(|| invalid("Bridge response payload missing"))
    }
}
