// SPDX-License-Identifier: Apache-2.0

pub use crate::development::ExperienceRef;
use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    budget::ExperienceBudget,
    core::{
        AgentIdentity, ChaosCampaignId, ClaimId, EpistemicFaultDomainId, EvidencePathId,
        EvidenceSessionId, ExperimentId, LessonId, RecoveryId, RuntimeDecisionId,
    },
    experimentation::ExperimentRequest,
    federation::ExperienceNodeId,
    lesson::{ActionPattern, ContextSelector},
    tool::ToolIdentity,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimRef {
    pub id: ClaimId,
}

impl From<ClaimId> for ClaimRef {
    fn from(id: ClaimId) -> Self {
        Self { id }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    StrategyPreference,
    LessonClaim,
    RecoveryClaim,
    SkillBehavior,
    FailureCause,
    OperatingEnvelopeClaim,
    RuntimeDecisionClaim,
    Custom,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Claim {
    pub id: ClaimId,
    pub kind: ClaimKind,
    pub statement: String,
    pub scope: ContextSelector,
    pub created_at: DateTime<Utc>,
}

impl Claim {
    pub fn validate(&self) -> crate::Result<()> {
        if self.statement.trim().is_empty() || self.statement.len() > 4096 {
            return Err(crate::Error::InvalidInput(
                "Claim statement must be nonempty and at most 4096 bytes".into(),
            ));
        }
        Ok(())
    }

    /// Canonicalization is deliberately lexical and deterministic. V0.13 does
    /// not introduce embedding-based semantic claim identity.
    pub fn canonical_statement(&self) -> String {
        self.statement
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSourceKind {
    Agent,
    Experiment,
    Chaos,
    RecoveryTrial,
    Federation,
    StaticCheck,
    HumanObservation,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceSource {
    Agent { identity: AgentIdentity },
    Experiment { experiment_id: ExperimentId },
    Chaos { campaign_id: ChaosCampaignId },
    RecoveryTrial { recovery_id: RecoveryId },
    Federation { node_id: ExperienceNodeId },
    StaticCheck { evaluator: String },
    HumanObservation { label: String },
}

impl EvidenceSource {
    pub fn kind(&self) -> EvidenceSourceKind {
        match self {
            Self::Agent { .. } => EvidenceSourceKind::Agent,
            Self::Experiment { .. } => EvidenceSourceKind::Experiment,
            Self::Chaos { .. } => EvidenceSourceKind::Chaos,
            Self::RecoveryTrial { .. } => EvidenceSourceKind::RecoveryTrial,
            Self::Federation { .. } => EvidenceSourceKind::Federation,
            Self::StaticCheck { .. } => EvidenceSourceKind::StaticCheck,
            Self::HumanObservation { .. } => EvidenceSourceKind::HumanObservation,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Agent { identity } => format!("agent:{}", identity.kind),
            Self::Experiment { experiment_id } => format!("experiment:{experiment_id}"),
            Self::Chaos { campaign_id } => format!("chaos:{campaign_id}"),
            Self::RecoveryTrial { recovery_id } => format!("recovery:{recovery_id}"),
            Self::Federation { node_id } => format!("federation:{node_id}"),
            Self::StaticCheck { evaluator } => format!("static-check:{evaluator}"),
            Self::HumanObservation { label } => format!("human:{label}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceOutcome {
    Supports,
    Contradicts,
    Inconclusive,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub kind: String,
    pub id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluatorKind {
    TestSuite,
    BehavioralContract,
    StaticAnalysis,
    PropertyCheck,
    ExternalOracle,
    HumanReview,
    Custom,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EvaluatorIdentity {
    pub name: String,
    pub version: String,
    pub kind: EvaluatorKind,
}

impl EvaluatorIdentity {
    pub fn dependency_key(&self) -> String {
        format!("{}@{}:{:?}", self.name, self.version, self.kind)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpistemicDependencySet {
    pub model_family: Option<String>,
    pub model_version: Option<String>,
    pub agent_runtime: Option<String>,
    pub system_prompt_family: Option<String>,
    pub experience_profile: Option<String>,
    #[serde(default)]
    pub retrieval_sources: Vec<String>,
    #[serde(default)]
    pub external_documents: Vec<String>,
    #[serde(default)]
    pub tools: Vec<ToolIdentity>,
    #[serde(default)]
    pub evaluators: Vec<String>,
    #[serde(default)]
    pub evaluator_identities: Vec<EvaluatorIdentity>,
    pub environment_family: Option<String>,
    #[serde(default)]
    pub originating_lessons: Vec<LessonId>,
    #[serde(default)]
    pub originating_federated_nodes: Vec<ExperienceNodeId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpistemicDependencyKind {
    Model,
    AgentRuntime,
    Prompt,
    RetrievalSource,
    Experience,
    Tool,
    Evaluator,
    Environment,
    ExternalEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyValue {
    pub kind: EpistemicDependencyKind,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpistemicDependencyNode {
    pub id: String,
    pub kind: EpistemicDependencyNodeKind,
    pub label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpistemicDependencyNodeKind {
    EvidencePath,
    Source,
    Dependency,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpistemicDependencyEdgeKind {
    Uses,
    DerivedFrom,
    RetrievedFrom,
    EvaluatedBy,
    SharesWith,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpistemicDependencyEdge {
    pub from: String,
    pub to: String,
    pub kind: EpistemicDependencyEdgeKind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpistemicDependencyGraph {
    pub nodes: Vec<EpistemicDependencyNode>,
    pub edges: Vec<EpistemicDependencyEdge>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum EvidenceContextMode {
    #[default]
    Normal,
    BlindToSelectedExperience {
        excluded: Vec<ExperienceRef>,
    },
    IndependentRetrieval,
    AlternativeTooling,
    AlternativeEvaluator,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpistemicContextFingerprint {
    pub hash: String,
    pub model_family: Option<String>,
    pub active_experience_hash: String,
    pub retrieval_source_hash: String,
    pub toolset_hash: String,
    pub evaluator_hash: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceContext {
    #[serde(default)]
    pub mode: EvidenceContextMode,
    pub repository: Option<String>,
    pub task: Option<String>,
    #[serde(default)]
    pub injected_experience: Vec<ExperienceRef>,
    pub active_experience_package: Option<String>,
    #[serde(default)]
    pub visible_federated_evidence: Vec<EvidenceRef>,
    #[serde(default)]
    pub available_tools: Vec<String>,
    #[serde(default)]
    pub evaluators: Vec<EvaluatorIdentity>,
    #[serde(default)]
    pub root_evidence_origins: Vec<String>,
    #[serde(default)]
    pub fingerprint: EpistemicContextFingerprint,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvidencePath {
    pub id: EvidencePathId,
    pub claim: ClaimRef,
    pub source: EvidenceSource,
    pub context: EvidenceContext,
    pub dependencies: EpistemicDependencySet,
    #[serde(default)]
    pub evidence_refs: Vec<EvidenceRef>,
    pub outcome: EvidenceOutcome,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyOverlap {
    pub kind: EpistemicDependencyKind,
    pub shared_value: String,
    pub paths: Vec<EvidencePathId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiversityClass {
    Unknown,
    Low,
    Moderate,
    High,
}

impl DiversityClass {
    pub fn satisfies(self, minimum: Self) -> bool {
        self >= minimum
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceDiversityAssessment {
    pub path_count: usize,
    pub source_type_count: usize,
    pub model_family_count: usize,
    pub agent_runtime_count: usize,
    pub retrieval_source_count: usize,
    pub evaluator_count: usize,
    pub environment_count: usize,
    pub dependency_overlaps: Vec<DependencyOverlap>,
    pub diversity_class: DiversityClass,
    #[serde(default)]
    pub duplicate_fingerprints: BTreeMap<String, Vec<EvidencePathId>>,
    #[serde(default)]
    pub missing_metadata: Vec<String>,
    #[serde(default)]
    pub caveats: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FusedEvidenceStatus {
    WeakSupport,
    Supported,
    DiverseSupport,
    Disputed,
    Inconclusive,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FusedEvidenceAssessment {
    pub claim: ClaimId,
    pub support_paths: Vec<EvidencePathId>,
    pub contradiction_paths: Vec<EvidencePathId>,
    pub inconclusive_paths: Vec<EvidencePathId>,
    pub diversity: EvidenceDiversityAssessment,
    pub status: FusedEvidenceStatus,
    pub caveats: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpistemicFaultDomain {
    pub id: EpistemicFaultDomainId,
    pub kind: EpistemicDependencyKind,
    pub dependency: String,
    pub members: Vec<EvidencePathId>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginDiversity {
    pub immediate_nodes: usize,
    pub root_evidence_origins: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceEchoStatus {
    None,
    Possible,
    Strong,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvidenceEchoAssessment {
    pub origin_diversity: OriginDiversity,
    pub dominant_root: Option<String>,
    pub dominant_root_paths: usize,
    pub status: EvidenceEchoStatus,
    pub caveats: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChallengeStrategy {
    RemoveDominantExperience,
    AlternativeAgent,
    AlternativeEvaluator,
    AlternativeTool,
    AlternativeEnvironment,
    CounterfactualExperiment,
    AdversarialPerturbation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FederatedObjectRef {
    pub node_id: ExperienceNodeId,
    pub object_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum EvidenceAcquisitionAction {
    AskAgent {
        agent: AgentIdentity,
        context_mode: EvidenceContextMode,
    },
    RunControlledExperiment {
        request: Box<ExperimentRequest>,
    },
    RunAlternativeEvaluator {
        evaluator: String,
    },
    ReproduceFederatedEvidence {
        object: FederatedObjectRef,
    },
    ChallengeClaim {
        strategy: ChallengeStrategy,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvidenceAcquisitionPlan {
    pub claim: ClaimId,
    pub actions: Vec<EvidenceAcquisitionAction>,
    pub rationale: Vec<String>,
    pub requirement_satisfied: bool,
    pub stop_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvidenceAcquisitionBudget {
    pub experience_budget: ExperienceBudget,
    pub max_agents: usize,
    pub max_challenges: usize,
}

impl Default for EvidenceAcquisitionBudget {
    fn default() -> Self {
        Self {
            experience_budget: ExperienceBudget::default(),
            max_agents: 2,
            max_challenges: 3,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentCapabilityProfile {
    pub identity: AgentIdentity,
    pub model_family: Option<String>,
    pub runtime: String,
    pub available: bool,
    pub integration_mode: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeDiversityRequirement {
    pub action_pattern: ActionPattern,
    pub minimum_diversity: DiversityClass,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvidenceRequirement {
    pub claim: ClaimId,
    pub required_diversity: Option<DiversityClass>,
    #[serde(default)]
    pub preferred_sources: Vec<EvidenceSourceKind>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvidenceQuorumPolicy {
    pub minimum_supporting_paths: usize,
    pub minimum_fault_domains: usize,
    pub require_controlled_empirical_path: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSessionOrigin {
    Runtime,
    User,
    Curriculum,
    Assurance,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvidenceSession {
    pub id: EvidenceSessionId,
    pub claim: ClaimId,
    pub requested_by: EvidenceSessionOrigin,
    pub paths: Vec<EvidencePathId>,
    pub acquisition_plan: EvidenceAcquisitionPlan,
    pub fused: Option<FusedEvidenceAssessment>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InfluenceOutcome {
    Successful,
    Failed,
    Inconclusive,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperienceInfluence {
    pub lesson_id: LessonId,
    pub session: String,
    pub agent: AgentIdentity,
    pub repository: String,
    pub decision: Option<RuntimeDecisionId>,
    pub outcome: InfluenceOutcome,
    pub observed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperienceBlastRadius {
    pub sessions_influenced: usize,
    pub agents_influenced: usize,
    pub repositories_influenced: usize,
    pub decisions_influenced: usize,
    pub successful: usize,
    pub failed: usize,
    pub inconclusive: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperienceActivationState {
    #[default]
    Active,
    Advisory,
    Quarantined,
    Disabled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperienceQuarantineEvent {
    pub lesson_id: LessonId,
    pub state: ExperienceActivationState,
    pub reason: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LessonImpactAssessment {
    pub lesson_id: LessonId,
    pub blast_radius: ExperienceBlastRadius,
    pub activation_state: ExperienceActivationState,
    pub revalidation_required: bool,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvidenceContrast {
    pub left: EvidencePathId,
    pub right: EvidencePathId,
    pub shared: Vec<DependencyValue>,
    pub different: Vec<EpistemicDependencyKind>,
    pub missing: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EpistemicReport {
    pub claim: Claim,
    pub paths: Vec<EvidencePath>,
    pub graph: EpistemicDependencyGraph,
    pub diversity: EvidenceDiversityAssessment,
    pub domains: Vec<EpistemicFaultDomain>,
    pub fused: FusedEvidenceAssessment,
    pub echoes: EvidenceEchoAssessment,
    pub gaps: Vec<String>,
    pub challenge: EvidenceAcquisitionPlan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpistemicBenchmarkArm {
    SingleAgent,
    NaiveMajority,
    DiversityAware,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EpistemicBenchmarkMetrics {
    pub task_success_rate: f64,
    pub correlated_error_escape_rate: f64,
    pub common_mode_detection_rate: f64,
    pub redundant_agent_run_rate: f64,
    pub diversity_challenge_precision: f64,
    pub agent_runs_per_decision: f64,
    pub experiments_per_decision: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EpistemicBenchmarkResult {
    pub arms: BTreeMap<EpistemicBenchmarkArm, EpistemicBenchmarkMetrics>,
    pub dependency_graph_build_micros: u128,
    pub diversity_assessment_micros: u128,
    pub fusion_micros: u128,
}
