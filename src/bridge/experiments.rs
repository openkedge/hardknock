// SPDX-License-Identifier: Apache-2.0
//! Bounded asynchronous experiment service, separate from the action/learning queue.
use super::{config::Config, engine::Session, protocol::ExperimentRequested};
use crate::{
    Error, Result,
    cancellation::Cancellation,
    core::ExperimentId,
    experimentation::*,
    store::{ExperimentStore, Store},
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        mpsc::{self, SyncSender},
    },
    thread::JoinHandle,
};

struct Pending {
    session: String,
    cancel: Cancellation,
}
#[derive(Default)]
struct State {
    pending: HashMap<ExperimentId, Pending>,
    ended: HashSet<String>,
}
pub struct ExperimentService {
    home: PathBuf,
    state: Arc<Mutex<State>>,
    sender: Option<SyncSender<ExperimentId>>,
    worker: Option<JoinHandle<()>>,
}

impl ExperimentService {
    pub fn open(home: &Path, config: &Config) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<ExperimentId>(16);
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        let root = home.to_owned();
        let settings = config.clone();
        let worker = std::thread::Builder::new().name("hardknock-experiments".into()).spawn(move || {
            for id in receiver {
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
            sender: Some(sender),
            worker: Some(worker),
        })
    }

    pub fn request(
        &self,
        wire: ExperimentRequested,
        session: &Session,
        config: &Config,
    ) -> Result<Value> {
        // Serialize admission and session-end cancellation, not candidate execution.
        let mut state = self.state.lock().expect("experiment service lock");
        if session.ended || state.ended.contains(&session.id) {
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
        if !experiment.status.terminal() && !state.pending.contains_key(&experiment.id) {
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
                state.pending.insert(
                    experiment.id.clone(),
                    Pending {
                        session: session.id.clone(),
                        cancel: Cancellation::default(),
                    },
                );
                if self
                    .sender
                    .as_ref()
                    .is_none_or(|sender| sender.try_send(experiment.id.clone()).is_err())
                {
                    state.pending.remove(&experiment.id);
                    experiment.status = ExperimentStatus::Rejected;
                    experiment.failure = Some("Experiment queue is full or stopping".into());
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
        let mut state = self.state.lock().expect("experiment service lock");
        state.ended.insert(id.into());
        if !continue_after_end {
            for pending in state.pending.values().filter(|p| p.session == id) {
                pending.cancel.cancel();
            }
        }
    }
    pub fn resume_session(&self, id: &str) {
        self.state
            .lock()
            .expect("experiment service lock")
            .ended
            .remove(id);
    }
    pub fn cancel_all(&self) {
        for pending in self
            .state
            .lock()
            .expect("experiment service lock")
            .pending
            .values()
        {
            pending.cancel.cancel();
        }
    }
}
impl Drop for ExperimentService {
    fn drop(&mut self) {
        self.cancel_all();
        self.sender.take();
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            tracing::error!("Experiment service worker panicked");
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

/// Return evaluator evidence without transcripts, native prompts, or raw artifacts.
fn compact_result(result: &ExperimentResult) -> Value {
    json!({"experiment_id":result.experiment_id,"question":super::privacy::redact(&result.question,512),"quality":result.quality,"changed_variables":result.changed_variables,"starting_state":result.starting_state,"comparison":result.comparison,"recommendation":result.recommendation,"confidence":result.confidence,"created_experience":result.created_experience,"candidate_lessons":result.candidate_lessons,"usage":result.usage,"candidates":result.candidates.iter().map(|c|json!({"candidate_id":c.candidate_id,"name":c.name,"reality_id":c.reality_id,"experience_id":c.experience_id,"execution_status":c.execution_status,"evaluation":{"success":c.evaluation.success,"status":c.evaluation.status,"summary":c.evaluation.summary,"checks":c.evaluation.checks.iter().map(|check|json!({"name":check.name,"status":check.status})).collect::<Vec<_>>()},"diff_summary":c.diff_summary,"duration_ms":c.duration_ms,"starting_fingerprint":c.starting_fingerprint})).collect::<Vec<_>>()})
}
