// SPDX-License-Identifier: Apache-2.0
use super::{Bridge, protocol::*};
use crate::{Error, Result, cancellation::Cancellation, store::Store};
use fs2::FileExt;
use nix::unistd::geteuid;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr},
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc as std_mpsc,
    },
    thread::JoinHandle as ThreadJoinHandle,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream, UnixListener, UnixStream},
    sync::Semaphore,
    task::JoinHandle as TokioJoinHandle,
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
const BRIDGE_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
const TASK_DRAIN_SLICE: Duration = Duration::from_millis(250);

fn invalid(s: &str) -> Error {
    Error::InvalidInput(s.into())
}

#[derive(Default)]
struct BlockingTracker {
    active: AtomicUsize,
}

struct BlockingGuard(Arc<BlockingTracker>);

impl Drop for BlockingGuard {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

async fn owned_blocking<T, F>(
    tracker: Arc<BlockingTracker>,
    cancel: &Cancellation,
    work: F,
) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (sender, receiver) = tokio::sync::oneshot::channel();
    tracker.active.fetch_add(1, Ordering::AcqRel);
    let guard = BlockingGuard(tracker);
    if let Err(error) = std::thread::Builder::new()
        .name("hardknock-bridge-handler".into())
        .spawn(move || {
            let _guard = guard;
            let _ = sender.send(work());
        })
    {
        return Err(error.into());
    }
    tokio::select! {
        _=cancel.cancelled()=>Err(invalid("Bridge stopping with a request handler still running")),
        result=receiver=>result.map_err(|_|invalid("Bridge handler thread failed")),
    }
}

struct BackgroundThread<T> {
    receiver: std_mpsc::Receiver<T>,
    handle: Option<ThreadJoinHandle<()>>,
}

impl<T: Send + 'static> BackgroundThread<T> {
    fn spawn(name: &str, work: impl FnOnce() -> T + Send + 'static) -> Result<Self> {
        let (sender, receiver) = std_mpsc::sync_channel(1);
        let handle = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let value = work();
                let _ = sender.try_send(value);
            })?;
        Ok(Self {
            receiver,
            handle: Some(handle),
        })
    }

    fn poll(&mut self) -> Option<Result<T>> {
        match self.receiver.try_recv() {
            Ok(value) => {
                if self
                    .handle
                    .as_ref()
                    .is_some_and(|handle| handle.is_finished())
                    && let Some(handle) = self.handle.take()
                {
                    let _ = handle.join();
                }
                Some(Ok(value))
            }
            Err(std_mpsc::TryRecvError::Empty) => None,
            Err(std_mpsc::TryRecvError::Disconnected) => {
                if let Some(handle) = self.handle.take() {
                    let _ = handle.join();
                }
                Some(Err(invalid("Bridge background task failed")))
            }
        }
    }
}
fn private_read(path: &Path) -> Result<String> {
    let before = fs::symlink_metadata(path)?;
    if !before.is_file()
        || before.file_type().is_symlink()
        || before.uid() != geteuid().as_raw()
        || before.nlink() != 1
        || before.permissions().mode() & 0o777 != 0o600
        || before.len() > 8192
    {
        return Err(invalid(
            "Bridge runtime file must be a private regular file (0600)",
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)?;
    let opened = file.metadata()?;
    if opened.dev() != before.dev()
        || opened.ino() != before.ino()
        || opened.uid() != before.uid()
        || opened.nlink() != 1
        || opened.permissions().mode() & 0o777 != 0o600
    {
        return Err(invalid("Bridge runtime file changed while being opened"));
    }
    let mut value = String::new();
    (&mut file).take(8193).read_to_string(&mut value)?;
    if value.len() > 8192 {
        return Err(invalid("Bridge runtime file exceeds 8 KiB"));
    }
    let current = fs::symlink_metadata(path)?;
    if current.file_type().is_symlink()
        || current.dev() != opened.dev()
        || current.ino() != opened.ino()
        || current.len() != opened.len()
        || current.mtime() != opened.mtime()
        || current.mtime_nsec() != opened.mtime_nsec()
    {
        return Err(invalid("Bridge runtime file changed while being read"));
    }
    Ok(value)
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
    temp.persist_noclobber(path)
        .map_err(|e| Error::Io(e.error))?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
pub struct RuntimeFiles {
    home: PathBuf,
    _lock: fs::File,
}
impl RuntimeFiles {
    fn unpublish(&self) -> Result<()> {
        let run = self.home.join("run");
        let mut failures = Vec::new();
        for name in ["hardknock.sock", "bridge-token", "bridge-endpoint.json"] {
            let path = run.join(name);
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => failures.push(format!("{}: {error}", path.display())),
            }
        }
        fs::File::open(&run)?.sync_all()?;
        if failures.is_empty() {
            Ok(())
        } else {
            Err(invalid(&format!(
                "Could not remove every Bridge runtime endpoint: {}",
                failures.join("; ")
            )))
        }
    }
}
impl Drop for RuntimeFiles {
    fn drop(&mut self) {
        let _ = self.unpublish();
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
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(lock_path)?;
    let lock_metadata = lock.metadata()?;
    let lock_path_metadata = fs::symlink_metadata(run.join("bridge.lock"))?;
    if !lock_metadata.is_file()
        || lock_metadata.uid() != geteuid().as_raw()
        || lock_metadata.nlink() != 1
        || lock_metadata.permissions().mode() & 0o777 != 0o600
        || lock_path_metadata.file_type().is_symlink()
        || lock_path_metadata.dev() != lock_metadata.dev()
        || lock_path_metadata.ino() != lock_metadata.ino()
    {
        return Err(invalid(
            "Bridge lock must be an owned private regular file (0600)",
        ));
    }
    lock.try_lock_exclusive()
        .map_err(|_| invalid("Bridge already running (runtime lock held)"))?;
    let removed_runtime = crate::reconciliation::reconcile_stale_bridge_runtime(&home)?;
    let recovery_store = Store::open(&home)?;
    let reality_recovery = crate::reconciliation::reconcile_ephemeral_realities(&recovery_store)?;
    if !reality_recovery.failed_realities.is_empty() {
        return Err(Error::Intervention(format!(
            "Bridge startup could not reconcile {} orphaned Realities: {}",
            reality_recovery.failed_realities.len(),
            reality_recovery
                .failed_realities
                .iter()
                .map(|failure| format!("{} ({})", failure.reality_id, failure.reason))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let work_recovery = crate::reconciliation::reconcile_interrupted_bridge_work(&recovery_store)?;
    if !removed_runtime.is_empty()
        || !reality_recovery.discarded_realities.is_empty()
        || !work_recovery.failed_experiments.is_empty()
        || !work_recovery.partial_curricula.is_empty()
    {
        tracing::warn!(
            removed_runtime_paths = removed_runtime.len(),
            discarded_realities = reality_recovery.discarded_realities.len(),
            interrupted_experiments = work_recovery.failed_experiments.len(),
            interrupted_curricula = work_recovery.partial_curricula.len(),
            "Reconciled resources left by an interrupted Bridge"
        );
    }
    drop(recovery_store);
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
    private_write(
        &run.join("bridge-endpoint.json"),
        &serde_json::to_vec(&endpoint)?,
    )?;
    let (bridge, worker) = Bridge::open(&home)?;
    let semaphore = Arc::new(Semaphore::new(32));
    let mut clients = tokio::task::JoinSet::new();
    let mut reality_relays = HashMap::new();
    let connection_cancel = Cancellation::default();
    let blocking_handlers = Arc::new(BlockingTracker::default());
    let mut refresh_task: Option<BackgroundThread<Result<Vec<crate::core::Reality>>>> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut refresh = tokio::time::interval(Duration::from_secs(2));
    let serve_result = loop {
        if bridge.stopping.load(Ordering::Relaxed) {
            break Ok(());
        }
        tokio::select! {
            _=cancel.cancelled()=>break Ok(()),
            _=tick.tick()=>{
                if let Some(task)=refresh_task.as_mut()
                    && let Some(result)=task.poll() {
                    refresh_task=None;
                    match result.and_then(|result|result) {
                        Ok(realities)=>{
                            if let Err(error)=refresh_reality_relays(realities,bridge.clone(),&mut reality_relays,blocking_handlers.clone()).await {
                                break Err(error);
                            }
                        }
                        Err(error)=>break Err(error),
                    }
                }
            },
            _=refresh.tick()=>{
                if refresh_task.is_none() {
                    let b=bridge.clone();
                    let refresh_home=home.clone();
                    let task=BackgroundThread::spawn("hardknock-bridge-refresh",move||{
                        b.refresh()?;
                        Store::open(&refresh_home)?.realities()
                    });
                    match task {
                        Ok(task)=>refresh_task=Some(task),
                        Err(error)=>break Err(error),
                    }
                }
            },
            accepted=async { match &listener {Listener::Unix(l)=>l.accept().await.map(|(s,_)|Box::new(s)as BoxStream),Listener::Tcp(l)=>l.accept().await.map(|(s,_)|Box::new(s)as BoxStream)} }=>{
                let stream=match accepted {
                    Ok(stream)=>stream,
                    Err(error)=>break Err(error.into()),
                };
                let Ok(permit)=semaphore.clone().try_acquire_owned()else{drop(stream);continue;};
                let b=bridge.clone();let t=token.clone();
                let request_cancel=connection_cancel.clone();
                let handlers=blocking_handlers.clone();
                clients.spawn(async move{let _permit=permit;connection(stream,b,&t,None,&request_cancel,handlers).await});
            }
            Some(_)=clients.join_next()=>{},
        }
    };
    bridge.stopping.store(true, Ordering::Relaxed);
    bridge.learning_cancel.cancel();
    drop(listener);
    let endpoint_cleanup = guard.unpublish();
    connection_cancel.cancel();
    let mut task_cleanup_failures = Vec::new();
    if let Err(error) = endpoint_cleanup {
        task_cleanup_failures.push(error.to_string());
    }
    let shutdown_deadline = tokio::time::Instant::now() + BRIDGE_SHUTDOWN_GRACE;
    for relay in reality_relays.values() {
        relay.cancel.cancel();
    }
    for (_, relay) in reality_relays.drain() {
        let relay_deadline = shutdown_deadline.min(tokio::time::Instant::now() + TASK_DRAIN_SLICE);
        if let Err(error) = stop_reality_relay(relay, relay_deadline).await {
            task_cleanup_failures.push(error.to_string());
        }
    }
    let client_deadline = shutdown_deadline.min(tokio::time::Instant::now() + TASK_DRAIN_SLICE);
    while !clients.is_empty() {
        match tokio::time::timeout_at(client_deadline, clients.join_next()).await {
            Ok(Some(Ok(Ok(())))) => {}
            Ok(Some(Ok(Err(error)))) if error.to_string().contains("Bridge stopping") => {}
            Ok(Some(Ok(Err(error)))) => {
                task_cleanup_failures.push(format!("Bridge client task failed: {error}"));
            }
            Ok(Some(Err(error))) => {
                task_cleanup_failures.push(format!("Bridge client task failed: {error}"));
            }
            Ok(None) => break,
            Err(_) => {
                clients.abort_all();
                task_cleanup_failures.push("Bridge client tasks exceeded shutdown deadline".into());
                break;
            }
        }
    }
    if let Some(mut task) = refresh_task {
        let refresh_deadline =
            shutdown_deadline.min(tokio::time::Instant::now() + TASK_DRAIN_SLICE);
        loop {
            if let Some(result) = task.poll() {
                if let Err(error) = result.and_then(|result| result.map(|_| ())) {
                    task_cleanup_failures.push(error.to_string());
                }
                break;
            }
            if tokio::time::Instant::now() >= refresh_deadline {
                task_cleanup_failures
                    .push("Bridge refresh thread exceeded shutdown deadline".into());
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    let experiment_timeout = shutdown_deadline
        .saturating_duration_since(tokio::time::Instant::now())
        .min(TASK_DRAIN_SLICE);
    let report = bridge.experiments.shutdown_with_timeout(experiment_timeout);
    if !report.completed() {
        task_cleanup_failures.push(format!(
            "Bridge experiment cleanup did not finish: {:?}; admission_closed={}; state_observed={}; pending experiments: {}; pending curricula: {}",
            report.outcome,
            report.admission_closed,
            report.state_observed,
            report.pending_experiments.len(),
            report.pending_curricula.len()
        ));
    }
    let flush_timeout = shutdown_deadline.saturating_duration_since(tokio::time::Instant::now());
    if let Err(error) = bridge.flush_with_timeout(flush_timeout) {
        task_cleanup_failures.push(error.to_string());
    }
    let active_handlers = blocking_handlers.active.load(Ordering::Acquire);
    if active_handlers != 0 {
        task_cleanup_failures.push(format!(
            "{active_handlers} Bridge request handler(s) exceeded shutdown deadline"
        ));
    }
    drop(bridge);
    while !worker.is_finished() && tokio::time::Instant::now() < shutdown_deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    if worker.is_finished() {
        if worker.join().is_err() {
            task_cleanup_failures.push("Bridge writer worker panicked".into());
        }
    } else {
        task_cleanup_failures.push("Bridge writer worker exceeded shutdown deadline".into());
        drop(worker);
    }
    drop(guard);
    let cleanup_result = if task_cleanup_failures.is_empty() {
        Ok(())
    } else {
        Err(invalid(&format!(
            "Bridge task cleanup failed: {}",
            task_cleanup_failures.join("; ")
        )))
    };
    match (serve_result, cleanup_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(()), Err(cleanup)) => Err(cleanup),
        (Err(primary), Err(cleanup)) => Err(Error::Cleanup {
            primary: Box::new(primary),
            cleanup: Box::new(cleanup),
        }),
    }
}

struct RealityRelay {
    path: PathBuf,
    cancel: Cancellation,
    handle: TokioJoinHandle<Result<()>>,
}

async fn stop_reality_relay(mut relay: RealityRelay, deadline: tokio::time::Instant) -> Result<()> {
    relay.cancel.cancel();
    let mut cleanup_error = match tokio::time::timeout_at(deadline, &mut relay.handle).await {
        Ok(Ok(Ok(()))) => None,
        Ok(Ok(Err(error))) => Some(error),
        Ok(Err(_)) => Some(invalid("Reality relay task failed")),
        Err(_) => {
            relay.handle.abort();
            Some(invalid("Reality relay exceeded shutdown deadline"))
        }
    };
    match fs::remove_file(&relay.path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            cleanup_error.get_or_insert_with(|| error.into());
        }
    }
    if let Some(directory) = relay.path.parent() {
        match fs::remove_dir(directory) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(error) => {
                cleanup_error.get_or_insert_with(|| error.into());
            }
        }
    }
    cleanup_error.map_or(Ok(()), Err)
}

async fn refresh_reality_relays(
    realities: Vec<crate::core::Reality>,
    bridge: Arc<Bridge>,
    relays: &mut HashMap<String, RealityRelay>,
    blocking_handlers: Arc<BlockingTracker>,
) -> Result<()> {
    let finished = relays
        .iter()
        .filter(|(_, relay)| relay.handle.is_finished())
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    for id in finished {
        if let Some(relay) = relays.remove(&id)
            && let Err(error) =
                stop_reality_relay(relay, tokio::time::Instant::now() + TASK_DRAIN_SLICE).await
        {
            tracing::warn!(%error, %id, "Restarting a failed Reality Bridge relay");
        }
    }
    let active: HashSet<_> = realities
        .iter()
        .filter(|reality| {
            reality.execution_boundary.provider == "container"
                && reality.status != crate::core::RealityStatus::Discarded
                && !reality.execution_boundary.frozen
        })
        .map(|reality| reality.id.to_string())
        .collect();
    let obsolete: Vec<_> = relays
        .keys()
        .filter(|id| !active.contains(*id))
        .cloned()
        .collect();
    for id in obsolete {
        if let Some(relay) = relays.remove(&id) {
            stop_reality_relay(relay, tokio::time::Instant::now() + TASK_DRAIN_SLICE).await?;
        }
    }
    let missing: Vec<_> = realities
        .into_iter()
        .filter(|reality| {
            active.contains(&reality.id.to_string())
                && !relays.contains_key(&reality.id.to_string())
        })
        .collect();
    for reality in missing {
        let directory =
            crate::reconciliation::ensure_reality_control_directory(&bridge.home, &reality.id)?;
        let path = directory.join("bridge.sock");
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            if !metadata.file_type().is_socket() {
                return Err(invalid("Refusing unsafe Reality Bridge relay path"));
            }
            fs::remove_file(&path)?;
        }
        let listener = UnixListener::bind(&path)?;
        // The host parent remains below a 0700 HARDKNOCK_HOME. Mode 0666 is
        // needed only across the container bind mount; every request still
        // requires a signed, short-lived, Reality-bound token.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666))?;
        let relay_bridge = bridge.clone();
        let bound = reality.id.clone();
        let unexposed_guard = format!("relay-{}", uuid::Uuid::new_v4());
        let relay_cancel = Cancellation::default();
        let task_cancel = relay_cancel.clone();
        let relay_handlers = blocking_handlers.clone();
        let handle = tokio::spawn(async move {
            let semaphore = Arc::new(Semaphore::new(8));
            let mut connections = tokio::task::JoinSet::new();
            let relay_result = loop {
                tokio::select! {
                    _=task_cancel.cancelled()=>break Ok(()),
                    accepted=listener.accept()=>{
                        let (stream, _) = match accepted {
                            Ok(accepted)=>accepted,
                            Err(error)=>break Err(error.into()),
                        };
                        let Ok(permit) = semaphore.clone().try_acquire_owned() else {
                            drop(stream);
                            continue;
                        };
                        let bridge = relay_bridge.clone();
                        let guard = unexposed_guard.clone();
                        let bound = bound.clone();
                        let request_cancel = task_cancel.clone();
                        let handlers = relay_handlers.clone();
                        connections.spawn(async move {
                            let _permit = permit;
                            let stream: BoxStream = Box::new(stream);
                            let _ = connection(
                                stream,
                                bridge,
                                &guard,
                                Some(&bound),
                                &request_cancel,
                                handlers,
                            )
                            .await;
                        });
                    }
                    Some(_)=connections.join_next()=>{},
                }
            };
            task_cancel.cancel();
            connections.abort_all();
            while connections.join_next().await.is_some() {}
            relay_result
        });
        relays.insert(
            reality.id.to_string(),
            RealityRelay {
                path,
                cancel: relay_cancel,
                handle,
            },
        );
    }
    Ok(())
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
async fn connection(
    stream: BoxStream,
    bridge: Arc<Bridge>,
    token: &str,
    bound_reality: Option<&crate::core::RealityId>,
    cancel: &Cancellation,
    blocking_handlers: Arc<BlockingTracker>,
) -> Result<()> {
    let mut stream = BufReader::new(stream);
    let bytes = tokio::select! {
        _=cancel.cancelled()=>return Err(invalid("Bridge stopping")),
        result=tokio::time::timeout(Duration::from_secs(10),frame(&mut stream))=>{
            result.map_err(|_|invalid("Bridge request frame timed out"))??
        }
    };
    let raw: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| invalid("Malformed JSON"))?;
    let request_id = raw["request_id"]
        .as_str()
        .filter(|s| s.len() <= 128)
        .unwrap_or("")
        .to_owned();
    let result = if raw["protocol_version"] != PROTOCOL_VERSION {
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
            Ok(envelope) => {
                let direct_token = token_matches(&envelope.token, token);
                let bound_reality = bound_reality.cloned();
                owned_blocking(blocking_handlers, cancel, move || {
                    let authenticated = direct_token
                        || authenticate_reality_capability(
                            &bridge.home,
                            &envelope.token,
                            &envelope.payload,
                            bound_reality.as_ref(),
                        )
                        .unwrap_or(false);
                    if !authenticated {
                        Err(("unauthorized", "Bridge authentication failed".into()))
                    } else {
                        bridge.handle(envelope.payload).map_err(|error| {
                            ("rejected", super::privacy::redact(&error.to_string(), 512))
                        })
                    }
                })
                .await?
            }
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
    tokio::select! {
        _=cancel.cancelled()=>return Err(invalid("Bridge stopping")),
        result=tokio::time::timeout(Duration::from_secs(10),stream.get_mut().write_all(&bytes))=>{
            result.map_err(|_|invalid("Bridge response write timed out"))??;
        }
    }
    Ok(())
}

fn authenticate_reality_capability(
    home: &Path,
    encoded: &str,
    event: &AgentEvent,
    bound_reality: Option<&crate::core::RealityId>,
) -> Result<bool> {
    use crate::{
        capability::{
            CapabilityTokenAuthority, RealityTokenOperation, SignedRealityCapabilityToken,
        },
        store::{CapabilityStore, Store, token_hash},
    };
    let (reality_id, operation) = match event {
        AgentEvent::RealityEffectProposed { reality_id, .. } => {
            (reality_id, RealityTokenOperation::EffectPrepare)
        }
        AgentEvent::RealityEffectStatus { reality_id, .. } => {
            (reality_id, RealityTokenOperation::EffectStatus)
        }
        AgentEvent::RealityEffectDiscardRequested { reality_id, .. } => {
            (reality_id, RealityTokenOperation::EffectDiscard)
        }
        _ => return Ok(false),
    };
    let signed: SignedRealityCapabilityToken =
        serde_json::from_str(encoded).map_err(|_| invalid("Malformed Reality capability token"))?;
    if signed.claims.reality_id != *reality_id
        || bound_reality.is_some_and(|bound| bound != reality_id)
    {
        return Ok(false);
    }
    let store = Store::open(home)?;
    let reality = store.reality(reality_id)?;
    let manifest = store.effective_capability_manifest(reality_id)?;
    CapabilityTokenAuthority::load_or_create(home)?
        .verify(&signed, &reality, &manifest, operation)?;
    Ok(!store.capability_token_revoked(&token_hash(&signed)?)?)
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    fn stop_bridge(bridge: Arc<Bridge>, worker: std::thread::JoinHandle<()>) {
        bridge.stopping.store(true, Ordering::Relaxed);
        bridge.flush().unwrap();
        drop(bridge);
        worker.join().unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancellation_drains_an_incomplete_owned_connection() {
        let temporary = tempfile::tempdir().unwrap();
        let (bridge, worker) = Bridge::open(temporary.path()).unwrap();
        let (mut client, server) = duplex(1024);
        let cancel = Cancellation::default();
        let task_cancel = cancel.clone();
        let task_bridge = bridge.clone();
        let handlers = Arc::new(BlockingTracker::default());
        let task_handlers = handlers.clone();
        let task = tokio::spawn(async move {
            connection(
                Box::new(server),
                task_bridge,
                "secret",
                None,
                &task_cancel,
                task_handlers,
            )
            .await
        });
        client.write_all(b"{").await.unwrap();
        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("connection task did not drain")
            .unwrap();
        assert!(result.unwrap_err().to_string().contains("stopping"));
        stop_bridge(bridge, worker);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn authenticated_handler_finishes_before_connection_shutdown() {
        let temporary = tempfile::tempdir().unwrap();
        let (bridge, worker) = Bridge::open(temporary.path()).unwrap();
        let (mut client, server) = duplex(4096);
        let cancel = Cancellation::default();
        let task_cancel = cancel.clone();
        let task_bridge = bridge.clone();
        let handlers = Arc::new(BlockingTracker::default());
        let task_handlers = handlers.clone();
        let task = tokio::spawn(async move {
            connection(
                Box::new(server),
                task_bridge,
                "secret",
                None,
                &task_cancel,
                task_handlers,
            )
            .await
        });
        let mut request = serde_json::to_vec(&BridgeEnvelope {
            protocol_version: PROTOCOL_VERSION.into(),
            request_id: "owned-handler".into(),
            token: "secret".into(),
            payload: AgentEvent::Status,
        })
        .unwrap();
        request.push(b'\n');
        client.write_all(&request).await.unwrap();
        let response = frame(&mut BufReader::new(&mut client)).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<BridgeResponse>(&response)
                .unwrap()
                .payload
                .unwrap()["status"],
            "running"
        );
        task.await.unwrap().unwrap();
        cancel.cancel();
        stop_bridge(bridge, worker);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancellation_detaches_a_stalled_owned_handler_with_observable_state() {
        let tracker = Arc::new(BlockingTracker::default());
        let cancel = Cancellation::default();
        let task_cancel = cancel.clone();
        let task_tracker = tracker.clone();
        let (release_sender, release_receiver) = std_mpsc::sync_channel(1);
        let task = tokio::spawn(async move {
            owned_blocking(task_tracker, &task_cancel, move || {
                release_receiver.recv().unwrap();
            })
            .await
        });
        while tracker.active.load(Ordering::Acquire) == 0 {
            tokio::task::yield_now().await;
        }

        cancel.cancel();
        let error = tokio::time::timeout(Duration::from_millis(100), task)
            .await
            .expect("stalled handler was not detached")
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("still running"));
        assert_eq!(tracker.active.load(Ordering::Acquire), 1);

        release_sender.send(()).unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while tracker.active.load(Ordering::Acquire) != 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "detached handler did not release its ownership"
            );
            tokio::task::yield_now().await;
        }
    }
}
