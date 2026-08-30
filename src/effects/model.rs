// SPDX-License-Identifier: Apache-2.0
use crate::{
    Error, Result,
    core::{
        CommitAuthorizationId, CommitReceiptId, CompensationReceiptId, EffectEventId,
        EffectGroupId, EffectId, EffectInvariantId, EffectLedgerId, EffectPlanId,
        ExternalStateSnapshotId, PreparedEffectId, RealityId, ReconciliationAttemptId,
    },
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    Filesystem,
    Process,
    Database,
    HttpApi,
    CloudResource,
    Message,
    Deployment,
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectLifecycle {
    Proposed,
    Classified,
    Virtualized,
    Prepared,
    Rejected,
    Committed,
    Discarded,
    Failed,
    Compensated,
    Unknown,
}

impl EffectLifecycle {
    pub fn allows(self, next: Self) -> bool {
        use EffectLifecycle::*;
        matches!(
            (self, next),
            (Proposed, Classified | Rejected | Failed | Discarded)
                | (
                    Classified,
                    Virtualized | Prepared | Rejected | Failed | Discarded
                )
                | (Virtualized, Prepared | Failed | Discarded)
                | (Prepared, Committed | Failed | Discarded | Unknown)
                | (Unknown, Committed | Failed)
                | (Committed, Compensated)
                | (Failed, Discarded)
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReversibilityClass {
    NaturallyReversible,
    Compensatable,
    Shadowable,
    Deferrable,
    Irreversible,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdempotencyClass {
    Idempotent,
    IdempotentWithKey,
    NonIdempotent,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationRequirement {
    RealityLocal,
    Staged,
    Shadow,
    ProviderTransaction,
    Unsupported,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalityClass {
    RealityLocal,
    HostLocal,
    ExternalSystem,
    HumanVisible,
    Financial,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectRisk {
    ReadOnly,
    Low,
    Medium,
    High,
    Critical,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitStrategy {
    Direct,
    DeferredDispatch,
    ShadowPromote,
    ReserveCommit,
    Compensating,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectClassification {
    pub reversibility: ReversibilityClass,
    pub idempotency: IdempotencyClass,
    pub isolation_requirement: IsolationRequirement,
    pub externality: ExternalityClass,
    pub risk: EffectRisk,
    pub commit_strategy: CommitStrategy,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionRef {
    pub id: String,
    pub kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectTarget {
    pub uri: String,
}
impl EffectTarget {
    pub fn scheme(&self) -> Option<&str> {
        self.uri.split_once("://").map(|(scheme, _)| scheme)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum EffectOperation {
    Read,
    Create,
    Update,
    Delete,
    Post,
    Dispatch,
    Promote,
    Custom(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectFault {
    #[default]
    None,
    PrepareFailure,
    CommitFailureBeforeMutation,
    ResponseLossAfterMutation,
    ResponseLossWithReconciliationFailure,
    ReservationExpiry,
    DiscardFailure,
    CompensationFailure,
    ReconciliationFailure,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectRequest {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reality_id: Option<RealityId>,
    pub source_action: ActionRef,
    pub kind: EffectKind,
    pub target: EffectTarget,
    pub operation: EffectOperation,
    #[serde(default)]
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub fault: EffectFault,
}
impl EffectRequest {
    pub fn validate(&self) -> Result<()> {
        if self.session_id.trim().is_empty()
            || self.session_id.len() > 256
            || self.source_action.id.trim().is_empty()
            || self.source_action.id.len() > 256
        {
            return Err(Error::InvalidInput(
                "Effect session/action identifiers must be nonempty and bounded".into(),
            ));
        }
        if self.target.uri.len() > 2048
            || self.target.uri.contains(['\0', '\n', '\r'])
            || self.target.scheme().is_none()
        {
            return Err(Error::InvalidInput(
                "Effect target must be a bounded single-line scheme URI".into(),
            ));
        }
        if serde_json::to_vec(&self.payload)?.len() > 256 * 1024 || self.evidence.len() > 128 {
            return Err(Error::InvalidInput(
                "Effect payload or evidence exceeded its bound".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Effect {
    pub id: EffectId,
    pub ledger_id: EffectLedgerId,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reality_id: Option<RealityId>,
    pub source_action: ActionRef,
    pub kind: EffectKind,
    pub target: EffectTarget,
    pub operation: EffectOperation,
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification: Option<EffectClassification>,
    pub lifecycle: EffectLifecycle,
    pub adapter: String,
    pub idempotency_key: String,
    pub evidence: Vec<String>,
    pub fault: EffectFault,
    pub created_at: DateTime<Utc>,
}
impl Effect {
    pub fn from_request(
        request: EffectRequest,
        ledger_id: EffectLedgerId,
        adapter: String,
    ) -> Self {
        let id = EffectId::new();
        Self {
            idempotency_key: format!("hk-effect:{id}"),
            id,
            ledger_id,
            session_id: request.session_id,
            reality_id: request.reality_id,
            source_action: request.source_action,
            kind: request.kind,
            target: request.target,
            operation: request.operation,
            payload: request.payload,
            classification: None,
            lifecycle: EffectLifecycle::Proposed,
            adapter,
            evidence: request.evidence,
            fault: request.fault,
            created_at: Utc::now(),
        }
    }
    pub fn scope_hash(&self) -> Result<String> {
        #[derive(Serialize)]
        struct Scope<'a> {
            id: &'a EffectId,
            adapter: &'a str,
            kind: EffectKind,
            target: &'a EffectTarget,
            operation: &'a EffectOperation,
            payload: &'a Value,
            idempotency_key: &'a str,
        }
        Ok(blake3::hash(&serde_json::to_vec(&Scope {
            id: &self.id,
            adapter: &self.adapter,
            kind: self.kind,
            target: &self.target,
            operation: &self.operation,
            payload: &self.payload,
            idempotency_key: &self.idempotency_key,
        })?)
        .to_hex()
        .to_string())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectLedger {
    pub id: EffectLedgerId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reality_id: Option<RealityId>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectEventType {
    Proposed,
    Classified,
    Virtualized,
    Prepared,
    CommitAuthorized,
    CommitRejected,
    Committed,
    Discarded,
    CommitFailed,
    CompensationStarted,
    Compensated,
    CompensationFailed,
    Unknown,
    Reconciled,
    CleanupFailed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectEvent {
    pub id: EffectEventId,
    pub effect_id: EffectId,
    pub sequence: u64,
    pub event_type: EffectEventType,
    pub timestamp: DateTime<Utc>,
    pub evidence: Vec<String>,
    pub metadata: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectPreview {
    pub summary: String,
    pub current: Value,
    pub prepared: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExternalStateSnapshot {
    pub id: ExternalStateSnapshotId,
    pub effect_id: EffectId,
    pub adapter: String,
    pub target: EffectTarget,
    pub version: Option<String>,
    pub fingerprint: String,
    pub state: Value,
    pub captured_at: DateTime<Utc>,
}
impl ExternalStateSnapshot {
    pub fn capture(
        effect_id: EffectId,
        adapter: &str,
        target: EffectTarget,
        version: Option<String>,
        state: Value,
    ) -> Result<Self> {
        let fingerprint = blake3::hash(&serde_json::to_vec(&(adapter, &target, &version, &state))?)
            .to_hex()
            .to_string();
        Ok(Self {
            id: ExternalStateSnapshotId::new(),
            effect_id,
            adapter: adapter.into(),
            target,
            version,
            fingerprint,
            state,
            captured_at: Utc::now(),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreparedEffect {
    pub id: PreparedEffectId,
    pub effect_id: EffectId,
    pub adapter: String,
    pub preparation_token: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub preview: EffectPreview,
    pub before: ExternalStateSnapshot,
    pub scope_hash: String,
    pub evidence: Vec<String>,
}
impl PreparedEffect {
    pub fn expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|expiry| expiry <= now)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommitReceipt {
    pub id: CommitReceiptId,
    pub effect_id: EffectId,
    pub adapter: String,
    pub committed_at: DateTime<Utc>,
    pub external_reference: Option<String>,
    pub idempotency_key: Option<String>,
    pub result_hash: Option<String>,
    pub metadata: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompensationStatus {
    Successful,
    Partial,
    Failed,
    Unsupported,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompensationReceipt {
    pub id: CompensationReceiptId,
    pub original_receipt: CommitReceiptId,
    pub compensated_at: DateTime<Utc>,
    pub status: CompensationStatus,
    pub metadata: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectAdapterCapabilities {
    pub simulation: bool,
    pub prepare: bool,
    pub commit: bool,
    pub discard: bool,
    pub compensate: bool,
    pub reconciliation: bool,
    pub idempotency_keys: bool,
    pub shadow_resources: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectDecision {
    AllowDirect,
    RequirePrepare,
    RequireApproval,
    RequireShadow,
    Reject,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectCapability {
    Observe,
    Propose,
    Prepare,
    Commit,
    Compensate,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EffectCapabilityGrant {
    pub observe: bool,
    pub propose: bool,
    pub prepare: bool,
    pub commit: bool,
    pub compensate: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EffectConfig {
    pub default: EffectCapabilityGrant,
    pub adapters: BTreeMap<String, EffectCapabilityGrant>,
}
impl Default for EffectConfig {
    fn default() -> Self {
        Self {
            default: EffectCapabilityGrant {
                observe: true,
                propose: true,
                prepare: true,
                commit: false,
                compensate: false,
            },
            adapters: BTreeMap::new(),
        }
    }
}
impl EffectConfig {
    pub fn validate(&self) -> Result<()> {
        if self.adapters.len() > 128
            || self.adapters.keys().any(|name| {
                name.is_empty() || name.len() > 128 || name.contains(['\0', '\n', '\r'])
            })
        {
            return Err(Error::InvalidInput(
                "Effect adapter policy names must be bounded".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectContext {
    pub actor: String,
    pub is_agent: bool,
    pub capabilities: EffectCapabilityGrant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitAuthority {
    User,
    Policy,
    Ci,
    ExternalApprovalSystem,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommitAuthorization {
    pub id: CommitAuthorizationId,
    pub authority: CommitAuthority,
    pub effect_ids: Vec<EffectId>,
    pub granted_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub scope_hash: String,
}
impl CommitAuthorization {
    pub fn for_effects(authority: CommitAuthority, effects: &[Effect]) -> Result<Self> {
        let mut scopes: Vec<_> = effects
            .iter()
            .map(|effect| Ok((effect.id.to_string(), effect.scope_hash()?)))
            .collect::<Result<_>>()?;
        scopes.sort();
        let scope_hash = blake3::hash(&serde_json::to_vec(&scopes)?)
            .to_hex()
            .to_string();
        Ok(Self {
            id: CommitAuthorizationId::new(),
            authority,
            effect_ids: effects.iter().map(|effect| effect.id.clone()).collect(),
            granted_at: Utc::now(),
            expires_at: Some(Utc::now() + Duration::minutes(15)),
            scope_hash,
        })
    }
    pub fn validate(&self, effects: &[Effect], now: DateTime<Utc>) -> Result<()> {
        if self.expires_at.is_some_and(|expiry| expiry <= now) {
            return Err(Error::Intervention("Commit authorization expired".into()));
        }
        let expected = Self::for_effects(self.authority, effects)?;
        let requested: std::collections::BTreeSet<_> =
            self.effect_ids.iter().map(ToString::to_string).collect();
        let actual: std::collections::BTreeSet<_> =
            effects.iter().map(|effect| effect.id.to_string()).collect();
        if requested != actual || self.scope_hash != expected.scope_hash {
            return Err(Error::Intervention(
                "Commit authorization is not bound to the current effect scope".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvariantPhase {
    BeforePrepare,
    AfterPrepare,
    BeforeCommit,
    AfterCommit,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InvariantPredicate {
    VersionEquals { expected: String },
    TargetScheme { expected: String },
    JsonPointerEquals { pointer: String, expected: Value },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectInvariant {
    pub id: EffectInvariantId,
    pub predicate: InvariantPredicate,
    pub phase: InvariantPhase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitGateDecision {
    Allow,
    RequireApproval,
    Reprepare,
    Reject,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AdapterCommitOutcome {
    Committed { receipt: CommitReceipt },
    NotCommitted { reason: String },
    Unknown { reason: String },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ReconciliationResult {
    Committed { receipt: CommitReceipt },
    NotCommitted,
    StillUnknown { reason: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CommitOutcome {
    Committed { receipt: CommitReceipt },
    Rejected { reason: String, reprepare: bool },
    Unknown { reason: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectDependency {
    pub before: EffectId,
    pub after: EffectId,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectAtomicity {
    SingleEffect,
    BestEffortGroup,
    CompensatingGroup,
    AtomicProviderTransaction,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectPlan {
    pub id: EffectPlanId,
    pub effects: Vec<EffectId>,
    pub dependencies: Vec<EffectDependency>,
    pub atomicity: EffectAtomicity,
    pub created_at: DateTime<Utc>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitGroupOutcome {
    FullyCommitted,
    NotCommitted,
    PartiallyCommitted,
    Unknown,
    FullyCompensated,
    PartiallyCompensated,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompensationStep {
    pub effect_id: EffectId,
    pub receipt_id: CommitReceiptId,
    pub status: CompensationStatus,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompensationPlan {
    pub steps: Vec<CompensationStep>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectGroupResult {
    pub id: EffectGroupId,
    pub plan_id: EffectPlanId,
    pub commit_outcome: CommitGroupOutcome,
    pub outcome: CommitGroupOutcome,
    pub committed: Vec<CommitReceipt>,
    pub failed_effect: Option<EffectId>,
    pub compensation: Vec<CompensationReceipt>,
    pub manual_intervention_required: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReconciliationAttempt {
    pub id: ReconciliationAttemptId,
    pub effect_id: EffectId,
    pub attempted_at: DateTime<Utc>,
    pub result: ReconciliationResult,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExternalEffectCapabilities {
    pub virtualized: bool,
    pub staged: bool,
    pub commit_supported: bool,
    pub compensation_supported: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExternalTaskOutcome {
    pub experiment_success: bool,
    pub commit_status: CommitGroupOutcome,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectBenchmarkRun {
    pub id: crate::core::BenchmarkRunId,
    pub created_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub metrics: BTreeMap<String, Value>,
    pub scenarios: Value,
    pub artifact: std::path::PathBuf,
}
