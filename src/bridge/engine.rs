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
    core::{ExperienceId, RuntimeDecisionId, StateRef},
    experience::ExperienceContext,
    retrieval::RetrievedLesson,
    store::{EffectStore, RuntimeStore, Store},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::JoinHandle,
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
}
pub struct Bridge {
    pub home: PathBuf,
    pub config: Config,
    pub cache: RwLock<ExperienceHotCache>,
    sessions: Mutex<HashMap<String, Session>>,
    jobs: SyncSender<Job>,
    pub stopping: AtomicBool,
    pub persistence_error: Mutex<Option<String>>,
    pub(crate) learning_cancel: crate::cancellation::Cancellation,
    pub(crate) experiments: super::experiments::ExperimentService,
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.learning_cancel.cancel();
        self.experiments.cancel_all();
    }
}
enum Job {
    Persist {
        id: String,
        kind: String,
        data: Value,
    },
    Complete(Box<Session>, RunRecord),
    RuntimeDecision(
        Box<crate::runtime::RuntimeDecisionRecord>,
        crate::runtime::RuntimePolicyConfig,
    ),
    Flush(mpsc::Sender<()>),
}

pub fn session_key(agent: &str, external: &str) -> String {
    format!(
        "hk-s-{}",
        &blake3::hash(format!("{agent}\0{external}").as_bytes()).to_hex()[..32]
    )
}
fn invalid(message: &str) -> Error {
    Error::InvalidInput(message.into())
}
fn valid_id(s: &str) -> Result<()> {
    if s.is_empty() || s.len() > 256 || s.chars().any(char::is_control) {
        return Err(invalid(
            "Identifier must be 1–256 bytes without control characters",
        ));
    }
    Ok(())
}
impl Bridge {
    pub fn open(home: &Path) -> Result<(Arc<Self>, JoinHandle<()>)> {
        let store = Store::open(home)?;
        let config = Config::load(home)?;
        let cache = ExperienceHotCache::load(&store)?;
        let mut sessions = HashMap::new();
        for mut session in store.bridge_sessions()? {
            for run in store.bridge_runs(&session.id)? {
                session.runs.insert(run.run_id.clone(), run);
            }
            // An unacknowledged in-flight run is never silently presented as success after a crash.
            for run in session.runs.values_mut().filter(|r| r.status == "queued") {
                run.status = "interrupted".into();
                run.error =
                    Some("Bridge restarted before completion; inspect retained evidence".into());
            }
            sessions.insert(session.id.clone(), session);
        }
        let (tx, rx) = mpsc::sync_channel(4096);
        let learning_cancel = crate::cancellation::Cancellation::default();
        let bridge = Arc::new(Self {
            experiments: super::experiments::ExperimentService::open(&store.home, &config)?,
            home: store.home.clone(),
            config,
            cache: RwLock::new(cache),
            sessions: Mutex::new(sessions),
            jobs: tx,
            stopping: AtomicBool::new(false),
            persistence_error: Mutex::new(None),
            learning_cancel: learning_cancel.clone(),
        });
        let weak = Arc::downgrade(&bridge);
        let worker = std::thread::spawn(move || {
            for job in rx {
                let result: Result<()> = (|| match job {
                    Job::Flush(tx) => {
                        let _ = tx.send(());
                        Ok(())
                    }
                    Job::Persist { id, kind, data } => {
                        if let Some(bridge) = weak.upgrade() {
                            let snapshot = bridge
                                .sessions
                                .lock()
                                .expect("session lock")
                                .get(&id)
                                .cloned();
                            if let Some(session) = snapshot {
                                store.save_bridge_session(&session)?;
                                if kind == "lesson_rejected" {
                                    for feedback in session.rejections.values() {
                                        let reason = serde_json::to_value(&feedback.reason)?;
                                        store.bridge_feedback(
                                            &id,
                                            &session.agent.name,
                                            &feedback.lesson_id.parse()?,
                                            reason.as_str().unwrap_or("other"),
                                        )?;
                                    }
                                }
                            }
                        }
                        store.bridge_event(&id, &kind, &data)
                    }
                    Job::RuntimeDecision(record, config) => {
                        store.persist_runtime_decision(&record, config)
                    }
                    Job::Complete(snapshot, mut run) => {
                        store.save_bridge_session(&snapshot)?;
                        store.bridge_event(
                            &snapshot.id,
                            "run_completed",
                            &json!({"run_id":run.run_id,"claimed_success":run.claimed_success}),
                        )?;
                        let completed = super::recording::record(
                            &store,
                            &snapshot,
                            &run,
                            &config_for(&weak),
                            &learning_cancel,
                        );
                        match completed {
                            Ok(exp) => {
                                run.status = "completed".into();
                                run.outcome = Some(
                                    serde_json::to_value(exp.outcome)?
                                        .as_str()
                                        .unwrap_or("inconclusive")
                                        .into(),
                                );
                            }
                            Err(error) => {
                                run.status = "failed".into();
                                run.error = Some(redact(&error.to_string(), 512));
                            }
                        }
                        store.save_bridge_run(&snapshot.id, &run)?;
                        store.bridge_event(&snapshot.id, if run.status == "completed" { "experience_created" } else { "recording_failed" }, &json!({"run_id":run.run_id,"experience_id":run.experience_id,"outcome":run.outcome,"error":run.error}))?;
                        if let Some(bridge) = weak.upgrade() {
                            let saved = {
                                let mut sessions = bridge.sessions.lock().expect("session lock");
                                sessions.get_mut(&snapshot.id).map(|session| {
                                    session.runs.insert(run.run_id.clone(), run);
                                    session.revision += 1;
                                    session.clone()
                                })
                            };
                            if let Some(session) = saved {
                                store.save_bridge_session(&session)?;
                            }
                            if let Ok(cache) = ExperienceHotCache::load(&store) {
                                *bridge.cache.write().expect("cache lock") = cache;
                            }
                        }
                        Ok(())
                    }
                })();
                if let Err(error) = result
                    && let Some(bridge) = weak.upgrade()
                {
                    *bridge.persistence_error.lock().expect("error lock") =
                        Some(redact(&error.to_string(), 512));
                }
            }
        });
        Ok((bridge, worker))
    }
    pub fn flush(&self) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        self.jobs
            .send(Job::Flush(tx))
            .map_err(|_| invalid("Bridge writer unavailable"))?;
        rx.recv_timeout(std::time::Duration::from_secs(60))
            .map_err(|_| invalid("Bridge flush timed out"))?;
        if let Some(error) = &*self.persistence_error.lock().expect("error lock") {
            return Err(invalid(error));
        }
        Ok(())
    }
    pub fn refresh(&self) -> Result<()> {
        let cache = ExperienceHotCache::load(&Store::open(&self.home)?)?;
        *self.cache.write().expect("cache lock") = cache;
        Ok(())
    }
    fn enqueue(&self, id: &str, kind: &str, data: Value) -> Result<()> {
        self.jobs
            .try_send(Job::Persist {
                id: id.into(),
                kind: kind.into(),
                data,
            })
            .map_err(|_| {
                invalid(
                    "Bridge persistence queue full or unavailable; observation not acknowledged",
                )
            })
    }
    fn enqueue_runtime_decision(
        &self,
        record: crate::runtime::RuntimeDecisionRecord,
    ) -> Result<()> {
        self.jobs
            .try_send(Job::RuntimeDecision(
                Box::new(record),
                self.config.runtime.policy_config(),
            ))
            .map_err(|_| {
                invalid(
                    "Bridge persistence queue full or unavailable; runtime decision not acknowledged",
                )
            })
    }
    pub fn handle(&self, event: AgentEvent) -> Result<Value> {
        if self.stopping.load(Ordering::Relaxed) {
            return Err(invalid("Bridge stopping"));
        }
        match event {
            AgentEvent::CurriculumRequested(request)=>{
                let session=self.with_session(&request.hardknock_session_id,|s|Ok(s.clone()))?;self.context_config(&session.agent.name)?;
                self.experiments.request_curriculum(request,&session,&self.config)
            },
            AgentEvent::CurriculumStarted {hardknock_session_id,curriculum_id}=>{
                let session=self.with_session(&hardknock_session_id,|s|Ok(s.clone()))?;self.context_config(&session.agent.name)?;
                self.experiments.start_curriculum(&session,&curriculum_id,&self.config)
            },
            AgentEvent::CurriculumProgress {hardknock_session_id,curriculum_id,after}=>{
                self.with_session(&hardknock_session_id,|_|Ok(()))?;self.experiments.poll_curriculum(&hardknock_session_id,&curriculum_id,after)
            },
            AgentEvent::CurriculumCancelled {hardknock_session_id,curriculum_id}=>{
                self.with_session(&hardknock_session_id,|_|Ok(()))?;self.experiments.cancel_curriculum(&hardknock_session_id,&curriculum_id)
            },
            AgentEvent::SkillPackageRequested {hardknock_session_id,skill,profile}=>{
                let session=self.with_session(&hardknock_session_id,|s|Ok(s.clone()))?;
                let store=Store::open(&self.home)?;let s=store.skill(&skill)?;
                if store.experience(&s.source_experience)?.starting_state.repo_path!=session.starting_state.repo_path {return Err(invalid("Skill belongs to another repository"));}
                let p=crate::curriculum::skill_package(&store,&skill,&profile,&self.config.curriculum)?;
                Ok(json!({"skill":p.skill,"maturity":p.maturity,"profile_coverage":{"profile":p.coverage.profile,"tested_conditions":p.coverage.tested_conditions,"configured_conditions":p.coverage.configured_conditions,"profile_coverage":p.coverage.profile_coverage,"dimensions":p.coverage.dimensions.iter().take(32).map(|d|json!({"name":d.name,"unknown":d.unknown.iter().take(16).collect::<Vec<_>>(),"latest_observations":d.tested.iter().rev().take(3).collect::<Vec<_>>()})).collect::<Vec<_>>()},"lessons":p.lessons.iter().take(32).collect::<Vec<_>>(),"reflexes":p.reflexes.iter().take(32).collect::<Vec<_>>(),"recoveries":p.recoveries.iter().take(32).collect::<Vec<_>>(),"provenance":"Inspect local skill package for complete versioned evidence"}))
            },
            AgentEvent::SessionStarted(start) => self.start(start),
            AgentEvent::Status => {
                let sessions = self.sessions.lock().expect("session lock");
                let agents: std::collections::BTreeSet<_> = sessions.values().filter(|s| !s.ended).map(|s| s.agent.name.clone()).collect();
                Ok(json!({"status":"running","protocol":PROTOCOL_VERSION,"sessions":sessions.values().filter(|s| !s.ended).count(),"adapters":agents,"persistence_error":*self.persistence_error.lock().expect("error lock")}))
            }
            AgentEvent::Sessions => Ok(json!({"sessions":self.sessions.lock().expect("session lock").values().map(session_summary).collect::<Vec<_>>()})),
            AgentEvent::Inspect { hardknock_session_id: id } => self.with_session(&id, |s| Ok(json!({"session":session_summary(s),"runs":s.runs,"actions":s.actions.iter().rev().take(50).map(|a|json!({"action_id":a.action_id,"type":action_type(&a.action),"decision":a.decision,"completed":a.result.is_some()})).collect::<Vec<_>>()}))),
            AgentEvent::RunStatus { hardknock_session_id: id, run_id } => self.with_session(&id, |s| Ok(serde_json::to_value(s.runs.get(&run_id).ok_or_else(|| invalid("Unknown run"))?)?)),
            AgentEvent::Events { after } => Store::open(&self.home)?.bridge_events(after),
            AgentEvent::RefreshCache => { self.refresh()?; Ok(json!({"refreshed":true})) },
            AgentEvent::Shutdown => { self.stopping.store(true, Ordering::Relaxed); Ok(json!({"stopping":true})) },
            AgentEvent::ContextRequested(request) => {
                self.refresh()?;
                let cwd = self.with_session(&request.hardknock_session_id, |s| Ok(s.cwd.clone()))?;
                let (starting_state, context, clean_start) = super::recording::capture_context(&cwd, &EnvironmentSummary::default())?;
                let (response,context,agent)=self.with_session(&request.hardknock_session_id, |s| {
                    if s.actions.len() == s.next_action_start {
                        s.starting_state = starting_state; s.context = context; s.clean_start = clean_start;
                    }
                    if let Some(task) = request.task { s.task = redact(&task,512); }
                    let lessons = self.cache.read().expect("cache lock").retrieve(&s.context, &s.task, vec![]);
                    let response = context_response(&s.id, &lessons, &self.context_config(&s.agent.name)?);
                    s.delivered = lessons.into_iter().filter(|l|response.relevant_experience.iter().any(|b|b.id == l.lesson.id.to_string())).collect();
                    s.revision += 1;
                    self.enqueue(&s.id,"experience_injected",json!({"count":s.delivered.len()}))?;
                    Ok((response,s.context.clone(),s.agent.clone()))
                })?;
                Ok(serde_json::to_value(self.development_response(response,&context,&agent)?)?)
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
            AgentEvent::RealityEffectProposed { reality_id, mut request } => {
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
            AgentEvent::RealityEffectStatus { reality_id, effect_id } => {
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
            AgentEvent::RealityEffectDiscardRequested { reality_id, effect_id } => {
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
            AgentEvent::EffectCommitRequested { hardknock_session_id, effect_id } => {
                let agent = self.with_session(&hardknock_session_id, |session| Ok(session.agent.name.clone()))?;
                let store = Store::open(&self.home)?;
                let manager = crate::effects::EffectManager::new(&store)?;
                match manager.commit(&effect_id,None,&crate::effects::EffectManager::agent_context(&agent)) {
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
            AgentEvent::EffectDiscardRequested { hardknock_session_id, effect_id } => {
                let agent = self.with_session(&hardknock_session_id, |session| Ok(session.agent.name.clone()))?;
                let store = Store::open(&self.home)?;
                let effect = crate::effects::EffectManager::new(&store)?.discard(
                    &effect_id,
                    &crate::effects::EffectManager::agent_context(&agent),
                )?;
                self.enqueue(&hardknock_session_id,"effect_discarded",json!({"effect_id":effect_id}))?;
                Ok(json!({"effect":effect,"committed":false}))
            }
            AgentEvent::EffectStatus { hardknock_session_id, effect_id } => {
                self.with_session(&hardknock_session_id, |_| Ok(()))?;
                let store = Store::open(&self.home)?;
                Ok(json!({
                    "effect":store.effect(&effect_id)?,
                    "prepared":store.prepared_effect(&effect_id).ok(),
                    "receipt":store.commit_receipt_for_effect(&effect_id)?,
                    "events":store.effect_events(&effect_id)?
                }))
            }
            AgentEvent::EffectReconcileRequested { hardknock_session_id, effect_id } => {
                self.with_session(&hardknock_session_id, |_| Ok(()))?;
                let store = Store::open(&self.home)?;
                let result = crate::effects::EffectManager::new(&store)?.reconcile(&effect_id)?;
                self.enqueue(&hardknock_session_id,"effect_reconciled",json!({"effect_id":effect_id,"result":result}))?;
                Ok(json!({"effect_id":effect_id,"result":result}))
            }
            AgentEvent::ActionProposed(mut proposed) => {
                valid_id(&proposed.action_id)?; validate_action(&proposed.action)?;
                self.with_session(&proposed.hardknock_session_id.clone(), |s| {
                    if s.ended { return Err(invalid("Session has ended")); }
                    normalize_cwd(&mut proposed.action, s);
                    sanitize_action(&mut proposed.action)?;
                    if let Some(existing) = s.actions.iter().find(|a|a.action_id == proposed.action_id) {
                        if existing.action != proposed.action { return Err(invalid("Action id reused with different action")); }
                        return Ok(serde_json::to_value(&existing.decision)?);
                    }
                    if s.actions.len() >= self.config.bridge.max_actions { return Err(invalid("Session action budget exhausted")); }
                    let (runtime_context,runtime_evaluation)=self.cache.read().expect("cache lock").evaluate_runtime(RuntimeEvaluationRequest {
                        context: &s.context,
                        proposed: &proposed,
                        failures: s.consecutive_failures,
                        bridge: &self.config.bridge,
                        runtime: &self.config.runtime,
                        agent: &s.agent,
                        task: &s.task,
                    })?;
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
                    self.enqueue_runtime_decision(runtime_record.clone())?;
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
                    self.enqueue(&s.id,"action_proposed",json!({"action_id":proposed.action_id,"decision":decision,"runtime_decision_id":runtime_record.id}))?;
                    if matches!(decision,ActionDecision::Warn{..}|ActionDecision::Replan{..}) { self.enqueue(&s.id,"reflex_matched",json!({"action_id":proposed.action_id}))?; }
                    Ok(serde_json::to_value(decision)?)
                })
            }
            AgentEvent::RuntimeDecisionRequested(request) => self.handle(
                AgentEvent::ActionProposed(ActionProposed {
                    hardknock_session_id: request.hardknock_session_id,
                    action_id: request.action_id,
                    action: request.action,
                    context: request.context,
                }),
            ),
            AgentEvent::RuntimeDecisionMade { hardknock_session_id, decision_id } => {
                self.with_session(&hardknock_session_id, |_| Ok(()))?;
                let store=Store::open(&self.home)?;
                let record=store.runtime_decision(&decision_id)?;
                if record.session_id != crate::core::HardknockSessionId::from_external(&hardknock_session_id) {
                    return Err(invalid("Runtime decision belongs to a different session"));
                }
                Ok(serde_json::to_value(record)?)
            }
            AgentEvent::RuntimeDecisionFeedback(report) => {
                self.with_session(&report.hardknock_session_id, |_| Ok(()))?;
                let store=Store::open(&self.home)?;
                let record=store.runtime_decision(&report.feedback.decision_id)?;
                if record.session_id != crate::core::HardknockSessionId::from_external(&report.hardknock_session_id) {
                    return Err(invalid("Runtime feedback belongs to a different session"));
                }
                store.record_runtime_feedback(&report.feedback)?;
                Ok(json!({"accepted":true,"decision_id":record.id}))
            }
            AgentEvent::ActionCompleted(mut completed) => {
                valid_id(&completed.action_id)?; validate_action(&completed.action)?;
                if completed.result.success && completed.result.exit_code.is_some_and(|c| c != 0) { return Err(invalid("Success conflicts with exit code")); }
                sanitize_action(&mut completed.action)?;
                if let Some(output) = &mut completed.result.output_summary { *output = redact(output,MAX_OUTPUT_BYTES); }
                if let Some(class) = &mut completed.result.error_class { *class = redact(class,128); }
                // References are metadata only: never open adapter-supplied paths.
                completed.result.artifacts.truncate(16);
                for a in &mut completed.result.artifacts { a.uri = redact(&a.uri,512); a.description = a.description.as_ref().map(|s|redact(s,256)); }
                self.with_session(&completed.hardknock_session_id, |s| {
                    normalize_cwd(&mut completed.action, s);
                    let action = s.actions.iter_mut().find(|a|a.action_id == completed.action_id).ok_or_else(||invalid("Completion has no corresponding action proposal"))?;
                    if action.action != completed.action { return Err(invalid("Completion action differs from proposal")); }
                    if let Some(result) = &action.result { if result != &completed.result { return Err(invalid("Conflicting duplicate action result")); } return Ok(json!({"accepted":true,"duplicate":true})); }
                    action.result = Some(completed.result); action.duration_ms = completed.duration_ms;
                    s.consecutive_failures = if action.result.as_ref().is_some_and(|r|r.success) { 0 } else { s.consecutive_failures.saturating_add(1) };
                    let failed_signature=action.result.as_ref().filter(|result|!result.success).and_then(|result|result.error_class.clone());
                    let completed_action=action.action.clone();
                    let completed_action_id=action.action_id.clone();
                    s.revision += 1;
                    self.enqueue(&s.id,"action_completed",json!({"action_id":action.action_id,"success":action.result.as_ref().map(|r|r.success)}))?;
                    if let Some(signature)=failed_signature {
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
                        let runtime_record=Store::open(&self.home)?.record_runtime_decision(&runtime_context,self.config.runtime.policy_config())?;
                        let guidance=bridge_decision_from_runtime(&runtime_record.evaluation,self.config.runtime.mode);
                        self.enqueue(&s.id,"recovery_evaluated",json!({"action_id":completed_action_id,"runtime_decision_id":runtime_record.id,"decision":runtime_record.decision.kind()}))?;
                        return Ok(json!({"accepted":true,"runtime_decision_id":runtime_record.id,"guidance":guidance}));
                    }
                    Ok(json!({"accepted":true}))
                })
            }
            AgentEvent::RunCompleted(run) => {
                valid_id(&run.run_id)?;
                self.with_session(&run.hardknock_session_id, |s| {
                    if let Some(existing) = s.runs.get(&run.run_id) { return Ok(serde_json::to_value(existing)?); }
                    if s.runs.len() >= 128 || s.ended { return Err(invalid("Session ended or run budget exhausted")); }
                    let record = RunRecord { run_id: run.run_id, experience_id: ExperienceId::new().to_string(), status: "queued".into(), outcome: None, error: None,
                        action_start: s.next_action_start, action_end:s.actions.len(), duration_ms:run.duration_ms, claimed_success:run.success, termination:run.termination };
                    let mut snapshot = s.clone();
                    snapshot.runs.insert(record.run_id.clone(), record.clone()); snapshot.next_action_start = s.actions.len(); snapshot.revision += 1;
                    // Enqueue before committing state: a full queue must not strand a run forever.
                    self.jobs.try_send(Job::Complete(Box::new(snapshot.clone()),record.clone())).map_err(|_|invalid("Learning queue full"))?;
                    *s = snapshot;
                    // Only a new context request can establish a clean baseline for the next run.
                    s.clean_start = false;
                    // Full final messages, metadata and transcripts are intentionally not retained.
                    Ok(serde_json::to_value(record)?)
                })
            }
            AgentEvent::SessionEnded(end) => {
                self.with_session(&end.hardknock_session_id, |s| { s.ended = true; s.revision += 1; self.enqueue(&s.id,"session_ended",json!({}))?; Ok(()) })?;
                self.experiments.end_session(&end.hardknock_session_id,self.config.experiments.continue_after_session_end);
                Ok(json!({"accepted":true}))
            },
            AgentEvent::LessonRejected(mut feedback) => self.with_session(&feedback.hardknock_session_id.clone(), |s| {
                if !s.delivered.iter().any(|l| l.lesson.id.to_string() == feedback.lesson_id) { return Err(invalid("Cannot reject an undelivered lesson")); }
                feedback.detail = feedback.detail.as_ref().map(|d|redact(d,512));
                s.rejections.insert(feedback.lesson_id.clone(),feedback); s.revision += 1;
                self.enqueue(&s.id,"lesson_rejected",json!({}))?; Ok(json!({"accepted":true}))
            }),
            AgentEvent::AgentMessage(message) => self.with_session(&message.hardknock_session_id, |s| {
                self.enqueue(&s.id,"agent_message",json!({"summary":redact(&message.summary,512)}))?; Ok(json!({"accepted":true}))
            }),
            AgentEvent::ExperimentRequested(request) => {
                let session = self.with_session(&request.hardknock_session_id, |s| Ok(s.clone()))?;
                self.context_config(&session.agent.name)?;
                self.experiments.request(request,&session,&self.config)
            },
            AgentEvent::ExperimentProgress { hardknock_session_id, experiment_id, after } => {
                self.with_session(&hardknock_session_id, |_| Ok(()))?;
                self.experiments.poll(&hardknock_session_id,&experiment_id,after)
            },
            AgentEvent::ExperimentCancelled { hardknock_session_id, experiment_id } => {
                self.with_session(&hardknock_session_id, |_| Ok(()))?;
                self.experiments.cancel(&hardknock_session_id,&experiment_id)
            },
        }
    }
    fn with_session<T>(&self, id: &str, f: impl FnOnce(&mut Session) -> Result<T>) -> Result<T> {
        let mut sessions = self.sessions.lock().expect("session lock");
        f(sessions
            .get_mut(id)
            .ok_or_else(|| invalid("Unknown Hardknock session"))?)
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
        let mut value = json!({"relevant":{"lessons":response.relevant_experience.iter().map(|b|&b.id).collect::<Vec<_>>(),"reflexes":bundle.relevant.reflexes,"recoveries":bundle.relevant.recoveries},"known_unknowns":bundle.known_unknowns.iter().take(8).map(|s|redact(s,256)).collect::<Vec<_>>(),"stale_items":bundle.stale_items,"contradictions":bundle.contradictions,"recommendations":bundle.recommendations.iter().take(3).map(|s|redact(s,256)).collect::<Vec<_>>(),"auto_run":false});
        redact_value(&mut value);
        response.development_context = Some(value);
        if serde_json::to_vec(&response)?.len() > self.config.bridge.max_context_bytes {
            response.development_context = None;
        }
        Ok(response)
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
        let mut sessions = self.sessions.lock().expect("session lock");
        if let Some(session) = sessions.get_mut(&id) {
            if session.cwd != cwd || session.agent != start.agent {
                return Err(invalid(
                    "Session identity/cwd changed; register a new external session id",
                ));
            }
            session.ended = false;
            self.experiments.resume_session(&id);
            session.revision += 1;
            self.enqueue(&id, "session_resumed", json!({"agent":session.agent.name}))?;
            let response = context_response(&id, &session.delivered, &config);
            let context = session.context.clone();
            let agent = session.agent.clone();
            drop(sessions);
            return Ok(serde_json::to_value(
                self.development_response(response, &context, &agent)?,
            )?);
        }
        if sessions.len() >= self.config.bridge.max_sessions {
            return Err(invalid("Bridge session budget exhausted"));
        }
        drop(sessions);
        start.task = start.task.as_ref().map(|t| redact(t, 512));
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
            .filter(|l| {
                response
                    .relevant_experience
                    .iter()
                    .any(|b| b.id == l.lesson.id.to_string())
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
        };
        let mut sessions = self.sessions.lock().expect("session lock");
        if sessions.len() >= self.config.bridge.max_sessions {
            return Err(invalid("Bridge session budget exhausted"));
        }
        if let Some(existing) = sessions.get(&id) {
            if existing.cwd != session.cwd || existing.agent != session.agent {
                return Err(invalid(
                    "Session identity/cwd changed; register a new external session id",
                ));
            }
            let response = context_response(&id, &existing.delivered, &config);
            let context = existing.context.clone();
            let agent = existing.agent.clone();
            drop(sessions);
            return Ok(serde_json::to_value(
                self.development_response(response, &context, &agent)?,
            )?);
        }
        sessions.insert(id.clone(), session);
        self.enqueue(&id, "session_started", json!({}))?;
        self.enqueue(
            &id,
            "experience_injected",
            json!({"count":response.relevant_experience.len()}),
        )?;
        Ok(serde_json::to_value(response)?)
    }
}
fn config_for(bridge: &std::sync::Weak<Bridge>) -> super::config::BridgeConfig {
    bridge
        .upgrade()
        .map(|b| b.config.bridge.clone())
        .unwrap_or_default()
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
