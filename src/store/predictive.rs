// SPDX-License-Identifier: Apache-2.0
use crate::{
    Error, Result,
    causal::CausalHypothesisStatus,
    core::*,
    experimentation::{ExperimentQuality, ExperimentStatus},
    predictive::*,
    store::Store,
};
use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use std::collections::{BTreeMap, BTreeSet};

fn data(value: &impl serde::Serialize) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}
fn name(value: &impl serde::Serialize) -> Result<String> {
    Ok(serde_json::to_value(value)?
        .as_str()
        .unwrap_or("custom")
        .to_owned())
}
fn event(tx: &Transaction<'_>, subject: &str, kind: &str, value: serde_json::Value) -> Result<()> {
    tx.execute(
        "INSERT INTO predictive_events(subject,kind,data) VALUES(?1,?2,?3)",
        params![subject, kind, serde_json::to_string(&value)?],
    )?;
    tx.execute(
        "INSERT INTO bridge_events(session_id,kind,data) VALUES('predictive-local',?1,?2)",
        params![
            kind,
            serde_json::to_string(&serde_json::json!({"subject":subject}))?
        ],
    )?;
    Ok(())
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct NewTrajectory {
    pub session_id: HardknockSessionId,
    #[serde(default)]
    pub subject: Option<TrajectorySubject>,
    pub task_family: Option<TaskFamilyId>,
    pub context: TrajectoryContext,
}

fn derive_signals(
    event: &TrajectoryEventKind,
    observation: &TrajectoryObservation,
    evidence: &[TrajectoryEvidenceRef],
    observed_at: chrono::DateTime<Utc>,
) -> Vec<RiskSignal> {
    let mut signals = Vec::new();
    for (feature, value) in &observation.features {
        let kind = match feature.as_str() {
            "retry_count" => Some(RiskSignalKind::RetryCount),
            "state_age_ms" | "prepared_effect_age_ms" => Some(RiskSignalKind::StateAge),
            "latency_ms" => Some(RiskSignalKind::Latency),
            "credential_age_ms" => Some(RiskSignalKind::CredentialAge),
            "resource_contention" => Some(RiskSignalKind::ResourceContention),
            "envelope_proximity" => Some(RiskSignalKind::EnvelopeBoundaryDistance),
            "state_stale" | "version_mismatch" => Some(RiskSignalKind::VersionMismatch),
            _ => None,
        };
        if let Some(kind) = kind {
            signals.push(RiskSignal {
                id: RiskSignalId::new(),
                kind,
                value: value.clone(),
                evidence: evidence.to_vec(),
                observed_at,
            });
        }
    }
    if matches!(event, TrajectoryEventKind::EffectUnknown) {
        signals.push(RiskSignal {
            id: RiskSignalId::new(),
            kind: RiskSignalKind::UnknownOutcome,
            value: TrajectoryValue::Boolean(true),
            evidence: evidence.to_vec(),
            observed_at,
        });
    }
    signals
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct NewTrajectoryEvent {
    pub kind: TrajectoryEventKind,
    pub observation: TrajectoryObservation,
    pub evidence: Vec<TrajectoryEvidenceRef>,
}

fn validate_observation(observation: &TrajectoryObservation) -> Result<()> {
    if observation.features.len() > 64 {
        return Err(Error::InvalidInput(
            "A trajectory event accepts at most 64 normalized features".into(),
        ));
    }
    for (key, value) in &observation.features {
        let valid_key = !key.is_empty()
            && key.len() <= 64
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
        if !valid_key || value.normalized().len() > 512 {
            return Err(Error::InvalidInput(
                "Trajectory features need bounded operational names and values; raw transcripts and secrets are not accepted".into(),
            ));
        }
    }
    Ok(())
}

fn outcome_name(outcome: &TrajectoryOutcome) -> &'static str {
    match outcome {
        TrajectoryOutcome::Success => "success",
        TrajectoryOutcome::Degraded => "degraded",
        TrajectoryOutcome::Failure(_) => "failure",
        TrajectoryOutcome::Aborted => "aborted",
        TrajectoryOutcome::Abstained => "abstained",
        TrajectoryOutcome::Unknown => "unknown",
    }
}

fn subject_parts(subject: &TrajectorySubject) -> (&'static str, String) {
    match subject {
        TrajectorySubject::Task(id) => ("task", id.to_string()),
        TrajectorySubject::Skill(id) => ("skill", id.to_string()),
        TrajectorySubject::EffectPlan(id) => ("effect_plan", id.to_string()),
        TrajectorySubject::Recovery(id) => ("recovery", id.to_string()),
        TrajectorySubject::RuntimeDecision(id) => ("runtime_decision", id.to_string()),
    }
}

fn pattern_step(condition: &TrajectoryCondition) -> TrajectoryPatternStep {
    let (event_pattern, state_predicates) = match condition {
        TrajectoryCondition::EventObserved { predicate } => (
            Some(EventPattern {
                kind: predicate.kind.clone(),
            }),
            predicate.feature.iter().cloned().collect(),
        ),
        TrajectoryCondition::FeatureCondition { predicate } => (None, vec![predicate.clone()]),
        _ => (None, Vec::new()),
    };
    TrajectoryPatternStep {
        event_pattern,
        state_predicates,
        causal_relevance: None,
        temporal: None,
        condition: Some(condition.clone()),
    }
}

impl Store {
    pub fn start_trajectory(&self, input: NewTrajectory) -> Result<ExecutionTrajectory> {
        if input.context.observability.len() > 128 {
            return Err(Error::InvalidInput(
                "Observability declaration is too large".into(),
            ));
        }
        let trajectory = ExecutionTrajectory {
            id: TrajectoryId::new(),
            session_id: input.session_id,
            subject: input
                .subject
                .unwrap_or_else(|| TrajectorySubject::Task(TaskId::new())),
            task_family: input.task_family,
            started_at: Utc::now(),
            ended_at: None,
            completed_at: None,
            events: Vec::new(),
            points: Vec::new(),
            outcome: None,
            context: input.context,
            fingerprint: Default::default(),
        };
        let (subject_kind, subject_id) = subject_parts(&trajectory.subject);
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO execution_trajectories(id,session_id,task_family_id,started_at,subject_kind,subject_id,data) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![trajectory.id.to_string(), trajectory.session_id.to_string(), trajectory.task_family.as_ref().map(ToString::to_string), trajectory.started_at.to_rfc3339(),subject_kind,subject_id, data(&trajectory)?],
        )?;
        event(
            &tx,
            &trajectory.id.to_string(),
            "trajectory_started",
            serde_json::json!({}),
        )?;
        tx.commit()?;
        Ok(trajectory)
    }

    pub fn append_trajectory_event(
        &self,
        id: &TrajectoryId,
        input: NewTrajectoryEvent,
    ) -> Result<TrajectoryEvent> {
        validate_observation(&input.observation)?;
        let mut trajectory = self.trajectory(id)?;
        if trajectory.ended_at.is_some() {
            return Err(Error::InvalidInput(
                "Ended trajectories are immutable".into(),
            ));
        }
        let sequence = u64::try_from(trajectory.events.len())
            .map_err(|_| Error::InvalidInput("Trajectory sequence overflow".into()))?;
        let item = TrajectoryEvent {
            id: TrajectoryEventId::new(),
            trajectory_id: id.clone(),
            sequence,
            timestamp: Utc::now(),
            kind: input.kind,
            observation: input.observation,
            evidence: input.evidence,
        };
        let point = TrajectoryPoint {
            index: sequence,
            timestamp: item.timestamp,
            event: item.kind.clone(),
            state: StateObservation {
                variables: item.observation.features.clone(),
            },
            derived_signals: derive_signals(
                &item.kind,
                &item.observation,
                &item.evidence,
                item.timestamp,
            ),
        };
        let mut all_events = self.trajectory_events(id)?;
        all_events.push(item.clone());
        trajectory.events.push(item.id.clone());
        trajectory.points.push(point.clone());
        trajectory.fingerprint = fingerprint(&all_events)?;
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO trajectory_events(id,trajectory_id,sequence,event_kind,created_at,data) VALUES(?1,?2,?3,?4,?5,?6)",
            params![item.id.to_string(), id.to_string(), i64::try_from(sequence).map_err(|_| Error::InvalidInput("Sequence overflow".into()))?, name(&item.kind)?, item.timestamp.to_rfc3339(), data(&item)?],
        )?;
        tx.execute(
            "INSERT INTO trajectory_points(trajectory_id,point_index,event_id,observed_at,data) VALUES(?1,?2,?3,?4,?5)",
            params![id.to_string(), i64::try_from(sequence).map_err(|_| Error::InvalidInput("Sequence overflow".into()))?, item.id.to_string(), point.timestamp.to_rfc3339(), data(&point)?],
        )?;
        for signal in &point.derived_signals {
            tx.execute(
                "INSERT INTO risk_signals(id,trajectory_id,point_index,kind,observed_at,data) VALUES(?1,?2,?3,?4,?5,?6)",
                params![signal.id.to_string(), id.to_string(), i64::try_from(sequence).map_err(|_| Error::InvalidInput("Sequence overflow".into()))?, name(&signal.kind)?, signal.observed_at.to_rfc3339(), data(signal)?],
            )?;
            event(
                &tx,
                &signal.id.to_string(),
                "trajectory_signal_observed",
                serde_json::json!({"trajectory":id,"point":sequence,"kind":signal.kind}),
            )?;
        }
        tx.execute(
            "UPDATE execution_trajectories SET data=?2 WHERE id=?1 AND ended_at IS NULL",
            params![id.to_string(), data(&trajectory)?],
        )?;
        event(
            &tx,
            &id.to_string(),
            "trajectory_updated",
            serde_json::json!({"event":item.id,"sequence":sequence}),
        )?;
        tx.commit()?;
        Ok(item)
    }

    pub fn trajectory(&self, id: &TrajectoryId) -> Result<ExecutionTrajectory> {
        self.get(
            "SELECT data FROM execution_trajectories WHERE id=?1",
            &id.to_string(),
        )
    }
    pub fn trajectories(&self) -> Result<Vec<ExecutionTrajectory>> {
        self.list("SELECT data FROM execution_trajectories ORDER BY started_at,id")
    }
    pub fn trajectory_events(&self, id: &TrajectoryId) -> Result<Vec<TrajectoryEvent>> {
        let mut statement = self.connection.prepare(
            "SELECT data FROM trajectory_events WHERE trajectory_id=?1 ORDER BY sequence",
        )?;
        statement
            .query_map([id.to_string()], |row| row.get::<_, String>(0))?
            .map(|item| Ok(serde_json::from_str(&item?)?))
            .collect()
    }

    pub fn finish_trajectory(
        &self,
        id: &TrajectoryId,
        outcome: TrajectoryOutcome,
    ) -> Result<ExecutionTrajectory> {
        let mut trajectory = self.trajectory(id)?;
        if trajectory.ended_at.is_some() {
            return Err(Error::InvalidInput(
                "Trajectory outcome is already final".into(),
            ));
        }
        let events = self.trajectory_events(id)?;
        trajectory.fingerprint = fingerprint(&events)?;
        trajectory.outcome = Some(outcome.clone());
        trajectory.ended_at = Some(Utc::now());
        trajectory.completed_at = trajectory.ended_at;
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE execution_trajectories SET outcome_kind=?2,ended_at=?3,fingerprint=?4,data=?5 WHERE id=?1 AND ended_at IS NULL",
            params![id.to_string(), outcome_name(&outcome), trajectory.ended_at.expect("set").to_rfc3339(), trajectory.fingerprint.hash, data(&trajectory)?],
        )?;
        event(
            &tx,
            &id.to_string(),
            "trajectory_resolved",
            serde_json::json!({"outcome":outcome}),
        )?;
        tx.commit()?;
        Ok(trajectory)
    }

    pub fn register_risk_indicator(&self, mut indicator: RiskIndicator) -> Result<RiskIndicator> {
        indicator.status = RiskIndicatorStatus::Candidate;
        let failures = indicator
            .associated_failures
            .iter()
            .map(|f| f.signature.as_str())
            .collect::<Vec<_>>()
            .join("|");
        self.connection.execute(
            "INSERT INTO risk_indicators(id,status,failure_class,data) VALUES(?1,'candidate',?2,?3)",
            params![indicator.id.to_string(), failures, data(&indicator)?],
        )?;
        Ok(indicator)
    }

    pub fn risk_indicators(&self) -> Result<Vec<RiskIndicator>> {
        self.list("SELECT data FROM risk_indicators ORDER BY id")
    }

    pub fn register_failure_trajectory(
        &self,
        mut trajectory: FailureTrajectory,
    ) -> Result<FailureTrajectory> {
        if trajectory.sequence.is_empty() || trajectory.sequence.len() > 20 {
            return Err(Error::InvalidInput(
                "A failure trajectory needs 1..20 ordered pattern steps".into(),
            ));
        }
        trajectory.revision = trajectory.revision.max(1);
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO failure_trajectories(id,failure_class,status,runtime_version,revision,data) VALUES(?1,?2,?3,?4,?5,?6)",
            params![trajectory.id.to_string(),trajectory.failure_signature.signature,name(&trajectory.status)?,trajectory.required_runtime_version,i64::try_from(trajectory.revision).map_err(|_|Error::InvalidInput("Failure trajectory revision overflow".into()))?,data(&trajectory)?],
        )?;
        for (step_index, step) in trajectory.sequence.iter().enumerate() {
            tx.execute(
                "INSERT INTO failure_trajectory_steps(failure_trajectory_id,revision,step_index,data) VALUES(?1,?2,?3,?4)",
                params![trajectory.id.to_string(), i64::try_from(trajectory.revision).map_err(|_|Error::InvalidInput("Failure trajectory revision overflow".into()))?, i64::try_from(step_index).map_err(|_|Error::InvalidInput("Failure trajectory step overflow".into()))?, data(step)?],
            )?;
        }
        tx.commit()?;
        Ok(trajectory)
    }

    pub fn failure_trajectory(&self, id: &FailureTrajectoryId) -> Result<FailureTrajectory> {
        self.get(
            "SELECT data FROM failure_trajectories WHERE id=?1",
            &id.to_string(),
        )
    }

    pub fn failure_trajectories(&self) -> Result<Vec<FailureTrajectory>> {
        self.list("SELECT data FROM failure_trajectories ORDER BY id")
    }

    pub fn failure_trajectories_for(&self, failure: &str) -> Result<Vec<FailureTrajectory>> {
        let mut statement = self
            .connection
            .prepare("SELECT data FROM failure_trajectories WHERE failure_class=?1 ORDER BY id")?;
        statement
            .query_map([failure], |row| row.get::<_, String>(0))?
            .map(|item| Ok(serde_json::from_str(&item?)?))
            .collect()
    }

    pub fn register_failure_trajectory_family(
        &self,
        family: FailureTrajectoryFamily,
    ) -> Result<FailureTrajectoryFamily> {
        if family.trajectories.len() < 2 {
            return Err(Error::InvalidInput(
                "A failure trajectory family requires at least two paths".into(),
            ));
        }
        for id in &family.trajectories {
            let trajectory = self.failure_trajectory(id)?;
            if trajectory.failure_signature != family.failure_signature {
                return Err(Error::InvalidInput(
                    "Every family path must target the same failure signature".into(),
                ));
            }
        }
        self.connection.execute(
            "INSERT INTO failure_trajectory_families(id,failure_class,data) VALUES(?1,?2,?3)",
            params![
                family.id.to_string(),
                family.failure_signature.signature,
                data(&family)?
            ],
        )?;
        Ok(family)
    }

    pub fn failure_trajectory_families(&self) -> Result<Vec<FailureTrajectoryFamily>> {
        self.list("SELECT data FROM failure_trajectory_families ORDER BY id")
    }

    /// Compare repeated target failures with successful negative controls and
    /// persist only candidate indicators. Discovery never activates a warning.
    pub fn discover_candidate_risk_indicators(&self, failure: &str) -> Result<Vec<RiskIndicator>> {
        let trajectories = self.trajectories()?;
        let target_failures = trajectories
            .iter()
            .filter(|trajectory| {
                matches!(&trajectory.outcome, Some(TrajectoryOutcome::Failure(observed)) if observed.signature == failure)
            })
            .collect::<Vec<_>>();
        let Some(reference) = target_failures.first() else {
            return Ok(Vec::new());
        };
        let reference_scope = reference.context.scope.clone();
        let reference_runtime = reference.context.runtime_version.clone();
        let failures = target_failures
            .into_iter()
            .filter(|trajectory| {
                trajectory.context.scope == reference_scope
                    && trajectory.context.runtime_version == reference_runtime
            })
            .collect::<Vec<_>>();
        let successes = trajectories
            .iter()
            .filter(|trajectory| {
                trajectory.outcome == Some(TrajectoryOutcome::Success)
                    && trajectory.context.scope == reference_scope
                    && trajectory.context.runtime_version == reference_runtime
            })
            .collect::<Vec<_>>();
        if failures.len() < 2 || successes.len() < 2 {
            return Ok(Vec::new());
        }

        let candidates_for =
            |trajectory: &ExecutionTrajectory| -> Result<BTreeMap<String, TrajectoryCondition>> {
                let mut candidates = BTreeMap::new();
                for event in self
                    .trajectory_events(&trajectory.id)?
                    .into_iter()
                    .take_while(|event| event.kind != TrajectoryEventKind::FailureObserved)
                {
                    let event_condition = TrajectoryCondition::EventObserved {
                        predicate: EventPredicate {
                            kind: event.kind.clone(),
                            feature: None,
                        },
                    };
                    candidates.insert(data(&event_condition)?, event_condition);
                    for (feature, value) in event.observation.features {
                        if matches!(feature.as_str(), "duration_ms" | "success") {
                            continue;
                        }
                        let condition = TrajectoryCondition::EventObserved {
                            predicate: EventPredicate {
                                kind: event.kind.clone(),
                                feature: Some(FeaturePredicate {
                                    feature,
                                    operator: ComparisonOperator::Equals,
                                    value,
                                }),
                            },
                        };
                        candidates.insert(data(&condition)?, condition);
                    }
                }
                Ok(candidates)
            };

        let mut failure_counts: BTreeMap<String, (TrajectoryCondition, usize)> = BTreeMap::new();
        for trajectory in &failures {
            for (key, condition) in candidates_for(trajectory)? {
                failure_counts
                    .entry(key)
                    .and_modify(|(_, count)| *count += 1)
                    .or_insert((condition, 1));
            }
        }
        let mut success_counts = BTreeMap::<String, usize>::new();
        for trajectory in &successes {
            for key in candidates_for(trajectory)?.into_keys() {
                *success_counts.entry(key).or_default() += 1;
            }
        }

        let existing = self.risk_indicators()?;
        let common_scope = failures[0].context.scope.clone();
        let evidence = failures
            .iter()
            .map(|trajectory| TrajectoryEvidenceRef::Trajectory(trajectory.id.clone()))
            .collect::<Vec<_>>();
        let target = crate::runtime::FailureSignatureRef {
            signature: failure.to_owned(),
        };
        let mut discovered = Vec::new();
        for (key, (condition, failure_count)) in failure_counts {
            let success_count = success_counts.get(&key).copied().unwrap_or(0);
            if failure_count != failures.len() || success_count * 4 > successes.len() {
                continue;
            }
            let indicator_condition = IndicatorCondition {
                conditions: vec![condition],
            };
            if let Some(indicator) = existing.iter().find(|indicator| {
                indicator.associated_failures.contains(&target)
                    && indicator.condition.conditions == indicator_condition.conditions
            }) {
                discovered.push(indicator.clone());
                continue;
            }
            let suffix = &blake3::hash(key.as_bytes()).to_hex()[..12];
            discovered.push(self.register_risk_indicator(RiskIndicator {
                id: RiskIndicatorId::new(),
                name: format!("candidate precursor {failure} {suffix}"),
                condition: indicator_condition,
                associated_failures: vec![target.clone()],
                scope: common_scope.clone(),
                evidence: evidence.clone(),
                status: RiskIndicatorStatus::Candidate,
                origin: PredictiveOrigin::Local,
            })?);
        }
        Ok(discovered)
    }

    pub fn register_warning_signature(
        &self,
        mut signature: EarlyWarningSignature,
    ) -> Result<EarlyWarningSignature> {
        if signature.ordered_conditions.is_empty() || signature.ordered_conditions.len() > 20 {
            return Err(Error::InvalidInput(
                "A warning signature needs 1..20 explicit conditions".into(),
            ));
        }
        signature.status = RiskIndicatorStatus::Candidate;
        signature.revision = 1;
        signature.created_at = Utc::now();
        signature.updated_at = signature.created_at;
        if signature.failure_trajectory.is_none() {
            let failure_trajectory = self.register_failure_trajectory(FailureTrajectory {
                id: FailureTrajectoryId::new(),
                failure_signature: signature.failure.clone(),
                causal_model: None,
                sequence: signature
                    .ordered_conditions
                    .iter()
                    .map(pattern_step)
                    .collect(),
                scope: signature.scope.clone(),
                evidence: signature.evidence.clone(),
                status: FailureTrajectoryStatus::Candidate,
                origin: signature.origin,
                required_runtime_version: signature.required_runtime_version.clone(),
                revision: 1,
            })?;
            signature.failure_trajectory = Some(failure_trajectory.id);
        }
        let revision = ForecastSignatureRevision {
            id: ForecastRevisionId::new(),
            signature_id: signature.id.clone(),
            revision: 1,
            conditions: signature.ordered_conditions.clone(),
            evidence: signature.evidence.clone(),
            created_at: signature.created_at,
        };
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO early_warning_signatures(id,status,failure_class,revision,runtime_version,data) VALUES(?1,'candidate',?2,1,?3,?4)",
            params![signature.id.to_string(), signature.failure.signature, signature.required_runtime_version, data(&signature)?],
        )?;
        tx.execute(
            "INSERT INTO early_warning_revisions(id,signature_id,revision,created_at,data) VALUES(?1,?2,1,?3,?4)",
            params![revision.id.to_string(), signature.id.to_string(), revision.created_at.to_rfc3339(), data(&revision)?],
        )?;
        event(
            &tx,
            &signature.id.to_string(),
            "early_warning_signature_registered",
            serde_json::json!({"origin":signature.origin}),
        )?;
        tx.commit()?;
        Ok(signature)
    }

    pub fn warning_signature(&self, id: &EarlyWarningSignatureId) -> Result<EarlyWarningSignature> {
        self.get(
            "SELECT data FROM early_warning_signatures WHERE id=?1",
            &id.to_string(),
        )
    }
    pub fn warning_signatures(&self) -> Result<Vec<EarlyWarningSignature>> {
        self.list("SELECT data FROM early_warning_signatures ORDER BY id")
    }

    pub fn refresh_warning_freshness(
        &self,
        id: &EarlyWarningSignatureId,
        context: &TrajectoryContext,
    ) -> Result<EarlyWarningSignature> {
        let mut signature = self.warning_signature(id)?;
        let version_changed = signature
            .required_runtime_version
            .as_ref()
            .is_some_and(|version| context.runtime_version.as_ref() != Some(version));
        if !version_changed
            || !matches!(
                signature.status,
                RiskIndicatorStatus::Supported | RiskIndicatorStatus::Validated
            )
        {
            return Ok(signature);
        }
        signature.status = RiskIndicatorStatus::Stale;
        signature.updated_at = Utc::now();
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE early_warning_signatures SET status='stale',data=?2 WHERE id=?1",
            params![id.to_string(), data(&signature)?],
        )?;
        if let Some(trajectory_id) = &signature.failure_trajectory {
            let mut trajectory = self.failure_trajectory(trajectory_id)?;
            trajectory.status = FailureTrajectoryStatus::Stale;
            tx.execute(
                "UPDATE failure_trajectories SET status='stale',data=?2 WHERE id=?1",
                params![trajectory_id.to_string(), data(&trajectory)?],
            )?;
        }
        event(
            &tx,
            &id.to_string(),
            "early_warning_stale",
            serde_json::json!({
                "required_runtime_version":signature.required_runtime_version,
                "observed_runtime_version":context.runtime_version
            }),
        )?;
        tx.commit()?;
        Ok(signature)
    }
    pub fn warning_signature_history(
        &self,
        id: &EarlyWarningSignatureId,
    ) -> Result<Vec<ForecastSignatureRevision>> {
        let mut statement = self.connection.prepare(
            "SELECT data FROM early_warning_revisions WHERE signature_id=?1 ORDER BY revision",
        )?;
        statement
            .query_map([id.to_string()], |row| row.get::<_, String>(0))?
            .map(|item| Ok(serde_json::from_str(&item?)?))
            .collect()
    }

    pub fn propose_warning_revision(
        &self,
        id: &EarlyWarningSignatureId,
        conditions: Vec<TrajectoryCondition>,
        evidence: Vec<TrajectoryEvidenceRef>,
    ) -> Result<ForecastSignatureRevision> {
        if conditions.is_empty() || conditions.len() > 20 {
            return Err(Error::InvalidInput(
                "A revision needs 1..20 explicit conditions".into(),
            ));
        }
        let current = self.warning_signature(id)?;
        let revision = ForecastSignatureRevision {
            id: ForecastRevisionId::new(),
            signature_id: id.clone(),
            revision: current.revision + 1,
            conditions,
            evidence,
            created_at: Utc::now(),
        };
        self.connection.execute(
            "INSERT INTO early_warning_revisions(id,signature_id,revision,created_at,data) VALUES(?1,?2,?3,?4,?5)",
            params![revision.id.to_string(), id.to_string(), i64::try_from(revision.revision).map_err(|_| Error::InvalidInput("Revision overflow".into()))?, revision.created_at.to_rfc3339(), data(&revision)?],
        )?;
        Ok(revision)
    }

    pub fn validate_warning_signature(
        &self,
        id: &EarlyWarningSignatureId,
        positives: &[TrajectoryId],
        negative_controls: &[TrajectoryId],
    ) -> Result<EarlyWarningSignature> {
        let mut signature = self.warning_signature(id)?;
        if signature.origin != PredictiveOrigin::Local {
            return Err(Error::Intervention("Federated signatures remain advisory until copied into a local candidate and reproduced".into()));
        }
        if positives.is_empty() || negative_controls.is_empty() {
            return Err(Error::InvalidInput(
                "Validation requires failure examples and successful negative controls".into(),
            ));
        }
        let positive_ids: BTreeSet<_> = positives.iter().collect();
        let negative_ids: BTreeSet<_> = negative_controls.iter().collect();
        if positive_ids.len() != positives.len()
            || negative_ids.len() != negative_controls.len()
            || !positive_ids.is_disjoint(&negative_ids)
        {
            return Err(Error::InvalidInput(
                "Validation requires distinct positive and negative-control trajectories".into(),
            ));
        }
        let mut true_positives = 0;
        let mut false_positives = 0;
        let mut evidence = BTreeSet::new();
        for trajectory_id in positives {
            let trajectory = self.trajectory(trajectory_id)?;
            if !crate::predictive::scope_compatible(&signature.scope, &trajectory.context)
                || signature
                    .required_runtime_version
                    .as_ref()
                    .is_some_and(|version| {
                        trajectory.context.runtime_version.as_ref() != Some(version)
                    })
            {
                return Err(Error::InvalidInput(
                    "Validation trajectories must satisfy the signature scope and runtime version"
                        .into(),
                ));
            }
            let is_target = matches!(trajectory.outcome, Some(TrajectoryOutcome::Failure(ref f)) if f == &signature.failure);
            let events = self.trajectory_events(trajectory_id)?;
            let before_failure = events
                .iter()
                .take_while(|event| event.kind != TrajectoryEventKind::FailureObserved)
                .cloned()
                .collect::<Vec<_>>();
            let matched = signature_matches(&signature, &before_failure)
                .unmatched_conditions
                .is_empty();
            if is_target && matched {
                true_positives += 1;
            }
            evidence.insert(TrajectoryEvidenceRef::Trajectory(trajectory_id.clone()));
        }
        for trajectory_id in negative_controls {
            let trajectory = self.trajectory(trajectory_id)?;
            if !crate::predictive::scope_compatible(&signature.scope, &trajectory.context)
                || signature
                    .required_runtime_version
                    .as_ref()
                    .is_some_and(|version| {
                        trajectory.context.runtime_version.as_ref() != Some(version)
                    })
            {
                return Err(Error::InvalidInput(
                    "Validation trajectories must satisfy the signature scope and runtime version"
                        .into(),
                ));
            }
            if trajectory.outcome != Some(TrajectoryOutcome::Success) {
                return Err(Error::InvalidInput(
                    "Negative controls must have successful ground-truth outcomes".into(),
                ));
            }
            if signature_matches(&signature, &self.trajectory_events(trajectory_id)?)
                .unmatched_conditions
                .is_empty()
            {
                false_positives += 1;
            }
            evidence.insert(TrajectoryEvidenceRef::Trajectory(trajectory_id.clone()));
        }
        signature.evidence.extend(evidence);
        signature.status = if true_positives == positives.len() && false_positives == 0 {
            if positives.len() >= 2 && negative_controls.len() >= 2 {
                RiskIndicatorStatus::Validated
            } else {
                RiskIndicatorStatus::Supported
            }
        } else if false_positives * 2 > negative_controls.len() {
            RiskIndicatorStatus::Contradicted
        } else {
            RiskIndicatorStatus::Candidate
        };
        let forecast_count = true_positives + false_positives;
        signature.precision = (forecast_count > 0).then(|| EmpiricalRate {
            value: true_positives as f64 / forecast_count as f64,
            sample_count: u64::try_from(forecast_count).unwrap_or(u64::MAX),
        });
        signature.recall = (!positives.is_empty()).then(|| EmpiricalRate {
            value: true_positives as f64 / positives.len() as f64,
            sample_count: u64::try_from(positives.len()).unwrap_or(u64::MAX),
        });
        signature.updated_at = Utc::now();
        self.connection.execute(
            "UPDATE early_warning_signatures SET status=?2,data=?3 WHERE id=?1",
            params![id.to_string(), name(&signature.status)?, data(&signature)?],
        )?;
        if let Some(trajectory_id) = &signature.failure_trajectory {
            let mut failure_trajectory = self.failure_trajectory(trajectory_id)?;
            failure_trajectory.status = match signature.status {
                RiskIndicatorStatus::Candidate => FailureTrajectoryStatus::Candidate,
                RiskIndicatorStatus::Supported => FailureTrajectoryStatus::Supported,
                RiskIndicatorStatus::Validated => FailureTrajectoryStatus::Validated,
                RiskIndicatorStatus::Noisy => FailureTrajectoryStatus::Candidate,
                RiskIndicatorStatus::Stale => FailureTrajectoryStatus::Stale,
                RiskIndicatorStatus::Contradicted => FailureTrajectoryStatus::Contradicted,
                RiskIndicatorStatus::Retired => FailureTrajectoryStatus::Retired,
            };
            failure_trajectory.evidence = signature.evidence.clone();
            self.connection.execute(
                "UPDATE failure_trajectories SET status=?2,data=?3 WHERE id=?1",
                params![
                    trajectory_id.to_string(),
                    name(&failure_trajectory.status)?,
                    data(&failure_trajectory)?
                ],
            )?;
        }
        Ok(signature)
    }

    pub fn localize_federated_signature(
        &self,
        remote: &EarlyWarningSignatureId,
    ) -> Result<EarlyWarningSignature> {
        let source = self.warning_signature(remote)?;
        if source.origin != PredictiveOrigin::FederatedAdvisory {
            return Err(Error::InvalidInput(
                "Only a federated advisory needs local reproduction".into(),
            ));
        }
        self.register_warning_signature(EarlyWarningSignature {
            id: EarlyWarningSignatureId::new(),
            origin: PredictiveOrigin::Local,
            status: RiskIndicatorStatus::Candidate,
            evidence: vec![TrajectoryEvidenceRef::ExternalAdvisory(remote.to_string())],
            revision: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            ..source
        })
    }

    pub fn promote_warning_revision(
        &self,
        revision_id: &ForecastRevisionId,
        positives: &[TrajectoryId],
        negative_controls: &[TrajectoryId],
    ) -> Result<EarlyWarningSignature> {
        let revision: ForecastSignatureRevision = self.get(
            "SELECT data FROM early_warning_revisions WHERE id=?1",
            &revision_id.to_string(),
        )?;
        let mut signature = self.warning_signature(&revision.signature_id)?;
        signature.ordered_conditions = revision.conditions;
        signature.evidence = revision.evidence;
        signature.revision = revision.revision;
        signature.status = RiskIndicatorStatus::Candidate;
        signature.updated_at = Utc::now();
        if let Some(id) = &signature.failure_trajectory {
            let mut failure_trajectory = self.failure_trajectory(id)?;
            failure_trajectory.sequence = signature
                .ordered_conditions
                .iter()
                .map(pattern_step)
                .collect();
            failure_trajectory.revision = revision.revision;
            failure_trajectory.status = FailureTrajectoryStatus::Candidate;
            failure_trajectory.evidence = signature.evidence.clone();
            let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
            tx.execute(
                "UPDATE failure_trajectories SET status='candidate',revision=?2,data=?3 WHERE id=?1",
                params![id.to_string(),i64::try_from(revision.revision).map_err(|_|Error::InvalidInput("Failure trajectory revision overflow".into()))?,data(&failure_trajectory)?],
            )?;
            for (step_index, step) in failure_trajectory.sequence.iter().enumerate() {
                tx.execute(
                    "INSERT INTO failure_trajectory_steps(failure_trajectory_id,revision,step_index,data) VALUES(?1,?2,?3,?4)",
                    params![id.to_string(), i64::try_from(revision.revision).map_err(|_|Error::InvalidInput("Failure trajectory revision overflow".into()))?, i64::try_from(step_index).map_err(|_|Error::InvalidInput("Failure trajectory step overflow".into()))?, data(step)?],
                )?;
            }
            tx.commit()?;
        }
        self.connection.execute(
            "UPDATE early_warning_signatures SET revision=?2,status='candidate',data=?3 WHERE id=?1",
            params![signature.id.to_string(), i64::try_from(signature.revision).map_err(|_| Error::InvalidInput("Revision overflow".into()))?, data(&signature)?],
        )?;
        self.validate_warning_signature(&signature.id, positives, negative_controls)
    }

    pub fn forecast_trajectory(&self, id: &TrajectoryId) -> Result<Vec<FailureForecast>> {
        self.forecast_trajectory_internal(id, true, ForecastPolicyConfig::default())
    }

    /// Synchronous runtime path. Signature validation already names its positive and
    /// negative-control trajectories, so this avoids rescanning historical trajectories.
    pub fn forecast_trajectory_fast(&self, id: &TrajectoryId) -> Result<Vec<FailureForecast>> {
        self.forecast_trajectory_internal(id, false, ForecastPolicyConfig::default())
    }

    pub fn forecast_trajectory_fast_with_policy(
        &self,
        id: &TrajectoryId,
        profile: ForecastPolicyProfile,
    ) -> Result<Vec<FailureForecast>> {
        self.forecast_trajectory_internal(id, false, ForecastPolicyConfig::for_profile(profile))
    }

    fn forecast_trajectory_internal(
        &self,
        id: &TrajectoryId,
        match_history: bool,
        policy: ForecastPolicyConfig,
    ) -> Result<Vec<FailureForecast>> {
        let trajectory = self.trajectory(id)?;
        let events = self.trajectory_events(id)?;
        let mut signatures = self.warning_signatures()?;
        for signature in &mut signatures {
            if signature.origin == PredictiveOrigin::Local {
                *signature = self.refresh_warning_freshness(&signature.id, &trajectory.context)?;
            }
        }
        let mut causal = Vec::new();
        for hypothesis in self.causal_hypotheses()? {
            if matches!(
                hypothesis.status,
                CausalHypothesisStatus::Supported | CausalHypothesisStatus::StronglySupported
            ) && hypothesis.remote_origin.is_none()
            {
                causal.push(hypothesis.id);
            }
        }
        let history = if match_history {
            self.trajectories()?
                .into_iter()
                .filter(|item| item.ended_at.is_some())
                .collect()
        } else {
            Vec::new()
        };
        let context = ForecastContext {
            events,
            signatures,
            indicators: self.risk_indicators()?,
            history,
            supported_causal_hypotheses: causal,
            causal_precursors: Vec::new(),
            envelope_proximity: trajectory
                .points
                .iter()
                .rev()
                .find_map(|point| point.state.variables.get("envelope_proximity"))
                .and_then(|value| match value {
                    TrajectoryValue::Text(value) => match value.as_str() {
                        "interior" => Some(EnvelopeProximity::Interior),
                        "near_boundary" => Some(EnvelopeProximity::NearBoundary),
                        "at_boundary" => Some(EnvelopeProximity::AtBoundary),
                        "outside_known_safe_region" => {
                            Some(EnvelopeProximity::OutsideKnownSafeRegion)
                        }
                        _ => None,
                    },
                    _ => None,
                })
                .unwrap_or_default(),
            risk: crate::curriculum::Severity::Medium,
            failure_trajectories: self.failure_trajectories()?,
            interventions: self.preventive_interventions()?,
            policy,
            window: Default::default(),
        };
        let forecasts = DeterministicForecastEngine.forecast(&trajectory, &context)?;
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO forecast_policy_versions(version,data) VALUES(?1,?2) ON CONFLICT(version) DO NOTHING",
            params![context.policy.version,data(&context.policy)?],
        )?;
        let mut stored = Vec::new();
        for mut forecast in forecasts {
            let existing = tx.query_row(
                "SELECT data FROM failure_forecasts WHERE trajectory_id=?1 AND signature_id=?2 AND status IN ('watch','elevated','actionable','imminent','active') ORDER BY created_at LIMIT 1",
                params![id.to_string(), forecast.signature.to_string()],
                |row| row.get::<_, String>(0),
            ).optional()?;
            if let Some(existing) = existing {
                let prior: FailureForecast = serde_json::from_str(&existing)?;
                forecast.id = prior.id.clone();
                forecast.created_at = prior.created_at;
                if forecast.status > prior.status {
                    tx.execute(
                        "UPDATE failure_forecasts SET status=?2,data=?3 WHERE id=?1",
                        params![
                            forecast.id.to_string(),
                            name(&forecast.status)?,
                            data(&forecast)?
                        ],
                    )?;
                    event(
                        &tx,
                        &forecast.id.to_string(),
                        "failure_forecast_escalated",
                        serde_json::json!({"from":prior.status,"to":forecast.status}),
                    )?;
                } else {
                    forecast = prior;
                }
                stored.push(forecast);
                continue;
            }
            tx.execute(
                "INSERT INTO failure_forecasts(id,trajectory_id,signature_id,failure_class,status,created_at,data) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![forecast.id.to_string(), id.to_string(), forecast.signature.to_string(), forecast.failure.signature,name(&forecast.status)?, forecast.created_at.to_rfc3339(), data(&forecast)?],
            )?;
            for signal in &forecast.matched_signals {
                tx.execute(
                    "INSERT OR IGNORE INTO forecast_signal_refs(forecast_id,signal_id) VALUES(?1,?2)",
                    params![forecast.id.to_string(),signal.to_string()],
                )?;
            }
            event(
                &tx,
                &forecast.id.to_string(),
                "failure_forecast_created",
                serde_json::json!({"trajectory":id,"strength":forecast.strength,"horizon":forecast.horizon}),
            )?;
            for indicator in &forecast.indicators {
                event(
                    &tx,
                    &indicator.to_string(),
                    "risk_indicator_matched",
                    serde_json::json!({"forecast":forecast.id,"trajectory":id}),
                )?;
            }
            stored.push(forecast);
        }
        tx.commit()?;
        Ok(stored)
    }

    pub fn forecast(&self, id: &FailureForecastId) -> Result<FailureForecast> {
        self.get(
            "SELECT data FROM failure_forecasts WHERE id=?1",
            &id.to_string(),
        )
    }
    pub fn forecasts(&self) -> Result<Vec<FailureForecast>> {
        self.list("SELECT data FROM failure_forecasts ORDER BY created_at,id")
    }

    pub fn active_forecasts(&self) -> Result<Vec<FailureForecast>> {
        Ok(self
            .forecasts()?
            .into_iter()
            .filter(|forecast| forecast.status.is_active())
            .collect())
    }

    pub fn backtest_warning(&self, id: &EarlyWarningSignatureId) -> Result<serde_json::Value> {
        let signature = self.warning_signature(id)?;
        let development_ids: BTreeSet<_> = signature
            .evidence
            .iter()
            .filter_map(|evidence| {
                if let TrajectoryEvidenceRef::Trajectory(id) = evidence {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect();
        let all = self
            .trajectories()?
            .into_iter()
            .filter(|trajectory| {
                trajectory.ended_at.is_some()
                    && scope_compatible(&signature.scope, &trajectory.context)
                    && signature
                        .required_runtime_version
                        .as_ref()
                        .is_none_or(|version| {
                            trajectory.context.runtime_version.as_ref() == Some(version)
                        })
            })
            .collect::<Vec<_>>();
        let held_out = all
            .iter()
            .filter(|trajectory| !development_ids.contains(&trajectory.id))
            .collect::<Vec<_>>();
        let (evaluation, held_out_only) = if held_out.is_empty() {
            (all.iter().collect::<Vec<_>>(), false)
        } else {
            (held_out, true)
        };
        let mut true_positives = 0_usize;
        let mut false_positives = 0_usize;
        let mut misses = 0_usize;
        let mut leads = Vec::<u64>::new();
        for trajectory in evaluation {
            let events = self.trajectory_events(&trajectory.id)?;
            let before_failure = events
                .iter()
                .take_while(|event| event.kind != TrajectoryEventKind::FailureObserved)
                .cloned()
                .collect::<Vec<_>>();
            let mut first_match = None;
            for end in 1..=before_failure.len() {
                if signature_matches(&signature, &before_failure[..end])
                    .unmatched_conditions
                    .is_empty()
                {
                    first_match = Some(end - 1);
                    break;
                }
            }
            let target_failure = matches!(&trajectory.outcome,Some(TrajectoryOutcome::Failure(observed)) if observed == &signature.failure);
            match (first_match, target_failure) {
                (Some(index), true) => {
                    true_positives += 1;
                    leads.push(before_failure.len().saturating_sub(index + 1) as u64);
                }
                (Some(_), false) => false_positives += 1,
                (None, true) => misses += 1,
                (None, false) => {}
            }
        }
        let forecast_count = true_positives + false_positives;
        let failures = true_positives + misses;
        let precision =
            (forecast_count > 0).then_some(true_positives as f64 / forecast_count as f64);
        let recall = (failures > 0).then_some(true_positives as f64 / failures as f64);
        leads.sort_unstable();
        let median_lead = leads.get(leads.len() / 2).copied();
        Ok(serde_json::json!({
            "warning":id,
            "historical_support_only":true,
            "held_out_only":held_out_only,
            "sample_count":forecast_count+misses,
            "true_positives":true_positives,
            "false_positives":false_positives,
            "misses":misses,
            "precision":precision,
            "recall":recall,
            "lead_events":leads,
            "median_lead_events":median_lead,
            "notice":"Historical backtest does not prospectively validate a warning"
        }))
    }

    pub fn forecast_audit(&self, limit: usize) -> Result<serde_json::Value> {
        if limit == 0 || limit > 10_000 {
            return Err(Error::InvalidInput(
                "Forecast audit limit must be between 1 and 10000".into(),
            ));
        }
        let forecasts = self.forecasts()?;
        let mut statuses = BTreeMap::<ForecastStatus, u64>::new();
        for forecast in forecasts.iter().rev().take(limit) {
            *statuses.entry(forecast.status).or_default() += 1;
        }
        let feedback: Vec<ForecastFeedback> =
            self.list("SELECT data FROM forecast_feedback ORDER BY created_at DESC,id DESC")?;
        let mut outcomes = BTreeMap::<String, u64>::new();
        for item in feedback.into_iter().take(limit) {
            *outcomes.entry(name(&item.outcome)?).or_default() += 1;
        }
        Ok(serde_json::json!({"limit":limit,"statuses":statuses,"outcomes":outcomes}))
    }

    pub fn forecast_gaps(&self) -> Result<serde_json::Value> {
        let signatures = self.warning_signatures()?;
        let known_failures = self
            .trajectories()?
            .into_iter()
            .filter_map(|trajectory| match trajectory.outcome {
                Some(TrajectoryOutcome::Failure(failure)) => Some(failure.signature),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let uncovered = known_failures
            .into_iter()
            .filter(|failure| {
                !signatures.iter().any(|signature| {
                    signature.failure.signature == *failure
                        && signature.status == RiskIndicatorStatus::Validated
                        && signature.origin == PredictiveOrigin::Local
                })
            })
            .collect::<Vec<_>>();
        let mut noisy = Vec::new();
        for signature in &signatures {
            let health = self.forecast_health(&signature.id)?;
            if matches!(
                health.health,
                ForecastHealth::Degrading | ForecastHealth::Contradicted
            ) {
                noisy.push(health);
            }
        }
        Ok(serde_json::json!({
            "failure_classes_without_validated_warning":uncovered,
            "noisy_warnings":noisy,
            "missed_failures":self.forecast_misses()?,
            "curriculum":self.predictive_curriculum_goals()?
        }))
    }

    pub fn register_preventive_intervention(
        &self,
        mut intervention: PreventiveIntervention,
    ) -> Result<PreventiveIntervention> {
        let signature = self.warning_signature(&intervention.signature)?;
        if signature.failure != intervention.target_failure {
            return Err(Error::InvalidInput(
                "Intervention target must match its warning signature".into(),
            ));
        }
        intervention.status = PreventiveInterventionStatus::Candidate;
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO preventive_interventions(id,signature_id,status,failure_class,data) VALUES(?1,?2,'candidate',?3,?4)",
            params![intervention.id.to_string(), intervention.signature.to_string(), intervention.target_failure.signature, data(&intervention)?],
        )?;
        if let Some(window) = &intervention.window {
            if window
                .closes_at
                .as_ref()
                .is_some_and(|(trajectory, _)| trajectory != &window.opens_at.0)
            {
                return Err(Error::InvalidInput(
                    "An intervention window must reference one trajectory".into(),
                ));
            }
            tx.execute(
                "INSERT INTO intervention_windows(intervention_id,trajectory_id,opens_at,closes_at,data) VALUES(?1,?2,?3,?4,?5)",
                params![intervention.id.to_string(),window.opens_at.0.to_string(),i64::try_from(window.opens_at.1).map_err(|_|Error::InvalidInput("Intervention point overflow".into()))?,window.closes_at.as_ref().map(|(_,point)|i64::try_from(*point)).transpose().map_err(|_|Error::InvalidInput("Intervention point overflow".into()))?,data(window)?],
            )?;
        }
        tx.commit()?;
        Ok(intervention)
    }
    pub fn preventive_intervention(
        &self,
        id: &PreventiveInterventionId,
    ) -> Result<PreventiveIntervention> {
        self.get(
            "SELECT data FROM preventive_interventions WHERE id=?1",
            &id.to_string(),
        )
    }
    pub fn preventive_interventions(&self) -> Result<Vec<PreventiveIntervention>> {
        self.list("SELECT data FROM preventive_interventions ORDER BY id")
    }

    pub fn record_preventive_counterfactual(
        &self,
        forecast_id: &FailureForecastId,
        intervention_id: &PreventiveInterventionId,
        control: crate::causal::TrialRef,
        intervention_trial: crate::causal::TrialRef,
    ) -> Result<PreventiveCounterfactual> {
        let forecast = self.forecast(forecast_id)?;
        let mut intervention = self.preventive_intervention(intervention_id)?;
        if intervention.signature != forecast.signature
            || control.experiment != intervention_trial.experiment
            || control.candidate == intervention_trial.candidate
        {
            return Err(Error::InvalidInput(
                "Prevention evidence requires distinct arms of one matching controlled experiment"
                    .into(),
            ));
        }
        let experiment = self.strategy_experiment(&control.experiment)?;
        if experiment.status != ExperimentStatus::Completed {
            return Err(Error::InvalidInput(
                "Preventive counterfactual requires a completed Experiment".into(),
            ));
        }
        let result = experiment
            .result
            .as_ref()
            .ok_or_else(|| Error::InvalidInput("Experiment result missing".into()))?;
        let control_result = result
            .candidates
            .iter()
            .find(|item| {
                item.candidate_id == control.candidate && item.experience_id == control.experience
            })
            .ok_or_else(|| {
                Error::InvalidInput("Control TrialRef is not in the Experiment".into())
            })?;
        let intervention_result = result
            .candidates
            .iter()
            .find(|item| {
                item.candidate_id == intervention_trial.candidate
                    && item.experience_id == intervention_trial.experience
            })
            .ok_or_else(|| {
                Error::InvalidInput("Intervention TrialRef is not in the Experiment".into())
            })?;
        let controlled = result.quality == ExperimentQuality::Controlled
            && result.starting_state.as_ref().is_some_and(|proof| {
                control_result.starting_fingerprint == proof.fingerprint
                    && intervention_result.starting_fingerprint == proof.fingerprint
            });
        let result_kind = if !controlled {
            PreventiveEvidenceOutcome::Inconclusive
        } else if !control_result.evaluation.success && intervention_result.evaluation.success {
            PreventiveEvidenceOutcome::AvoidedFailure
        } else if control_result.evaluation.success && !intervention_result.evaluation.success {
            PreventiveEvidenceOutcome::Harmful
        } else if !intervention_result.evaluation.success {
            PreventiveEvidenceOutcome::NoEffect
        } else {
            PreventiveEvidenceOutcome::Inconclusive
        };
        let pair = PreventiveCounterfactual {
            id: PreventiveCounterfactualId::new(),
            forecast: forecast_id.clone(),
            intervention: intervention_id.clone(),
            control,
            intervention_trial,
            result: result_kind,
            starting_fingerprint: result
                .starting_state
                .as_ref()
                .map(|proof| proof.fingerprint.clone())
                .unwrap_or_default(),
            created_at: Utc::now(),
        };
        let prior: i64 = self.connection.query_row(
            "SELECT COUNT(DISTINCT experiment_id) FROM preventive_counterfactuals WHERE intervention_id=?1 AND json_extract(data,'$.result')='avoided_failure'",
            [intervention_id.to_string()], |row| row.get(0))?;
        let repeated_experiment: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM preventive_counterfactuals WHERE intervention_id=?1 AND experiment_id=?2)",
            params![intervention_id.to_string(), pair.control.experiment.to_string()],
            |row| row.get(0),
        )?;
        intervention.status = match pair.result {
            PreventiveEvidenceOutcome::AvoidedFailure if prior >= 1 && !repeated_experiment => {
                PreventiveInterventionStatus::Validated
            }
            PreventiveEvidenceOutcome::AvoidedFailure => PreventiveInterventionStatus::Supported,
            PreventiveEvidenceOutcome::Harmful => PreventiveInterventionStatus::Contradicted,
            _ => intervention.status,
        };
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO preventive_counterfactuals(id,forecast_id,intervention_id,experiment_id,created_at,data) VALUES(?1,?2,?3,?4,?5,?6)",
            params![pair.id.to_string(), forecast_id.to_string(), intervention_id.to_string(), pair.control.experiment.to_string(), pair.created_at.to_rfc3339(), data(&pair)?],
        )?;
        tx.execute(
            "UPDATE preventive_interventions SET status=?2,data=?3 WHERE id=?1",
            params![
                intervention_id.to_string(),
                name(&intervention.status)?,
                data(&intervention)?
            ],
        )?;
        event(
            &tx,
            &intervention_id.to_string(),
            "preventive_counterfactual_recorded",
            serde_json::json!({"result":pair.result}),
        )?;
        tx.commit()?;
        Ok(pair)
    }

    pub fn record_forecast_feedback(
        &self,
        mut feedback: ForecastFeedback,
    ) -> Result<ForecastFeedback> {
        let mut forecast = self.forecast(&feedback.forecast_id)?;
        if !forecast.status.is_active() {
            return Err(Error::InvalidInput(
                "Feedback must resolve one active forecast exactly once".into(),
            ));
        }
        feedback.status = match feedback.outcome {
            ForecastOutcome::FailureMaterialized => ForecastStatus::Materialized,
            ForecastOutcome::FailureAvoided | ForecastOutcome::ClearedWithoutIntervention => {
                ForecastStatus::Cleared
            }
            ForecastOutcome::FalsePositive => ForecastStatus::FalsePositive,
            ForecastOutcome::Inconclusive | ForecastOutcome::InterventionWindowMissed => {
                ForecastStatus::Inconclusive
            }
        };
        if feedback.warning_lead_actions.is_none()
            && matches!(feedback.outcome, ForecastOutcome::FailureMaterialized)
        {
            let trajectory = self.trajectory(&forecast.trajectory_id)?;
            feedback.warning_lead_actions = u64::try_from(trajectory.events.len())
                .ok()
                .map(|end| end.saturating_sub(forecast.warning_sequence + 1));
        }
        if feedback.lead.event_distance.is_none() {
            feedback.lead.event_distance = feedback
                .warning_lead_actions
                .and_then(|value| u32::try_from(value).ok());
        }
        forecast.status = feedback.status;
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO forecast_feedback(id,forecast_id,status,created_at,data) VALUES(?1,?2,?3,?4,?5)", params![feedback.id.to_string(), feedback.forecast_id.to_string(), name(&feedback.status)?, feedback.created_at.to_rfc3339(), data(&feedback)?])?;
        tx.execute(
            "UPDATE failure_forecasts SET status=?2,data=?3 WHERE id=?1 AND status IN ('watch','elevated','actionable','imminent','active')",
            params![
                forecast.id.to_string(),
                name(&forecast.status)?,
                data(&forecast)?
            ],
        )?;
        event(
            &tx,
            &forecast.id.to_string(),
            "forecast_resolved",
            serde_json::json!({"status":feedback.status}),
        )?;
        let lifecycle_event = match feedback.outcome {
            ForecastOutcome::FailureMaterialized => "forecast_materialized",
            ForecastOutcome::FalsePositive => "forecast_false_positive",
            ForecastOutcome::FailureAvoided | ForecastOutcome::ClearedWithoutIntervention => {
                "failure_forecast_cleared"
            }
            ForecastOutcome::InterventionWindowMissed => "intervention_window_missed",
            ForecastOutcome::Inconclusive => "forecast_inconclusive",
        };
        event(
            &tx,
            &forecast.id.to_string(),
            lifecycle_event,
            serde_json::json!({"outcome":feedback.outcome}),
        )?;
        event(
            &tx,
            &forecast.id.to_string(),
            "failure_forecast_updated",
            serde_json::json!({"status":feedback.status}),
        )?;
        if let Some(intervention) = &feedback.intervention {
            event(
                &tx,
                &intervention.to_string(),
                "preventive_intervention_applied",
                serde_json::json!({"forecast":forecast.id,"status":feedback.status}),
            )?;
        }
        tx.commit()?;
        Ok(feedback)
    }

    pub fn record_forecast_miss(
        &self,
        trajectory_id: &TrajectoryId,
        observability_gap: Option<String>,
    ) -> Result<ForecastMiss> {
        if let Some(existing) = self
            .forecast_misses()?
            .into_iter()
            .find(|item| &item.trajectory == trajectory_id)
        {
            return Ok(existing);
        }
        let trajectory = self.trajectory(trajectory_id)?;
        let Some(TrajectoryOutcome::Failure(failure)) = trajectory.outcome else {
            return Err(Error::InvalidInput(
                "A forecast miss requires a completed failure trajectory".into(),
            ));
        };
        let active: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM failure_forecasts WHERE trajectory_id=?1",
            [trajectory_id.to_string()],
            |row| row.get(0),
        )?;
        if active > 0 {
            return Err(Error::InvalidInput(
                "The trajectory had a forecast and is not a miss".into(),
            ));
        }
        let forecastability =
            if trajectory.context.observability.is_empty() || observability_gap.is_some() {
                Forecastability::InsufficientObservability
            } else {
                Forecastability::NotYetPredictable
            };
        let miss = ForecastMiss {
            trajectory: trajectory_id.clone(),
            failure,
            forecastability,
            observability_gap,
            created_at: Utc::now(),
        };
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO forecast_misses(trajectory_id,failure_class,forecastability,created_at,data) VALUES(?1,?2,?3,?4,?5)", params![trajectory_id.to_string(), miss.failure.signature, name(&miss.forecastability)?, miss.created_at.to_rfc3339(), data(&miss)?])?;
        event(
            &tx,
            &trajectory_id.to_string(),
            "forecast_missed",
            serde_json::json!({"failure":miss.failure.signature,"forecastability":miss.forecastability}),
        )?;
        tx.commit()?;
        let sequence = trajectory
            .points
            .iter()
            .filter(|point| point.event != TrajectoryEventKind::FailureObserved)
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|point| TrajectoryPatternStep {
                event_pattern: Some(EventPattern {
                    kind: point.event.clone(),
                }),
                state_predicates: point
                    .state
                    .variables
                    .iter()
                    .filter(|(feature, _)| !matches!(feature.as_str(), "duration_ms" | "success"))
                    .take(4)
                    .map(|(feature, value)| FeaturePredicate {
                        feature: feature.clone(),
                        operator: ComparisonOperator::Equals,
                        value: value.clone(),
                    })
                    .collect(),
                causal_relevance: None,
                temporal: None,
                condition: None,
            })
            .collect::<Vec<_>>();
        if !sequence.is_empty() {
            self.register_failure_trajectory(FailureTrajectory {
                id: FailureTrajectoryId::new(),
                failure_signature: miss.failure.clone(),
                causal_model: None,
                sequence,
                scope: trajectory.context.scope,
                evidence: vec![TrajectoryEvidenceRef::Trajectory(trajectory.id)],
                status: FailureTrajectoryStatus::Candidate,
                origin: PredictiveOrigin::Local,
                required_runtime_version: trajectory.context.runtime_version,
                revision: 1,
            })?;
        }
        Ok(miss)
    }

    pub fn forecast_misses(&self) -> Result<Vec<ForecastMiss>> {
        self.list("SELECT data FROM forecast_misses ORDER BY created_at,trajectory_id")
    }

    pub fn forecast_quality(&self) -> Result<ForecastQualitySummary> {
        let feedback: Vec<ForecastFeedback> =
            self.list("SELECT data FROM forecast_feedback ORDER BY created_at,id")?;
        let misses = self.forecast_misses()?;
        let resolved = feedback.len();
        let correct = feedback
            .iter()
            .filter(|item| {
                matches!(
                    item.outcome,
                    ForecastOutcome::FailureMaterialized | ForecastOutcome::FailureAvoided
                )
            })
            .count();
        let false_alarms = feedback
            .iter()
            .filter(|item| item.outcome == ForecastOutcome::FalsePositive)
            .count();
        let failures_observed = correct + misses.len();
        let interventions: Vec<_> = feedback
            .iter()
            .filter(|item| item.intervention.is_some())
            .collect();
        let unnecessary = interventions
            .iter()
            .filter(|item| {
                matches!(
                    item.outcome,
                    ForecastOutcome::FalsePositive | ForecastOutcome::ClearedWithoutIntervention
                )
            })
            .count();
        let preventive_pairs: Vec<PreventiveCounterfactual> =
            self.list("SELECT data FROM preventive_counterfactuals ORDER BY created_at,id")?;
        let counterfactual_avoided = preventive_pairs
            .iter()
            .filter(|item| item.result == PreventiveEvidenceOutcome::AvoidedFailure)
            .count();
        let counterfactual_unnecessary = preventive_pairs
            .iter()
            .filter(|item| item.result == PreventiveEvidenceOutcome::NoEffect)
            .count();
        let ratio = |numerator: usize, denominator: usize| {
            (denominator > 0).then_some(numerator as f64 / denominator as f64)
        };
        let mut leads: Vec<_> = feedback
            .iter()
            .filter_map(|item| item.warning_lead_actions)
            .collect();
        leads.sort_unstable();
        let median = if leads.is_empty() {
            None
        } else if leads.len() % 2 == 1 {
            Some(leads[leads.len() / 2] as f64)
        } else {
            Some((leads[leads.len() / 2 - 1] + leads[leads.len() / 2]) as f64 / 2.0)
        };
        let sufficient = resolved >= 2;
        Ok(ForecastQualitySummary {
            resolved_forecasts: resolved,
            failures_observed,
            forecasted_failures: correct,
            precision: sufficient
                .then(|| ratio(correct, correct + false_alarms))
                .flatten(),
            recall: (failures_observed >= 2)
                .then(|| ratio(correct, failures_observed))
                .flatten(),
            false_positive_rate: sufficient.then(|| ratio(false_alarms, resolved)).flatten(),
            false_negative_rate: (failures_observed >= 2)
                .then(|| ratio(misses.len(), failures_observed))
                .flatten(),
            median_warning_lead_actions: sufficient.then_some(median).flatten(),
            avoided_failure_rate: (preventive_pairs.len() >= 2)
                .then(|| ratio(counterfactual_avoided, preventive_pairs.len()))
                .flatten(),
            unnecessary_preventive_intervention_rate: if interventions.len() >= 2 {
                ratio(unnecessary, interventions.len())
            } else if preventive_pairs.len() >= 2 {
                ratio(counterfactual_unnecessary, preventive_pairs.len())
            } else {
                None
            },
            sufficient_samples: sufficient,
        })
    }

    pub fn forecast_health(
        &self,
        id: &EarlyWarningSignatureId,
    ) -> Result<ForecastHealthAssessment> {
        let signature = self.warning_signature(id)?;
        if signature.status == RiskIndicatorStatus::Contradicted {
            return Ok(ForecastHealthAssessment {
                signature: id.clone(),
                health: ForecastHealth::Contradicted,
                reason: "Negative controls or feedback contradict the warning".into(),
                revalidation_recommended: true,
            });
        }
        if matches!(
            signature.status,
            RiskIndicatorStatus::Noisy | RiskIndicatorStatus::Stale
        ) {
            return Ok(ForecastHealthAssessment {
                signature: id.clone(),
                health: if signature.status == RiskIndicatorStatus::Stale {
                    ForecastHealth::Stale
                } else {
                    ForecastHealth::Degrading
                },
                reason: format!(
                    "Warning lifecycle is {:?}; revalidation is required",
                    signature.status
                ),
                revalidation_recommended: true,
            });
        }
        let mut statement=self.connection.prepare("SELECT ff.data FROM forecast_feedback ff JOIN failure_forecasts f ON f.id=ff.forecast_id WHERE f.signature_id=?1 ORDER BY ff.created_at DESC LIMIT 20")?;
        let feedback: Vec<ForecastFeedback> = statement
            .query_map([id.to_string()], |row| row.get::<_, String>(0))?
            .map(|item| Ok(serde_json::from_str(&item?)?))
            .collect::<Result<_>>()?;
        let false_alarms = feedback
            .iter()
            .filter(|item| item.outcome == ForecastOutcome::FalsePositive)
            .count();
        let degrading = feedback.len() >= 2 && false_alarms * 4 > feedback.len();
        Ok(ForecastHealthAssessment {
            signature: id.clone(),
            health: if degrading {
                ForecastHealth::Degrading
            } else {
                ForecastHealth::Healthy
            },
            reason: if degrading {
                format!(
                    "{false_alarms} false alarms across {} recent resolved forecasts",
                    feedback.len()
                )
            } else {
                "No material deterministic degradation observed".into()
            },
            revalidation_recommended: degrading,
        })
    }

    pub fn forecast_health_for_context(
        &self,
        id: &EarlyWarningSignatureId,
        context: &TrajectoryContext,
    ) -> Result<ForecastHealthAssessment> {
        let signature = self.warning_signature(id)?;
        if signature
            .required_runtime_version
            .as_ref()
            .is_some_and(|version| context.runtime_version.as_ref() != Some(version))
        {
            return Ok(ForecastHealthAssessment {
                signature: id.clone(),
                health: ForecastHealth::Stale,
                reason: format!(
                    "Signature revision {} is scoped to runtime {:?}; observed {:?}",
                    signature.revision, signature.required_runtime_version, context.runtime_version
                ),
                revalidation_recommended: true,
            });
        }
        self.forecast_health(id)
    }

    pub fn forecastability(&self, failure: &str) -> Result<Forecastability> {
        let signatures: Vec<_> = self
            .warning_signatures()?
            .into_iter()
            .filter(|item| {
                item.failure.signature == failure && item.origin == PredictiveOrigin::Local
            })
            .collect();
        if signatures
            .iter()
            .any(|item| item.status == RiskIndicatorStatus::Validated)
        {
            return Ok(Forecastability::Predictable);
        }
        if signatures
            .iter()
            .any(|item| item.status == RiskIndicatorStatus::Supported)
        {
            return Ok(Forecastability::WeaklyPredictable);
        }
        let misses: Vec<_> = self
            .forecast_misses()?
            .into_iter()
            .filter(|item| item.failure.signature == failure)
            .collect();
        if !misses.is_empty()
            && misses
                .iter()
                .all(|item| item.forecastability == Forecastability::InsufficientObservability)
        {
            Ok(Forecastability::InsufficientObservability)
        } else {
            Ok(Forecastability::NotYetPredictable)
        }
    }

    pub fn predictive_summary(&self) -> Result<PredictiveExperienceSummary> {
        let signatures = self.warning_signatures()?;
        Ok(PredictiveExperienceSummary {
            early_warning_signatures: signatures.len(),
            validated_signatures: signatures
                .iter()
                .filter(|item| item.status == RiskIndicatorStatus::Validated)
                .count(),
            forecast_quality: self.forecast_quality()?,
            validated_preventive_interventions: self
                .preventive_interventions()?
                .iter()
                .filter(|item| item.status == PreventiveInterventionStatus::Validated)
                .count(),
        })
    }

    pub fn predictive_impact(&self, id: &EarlyWarningSignatureId) -> Result<serde_json::Value> {
        let forecasts: Vec<_> = self
            .forecasts()?
            .into_iter()
            .filter(|item| &item.signature == id)
            .collect();
        let feedback: Vec<ForecastFeedback> =
            self.list("SELECT data FROM forecast_feedback ORDER BY created_at,id")?;
        let ids: BTreeSet<_> = forecasts.iter().map(|item| &item.id).collect();
        let relevant: Vec<_> = feedback
            .iter()
            .filter(|item| ids.contains(&item.forecast_id))
            .collect();
        Ok(
            serde_json::json!({"signature":self.warning_signature(id)?,"forecasts_emitted":forecasts.len(),"runtime_decisions_influenced":0,"preventive_interventions":relevant.iter().filter(|item|item.intervention.is_some()).count(),"avoided_failures":relevant.iter().filter(|item|item.status==ForecastStatus::ResolvedAvoided).count(),"false_alarms":relevant.iter().filter(|item|item.status==ForecastStatus::ResolvedFalseAlarm).count(),"inconclusive":relevant.iter().filter(|item|item.status==ForecastStatus::Inconclusive).count()}),
        )
    }

    pub fn forecast_provenance(&self, id: &FailureForecastId) -> Result<serde_json::Value> {
        let forecast = self.forecast(id)?;
        let signature = self.warning_signature(&forecast.signature)?;
        let failure_trajectory = forecast
            .matched_trajectory
            .as_ref()
            .map(|id| self.failure_trajectory(id))
            .transpose()?;
        let interventions = self
            .preventive_interventions()?
            .into_iter()
            .filter(|item| item.signature == signature.id)
            .collect::<Vec<_>>();
        let intervention_ids = interventions
            .iter()
            .map(|item| item.id.clone())
            .collect::<BTreeSet<_>>();
        let counterfactuals: Vec<PreventiveCounterfactual> =
            self.list("SELECT data FROM preventive_counterfactuals ORDER BY created_at,id")?;
        Ok(serde_json::json!({
            "forecast":forecast,
            "early_warning_signature":signature,
            "failure_trajectory":failure_trajectory,
            "historical_failures":forecast.historical_matches,
            "causal_hypotheses":forecast.causal_basis,
            "preventive_interventions":interventions,
            "counterfactual_evidence":counterfactuals.into_iter().filter(|pair|intervention_ids.contains(&pair.intervention)).collect::<Vec<_>>()
        }))
    }

    pub fn predictive_curriculum_goals(&self) -> Result<Vec<crate::curriculum::CurriculumGoal>> {
        use crate::curriculum::*;
        let mut goals = Vec::new();
        for miss in self.forecast_misses()? {
            if miss.forecastability == Forecastability::InsufficientObservability {
                continue;
            }
            goals.push(CurriculumGoal {
                id: CurriculumGoalId::new(),
                kind: CurriculumGoalKind::DiscoverEarlyWarning,
                description: format!(
                    "Discover an observable precursor for {}",
                    miss.failure.signature
                ),
                priority: Priority::High,
                score: PriorityScore {
                    score: 80,
                    priority: Priority::High,
                    explanation: "Observed failure had no active forecast".into(),
                },
                evidence_gap: EvidenceGap {
                    dimension: "trajectory precursor".into(),
                    known_values: vec![miss.trajectory.to_string()],
                    unknown_values: vec!["validated early warning".into()],
                    rationale: "Compare failed trajectories with successful negative controls"
                        .into(),
                },
                status: GoalStatus::Planned,
                decision: CurriculumDecision::RequiresApproval,
                reason: "Recommendation only; no automatic predictor creation or trial execution"
                    .into(),
                severity: Severity::High,
                safety: TrialSafety::Safe,
            });
        }
        for signature in self.warning_signatures()? {
            if signature.origin == PredictiveOrigin::Local
                && signature.status == RiskIndicatorStatus::Candidate
            {
                goals.push(CurriculumGoal {
                    id:CurriculumGoalId::new(),kind:CurriculumGoalKind::ValidateEarlyWarning,
                    description:format!("Validate {} on positive, near-boundary, and held-out negative trajectories",signature.id),
                    priority:Priority::Medium,score:PriorityScore{score:65,priority:Priority::Medium,explanation:"Candidate warning cannot affect runtime until hardened".into()},
                    evidence_gap:EvidenceGap{dimension:"prospective warning validation".into(),known_values:signature.evidence.iter().map(|item|format!("{item:?}")).collect(),unknown_values:vec!["held-out precision, recall, and lead".into()],rationale:"Historical precursor discovery is not prospective validation".into()},
                    status:GoalStatus::Planned,decision:CurriculumDecision::RequiresApproval,reason:"No automatic experiment or predictor promotion".into(),severity:Severity::Medium,safety:TrialSafety::Safe,
                });
            }
            let health = self.forecast_health(&signature.id)?;
            if health.revalidation_recommended {
                goals.push(CurriculumGoal {
                    id:CurriculumGoalId::new(), kind:CurriculumGoalKind::ReduceForecastFalsePositives,
                    description:format!("Revalidate or refine {} against fresh negative controls",signature.id),
                    priority:Priority::High,score:PriorityScore{score:85,priority:Priority::High,explanation:health.reason.clone()},
                    evidence_gap:EvidenceGap{dimension:"forecast calibration".into(),known_values:vec![format!("health={:?}",health.health)],unknown_values:vec!["fresh precision and false-positive rate".into()],rationale:"Predictor health degraded or became stale".into()},
                    status:GoalStatus::Planned,decision:CurriculumDecision::RequiresApproval,reason:"Produce a revision candidate, then validate it; runtime does not self-modify predictors".into(),severity:Severity::High,safety:TrialSafety::Safe,
                });
            }
        }
        for intervention in self.preventive_interventions()? {
            if intervention.status == PreventiveInterventionStatus::Supported {
                goals.push(CurriculumGoal {
                    id:CurriculumGoalId::new(),kind:CurriculumGoalKind::ValidatePreventiveIntervention,
                    description:format!("Replicate preventive intervention {} in an independent controlled Experiment",intervention.id),
                    priority:Priority::High,score:PriorityScore{score:75,priority:Priority::High,explanation:"One controlled avoided-failure pair supports but does not validate prevention".into()},
                    evidence_gap:EvidenceGap{dimension:"preventive replication".into(),known_values:vec![intervention.id.to_string()],unknown_values:vec!["independent same-start replication".into()],rationale:"Prevention can itself cause failures and needs replication".into()},
                    status:GoalStatus::Planned,decision:CurriculumDecision::RequiresApproval,reason:"Recommendation only; effect authority and trial budget remain external".into(),severity:Severity::High,safety:TrialSafety::Safe,
                });
            }
        }
        Ok(goals)
    }
}

impl TrajectoryStore for Store {
    fn append_point(
        &self,
        trajectory: &TrajectoryId,
        event_kind: TrajectoryEventKind,
        state: StateObservation,
        evidence: Vec<TrajectoryEvidenceRef>,
    ) -> Result<TrajectoryPoint> {
        self.append_trajectory_event(
            trajectory,
            NewTrajectoryEvent {
                kind: event_kind,
                observation: TrajectoryObservation {
                    features: state.variables,
                },
                evidence,
            },
        )?;
        self.trajectory(trajectory)?
            .points
            .last()
            .cloned()
            .ok_or_else(|| Error::InvalidInput("Appended trajectory point is missing".into()))
    }

    fn get(&self, trajectory: &TrajectoryId) -> Result<ExecutionTrajectory> {
        self.trajectory(trajectory)
    }

    fn list(&self) -> Result<Vec<ExecutionTrajectory>> {
        self.trajectories()
    }

    fn find_by_failure(
        &self,
        failure: &crate::runtime::FailureSignatureRef,
    ) -> Result<Vec<ExecutionTrajectory>> {
        Ok(self
            .trajectories()?
            .into_iter()
            .filter(|trajectory| {
                matches!(&trajectory.outcome, Some(TrajectoryOutcome::Failure(observed)) if observed == failure)
            })
            .collect())
    }
}
