// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{
    Error, Result, budget::ExperienceBudget, core::*, curriculum::*, economics::*,
    experimentation::ExperimentRequest, hierarchy::*,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictResolutionTarget {
    ValidateSpecialization,
    ValidateException,
    NarrowScope,
    SupersedeArtifact,
    RetireArtifact,
    PreserveConflict,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConflictHypothesis {
    pub artifact: KnowledgeArtifactRef,
    pub statement: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeConflictResolutionPlan {
    pub conflict: KnowledgeConflictId,
    pub hypotheses: Vec<ConflictHypothesis>,
    pub experiments: Vec<ExperimentRequest>,
    pub expected_resolution: ConflictResolutionTarget,
    pub goal: CurriculumGoal,
    pub mechanism: String,
    pub reason: String,
}
pub trait KnowledgeConflictPlanner {
    fn plan(
        &self,
        conflict: &KnowledgeConflict,
        context: &KnowledgeContext,
        budget: &ExperienceBudget,
    ) -> Result<KnowledgeConflictResolutionPlan>;
}
#[derive(Default)]
pub struct DefaultKnowledgeConflictPlanner {
    pub experiment_template: Option<ExperimentRequest>,
}
impl KnowledgeConflictPlanner for DefaultKnowledgeConflictPlanner {
    fn plan(
        &self,
        c: &KnowledgeConflict,
        context: &KnowledgeContext,
        budget: &ExperienceBudget,
    ) -> Result<KnowledgeConflictResolutionPlan> {
        let mut experiments = vec![];
        if let Some(template) = &self.experiment_template {
            if budget.max_realities < 2
                || template.candidates.len() < 2
                || template.candidates.len() > budget.max_realities
            {
                return Err(Error::InvalidInput(
                    "Conflict comparison requires a baseline and counterfactual within budget"
                        .into(),
                ));
            }
            template.evaluator.validate()?;
            if template.evaluator.checks.is_empty() {
                return Err(Error::InvalidInput(
                    "Conflict comparison requires a concrete evaluator".into(),
                ));
            }
            let mut request = template.clone();
            request.id = ExperimentRequestId::new();
            request.question = format!("Resolve {:?}: {}", c.kind, c.reason);
            request.budget = budget.clone();
            request.intent = crate::experimentation::ExperimentIntent::CompareStrategies;
            experiments.push(request);
        }
        let kind = match c.kind {
            KnowledgeConflictKind::CompetingExceptions => CurriculumGoalKind::ValidateException,
            KnowledgeConflictKind::CompetingSpecializations => {
                CurriculumGoalKind::ValidateSpecialization
            }
            _ => CurriculumGoalKind::ResolveKnowledgeConflict,
        };
        let executable = !experiments.is_empty();
        Ok(KnowledgeConflictResolutionPlan{conflict:KnowledgeConflictId::new(),hypotheses:c.artifacts.iter().map(|a|ConflictHypothesis{artifact:a.clone(),statement:format!("{} is supported in the observed scope",a.id)}).collect(),experiments,expected_resolution:if executable{ConflictResolutionTarget::NarrowScope}else{ConflictResolutionTarget::PreserveConflict},goal:CurriculumGoal{id:CurriculumGoalId::new(),kind,description:c.reason.clone(),priority:Priority::High,score:PriorityScore{score:80,priority:Priority::High,explanation:"Conflicting runtime guidance requires controlled scope discrimination".into()},evidence_gap:EvidenceGap{dimension:"knowledge_scope".into(),known_values:context.values.keys().cloned().collect(),unknown_values:c.artifacts.iter().map(|a|a.id.clone()).collect(),rationale:c.reason.clone()},status:if executable{GoalStatus::Planned}else{GoalStatus::Deferred},decision:if executable{CurriculumDecision::Approved}else{CurriculumDecision::RequiresApproval},reason:"Uses existing ExperimentRequest and CurriculumGoal; no hypothesis is selected by the planner".into(),severity:Severity::High,safety:TrialSafety::RequiresIsolation},mechanism:"transfer_scope_experiment".into(),reason:if executable{"Explicit paired experiment template compiled; comparison cannot grant authority".into()}else{"Supply a paired experiment template with a concrete evaluator; conflict remains unresolved".into()}})
    }
}
/// Causal conflicts use the existing V0.14 intervention planner and compiler.
pub fn plan_causal_conflict(
    store: &crate::store::Store,
    spec: &crate::causal::CausalTestSpec,
    hypotheses: &[crate::causal::CausalHypothesis],
    budget: &ExperienceBudget,
) -> Result<Vec<crate::causal::CausalRun>> {
    use crate::causal::InterventionPlanner;
    let context = crate::causal::planning_context(
        spec,
        store,
        &crate::bridge::config::Config::load(&store.home)?,
    )?;
    let plan = crate::causal::DeterministicInterventionPlanner::default()
        .plan(hypotheses, &context, budget)?;
    let id = CausalInvestigationId::new();
    plan.experiments
        .iter()
        .map(|d| crate::causal::compile_intervention(&id, spec, d))
        .collect()
}
/// Evidence-diversity disputes reuse the V0.13 challenge/acquisition planner.
pub fn plan_diversity_conflict(
    claim: &crate::epistemic::Claim,
    paths: &[crate::epistemic::EvidencePath],
    budget: &ExperienceBudget,
) -> Result<crate::epistemic::EvidenceAcquisitionPlan> {
    use crate::epistemic::EvidenceAcquisitionPlanner;
    crate::epistemic::DeterministicEvidenceAcquisitionPlanner::default().plan(claim, paths, budget)
}
pub fn conflict_opportunity(
    c: &StoredKnowledgeConflict,
    exposure: usize,
    budget: &ExperienceBudget,
) -> Result<ExperienceOpportunity> {
    let constraint = c.conflict.artifacts.iter().any(|a| {
        matches!(
            a.kind,
            KnowledgeArtifactKind::Constraint
                | KnowledgeArtifactKind::AbstractKnowledge(
                    crate::abstraction::AbstractKnowledgeKind::AbstractConstraint
                )
        )
    });
    let importance = if constraint {
        ValueBand::High
    } else {
        ValueBand::Low
    };
    let gap = ExperienceGap {
        kind: ExperienceOpportunityKind::ResolveContradiction,
        target: ExperienceOpportunityTarget::RuntimeGap(c.id.to_string()),
        reasons: vec![
            OpportunityReason::Custom("UnvalidatedKnowledgeOverride".into()),
            OpportunityReason::EvidenceContradicted,
        ],
        severity: if constraint {
            Severity::High
        } else {
            Severity::Low
        },
        exposure: if exposure >= 10 {
            ExposureBand::Frequent
        } else {
            ExposureBand::Rare
        },
        mitigation_gap: MitigationGap::Significant,
        learning: LearningValueEstimate {
            band: importance,
            possible_outcomes: vec![
                LearningOutcomeClass::NarrowScope,
                LearningOutcomeClass::ChangeRuntimeDecision,
            ],
            decision_changing_outcomes: 2,
            rationale: vec![c.conflict.reason.clone()],
        },
        decision_relevance: DecisionRelevance {
            affected_decisions: exposure,
            affected_task_families: 1,
            current_runtime_use: if exposure >= 10 {
                RuntimeUseBand::High
            } else {
                RuntimeUseBand::Low
            },
            likely_decision_change: importance,
        },
        reuse: ReusePotential::Reusable,
        novelty: EvidenceNovelty::ContextExtension,
        evidence: crate::economics::EvidenceSummary {
            contradictions: 1,
            ..Default::default()
        },
        estimated_cost: ExperimentCost {
            trials: budget.max_realities.min(3),
            ..Default::default()
        },
        risk: OpportunityRisk {
            trial_safety: TrialSafety::RequiresIsolation,
            external_effect_risk: crate::effects::EffectRisk::ReadOnly,
            isolation_required: crate::runtime::ExperimentCapabilitySummary::default().requirements,
            approval_required: false,
        },
        dependencies: vec![],
    };
    DeterministicExperienceOpportunityGenerator
        .generate(&ExperiencePlanningContext {
            gaps: vec![gap],
            completed_dependencies: Default::default(),
            objective: ExperiencePortfolioObjective::Balanced,
            now: Utc::now(),
        })?
        .pop()
        .ok_or_else(|| Error::InvalidInput("No conflict opportunity generated".into()))
}
