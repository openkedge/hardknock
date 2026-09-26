// SPDX-License-Identifier: Apache-2.0
use super::{
    cache::{
        ExperienceHotCache, RuntimeEvaluationRequest, bridge_decision_from_runtime,
        context_response,
    },
    config::Config,
    privacy::{redact, redact_value},
    protocol::*,
};
use crate::{
    Error, Result,
    core::{ExperienceId, RuntimeDecisionId, StateRef, TaskId, TrajectoryId},
    experience::ExperienceContext,
    predictive::{ExecutionTrajectory, TrajectoryOutcome, TrajectorySubject},
    retrieval::RetrievedLesson,
    store::{
        EffectStore, EpistemicStore, PreparedRoleKnowledgeView, PreparedRuntimeDecision,
        PreparedTrajectoryMutation, RuntimeStore, Store,
    },
};
use chrono::Utc;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::JoinHandle,
    time::Duration,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordedAction {
    pub action_id: String,
    pub action: NormalizedAction,
    pub decision: ActionDecision,
    pub result: Option<ActionResult>,
    pub duration_ms: u64,
    pub can_intercept: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: String,
    pub experience_id: String,
    pub status: String,
    pub outcome: Option<String>,
    pub error: Option<String>,
    pub action_start: usize,
    pub action_end: usize,
    pub duration_ms: u64,
    pub claimed_success: Option<bool>,
    #[serde(default)]
    pub termination: RunTermination,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub external_id: String,
    pub agent: AgentIdentity,
    pub cwd: PathBuf,
    pub reported_cwd: String,
    pub task: String,
    pub context: ExperienceContext,
    pub starting_state: StateRef,
    pub clean_start: bool,
    pub started_at: chrono::DateTime<Utc>,
    pub ended: bool,
    pub revision: u64,
    pub consecutive_failures: u32,
    pub actions: Vec<RecordedAction>,
    pub delivered: Vec<RetrievedLesson>,
    pub rejections: BTreeMap<String, LessonFeedback>,
    pub runs: BTreeMap<String, RunRecord>,
    pub next_action_start: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trajectory_id: Option<crate::core::TrajectoryId>,
}
pub struct Bridge {
    pub home: PathBuf,
    pub config: Config,
    pub cache: RwLock<ExperienceHotCache>,
    sessions: Mutex<HashMap<String, Session>>,
    admissions: Mutex<HashSet<String>>,
    action_order: Mutex<()>,
    jobs: SyncSender<Job>,
    job_permits: Arc<JobPermitPool>,
    pub stopping: AtomicBool,
    pub persistence_error: Mutex<Option<String>>,
    shutdown_complete: (Mutex<bool>, Condvar),
    pub(crate) learning_cancel: crate::cancellation::Cancellation,
    pub(crate) experiments: super::experiments::ExperimentService,
    #[cfg(test)]
    fail_action_before_persistence: AtomicBool,
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.learning_cancel.cancel();
        self.experiments.request_shutdown();
    }
}
type PersistenceAck = SyncSender<std::result::Result<(), String>>;
type SessionAdmissionAck = SyncSender<std::result::Result<Box<Session>, String>>;

struct JobPermitPool {
    available: Mutex<usize>,
}

impl JobPermitPool {
    fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            available: Mutex::new(capacity),
        })
    }

    fn try_acquire(self: &Arc<Self>) -> Option<JobPermit> {
        let mut available = self.available.lock().expect("job permit lock");
        if *available == 0 {
            return None;
        }
        *available -= 1;
        Some(JobPermit {
            pool: Arc::clone(self),
        })
    }
}

struct JobPermit {
    pool: Arc<JobPermitPool>,
}

impl Drop for JobPermit {
    fn drop(&mut self) {
        let mut available = self.pool.available.lock().expect("job permit lock");
        *available = available.saturating_add(1);
    }
}

struct ActionPersistence {
    session: Box<Session>,
    expected_revision: u64,
    events: Vec<(String, Value)>,
    trajectory: Option<PreparedTrajectoryMutation>,
    runtime_decision: Option<PreparedRuntimeDecision>,
    role_knowledge_view: Option<PreparedRoleKnowledgeView>,
}

struct Job {
    permit: JobPermit,
    kind: JobKind,
}

enum JobKind {
    Persist {
        session: Option<Box<Session>>,
        expected_revision: Option<u64>,
        id: String,
        events: Vec<(String, Value)>,
        acknowledgement: Option<PersistenceAck>,
    },
    AdmitSession {
        session: Box<Session>,
        expected_revision: Option<u64>,
        events: Vec<(String, Value)>,
        acknowledgement: SessionAdmissionAck,
    },
    EndSession {
        ended: Box<Session>,
        expected_revision: u64,
        trajectory: Option<(
            crate::core::TrajectoryId,
            crate::predictive::TrajectoryOutcome,
        )>,
        acknowledgement: SyncSender<SessionEndAcknowledgement>,
    },
    Complete {
        snapshot: Box<Session>,
        expected_revision: u64,
        recording_clean_start: bool,
        run: RunRecord,
        acknowledgement: PersistenceAck,
    },
    ActionPersistence {
        persistence: Box<ActionPersistence>,
        acknowledgement: PersistenceAck,
    },
    Barrier(PersistenceAck),
    Flush(SyncSender<Option<String>>),
    Shutdown(PersistenceAck),
    #[cfg(test)]
    Block {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    },
}

enum SessionEndAcknowledgement {
    Committed,
    RolledBack {
        session: Box<Session>,
        error: String,
    },
    InDoubt {
        error: String,
    },
}

const DEFAULT_JOB_CAPACITY: usize = 4096;
const DEFAULT_FLUSH_TIMEOUT: Duration = Duration::from_secs(60);
const LIFECYCLE_ACK_TIMEOUT: Duration = Duration::from_secs(10);
const ACTION_COMMIT_TIMEOUT: Duration = Duration::from_secs(10);

pub fn session_key(agent: &str, external: &str) -> String {
    format!(
        "hk-s-{}",
        &blake3::hash(format!("{agent}\0{external}").as_bytes()).to_hex()[..32]
    )
}
fn invalid(message: &str) -> Error {
    Error::InvalidInput(message.into())
}
fn persistence_message(error: &Error) -> String {
    redact(&error.to_string(), 512)
}
fn publish_persistence_error(bridge: &std::sync::Weak<Bridge>, message: &str) {
    if let Some(bridge) = bridge.upgrade()
        && let Ok(mut current) = bridge.persistence_error.try_lock()
    {
        *current = Some(message.into());
    }
}
fn remember_persistence_error(
    bridge: &std::sync::Weak<Bridge>,
    last_error: &mut Option<String>,
    error: &Error,
) -> String {
    let message = persistence_message(error);
    publish_persistence_error(bridge, &message);
    *last_error = Some(message.clone());
    message
}
fn valid_id(s: &str) -> Result<()> {
    if s.is_empty() || s.len() > 256 || s.chars().any(char::is_control) {
        return Err(invalid(
            "Identifier must be 1–256 bytes without control characters",
        ));
    }
    Ok(())
}
fn bridge_read_connection(home: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(
        home.join("hardknock.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(connection)
}
fn bridge_write_connection(home: &Path) -> Result<Connection> {
    let connection = Connection::open(home.join("hardknock.db"))?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(connection)
}
fn trajectory_outcome_name(outcome: &TrajectoryOutcome) -> &'static str {
    match outcome {
        TrajectoryOutcome::Success => "success",
        TrajectoryOutcome::Degraded => "degraded",
        TrajectoryOutcome::Failure(_) => "failure",
        TrajectoryOutcome::Aborted => "aborted",
        TrajectoryOutcome::Abstained => "abstained",
        TrajectoryOutcome::Unknown => "unknown",
    }
}
fn session_trajectory(session: &Session) -> ExecutionTrajectory {
    ExecutionTrajectory {
        id: TrajectoryId::new(),
        session_id: crate::core::HardknockSessionId::from_external(&session.external_id),
        subject: TrajectorySubject::Task(TaskId::new()),
        task_family: None,
        started_at: Utc::now(),
        ended_at: None,
        completed_at: None,
        events: Vec::new(),
        points: Vec::new(),
        outcome: None,
        context: crate::predictive::TrajectoryContext {
            scope: crate::lesson::ContextSelector::from_context(&session.context),
            runtime_version: Some(env!("CARGO_PKG_VERSION").into()),
            tool_versions: BTreeMap::new(),
            observability: vec![
                "retry_count".into(),
                "no_state_change".into(),
                "config_changed".into(),
                "action_kind".into(),
                "success".into(),
                "duration_ms".into(),
                "tool_failure_count".into(),
            ],
        },
        fingerprint: Default::default(),
    }
}
fn save_bridge_events_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    session_id: &str,
    events: &[(String, Value)],
) -> Result<()> {
    for (kind, data) in events {
        transaction.execute(
            "INSERT INTO bridge_events(session_id,kind,data) VALUES(?1,?2,?3)",
            params![session_id, kind, serde_json::to_string(data)?],
        )?;
    }
    Ok(())
}

enum SessionCas {
    Updated,
    AlreadyDurable,
}

fn sql_revision(revision: u64) -> Result<i64> {
    i64::try_from(revision).map_err(|_| invalid("Bridge session revision overflow"))
}

fn checked_session_update(
    transaction: &rusqlite::Transaction<'_>,
    session: &Session,
    expected_revision: u64,
) -> Result<SessionCas> {
    if session.revision != expected_revision.saturating_add(1) {
        return Err(invalid(
            "Bridge session candidate does not advance exactly one revision",
        ));
    }
    let serialized = serde_json::to_string(session)?;
    let changed = transaction.execute(
        "UPDATE bridge_sessions SET revision=?2,data=?3
         WHERE id=?1 AND revision=?4",
        params![
            session.id,
            sql_revision(session.revision)?,
            serialized,
            sql_revision(expected_revision)?
        ],
    )?;
    if changed == 1 {
        return Ok(SessionCas::Updated);
    }
    let durable = transaction
        .query_row(
            "SELECT revision,data FROM bridge_sessions WHERE id=?1",
            [&session.id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((revision, data)) = durable
        && revision == sql_revision(session.revision)?
        && serde_json::from_str::<Value>(&data)? == serde_json::to_value(session)?
    {
        return Ok(SessionCas::AlreadyDurable);
    }
    Err(Error::Intervention(
        "Bridge session revision changed before durable commit".into(),
    ))
}

fn checked_session_insert(
    transaction: &rusqlite::Transaction<'_>,
    session: &Session,
) -> Result<SessionCas> {
    if session.revision != 1 {
        return Err(invalid("New Bridge sessions must begin at revision one"));
    }
    let serialized = serde_json::to_string(session)?;
    let changed = transaction.execute(
        "INSERT INTO bridge_sessions(id,revision,data) VALUES(?1,?2,?3)
         ON CONFLICT(id) DO NOTHING",
        params![session.id, sql_revision(session.revision)?, serialized],
    )?;
    if changed == 1 {
        return Ok(SessionCas::Updated);
    }
    let durable = transaction
        .query_row(
            "SELECT revision,data FROM bridge_sessions WHERE id=?1",
            [&session.id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((revision, data)) = durable
        && revision == sql_revision(session.revision)?
        && serde_json::from_str::<Value>(&data)? == serde_json::to_value(session)?
    {
        return Ok(SessionCas::AlreadyDurable);
    }
    Err(Error::Intervention(
        "Bridge session already exists with different durable state".into(),
    ))
}

fn persist_action_atomic(store: &Store, persistence: &ActionPersistence) -> Result<()> {
    let transaction = store.immediate_transaction()?;
    if matches!(
        checked_session_update(
            &transaction,
            &persistence.session,
            persistence.expected_revision
        )?,
        SessionCas::AlreadyDurable
    ) {
        return Ok(());
    }
    if let Some(trajectory) = &persistence.trajectory {
        store.persist_prepared_trajectory_mutation(&transaction, trajectory)?;
    }
    if let Some(runtime_decision) = &persistence.runtime_decision {
        store.persist_prepared_runtime_decision(&transaction, runtime_decision)?;
    }
    if let Some(role_knowledge_view) = &persistence.role_knowledge_view {
        store.persist_prepared_role_knowledge_view(&transaction, role_knowledge_view)?;
    }
    save_bridge_events_in_transaction(&transaction, &persistence.session.id, &persistence.events)?;
    transaction.commit()?;
    Ok(())
}

fn persist_bridge_job_atomic(
    store: &Store,
    session: Option<&Session>,
    expected_revision: Option<u64>,
    id: &str,
    events: &[(String, Value)],
) -> Result<()> {
    let transaction = store.immediate_transaction()?;
    if let Some(session) = session {
        let expected_revision = expected_revision
            .ok_or_else(|| invalid("Bridge session persistence requires an expected revision"))?;
        if matches!(
            checked_session_update(&transaction, session, expected_revision)?,
            SessionCas::AlreadyDurable
        ) {
            return Ok(());
        }
        if events.iter().any(|(kind, _)| kind == "lesson_rejected") {
            for feedback in session.rejections.values() {
                let reason = serde_json::to_value(&feedback.reason)?;
                store.bridge_feedback(
                    id,
                    &session.agent.name,
                    &feedback.lesson_id.parse()?,
                    reason.as_str().unwrap_or("other"),
                )?;
            }
        }
    }
    save_bridge_events_in_transaction(&transaction, id, events)?;
    transaction.commit()?;
    Ok(())
}

fn persist_run_queued_atomic(
    store: &Store,
    session: &Session,
    expected_revision: u64,
    run: &RunRecord,
) -> Result<()> {
    let transaction = store.immediate_transaction()?;
    if matches!(
        checked_session_update(&transaction, session, expected_revision)?,
        SessionCas::AlreadyDurable
    ) {
        return Ok(());
    }
    if run.status != "queued" {
        return Err(invalid(
            "Bridge recording must enter the queued state first",
        ));
    }
    let changed = transaction.execute(
        "INSERT INTO bridge_runs(session_id,run_id,experience_id,data)
         VALUES(?1,?2,NULL,?3)
         ON CONFLICT(session_id,run_id) DO NOTHING",
        params![session.id, run.run_id, serde_json::to_string(run)?],
    )?;
    if changed != 1 {
        return Err(Error::Intervention(
            "Bridge run changed before queued-state persistence".into(),
        ));
    }
    save_bridge_events_in_transaction(
        &transaction,
        &session.id,
        &[("run_queued".into(), json!({"run_id":run.run_id}))],
    )?;
    transaction.commit()?;
    Ok(())
}

fn persist_run_final_atomic(
    store: &Store,
    session_id: &str,
    queued_run: &RunRecord,
    final_run: &RunRecord,
) -> Result<()> {
    let transaction = store.immediate_transaction()?;
    let current: String = transaction.query_row(
        "SELECT data FROM bridge_runs WHERE session_id=?1 AND run_id=?2",
        params![session_id, final_run.run_id],
        |row| row.get(0),
    )?;
    let current_value = serde_json::from_str::<Value>(&current)?;
    let final_value = serde_json::to_value(final_run)?;
    if current_value != final_value {
        if current_value != serde_json::to_value(queued_run)? {
            return Err(Error::Intervention(
                "Bridge run changed before final-state persistence".into(),
            ));
        }
        let changed = transaction.execute(
            "UPDATE bridge_runs SET experience_id=?3,data=?4
             WHERE session_id=?1 AND run_id=?2 AND data=?5",
            params![
                session_id,
                final_run.run_id,
                (final_run.status == "completed").then_some(&final_run.experience_id),
                serde_json::to_string(final_run)?,
                current
            ],
        )?;
        if changed != 1 {
            return Err(Error::Intervention(
                "Bridge run changed before final-state persistence".into(),
            ));
        }
    }
    save_bridge_events_in_transaction(
        &transaction,
        session_id,
        &[(
            if final_run.status == "completed" {
                "experience_created".into()
            } else {
                "recording_failed".into()
            },
            json!({
                "run_id":final_run.run_id,
                "experience_id":final_run.experience_id,
                "outcome":final_run.outcome,
                "error":final_run.error
            }),
        )],
    )?;
    transaction.commit()?;
    Ok(())
}
fn persist_session_admission(
    home: &Path,
    mut session: Session,
    expected_revision: Option<u64>,
    events: &[(String, Value)],
) -> Result<Session> {
    let mut connection = bridge_write_connection(home)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let active_trajectory = session
        .trajectory_id
        .as_ref()
        .map(|id| {
            transaction
                .query_row(
                    "SELECT ended_at IS NULL FROM execution_trajectories WHERE id=?1",
                    [id.to_string()],
                    |row| row.get::<_, bool>(0),
                )
                .optional()
        })
        .transpose()?
        .flatten()
        .unwrap_or(false);
    let trajectory = if !active_trajectory {
        let trajectory = session_trajectory(&session);
        session.trajectory_id = Some(trajectory.id.clone());
        Some(trajectory)
    } else {
        None
    };
    let session_cas = match expected_revision {
        Some(expected_revision) => {
            checked_session_update(&transaction, &session, expected_revision)?
        }
        None => checked_session_insert(&transaction, &session)?,
    };
    if matches!(session_cas, SessionCas::AlreadyDurable) {
        return Ok(session);
    }
    if let Some(trajectory) = trajectory {
        let TrajectorySubject::Task(subject_id) = &trajectory.subject else {
            unreachable!("Bridge trajectories always use task subjects");
        };
        transaction.execute(
            "INSERT INTO execution_trajectories(
                id,session_id,task_family_id,started_at,subject_kind,subject_id,data
             ) VALUES(?1,?2,NULL,?3,'task',?4,?5)",
            params![
                trajectory.id.to_string(),
                trajectory.session_id.to_string(),
                trajectory.started_at.to_rfc3339(),
                subject_id.to_string(),
                serde_json::to_string(&trajectory)?
            ],
        )?;
        transaction.execute(
            "INSERT INTO predictive_events(subject,kind,data)
             VALUES(?1,'trajectory_started','{}')",
            [trajectory.id.to_string()],
        )?;
        transaction.execute(
            "INSERT INTO bridge_events(session_id,kind,data)
             VALUES('predictive-local','trajectory_started',?1)",
            [serde_json::to_string(&json!({"subject":trajectory.id}))?],
        )?;
    }
    save_bridge_events_in_transaction(&transaction, &session.id, events)?;
    transaction.commit()?;
    Ok(session)
}
fn persist_session_end_atomic(
    home: &Path,
    ended: &Session,
    expected_revision: u64,
    resolution: Option<&(TrajectoryId, TrajectoryOutcome)>,
) -> Result<()> {
    let mut connection = bridge_write_connection(home)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if matches!(
        checked_session_update(&transaction, ended, expected_revision)?,
        SessionCas::AlreadyDurable
    ) {
        return Ok(());
    }
    let trajectory = match (&ended.trajectory_id, resolution) {
        (Some(id), Some((expected_id, outcome))) if id == expected_id => {
            let serialized = transaction
                .query_row(
                    "SELECT data FROM execution_trajectories
                     WHERE id=?1 AND ended_at IS NULL",
                    [id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| invalid("Session trajectory is missing or already final"))?;
            let mut trajectory: ExecutionTrajectory = serde_json::from_str(&serialized)?;
            let mut statement = transaction.prepare(
                "SELECT data FROM trajectory_events
                 WHERE trajectory_id=?1 ORDER BY sequence",
            )?;
            let serialized_events = statement
                .query_map([id.to_string()], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            drop(statement);
            let trajectory_events = serialized_events
                .into_iter()
                .map(|event| Ok(serde_json::from_str(&event)?))
                .collect::<Result<Vec<_>>>()?;
            trajectory.fingerprint = crate::predictive::fingerprint(&trajectory_events)?;
            trajectory.outcome = Some(outcome.clone());
            trajectory.ended_at = Some(Utc::now());
            trajectory.completed_at = trajectory.ended_at;
            Some(trajectory)
        }
        (None, None) => None,
        _ => {
            return Err(invalid(
                "Session trajectory and termination outcome are inconsistent",
            ));
        }
    };
    if let Some(trajectory) = &trajectory {
        let changed = transaction.execute(
            "UPDATE execution_trajectories
             SET outcome_kind=?2,ended_at=?3,fingerprint=?4,data=?5
             WHERE id=?1 AND ended_at IS NULL",
            params![
                trajectory.id.to_string(),
                trajectory_outcome_name(
                    trajectory.outcome.as_ref().expect("trajectory outcome set")
                ),
                trajectory
                    .ended_at
                    .expect("trajectory end set")
                    .to_rfc3339(),
                trajectory.fingerprint.hash,
                serde_json::to_string(trajectory)?
            ],
        )?;
        if changed != 1 {
            return Err(invalid(
                "Session trajectory changed before atomic termination",
            ));
        }
        transaction.execute(
            "INSERT INTO predictive_events(subject,kind,data)
             VALUES(?1,'trajectory_resolved',?2)",
            params![
                trajectory.id.to_string(),
                serde_json::to_string(&json!({
                    "outcome": trajectory.outcome
                }))?
            ],
        )?;
        transaction.execute(
            "INSERT INTO bridge_events(session_id,kind,data)
             VALUES('predictive-local','trajectory_resolved',?1)",
            [serde_json::to_string(&json!({"subject":trajectory.id}))?],
        )?;
    }
    save_bridge_events_in_transaction(
        &transaction,
        &ended.id,
        &[("session_ended".into(), json!({}))],
    )?;
    transaction.commit()?;
    Ok(())
}
fn active_bridge_sessions(store: &Store, max_sessions: usize) -> Result<Vec<Session>> {
    let connection = bridge_read_connection(&store.home)?;
    let limit = i64::try_from(max_sessions.saturating_add(1))
        .map_err(|_| invalid("Bridge session budget is too large"))?;
    let mut statement = connection.prepare(
        "SELECT data FROM bridge_sessions
         WHERE CAST(json_extract(data, '$.ended') AS INTEGER) = 0
         ORDER BY id LIMIT ?1",
    )?;
    let serialized = statement
        .query_map([limit], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if serialized.len() > max_sessions {
        return Err(invalid(
            "Persisted active Bridge sessions exceed the configured session budget",
        ));
    }
    serialized
        .into_iter()
        .map(|data| Ok(serde_json::from_str(&data)?))
        .collect()
}
pub(super) fn persisted_bridge_session(home: &Path, id: &str) -> Result<Option<Session>> {
    let connection = bridge_read_connection(home)?;
    let durable = connection
        .query_row(
            "SELECT revision,data FROM bridge_sessions WHERE id = ?1",
            [id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    durable
        .map(|(revision, data)| {
            let session: Session = serde_json::from_str(&data)?;
            if revision != sql_revision(session.revision)? {
                return Err(Error::Intervention(
                    "Bridge session revision column differs from its durable payload".into(),
                ));
            }
            Ok(session)
        })
        .transpose()
}

fn persisted_session_with_runs(home: &Path, id: &str) -> Result<Option<Session>> {
    let Some(mut session) = persisted_bridge_session(home, id)? else {
        return Ok(None);
    };
    for run in Store::open(home)?.bridge_runs(id)? {
        session.runs.insert(run.run_id.clone(), run);
    }
    Ok(Some(session))
}

fn reconcile_weak_session(bridge: &std::sync::Weak<Bridge>, home: &Path, id: &str) {
    let Some(bridge) = bridge.upgrade() else {
        return;
    };
    let Ok(durable) = persisted_session_with_runs(home, id) else {
        return;
    };
    let mut sessions = bridge.sessions.lock().expect("session lock");
    match durable {
        Some(session) if !session.ended => {
            sessions.insert(id.to_owned(), session);
        }
        _ => {
            sessions.remove(id);
        }
    }
}
impl Bridge {
    pub fn open(home: &Path) -> Result<(Arc<Self>, JoinHandle<()>)> {
        Self::open_with_job_capacity(home, DEFAULT_JOB_CAPACITY)
    }

    fn open_with_job_capacity(
        home: &Path,
        job_capacity: usize,
    ) -> Result<(Arc<Self>, JoinHandle<()>)> {
        let store = Store::open(home)?;
        let config = Config::load(home)?;
        let cache = ExperienceHotCache::load(&store)?;
        let mut sessions = HashMap::new();
        for mut session in active_bridge_sessions(&store, config.bridge.max_sessions)? {
            let expected_revision = session.revision;
            let persisted_runs = serde_json::to_value(&session.runs)?;
            for run in store.bridge_runs(&session.id)? {
                session.runs.insert(run.run_id.clone(), run);
            }
            // An unacknowledged in-flight run is never silently presented as success after a crash.
            let mut interrupted = Vec::<(RunRecord, RunRecord)>::new();
            for run in session.runs.values_mut().filter(|r| r.status == "queued") {
                let queued = run.clone();
                run.status = "interrupted".into();
                run.error =
                    Some("Bridge restarted before completion; inspect retained evidence".into());
                interrupted.push((queued, run.clone()));
            }
            if !interrupted.is_empty() || persisted_runs != serde_json::to_value(&session.runs)? {
                session.revision = session.revision.saturating_add(1);
                let transaction = store.immediate_transaction()?;
                if !matches!(
                    checked_session_update(&transaction, &session, expected_revision)?,
                    SessionCas::AlreadyDurable
                ) {
                    for (queued, run) in &interrupted {
                        let changed = transaction.execute(
                            "UPDATE bridge_runs SET experience_id=NULL,data=?3
                             WHERE session_id=?1 AND run_id=?2 AND data=?4",
                            params![
                                session.id,
                                run.run_id,
                                serde_json::to_string(run)?,
                                serde_json::to_string(queued)?
                            ],
                        )?;
                        if changed != 1 {
                            return Err(Error::Intervention(
                                "Bridge run changed during restart reconciliation".into(),
                            ));
                        }
                        save_bridge_events_in_transaction(
                            &transaction,
                            &session.id,
                            &[(
                                "run_interrupted".into(),
                                json!({"run_id":run.run_id,"reason":run.error}),
                            )],
                        )?;
                    }
                    transaction.commit()?;
                }
            }
            sessions.insert(session.id.clone(), session);
        }
        let (tx, rx) = mpsc::sync_channel(job_capacity);
        let job_permits = JobPermitPool::new(job_capacity);
        let learning_cancel = crate::cancellation::Cancellation::default();
        let bridge = Arc::new(Self {
            experiments: super::experiments::ExperimentService::open(&store.home, &config)?,
            home: store.home.clone(),
            config,
            cache: RwLock::new(cache),
            sessions: Mutex::new(sessions),
            admissions: Mutex::new(HashSet::new()),
            action_order: Mutex::new(()),
            jobs: tx,
            job_permits,
            stopping: AtomicBool::new(false),
            persistence_error: Mutex::new(None),
            shutdown_complete: (Mutex::new(false), Condvar::new()),
            learning_cancel: learning_cancel.clone(),
            #[cfg(test)]
            fail_action_before_persistence: AtomicBool::new(false),
        });
        let weak = Arc::downgrade(&bridge);
        let worker = std::thread::spawn(move || {
            let mut last_error = None;
            for job in rx {
                let Job { permit, kind } = job;
                drop(permit);
                match kind {
                    JobKind::Barrier(acknowledgement) => {
                        let result = last_error.clone().map_or(Ok(()), Err);
                        let _ = acknowledgement.try_send(result);
                    }
                    JobKind::Flush(acknowledgement) => {
                        let _ = acknowledgement.try_send(last_error.clone());
                    }
                    JobKind::Shutdown(acknowledgement) => {
                        if let Some(bridge) = weak.upgrade() {
                            let (complete, notification) = &bridge.shutdown_complete;
                            *complete.lock().expect("shutdown lock") = true;
                            notification.notify_all();
                        }
                        let _ = acknowledgement.try_send(Ok(()));
                        break;
                    }
                    #[cfg(test)]
                    JobKind::Block { entered, release } => {
                        let _ = entered.send(());
                        let _ = release.recv();
                    }
                    JobKind::Persist {
                        session,
                        expected_revision,
                        id,
                        events,
                        acknowledgement,
                    } => {
                        let result = persist_bridge_job_atomic(
                            &store,
                            session.as_deref(),
                            expected_revision,
                            &id,
                            &events,
                        );
                        let acknowledged = match result {
                            Ok(()) => Ok(()),
                            Err(error) => {
                                Err(remember_persistence_error(&weak, &mut last_error, &error))
                            }
                        };
                        if let Some(acknowledgement) = acknowledgement
                            && acknowledgement.try_send(acknowledged).is_err()
                            && session.is_some()
                        {
                            reconcile_weak_session(&weak, &store.home, &id);
                        }
                    }
                    JobKind::AdmitSession {
                        session,
                        expected_revision,
                        events,
                        acknowledgement,
                    } => {
                        let id = session.id.clone();
                        let result = persist_session_admission(
                            &store.home,
                            *session,
                            expected_revision,
                            &events,
                        );
                        match result {
                            Ok(session) => {
                                if acknowledgement.try_send(Ok(Box::new(session))).is_err() {
                                    reconcile_weak_session(&weak, &store.home, &id);
                                }
                            }
                            Err(error) => {
                                let message =
                                    remember_persistence_error(&weak, &mut last_error, &error);
                                if acknowledgement.try_send(Err(message)).is_err() {
                                    reconcile_weak_session(&weak, &store.home, &id);
                                }
                            }
                        }
                    }
                    JobKind::EndSession {
                        ended,
                        expected_revision,
                        trajectory,
                        acknowledgement,
                    } => {
                        let acknowledged = match persist_session_end_atomic(
                            &store.home,
                            &ended,
                            expected_revision,
                            trajectory.as_ref(),
                        ) {
                            Ok(()) => SessionEndAcknowledgement::Committed,
                            Err(error) => match persisted_bridge_session(&store.home, &ended.id) {
                                Ok(Some(durable))
                                    if durable.ended
                                        && durable.revision == ended.revision
                                        && serde_json::to_value(&durable).ok()
                                            == serde_json::to_value(&ended).ok() =>
                                {
                                    SessionEndAcknowledgement::Committed
                                }
                                Ok(Some(durable)) if !durable.ended => {
                                    SessionEndAcknowledgement::RolledBack {
                                        session: Box::new(durable),
                                        error: persistence_message(&error),
                                    }
                                }
                                Ok(_) => SessionEndAcknowledgement::InDoubt {
                                    error: persistence_message(&error),
                                },
                                Err(inspect) => SessionEndAcknowledgement::InDoubt {
                                    error: format!(
                                        "{}; additionally, durable session inspection failed: {}",
                                        persistence_message(&error),
                                        persistence_message(&inspect)
                                    ),
                                },
                            },
                        };
                        let committed =
                            matches!(acknowledged, SessionEndAcknowledgement::Committed);
                        if !committed {
                            let message = match &acknowledged {
                                SessionEndAcknowledgement::Committed => unreachable!(),
                                SessionEndAcknowledgement::RolledBack { error, .. }
                                | SessionEndAcknowledgement::InDoubt { error } => error,
                            };
                            publish_persistence_error(&weak, message);
                            last_error = Some(message.clone());
                            if matches!(&acknowledged, SessionEndAcknowledgement::InDoubt { .. })
                                && let Some(bridge) = weak.upgrade()
                            {
                                bridge.stopping.store(true, Ordering::Release);
                            }
                        }
                        let _ = acknowledgement.try_send(acknowledged);
                    }
                    JobKind::ActionPersistence {
                        persistence,
                        acknowledgement,
                    } => {
                        let id = persistence.session.id.clone();
                        let result = persist_action_atomic(&store, &persistence);
                        let acknowledged = match result {
                            Ok(()) => Ok(()),
                            Err(error) => {
                                Err(remember_persistence_error(&weak, &mut last_error, &error))
                            }
                        };
                        if acknowledgement.try_send(acknowledged).is_err() {
                            reconcile_weak_session(&weak, &store.home, &id);
                        }
                    }
                    JobKind::Complete {
                        snapshot,
                        expected_revision,
                        recording_clean_start,
                        mut run,
                        acknowledgement,
                    } => {
                        let id = snapshot.id.clone();
                        let queued =
                            persist_run_queued_atomic(&store, &snapshot, expected_revision, &run);
                        if let Err(error) = queued {
                            let message =
                                remember_persistence_error(&weak, &mut last_error, &error);
                            let _ = acknowledgement.try_send(Err(message));
                            continue;
                        }
                        if acknowledgement.try_send(Ok(())).is_err() {
                            reconcile_weak_session(&weak, &store.home, &id);
                        }
                        let queued_run = run.clone();
                        let mut recording_snapshot = (*snapshot).clone();
                        recording_snapshot.clean_start = recording_clean_start;
                        let completed = super::recording::record(
                            &store,
                            &recording_snapshot,
                            &run,
                            &config_for(&weak),
                            &learning_cancel,
                        );
                        match completed {
                            Ok(exp) => {
                                run.status = "completed".into();
                                run.outcome = serde_json::to_value(exp.outcome)
                                    .ok()
                                    .and_then(|value| value.as_str().map(str::to_owned))
                                    .or_else(|| Some("inconclusive".into()));
                            }
                            Err(error) => {
                                run.status = "failed".into();
                                run.error = Some(redact(&error.to_string(), 512));
                            }
                        }
                        let mut final_session = (*snapshot).clone();
                        final_session.runs.insert(run.run_id.clone(), run.clone());
                        match persist_run_final_atomic(&store, &snapshot.id, &queued_run, &run) {
                            Ok(()) => {
                                if let Some(bridge) = weak.upgrade()
                                    && let Ok(mut sessions) = bridge.sessions.try_lock()
                                    && sessions.get(&id).is_some_and(|current| {
                                        current.revision == snapshot.revision
                                    })
                                {
                                    sessions.insert(id.clone(), final_session);
                                }
                            }
                            Err(error) => {
                                remember_persistence_error(&weak, &mut last_error, &error);
                                reconcile_weak_session(&weak, &store.home, &id);
                            }
                        }
                        if let Some(bridge) = weak.upgrade()
                            && let Ok(cache) = ExperienceHotCache::load(&store)
                        {
                            *bridge.cache.write().expect("cache lock") = cache;
                        }
                    }
                }
            }
            if let Some(bridge) = weak.upgrade() {
                let (complete, notification) = &bridge.shutdown_complete;
                let mut complete = complete.lock().expect("shutdown lock");
                if bridge.stopping.load(Ordering::Acquire) {
                    *complete = true;
                    notification.notify_all();
                }
            }
        });
        Ok((bridge, worker))
    }

    fn reserve_job_permit(&self, error: &str) -> Result<JobPermit> {
        self.job_permits.try_acquire().ok_or_else(|| invalid(error))
    }

    fn try_send_job(&self, kind: JobKind, error: &str) -> Result<()> {
        let permit = self.reserve_job_permit(error)?;
        self.send_reserved_job(kind, permit, error)
    }

    fn send_reserved_job(&self, kind: JobKind, permit: JobPermit, error: &str) -> Result<()> {
        self.jobs
            .try_send(Job { permit, kind })
            .map_err(|_| invalid(error))
    }

    pub fn flush(&self) -> Result<()> {
        self.flush_with_timeout(DEFAULT_FLUSH_TIMEOUT)
    }

    pub(crate) fn flush_with_timeout(&self, timeout: Duration) -> Result<()> {
        if self.shutdown_complete() {
            return self
                .persistence_error
                .lock()
                .expect("error lock")
                .clone()
                .map_or(Ok(()), |error| Err(invalid(&error)));
        }
        let started = std::time::Instant::now();
        let (tx, rx) = mpsc::sync_channel(1);
        self.try_send_job(
            JobKind::Flush(tx),
            "Bridge writer queue full or unavailable; flush not acknowledged",
        )?;
        let remaining = timeout.saturating_sub(started.elapsed());
        if let Some(error) = rx
            .recv_timeout(remaining)
            .map_err(|_| invalid("Bridge flush timed out"))?
        {
            return Err(invalid(&error));
        }
        Ok(())
    }

    fn lifecycle_ack_failure(&self, operation: &str, reason: &str) -> Error {
        self.stopping.store(true, Ordering::Release);
        let message = format!(
            "Bridge {operation} persistence acknowledgement {reason}; Bridge is stopping because durable state may be in doubt"
        );
        if let Ok(mut current) = self.persistence_error.lock() {
            *current = Some(message.clone());
        }
        invalid(&message)
    }

    fn persistence_barrier(&self) -> Result<()> {
        self.persistence_barrier_with_timeout(LIFECYCLE_ACK_TIMEOUT)
    }

    fn persistence_barrier_with_timeout(&self, timeout: Duration) -> Result<()> {
        if let Some(error) = self.persistence_error.lock().expect("error lock").clone() {
            return Err(invalid(&error));
        }
        if self.shutdown_complete() {
            return Ok(());
        }
        let (tx, rx) = mpsc::sync_channel(1);
        self.try_send_job(
            JobKind::Barrier(tx),
            "Bridge writer queue full or unavailable; barrier not acknowledged",
        )?;
        match rx.recv_timeout(timeout) {
            Ok(result) => result.map_err(|error| invalid(&error)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err(self.lifecycle_ack_failure("barrier", "timed out"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(self.lifecycle_ack_failure("barrier", "channel disconnected"))
            }
        }
    }
    pub fn refresh(&self) -> Result<()> {
        let cache = ExperienceHotCache::load(&Store::open(&self.home)?)?;
        *self.cache.write().expect("cache lock") = cache;
        Ok(())
    }
    fn enqueue(&self, id: &str, kind: &str, data: Value) -> Result<()> {
        self.try_send_job(
            JobKind::Persist {
                session: None,
                expected_revision: None,
                id: id.into(),
                events: vec![(kind.into(), data)],
                acknowledgement: None,
            },
            "Bridge persistence queue full or unavailable; observation not acknowledged",
        )
    }

    fn enqueue_session(&self, session: &Session, kind: &str, data: Value) -> Result<()> {
        self.enqueue_session_events(session, vec![(kind.into(), data)])
    }

    fn enqueue_session_events(
        &self,
        session: &Session,
        events: Vec<(String, Value)>,
    ) -> Result<()> {
        self.enqueue_session_events_with_timeout(session, events, LIFECYCLE_ACK_TIMEOUT)
    }

    fn enqueue_session_events_with_timeout(
        &self,
        session: &Session,
        events: Vec<(String, Value)>,
        timeout: Duration,
    ) -> Result<()> {
        let expected_revision = session
            .revision
            .checked_sub(1)
            .ok_or_else(|| invalid("Bridge session revision cannot be zero"))?;
        let (acknowledgement, received) = mpsc::sync_channel(1);
        self.try_send_job(
            JobKind::Persist {
                session: Some(Box::new(session.clone())),
                expected_revision: Some(expected_revision),
                id: session.id.clone(),
                events,
                acknowledgement: Some(acknowledgement),
            },
            "Bridge persistence queue full or unavailable; observation not acknowledged",
        )?;
        match received.recv_timeout(timeout) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                self.stopping.store(true, Ordering::Release);
                Err(invalid(&error))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err(self.lifecycle_ack_failure("session update", "timed out"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(self.lifecycle_ack_failure("session update", "channel disconnected"))
            }
        }
    }

    fn admit_session(
        &self,
        session: Session,
        expected_revision: Option<u64>,
        events: Vec<(String, Value)>,
    ) -> Result<Session> {
        self.admit_session_with_timeout(session, expected_revision, events, LIFECYCLE_ACK_TIMEOUT)
    }

    fn admit_session_with_timeout(
        &self,
        session: Session,
        expected_revision: Option<u64>,
        events: Vec<(String, Value)>,
        timeout: Duration,
    ) -> Result<Session> {
        let id = session.id.clone();
        let (acknowledgement, received) = mpsc::sync_channel(1);
        self.try_send_job(
            JobKind::AdmitSession {
                session: Box::new(session),
                expected_revision,
                events,
                acknowledgement,
            },
            "Bridge persistence queue full or unavailable; session start not acknowledged",
        )?;
        match received.recv_timeout(timeout) {
            Ok(Ok(session)) => Ok(*session),
            Ok(Err(error)) => {
                self.reconcile_session(&id);
                Err(invalid(&error))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.reconcile_session(&id);
                Err(self.lifecycle_ack_failure("session admission", "timed out"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.reconcile_session(&id);
                Err(self.lifecycle_ack_failure("session admission", "channel disconnected"))
            }
        }
    }

    fn persist_session_end(
        &self,
        ended: Session,
        expected_revision: u64,
        trajectory: Option<(
            crate::core::TrajectoryId,
            crate::predictive::TrajectoryOutcome,
        )>,
    ) -> Result<SessionEndAcknowledgement> {
        self.persist_session_end_with_timeout(
            ended,
            expected_revision,
            trajectory,
            LIFECYCLE_ACK_TIMEOUT,
        )
    }

    fn persist_session_end_with_timeout(
        &self,
        ended: Session,
        expected_revision: u64,
        trajectory: Option<(
            crate::core::TrajectoryId,
            crate::predictive::TrajectoryOutcome,
        )>,
        timeout: Duration,
    ) -> Result<SessionEndAcknowledgement> {
        let (acknowledgement, received) = mpsc::sync_channel(1);
        self.try_send_job(
            JobKind::EndSession {
                ended: Box::new(ended),
                expected_revision,
                trajectory,
                acknowledgement,
            },
            "Bridge persistence queue full or unavailable; session end not acknowledged",
        )?;
        match received.recv_timeout(timeout) {
            Ok(result) => Ok(result),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err(self.lifecycle_ack_failure("session end", "timed out"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(self.lifecycle_ack_failure("session end", "channel disconnected"))
            }
        }
    }
    #[cfg(test)]
    fn persist_action_with_timeout(
        &self,
        persistence: ActionPersistence,
        timeout: Duration,
    ) -> Result<()> {
        let permit = self.reserve_job_permit(
            "Bridge persistence queue full or unavailable; action not acknowledged",
        )?;
        self.persist_action_with_permit(persistence, permit, timeout)
    }

    fn persist_action_with_permit(
        &self,
        persistence: ActionPersistence,
        permit: JobPermit,
        timeout: Duration,
    ) -> Result<()> {
        let (acknowledgement, received) = mpsc::sync_channel(1);
        self.send_reserved_job(
            JobKind::ActionPersistence {
                persistence: Box::new(persistence),
                acknowledgement,
            },
            permit,
            "Bridge persistence queue full or unavailable; action not acknowledged",
        )?;
        match received.recv_timeout(timeout) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                self.stopping.store(true, Ordering::Release);
                Err(invalid(&error))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err(self.lifecycle_ack_failure("action commit", "timed out"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(self.lifecycle_ack_failure("action commit", "channel disconnected"))
            }
        }
    }

    fn enqueue_run_completion(
        &self,
        snapshot: Session,
        expected_revision: u64,
        recording_clean_start: bool,
        run: RunRecord,
    ) -> Result<()> {
        let (acknowledgement, received) = mpsc::sync_channel(1);
        self.try_send_job(
            JobKind::Complete {
                snapshot: Box::new(snapshot),
                expected_revision,
                recording_clean_start,
                run,
                acknowledgement,
            },
            "Learning queue full",
        )?;
        match received.recv_timeout(LIFECYCLE_ACK_TIMEOUT) {
            Ok(result) => result.map_err(|error| invalid(&error)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err(self.lifecycle_ack_failure("recording queue", "timed out"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(self.lifecycle_ack_failure("recording queue", "channel disconnected"))
            }
        }
    }

    fn reconcile_session(&self, id: &str) {
        let Ok(durable) = persisted_session_with_runs(&self.home, id) else {
            return;
        };
        let mut sessions = self.sessions.lock().expect("session lock");
        match durable {
            Some(session) if !session.ended => {
                sessions.insert(id.to_owned(), session);
            }
            _ => {
                sessions.remove(id);
            }
        }
    }

    pub fn shutdown_complete(&self) -> bool {
        *self.shutdown_complete.0.lock().expect("shutdown lock")
    }

    pub fn wait_for_shutdown_complete(&self, timeout: Duration) -> bool {
        let (complete, notification) = &self.shutdown_complete;
        let guard = complete.lock().expect("shutdown lock");
        if *guard {
            return true;
        }
        let (guard, _) = notification
            .wait_timeout_while(guard, timeout, |complete| !*complete)
            .expect("shutdown lock");
        *guard
    }

    fn request_shutdown(&self) -> Result<()> {
        self.stopping.store(true, Ordering::Release);
        self.learning_cancel.cancel();
        self.experiments.request_shutdown();
        if self.shutdown_complete() {
            return self.flush_with_timeout(Duration::ZERO);
        }
        let (acknowledgement, received) = mpsc::sync_channel(1);
        self.try_send_job(
            JobKind::Shutdown(acknowledgement),
            "Bridge shutdown could not enter the writer queue",
        )
        .map_err(|_| self.lifecycle_ack_failure("shutdown", "could not enter the writer queue"))?;
        match received.recv_timeout(LIFECYCLE_ACK_TIMEOUT) {
            Ok(result) => result.map_err(|error| invalid(&error)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err(self.lifecycle_ack_failure("shutdown", "timed out"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(self.lifecycle_ack_failure("shutdown", "channel disconnected"))
            }
        }
    }
    pub fn handle(&self, event: AgentEvent) -> Result<Value> {
        if self.stopping.load(Ordering::Relaxed)
            && !matches!(
                &event,
                AgentEvent::Status
                    | AgentEvent::Sessions
                    | AgentEvent::Inspect { .. }
                    | AgentEvent::Events { .. }
                    | AgentEvent::Shutdown
            )
        {
            return Err(invalid("Bridge stopping"));
        }
        match event {
            AgentEvent::CurriculumRequested(request) => {
                let session =
                    self.with_session(&request.hardknock_session_id, |s| Ok(s.clone()))?;
                self.context_config(&session.agent.name)?;
                self.experiments
                    .request_curriculum(request, &session, &self.config)
            }
            AgentEvent::CurriculumStarted {
                hardknock_session_id,
                curriculum_id,
            } => {
                let session = self.with_session(&hardknock_session_id, |s| Ok(s.clone()))?;
                self.context_config(&session.agent.name)?;
                self.experiments
                    .start_curriculum(&session, &curriculum_id, &self.config)
            }
            AgentEvent::CurriculumProgress {
                hardknock_session_id,
                curriculum_id,
                after,
            } => {
                self.with_session(&hardknock_session_id, |_| Ok(()))?;
                self.experiments
                    .poll_curriculum(&hardknock_session_id, &curriculum_id, after)
            }
            AgentEvent::CurriculumCancelled {
                hardknock_session_id,
                curriculum_id,
            } => {
                self.with_session(&hardknock_session_id, |_| Ok(()))?;
                self.experiments
                    .cancel_curriculum(&hardknock_session_id, &curriculum_id)
            }
            AgentEvent::SkillPackageRequested {
                hardknock_session_id,
                skill,
                profile,
            } => {
                let session = self.with_session(&hardknock_session_id, |s| Ok(s.clone()))?;
                let store = Store::open(&self.home)?;
                let s = store.skill(&skill)?;
                if store
                    .experience(&s.source_experience)?
                    .starting_state
                    .repo_path
                    != session.starting_state.repo_path
                {
                    return Err(invalid("Skill belongs to another repository"));
                }
                let p = crate::curriculum::skill_package(
                    &store,
                    &skill,
                    &profile,
                    &self.config.curriculum,
                )?;
                Ok(
                    json!({"skill":p.skill,"maturity":p.maturity,"profile_coverage":{"profile":p.coverage.profile,"tested_conditions":p.coverage.tested_conditions,"configured_conditions":p.coverage.configured_conditions,"profile_coverage":p.coverage.profile_coverage,"dimensions":p.coverage.dimensions.iter().take(32).map(|d|json!({"name":d.name,"unknown":d.unknown.iter().take(16).collect::<Vec<_>>(),"latest_observations":d.tested.iter().rev().take(3).collect::<Vec<_>>()})).collect::<Vec<_>>()},"lessons":p.lessons.iter().take(32).collect::<Vec<_>>(),"reflexes":p.reflexes.iter().take(32).collect::<Vec<_>>(),"recoveries":p.recoveries.iter().take(32).collect::<Vec<_>>(),"provenance":"Inspect local skill package for complete versioned evidence"}),
                )
            }
            AgentEvent::SessionStarted(start) => self.start(start),
            AgentEvent::Status => {
                let sessions = self.sessions.lock().expect("session lock");
                let agents: std::collections::BTreeSet<_> = sessions
                    .values()
                    .filter(|s| !s.ended)
                    .map(|s| s.agent.name.clone())
                    .collect();
                Ok(
                    json!({"status":if self.shutdown_complete() {"stopped"} else if self.stopping.load(Ordering::Acquire) {"stopping"} else {"running"},"protocol":PROTOCOL_VERSION,"sessions":sessions.values().filter(|s| !s.ended).count(),"adapters":agents,"persistence_error":*self.persistence_error.lock().expect("error lock"),"shutdown_complete":self.shutdown_complete()}),
                )
            }
            AgentEvent::Sessions => Ok(
                json!({"sessions":self.sessions.lock().expect("session lock").values().map(session_summary).collect::<Vec<_>>()}),
            ),
            AgentEvent::Inspect {
                hardknock_session_id: id,
            } => self.inspect_session(&id),
            AgentEvent::RunStatus {
                hardknock_session_id: id,
                run_id,
            } => self.run_status(&id, &run_id),
            AgentEvent::Events { after } => Store::open(&self.home)?.bridge_events(after),
            AgentEvent::RefreshCache => {
                self.refresh()?;
                Ok(json!({"refreshed":true}))
            }
            AgentEvent::Shutdown => {
                self.request_shutdown()?;
                Ok(json!({"stopping":true,"shutdown_complete":self.shutdown_complete()}))
            }
            AgentEvent::ContextRequested(request) => {
                self.refresh()?;
                let cwd =
                    self.with_session(&request.hardknock_session_id, |s| Ok(s.cwd.clone()))?;
                let (starting_state, context, clean_start) =
                    super::recording::capture_context(&cwd, &EnvironmentSummary::default())?;
                let (response, context, agent) =
                    self.with_session(&request.hardknock_session_id, |s| {
                        if s.actions.len() == s.next_action_start {
                            s.starting_state = starting_state;
                            s.context = context;
                            s.clean_start = clean_start;
                        }
                        if let Some(task) = request.task {
                            s.task = redact(&task, 512);
                        }
                        let lessons = self.cache.read().expect("cache lock").retrieve(
                            &s.context,
                            &s.task,
                            vec![],
                        );
                        let response =
                            context_response(&s.id, &lessons, &self.context_config(&s.agent.name)?);
                        s.delivered = lessons
                            .into_iter()
                            .filter(|l| {
                                response
                                    .relevant_experience
                                    .iter()
                                    .any(|b| b.id == l.lesson.id.to_string())
                            })
                            .collect();
                        s.revision += 1;
                        self.enqueue_session(
                            s,
                            "experience_injected",
                            json!({"count":s.delivered.len()}),
                        )?;
                        Ok((response, s.context.clone(), s.agent.clone()))
                    })?;
                Ok(serde_json::to_value(
                    self.development_response(response, &context, &agent)?,
                )?)
            }
            AgentEvent::EffectProposed(mut proposal) => {
                let agent = self.with_session(&proposal.hardknock_session_id, |session| {
                    if session.ended {
                        return Err(invalid("Session has ended"));
                    }
                    Ok(session.agent.name.clone())
                })?;
                if proposal.request.session_id != proposal.hardknock_session_id {
                    return Err(invalid("Effect proposal session binding mismatch"));
                }
                proposal.request.evidence.truncate(128);
                let store = Store::open(&self.home)?;
                let manager = crate::effects::EffectManager::new(&store)?;
                let (effect, prepared) = manager.propose_and_prepare(
                    proposal.request,
                    &crate::effects::EffectManager::agent_context(&agent),
                )?;
                self.enqueue(
                    &proposal.hardknock_session_id,
                    "effect_prepared",
                    json!({"effect_id":effect.id,"prepared_id":prepared.id,"committed":false}),
                )?;
                Ok(json!({
                    "effect_id":effect.id,
                    "status":"prepared",
                    "committed":false,
                    "preview":prepared.preview,
                    "message":"The effect is prepared only. No authoritative external mutation has occurred."
                }))
            }
            AgentEvent::RealityEffectProposed {
                reality_id,
                mut request,
            } => {
                request.reality_id = Some(reality_id.clone());
                request.session_id = format!("reality:{reality_id}");
                request.evidence.truncate(128);
                let store = Store::open(&self.home)?;
                let manager = crate::effects::EffectManager::new(&store)?;
                let (effect, prepared) = manager.propose_and_prepare(
                    request,
                    &crate::effects::EffectManager::agent_context(&format!(
                        "isolated-reality:{reality_id}"
                    )),
                )?;
                Ok(json!({
                    "effect_id":effect.id,
                    "status":"prepared",
                    "committed":false,
                    "preview":prepared.preview,
                    "message":"Prepared through the scoped Reality channel. No authoritative external mutation occurred."
                }))
            }
            AgentEvent::RealityEffectStatus {
                reality_id,
                effect_id,
            } => {
                let store = Store::open(&self.home)?;
                let effect = store.effect(&effect_id)?;
                if effect.reality_id.as_ref() != Some(&reality_id) {
                    return Err(Error::Intervention(
                        "Effect is outside the authenticated Reality scope".into(),
                    ));
                }
                Ok(json!({
                    "effect":effect,
                    "events":store.effect_events(&effect_id)?,
                    "prepared":store.prepared_effect(&effect_id).ok(),
                    "committed":store.commit_receipt_for_effect(&effect_id)?
                }))
            }
            AgentEvent::RealityEffectDiscardRequested {
                reality_id,
                effect_id,
            } => {
                let store = Store::open(&self.home)?;
                let effect = store.effect(&effect_id)?;
                if effect.reality_id.as_ref() != Some(&reality_id) {
                    return Err(Error::Intervention(
                        "Effect is outside the authenticated Reality scope".into(),
                    ));
                }
                let effect = crate::effects::EffectManager::new(&store)?.discard(
                    &effect_id,
                    &crate::effects::EffectManager::agent_context(&format!(
                        "isolated-reality:{reality_id}"
                    )),
                )?;
                Ok(json!({"effect":effect,"committed":false}))
            }
            AgentEvent::EffectCommitRequested {
                hardknock_session_id,
                effect_id,
            } => {
                let agent = self.with_session(&hardknock_session_id, |session| {
                    Ok(session.agent.name.clone())
                })?;
                let store = Store::open(&self.home)?;
                let manager = crate::effects::EffectManager::new(&store)?;
                match manager.commit(
                    &effect_id,
                    None,
                    &crate::effects::EffectManager::agent_context(&agent),
                ) {
                    Ok(result) => Ok(json!({"effect_id":effect_id,"result":result})),
                    Err(Error::Intervention(reason)) => Ok(json!({
                        "effect_id":effect_id,
                        "status":"authorization_required",
                        "committed":false,
                        "reason":reason
                    })),
                    Err(error) => Err(error),
                }
            }
            AgentEvent::EffectDiscardRequested {
                hardknock_session_id,
                effect_id,
            } => {
                let agent = self.with_session(&hardknock_session_id, |session| {
                    Ok(session.agent.name.clone())
                })?;
                let store = Store::open(&self.home)?;
                let effect = crate::effects::EffectManager::new(&store)?.discard(
                    &effect_id,
                    &crate::effects::EffectManager::agent_context(&agent),
                )?;
                self.enqueue(
                    &hardknock_session_id,
                    "effect_discarded",
                    json!({"effect_id":effect_id}),
                )?;
                Ok(json!({"effect":effect,"committed":false}))
            }
            AgentEvent::EffectStatus {
                hardknock_session_id,
                effect_id,
            } => {
                self.with_session(&hardknock_session_id, |_| Ok(()))?;
                let store = Store::open(&self.home)?;
                Ok(json!({
                    "effect":store.effect(&effect_id)?,
                    "prepared":store.prepared_effect(&effect_id).ok(),
                    "receipt":store.commit_receipt_for_effect(&effect_id)?,
                    "events":store.effect_events(&effect_id)?
                }))
            }
            AgentEvent::EffectReconcileRequested {
                hardknock_session_id,
                effect_id,
            } => {
                self.with_session(&hardknock_session_id, |_| Ok(()))?;
                let store = Store::open(&self.home)?;
                let result = crate::effects::EffectManager::new(&store)?.reconcile(&effect_id)?;
                self.enqueue(
                    &hardknock_session_id,
                    "effect_reconciled",
                    json!({"effect_id":effect_id,"result":result}),
                )?;
                Ok(json!({"effect_id":effect_id,"result":result}))
            }
            AgentEvent::ActionProposed(mut proposed) => {
                valid_id(&proposed.action_id)?;
                validate_action(&proposed.action)?;
                let _action_order = self.action_order.lock().expect("action order lock");
                let predictive_enabled = self.config.runtime.forecast.mode
                    != crate::runtime::ForecastRuntimeMode::Off
                    && !self
                        .cache
                        .read()
                        .expect("cache lock")
                        .warning_signatures
                        .is_empty();
                if predictive_enabled {
                    self.persistence_barrier()?;
                }
                let mut action_permit = Some(self.reserve_job_permit(
                    "Bridge persistence queue full or unavailable; action not acknowledged",
                )?);
                let session_id = proposed.hardknock_session_id.clone();
                let result = self.with_session(&session_id, |s| {
                    if s.ended { return Err(invalid("Session has ended")); }
                    for run in Store::open(&self.home)?.bridge_runs(&s.id)? {
                        s.runs.insert(run.run_id.clone(), run);
                    }
                    let expected_revision = s.revision;
                    normalize_cwd(&mut proposed.action, s);
                    sanitize_action(&mut proposed.action)?;
                    if let Some(existing) = s.actions.iter().find(|a|a.action_id == proposed.action_id) {
                        if existing.action != proposed.action { return Err(invalid("Action id reused with different action")); }
                        let session_id=crate::core::HardknockSessionId::from_external(&s.id);
                        if proposed.context.team.is_none() && Store::open(&self.home)?.agent_teams()?.iter().all(|t| t.members.iter().all(|m| m.session!=session_id)) && Store::open(&self.home)?.knowledge_hierarchies()?.is_empty() { return Ok(serde_json::to_value(&existing.decision)?); }
                        return Err(Error::Intervention("Repeat action requires a new action id and fresh knowledge resolution".into()));
                    }
                    if s.actions.len() >= self.config.bridge.max_actions { return Err(invalid("Session action budget exhausted")); }
                    let (mut runtime_context,mut runtime_evaluation)=self.cache.read().expect("cache lock").evaluate_runtime(RuntimeEvaluationRequest {
                        context: &s.context,
                        proposed: &proposed,
                        failures: s.consecutive_failures,
                        bridge: &self.config.bridge,
                        runtime: &self.config.runtime,
                        agent: &s.agent,
                        task: &s.task,
                    })?;
                    let knowledge_store = Store::open(&self.home)?;
                    let mut prepared_trajectory = None;
                    if let Some(trajectory_id)=&s.trajectory_id {
                        let mut features=std::collections::BTreeMap::new();
                        features.insert("retry_count".into(),crate::predictive::TrajectoryValue::Integer(i64::from(s.consecutive_failures)));
                        features.insert("no_state_change".into(),crate::predictive::TrajectoryValue::Boolean(proposed.context.no_state_change));
                        features.insert("config_changed".into(),crate::predictive::TrajectoryValue::Boolean(proposed.context.config_changed));
                        features.insert("action_kind".into(),crate::predictive::TrajectoryValue::Text(action_type(&proposed.action).into()));
                        let trajectory_event=crate::store::NewTrajectoryEvent {kind:crate::predictive::TrajectoryEventKind::ActionProposed,observation:crate::predictive::TrajectoryObservation{features},evidence:vec![]};
                        let prepared=knowledge_store.prepare_trajectory_mutation(
                            trajectory_id,
                            trajectory_event,
                            predictive_enabled.then_some(self.config.runtime.forecast.policy),
                        )?;
                        if predictive_enabled {
                            runtime_context.active_forecasts=prepared.forecasts().to_vec();
                            let signatures:std::collections::BTreeSet<_>=runtime_context.active_forecasts.iter().map(|item|item.signature.clone()).collect();
                            runtime_context.preventive_interventions=self.cache.read().expect("cache lock").preventive_interventions.iter().filter(|item|signatures.contains(&item.signature)).cloned().collect();
                            runtime_evaluation=crate::runtime::DeterministicRuntimeController::with_config(self.config.runtime.policy_config())?.evaluate(&runtime_context)?;
                        }
                        prepared_trajectory=Some(prepared);
                    }
                    runtime_context.knowledge_action_id=Some(proposed.action_id.clone());
                    runtime_context.team = proposed.context.team.clone();
                    if let Some(mut plan) = proposed.context.plan.clone() {
                        plan.validity = None;
                        plan.crossed_commitments.clear();
                        runtime_context.plan = Some(plan);
                    }
                    if let Some(mut composition) = proposed.context.composition.clone() {
                        for claim in &mut composition.state { claim.source = crate::composition::StateClaimSource::AgentReported; }
                        for handoff in &mut composition.current_handoffs { for claim in &mut handoff.facts { claim.source = crate::composition::StateClaimSource::AgentReported; } }
                        runtime_context.composition = Some(composition);
                    }
                    for (key,value) in &proposed.context.knowledge_reports {runtime_context.context_observations.entry(key.clone()).or_default().push(crate::knowledge_runtime::ContextValue{value:value.clone(),source:crate::knowledge_runtime::ContextValueSource::AgentReported});}
                    knowledge_store.attach_runtime_knowledge_read_only(&mut runtime_context)?;
                    knowledge_store.attach_team_authority(&mut runtime_context)?;
                    if runtime_context.operational_knowledge.is_some() || runtime_context.team.is_some() {
                        runtime_evaluation=crate::runtime::DeterministicRuntimeController::with_config(self.config.runtime.policy_config())?.evaluate(&runtime_context)?;
                    }
                    let decision = bridge_decision_from_runtime(&runtime_evaluation,self.config.runtime.mode);
                    let runtime_record=crate::runtime::RuntimeDecisionRecord {
                        id: RuntimeDecisionId::new(),
                        session_id: runtime_context.session_id.clone(),
                        context_hash: runtime_context.context_hash()?,
                        context: runtime_context,
                        decision: runtime_evaluation.decision.clone(),
                        evaluation: runtime_evaluation,
                        created_at: Utc::now(),
                    };
                    let prepared_runtime_decision=knowledge_store.prepare_runtime_decision(
                        &runtime_record,
                        self.config.runtime.policy_config(),
                    )?;
                    let mut response=serde_json::to_value(&decision)?;
                    let mut prepared_role_knowledge_view=None;
                    if runtime_record.context.team.is_some() {
                        let prepared=knowledge_store.prepare_role_knowledge_view(&runtime_record.context)?;
                        let view=&prepared.view;
                        response["knowledge"]=serde_json::to_value(&view.bundle)?;
                        response["experience"]=serde_json::to_value(&view.lessons)?;
                        response["team_knowledge"]=serde_json::to_value(serde_json::json!({"mode":view.mode,"visible_artifacts":view.visible_artifacts,"hidden_artifacts":view.hidden_artifacts,"snapshot":view.snapshot}))?;
                        prepared_role_knowledge_view=Some(prepared);
                    } else if let Some(k)=&runtime_record.context.operational_knowledge { response["knowledge"]=serde_json::to_value(&k.bundle)?; }
                    // Deliver matching action-time advice as well as startup context.
                    if matches!(&proposed.action, NormalizedAction::Shell { .. }) {
                        // Runtime evaluation already ranked this exact context/action. Reuse its
                        // bounded result instead of scanning and sorting the hot cache twice.
                        for lesson in &runtime_record.context.relevant_experience.lessons {
                            if let Some(current) = s.delivered.iter_mut().find(|l|l.lesson.id == lesson.lesson.id) { if f64::from(lesson.relevance) > f64::from(current.relevance) { *current = lesson.clone(); } }
                            else if proposed.context.can_intercept && decision.references_lesson(&lesson.lesson.id.to_string()) { s.delivered.push(lesson.clone()); }
                        }
                    }
                    s.actions.push(RecordedAction { action_id: proposed.action_id.clone(), action: proposed.action,
                        decision: decision.clone(), result: None, duration_ms: 0, can_intercept: proposed.context.can_intercept });
                    s.revision += 1;
                    let mut events=vec![("action_proposed".into(),json!({"action_id":proposed.action_id,"decision":decision,"runtime_decision_id":runtime_record.id}))];
                    if matches!(decision,ActionDecision::Warn{..}|ActionDecision::Replan{..}) {
                        events.push(("reflex_matched".into(),json!({"action_id":proposed.action_id})));
                    }
                    #[cfg(test)]
                    if self.fail_action_before_persistence.swap(false, Ordering::Relaxed) {
                        return Err(invalid("Injected action failure before persistence"));
                    }
                    self.persist_action_with_permit(
                        ActionPersistence {
                            session: Box::new(s.clone()),
                            expected_revision,
                            events,
                            trajectory: prepared_trajectory,
                            runtime_decision: Some(prepared_runtime_decision),
                            role_knowledge_view: prepared_role_knowledge_view,
                        },
                        action_permit.take().expect("action permit is available"),
                        ACTION_COMMIT_TIMEOUT,
                    )?;
                    Ok(response)
                });
                if result.is_err() {
                    self.reconcile_session(&session_id);
                }
                result
            }
            AgentEvent::RuntimeDecisionRequested(request) => {
                self.handle(AgentEvent::ActionProposed(ActionProposed {
                    hardknock_session_id: request.hardknock_session_id,
                    action_id: request.action_id,
                    action: request.action,
                    context: request.context,
                }))
            }
            AgentEvent::RuntimeDecisionMade {
                hardknock_session_id,
                decision_id,
            } => {
                self.with_session(&hardknock_session_id, |_| Ok(()))?;
                let store = Store::open(&self.home)?;
                let record = store.runtime_decision(&decision_id)?;
                if record.session_id
                    != crate::core::HardknockSessionId::from_external(&hardknock_session_id)
                {
                    return Err(invalid("Runtime decision belongs to a different session"));
                }
                Ok(serde_json::to_value(record)?)
            }
            AgentEvent::RuntimeDecisionFeedback(report) => {
                self.with_session(&report.hardknock_session_id, |_| Ok(()))?;
                let store = Store::open(&self.home)?;
                let record = store.runtime_decision(&report.feedback.decision_id)?;
                if record.session_id
                    != crate::core::HardknockSessionId::from_external(&report.hardknock_session_id)
                {
                    return Err(invalid("Runtime feedback belongs to a different session"));
                }
                store.record_runtime_feedback(&report.feedback)?;
                Ok(json!({"accepted":true,"decision_id":record.id}))
            }
            AgentEvent::EvidenceRequested(request) => {
                self.with_session(&request.hardknock_session_id, |_| Ok(()))?;
                let store = Store::open(&self.home)?;
                Ok(serde_json::to_value(
                    store.epistemic_report(&request.claim_id)?,
                )?)
            }
            AgentEvent::EvidencePathReported(report) => {
                let session =
                    self.with_session(&report.hardknock_session_id, |session| Ok(session.clone()))?;
                let crate::epistemic::EvidenceSource::Agent { identity } = &report.path.source
                else {
                    return Err(invalid(
                        "Agent Bridge reports may only submit agent-generated EvidencePaths",
                    ));
                };
                if identity.kind != session.agent.name {
                    return Err(invalid(
                        "EvidencePath agent identity does not match the authenticated session",
                    ));
                }
                if report
                    .path
                    .context
                    .repository
                    .as_ref()
                    .is_some_and(|repository| {
                        repository != &session.starting_state.repo_path.display().to_string()
                    })
                {
                    return Err(invalid(
                        "EvidencePath repository does not match the authenticated session",
                    ));
                }
                let store = Store::open(&self.home)?;
                let path = store.insert_evidence_path(&report.path)?;
                self.enqueue(
                    &report.hardknock_session_id,
                    "evidence_path_reported",
                    json!({"path_id":path.id,"claim_id":path.claim.id}),
                )?;
                Ok(json!({"accepted":true,"path":path}))
            }
            AgentEvent::EvidenceChallengeRequested(request) => {
                self.with_session(&request.hardknock_session_id, |_| Ok(()))?;
                let report = Store::open(&self.home)?.epistemic_report(&request.claim_id)?;
                Ok(json!({"claim":report.claim,"plan":report.challenge}))
            }
            AgentEvent::EvidenceAssessmentUpdated(report) => {
                self.with_session(&report.hardknock_session_id, |_| Ok(()))?;
                let store = Store::open(&self.home)?;
                store.record_fused_assessment(&report.assessment)?;
                self.enqueue(
                    &report.hardknock_session_id,
                    "evidence_assessment_updated",
                    json!({"claim_id":report.assessment.claim,"status":report.assessment.status}),
                )?;
                Ok(json!({"accepted":true,"claim_id":report.assessment.claim}))
            }
            AgentEvent::ActionCompleted(mut completed) => {
                valid_id(&completed.action_id)?;
                validate_action(&completed.action)?;
                if completed.result.success && completed.result.exit_code.is_some_and(|c| c != 0) {
                    return Err(invalid("Success conflicts with exit code"));
                }
                sanitize_action(&mut completed.action)?;
                if let Some(output) = &mut completed.result.output_summary {
                    *output = redact(output, MAX_OUTPUT_BYTES);
                }
                if let Some(class) = &mut completed.result.error_class {
                    *class = redact(class, 128);
                }
                // References are metadata only: never open adapter-supplied paths.
                completed.result.artifacts.truncate(16);
                for a in &mut completed.result.artifacts {
                    a.uri = redact(&a.uri, 512);
                    a.description = a.description.as_ref().map(|s| redact(s, 256));
                }
                let _action_order = self.action_order.lock().expect("action order lock");
                let mut action_permit = Some(self.reserve_job_permit(
                    "Bridge persistence queue full or unavailable; action not acknowledged",
                )?);
                let session_id = completed.hardknock_session_id.clone();
                let result = self.with_session(&session_id, |s| {
                    for run in Store::open(&self.home)?.bridge_runs(&s.id)? {
                        s.runs.insert(run.run_id.clone(), run);
                    }
                    let expected_revision = s.revision;
                    normalize_cwd(&mut completed.action, s);
                    let action = s.actions.iter_mut().find(|a|a.action_id == completed.action_id).ok_or_else(||invalid("Completion has no corresponding action proposal"))?;
                    if action.action != completed.action { return Err(invalid("Completion action differs from proposal")); }
                    if let Some(result) = &action.result { if result != &completed.result { return Err(invalid("Conflicting duplicate action result")); } return Ok(json!({"accepted":true,"duplicate":true})); }
                    action.result = Some(completed.result); action.duration_ms = completed.duration_ms;
                    s.consecutive_failures = if action.result.as_ref().is_some_and(|r|r.success) { 0 } else { s.consecutive_failures.saturating_add(1) };
                    let failed_signature=action.result.as_ref().filter(|result|!result.success).and_then(|result|result.error_class.clone());
                    let completed_action=action.action.clone();
                    let completed_action_id=action.action_id.clone();
                    let completed_success=action.result.as_ref().map(|result|result.success);
                    s.revision += 1;
                    let store=Store::open(&self.home)?;
                    let mut prepared_trajectory = None;
                    if let Some(trajectory_id)=&s.trajectory_id {
                        let result=action.result.as_ref().expect("set above");
                        let mut features=std::collections::BTreeMap::new();
                        features.insert("success".into(),crate::predictive::TrajectoryValue::Boolean(result.success));
                        features.insert("duration_ms".into(),crate::predictive::TrajectoryValue::Integer(i64::try_from(action.duration_ms).unwrap_or(i64::MAX)));
                        features.insert("tool_failure_count".into(),crate::predictive::TrajectoryValue::Integer(i64::from(s.consecutive_failures)));
                        let trajectory_event=crate::store::NewTrajectoryEvent{kind:if result.success {crate::predictive::TrajectoryEventKind::ActionCompleted}else{crate::predictive::TrajectoryEventKind::FailureObserved},observation:crate::predictive::TrajectoryObservation{features},evidence:vec![]};
                        prepared_trajectory=Some(store.prepare_trajectory_mutation(
                            trajectory_id,
                            trajectory_event,
                            None,
                        )?);
                    }
                    let mut events=vec![("action_completed".into(),json!({"action_id":completed_action_id,"success":completed_success}))];
                    let (response,prepared_runtime_decision)=if let Some(signature)=failed_signature {
                        let proposal=ActionProposed{hardknock_session_id:s.id.clone(),action_id:format!("recovery:{completed_action_id}"),action:completed_action,context:Default::default()};
                        let cache=self.cache.read().expect("cache lock");
                        let (mut runtime_context,_)=cache.evaluate_runtime(RuntimeEvaluationRequest {
                            context: &s.context,
                            proposed: &proposal,
                            failures: s.consecutive_failures,
                            bridge: &self.config.bridge,
                            runtime: &self.config.runtime,
                            agent: &s.agent,
                            task: &s.task,
                        })?;
                        runtime_context.failure_signature=Some(crate::runtime::FailureSignatureRef{signature:signature.clone()});
                        runtime_context.available_recovery=cache.matching_recoveries(&s.context,&signature);
                        runtime_context.relevant_experience.recoveries=runtime_context.available_recovery.iter().map(|recovery|crate::development::ExperienceRef{kind:"recovery".into(),id:recovery.id.to_string(),revision:u64::from(recovery.version)}).collect();
                        drop(cache);
                        let prepared=store.prepare_runtime_decision_from_context(
                            &runtime_context,
                            self.config.runtime.policy_config(),
                        )?;
                        let runtime_record=&prepared.record;
                        let guidance=bridge_decision_from_runtime(&runtime_record.evaluation,self.config.runtime.mode);
                        events.push(("recovery_evaluated".into(),json!({"action_id":completed_action_id,"runtime_decision_id":runtime_record.id,"decision":runtime_record.decision.kind()})));
                        (json!({"accepted":true,"runtime_decision_id":runtime_record.id,"guidance":guidance}),Some(prepared))
                    } else {
                        (json!({"accepted":true}),None)
                    };
                    #[cfg(test)]
                    if self.fail_action_before_persistence.swap(false, Ordering::Relaxed) {
                        return Err(invalid("Injected action failure before persistence"));
                    }
                    self.persist_action_with_permit(
                        ActionPersistence {
                            session: Box::new(s.clone()),
                            expected_revision,
                            events,
                            trajectory: prepared_trajectory,
                            runtime_decision: prepared_runtime_decision,
                            role_knowledge_view: None,
                        },
                        action_permit.take().expect("action permit is available"),
                        ACTION_COMMIT_TIMEOUT,
                    )?;
                    Ok(response)
                });
                if result.is_err() {
                    self.reconcile_session(&session_id);
                }
                result
            }
            AgentEvent::RunCompleted(run) => {
                valid_id(&run.run_id)?;
                let session_id = run.hardknock_session_id.clone();
                let result = self.with_session(&session_id, |s| {
                    if let Some(existing) = s.runs.get(&run.run_id) {
                        return Ok(serde_json::to_value(existing)?);
                    }
                    if s.runs.len() >= 128 || s.ended {
                        return Err(invalid("Session ended or run budget exhausted"));
                    }
                    let record = RunRecord {
                        run_id: run.run_id,
                        experience_id: ExperienceId::new().to_string(),
                        status: "queued".into(),
                        outcome: None,
                        error: None,
                        action_start: s.next_action_start,
                        action_end: s.actions.len(),
                        duration_ms: run.duration_ms,
                        claimed_success: run.success,
                        termination: run.termination,
                    };
                    let expected_revision = s.revision;
                    let recording_clean_start = s.clean_start;
                    let mut snapshot = s.clone();
                    snapshot.runs.insert(record.run_id.clone(), record.clone());
                    snapshot.next_action_start = s.actions.len();
                    snapshot.clean_start = false;
                    snapshot.revision += 1;
                    self.enqueue_run_completion(
                        snapshot.clone(),
                        expected_revision,
                        recording_clean_start,
                        record.clone(),
                    )?;
                    *s = snapshot;
                    // Full final messages, metadata and transcripts are intentionally not retained.
                    Ok(serde_json::to_value(record)?)
                });
                if result.is_err() {
                    self.reconcile_session(&session_id);
                }
                result
            }
            AgentEvent::SessionEnded(end) => self.end(end),
            AgentEvent::LessonRejected(mut feedback) => {
                self.with_session(&feedback.hardknock_session_id.clone(), |s| {
                    if !s
                        .delivered
                        .iter()
                        .any(|l| l.lesson.id.to_string() == feedback.lesson_id)
                    {
                        return Err(invalid("Cannot reject an undelivered lesson"));
                    }
                    feedback.detail = feedback.detail.as_ref().map(|d| redact(d, 512));
                    s.rejections.insert(feedback.lesson_id.clone(), feedback);
                    s.revision += 1;
                    self.enqueue_session(s, "lesson_rejected", json!({}))?;
                    Ok(json!({"accepted":true}))
                })
            }
            AgentEvent::AgentMessage(message) => {
                self.with_session(&message.hardknock_session_id, |s| {
                    self.enqueue(
                        &s.id,
                        "agent_message",
                        json!({"summary":redact(&message.summary,512)}),
                    )?;
                    Ok(json!({"accepted":true}))
                })
            }
            AgentEvent::ExperimentRequested(request) => {
                let session =
                    self.with_session(&request.hardknock_session_id, |s| Ok(s.clone()))?;
                self.context_config(&session.agent.name)?;
                self.experiments.request(request, &session, &self.config)
            }
            AgentEvent::ExperimentProgress {
                hardknock_session_id,
                experiment_id,
                after,
            } => {
                self.with_session(&hardknock_session_id, |_| Ok(()))?;
                self.experiments
                    .poll(&hardknock_session_id, &experiment_id, after)
            }
            AgentEvent::ExperimentCancelled {
                hardknock_session_id,
                experiment_id,
            } => {
                self.with_session(&hardknock_session_id, |_| Ok(()))?;
                self.experiments
                    .cancel(&hardknock_session_id, &experiment_id)
            }
        }
    }
    fn session_snapshot(&self, id: &str) -> Result<Session> {
        if let Some(session) = self.sessions.lock().expect("session lock").get(id).cloned() {
            return Ok(session);
        }
        let mut session = persisted_bridge_session(&self.home, id)?
            .ok_or_else(|| invalid("Unknown Hardknock session"))?;
        for run in Store::open(&self.home)?.bridge_runs(id)? {
            session.runs.insert(run.run_id.clone(), run);
        }
        Ok(session)
    }
    fn inspect_session(&self, id: &str) -> Result<Value> {
        let session = self.session_snapshot(id)?;
        Ok(json!({
            "session": session_summary(&session),
            "runs": session.runs,
            "actions": session.actions.iter().rev().take(50).map(|action| json!({
                "action_id": action.action_id,
                "type": action_type(&action.action),
                "decision": action.decision,
                "completed": action.result.is_some(),
            })).collect::<Vec<_>>(),
        }))
    }
    fn run_status(&self, id: &str, run_id: &str) -> Result<Value> {
        let session = self.session_snapshot(id)?;
        Ok(serde_json::to_value(
            session
                .runs
                .get(run_id)
                .ok_or_else(|| invalid("Unknown run"))?,
        )?)
    }
    fn with_session<T>(&self, id: &str, f: impl FnOnce(&mut Session) -> Result<T>) -> Result<T> {
        let admissions = self.admissions.lock().expect("admission lock");
        if admissions.contains(id) {
            return Err(invalid("Bridge session lifecycle is already in progress"));
        }
        let mut sessions = self.sessions.lock().expect("session lock");
        let current = sessions
            .get_mut(id)
            .ok_or_else(|| invalid("Unknown Hardknock session"))?;
        let mut candidate = current.clone();
        let result = f(&mut candidate);
        if result.is_ok() {
            *current = candidate;
        }
        drop(sessions);
        drop(admissions);
        result
    }
    fn context_config(&self, agent: &str) -> Result<super::config::BridgeConfig> {
        let mut config = self.config.bridge.clone();
        config.experiment_budget.max_realities = if self.config.experiments.agent_requests.enabled {
            self.config.experiments.agent_requests.max_realities
        } else {
            0
        };
        if let Some(adapter) = self.config.integrations.get(agent) {
            if !adapter.enabled {
                return Err(invalid("Integration disabled in local configuration"));
            }
            config.max_context_lessons =
                config.max_context_lessons.min(adapter.max_context_lessons);
        }
        Ok(config)
    }
    fn development_response(
        &self,
        mut response: SessionStartResponse,
        context: &ExperienceContext,
        agent: &AgentIdentity,
    ) -> Result<SessionStartResponse> {
        if !self.config.development.bridge_context {
            return Ok(response);
        }
        let identity = crate::core::AgentIdentity {
            kind: agent.name.clone(),
            executable: agent.name.clone(),
            version: agent.version.clone(),
            model: agent.model.clone(),
        };
        let bundle = crate::development::context_bundle(
            &Store::open(&self.home)?,
            context,
            &identity,
            &self.config.development,
        )?;
        // Only bounded summaries/IDs cross the Bridge, never full Lessons or raw artifacts.
        let mut value = json!({"relevant":{"lessons":response.relevant_experience.iter().map(|b|&b.id).collect::<Vec<_>>(),"reflexes":bundle.relevant.reflexes,"recoveries":bundle.relevant.recoveries},"known_unknowns":bundle.known_unknowns.iter().take(8).map(|s|redact(s,256)).collect::<Vec<_>>(),"stale_items":bundle.stale_items,"contradictions":bundle.contradictions,"recommendations":bundle.recommendations.iter().take(3).map(|s|redact(s,256)).collect::<Vec<_>>(),"auto_run":false,"knowledge":bundle.knowledge});
        redact_value(&mut value);
        response.development_context = Some(value);
        if serde_json::to_vec(&response)?.len() > self.config.bridge.max_context_bytes {
            response.development_context = None;
        }
        Ok(response)
    }
    fn start_reserved(
        &self,
        mut start: SessionStarted,
        cwd: PathBuf,
        id: String,
        config: super::config::BridgeConfig,
    ) -> Result<Value> {
        let live_session = {
            self.sessions
                .lock()
                .expect("session lock")
                .get(&id)
                .cloned()
        };
        if let Some(mut session) = live_session {
            if session.cwd != cwd || session.agent != start.agent {
                return Err(invalid(
                    "Session identity/cwd changed; register a new external session id",
                ));
            }
            if session.ended {
                return Err(invalid(
                    "Session end is still being persisted; retry session start",
                ));
            }
            let expected_revision = session.revision;
            session.revision = session.revision.saturating_add(1);
            let response = self.development_response(
                context_response(&id, &session.delivered, &config),
                &session.context,
                &session.agent,
            )?;
            let session = self.admit_session(
                session,
                Some(expected_revision),
                vec![("session_resumed".into(), json!({"agent":start.agent.name}))],
            )?;
            self.sessions
                .lock()
                .expect("session lock")
                .insert(id.clone(), session);
            self.experiments.resume_session(&id);
            return Ok(serde_json::to_value(response)?);
        }

        if let Some(mut session) = persisted_bridge_session(&self.home, &id)? {
            if !session.ended {
                return Err(invalid("Session is already active in durable Bridge state"));
            }
            if session.cwd != cwd || session.agent != start.agent {
                return Err(invalid(
                    "Session identity/cwd changed; register a new external session id",
                ));
            }
            for run in Store::open(&self.home)?.bridge_runs(&id)? {
                session.runs.insert(run.run_id.clone(), run);
            }
            let expected_revision = session.revision;
            session.ended = false;
            session.trajectory_id = None;
            session.revision = session.revision.saturating_add(1);
            let response = self.development_response(
                context_response(&id, &session.delivered, &config),
                &session.context,
                &session.agent,
            )?;
            let session = self.admit_session(
                session,
                Some(expected_revision),
                vec![("session_resumed".into(), json!({"agent":start.agent.name}))],
            )?;
            self.sessions
                .lock()
                .expect("session lock")
                .insert(id.clone(), session);
            self.experiments.resume_session(&id);
            return Ok(serde_json::to_value(response)?);
        }

        start.task = start.task.as_ref().map(|task| redact(task, 512));
        let (starting_state, context, clean_start) =
            super::recording::capture_context(&cwd, &start.environment)?;
        let task = start
            .task
            .unwrap_or_else(|| "External agent task (summary unavailable)".into());
        let lessons = self
            .cache
            .read()
            .expect("cache lock")
            .retrieve(&context, &task, vec![]);
        let response = self.development_response(
            context_response(&id, &lessons, &config),
            &context,
            &start.agent,
        )?;
        let delivered = lessons
            .into_iter()
            .filter(|lesson| {
                response
                    .relevant_experience
                    .iter()
                    .any(|brief| brief.id == lesson.lesson.id.to_string())
            })
            .collect();
        let session = Session {
            id: id.clone(),
            external_id: start.session_id,
            agent: start.agent,
            cwd,
            reported_cwd: start.cwd,
            task,
            context,
            starting_state,
            clean_start,
            started_at: Utc::now(),
            ended: false,
            revision: 1,
            consecutive_failures: 0,
            actions: vec![],
            delivered,
            rejections: BTreeMap::new(),
            runs: BTreeMap::new(),
            next_action_start: 0,
            trajectory_id: None,
        };
        let session = self.admit_session(
            session,
            None,
            vec![
                ("session_started".into(), json!({})),
                (
                    "experience_injected".into(),
                    json!({"count":response.relevant_experience.len()}),
                ),
            ],
        )?;
        self.sessions
            .lock()
            .expect("session lock")
            .insert(id.clone(), session);
        self.experiments.resume_session(&id);
        Ok(serde_json::to_value(response)?)
    }
    fn start(&self, mut start: SessionStarted) -> Result<Value> {
        valid_id(&start.session_id)?;
        valid_id(&start.agent.name)?;
        valid_id(&start.agent.adapter_version)?;
        if !Path::new(&start.cwd).is_absolute() {
            return Err(invalid("Session cwd must be absolute"));
        }
        start.agent.version = start.agent.version.as_ref().map(|v| redact(v, 128));
        start.agent.model = start.agent.model.as_ref().map(|v| redact(v, 128));
        let cwd = Path::new(&start.cwd).canonicalize()?;
        if !cwd.is_dir() || cwd.starts_with(&self.home) || self.home.starts_with(&cwd) {
            return Err(invalid(
                "Workspace and Hardknock data must be separate directories",
            ));
        }
        let id = session_key(&start.agent.name, &start.session_id);
        let config = self.context_config(&start.agent.name)?;
        self.refresh()?;
        {
            let mut admissions = self.admissions.lock().expect("admission lock");
            if admissions.contains(&id) {
                return Err(invalid("Bridge session lifecycle is already in progress"));
            }
            let sessions = self.sessions.lock().expect("session lock");
            if !sessions.contains_key(&id)
                && active_session_count(&sessions).saturating_add(admissions.len())
                    >= self.config.bridge.max_sessions
            {
                return Err(invalid("Bridge session budget exhausted"));
            }
            admissions.insert(id.clone());
        }
        let result = self
            .persistence_barrier()
            .and_then(|()| self.start_reserved(start, cwd, id.clone(), config));
        self.admissions.lock().expect("admission lock").remove(&id);
        result
    }

    fn end(&self, end: SessionEnded) -> Result<Value> {
        let id = end.hardknock_session_id;
        {
            let mut admissions = self.admissions.lock().expect("admission lock");
            if admissions.contains(&id) {
                return Err(invalid("Bridge session lifecycle is already in progress"));
            }
            if !self
                .sessions
                .lock()
                .expect("session lock")
                .contains_key(&id)
            {
                return Err(invalid("Unknown Hardknock session"));
            }
            admissions.insert(id.clone());
        }
        let result = (|| {
            self.persistence_barrier()?;
            let current = self
                .sessions
                .lock()
                .expect("session lock")
                .get(&id)
                .cloned()
                .ok_or_else(|| invalid("Unknown Hardknock session"))?;
            let mut ended = current.clone();
            ended.ended = true;
            ended.revision = ended.revision.saturating_add(1);
            let trajectory = ended.trajectory_id.clone().map(|trajectory_id| {
                let outcome = if ended.consecutive_failures > 0 {
                    crate::predictive::TrajectoryOutcome::Failure(
                        crate::runtime::FailureSignatureRef {
                            signature: "session-ended-after-action-failure".into(),
                        },
                    )
                } else {
                    crate::predictive::TrajectoryOutcome::Success
                };
                (trajectory_id, outcome)
            });
            match self.persist_session_end(ended, current.revision, trajectory)? {
                SessionEndAcknowledgement::Committed => {
                    self.sessions.lock().expect("session lock").remove(&id);
                    self.experiments
                        .end_session(&id, self.config.experiments.continue_after_session_end);
                    Ok(json!({"accepted":true}))
                }
                SessionEndAcknowledgement::RolledBack { session, error } => {
                    self.sessions
                        .lock()
                        .expect("session lock")
                        .insert(id.clone(), *session);
                    Err(invalid(&error))
                }
                SessionEndAcknowledgement::InDoubt { error } => {
                    match persisted_bridge_session(&self.home, &id)? {
                        Some(session) if session.ended => {
                            self.sessions.lock().expect("session lock").remove(&id);
                            self.experiments.end_session(
                                &id,
                                self.config.experiments.continue_after_session_end,
                            );
                        }
                        Some(session) => {
                            self.sessions
                                .lock()
                                .expect("session lock")
                                .insert(id.clone(), session);
                        }
                        None => {
                            self.sessions
                                .lock()
                                .expect("session lock")
                                .insert(id.clone(), current);
                        }
                    }
                    Err(invalid(&error))
                }
            }
        })();
        self.admissions.lock().expect("admission lock").remove(&id);
        result
    }
}
fn config_for(bridge: &std::sync::Weak<Bridge>) -> super::config::BridgeConfig {
    bridge
        .upgrade()
        .map(|b| b.config.bridge.clone())
        .unwrap_or_default()
}
fn active_session_count(sessions: &HashMap<String, Session>) -> usize {
    sessions.values().filter(|session| !session.ended).count()
}
fn session_summary(s: &Session) -> Value {
    json!({"id":s.id,"agent":s.agent.name,"cwd":s.cwd,"actions":s.actions.len(),"started_at":s.started_at,"ended":s.ended})
}
fn action_type(action: &NormalizedAction) -> &'static str {
    match action {
        NormalizedAction::Shell { .. } => "shell",
        NormalizedAction::FileRead { .. } => "file_read",
        NormalizedAction::FileWrite { .. } => "file_write",
        NormalizedAction::FileDelete { .. } => "file_delete",
        NormalizedAction::ToolCall { .. } => "tool_call",
        NormalizedAction::Network { .. } => "network",
        NormalizedAction::Custom { .. } => "custom",
    }
}
fn validate_action(action: &NormalizedAction) -> Result<()> {
    if let NormalizedAction::Shell { command, cwd } = action
        && (command.trim().is_empty() || command.contains('\0') || !Path::new(cwd).is_absolute())
    {
        return Err(invalid(
            "Shell action requires nonempty command and absolute cwd",
        ));
    }
    Ok(())
}
fn sanitize_action(action: &mut NormalizedAction) -> Result<()> {
    let mut value = serde_json::to_value(&*action)?;
    redact_value(&mut value);
    *action = serde_json::from_value(value)?;
    Ok(())
}

fn normalize_cwd(action: &mut NormalizedAction, session: &Session) {
    if let NormalizedAction::Shell { cwd, .. } = action
        && *cwd == session.reported_cwd
    {
        *cwd = session.cwd.to_string_lossy().into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started_bridge(
        job_capacity: usize,
        external_id: &str,
    ) -> (
        tempfile::TempDir,
        PathBuf,
        Arc<Bridge>,
        JoinHandle<()>,
        String,
    ) {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        Store::open(&home).unwrap();
        let (bridge, worker) = Bridge::open_with_job_capacity(&home, job_capacity).unwrap();
        let id = bridge
            .handle(AgentEvent::SessionStarted(SessionStarted {
                session_id: external_id.into(),
                agent: AgentIdentity::new("queue-test"),
                cwd: repo.display().to_string(),
                repository: None,
                task: None,
                environment: Default::default(),
            }))
            .unwrap()["hardknock_session_id"]
            .as_str()
            .unwrap()
            .to_owned();
        bridge.flush().unwrap();
        (root, repo, bridge, worker, id)
    }

    fn block_writer(bridge: &Bridge) -> mpsc::Sender<()> {
        let (entered_sender, entered_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        bridge
            .try_send_job(
                JobKind::Block {
                    entered: entered_sender,
                    release: release_receiver,
                },
                "test writer block queue full",
            )
            .unwrap();
        entered_receiver.recv().unwrap();
        release_sender
    }

    fn fill_writer_queue(bridge: &Bridge) {
        bridge
            .try_send_job(
                JobKind::Persist {
                    session: None,
                    expected_revision: None,
                    id: "queue-pressure".into(),
                    events: vec![("queue-pressure".into(), json!({}))],
                    acknowledgement: None,
                },
                "test writer queue full",
            )
            .unwrap();
    }

    fn shell_action(repo: &Path) -> NormalizedAction {
        NormalizedAction::Shell {
            command: "printf bridge".into(),
            cwd: repo.display().to_string(),
        }
    }

    fn flush_after_release(bridge: &Bridge) {
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            match bridge.flush() {
                Ok(()) => return,
                Err(error)
                    if error.to_string().contains("queue full")
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::yield_now();
                }
                Err(error) => panic!("writer did not drain after release: {error}"),
            }
        }
    }

    #[test]
    fn full_writer_queue_never_admits_a_session() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        Store::open(&home).unwrap();
        let (bridge, worker) = Bridge::open_with_job_capacity(&home, 1).unwrap();
        let (entered_sender, entered_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        bridge
            .try_send_job(
                JobKind::Block {
                    entered: entered_sender,
                    release: release_receiver,
                },
                "test writer block queue full",
            )
            .unwrap();
        entered_receiver.recv().unwrap();
        bridge
            .try_send_job(
                JobKind::Persist {
                    session: None,
                    expected_revision: None,
                    id: "queue-pressure".into(),
                    events: vec![("queue-pressure".into(), json!({}))],
                    acknowledgement: None,
                },
                "test writer queue full",
            )
            .unwrap();

        let error = bridge
            .handle(AgentEvent::SessionStarted(SessionStarted {
                session_id: "queue-pressure".into(),
                agent: AgentIdentity::new("queue-test"),
                cwd: repo.display().to_string(),
                repository: None,
                task: None,
                environment: Default::default(),
            }))
            .unwrap_err();

        assert!(error.to_string().contains("queue full"));
        assert_eq!(bridge.handle(AgentEvent::Status).unwrap()["sessions"], 0);
        assert!(
            Store::open(&home)
                .unwrap()
                .trajectories()
                .unwrap()
                .is_empty()
        );

        release_sender.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            match bridge.flush() {
                Ok(()) => break,
                Err(error)
                    if error.to_string().contains("queue full")
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::yield_now();
                }
                Err(error) => panic!("writer did not drain after release: {error}"),
            }
        }
        drop(bridge);
        worker.join().unwrap();
    }

    #[test]
    fn action_proposal_enqueue_failure_rolls_back_live_session() {
        let (_root, repo, bridge, worker, id) = started_bridge(1, "proposal-rollback");
        let release = block_writer(&bridge);
        fill_writer_queue(&bridge);
        let action = shell_action(&repo);
        let store = Store::open(&bridge.home).unwrap();
        let trajectory_id = bridge.session_snapshot(&id).unwrap().trajectory_id.unwrap();
        let trajectory_events = store.trajectory_events(&trajectory_id).unwrap().len();
        let runtime_decisions = store.runtime_decisions().unwrap().len();
        drop(store);

        let error = bridge
            .handle(AgentEvent::ActionProposed(ActionProposed {
                hardknock_session_id: id.clone(),
                action_id: "proposal-1".into(),
                action: action.clone(),
                context: Default::default(),
            }))
            .unwrap_err();

        assert!(error.to_string().contains("queue full"));
        let session = bridge.session_snapshot(&id).unwrap();
        assert!(session.actions.is_empty());
        assert_eq!(session.revision, 1);
        let store = Store::open(&bridge.home).unwrap();
        assert_eq!(
            store.trajectory_events(&trajectory_id).unwrap().len(),
            trajectory_events
        );
        assert_eq!(store.runtime_decisions().unwrap().len(), runtime_decisions);
        drop(store);

        release.send(()).unwrap();
        flush_after_release(&bridge);
        bridge
            .handle(AgentEvent::ActionProposed(ActionProposed {
                hardknock_session_id: id.clone(),
                action_id: "proposal-1".into(),
                action,
                context: Default::default(),
            }))
            .unwrap();
        flush_after_release(&bridge);
        assert_eq!(bridge.session_snapshot(&id).unwrap().actions.len(), 1);
        let store = Store::open(&bridge.home).unwrap();
        assert_eq!(
            store.trajectory_events(&trajectory_id).unwrap().len(),
            trajectory_events + 1
        );
        assert_eq!(
            store.runtime_decisions().unwrap().len(),
            runtime_decisions + 1
        );

        drop(bridge);
        worker.join().unwrap();
    }

    #[test]
    fn action_failure_after_preparation_has_no_durable_side_effects() {
        let (_root, repo, bridge, worker, id) = started_bridge(2, "prepared-action-rollback");
        let trajectory_id = bridge.session_snapshot(&id).unwrap().trajectory_id.unwrap();
        let durable_before = persisted_bridge_session(&bridge.home, &id)
            .unwrap()
            .unwrap();
        bridge
            .fail_action_before_persistence
            .store(true, Ordering::Relaxed);

        let error = bridge
            .handle(AgentEvent::ActionProposed(ActionProposed {
                hardknock_session_id: id.clone(),
                action_id: "prepared-action".into(),
                action: shell_action(&repo),
                context: Default::default(),
            }))
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("Injected action failure before persistence")
        );
        assert!(bridge.session_snapshot(&id).unwrap().actions.is_empty());
        let durable_after = persisted_bridge_session(&bridge.home, &id)
            .unwrap()
            .unwrap();
        assert_eq!(durable_after.revision, durable_before.revision);
        assert_eq!(durable_after.actions.len(), durable_before.actions.len());
        let connection = bridge_read_connection(&bridge.home).unwrap();
        let trajectory_events: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM trajectory_events WHERE trajectory_id=?1",
                [trajectory_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let runtime_decisions: i64 = connection
            .query_row("SELECT COUNT(*) FROM runtime_decisions", [], |row| {
                row.get(0)
            })
            .unwrap();
        let action_events: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM bridge_events WHERE session_id=?1 AND kind='action_proposed'",
                [&id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(trajectory_events, 0);
        assert_eq!(runtime_decisions, 0);
        assert_eq!(action_events, 0);

        drop(bridge);
        worker.join().unwrap();
    }

    #[test]
    fn action_completion_enqueue_failure_rolls_back_result_and_failure_count() {
        let (_root, repo, bridge, worker, id) = started_bridge(1, "completion-rollback");
        let action = shell_action(&repo);
        bridge
            .handle(AgentEvent::ActionProposed(ActionProposed {
                hardknock_session_id: id.clone(),
                action_id: "completion-1".into(),
                action: action.clone(),
                context: Default::default(),
            }))
            .unwrap();
        flush_after_release(&bridge);
        let revision = bridge.session_snapshot(&id).unwrap().revision;
        let trajectory_id = bridge.session_snapshot(&id).unwrap().trajectory_id.unwrap();
        let store = Store::open(&bridge.home).unwrap();
        let trajectory_events = store.trajectory_events(&trajectory_id).unwrap().len();
        let runtime_decisions = store.runtime_decisions().unwrap().len();
        drop(store);
        let release = block_writer(&bridge);
        fill_writer_queue(&bridge);
        let completion = ActionCompleted {
            hardknock_session_id: id.clone(),
            action_id: "completion-1".into(),
            action,
            result: ActionResult {
                success: false,
                exit_code: Some(1),
                error_class: Some("forced-test-failure".into()),
                output_summary: None,
                artifacts: vec![],
            },
            duration_ms: 7,
        };

        let error = bridge
            .handle(AgentEvent::ActionCompleted(completion.clone()))
            .unwrap_err();

        assert!(error.to_string().contains("queue full"));
        let session = bridge.session_snapshot(&id).unwrap();
        assert_eq!(session.revision, revision);
        assert_eq!(session.consecutive_failures, 0);
        assert!(session.actions[0].result.is_none());
        let store = Store::open(&bridge.home).unwrap();
        assert_eq!(
            store.trajectory_events(&trajectory_id).unwrap().len(),
            trajectory_events
        );
        assert_eq!(store.runtime_decisions().unwrap().len(), runtime_decisions);
        drop(store);

        release.send(()).unwrap();
        flush_after_release(&bridge);
        bridge
            .handle(AgentEvent::ActionCompleted(completion))
            .unwrap();
        let session = bridge.session_snapshot(&id).unwrap();
        assert_eq!(session.consecutive_failures, 1);
        assert!(session.actions[0].result.is_some());
        flush_after_release(&bridge);
        assert_eq!(
            Store::open(&bridge.home)
                .unwrap()
                .trajectory_events(&trajectory_id)
                .unwrap()
                .len(),
            trajectory_events + 1
        );

        drop(bridge);
        worker.join().unwrap();
    }

    #[test]
    fn lesson_rejection_enqueue_failure_rolls_back_live_session() {
        let (_root, _repo, bridge, worker, id) = started_bridge(1, "rejection-rollback");
        let revision = bridge.session_snapshot(&id).unwrap().revision;
        let release = block_writer(&bridge);
        bridge
            .try_send_job(
                JobKind::Persist {
                    session: None,
                    expected_revision: None,
                    id: "queue-pressure".into(),
                    events: vec![("queue-pressure".into(), json!({}))],
                    acknowledgement: None,
                },
                "test writer queue full",
            )
            .unwrap();
        let lesson_id = crate::core::LessonId::new().to_string();

        let error = bridge
            .with_session(&id, |session| {
                session.rejections.insert(
                    lesson_id.clone(),
                    LessonFeedback {
                        hardknock_session_id: id.clone(),
                        lesson_id: lesson_id.clone(),
                        reason: RejectionReason::Other,
                        detail: Some("queue-pressure".into()),
                    },
                );
                session.revision = session.revision.saturating_add(1);
                bridge.enqueue_session(session, "lesson_rejected", json!({}))
            })
            .unwrap_err();

        assert!(error.to_string().contains("queue full"));
        let session = bridge.session_snapshot(&id).unwrap();
        assert_eq!(session.revision, revision);
        assert!(session.rejections.is_empty());

        release.send(()).unwrap();
        flush_after_release(&bridge);
        drop(bridge);
        worker.join().unwrap();
    }

    #[test]
    fn action_commit_timeout_is_bounded_and_live_state_waits_for_durable_commit() {
        let (_root, _repo, bridge, worker, id) = started_bridge(1, "action-commit-timeout");
        let release = block_writer(&bridge);
        let current = bridge.session_snapshot(&id).unwrap();
        let mut candidate = current.clone();
        candidate.task = "durable-only-after-release".into();
        candidate.revision = candidate.revision.saturating_add(1);

        let started = std::time::Instant::now();
        let error = bridge
            .persist_action_with_timeout(
                ActionPersistence {
                    session: Box::new(candidate.clone()),
                    expected_revision: current.revision,
                    events: vec![("action_timeout_test".into(), json!({}))],
                    trajectory: None,
                    runtime_decision: None,
                    role_knowledge_view: None,
                },
                Duration::from_millis(20),
            )
            .unwrap_err();

        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(
            error
                .to_string()
                .contains("action commit persistence acknowledgement timed out")
        );
        assert_eq!(bridge.session_snapshot(&id).unwrap().task, current.task);
        assert_eq!(
            persisted_bridge_session(&bridge.home, &id)
                .unwrap()
                .unwrap()
                .task,
            current.task
        );
        assert!(bridge.stopping.load(Ordering::Acquire));

        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while persisted_bridge_session(&bridge.home, &id)
            .unwrap()
            .is_some_and(|session| session.revision != candidate.revision)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "late action commit did not finish"
            );
            std::thread::yield_now();
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while bridge.session_snapshot(&id).unwrap().revision != candidate.revision {
            assert!(
                std::time::Instant::now() < deadline,
                "late durable action was not reconciled into live state"
            );
            std::thread::yield_now();
        }

        drop(bridge);
        worker.join().unwrap();
    }

    #[test]
    fn barrier_and_flush_propagate_sticky_writer_failure() {
        let (_root, _repo, bridge, worker, _id) = started_bridge(2, "sticky-writer-error");
        let connection = bridge_write_connection(&bridge.home).unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER fail_sticky_event
                 BEFORE INSERT ON bridge_events
                 WHEN NEW.kind='sticky_failure'
                 BEGIN
                   SELECT RAISE(ABORT, 'sticky writer failure');
                 END;",
            )
            .unwrap();
        bridge
            .enqueue("sticky-session", "sticky_failure", json!({}))
            .unwrap();

        let barrier = bridge.persistence_barrier().unwrap_err();
        assert!(
            barrier.to_string().contains("sticky writer failure"),
            "{barrier}"
        );
        let flush = bridge.flush().unwrap_err();
        assert!(
            flush.to_string().contains("sticky writer failure"),
            "{flush}"
        );

        drop(bridge);
        worker.join().unwrap();
    }

    #[test]
    fn lifecycle_barrier_has_a_bounded_deadline() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        Store::open(&home).unwrap();
        let (bridge, worker) = Bridge::open_with_job_capacity(&home, 1).unwrap();
        let release = block_writer(&bridge);

        let error = bridge
            .persistence_barrier_with_timeout(Duration::from_millis(20))
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("barrier persistence acknowledgement timed out")
        );
        assert!(bridge.stopping.load(Ordering::Acquire));

        release.send(()).unwrap();
        drop(bridge);
        worker.join().unwrap();
    }

    #[test]
    fn session_admission_has_a_bounded_deadline() {
        let (_root, _repo, bridge, worker, id) = started_bridge(1, "admission-timeout");
        let mut session = bridge.session_snapshot(&id).unwrap();
        let expected_revision = session.revision;
        session.revision = session.revision.saturating_add(1);
        let release = block_writer(&bridge);

        let error = bridge
            .admit_session_with_timeout(
                session,
                Some(expected_revision),
                vec![],
                Duration::from_millis(20),
            )
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("session admission persistence acknowledgement timed out")
        );
        assert!(bridge.stopping.load(Ordering::Acquire));

        release.send(()).unwrap();
        drop(bridge);
        worker.join().unwrap();
    }

    #[test]
    fn session_end_has_a_bounded_deadline() {
        let (_root, _repo, bridge, worker, id) = started_bridge(1, "end-timeout");
        let mut session = bridge.session_snapshot(&id).unwrap();
        let expected_revision = session.revision;
        session.ended = true;
        session.revision = session.revision.saturating_add(1);
        let trajectory = session
            .trajectory_id
            .clone()
            .map(|id| (id, TrajectoryOutcome::Success));
        let release = block_writer(&bridge);

        let error = match bridge.persist_session_end_with_timeout(
            session,
            expected_revision,
            trajectory,
            Duration::from_millis(20),
        ) {
            Ok(_) => panic!("session end unexpectedly received an acknowledgement"),
            Err(error) => error,
        };

        assert!(
            error
                .to_string()
                .contains("session end persistence acknowledgement timed out")
        );
        assert!(bridge.stopping.load(Ordering::Acquire));

        release.send(()).unwrap();
        drop(bridge);
        worker.join().unwrap();
    }
}
