// SPDX-License-Identifier: Apache-2.0
use crate::{
    core::*,
    experimentation::{ExperimentQuality, ExperimentStartingState, StartingStateProof},
    lesson::ContextSelector,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

pub type CausalVariableRef = CausalVariableId;
pub type CausalHypothesisRef = CausalHypothesisId;
pub type CausalEvidenceRef = CausalEvidenceId;
pub type InterventionRef = InterventionId;
pub type RealityCapabilities = crate::capability::RealityProviderCapabilities;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum VariableValue {
    Boolean(bool),
    Integer(i64),
    Float(f64),
    Text(String),
}
impl VariableValue {
    pub fn literal(&self) -> String {
        match self {
            Self::Boolean(v) => v.to_string(),
            Self::Integer(v) => v.to_string(),
            Self::Float(v) => v.to_string(),
            Self::Text(v) => v.clone(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VariableDomain {
    Boolean,
    Categorical { values: Vec<String> },
    IntegerRange { min: i64, max: i64 },
    FloatRange { min: f64, max: f64 },
    Custom { description: String },
}
impl VariableDomain {
    pub fn contains(&self, v: &VariableValue) -> bool {
        match (self, v) {
            (Self::Boolean, VariableValue::Boolean(_)) => true,
            (Self::Categorical { values }, VariableValue::Text(v)) => values.contains(v),
            (Self::IntegerRange { min, max }, VariableValue::Integer(v)) => min <= v && v <= max,
            (Self::FloatRange { min, max }, VariableValue::Float(v)) => {
                v.is_finite() && min.is_finite() && max.is_finite() && min <= v && v <= max
            }
            _ => false,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CausalVariableKind {
    Action,
    Environment,
    Configuration,
    State,
    Perturbation,
    Tool,
    Agent,
    Outcome,
    Intermediate,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CausalVariable {
    pub id: CausalVariableId,
    pub name: String,
    pub kind: CausalVariableKind,
    pub domain: VariableDomain,
    pub observable: bool,
    pub intervenable: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum VariablePredicate {
    Equals(VariableValue),
    NotEquals(VariableValue),
    OneOf(Vec<VariableValue>),
}
impl VariablePredicate {
    pub fn matches(&self, v: &VariableValue) -> bool {
        match self {
            Self::Equals(x) => x == v,
            Self::NotEquals(x) => x != v,
            Self::OneOf(xs) => xs.contains(v),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CausalCondition {
    pub variable: CausalVariableRef,
    pub predicate: VariablePredicate,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CausalClaim {
    Causes,
    Prevents,
    IncreasesRisk,
    DecreasesRisk,
    NecessaryUnderScope,
    SufficientUnderScope,
    Mediates,
    Custom(String),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CausalHypothesisStatus {
    Candidate,
    Testable,
    Supported,
    StronglySupported,
    Contradicted,
    Inconclusive,
    Untestable,
    Retired,
}
impl CausalHypothesisStatus {
    pub fn supported(self) -> bool {
        matches!(self, Self::Supported | Self::StronglySupported)
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CausalHypothesisOrigin {
    Reflection,
    AgentSuggestion,
    HumanSpecified,
    ExperimentComparison,
    ChaosCampaign,
    ContradictionAnalysis,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredictedOutcome {
    Pass,
    Fail,
    Degraded,
    NoChange,
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteCausalOrigin {
    pub node: String,
    pub root_experiment: String,
    pub reported_status: CausalHypothesisStatus,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CausalHypothesis {
    pub id: CausalHypothesisId,
    pub statement: String,
    pub claim: CausalClaim,
    pub cause: CausalVariableRef,
    pub effect: CausalVariableRef,
    pub scope: ContextSelector,
    #[serde(default)]
    pub conditions: Vec<CausalCondition>,
    pub status: CausalHypothesisStatus,
    #[serde(default)]
    pub evidence: Vec<CausalEvidenceRef>,
    pub origin: CausalHypothesisOrigin,
    /// Explicit predictions, not hidden reasoning or evidence.
    pub baseline_prediction: PredictedOutcome,
    pub intervention_prediction: PredictedOutcome,
    #[serde(default)]
    pub remote_origin: Option<RemoteCausalOrigin>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialRef {
    pub experiment: ExperimentId,
    pub candidate: CandidateId,
    pub experience: ExperienceId,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Intervention {
    pub id: InterventionId,
    pub variable: CausalVariableRef,
    pub from: Option<VariableValue>,
    pub to: VariableValue,
    pub held_constant: Vec<CausalVariableRef>,
    pub rationale: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnownConfounder {
    pub variable: CausalVariableRef,
    pub reason: String,
    pub controlled: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CausalExperimentQuality {
    pub equivalent_start: bool,
    pub intervention_isolated: bool,
    pub evaluator_consistent: bool,
    pub changed_variable_count: usize,
    pub known_confounders_controlled: bool,
    pub uncontrolled_variables: Vec<CausalVariableRef>,
    pub quality: ExperimentQuality,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CounterfactualPair {
    pub id: CounterfactualPairId,
    pub starting_state: StartingStateProof,
    pub baseline: TrialRef,
    pub intervention: TrialRef,
    pub changed_variables: Vec<CausalVariableRef>,
    pub held_constant: Vec<CausalVariableRef>,
    pub quality: CausalExperimentQuality,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CausalEvidenceOutcome {
    Supports,
    Contradicts,
    Inconclusive,
    Invalid,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CausalEvidenceKind {
    Observational,
    Interventional,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CausalEvidence {
    pub id: CausalEvidenceId,
    pub hypothesis_id: CausalHypothesisId,
    pub intervention: InterventionRef,
    pub baseline_trial: Option<TrialRef>,
    pub intervention_trial: TrialRef,
    pub pair: CounterfactualPairId,
    pub outcome: CausalEvidenceOutcome,
    pub kind: CausalEvidenceKind,
    pub experiment_quality: ExperimentQuality,
    pub baseline_outcome: PredictedOutcome,
    pub intervention_outcome: PredictedOutcome,
    pub context: ContextSelector,
    pub conditions: BTreeMap<CausalVariableId, VariableValue>,
    pub dependencies: crate::epistemic::EpistemicDependencySet,
    pub created_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HypothesisPrediction {
    pub hypothesis: CausalHypothesisId,
    pub expected: PredictedOutcome,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HypothesisDiscrimination {
    pub intervention: Intervention,
    pub predictions: Vec<HypothesisPrediction>,
    pub separated_pairs: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InterventionCapability {
    pub variable: CausalVariableRef,
    pub supported_values: Vec<VariableValue>,
    pub required_reality: crate::capability::RealityRequirements,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CausalPlanningContext {
    pub starting_state: StartingStateProof,
    pub variables: Vec<CausalVariable>,
    pub values: BTreeMap<CausalVariableId, VariableValue>,
    pub available_interventions: Vec<InterventionCapability>,
    pub evaluator: crate::evaluation::EvaluationSpec,
    pub known_confounders: Vec<KnownConfounder>,
    pub reality_capabilities: RealityCapabilities,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CausalBudget {
    pub max_interventions: usize,
    pub max_variables_per_intervention: usize,
    pub max_duration: Option<Duration>,
}
impl Default for CausalBudget {
    fn default() -> Self {
        Self {
            max_interventions: 3,
            max_variables_per_intervention: 1,
            max_duration: Some(Duration::from_secs(300)),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InterventionPlan {
    pub experiments: Vec<HypothesisDiscrimination>,
    pub untestable: Vec<CausalHypothesisId>,
    pub caveats: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "target", rename_all = "snake_case")]
pub enum CausalTarget {
    FailureSignature(String),
    Lesson(LessonId),
    Claim(ClaimId),
    RuntimeDecision(RuntimeDecisionId),
    Outcome(String),
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationStatus {
    Open,
    Testing,
    Narrowed,
    ResolvedUnderScope,
    Inconclusive,
    Closed,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CausalInvestigation {
    pub id: CausalInvestigationId,
    pub target: CausalTarget,
    pub hypotheses: Vec<CausalHypothesisId>,
    pub interventions: Vec<InterventionId>,
    pub evidence: Vec<CausalEvidenceId>,
    pub status: InvestigationStatus,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CausalEdge {
    pub hypothesis: CausalHypothesisId,
    pub cause: CausalVariableRef,
    pub effect: CausalVariableRef,
    pub conditions: Vec<CausalCondition>,
    #[serde(default)]
    pub tested_inputs: Vec<BTreeMap<CausalVariableId, VariableValue>>,
    pub status: CausalHypothesisStatus,
    pub evidence: Vec<CausalEvidenceRef>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextualCausalModel {
    pub id: CausalModelId,
    pub scope: ContextSelector,
    pub variables: Vec<CausalVariable>,
    pub edges: Vec<CausalEdge>,
    pub known_unknowns: Vec<CausalGap>,
    pub revision: u64,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CausalGapReason {
    Untested,
    Unintervenable,
    Confounded,
    Contradictory,
    InsufficientEvidence,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CausalGap {
    pub description: String,
    pub related_variables: Vec<CausalVariableRef>,
    pub reason: CausalGapReason,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompetingHypothesisSet {
    pub id: String,
    pub effect: CausalVariableRef,
    pub hypotheses: Vec<CausalHypothesisId>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InteractionHypothesis {
    pub variables: Vec<CausalVariableRef>,
    pub effect: CausalVariableRef,
    pub scope: ContextSelector,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnvelopeObservation {
    pub conditions: BTreeMap<String, VariableValue>,
    pub outcome: PredictedOutcome,
    pub evidence: Vec<TrialRef>,
}

/// User-authored, trusted local fixture adapter. Only input literals differ between candidates.
/// The worktree backend is NOT a shell security sandbox.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CausalTestSpec {
    pub starting_state: ExperimentStartingState,
    pub variables: Vec<CausalVariable>,
    pub baseline: BTreeMap<CausalVariableId, VariableValue>,
    pub bindings: BTreeMap<CausalVariableId, PathBuf>,
    pub command: String,
    pub evaluator: crate::evaluation::EvaluationSpec,
    pub scope: ContextSelector,
    pub available_interventions: Vec<InterventionCapability>,
    #[serde(default)]
    pub known_confounders: Vec<KnownConfounder>,
    #[serde(default)]
    pub budget: crate::budget::ExperienceBudget,
    #[serde(default)]
    pub causal_budget: CausalBudget,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CausalInvestigationInput {
    pub target: CausalTarget,
    pub hypotheses: Vec<CausalHypothesis>,
    pub spec: CausalTestSpec,
    #[serde(default)]
    pub source_experiences: Vec<ExperienceId>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CausalObservation {
    pub experience: ExperienceId,
    pub values: BTreeMap<CausalVariableId, VariableValue>,
    pub outcome: crate::experience::Outcome,
}

/// Only declared observable facts are extracted. Rationale is not parsed as evidence.
pub fn extract_causal_observation(
    experience: &crate::experience::Experience,
    variables: &[CausalVariable],
) -> CausalObservation {
    let values = variables
        .iter()
        .filter(|v| v.observable)
        .filter_map(|v| {
            let raw = experience.context.environment.facts.get(&v.name)?;
            let value = match &v.domain {
                VariableDomain::Boolean => VariableValue::Boolean(raw.parse().ok()?),
                VariableDomain::IntegerRange { .. } => VariableValue::Integer(raw.parse().ok()?),
                VariableDomain::FloatRange { .. } => VariableValue::Float(raw.parse().ok()?),
                VariableDomain::Categorical { .. } => VariableValue::Text(raw.clone()),
                VariableDomain::Custom { .. } => return None,
            };
            v.domain.contains(&value).then(|| (v.id.clone(), value))
        })
        .collect();
    CausalObservation {
        experience: experience.id.clone(),
        values,
        outcome: experience.outcome,
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CausalRun {
    pub investigation: CausalInvestigationId,
    pub discrimination: HypothesisDiscrimination,
    pub request: crate::experimentation::ExperimentRequest,
    pub baseline: BTreeMap<CausalVariableId, VariableValue>,
    pub changed: BTreeMap<CausalVariableId, VariableValue>,
    pub known_confounders: Vec<KnownConfounder>,
    pub scope: ContextSelector,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum CausalArtifact {
    Lesson(LessonId),
    Reflex(ReflexId),
    Recovery(RecoveryId),
    RuntimeDecision(RuntimeDecisionId),
    Certification(SkillCertificationId),
    Skill(SkillId),
}
impl CausalArtifact {
    pub fn key(&self) -> String {
        match self {
            Self::Lesson(x) => x.to_string(),
            Self::Reflex(x) => x.to_string(),
            Self::Recovery(x) => x.to_string(),
            Self::RuntimeDecision(x) => x.to_string(),
            Self::Certification(x) => x.to_string(),
            Self::Skill(x) => x.to_string(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CausalArtifactDependency {
    pub hypothesis: CausalHypothesisId,
    pub artifact: CausalArtifact,
    pub intervention: Option<InterventionId>,
    pub severity: crate::curriculum::Severity,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CausalRevalidation {
    pub hypothesis: CausalHypothesisId,
    pub artifact: CausalArtifact,
    pub reason: String,
    pub automatic_guidance_quarantined: bool,
    pub created_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LessonRevisionCandidate {
    pub investigation: CausalInvestigationId,
    pub hypothesis: CausalHypothesisId,
    pub intervention: InterventionId,
    pub scope: ContextSelector,
    pub conditions: Vec<CausalCondition>,
    pub tested_inputs: BTreeMap<CausalVariableId, VariableValue>,
    pub lesson_guidance: String,
    pub reflex_guidance: String,
    pub recovery_guidance: String,
    pub requires_existing_validation: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InterventionRecommendation {
    pub hypothesis: CausalHypothesisId,
    pub intervention: Intervention,
    pub controlled_pairs: usize,
    pub recovery: Option<RecoveryId>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CausalRuntimeGuidance {
    pub applicable_hypotheses: Vec<CausalHypothesisRef>,
    pub supported_interventions: Vec<InterventionRecommendation>,
    pub causal_gaps: Vec<CausalGap>,
}
impl CausalRuntimeGuidance {
    pub fn is_empty(&self) -> bool {
        self.applicable_hypotheses.is_empty()
            && self.supported_interventions.is_empty()
            && self.causal_gaps.is_empty()
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CausalEventKind {
    CausalHypothesisCreated,
    CausalInterventionPlanned,
    CausalInterventionStarted,
    CausalEvidenceRecorded,
    CausalHypothesisSupported,
    CausalHypothesisContradicted,
    CausalModelRevised,
    CausalArtifactRevalidationRequested,
}

pub trait CausalHypothesisProvider {
    fn propose(
        &self,
        evidence: &[crate::experience::Experience],
    ) -> crate::Result<Vec<CausalHypothesis>>;
}
pub struct FixtureHypothesisProvider(pub Vec<CausalHypothesis>);
impl CausalHypothesisProvider for FixtureHypothesisProvider {
    fn propose(&self, _: &[crate::experience::Experience]) -> crate::Result<Vec<CausalHypothesis>> {
        Ok(self
            .0
            .iter()
            .cloned()
            .map(|mut h| {
                h.status = CausalHypothesisStatus::Candidate;
                h.evidence.clear();
                h
            })
            .collect())
    }
}
