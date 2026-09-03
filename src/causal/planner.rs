// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{
    Error, Result,
    budget::ExperienceBudget,
    capability::{EffectControlLevel, RealityRequirements},
    core::*,
    epistemic::{DiversityClass, EvidenceDiversityAssessment},
    experimentation::ExperimentQuality,
};
use std::collections::{BTreeMap, BTreeSet};

pub trait InterventionPlanner {
    fn plan(
        &self,
        hypotheses: &[CausalHypothesis],
        context: &CausalPlanningContext,
        budget: &ExperienceBudget,
    ) -> Result<InterventionPlan>;
}
#[derive(Default)]
pub struct DeterministicInterventionPlanner {
    pub budget: CausalBudget,
}
pub fn requirements_met(r: &RealityRequirements, c: &RealityCapabilities) -> bool {
    c.filesystem_isolation >= r.filesystem_isolation
        && c.process_isolation >= r.process_isolation
        && c.network_isolation >= r.network_isolation
        && c.credential_isolation >= r.credential_isolation
        && (!r.effect_gating || c.external_effect_control == EffectControlLevel::Gated)
}
impl InterventionPlanner for DeterministicInterventionPlanner {
    fn plan(
        &self,
        hypotheses: &[CausalHypothesis],
        context: &CausalPlanningContext,
        budget: &ExperienceBudget,
    ) -> Result<InterventionPlan> {
        let mut plans = vec![];
        let mut testable = BTreeSet::new();
        context.evaluator.validate()?;
        for cap in &context.available_interventions {
            let Some(variable) = context.variables.iter().find(|v| v.id == cap.variable) else {
                return Err(Error::InvalidInput("Unknown intervention variable".into()));
            };
            if !variable.intervenable
                || !requirements_met(&cap.required_reality, &context.reality_capabilities)
            {
                continue;
            }
            let Some(from) = context.values.get(&variable.id) else {
                continue;
            };
            for to in &cap.supported_values {
                if !variable.domain.contains(to) || from == to {
                    continue;
                }
                let active: Vec<_> = hypotheses
                    .iter()
                    .filter(|h| {
                        h.status != CausalHypothesisStatus::Retired
                            && h.conditions.iter().all(|c| {
                                context
                                    .values
                                    .get(&c.variable)
                                    .is_some_and(|v| c.predicate.matches(v))
                            })
                    })
                    .collect();
                let targets: Vec<_> = active.iter().filter(|h| h.cause == variable.id).collect();
                if targets.is_empty() {
                    continue;
                }
                testable.extend(targets.iter().map(|h| h.id.clone()));
                let predictions: Vec<_> = active
                    .iter()
                    .map(|h| HypothesisPrediction {
                        hypothesis: h.id.clone(),
                        expected: if h.cause == variable.id {
                            h.intervention_prediction
                        } else {
                            h.baseline_prediction
                        },
                    })
                    .collect();
                let separated_pairs = predictions
                    .iter()
                    .enumerate()
                    .map(|(i, a)| {
                        predictions[i + 1..]
                            .iter()
                            .filter(|b| {
                                a.expected != b.expected
                                    && a.expected != PredictedOutcome::Unknown
                                    && b.expected != PredictedOutcome::Unknown
                            })
                            .count()
                    })
                    .sum();
                plans.push(HypothesisDiscrimination { intervention: Intervention { id: InterventionId::new(), variable: variable.id.clone(), from: Some(from.clone()), to: to.clone(), held_constant: context.values.keys().filter(|id| **id != variable.id).cloned().collect(), rationale: format!("Vary {} alone; categorical predictions separate {separated_pairs} competing pairs. Predictions are not evidence.", variable.name) }, predictions, separated_pairs });
            }
        }
        // Stable semantic tie break, independent of randomly generated IDs.
        plans.sort_by_key(|p| {
            (
                std::cmp::Reverse(p.separated_pairs),
                !context.variables.iter().any(|v| {
                    v.id == p.intervention.variable && v.kind == CausalVariableKind::State
                }),
                context
                    .variables
                    .iter()
                    .find(|v| v.id == p.intervention.variable)
                    .map(|v| v.name.clone()),
                p.intervention.to.literal(),
            )
        });
        let limit = if self.budget.max_duration.is_some_and(|d| d.is_zero()) {
            0
        } else {
            self.budget.max_interventions
        };
        if budget.max_realities < 2
            || budget.max_agent_runs < 2
            || self.budget.max_variables_per_intervention == 0
        {
            plans.clear();
        } else {
            plans.truncate(limit);
        }
        Ok(InterventionPlan { experiments: plans, untestable: hypotheses.iter().filter(|h| !testable.contains(&h.id)).map(|h| h.id.clone()).collect(), caveats: vec!["Explicit known variables only; unknown confounders may remain. Single-variable interventions only; interactions require conditional hypotheses and separate controlled pairs.".into(), "Budget exhaustion is not evidence of untestability. Worktree isolation is cooperative, not a production sandbox.".into()] })
    }
}

pub fn assess_quality(
    equivalent_start: bool,
    evaluator_consistent: bool,
    changed: &[CausalVariableId],
    held: &[CausalVariableId],
    confounders: &[KnownConfounder],
    engine_quality: ExperimentQuality,
) -> CausalExperimentQuality {
    let uncontrolled: Vec<_> = confounders
        .iter()
        .filter(|c| {
            !c.controlled || (!held.contains(&c.variable) && !changed.contains(&c.variable))
        })
        .map(|c| c.variable.clone())
        .collect();
    let quality = if !equivalent_start
        || !evaluator_consistent
        || engine_quality == ExperimentQuality::Invalid
        || changed.is_empty()
    {
        ExperimentQuality::Invalid
    } else if changed.len() > 1
        || !uncontrolled.is_empty()
        || engine_quality == ExperimentQuality::Confounded
    {
        ExperimentQuality::Confounded
    } else if engine_quality != ExperimentQuality::Controlled {
        ExperimentQuality::PartiallyControlled
    } else {
        ExperimentQuality::Controlled
    };
    CausalExperimentQuality {
        equivalent_start,
        evaluator_consistent,
        intervention_isolated: changed.len() == 1,
        changed_variable_count: changed.len(),
        known_confounders_controlled: uncontrolled.is_empty(),
        uncontrolled_variables: uncontrolled,
        quality,
    }
}

pub fn classify_evidence(
    h: &CausalHypothesis,
    intervention: &Intervention,
    baseline: PredictedOutcome,
    outcome: PredictedOutcome,
    quality: ExperimentQuality,
) -> CausalEvidenceOutcome {
    if quality == ExperimentQuality::Invalid {
        return CausalEvidenceOutcome::Invalid;
    }
    if quality != ExperimentQuality::Controlled
        || h.cause != intervention.variable
        || baseline != h.baseline_prediction
        || !matches!(baseline, PredictedOutcome::Pass | PredictedOutcome::Fail)
        || !matches!(outcome, PredictedOutcome::Pass | PredictedOutcome::Fail)
        || !matches!(
            h.intervention_prediction,
            PredictedOutcome::Pass | PredictedOutcome::Fail
        )
        || h.intervention_prediction == baseline
    {
        return CausalEvidenceOutcome::Inconclusive;
    }
    if outcome != h.intervention_prediction {
        return CausalEvidenceOutcome::Contradicts;
    }
    // A single pair cannot establish necessity, sufficiency, mediation or risk magnitude.
    if !matches!(h.claim, CausalClaim::Causes | CausalClaim::Prevents) {
        return CausalEvidenceOutcome::Inconclusive;
    }
    CausalEvidenceOutcome::Supports
}

pub trait CausalSupportPolicy {
    fn evaluate(
        &self,
        hypothesis: &CausalHypothesis,
        evidence: &[CausalEvidence],
        diversity: Option<&EvidenceDiversityAssessment>,
    ) -> CausalHypothesisStatus;
}
pub struct DeterministicCausalSupportPolicy {
    pub minimum_support: usize,
    pub strong_replications: usize,
    pub strong_diversity: DiversityClass,
}
impl Default for DeterministicCausalSupportPolicy {
    fn default() -> Self {
        Self {
            minimum_support: 1,
            strong_replications: 3,
            strong_diversity: DiversityClass::Moderate,
        }
    }
}
impl CausalSupportPolicy for DeterministicCausalSupportPolicy {
    fn evaluate(
        &self,
        h: &CausalHypothesis,
        evidence: &[CausalEvidence],
        diversity: Option<&EvidenceDiversityAssessment>,
    ) -> CausalHypothesisStatus {
        if h.status == CausalHypothesisStatus::Retired {
            return h.status;
        }
        let valid: Vec<_> = evidence
            .iter()
            .filter(|e| {
                e.hypothesis_id == h.id
                    && e.kind == CausalEvidenceKind::Interventional
                    && e.experiment_quality == ExperimentQuality::Controlled
            })
            .collect();
        // Contradictions stay visible; support never outvotes one controlled counterexample.
        if valid
            .iter()
            .any(|e| e.outcome == CausalEvidenceOutcome::Contradicts)
        {
            return CausalHypothesisStatus::Contradicted;
        }
        let supports = valid
            .iter()
            .filter(|e| e.outcome == CausalEvidenceOutcome::Supports)
            .map(|e| e.intervention_trial.experiment.clone())
            .collect::<BTreeSet<_>>()
            .len();
        if supports >= self.minimum_support.max(1) {
            if supports >= self.strong_replications.max(2)
                && diversity.is_some_and(|d| d.diversity_class.satisfies(self.strong_diversity))
            {
                CausalHypothesisStatus::StronglySupported
            } else {
                CausalHypothesisStatus::Supported
            }
        } else if !evidence.is_empty() {
            CausalHypothesisStatus::Inconclusive
        } else {
            h.status
        }
    }
}

pub fn differing_conditions(
    evidence: &[CausalEvidence],
) -> BTreeMap<CausalVariableId, Vec<VariableValue>> {
    let mut values: BTreeMap<CausalVariableId, Vec<VariableValue>> = BTreeMap::new();
    for e in evidence {
        for (id, value) in &e.conditions {
            let items = values.entry(id.clone()).or_default();
            if !items.contains(value) {
                items.push(value.clone());
            }
        }
    }
    values.retain(|_, values| values.len() > 1);
    values
}
