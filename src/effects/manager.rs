// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{
    Error, Result,
    application::ExperienceRelation,
    capability::{
        CapabilityDecision, CapabilityEvent, CapabilityEventKind, CapabilityPolicy,
        CapabilityRequest, DenyByDefaultCapabilityPolicy, EffectCapabilityStage,
    },
    core::{
        ActionRecord, AgentIdentity, ArtifactKind, CommandSpec, EffectGroupId, EffectId,
        EffectPlanId, EnvironmentMode, ExecutionId, ExecutionRecord, ExperienceId, ProcessStatus,
        Reality, RealityId, RealityStatus, ReconciliationAttemptId, StateRef,
    },
    evaluation::{CheckResult, CheckStatus, Evaluation, EvaluationSpec, EvaluationStatus},
    experience::{
        EnvironmentContext, EvidenceBundle, Experience, ExperienceContext, Outcome,
        RepositoryContext,
    },
    store::{CapabilityStore, EffectStore, ExperienceStore, Store, artifact},
};
use chrono::Utc;
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, sync::Arc};

pub struct EffectManager<'a> {
    pub registry: EffectAdapterRegistry,
    pub policy: Arc<dyn EffectPolicy>,
    pub gate: Arc<dyn CommitGate>,
    pub store: &'a Store,
}

impl EffectManager<'_> {
    fn record_effect_experience(
        &self,
        effect: &Effect,
        kind: &str,
        success: bool,
        evidence: Value,
    ) -> Result<ExperienceId> {
        let directory = self.store.home.join("artifacts").join(format!(
            "effect-{}-{}",
            effect.id,
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&directory)?;
        let stdout_path = directory.join("receipt.json");
        let stderr_path = directory.join("stderr.txt");
        let diff_path = directory.join("external-state.json");
        let check_stdout_path = directory.join("verification.txt");
        let check_stderr_path = directory.join("verification-stderr.txt");
        fs::write(&stdout_path, serde_json::to_vec_pretty(&evidence)?)?;
        fs::write(&stderr_path, [])?;
        fs::write(
            &diff_path,
            serde_json::to_vec_pretty(&json!({
                "target":effect.target,
                "operation":effect.operation,
                "authoritative":true,
                "experience_kind":kind
            }))?,
        )?;
        fs::write(
            &check_stdout_path,
            if success {
                b"authoritative effect evidence recorded\n".as_slice()
            } else {
                b"authoritative effect operation did not fully succeed\n".as_slice()
            },
        )?;
        fs::write(&check_stderr_path, [])?;
        let scope_hash = effect.scope_hash()?;
        let starting_state = StateRef {
            repo_path: self.store.home.join("effects"),
            git_commit: scope_hash.clone(),
            tree_hash: blake3::hash(&serde_json::to_vec(&evidence)?)
                .to_hex()
                .to_string(),
        };
        let reality = Reality {
            id: RealityId::new(),
            parent: None,
            fork_reason: None,
            experiment_id: None,
            candidate_id: None,
            effect_ledger: None,
            execution_boundary: Default::default(),
            root: self.store.home.join("effects"),
            starting_state: starting_state.clone(),
            created_at: Utc::now(),
            status: if success {
                RealityStatus::Completed
            } else {
                RealityStatus::Failed
            },
            ephemeral: false,
        };
        self.store.insert_reality(&reality)?;
        let agent = AgentIdentity {
            kind: format!("effect-adapter:{}", effect.adapter),
            executable: "hardknock-effect-manager".into(),
            version: Some(env!("CARGO_PKG_VERSION").into()),
            model: None,
        };
        let action = ActionRecord {
            command: CommandSpec {
                program: "hardknock-effect-manager".into(),
                args: vec![kind.into(), effect.id.to_string()],
                environment: EnvironmentMode::Controlled,
                environment_overrides: Default::default(),
            },
            cwd: reality.root.clone(),
            started_at: Utc::now(),
            duration_ms: 0,
            exit_code: Some(if success { 0 } else { 1 }),
            signal: None,
            stdout: artifact(&stdout_path)?.with_kind(ArtifactKind::Stdout),
            stderr: artifact(&stderr_path)?.with_kind(ArtifactKind::Stderr),
        };
        let execution = ExecutionRecord {
            id: ExecutionId::new(),
            reality_id: reality.id.clone(),
            starting_state: starting_state.clone(),
            task: format!("Authoritative external effect {kind} {}", effect.id),
            agent: agent.clone(),
            status: if success {
                ProcessStatus::Succeeded
            } else {
                ProcessStatus::Failed
            },
            action: action.clone(),
            diff: artifact(&diff_path)?.with_kind(ArtifactKind::Diff),
        };
        self.store.insert_execution(&execution)?;
        let check_action = ActionRecord {
            command: CommandSpec::shell(
                "verify external effect receipt",
                EnvironmentMode::Controlled,
            ),
            cwd: reality.root.clone(),
            started_at: Utc::now(),
            duration_ms: 0,
            exit_code: Some(if success { 0 } else { 1 }),
            signal: None,
            stdout: artifact(&check_stdout_path)?.with_kind(ArtifactKind::EvaluationOutput),
            stderr: artifact(&check_stderr_path)?.with_kind(ArtifactKind::EvaluationOutput),
        };
        let evaluation = Evaluation {
            id: crate::core::EvaluationId::new(),
            spec: EvaluationSpec {
                checks: vec!["verify external effect receipt".into()],
            },
            status: EvaluationStatus::Completed,
            success,
            checks: vec![CheckResult {
                name: "effect-evidence".into(),
                command: "verify external effect receipt".into(),
                status: if success {
                    CheckStatus::Passed
                } else {
                    CheckStatus::Failed
                },
                action: Some(check_action.clone()),
            }],
            summary: if success {
                "Authoritative external effect and receipt observed".into()
            } else {
                "External compensation outcome requires review".into()
            },
        };
        let related = match kind {
            "compensation" => self
                .store
                .effect_experience_links(&effect.id, Some("commit"))?
                .into_iter()
                .next()
                .map(ExperienceRelation::CompensationOf),
            "reconciliation" => self
                .store
                .effect_experience_links(&effect.id, Some("experimental_candidate"))?
                .into_iter()
                .next()
                .map(ExperienceRelation::ReconciliationOf),
            _ => self
                .store
                .effect_experience_links(&effect.id, Some("experimental_candidate"))?
                .into_iter()
                .next()
                .map(ExperienceRelation::CommitOf),
        };
        let context = ExperienceContext {
            repository: RepositoryContext {
                path: self.store.home.join("effects"),
                name: "external-effects".into(),
                commit: scope_hash,
                branch: None,
            },
            environment: EnvironmentContext::capture(
                &self.store.home.join("effects"),
                EnvironmentMode::Controlled,
            )?,
            markers: Vec::new(),
            tags: vec![
                format!("effect:{kind}"),
                format!("adapter:{}", effect.adapter),
            ],
        };
        let experience = Experience {
            id: ExperienceId::new(),
            experiment: None,
            created_at: Utc::now(),
            goal: execution.task.clone(),
            context,
            starting_state,
            reality_id: reality.id,
            execution_id: execution.id,
            agent,
            actions: vec![action.clone(), check_action.clone()],
            perturbations: Vec::new(),
            outcome: if success {
                Outcome::Success
            } else {
                Outcome::Failure
            },
            evaluation,
            failure_signatures: Vec::new(),
            evidence: EvidenceBundle {
                artifacts: vec![
                    action.stdout,
                    action.stderr,
                    execution.diff,
                    check_action.stdout,
                    check_action.stderr,
                ],
                attestations: vec![],
                execution_assurance: Some(crate::capability::ExecutionAssurance {
                    reality_provider: "hardknock-host-effect-adapter".into(),
                    isolation: crate::capability::RealityProviderCapabilities::git_worktree(),
                    capability_manifest_hash: None,
                    external_effect_gating: true,
                    origin: crate::capability::ExecutionEvidenceOrigin::EffectBoundary,
                    attestation_id: None,
                }),
            },
            tags: vec!["authoritative-effect".into(), format!("effect:{kind}")],
            replay: None,
            lesson_applications: Vec::new(),
            relations: related.into_iter().collect(),
            repeated_mistakes: Vec::new(),
            observed_actions: Vec::new(),
            application_report_errors: Vec::new(),
            resilience: None,
        };
        ExperienceStore::insert(self.store, &experience)?;
        self.store
            .link_effect_experience(&effect.id, &experience.id, kind)?;
        Ok(experience.id)
    }
}
impl<'a> EffectManager<'a> {
    fn enforce_reality_capability(
        &self,
        reality_id: Option<&RealityId>,
        stage: EffectCapabilityStage,
        kind: EffectKind,
        target: &str,
        operation: &EffectOperation,
    ) -> Result<()> {
        let Some(reality_id) = reality_id else {
            return Ok(());
        };
        let reality = self.store.reality(reality_id)?;
        if reality.execution_boundary.provider != "container" {
            // Git worktrees remain explicitly cooperative. They do not gain a
            // security claim merely because Effect Manager was used.
            return Ok(());
        }
        if reality.execution_boundary.frozen {
            return Err(Error::Intervention(
                "Frozen Reality cannot request external effects".into(),
            ));
        }
        let manifest = self.store.effective_capability_manifest(reality_id)?;
        let request = CapabilityRequest::Effect {
            stage,
            kind,
            target: target.into(),
            operation: operation.clone(),
        };
        let evaluation = DenyByDefaultCapabilityPolicy.evaluate(&request, &manifest);
        self.store.append_capability_event(&CapabilityEvent {
            id: crate::core::CapabilityEventId::new(),
            reality_id: reality_id.clone(),
            manifest_id: manifest.id.clone(),
            kind: match evaluation.decision {
                CapabilityDecision::Allow => CapabilityEventKind::Allowed,
                CapabilityDecision::Deny => CapabilityEventKind::Denied,
                CapabilityDecision::RequireApproval => CapabilityEventKind::ApprovalRequired,
            },
            request: Some(request),
            reason: evaluation.reason.clone(),
            created_at: Utc::now(),
        })?;
        if evaluation.decision == CapabilityDecision::Allow {
            Ok(())
        } else {
            Err(Error::Intervention(evaluation.reason))
        }
    }

    pub fn new(store: &'a Store) -> Result<Self> {
        Ok(Self {
            registry: EffectAdapterRegistry::deterministic(&store.home)?,
            policy: Arc::new(DefaultEffectPolicy),
            gate: Arc::new(DeterministicCommitGate),
            store,
        })
    }
    pub fn user_context() -> EffectContext {
        EffectContext {
            actor: "local-user".into(),
            is_agent: false,
            capabilities: EffectCapabilityGrant {
                observe: true,
                propose: true,
                prepare: true,
                commit: true,
                compensate: true,
            },
        }
    }
    pub fn agent_context(agent: &str) -> EffectContext {
        EffectContext {
            actor: agent.into(),
            is_agent: true,
            capabilities: EffectCapabilityGrant {
                observe: true,
                propose: true,
                prepare: true,
                commit: false,
                compensate: false,
            },
        }
    }
    pub fn propose(&self, request: EffectRequest, context: &EffectContext) -> Result<Effect> {
        request.validate()?;
        if !context.capabilities.propose {
            return Err(Error::Intervention(
                "Actor lacks effect proposal capability".into(),
            ));
        }
        self.enforce_reality_capability(
            request.reality_id.as_ref(),
            EffectCapabilityStage::Propose,
            request.kind,
            &request.target.uri,
            &request.operation,
        )?;
        let adapter = self.registry.select_request(&request)?;
        let classification = adapter.classify(&request)?;
        let ledger = self
            .store
            .ensure_effect_ledger(request.reality_id.as_ref())?;
        let mut effect = Effect::from_request(request, ledger.id, adapter.name().into());
        self.store.insert_effect(&effect)?;
        effect.classification = Some(classification);
        let classification_metadata = json!({"classification":effect.classification});
        self.store.transition_effect(
            &mut effect,
            EffectLifecycle::Classified,
            EffectEventType::Classified,
            classification_metadata,
        )?;
        let decision = self.policy.evaluate(&effect, context);
        if decision == EffectDecision::Reject {
            self.store.transition_effect(
                &mut effect,
                EffectLifecycle::Rejected,
                EffectEventType::CommitRejected,
                json!({"reason":"effect policy rejected proposal"}),
            )?;
        }
        Ok(effect)
    }
    pub fn prepare(&self, id: &EffectId, context: &EffectContext) -> Result<PreparedEffect> {
        if !context.capabilities.prepare {
            return Err(Error::Intervention(
                "Actor lacks effect preparation capability".into(),
            ));
        }
        let mut effect = self.store.effect(id)?;
        self.enforce_reality_capability(
            effect.reality_id.as_ref(),
            EffectCapabilityStage::Prepare,
            effect.kind,
            &effect.target.uri,
            &effect.operation,
        )?;
        if effect.lifecycle != EffectLifecycle::Classified
            && effect.lifecycle != EffectLifecycle::Virtualized
        {
            return Err(Error::InvalidInput(format!(
                "Effect {} is {:?}, not eligible for prepare",
                effect.id, effect.lifecycle
            )));
        }
        let decision = self.policy.evaluate(&effect, context);
        if decision == EffectDecision::Reject {
            self.store.transition_effect(
                &mut effect,
                EffectLifecycle::Rejected,
                EffectEventType::CommitRejected,
                json!({"reason":"effect policy rejected prepare"}),
            )?;
            return Err(Error::Intervention("Effect policy rejected prepare".into()));
        }
        let adapter = self.registry.select_effect(&effect)?;
        if !adapter.capabilities().prepare {
            return Err(Error::Intervention(format!(
                "Adapter {} cannot prepare effects",
                adapter.name()
            )));
        }
        match adapter.prepare(&effect) {
            Ok(prepared) => {
                self.store.save_prepared_effect(&mut effect, &prepared)?;
                Ok(prepared)
            }
            Err(error) => {
                self.store.transition_effect(
                    &mut effect,
                    EffectLifecycle::Failed,
                    EffectEventType::CommitFailed,
                    json!({"phase":"prepare","error":error.to_string()}),
                )?;
                Err(error)
            }
        }
    }
    pub fn propose_and_prepare(
        &self,
        request: EffectRequest,
        context: &EffectContext,
    ) -> Result<(Effect, PreparedEffect)> {
        let effect = self.propose(request, context)?;
        if effect.lifecycle == EffectLifecycle::Rejected {
            return Err(Error::Intervention("Effect proposal was rejected".into()));
        }
        let prepared = self.prepare(&effect.id, context)?;
        Ok((self.store.effect(&effect.id)?, prepared))
    }
    pub fn authorize(
        &self,
        authority: CommitAuthority,
        effect_ids: &[EffectId],
    ) -> Result<CommitAuthorization> {
        let effects = effect_ids
            .iter()
            .map(|id| self.store.effect(id))
            .collect::<Result<Vec<_>>>()?;
        let authorization = CommitAuthorization::for_effects(authority, &effects)?;
        self.store.insert_commit_authorization(&authorization)?;
        Ok(authorization)
    }
    pub fn commit(
        &self,
        id: &EffectId,
        authorization: Option<&CommitAuthorization>,
        context: &EffectContext,
    ) -> Result<CommitOutcome> {
        let effect = self.store.effect(id)?;
        self.commit_internal(effect, authorization, context, None)
    }
    fn commit_internal(
        &self,
        mut effect: Effect,
        authorization: Option<&CommitAuthorization>,
        context: &EffectContext,
        authorization_scope: Option<&[Effect]>,
    ) -> Result<CommitOutcome> {
        if context.is_agent {
            self.enforce_reality_capability(
                effect.reality_id.as_ref(),
                EffectCapabilityStage::Commit,
                effect.kind,
                &effect.target.uri,
                &effect.operation,
            )?;
        }
        if context.is_agent || !context.capabilities.commit {
            self.store.append_effect_event(
                &effect,
                EffectEventType::CommitRejected,
                json!({"reason":"agent self-authorization is not commit authority","actor":context.actor}),
            )?;
            return Err(Error::Intervention(
                "Agent cannot self-authorize an external effect commit".into(),
            ));
        }
        if !matches!(
            effect.lifecycle,
            EffectLifecycle::Prepared | EffectLifecycle::Unknown
        ) {
            return Err(Error::InvalidInput(format!(
                "Effect {} is {:?}; PREPARED or UNKNOWN required",
                effect.id, effect.lifecycle
            )));
        }
        let prepared = self.store.prepared_effect(&effect.id)?;
        let authorization = authorization.ok_or_else(|| {
            Error::Intervention("Explicit commit authorization is required".into())
        })?;
        let scope = authorization_scope.unwrap_or_else(|| std::slice::from_ref(&effect));
        if let Err(error) = authorization.validate(scope, Utc::now()) {
            self.store.append_effect_event(
                &effect,
                EffectEventType::CommitRejected,
                json!({"reason":error.to_string(),"authorization":authorization.id}),
            )?;
            return Err(error);
        }
        if !scope.iter().any(|candidate| candidate.id == effect.id) {
            return Err(Error::Intervention(
                "Authorization does not include this effect".into(),
            ));
        }
        self.store.insert_commit_authorization(authorization)?;
        let adapter = self.registry.select_effect(&effect)?;
        if effect.lifecycle == EffectLifecycle::Unknown {
            if !adapter.capabilities().idempotency_keys {
                return Err(Error::Intervention(
                    "UNKNOWN effect cannot be retried without adapter idempotency support".into(),
                ));
            }
            self.store.append_effect_event(
                &effect,
                EffectEventType::CommitAuthorized,
                json!({"authorization":authorization.id,"retry":true,"idempotency_key":effect.idempotency_key}),
            )?;
            return match adapter.commit(&effect, &prepared)? {
                AdapterCommitOutcome::Committed { receipt } => {
                    let after = adapter.observe(&effect)?;
                    self.store
                        .save_commit_receipt(&mut effect, &receipt, &after)?;
                    self.record_effect_experience(
                        &effect,
                        "commit",
                        true,
                        serde_json::to_value(&receipt)?,
                    )?;
                    Ok(CommitOutcome::Committed { receipt })
                }
                AdapterCommitOutcome::NotCommitted { reason } => {
                    self.store.transition_effect(
                        &mut effect,
                        EffectLifecycle::Failed,
                        EffectEventType::CommitFailed,
                        json!({"reason":reason,"unknown_retry":true}),
                    )?;
                    Ok(CommitOutcome::Rejected {
                        reason,
                        reprepare: false,
                    })
                }
                AdapterCommitOutcome::Unknown { reason } => {
                    self.store.append_effect_event(
                        &effect,
                        EffectEventType::Unknown,
                        json!({"reason":reason,"retry":true}),
                    )?;
                    Ok(CommitOutcome::Unknown { reason })
                }
            };
        }
        let current = adapter.observe(&effect)?;
        self.store
            .insert_external_snapshot(&effect, "pre_commit", &current)?;
        match self.gate.evaluate(&effect, &prepared, &current) {
            CommitGateDecision::Reprepare => {
                let reason = format!(
                    "External state changed after preparation (expected {:?}, observed {:?}); reprepare required",
                    prepared.before.version, current.version
                );
                self.store.append_effect_event(
                    &effect,
                    EffectEventType::CommitRejected,
                    json!({"reason":reason,"expected_fingerprint":prepared.before.fingerprint,"observed_fingerprint":current.fingerprint}),
                )?;
                return Ok(CommitOutcome::Rejected {
                    reason,
                    reprepare: true,
                });
            }
            CommitGateDecision::Reject => {
                let reason = "Commit gate rejected unsupported effect".to_owned();
                self.store.append_effect_event(
                    &effect,
                    EffectEventType::CommitRejected,
                    json!({"reason":reason}),
                )?;
                return Ok(CommitOutcome::Rejected {
                    reason,
                    reprepare: false,
                });
            }
            CommitGateDecision::RequireApproval | CommitGateDecision::Allow => {}
        }
        self.store.append_effect_event(
            &effect,
            EffectEventType::CommitAuthorized,
            json!({"authorization":authorization.id,"authority":authorization.authority,"scope_hash":authorization.scope_hash}),
        )?;
        match adapter.commit(&effect, &prepared)? {
            AdapterCommitOutcome::Committed { receipt } => {
                let after = adapter.observe(&effect)?;
                self.store
                    .save_commit_receipt(&mut effect, &receipt, &after)?;
                self.record_effect_experience(
                    &effect,
                    "commit",
                    true,
                    serde_json::to_value(&receipt)?,
                )?;
                Ok(CommitOutcome::Committed { receipt })
            }
            AdapterCommitOutcome::NotCommitted { reason } => {
                self.store.transition_effect(
                    &mut effect,
                    EffectLifecycle::Failed,
                    EffectEventType::CommitFailed,
                    json!({"reason":reason,"authoritative_mutation":false}),
                )?;
                Ok(CommitOutcome::Rejected {
                    reason,
                    reprepare: false,
                })
            }
            AdapterCommitOutcome::Unknown { reason } => {
                if effect.lifecycle != EffectLifecycle::Unknown {
                    self.store.transition_effect(
                        &mut effect,
                        EffectLifecycle::Unknown,
                        EffectEventType::Unknown,
                        json!({"reason":reason,"mutation_may_have_occurred":true}),
                    )?;
                }
                Ok(CommitOutcome::Unknown { reason })
            }
        }
    }
    pub fn reconcile(&self, id: &EffectId) -> Result<ReconciliationResult> {
        let mut effect = self.store.effect(id)?;
        if effect.lifecycle != EffectLifecycle::Unknown {
            return Err(Error::InvalidInput(
                "Only UNKNOWN effects require reconciliation".into(),
            ));
        }
        let adapter = self.registry.select_effect(&effect)?;
        let result = adapter.reconcile(&effect)?;
        let attempt = ReconciliationAttempt {
            id: ReconciliationAttemptId::new(),
            effect_id: effect.id.clone(),
            attempted_at: Utc::now(),
            result: result.clone(),
        };
        self.store.insert_reconciliation_attempt(&attempt)?;
        match &result {
            ReconciliationResult::Committed { receipt } => {
                let after = adapter.observe(&effect)?;
                self.store
                    .save_commit_receipt(&mut effect, receipt, &after)?;
                self.record_effect_experience(
                    &effect,
                    "reconciliation",
                    true,
                    serde_json::to_value(receipt)?,
                )?;
                self.store.append_effect_event(
                    &effect,
                    EffectEventType::Reconciled,
                    json!({"attempt":attempt.id,"result":"committed","receipt":receipt.id}),
                )?;
            }
            ReconciliationResult::NotCommitted => {
                self.store.transition_effect(
                    &mut effect,
                    EffectLifecycle::Failed,
                    EffectEventType::Reconciled,
                    json!({"attempt":attempt.id,"result":"not_committed"}),
                )?;
            }
            ReconciliationResult::StillUnknown { reason } => {
                self.store.append_effect_event(
                    &effect,
                    EffectEventType::Reconciled,
                    json!({"attempt":attempt.id,"result":"still_unknown","reason":reason}),
                )?;
            }
        }
        Ok(result)
    }
    pub fn discard(&self, id: &EffectId, context: &EffectContext) -> Result<Effect> {
        if !context.capabilities.prepare {
            return Err(Error::Intervention(
                "Actor lacks effect discard capability".into(),
            ));
        }
        let mut effect = self.store.effect(id)?;
        if effect.lifecycle == EffectLifecycle::Unknown {
            return Err(Error::Intervention(
                "UNKNOWN effect cannot be discarded until reconciled".into(),
            ));
        }
        if effect.lifecycle == EffectLifecycle::Prepared {
            let prepared = self.store.prepared_effect(id)?;
            let adapter = self.registry.select_effect(&effect)?;
            if let Err(error) = adapter.discard(&effect, &prepared) {
                self.store.append_effect_event(
                    &effect,
                    EffectEventType::CleanupFailed,
                    json!({"error":error.to_string(),"manual_intervention_required":true}),
                )?;
                return Err(error);
            }
        }
        self.store.transition_effect(
            &mut effect,
            EffectLifecycle::Discarded,
            EffectEventType::Discarded,
            json!({"authoritative_mutation":false}),
        )?;
        Ok(effect)
    }
    pub fn compensate(
        &self,
        id: &EffectId,
        context: &EffectContext,
    ) -> Result<CompensationReceipt> {
        if context.is_agent || !context.capabilities.compensate {
            return Err(Error::Intervention(
                "External compensation authority is required".into(),
            ));
        }
        let mut effect = self.store.effect(id)?;
        if effect.lifecycle != EffectLifecycle::Committed {
            return Err(Error::InvalidInput(
                "Only committed effects may be compensated".into(),
            ));
        }
        let receipt = self
            .store
            .commit_receipt_for_effect(id)?
            .ok_or_else(|| Error::NotFound("Committed effect receipt missing".into()))?;
        self.store.append_effect_event(
            &effect,
            EffectEventType::CompensationStarted,
            json!({"receipt":receipt.id}),
        )?;
        let adapter = self.registry.select_effect(&effect)?;
        let compensation = adapter.compensate(&effect, &receipt)?;
        self.store
            .save_compensation_receipt(&mut effect, &compensation)?;
        self.record_effect_experience(
            &effect,
            "compensation",
            compensation.status == CompensationStatus::Successful,
            serde_json::to_value(&compensation)?,
        )?;
        Ok(compensation)
    }
    pub fn discard_reality(&self, reality_id: &RealityId) -> Result<Vec<EffectId>> {
        let context = Self::user_context();
        let mut discarded = Vec::new();
        let mut failures = Vec::new();
        for effect in self.store.effects(Some(reality_id))? {
            if matches!(
                effect.lifecycle,
                EffectLifecycle::Proposed
                    | EffectLifecycle::Classified
                    | EffectLifecycle::Virtualized
                    | EffectLifecycle::Prepared
                    | EffectLifecycle::Failed
            ) {
                match self.discard(&effect.id, &context) {
                    Ok(_) => discarded.push(effect.id),
                    Err(error) => failures.push(format!("{}: {error}", effect.id)),
                }
            } else if effect.lifecycle == EffectLifecycle::Unknown {
                failures.push(format!("{}: outcome remains UNKNOWN", effect.id));
            }
        }
        if failures.is_empty() {
            Ok(discarded)
        } else {
            Err(Error::Intervention(format!(
                "Reality effect cleanup incomplete: {}",
                failures.join("; ")
            )))
        }
    }
    pub fn cleanup_orphans(&self) -> Result<Vec<EffectId>> {
        let context = Self::user_context();
        self.store
            .orphaned_prepared_effects()?
            .into_iter()
            .map(|effect| {
                let id = effect.id.clone();
                self.discard(&id, &context)?;
                Ok(id)
            })
            .collect()
    }
    pub fn create_plan(
        &self,
        effects: Vec<EffectId>,
        dependencies: Vec<EffectDependency>,
        atomicity: EffectAtomicity,
    ) -> Result<EffectPlan> {
        let unique: BTreeSet<_> = effects.iter().map(ToString::to_string).collect();
        if effects.is_empty() || unique.len() != effects.len() {
            return Err(Error::InvalidInput(
                "Effect plan requires unique nonempty effects".into(),
            ));
        }
        for id in &effects {
            let effect = self.store.effect(id)?;
            if effect.lifecycle != EffectLifecycle::Prepared {
                return Err(Error::InvalidInput(format!("Effect {id} is not PREPARED")));
            }
        }
        for dependency in &dependencies {
            if !unique.contains(&dependency.before.to_string())
                || !unique.contains(&dependency.after.to_string())
                || dependency.before == dependency.after
            {
                return Err(Error::InvalidInput(
                    "Effect dependency references an invalid plan member".into(),
                ));
            }
        }
        let plan = EffectPlan {
            id: EffectPlanId::new(),
            effects,
            dependencies,
            atomicity,
            created_at: Utc::now(),
        };
        self.topological_order(&plan)?;
        self.store.insert_effect_plan(&plan)?;
        Ok(plan)
    }
    fn topological_order(&self, plan: &EffectPlan) -> Result<Vec<EffectId>> {
        let mut remaining: BTreeSet<_> = plan.effects.iter().map(ToString::to_string).collect();
        let mut ordered = Vec::new();
        while !remaining.is_empty() {
            let next = plan.effects.iter().find(|candidate| {
                remaining.contains(&candidate.to_string())
                    && !plan.dependencies.iter().any(|dependency| {
                        dependency.after == **candidate
                            && remaining.contains(&dependency.before.to_string())
                    })
            });
            let Some(next) = next else {
                return Err(Error::InvalidInput("Effect plan contains a cycle".into()));
            };
            remaining.remove(&next.to_string());
            ordered.push(next.clone());
        }
        Ok(ordered)
    }
    pub fn commit_plan(
        &self,
        plan: &EffectPlan,
        authorization: &CommitAuthorization,
        context: &EffectContext,
    ) -> Result<EffectGroupResult> {
        let scope = plan
            .effects
            .iter()
            .map(|id| self.store.effect(id))
            .collect::<Result<Vec<_>>>()?;
        authorization.validate(&scope, Utc::now())?;
        let order = self.topological_order(plan)?;
        let mut receipts = Vec::new();
        let mut failed_effect = None;
        let mut unknown = false;
        for id in order {
            let effect = self.store.effect(&id)?;
            match self.commit_internal(effect, Some(authorization), context, Some(&scope))? {
                CommitOutcome::Committed { receipt } => receipts.push(receipt),
                CommitOutcome::Rejected { .. } => {
                    failed_effect = Some(id);
                    break;
                }
                CommitOutcome::Unknown { .. } => {
                    failed_effect = Some(id);
                    unknown = true;
                    break;
                }
            }
        }
        let commit_outcome = if unknown {
            CommitGroupOutcome::Unknown
        } else if receipts.len() == plan.effects.len() {
            CommitGroupOutcome::FullyCommitted
        } else if receipts.is_empty() {
            CommitGroupOutcome::NotCommitted
        } else {
            CommitGroupOutcome::PartiallyCommitted
        };
        let mut compensation = Vec::new();
        let mut final_outcome = commit_outcome;
        if commit_outcome == CommitGroupOutcome::PartiallyCommitted
            && plan.atomicity == EffectAtomicity::CompensatingGroup
        {
            for receipt in receipts.iter().rev() {
                compensation.push(self.compensate(&receipt.effect_id, context)?);
            }
            final_outcome = if compensation
                .iter()
                .all(|receipt| receipt.status == CompensationStatus::Successful)
            {
                CommitGroupOutcome::FullyCompensated
            } else {
                CommitGroupOutcome::PartiallyCompensated
            };
        }
        let result = EffectGroupResult {
            id: EffectGroupId::new(),
            plan_id: plan.id.clone(),
            commit_outcome,
            outcome: final_outcome,
            committed: receipts,
            failed_effect,
            manual_intervention_required: final_outcome == CommitGroupOutcome::PartiallyCompensated
                || final_outcome == CommitGroupOutcome::Unknown,
            compensation,
            created_at: Utc::now(),
        };
        self.store.insert_effect_group(&result)?;
        Ok(result)
    }
}
