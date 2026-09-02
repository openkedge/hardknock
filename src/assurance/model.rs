// SPDX-License-Identifier: Apache-2.0

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    Result,
    capability::{ExecutionCapability, NetworkEndpointPattern},
    core::*,
    curriculum::{ExperiencePackage, Severity},
    development::ExperiencePackageRevision,
    federation::{ContextCompatibility, ExperienceNodeId, ProvenanceGraph},
    tool::AttestationAssurance,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum ContractSubject {
    Skill(SkillId),
    Tool(ToolId),
    Recovery(RecoveryId),
    EffectPlan(EffectPlanId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PredicateOperator {
    Equals,
    NotEquals,
    GreaterThan,
    GreaterThanOrEqual,
    LessThan,
    LessThanOrEqual,
    Exists,
    NotExists,
    Contains,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EffectPredicate {
    CommittedEffect,
    NoCommittedEffect,
    ExperimentalEffectLeak,
    NoExperimentalEffectLeak,
    CommitAuthorityExternalized,
    CommitAuthorityNotExternalized,
    EffectKind { effect_kind: String },
    NoEffectKind { effect_kind: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CapabilityPredicate {
    NoAmbientCredentials,
    NoEffectCommit,
    MaximumNotExceeded,
    DeclaredToolManifestsOnly,
    MinimizationValidated,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum BehavioralCondition {
    EvaluatorCheck {
        evaluator: String,
        expression: String,
    },
    StatePredicate {
        path: String,
        operator: PredicateOperator,
        #[serde(default)]
        value: Value,
    },
    EffectPredicate {
        predicate: EffectPredicate,
    },
    CapabilityPredicate {
        predicate: CapabilityPredicate,
    },
    Custom {
        kind: String,
        payload: Value,
    },
}

impl BehavioralCondition {
    pub fn fingerprint(&self) -> Result<String> {
        Ok(blake3::hash(&serde_json::to_vec(self)?)
            .to_hex()
            .to_string())
    }

    pub fn id(&self) -> Result<BehavioralConditionId> {
        Ok(BehavioralConditionId::from_fingerprint(
            &self.fingerprint()?,
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvariantEvaluationPhase {
    BeforeExecution,
    DuringExecution,
    AfterExecution,
    BeforeEffectCommit,
    AfterEffectCommit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BehavioralInvariant {
    pub id: BehavioralInvariantId,
    pub description: String,
    pub condition: BehavioralCondition,
    pub severity: Severity,
    pub phases: Vec<InvariantEvaluationPhase>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForbiddenOutcome {
    pub id: ForbiddenOutcomeId,
    pub description: String,
    pub detector: BehavioralCondition,
    pub severity: Severity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionCapabilityPattern {
    AnyNetwork,
    NetworkEndpoint { endpoint: NetworkEndpointPattern },
    AnyCredential,
    EffectCommit,
    FilesystemWrite { root: Option<String> },
    Exact { capability: ExecutionCapability },
}

impl ExecutionCapabilityPattern {
    pub fn matches(&self, capability: &ExecutionCapability) -> bool {
        match (self, capability) {
            (Self::AnyNetwork, ExecutionCapability::NetworkConnect(_))
            | (Self::AnyCredential, ExecutionCapability::CredentialUse(_))
            | (Self::EffectCommit, ExecutionCapability::EffectCommit(_)) => true,
            (
                Self::NetworkEndpoint { endpoint: expected },
                ExecutionCapability::NetworkConnect(actual),
            ) => expected == actual,
            (Self::FilesystemWrite { root: None }, ExecutionCapability::FilesystemWrite(_)) => true,
            (
                Self::FilesystemWrite {
                    root: Some(expected),
                },
                ExecutionCapability::FilesystemWrite(actual),
            ) => actual.root == *expected,
            (
                Self::Exact {
                    capability: expected,
                },
                actual,
            ) => expected == actual,
            _ => false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityEnvelope {
    #[serde(default)]
    pub allowed: Vec<ExecutionCapabilityPattern>,
    #[serde(default)]
    pub deny_ambient_credentials: bool,
    #[serde(default)]
    pub deny_effect_commit: bool,
}

impl CapabilityEnvelope {
    pub fn permits(&self, capability: &ExecutionCapability) -> bool {
        self.allowed
            .iter()
            .any(|pattern| pattern.matches(capability))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityRequirementSet {
    #[serde(default)]
    pub forbidden: Vec<ExecutionCapabilityPattern>,
    #[serde(default)]
    pub required: Vec<ExecutionCapabilityPattern>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<CapabilityEnvelope>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluationRequirementSet {
    #[serde(default)]
    pub evaluators: Vec<String>,
    #[serde(default)]
    pub observable_state_paths: Vec<String>,
    #[serde(default)]
    pub effects_observable: bool,
    #[serde(default)]
    pub capabilities_observable: bool,
    #[serde(default)]
    pub custom_condition_kinds: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BehavioralContract {
    pub id: BehavioralContractId,
    pub name: String,
    pub version: String,
    pub subject: ContractSubject,
    #[serde(default)]
    pub preconditions: Vec<BehavioralCondition>,
    #[serde(default)]
    pub postconditions: Vec<BehavioralCondition>,
    #[serde(default)]
    pub invariants: Vec<BehavioralInvariant>,
    #[serde(default)]
    pub forbidden_outcomes: Vec<ForbiddenOutcome>,
    #[serde(default)]
    pub capability_requirements: CapabilityRequirementSet,
    #[serde(default)]
    pub evaluation_requirements: EvaluationRequirementSet,
    pub created_at: DateTime<Utc>,
}

impl BehavioralContract {
    pub fn validate(&self) -> Result<()> {
        use crate::Error;
        if self.name.trim().is_empty()
            || self.name.len() > 160
            || self.version.trim().is_empty()
            || self.version.len() > 64
        {
            return Err(Error::InvalidInput(
                "Contract name and version must be bounded and nonempty".into(),
            ));
        }
        let mut invariant_ids = BTreeSet::new();
        for invariant in &self.invariants {
            if invariant.description.trim().is_empty()
                || invariant.phases.is_empty()
                || !invariant_ids.insert(invariant.id.clone())
            {
                return Err(Error::InvalidInput(
                    "Invariants require a unique ID, description, and evaluation phase".into(),
                ));
            }
        }
        let mut forbidden_ids = BTreeSet::new();
        if self.forbidden_outcomes.iter().any(|outcome| {
            outcome.description.trim().is_empty() || !forbidden_ids.insert(outcome.id.clone())
        }) {
            return Err(Error::InvalidInput(
                "Forbidden outcomes require a unique ID and description".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BehavioralContractRef {
    pub contract_id: BehavioralContractId,
    pub revision: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BehavioralContractRevision {
    pub contract_id: BehavioralContractId,
    pub revision: u64,
    pub contract: BehavioralContract,
    pub parent_revision: Option<u64>,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillRevisionRef {
    pub skill_id: SkillId,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssuranceProfileRef {
    pub id: AssuranceProfileId,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "parameters", rename_all = "snake_case")]
pub enum AssuranceRequirement {
    ContractSatisfied {
        minimum_runs: usize,
    },
    ControlledExperiments {
        minimum: usize,
    },
    PerturbationProfileCoverage {
        profile: String,
        minimum_fraction: f64,
    },
    RecoveryCoverage {
        minimum_high_severity_classes: usize,
    },
    ReflexFalsePositiveMaximum {
        maximum: f64,
    },
    CapabilityProfile {
        required_profile: String,
    },
    CapabilityMinimizationValidated,
    NoUnresolvedCriticalContradictions,
    EvidenceFreshness {
        maximum_age_days: Option<u32>,
    },
    ExecutionAttestation {
        minimum_assurance: AttestationAssurance,
    },
    EvidenceDiversity {
        minimum: crate::epistemic::DiversityClass,
    },
    Custom {
        kind: String,
        payload: Value,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssuranceProfile {
    pub id: AssuranceProfileId,
    pub name: String,
    pub version: String,
    pub requirements: Vec<AssuranceRequirement>,
    pub created_at: DateTime<Utc>,
}

impl AssuranceProfile {
    pub fn validate(&self) -> Result<()> {
        use crate::Error;
        if self.name.trim().is_empty()
            || self.name.len() > 160
            || self.version.trim().is_empty()
            || self.version.len() > 64
            || self.requirements.is_empty()
        {
            return Err(Error::InvalidInput(
                "Assurance Profile name, version, and requirements must be bounded and nonempty"
                    .into(),
            ));
        }
        for requirement in &self.requirements {
            let valid = match requirement {
                AssuranceRequirement::ContractSatisfied { minimum_runs } => *minimum_runs > 0,
                AssuranceRequirement::ControlledExperiments { minimum } => *minimum > 0,
                AssuranceRequirement::PerturbationProfileCoverage {
                    profile,
                    minimum_fraction,
                } => {
                    !profile.trim().is_empty()
                        && minimum_fraction.is_finite()
                        && (0.0..=1.0).contains(minimum_fraction)
                }
                AssuranceRequirement::RecoveryCoverage {
                    minimum_high_severity_classes,
                } => *minimum_high_severity_classes > 0,
                AssuranceRequirement::ReflexFalsePositiveMaximum { maximum } => {
                    maximum.is_finite() && (0.0..=1.0).contains(maximum)
                }
                AssuranceRequirement::CapabilityProfile { required_profile } => {
                    !required_profile.trim().is_empty()
                }
                AssuranceRequirement::Custom { kind, .. } => !kind.trim().is_empty(),
                _ => true,
            };
            if !valid {
                return Err(Error::InvalidInput(
                    "Assurance Profile contains an invalid requirement".into(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CertificationStatus {
    Candidate,
    Satisfied,
    Certified,
    Expired,
    Invalidated,
    Superseded,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillCertification {
    pub id: SkillCertificationId,
    pub skill: SkillRevisionRef,
    pub contract: BehavioralContractRef,
    pub profile: AssuranceProfileRef,
    pub status: CertificationStatus,
    pub evidence_manifest: EvidenceManifestId,
    pub issued_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub supersedes: Option<SkillCertificationId>,
    pub policy_versions: PolicyVersions,
    #[serde(default)]
    pub tool_artifact_hashes: BTreeSet<String>,
    #[serde(default)]
    pub runtime_digests: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_diversity: Option<crate::epistemic::DiversityClass>,
    #[serde(default)]
    pub evidence_source_types: usize,
    #[serde(default)]
    pub root_evidence_origins: usize,
    #[serde(default)]
    pub evaluator_kinds: usize,
    #[serde(default)]
    pub epistemic_dependency_caveats: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "subject", rename_all = "snake_case")]
pub enum EvidenceSubject {
    Skill(SkillRevisionRef),
    Tool(ToolId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractEvaluationStatus {
    Satisfied,
    Violated,
    Inconclusive,
    NotApplicable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConditionResult {
    pub condition: BehavioralConditionId,
    pub status: ContractEvaluationStatus,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InvariantResult {
    pub invariant_id: BehavioralInvariantId,
    pub severity: Severity,
    pub status: ContractEvaluationStatus,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ForbiddenOutcomeResult {
    pub outcome_id: ForbiddenOutcomeId,
    pub severity: Severity,
    pub status: ContractEvaluationStatus,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContractEvaluation {
    pub contract_id: BehavioralContractId,
    pub preconditions: Vec<ConditionResult>,
    pub postconditions: Vec<ConditionResult>,
    pub invariants: Vec<InvariantResult>,
    pub forbidden_outcomes: Vec<ForbiddenOutcomeResult>,
    pub overall: ContractEvaluationStatus,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EffectEvidence {
    pub observable: bool,
    pub committed_effect: bool,
    pub experimental_effect_leak: bool,
    pub commit_authority_externalized: bool,
    #[serde(default)]
    pub effect_kinds: BTreeSet<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CapabilityEvidence {
    pub observable: bool,
    #[serde(default)]
    pub observed: Vec<ExecutionCapability>,
    pub ambient_credentials: bool,
    pub effect_commit_granted: bool,
    pub declared_tool_manifests_only: bool,
    pub minimization_validated: bool,
    pub maximum_exceeded: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExecutionEvidence {
    #[serde(default)]
    pub states: BTreeMap<String, Value>,
    #[serde(default)]
    pub complete_state_snapshot: bool,
    /// Deterministic evaluator keys use `evaluator\u{1f}expression`.
    #[serde(default)]
    pub evaluator_results: BTreeMap<String, ContractEvaluationStatus>,
    #[serde(default)]
    pub custom_results: BTreeMap<String, ContractEvaluationStatus>,
    #[serde(default)]
    pub effects: EffectEvidence,
    #[serde(default)]
    pub capabilities: CapabilityEvidence,
}

impl ExecutionEvidence {
    pub fn evaluator_key(evaluator: &str, expression: &str) -> String {
        format!("{evaluator}\u{1f}{expression}")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContractObservability {
    pub observable: Vec<BehavioralConditionId>,
    pub unobservable: Vec<BehavioralConditionId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceContradiction {
    pub description: String,
    pub severity: Severity,
    pub evidence_ids: Vec<String>,
    pub resolved: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AssuranceEvidenceSummary {
    #[serde(default)]
    pub contract_evaluations: Vec<ContractEvaluation>,
    pub controlled_experiments: usize,
    #[serde(default)]
    pub perturbation_profile_coverage: BTreeMap<String, f64>,
    #[serde(default)]
    pub tested_conditions: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    pub high_severity_recovery_classes: BTreeSet<String>,
    pub reflex_checks: usize,
    pub reflex_false_positives: usize,
    #[serde(default)]
    pub contradictions: Vec<EvidenceContradiction>,
    #[serde(default)]
    pub observed_capabilities: Vec<ExecutionCapability>,
    pub capability_observed: bool,
    pub capability_maximum_exceeded: bool,
    pub ambient_credentials_observed: bool,
    pub effect_commit_granted: bool,
    pub declared_tool_manifests_only: bool,
    pub capability_minimization_validated: bool,
    #[serde(default)]
    pub capability_profiles_satisfied: BTreeSet<String>,
    #[serde(default)]
    pub attestation_assurance: Vec<AttestationAssurance>,
    pub attestations_intact: bool,
    pub oldest_required_evidence: Option<DateTime<Utc>>,
    pub newest_evidence: Option<DateTime<Utc>>,
    pub experimental_effect_leak: bool,
    #[serde(default)]
    pub known_unknowns: Vec<String>,
    #[serde(default)]
    pub tool_artifact_hashes: BTreeSet<String>,
    #[serde(default)]
    pub runtime_digests: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyVersions {
    pub confidence: String,
    pub validation: String,
    pub freshness: String,
    pub contract_evaluator: String,
    pub capability: String,
    pub evidence_selection: String,
}

impl Default for PolicyVersions {
    fn default() -> Self {
        Self {
            confidence: "hardknock.confidence.v1".into(),
            validation: "hardknock.validation.v1".into(),
            freshness: super::FRESHNESS_POLICY_VERSION.into(),
            contract_evaluator: super::CONTRACT_EVALUATOR_VERSION.into(),
            capability: super::CAPABILITY_POLICY_VERSION.into(),
            evidence_selection: super::EVIDENCE_POLICY_VERSION.into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvidenceManifest {
    pub id: EvidenceManifestId,
    pub subject: EvidenceSubject,
    pub generated_at: DateTime<Utc>,
    #[serde(default)]
    pub experiences: Vec<ExperienceId>,
    #[serde(default)]
    pub experiments: Vec<ExperimentId>,
    #[serde(default)]
    pub chaos_campaigns: Vec<ChaosCampaignId>,
    #[serde(default)]
    pub attestations: Vec<ExecutionAttestationId>,
    #[serde(default)]
    pub lessons: Vec<LessonId>,
    #[serde(default)]
    pub reflexes: Vec<ReflexId>,
    #[serde(default)]
    pub recoveries: Vec<RecoveryId>,
    #[serde(default)]
    pub envelopes: Vec<OperatingEnvelopeId>,
    #[serde(default)]
    pub capability_manifests: Vec<CapabilityManifestId>,
    #[serde(default)]
    pub effect_receipts: Vec<CommitReceiptId>,
    pub policy_versions: PolicyVersions,
    #[serde(default)]
    pub summary: AssuranceEvidenceSummary,
    pub evidence_hash: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssuranceGapKind {
    UntestedCondition,
    InsufficientEvidence,
    StaleEvidence,
    UnsupportedIsolation,
    UnsupportedEffect,
    ContractInconclusive,
    ContradictoryEvidence,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssuranceGap {
    pub kind: AssuranceGapKind,
    pub description: String,
    pub severity: Option<Severity>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssuranceRequirementStatus {
    Satisfied,
    Violated,
    Inconclusive,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssuranceRequirementResult {
    pub requirement: AssuranceRequirement,
    pub status: AssuranceRequirementStatus,
    pub evidence: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CertificationBlocker {
    pub description: String,
    pub severity: Severity,
    pub evidence_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CertificationRecommendation {
    Eligible,
    AdditionalEvidenceRequired,
    Blocked,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CertificationEvaluation {
    pub requirements: Vec<AssuranceRequirementResult>,
    pub gaps: Vec<AssuranceGap>,
    pub blockers: Vec<CertificationBlocker>,
    pub recommendation: CertificationRecommendation,
    pub dimensions: Vec<AssuranceDimensionResult>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssuranceDimension {
    Behavior,
    Resilience,
    Recovery,
    CapabilityDiscipline,
    EffectDiscipline,
    EvidenceFreshness,
    EvidenceDiversity,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssuranceDimensionResult {
    pub dimension: AssuranceDimension,
    pub status: AssuranceRequirementStatus,
    pub explanation: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EvidenceCompleteness {
    pub required: usize,
    pub satisfied: usize,
    pub missing: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CertificationFreshness {
    Current,
    ReviewRecommended,
    Expired,
    Invalidated,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CurrentAssuranceContext {
    pub now: DateTime<Utc>,
    pub skill_revision: u64,
    pub contract_revision: u64,
    #[serde(default)]
    pub tool_artifact_hashes: BTreeSet<String>,
    #[serde(default)]
    pub runtime_digests: BTreeSet<String>,
    pub invalidated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CertificationRevocation {
    pub certification_id: SkillCertificationId,
    pub reason: String,
    pub revoked_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssuranceEvidenceKind {
    Empirical,
    StaticAnalysis,
    FormalProof,
    ExternalAudit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssuranceEvidenceType {
    Execution,
    Experiment,
    ChaosTrial,
    RecoveryTrial,
    ContractCheck,
    CapabilityCheck,
    Attestation,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CertifiedExperiencePackage {
    pub experience_package: ExperiencePackageRevision,
    pub behavioral_contract: BehavioralContractRef,
    pub certifications: Vec<SkillCertificationId>,
    pub evidence_manifest: EvidenceManifestId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CandidateCertifiedExperiencePackage {
    pub experience_package: ExperiencePackage,
    pub behavioral_contract: BehavioralContractRef,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssuranceConflict {
    pub certification: SkillCertificationId,
    pub external_evidence: String,
    pub compatibility: ContextCompatibility,
    pub severity: Severity,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CertificationSignature {
    pub algorithm: String,
    pub producer: ExperienceNodeId,
    pub producer_name: String,
    pub public_key: String,
    pub signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CertificationArtifact {
    pub schema_version: String,
    pub certification: SkillCertification,
    pub contract: BehavioralContractRevision,
    pub profile: AssuranceProfile,
    pub evidence_manifest: EvidenceManifest,
    #[serde(default)]
    pub provenance: ProvenanceGraph,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<CertificationSignature>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CertificationVerification {
    pub schema_valid: bool,
    pub signature_valid: bool,
    pub manifest_intact: bool,
    pub internally_consistent: bool,
    pub authentic: bool,
    /// V0.11 never turns remote authenticity into local certification.
    pub local_certification_established: bool,
    pub local_reproduction_performed: bool,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ContractFile {
    pub schema: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub preconditions: Vec<BehavioralCondition>,
    #[serde(default)]
    pub postconditions: Vec<BehavioralCondition>,
    #[serde(default)]
    pub invariants: Vec<ContractFileInvariant>,
    #[serde(default)]
    pub forbidden_outcomes: Vec<ContractFileForbiddenOutcome>,
    #[serde(default)]
    pub capability_requirements: CapabilityRequirementSet,
    #[serde(default)]
    pub evaluation_requirements: EvaluationRequirementSet,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ContractFileInvariant {
    pub description: String,
    pub severity: Severity,
    #[serde(flatten)]
    pub condition: BehavioralCondition,
    pub phases: Vec<InvariantEvaluationPhase>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ContractFileForbiddenOutcome {
    pub description: String,
    pub severity: Severity,
    #[serde(flatten)]
    pub detector: BehavioralCondition,
}

impl ContractFile {
    pub fn into_contract(self, subject: ContractSubject) -> Result<BehavioralContract> {
        use crate::Error;
        if self.schema != super::CONTRACT_SCHEMA_V1 {
            return Err(Error::InvalidInput(format!(
                "Unsupported contract schema {}",
                self.schema
            )));
        }
        let contract = BehavioralContract {
            id: BehavioralContractId::new(),
            name: self.name,
            version: self.version,
            subject,
            preconditions: self.preconditions,
            postconditions: self.postconditions,
            invariants: self
                .invariants
                .into_iter()
                .map(|invariant| BehavioralInvariant {
                    id: BehavioralInvariantId::new(),
                    description: invariant.description,
                    condition: invariant.condition,
                    severity: invariant.severity,
                    phases: invariant.phases,
                })
                .collect(),
            forbidden_outcomes: self
                .forbidden_outcomes
                .into_iter()
                .map(|outcome| ForbiddenOutcome {
                    id: ForbiddenOutcomeId::new(),
                    description: outcome.description,
                    detector: outcome.detector,
                    severity: outcome.severity,
                })
                .collect(),
            capability_requirements: self.capability_requirements,
            evaluation_requirements: self.evaluation_requirements,
            created_at: Utc::now(),
        };
        contract.validate()?;
        Ok(contract)
    }
}
