// SPDX-License-Identifier: Apache-2.0
//! Bounded asynchronous experiment service, separate from the action/learning queue.
use super::{config::Config, engine::Session, protocol::ExperimentRequested};
use crate::{
    Error, Result,
    cancellation::Cancellation,
    core::{CurriculumId, ExperimentId},
    experimentation::*,
    store::{ExperimentStore, Store},
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, TryLockError,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// Bridge shutdown waits this long for experiment and curriculum cleanup before
/// detaching the worker. The worker retains its cancellation tokens and may
/// finish later, but the Bridge caller is never held indefinitely.
const EXPERIMENT_SHUTDOWN_POLL: Duration = Duration::from_millis(10);
const ENDED_SESSION_CAPACITY: usize = 1024;

struct Pending {
    session: String,
    cancel: Cancellation,
}
#[derive(Default)]
struct EndedSessions {
    members: HashSet<String>,
    order: VecDeque<String>,
}

impl EndedSessions {
    fn contains(&self, id: &str) -> bool {
        self.members.contains(id)
    }

    fn insert(&mut self, id: String) {
        if self.members.insert(id.clone()) {
            self.order.push_back(id);
        }
        while self.order.len() > ENDED_SESSION_CAPACITY {
            if let Some(evicted) = self.order.pop_front() {
                self.members.remove(&evicted);
            }
        }
    }

    fn remove(&mut self, id: &str) {
        if self.members.remove(id) {
            self.order.retain(|current| current != id);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.members.len()
    }
}

#[derive(Default)]
struct State {
    pending: HashMap<ExperimentId, Pending>,
    curricula: HashMap<CurriculumId, Pending>,
    ended: EndedSessions,
    sender: Option<SyncSender<Work>>,
}
enum Work {
    Experiment(ExperimentId),
    Curriculum(CurriculumId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExperimentShutdownOutcome {
    Complete,
    TimedOut,
    WorkerPanicked,
    WorkerExitedWithPending,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[must_use = "experiment shutdown failures and pending work must be observed"]
pub(crate) struct ExperimentShutdownReport {
    pub outcome: ExperimentShutdownOutcome,
    pub admission_closed: bool,
    pub state_observed: bool,
    pub cancelled_experiments: usize,
    pub cancelled_curricula: usize,
    pub pending_experiments: Vec<ExperimentId>,
    pub pending_curricula: Vec<CurriculumId>,
    pub waited: Duration,
}

impl ExperimentShutdownReport {
    pub fn completed(&self) -> bool {
        self.outcome == ExperimentShutdownOutcome::Complete
    }
}

struct WorkerState {
    handle: Option<JoinHandle<()>>,
    outcome: Option<ExperimentShutdownOutcome>,
}

pub struct ExperimentService {
    home: PathBuf,
    state: Arc<Mutex<State>>,
    accepting: AtomicBool,
    worker: Mutex<WorkerState>,
}

fn lock_until<'a, T>(mutex: &'a Mutex<T>, deadline: Instant) -> Option<MutexGuard<'a, T>> {
    loop {
        match mutex.try_lock() {
            Ok(guard) => return Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => return Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return None;
                }
                std::thread::sleep(EXPERIMENT_SHUTDOWN_POLL.min(remaining));
            }
        }
    }
}

impl ExperimentService {
    fn session_is_active(&self, session: &Session) -> Result<bool> {
        if session.ended || !self.accepting.load(Ordering::Acquire) {
            return Ok(false);
        }
        if self
            .state
            .lock()
            .expect("experiment service lock")
            .ended
            .contains(&session.id)
        {
            return Ok(false);
        }
        Ok(
            super::engine::persisted_bridge_session(&self.home, &session.id)?
                .is_some_and(|persisted| !persisted.ended),
        )
    }

    pub fn request_curriculum(
        &self,
        wire: super::protocol::CurriculumRequested,
        session: &Session,
        config: &Config,
    ) -> Result<Value> {
        use crate::{curriculum::*, store::CurriculumStore};
        if !config.curriculum.agent_requests || !self.session_is_active(session)? {
            return Err(Error::Intervention(
                "Agent curricula require explicit configuration and an active session".into(),
            ));
        }
        let store = Store::open(&self.home)?;
        let target = match &wire.target {
            super::protocol::CurriculumRequestTarget::Skill { skill } => {
                CurriculumTarget::Skill(store.skill(skill)?.id)
            }
            super::protocol::CurriculumRequestTarget::TaskFamily { task_family } => {
                CurriculumTarget::TaskFamily(store.task_family(task_family)?.id)
            }
        };
        if let Some(existing) = CurriculumStore::get(&store, &wire.request_id)? {
            if existing.session_id.as_deref() != Some(&session.id)
                || serde_json::to_value(&existing.target)? != serde_json::to_value(&target)?
                || existing.profile != wire.profile
                || existing.budget.max_curriculum_trials != Some(wire.budget.max_trials)
            {
                return Err(Error::InvalidInput(
                    "Curriculum request ID conflict or foreign session".into(),
                ));
            }
            return Ok(
                json!({"event":"curriculum_planned","curriculum_id":existing.id,"status":existing.status,"trials":existing.trials.len(),"budget":existing.budget}),
            );
        }
        let prior = CurriculumStore::list(
            &store,
            CurriculumQuery {
                session_id: Some(session.id.clone()),
            },
        )?;
        let reserved = prior
            .iter()
            .map(|c| c.budget.max_curriculum_trials.unwrap_or(0))
            .sum::<usize>();
        if reserved.saturating_add(wire.budget.max_trials)
            > config.curriculum.max_agent_session_trials
        {
            return Err(Error::Intervention("Cumulative session curriculum budget exceeded; planned, completed and cancelled reservations count".into()));
        }
        let context = inventory(&store, &target, &wire.profile, &config.curriculum)?;
        for skill in &context.skills {
            let source = store.experience(&skill.source_experience)?;
            if source.starting_state.repo_path != session.starting_state.repo_path
                || !matches!(
                    fixture_kind(&source),
                    Some(
                        crate::resilience::FixtureKind::SkillHardening
                            | crate::resilience::FixtureKind::SkillHardeningTransfer
                    )
                )
            {
                return Err(Error::Intervention("Agent curricula currently require a bundled hardening Skill in the requesting session repository; Git worktrees cannot sandbox arbitrary skill code".into()));
            }
            crate::curriculum::CurriculumExecutor {
                store: &store,
                config,
            }
            .verify_fixture(
                &crate::curriculum::skill_state(&store, skill)?,
                fixture_kind(&source)
                    .ok_or_else(|| Error::InvalidInput("Missing fixture kind".into()))?,
            )?;
            if source
                .replay
                .as_ref()
                .is_none_or(|r| r.script != "/bin/sh ./operation.sh")
                || source.evaluation.spec.checks != vec!["/bin/sh ./test.sh".to_string()]
            {
                return Err(Error::Intervention(
                    "Agent hardening requires the bundled procedure and evaluator".into(),
                ));
            }
        }
        let budget = config.curriculum.budget(wire.budget.max_trials)?;
        let mut c = DeterministicCurriculumPlanner.plan(&target, &context, &budget)?;
        c.id = wire.request_id;
        c.session_id = Some(session.id.clone());
        crate::curriculum::CurriculumExecutor {
            store: &store,
            config,
        }
        .validate(&c)?;
        // Agent plans must never acquire an unrelated contradiction context or opaque executor.
        for t in &c.trials {
            if let TrialExecution::Experiment { request } = &t.execution
                && (request.starting_state.state_ref.repo_path!=session.starting_state.repo_path || request.candidates.iter().any(|c|!matches!(&c.execution,crate::experimentation::CandidateExecution::Shell {commands} if commands==&vec!["/bin/sh ./operation.sh".to_string()])) || request.evaluator.checks!=vec!["/bin/sh ./test.sh".to_string()]) {return Err(Error::Intervention("Agent curriculum cannot execute unverified or cross-repository recipes".into()));}
        }
        if !self.session_is_active(session)? {
            return Err(Error::Intervention(
                "Session ended before curriculum planning completed".into(),
            ));
        }
        CurriculumStore::insert(&store, &c)?;
        store.bridge_event(
            &session.id,
            "curriculum_planned",
            &json!({"curriculum_id":c.id,"trials":c.trials.len()}),
        )?;
        Ok(
            json!({"event":"curriculum_planned","curriculum_id":c.id,"status":c.status,"trials":c.trials.len(),"budget":c.budget,"requires_start":true,"gaps":c.goals.iter().take(32).map(|g|json!({"kind":g.kind,"dimension":g.evidence_gap.dimension,"decision":g.decision})).collect::<Vec<_>>()}),
        )
    }
    pub fn start_curriculum(
        &self,
        session: &Session,
        id: &CurriculumId,
        config: &Config,
    ) -> Result<Value> {
        if !config.curriculum.agent_requests || !self.session_is_active(session)? {
            return Err(Error::Intervention(
                "Curriculum start requires an active enabled session".into(),
            ));
        }
        let store = Store::open(&self.home)?;
        let c = store.curriculum(id)?;
        if c.session_id.as_deref() != Some(&session.id) {
            return Err(Error::InvalidInput(
                "Curriculum belongs to another session".into(),
            ));
        }
        if !self.session_is_active(session)? {
            return Err(Error::Intervention(
                "Session ended before curriculum admission completed".into(),
            ));
        }
        let mut state = self.state.lock().expect("experiment service lock");
        if !self.accepting.load(Ordering::Acquire) || state.ended.contains(&session.id) {
            return Err(Error::Intervention(
                "Curriculum start requires an active enabled session".into(),
            ));
        }
        if !c.status.terminal() && !state.curricula.contains_key(id) {
            state.curricula.insert(
                id.clone(),
                Pending {
                    session: session.id.clone(),
                    cancel: Cancellation::default(),
                },
            );
            if !Self::enqueue_locked(&state, Work::Curriculum(id.clone())) {
                state.curricula.remove(id);
                return Err(Error::Intervention(
                    "Experiment/curriculum queue is full or stopping".into(),
                ));
            }
        }
        Ok(
            json!({"event":"curriculum_started","curriculum_id":id,"status":c.status,"queued":!c.status.terminal()}),
        )
    }
    pub fn poll_curriculum(&self, session: &str, id: &CurriculumId, after: u64) -> Result<Value> {
        let store = Store::open(&self.home)?;
        let c = store.curriculum(id)?;
        if c.session_id.as_deref() != Some(session) {
            return Err(Error::InvalidInput(
                "Curriculum belongs to another session".into(),
            ));
        }
        Ok(
            json!({"event":if c.status.terminal() {"curriculum_completed"} else {"curriculum_progress"},"curriculum_id":id,"status":c.status,"progress":store.curriculum_events(id,after)?.into_iter().take(16).collect::<Vec<_>>(),"result":compact_curriculum_result(&c)}),
        )
    }
    pub fn cancel_curriculum(&self, session: &str, id: &CurriculumId) -> Result<Value> {
        let store = Store::open(&self.home)?;
        if store.curriculum(id)?.session_id.as_deref() != Some(session) {
            return Err(Error::InvalidInput(
                "Curriculum belongs to another session".into(),
            ));
        }
        let requested = store.cancel_curriculum(id)?;
        if let Some(p) = self
            .state
            .lock()
            .expect("experiment service lock")
            .curricula
            .get(id)
        {
            p.cancel.cancel();
        }
        Ok(
            json!({"curriculum_id":id,"cancellation_requested":requested,"cleanup":"Poll for terminal confirmation"}),
        )
    }
    pub fn open(home: &Path, config: &Config) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Work>(16);
        let state = Arc::new(Mutex::new(State {
            sender: Some(sender),
            ..State::default()
        }));
        let shared = state.clone();
        let root = home.to_owned();
        let settings = config.clone();
        let worker = std::thread::Builder::new().name("hardknock-experiments".into()).spawn(move || {
            for work in receiver {
                if let Work::Curriculum(id)=work {
                    let cancel=shared.lock().expect("experiment service lock").curricula.get(&id).map(|p|p.cancel.clone()).unwrap_or_default();
                    let result=(||->Result<()> {
                        let store=Store::open(&root)?;
                        let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build()?;
                        let c=runtime.block_on(crate::curriculum::CurriculumExecutor {store:&store,config:&settings}.run(&id,&cancel))?;
                        store.bridge_event(c.session_id.as_deref().unwrap_or_default(),"curriculum_completed",&compact_curriculum_result(&c))?;Ok(())
                    })();
                    if let Err(error)=result {
                        tracing::error!(%error,%id,"Curriculum service failed");
                        if let Ok(store)=Store::open(&root) && let Ok(mut c)=store.curriculum(&id) && !c.status.terminal() {
                            c.status=crate::curriculum::CurriculumStatus::PartiallyCompleted;c.stop_reason=Some(error.to_string());c.revision+=1;c.updated_at=chrono::Utc::now();let _=crate::store::CurriculumStore::update(&store,&c);
                        }
                    }
                    shared.lock().expect("experiment service lock").curricula.remove(&id);
                    continue;
                }
                let Work::Experiment(id)=work else {unreachable!()};
                let cancel = shared.lock().expect("experiment service lock").pending.get(&id).map(|p| p.cancel.clone()).unwrap_or_default();
                let result = (|| -> Result<()> {
                    let store = Store::open(&root)?;
                    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
                    let experiment = runtime.block_on(ExperimentOrchestrator { store: &store, config: &settings }.execute(&id,&cancel))?;
                    let kind = terminal_event(experiment.status);
                    store.bridge_event(&experiment.request.session_id,kind,&json!({"experiment_id":id,"status":experiment.status,"result":experiment.result.as_ref().map(compact_result)}))?;
                    Ok(())
                })();
                if let Err(error) = result {
                    tracing::error!(%error,%id,"Experiment service failed");
                    if let Ok(store) = Store::open(&root)
                        && let Ok(mut experiment) = store.strategy_experiment(&id)
                        && !experiment.status.terminal() {
                        experiment.status = ExperimentStatus::Failed; experiment.failure = Some(error.to_string()); let _ = ExperimentStore::update_status(&store,&experiment);
                    }
                }
                shared.lock().expect("experiment service lock").pending.remove(&id);
            }
        })?;
        Ok(Self {
            home: home.to_owned(),
            state,
            accepting: AtomicBool::new(true),
            worker: Mutex::new(WorkerState {
                handle: Some(worker),
                outcome: None,
            }),
        })
    }

    pub fn request(
        &self,
        wire: ExperimentRequested,
        session: &Session,
        config: &Config,
    ) -> Result<Value> {
        if !self.session_is_active(session)? {
            return Err(Error::InvalidInput(
                "Session ended; experiment not started".into(),
            ));
        }
        let store = Store::open(&self.home)?;
        let existing = store.experiment_for_request(&wire.request_id)?;
        let request = ExperimentRequest {
            id: wire.request_id,
            session_id: session.id.clone(),
            question: wire.question,
            hypothesis: wire.hypothesis,
            candidates: wire.candidates,
            starting_state: existing
                .as_ref()
                .map(|e| e.request.starting_state.clone())
                .unwrap_or_else(|| ExperimentStartingState {
                    state_ref: session.starting_state.clone(),
                    expected_fingerprint: None,
                    parent_reality: None,
                    source: SnapshotSource::SessionCommitFallback,
                }),
            evaluator: wire.evaluator,
            budget: wire.budget,
            requested_by: crate::core::AgentIdentity {
                kind: session.agent.name.clone(),
                executable: "bridge-session".into(),
                version: session.agent.version.clone(),
                model: session.agent.model.clone(),
            },
            created_at: existing
                .as_ref()
                .map(|e| e.request.created_at)
                .unwrap_or_else(chrono::Utc::now),
            criteria: wire.criteria,
            origin: ExperimentOrigin::Agent,
            intent: wire.intent,
            capabilities: wire.capabilities,
        };
        let mut experiment = ExperimentOrchestrator {
            store: &store,
            config,
        }
        .submit(request)?;
        if !experiment.status.terminal() {
            // Session experiment spending is cumulative, preventing trivial repeated-budget bypass.
            let (spent, agent_runs) =
                store.session_experiment_reservations(&session.id, &experiment.id)?;
            let requested_agents = experiment
                .request
                .candidates
                .iter()
                .filter(|c| matches!(c.execution, CandidateExecution::AgentTask { .. }))
                .count();
            if spent.saturating_add(experiment.request.candidates.len())
                > config.experiments.agent_requests.max_realities
                || agent_runs.saturating_add(requested_agents)
                    > config.experience_budget.max_agent_runs
            {
                experiment.status = ExperimentStatus::Rejected;
                experiment.failure = Some("Agent session Reality budget exhausted (completed, cancelled and queued work count)".into());
                ExperimentStore::update_status(&store, &experiment)?;
            } else {
                if !self.session_is_active(session)? {
                    experiment.status = ExperimentStatus::Rejected;
                    experiment.failure =
                        Some("Session ended before experiment admission completed".into());
                    ExperimentStore::update_status(&store, &experiment)?;
                }
            }
            if !experiment.status.terminal() {
                let rejection = {
                    let mut state = self.state.lock().expect("experiment service lock");
                    if !self.accepting.load(Ordering::Acquire) || state.ended.contains(&session.id)
                    {
                        Some("Session ended before experiment admission completed")
                    } else if state.pending.contains_key(&experiment.id) {
                        None
                    } else {
                        state.pending.insert(
                            experiment.id.clone(),
                            Pending {
                                session: session.id.clone(),
                                cancel: Cancellation::default(),
                            },
                        );
                        if Self::enqueue_locked(&state, Work::Experiment(experiment.id.clone())) {
                            None
                        } else {
                            state.pending.remove(&experiment.id);
                            Some("Experiment queue is full or stopping")
                        }
                    }
                };
                if let Some(reason) = rejection {
                    experiment.status = ExperimentStatus::Rejected;
                    experiment.failure = Some(reason.into());
                    ExperimentStore::update_status(&store, &experiment)?;
                }
            }
        }
        let event = if experiment.status == ExperimentStatus::Rejected {
            "experiment_rejected"
        } else {
            "experiment_accepted"
        };
        store.bridge_event(
            &session.id,
            event,
            &json!({"experiment_id":experiment.id,"status":experiment.status}),
        )?;
        Ok(
            json!({"event":event,"experiment_id":experiment.id,"status":experiment.status,"budget":experiment.effective_budget,"reason":experiment.failure,"notices":experiment.notices}),
        )
    }

    pub fn poll(&self, session: &str, id: &ExperimentId, after: u64) -> Result<Value> {
        let store = Store::open(&self.home)?;
        let experiment = store.strategy_experiment(id)?;
        if experiment.request.session_id != session {
            return Err(Error::InvalidInput(
                "Experiment belongs to another session".into(),
            ));
        }
        let partial = store.candidate_results(id)?;
        Ok(
            json!({"event":terminal_event(experiment.status),"experiment_id":id,"status":experiment.status,"progress":store.experiment_progress(id,after)?,"result":experiment.result.as_ref().map(compact_result),"completed_candidates":partial.iter().map(|c|json!({"candidate_id":c.candidate_id,"name":c.name,"evaluation":c.evaluation.summary,"experience_id":c.experience_id})).collect::<Vec<_>>(),"reason":experiment.failure,"notices":experiment.notices}),
        )
    }

    pub fn cancel(&self, session: &str, id: &ExperimentId) -> Result<Value> {
        let store = Store::open(&self.home)?;
        if store.strategy_experiment(id)?.request.session_id != session {
            return Err(Error::InvalidInput(
                "Experiment belongs to another session".into(),
            ));
        }
        let requested = store.cancel_experiment(id)?;
        if let Some(pending) = self
            .state
            .lock()
            .expect("experiment service lock")
            .pending
            .get(id)
        {
            pending.cancel.cancel();
        }
        Ok(
            json!({"experiment_id":id,"cancellation_requested":requested,"cleanup":"Poll experiment_progress for terminal confirmation"}),
        )
    }

    pub fn end_session(&self, id: &str, continue_after_end: bool) {
        {
            let mut state = self.state.lock().expect("experiment service lock");
            state.ended.insert(id.into());
            if !continue_after_end {
                for pending in state.pending.values().filter(|p| p.session == id) {
                    pending.cancel.cancel();
                }
            }
            for pending in state.curricula.values().filter(|p| p.session == id) {
                pending.cancel.cancel();
            }
        }
        // Also retire persisted plans that were never enqueued. Leaving them
        // Planned would suppress equivalent evidence gathering in later sessions.
        let cancelled = (|| -> Result<()> {
            let store = Store::open(&self.home)?;
            for c in crate::store::CurriculumStore::list(
                &store,
                crate::curriculum::CurriculumQuery {
                    session_id: Some(id.into()),
                },
            )? {
                if !c.status.terminal() {
                    store.cancel_curriculum(&c.id)?;
                }
            }
            Ok(())
        })();
        if let Err(error) = cancelled {
            tracing::error!(%error, "Could not persist session curriculum cancellation");
        }
    }
    pub fn resume_session(&self, id: &str) {
        self.state
            .lock()
            .expect("experiment service lock")
            .ended
            .remove(id);
    }

    fn enqueue_locked(state: &State, work: Work) -> bool {
        state
            .sender
            .as_ref()
            .is_some_and(|sender| sender.try_send(work).is_ok())
    }

    #[cfg(test)]
    fn enqueue(&self, work: Work) -> bool {
        if !self.accepting.load(Ordering::Acquire) {
            return false;
        }
        let state = self.state.lock().expect("experiment service lock");
        self.accepting.load(Ordering::Acquire) && Self::enqueue_locked(&state, work)
    }

    pub(crate) fn request_shutdown(&self) {
        self.accepting.store(false, Ordering::Release);
        if let Ok(mut state) = self.state.try_lock() {
            state.sender.take();
            for pending in state.curricula.values() {
                pending.cancel.cancel();
            }
            for pending in state.pending.values() {
                pending.cancel.cancel();
            }
        }
    }

    /// Close admission, cancel all registered work, and wait at most two
    /// seconds for cooperative cleanup. A timeout leaves persisted work in its
    /// actual partial state and returns the IDs pending at the deadline; it
    /// never turns a timeout or worker panic into a successful shutdown.
    pub(crate) fn shutdown_with_timeout(&self, timeout: Duration) -> ExperimentShutdownReport {
        let started = Instant::now();
        let deadline = started.checked_add(timeout).unwrap_or(started);
        self.accepting.store(false, Ordering::Release);
        let (cancelled_experiments, cancelled_curricula) =
            if let Some(mut state) = lock_until(&self.state, deadline) {
                state.sender.take();
                for pending in state.pending.values() {
                    pending.cancel.cancel();
                }
                for pending in state.curricula.values() {
                    pending.cancel.cancel();
                }
                (state.pending.len(), state.curricula.len())
            } else {
                return ExperimentShutdownReport {
                    outcome: ExperimentShutdownOutcome::TimedOut,
                    admission_closed: true,
                    state_observed: false,
                    cancelled_experiments: 0,
                    cancelled_curricula: 0,
                    pending_experiments: Vec::new(),
                    pending_curricula: Vec::new(),
                    waited: started.elapsed(),
                };
            };

        let Some(mut worker) = lock_until(&self.worker, deadline) else {
            let pending = self.pending_work_until(deadline);
            return ExperimentShutdownReport {
                outcome: ExperimentShutdownOutcome::TimedOut,
                admission_closed: true,
                state_observed: pending.is_some(),
                cancelled_experiments,
                cancelled_curricula,
                pending_experiments: pending
                    .as_ref()
                    .map(|work| work.0.clone())
                    .unwrap_or_default(),
                pending_curricula: pending.map(|work| work.1).unwrap_or_default(),
                waited: started.elapsed(),
            };
        };

        let outcome = if let Some(handle) = worker.handle.as_ref() {
            while !handle.is_finished() {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                std::thread::sleep(EXPERIMENT_SHUTDOWN_POLL.min(remaining));
            }
            if handle.is_finished() {
                let handle = worker
                    .handle
                    .take()
                    .expect("finished experiment worker handle");
                if handle.join().is_err() {
                    ExperimentShutdownOutcome::WorkerPanicked
                } else {
                    ExperimentShutdownOutcome::Complete
                }
            } else {
                worker.handle.take();
                ExperimentShutdownOutcome::TimedOut
            }
        } else {
            worker
                .outcome
                .unwrap_or(ExperimentShutdownOutcome::WorkerExitedWithPending)
        };
        worker.outcome = Some(outcome);
        drop(worker);

        let pending = self.pending_work_until(deadline);
        let state_observed = pending.is_some();
        let (pending_experiments, pending_curricula) = pending.unwrap_or_default();
        let outcome = if !state_observed {
            ExperimentShutdownOutcome::TimedOut
        } else if outcome == ExperimentShutdownOutcome::Complete
            && (!pending_experiments.is_empty() || !pending_curricula.is_empty())
        {
            ExperimentShutdownOutcome::WorkerExitedWithPending
        } else {
            outcome
        };
        if let Ok(mut worker) = self.worker.try_lock() {
            worker.outcome = Some(outcome);
        }
        ExperimentShutdownReport {
            outcome,
            admission_closed: true,
            state_observed,
            cancelled_experiments,
            cancelled_curricula,
            pending_experiments,
            pending_curricula,
            waited: started.elapsed(),
        }
    }

    fn pending_work_until(
        &self,
        deadline: Instant,
    ) -> Option<(Vec<ExperimentId>, Vec<CurriculumId>)> {
        let state = lock_until(&self.state, deadline)?;
        let mut experiments = state.pending.keys().cloned().collect::<Vec<_>>();
        let mut curricula = state.curricula.keys().cloned().collect::<Vec<_>>();
        experiments.sort();
        curricula.sort();
        Some((experiments, curricula))
    }
}
impl Drop for ExperimentService {
    fn drop(&mut self) {
        self.request_shutdown();
        if let Ok(mut worker) = self.worker.try_lock()
            && let Some(handle) = worker.handle.take()
            && handle.is_finished()
        {
            let _ = handle.join();
        }
    }
}

fn terminal_event(status: ExperimentStatus) -> &'static str {
    match status {
        ExperimentStatus::Completed => "experiment_completed",
        ExperimentStatus::Cancelled => "experiment_cancelled",
        ExperimentStatus::Rejected | ExperimentStatus::Failed => "experiment_rejected",
        _ => "experiment_progress",
    }
}

fn compact_curriculum_result(c: &crate::curriculum::Curriculum) -> Value {
    let mut value = crate::cli::curriculum::report(c);
    if let Some(coverage) = value["coverage"].as_array_mut() {
        coverage.truncate(8);
        for p in coverage {
            for key in ["remaining_unknown", "recovery_gaps", "reflex_check_gaps"] {
                if let Some(values) = p[key].as_array_mut() {
                    values.truncate(16);
                }
            }
        }
    }
    for key in [
        "new_experiences",
        "new_lessons",
        "new_reflexes",
        "new_recoveries",
    ] {
        if let Some(values) = value[key].as_array_mut() {
            values.truncate(64);
        }
    }
    if let Some(reason) = c.stop_reason.as_deref() {
        value["stop_reason"] = json!(super::privacy::redact(reason, 512));
    }
    value["details"] =
        json!("Local curriculum show/report retains all evidence; Bridge lists are capped");
    value
}

/// Return evaluator evidence without transcripts, native prompts, or raw artifacts.
fn compact_result(result: &ExperimentResult) -> Value {
    json!({"experiment_id":result.experiment_id,"question":super::privacy::redact(&result.question,512),"quality":result.quality,"changed_variables":result.changed_variables,"starting_state":result.starting_state,"comparison":result.comparison,"recommendation":result.recommendation,"confidence":result.confidence,"created_experience":result.created_experience,"candidate_lessons":result.candidate_lessons,"usage":result.usage,"candidates":result.candidates.iter().map(|c|json!({"candidate_id":c.candidate_id,"name":c.name,"reality_id":c.reality_id,"experience_id":c.experience_id,"execution_status":c.execution_status,"evaluation":{"success":c.evaluation.success,"status":c.evaluation.status,"summary":c.evaluation.summary,"checks":c.evaluation.checks.iter().map(|check|json!({"name":check.name,"status":check.status})).collect::<Vec<_>>()},"diff_summary":c.diff_summary,"duration_ms":c.duration_ms,"starting_fingerprint":c.starting_fingerprint})).collect::<Vec<_>>()})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service_with(
        state: Arc<Mutex<State>>,
        sender: SyncSender<Work>,
        worker: JoinHandle<()>,
    ) -> ExperimentService {
        state.lock().unwrap().sender = Some(sender);
        ExperimentService {
            home: PathBuf::new(),
            state,
            accepting: AtomicBool::new(true),
            worker: Mutex::new(WorkerState {
                handle: Some(worker),
                outcome: None,
            }),
        }
    }

    #[test]
    fn shutdown_closes_admission_cancels_work_and_joins_completed_worker() {
        let state = Arc::new(Mutex::new(State::default()));
        let experiment_id = ExperimentId::new();
        let cancellation = Cancellation::default();
        state.lock().unwrap().pending.insert(
            experiment_id.clone(),
            Pending {
                session: "session-test".into(),
                cancel: cancellation.clone(),
            },
        );
        let (sender, receiver) = mpsc::sync_channel(1);
        sender
            .try_send(Work::Experiment(experiment_id.clone()))
            .unwrap();
        let shared = state.clone();
        let worker = std::thread::spawn(move || {
            for work in receiver {
                let Work::Experiment(id) = work else {
                    panic!("unexpected curriculum work");
                };
                let cancel = shared
                    .lock()
                    .unwrap()
                    .pending
                    .get(&id)
                    .unwrap()
                    .cancel
                    .clone();
                while !cancel.is_cancelled() {
                    std::thread::yield_now();
                }
                shared.lock().unwrap().pending.remove(&id);
            }
        });
        let service = service_with(state, sender, worker);

        let report = service.shutdown_with_timeout(Duration::from_secs(1));

        assert_eq!(report.outcome, ExperimentShutdownOutcome::Complete);
        assert!(report.admission_closed);
        assert!(report.state_observed);
        assert_eq!(report.cancelled_experiments, 1);
        assert_eq!(report.cancelled_curricula, 0);
        assert!(report.pending_experiments.is_empty());
        assert!(report.pending_curricula.is_empty());
        assert!(cancellation.is_cancelled());
        assert!(!service.enqueue(Work::Experiment(ExperimentId::new())));
    }

    #[test]
    fn shutdown_timeout_is_bounded_and_reports_pending_work() {
        let state = Arc::new(Mutex::new(State::default()));
        let curriculum_id = CurriculumId::new();
        let cancellation = Cancellation::default();
        state.lock().unwrap().curricula.insert(
            curriculum_id.clone(),
            Pending {
                session: "session-test".into(),
                cancel: cancellation.clone(),
            },
        );
        let (sender, receiver) = mpsc::sync_channel(1);
        let (release_sender, release_receiver) = mpsc::channel();
        let (finished_sender, finished_receiver) = mpsc::channel();
        let shared = state.clone();
        let worker_id = curriculum_id.clone();
        let worker = std::thread::spawn(move || {
            let _receiver = receiver;
            release_receiver.recv().unwrap();
            shared.lock().unwrap().curricula.remove(&worker_id);
            finished_sender.send(()).unwrap();
        });
        let service = service_with(state, sender, worker);

        let started = Instant::now();
        let report = service.shutdown_with_timeout(Duration::from_millis(25));

        assert_eq!(report.outcome, ExperimentShutdownOutcome::TimedOut);
        assert!(report.admission_closed);
        assert!(report.state_observed);
        assert_eq!(report.cancelled_curricula, 1);
        assert_eq!(report.pending_curricula, vec![curriculum_id]);
        assert!(report.waited >= Duration::from_millis(25));
        assert!(started.elapsed() < Duration::from_millis(250));
        assert!(cancellation.is_cancelled());

        release_sender.send(()).unwrap();
        finished_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        let repeated = service.shutdown_with_timeout(Duration::ZERO);
        assert_eq!(repeated.outcome, ExperimentShutdownOutcome::TimedOut);
        assert!(repeated.pending_curricula.is_empty());
    }

    #[test]
    fn shutdown_reports_worker_panic_without_clearing_partial_state() {
        let state = Arc::new(Mutex::new(State::default()));
        let experiment_id = ExperimentId::new();
        let cancellation = Cancellation::default();
        state.lock().unwrap().pending.insert(
            experiment_id.clone(),
            Pending {
                session: "session-test".into(),
                cancel: cancellation.clone(),
            },
        );
        let (sender, _receiver) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(|| panic!("worker failure"));
        let service = service_with(state, sender, worker);

        let report = service.shutdown_with_timeout(Duration::from_secs(1));

        assert_eq!(report.outcome, ExperimentShutdownOutcome::WorkerPanicked);
        assert_eq!(report.pending_experiments, vec![experiment_id]);
        assert!(cancellation.is_cancelled());
    }

    #[test]
    fn clean_worker_exit_with_pending_state_is_not_reported_as_success() {
        let state = Arc::new(Mutex::new(State::default()));
        let experiment_id = ExperimentId::new();
        state.lock().unwrap().pending.insert(
            experiment_id.clone(),
            Pending {
                session: "session-test".into(),
                cancel: Cancellation::default(),
            },
        );
        let (sender, _receiver) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(|| {});
        let service = service_with(state, sender, worker);

        let report = service.shutdown_with_timeout(Duration::from_secs(1));

        assert_eq!(
            report.outcome,
            ExperimentShutdownOutcome::WorkerExitedWithPending
        );
        assert_eq!(report.pending_experiments, vec![experiment_id]);
        assert!(!report.completed());
    }

    #[test]
    fn shutdown_deadline_includes_state_mutex_acquisition() {
        let state = Arc::new(Mutex::new(State::default()));
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || for _ in receiver {});
        let service = service_with(state.clone(), sender, worker);
        let (locked_sender, locked_receiver) = mpsc::sync_channel(1);
        let (release_sender, release_receiver) = mpsc::sync_channel(1);
        let lock_holder = std::thread::spawn(move || {
            let _guard = state.lock().unwrap();
            locked_sender.send(()).unwrap();
            release_receiver.recv().unwrap();
        });
        locked_receiver.recv().unwrap();

        let started = Instant::now();
        let report = service.shutdown_with_timeout(Duration::from_millis(25));

        assert_eq!(report.outcome, ExperimentShutdownOutcome::TimedOut);
        assert!(report.admission_closed);
        assert!(!report.state_observed);
        assert!(started.elapsed() < Duration::from_millis(250));
        release_sender.send(()).unwrap();
        lock_holder.join().unwrap();
    }

    #[test]
    fn ended_session_tombstones_evict_oldest_entries_at_a_fixed_cap() {
        let mut ended = EndedSessions::default();
        for index in 0..ENDED_SESSION_CAPACITY + 64 {
            ended.insert(format!("session-{index}"));
        }

        assert_eq!(ended.len(), ENDED_SESSION_CAPACITY);
        assert!(!ended.contains("session-0"));
        assert!(ended.contains(&format!("session-{}", ENDED_SESSION_CAPACITY + 63)));
        ended.remove(&format!("session-{}", ENDED_SESSION_CAPACITY + 63));
        assert_eq!(ended.len(), ENDED_SESSION_CAPACITY - 1);
    }
}
