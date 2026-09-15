// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{
    Error, Result,
    assurance::BehavioralCondition,
    composition::{
        StateClaim, StateFreshnessRequirement, composition_hash, condition_value, trusted_state,
    },
    core::*,
    curriculum::Severity,
    runtime::RuntimeDecisionContext,
};
use chrono::{DateTime, Utc};
use std::collections::BTreeSet;

pub fn validate_plan(plan: &ExecutionPlan) -> Result<PlanDependencyIndex> {
    if plan.revision == 0
        || plan.steps.is_empty()
        || plan.steps.len() > 100
        || plan.goal.description.trim().is_empty()
        || plan.assumptions.len() > 1000
        || plan.invariants.len() > 1000
    {
        return Err(Error::InvalidInput("Plan requires a goal, positive revision, 1..100 steps and bounded assumptions/invariants".into()));
    }
    if let (Some(default), Some(critical), Some(commitment)) = (
        plan.freshness_policy.default_max_age,
        plan.freshness_policy.critical_max_age,
        plan.freshness_policy.commitment_point_max_age,
    ) {
        if commitment.is_zero() || critical < commitment || default < critical {
            return Err(Error::InvalidInput(
                "Freshness limits must be positive and stricter at commitment".into(),
            ));
        }
    } else {
        return Err(Error::InvalidInput(
            "Bounded plan freshness limits are required".into(),
        ));
    }
    let steps: BTreeSet<_> = plan.steps.iter().map(|s| s.id.clone()).collect();
    let assumptions: BTreeSet<_> = plan.assumptions.iter().map(|a| a.id.clone()).collect();
    let invariants: BTreeSet<_> = plan.invariants.iter().map(|i| i.id.clone()).collect();
    let checkpoints: BTreeSet<_> = plan.checkpoints.iter().map(|c| c.id.clone()).collect();
    let commitments: BTreeSet<_> = plan
        .commitment_points
        .iter()
        .map(|c| c.id.clone())
        .collect();
    if steps.len() != plan.steps.len()
        || assumptions.len() != plan.assumptions.len()
        || invariants.len() != plan.invariants.len()
        || checkpoints.len() != plan.checkpoints.len()
        || commitments.len() != plan.commitment_points.len()
    {
        return Err(Error::InvalidInput("Duplicate plan identities".into()));
    }
    for step in &plan.steps {
        if step
            .dependencies
            .iter()
            .any(|d| d == &step.id || !steps.contains(d))
            || step
                .required_assumptions
                .iter()
                .any(|a| !assumptions.contains(a))
            || step
                .required_invariants
                .iter()
                .any(|i| !invariants.contains(i))
        {
            return Err(Error::InvalidInput("Dangling plan dependency".into()));
        }
    }
    let mut done = BTreeSet::new();
    while done.len() < steps.len() {
        let next = plan
            .steps
            .iter()
            .find(|s| !done.contains(&s.id) && s.dependencies.iter().all(|d| done.contains(d)))
            .ok_or_else(|| Error::InvalidInput("Plan dependency cycle".into()))?;
        done.insert(next.id.clone());
    }
    for assumption in &plan.assumptions {
        if assumption.required_by.iter().any(|s| !steps.contains(s)) {
            return Err(Error::InvalidInput(
                "Assumption references a missing step".into(),
            ));
        }
    }
    for checkpoint in &plan.checkpoints {
        if checkpoint
            .after_step
            .as_ref()
            .is_some_and(|s| !steps.contains(s))
            || checkpoint
                .assumptions_to_revalidate
                .iter()
                .any(|a| !assumptions.contains(a))
            || checkpoint
                .invariants_to_verify
                .iter()
                .any(|i| !invariants.contains(i))
        {
            return Err(Error::InvalidInput(
                "Checkpoint has missing dependencies".into(),
            ));
        }
    }
    for point in &plan.commitment_points {
        if !steps.contains(&point.after_step)
            || point
                .assumptions_invalidated
                .iter()
                .any(|a| !assumptions.contains(a))
            || !plan
                .commitment_gates
                .iter()
                .any(|g| g.commitment_point == point.id)
        {
            return Err(Error::InvalidInput(
                "Commitment requires a valid step and explicit gate".into(),
            ));
        }
    }
    let mut gates = BTreeSet::new();
    for gate in &plan.commitment_gates {
        if !commitments.contains(&gate.commitment_point)
            || !gates.insert(gate.commitment_point.clone())
            || gate
                .required_assumptions
                .iter()
                .any(|a| !assumptions.contains(a))
            || gate
                .required_invariants
                .iter()
                .any(|i| !invariants.contains(i))
        {
            return Err(Error::InvalidInput("Invalid commitment gate".into()));
        }
    }
    for invariant in &plan.invariants {
        let valid = match &invariant.scope {
            PlanInvariantScope::EntirePlan => true,
            PlanInvariantScope::Between { start, end } => {
                let mut ancestors = BTreeSet::new();
                let mut pending = vec![end.clone()];
                while let Some(id) = pending.pop() {
                    if ancestors.insert(id.clone())
                        && let Some(step) = plan.steps.iter().find(|s| s.id == id)
                    {
                        pending.extend(step.dependencies.iter().cloned());
                    }
                }
                steps.contains(start)
                    && steps.contains(end)
                    && start != end
                    && ancestors.contains(start)
            }
            PlanInvariantScope::UntilCheckpoint(c) => checkpoints.contains(c),
            PlanInvariantScope::UntilCommitmentPoint(c)
            | PlanInvariantScope::AfterCommitmentPoint(c) => commitments.contains(c),
        };
        if !valid {
            return Err(Error::InvalidInput("Invalid plan invariant scope".into()));
        }
    }
    for dependency in &plan.knowledge_dependencies {
        if dependency
            .dependent_steps
            .iter()
            .any(|s| !steps.contains(s))
        {
            return Err(Error::InvalidInput(
                "Knowledge dependency references missing step".into(),
            ));
        }
    }
    let mut index = PlanDependencyIndex::default();
    for step in &plan.steps {
        let mut required: BTreeSet<_> = step.required_assumptions.iter().cloned().collect();
        required.extend(
            plan.assumptions
                .iter()
                .filter(|a| a.required_by.contains(&step.id))
                .map(|a| a.id.clone()),
        );
        for id in &required {
            index
                .steps_by_assumption
                .entry(id.clone())
                .or_default()
                .push(step.id.clone());
        }
        index
            .assumptions_by_step
            .insert(step.id.clone(), required.into_iter().collect());
        index
            .invariants_by_step
            .insert(step.id.clone(), step.required_invariants.clone());
    }
    Ok(index)
}
pub fn predicate_key(condition: &BehavioralCondition) -> Option<&str> {
    match condition {
        BehavioralCondition::StatePredicate { path, .. } => Some(path),
        _ => None,
    }
}
fn claim_fresh(
    claim: &StateClaim,
    requirement: &StateFreshnessRequirement,
    state: &PlanState,
    now: DateTime<Utc>,
    max_age: Option<std::time::Duration>,
) -> bool {
    if claim.freshness.observed_at > now
        || claim.freshness.expires_at.is_some_and(|e| e <= now)
        || claim
            .freshness
            .external_version
            .as_ref()
            .is_some_and(|v| state.external_versions.get(&v.resource) != Some(&v.version))
    {
        return false;
    }
    let age = now
        .signed_duration_since(claim.freshness.observed_at)
        .to_std()
        .unwrap_or_default();
    if max_age.is_some_and(|max| age > max) {
        return false;
    }
    match requirement {
        StateFreshnessRequirement::None => true,
        StateFreshnessRequirement::MaxAge(max) => age <= *max,
        StateFreshnessRequirement::SameStep | StateFreshnessRequirement::BeforeNextMutation => {
            state.observation_epochs.get(&claim.key) == Some(&state.mutation_epoch)
        }
        StateFreshnessRequirement::AuthoritativeRefreshRequired => {
            state.observation_epochs.get(&claim.key) == Some(&state.mutation_epoch)
                && matches!(
                    claim.source,
                    crate::composition::StateClaimSource::RuntimeObservation
                        | crate::composition::StateClaimSource::EffectReceipt
                        | crate::composition::StateClaimSource::ToolAttestation
                )
        }
    }
}
pub fn assess_predicate(
    condition: &BehavioralCondition,
    requirement: &StateFreshnessRequirement,
    state: &PlanState,
    now: DateTime<Utc>,
    max_age: Option<std::time::Duration>,
) -> AssumptionValidity {
    let Some(key) = predicate_key(condition) else {
        return AssumptionValidity::Unknown;
    };
    let trusted: Vec<_> = state
        .observations
        .iter()
        .filter(|c| {
            c.key == key
                && c.source.trust() >= crate::knowledge_runtime::ContextValueSource::AdapterObserved
        })
        .collect();
    if trusted.is_empty() {
        return AssumptionValidity::Unknown;
    }
    let fresh: Vec<_> = trusted
        .iter()
        .filter(|c| claim_fresh(c, requirement, state, now, max_age))
        .map(|c| (*c).clone())
        .collect();
    if fresh.is_empty() {
        return AssumptionValidity::Stale;
    }
    match condition_value(
        condition,
        &trusted_state(&fresh, &state.external_versions, now),
    ) {
        Some(true) => AssumptionValidity::Supported,
        Some(false) => AssumptionValidity::Contradicted,
        None => AssumptionValidity::Unknown,
    }
}
pub fn plan_invariant_active(invariant: &PlanInvariant, state: &PlanState) -> bool {
    match &invariant.scope {
        PlanInvariantScope::EntirePlan => true,
        PlanInvariantScope::Between { start, end } => {
            state.completed_steps.contains(start) && !state.completed_steps.contains(end)
        }
        PlanInvariantScope::UntilCheckpoint(c) => !state.reached_checkpoints.contains(c),
        PlanInvariantScope::UntilCommitmentPoint(c) => !state.crossed_commitment_points.contains(c),
        PlanInvariantScope::AfterCommitmentPoint(c) => state.crossed_commitment_points.contains(c),
    }
}
pub trait PlanCheckpointEvaluator {
    fn evaluate(
        &self,
        checkpoint: &PlanCheckpoint,
        state: &PlanState,
    ) -> Result<CheckpointEvaluation>;
}
pub struct DeterministicPlanCheckpointEvaluator {
    pub now: DateTime<Utc>,
    pub max_age: Option<std::time::Duration>,
}
impl PlanCheckpointEvaluator for DeterministicPlanCheckpointEvaluator {
    fn evaluate(
        &self,
        checkpoint: &PlanCheckpoint,
        state: &PlanState,
    ) -> Result<CheckpointEvaluation> {
        let mut missing = vec![];
        let mut failed = false;
        for observation in &checkpoint.required_observations {
            match assess_predicate(
                &observation.condition,
                &observation.freshness,
                state,
                self.now,
                self.max_age,
            ) {
                AssumptionValidity::Supported => {}
                AssumptionValidity::Contradicted => {
                    failed = true;
                    missing.push(observation.key.clone());
                }
                _ => missing.push(observation.key.clone()),
            }
        }
        Ok(CheckpointEvaluation {
            checkpoint: checkpoint.id.clone(),
            status: if failed {
                CheckpointStatus::Failed
            } else if !missing.is_empty() {
                CheckpointStatus::VerificationRequired
            } else {
                CheckpointStatus::Satisfied
            },
            missing,
        })
    }
}
pub trait PlanValidityEvaluator {
    fn evaluate(
        &self,
        plan: &ExecutionPlan,
        state: &PlanState,
        context: &RuntimeDecisionContext,
    ) -> Result<PlanValidityAssessment>;
}
#[derive(Default)]
pub struct DeterministicPlanValidityEvaluator {
    pub inputs: PlanEvaluationInputs,
}
impl PlanValidityEvaluator for DeterministicPlanValidityEvaluator {
    fn evaluate(
        &self,
        plan: &ExecutionPlan,
        state: &PlanState,
        context: &RuntimeDecisionContext,
    ) -> Result<PlanValidityAssessment> {
        let index = validate_plan(plan)?;
        let now = self.inputs.now.unwrap_or_else(Utc::now);
        if state.plan != plan.id
            || state.revision != plan.revision
            || state
                .completed_steps
                .iter()
                .any(|id| !plan.steps.iter().any(|s| s.id == *id))
            || state.completed_steps.iter().collect::<BTreeSet<_>>().len()
                != state.completed_steps.len()
        {
            return Err(Error::InvalidInput(
                "Plan state does not match its revision".into(),
            ));
        }
        let next = state.current_step.as_ref().or_else(|| {
            plan.steps
                .iter()
                .find(|s| {
                    !state.completed_steps.contains(&s.id)
                        && s.dependencies
                            .iter()
                            .all(|d| state.completed_steps.contains(d))
                })
                .map(|s| &s.id)
        });
        let mut report = PlanValidityAssessment {
            id: PlanAssessmentId::new(),
            plan: plan.id.clone(),
            revision: plan.revision,
            status: PlanValidityStatus::Valid,
            assumptions: vec![],
            invariants: vec![],
            checkpoints: vec![],
            gates: vec![],
            next_step: next.cloned(),
            blockers: vec![],
            recommendations: vec![],
            reasons: vec![],
            state_hash: plan_state_hash(state)?,
            assessed_at: now,
        };
        let Some(next) = next else {
            return Ok(report);
        };
        let step = plan
            .steps
            .iter()
            .find(|s| &s.id == next)
            .ok_or_else(|| Error::InvalidInput("Next plan step missing".into()))?;
        if state.completed_steps.contains(next)
            || step
                .dependencies
                .iter()
                .any(|d| !state.completed_steps.contains(d))
        {
            report.blockers.push(PlanValidityBlocker::MissingDependency);
        }
        let gates: Vec<_> = plan
            .commitment_gates
            .iter()
            .filter(|g| {
                plan.commitment_points.iter().any(|p| {
                    p.id == g.commitment_point
                        && p.after_step == *next
                        && !state.crossed_commitment_points.contains(&p.id)
                })
            })
            .collect();
        let committing = !gates.is_empty() || self.inputs.nested_commitment_steps.contains(next);
        let mut required: BTreeSet<_> = index.assumptions_by_step[next].iter().cloned().collect();
        for gate in &gates {
            required.extend(gate.required_assumptions.iter().cloned());
        }
        if committing {
            required.extend(
                plan.assumptions
                    .iter()
                    .filter(|a| a.severity >= Severity::High)
                    .map(|a| a.id.clone()),
            );
        }
        let active_checkpoints: Vec<_> = plan
            .checkpoints
            .iter()
            .filter(|c| {
                !state.reached_checkpoints.contains(&c.id)
                    && c.after_step
                        .as_ref()
                        .is_none_or(|s| state.completed_steps.contains(s))
            })
            .collect();
        for checkpoint in &active_checkpoints {
            required.extend(checkpoint.assumptions_to_revalidate.iter().cloned());
        }
        for assumption in plan.assumptions.iter().filter(|a| required.contains(&a.id)) {
            let age = if committing {
                plan.freshness_policy.commitment_point_max_age
            } else if assumption.severity >= Severity::High {
                plan.freshness_policy.critical_max_age
            } else {
                plan.freshness_policy.default_max_age
            };
            let validity = assess_predicate(
                &assumption.predicate,
                &assumption.freshness.requirement,
                state,
                now,
                age,
            );
            let reason = format!("{}: {:?}", assumption.statement, validity);
            if validity != AssumptionValidity::Supported {
                report.reasons.push(reason.clone());
                if assumption.severity >= Severity::High
                    || committing
                    || active_checkpoints
                        .iter()
                        .any(|c| c.assumptions_to_revalidate.contains(&assumption.id))
                {
                    report.blockers.push(match validity {
                        AssumptionValidity::Contradicted => {
                            PlanValidityBlocker::ContradictedAssumption
                        }
                        AssumptionValidity::Stale => PlanValidityBlocker::StaleCriticalAssumption,
                        _ => PlanValidityBlocker::UnknownCriticalAssumption,
                    });
                } else {
                    report.status = PlanValidityStatus::ValidWithWarnings;
                }
                if let Some(key) = predicate_key(&assumption.predicate) {
                    report
                        .recommendations
                        .push(PlanValidityRecommendation::Verify(ObservationSpec {
                            key: key.into(),
                            condition: assumption.predicate.clone(),
                            freshness: assumption.freshness.requirement.clone(),
                        }));
                }
            }
            report.assumptions.push(AssumptionAssessment {
                assumption: assumption.id.clone(),
                validity,
                required_for_next: true,
                affected_steps: index
                    .steps_by_assumption
                    .get(&assumption.id)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|s| !state.completed_steps.contains(s))
                    .collect(),
                reason,
            });
        }
        let mut required_invariants: BTreeSet<_> =
            step.required_invariants.iter().cloned().collect();
        for gate in &gates {
            required_invariants.extend(gate.required_invariants.iter().cloned());
        }
        for checkpoint in &active_checkpoints {
            required_invariants.extend(checkpoint.invariants_to_verify.iter().cloned());
        }
        for invariant in plan
            .invariants
            .iter()
            .filter(|i| required_invariants.contains(&i.id) || plan_invariant_active(i, state))
        {
            let validity = assess_predicate(
                &invariant.condition,
                &StateFreshnessRequirement::None,
                state,
                now,
                if committing {
                    plan.freshness_policy.commitment_point_max_age
                } else {
                    plan.freshness_policy.critical_max_age
                },
            );
            let satisfied = match validity {
                AssumptionValidity::Supported => Some(true),
                AssumptionValidity::Contradicted => Some(false),
                _ => None,
            };
            if satisfied == Some(false) {
                report.blockers.push(PlanValidityBlocker::ViolatedInvariant);
            } else if satisfied.is_none()
                && (invariant.severity >= Severity::High
                    || committing
                    || required_invariants.contains(&invariant.id))
            {
                report
                    .blockers
                    .push(PlanValidityBlocker::UnknownCriticalAssumption);
            }
            report.invariants.push(InvariantEvaluation {
                invariant: invariant.id.clone(),
                satisfied,
                reason: format!("{:?}", validity),
            });
        }
        let evaluator = DeterministicPlanCheckpointEvaluator {
            now,
            max_age: plan.freshness_policy.critical_max_age,
        };
        for checkpoint in active_checkpoints {
            let evaluation = evaluator.evaluate(checkpoint, state)?;
            match evaluation.status {
                CheckpointStatus::Failed => report
                    .blockers
                    .push(PlanValidityBlocker::ContradictedAssumption),
                CheckpointStatus::Satisfied => {}
                _ => report
                    .blockers
                    .push(PlanValidityBlocker::UnknownCriticalAssumption),
            }
            report.checkpoints.push(evaluation);
        }
        for gate in gates {
            let mut reasons = vec![];
            let mut status = CommitmentGateStatus::Open;
            if report
                .assumptions
                .iter()
                .any(|a| a.validity != AssumptionValidity::Supported)
                || report.invariants.iter().any(|i| i.satisfied != Some(true))
            {
                status = CommitmentGateStatus::VerificationRequired;
                reasons
                    .push("Commitment requires fresh supported assumptions and invariants".into());
            }
            if gate
                .required_recoveries
                .iter()
                .any(|r| !self.inputs.available_recoveries.contains(r))
            {
                status = CommitmentGateStatus::Blocked;
                report.blockers.push(PlanValidityBlocker::MissingRecovery);
                reasons.push("Required recovery unavailable".into());
            }
            if gate
                .required_approvals
                .iter()
                .any(|a| !self.inputs.valid_approvals.contains(&a.id))
            {
                status = CommitmentGateStatus::ApprovalRequired;
                report.blockers.push(PlanValidityBlocker::ApprovalRequired);
                reasons.push("Fresh, effect-bound external approval required".into());
            }
            if self.inputs.low_diversity.contains(&gate.commitment_point) {
                status = CommitmentGateStatus::VerificationRequired;
                report
                    .blockers
                    .push(PlanValidityBlocker::InsufficientDiversity);
                reasons.push("Checkpoint evidence shares insufficiently diverse origins".into());
            }
            if status != CommitmentGateStatus::Open && report.blockers.is_empty() {
                report
                    .blockers
                    .push(PlanValidityBlocker::CommitmentPointConflict);
            }
            report.gates.push(CommitmentGateEvaluation {
                point: gate.commitment_point.clone(),
                status,
                reasons,
            });
        }
        for point in plan
            .commitment_points
            .iter()
            .filter(|p| state.crossed_commitment_points.contains(&p.id))
        {
            if point
                .new_recovery_requirements
                .iter()
                .any(|r| !self.inputs.available_recoveries.contains(r))
            {
                report.blockers.push(PlanValidityBlocker::MissingRecovery);
                report
                    .reasons
                    .push("Post-commitment recovery requirement is unavailable".into());
            }
        }
        if step
            .required_capabilities
            .iter()
            .any(|c| !context.capability_context.available.contains(c))
        {
            report.blockers.push(PlanValidityBlocker::MissingCapability);
        }
        if self.inputs.changed_knowledge.contains(next) {
            report
                .blockers
                .push(PlanValidityBlocker::InvalidatedKnowledge);
        }
        if self.inputs.invalid_components.contains(next) {
            report
                .blockers
                .push(PlanValidityBlocker::InvalidatedKnowledge);
        }
        if let Some(reasons) = self.inputs.nested_blockers.get(next) {
            report.blockers.push(PlanValidityBlocker::KnowledgeConflict);
            report.reasons.extend(reasons.clone());
        }
        if let PlanStepKind::Approval(requirement) = &step.kind
            && !self.inputs.valid_approvals.contains(&requirement.id)
        {
            report.blockers.push(PlanValidityBlocker::ApprovalRequired);
        }
        if matches!(step.kind, PlanStepKind::Custom(_)) {
            report.blockers.push(PlanValidityBlocker::UnsupportedStep);
        }
        if context
            .operational_knowledge
            .as_ref()
            .is_some_and(|k| !k.unresolved_conflicts.is_empty())
        {
            report.blockers.push(PlanValidityBlocker::KnowledgeConflict);
        }
        use PlanValidityBlocker::*;
        if report
            .blockers
            .iter()
            .any(|b| matches!(b, MissingDependency | UnsupportedStep | MissingCapability))
        {
            report.status = PlanValidityStatus::Invalid;
            report
                .recommendations
                .push(PlanValidityRecommendation::Abstain);
        } else if report.blockers.contains(&ViolatedInvariant)
            && !state.crossed_commitment_points.is_empty()
        {
            report.status = PlanValidityStatus::RecoveryRequired;
            report
                .recommendations
                .push(PlanValidityRecommendation::Abstain);
        } else if report.blockers.iter().any(|b| {
            matches!(
                b,
                ContradictedAssumption
                    | ViolatedInvariant
                    | InvalidatedKnowledge
                    | ExternalStateDrift
                    | KnowledgeConflict
            )
        }) {
            report.status = PlanValidityStatus::ReplanRequired;
            report
                .recommendations
                .push(PlanValidityRecommendation::Replan);
        } else if !report.blockers.is_empty() {
            report.status = PlanValidityStatus::VerificationRequired;
            if report.blockers.contains(&ApprovalRequired) {
                report
                    .recommendations
                    .push(PlanValidityRecommendation::RequireApproval);
            }
        } else {
            report
                .recommendations
                .push(PlanValidityRecommendation::Continue);
        }
        Ok(report)
    }
}
/// Derived display caches do not change the identity of observed execution state.
pub fn plan_state_hash(state: &PlanState) -> Result<String> {
    let mut state = state.clone();
    state.assumption_states.clear();
    state.invariant_states.clear();
    composition_hash(&state)
}
pub fn revalidation_set(
    plan: &ExecutionPlan,
    state: &PlanState,
    changed: &[PlanAssumptionId],
) -> Result<PlanRevalidationSet> {
    let index = validate_plan(plan)?;
    let mut steps = BTreeSet::new();
    for id in changed {
        steps.extend(
            index
                .steps_by_assumption
                .get(id)
                .into_iter()
                .flatten()
                .filter(|s| !state.completed_steps.contains(s))
                .cloned(),
        );
    }
    loop {
        let old = steps.len();
        for step in &plan.steps {
            if !state.completed_steps.contains(&step.id)
                && step.dependencies.iter().any(|d| steps.contains(d))
            {
                steps.insert(step.id.clone());
            }
        }
        if old == steps.len() {
            break;
        }
    }
    let invariants = steps
        .iter()
        .flat_map(|s| {
            index
                .invariants_by_step
                .get(s)
                .into_iter()
                .flatten()
                .cloned()
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(PlanRevalidationSet {
        assumptions: changed.to_vec(),
        steps: steps.into_iter().collect(),
        invariants,
    })
}
pub fn detect_assumption_drift(
    plan: &ExecutionPlan,
    previous: &PlanState,
    current: &PlanState,
    now: DateTime<Utc>,
) -> Vec<AssumptionDrift> {
    plan.assumptions
        .iter()
        .filter_map(|a| {
            let key = predicate_key(&a.predicate)?;
            let old = previous
                .observations
                .iter()
                .filter(|c| {
                    c.key == key
                        && c.source.trust()
                            >= crate::knowledge_runtime::ContextValueSource::AdapterObserved
                })
                .max_by_key(|c| (c.source.trust(), c.freshness.observed_at))?
                .clone();
            let new = current
                .observations
                .iter()
                .filter(|c| {
                    c.key == key
                        && c.source.trust()
                            >= crate::knowledge_runtime::ContextValueSource::AdapterObserved
                })
                .max_by_key(|c| (c.source.trust(), c.freshness.observed_at))
                .cloned();
            let drift = if let Some(n) = &new {
                if n.value != old.value {
                    AssumptionDriftKind::ValueChanged
                } else if n.source != old.source {
                    AssumptionDriftKind::SourceChanged
                } else if !claim_fresh(
                    n,
                    &a.freshness.requirement,
                    current,
                    now,
                    plan.freshness_policy.critical_max_age,
                ) {
                    AssumptionDriftKind::EvidenceStale
                } else {
                    return None;
                }
            } else {
                AssumptionDriftKind::Unknown
            };
            Some(AssumptionDrift {
                assumption: a.id.clone(),
                previous: old,
                current: new,
                drift,
                detected_at: now,
            })
        })
        .collect()
}
