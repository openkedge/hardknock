// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{
    Error, Result,
    assurance::{BehavioralCondition, PredicateOperator},
    capability::NetworkMode,
    core::*,
    curriculum::Severity,
    hierarchy::*,
};
use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, BTreeSet};

pub fn composition_hash(c: &impl serde::Serialize) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(c)?).to_hex().to_string())
}
pub fn ordered_steps(c: &Composition) -> Result<Vec<CompositionStepId>> {
    if c.steps.is_empty()
        || c.steps.len() > 50
        || c.relations.len() > 2500
        || c.revision == 0
        || c.name.trim().is_empty()
    {
        return Err(Error::InvalidInput(
            "Composition requires 1..50 steps, <=2500 relations, a name and a positive revision"
                .into(),
        ));
    }
    let ids: BTreeSet<_> = c.steps.iter().map(|s| s.id.clone()).collect();
    if ids.len() != c.steps.len() {
        return Err(Error::InvalidInput(
            "Repeated composition step identity".into(),
        ));
    }
    let mut parents: BTreeMap<_, BTreeSet<_>> =
        ids.iter().map(|id| (id.clone(), BTreeSet::new())).collect();
    for r in &c.relations {
        if !ids.contains(&r.from) || !ids.contains(&r.to) || r.from == r.to {
            return Err(Error::InvalidInput(
                "Composition relation has a dangling or self reference".into(),
            ));
        }
        if r.kind.orders() {
            parents
                .get_mut(&r.to)
                .expect("checked")
                .insert(r.from.clone());
        }
    }
    let mut result = vec![];
    let mut done = BTreeSet::new();
    while done.len() < ids.len() {
        let next = c
            .steps
            .iter()
            .find(|s| !done.contains(&s.id) && parents[&s.id].iter().all(|p| done.contains(p)))
            .ok_or_else(|| Error::InvalidInput("Composition ordering cycle".into()))?;
        done.insert(next.id.clone());
        result.push(next.id.clone());
    }
    for s in &c.steps {
        s.capabilities.validate()?;
        if s.component.revision.is_empty() || s.component.content_hash.is_empty() {
            return Err(Error::InvalidInput(
                "Each component requires a pinned revision and hash".into(),
            ));
        }
        for b in &s.input_bindings {
            if let Some(from) = &b.from
                && (!ids.contains(from) || !precedes(c, from, &s.id))
            {
                return Err(Error::InvalidInput(
                    "Handoff producer must precede its consumer explicitly".into(),
                ));
            }
        }
    }
    for p in &c.cross_step_preconditions {
        if !ids.contains(&p.required_by)
            || p.established_by
                .as_ref()
                .is_some_and(|id| !ids.contains(id) || !precedes(c, id, &p.required_by))
        {
            return Err(Error::InvalidInput(
                "Cross-step precondition has invalid endpoints".into(),
            ));
        }
    }
    for effect in &c.contract.effect_policy.effects {
        if !ids.contains(&effect.step) {
            return Err(Error::InvalidInput("Effect step is missing".into()));
        }
    }
    for compensation in &c.contract.effect_policy.compensation_edges {
        if !ids.contains(&compensation.committed)
            || !ids.contains(&compensation.compensating)
            || !compensation.preserves_original_receipt
        {
            return Err(Error::InvalidInput(
                "Compensation must reference real steps and preserve the original receipt".into(),
            ));
        }
    }
    let mut points = BTreeSet::new();
    for p in &c.contract.effect_policy.commit_points {
        if !ids.contains(&p.after_step) || !points.insert(p.id.clone()) {
            return Err(Error::InvalidInput("Invalid commitment point".into()));
        }
    }
    for i in &c.contract.sequence_invariants {
        let valid = match &i.scope {
            SequenceScope::EntireComposition => true,
            SequenceScope::Between { from, to } => {
                ids.contains(from) && ids.contains(to) && precedes(c, from, to)
            }
            SequenceScope::Before { step } | SequenceScope::After { step } => ids.contains(step),
            SequenceScope::UntilCommitPoint { commit_point } => points.contains(commit_point),
        };
        if !valid {
            return Err(Error::InvalidInput(
                "Invalid sequence invariant scope".into(),
            ));
        }
    }
    for p in &c.recoveries {
        if !ids.contains(&p.failure_point) {
            return Err(Error::InvalidInput(
                "Recovery failure point is missing".into(),
            ));
        }
    }
    Ok(result)
}
pub fn precedes(c: &Composition, from: &CompositionStepId, to: &CompositionStepId) -> bool {
    let mut visited = BTreeSet::new();
    let mut pending = vec![from.clone()];
    while let Some(id) = pending.pop() {
        if !visited.insert(id.clone()) {
            continue;
        }
        for r in c
            .relations
            .iter()
            .filter(|r| r.from == id && r.kind.orders())
        {
            if &r.to == to {
                return true;
            }
            pending.push(r.to.clone());
        }
    }
    false
}
pub fn condition_value(condition: &BehavioralCondition, state: &KnowledgeContext) -> Option<bool> {
    let BehavioralCondition::StatePredicate {
        path,
        operator,
        value,
    } = condition
    else {
        return None;
    };
    let current = state.values.get(path);
    use PredicateOperator::*;
    if matches!(operator, Exists) {
        return current.map(|_| true);
    }
    if matches!(operator, NotExists) {
        return current.map(|_| false);
    }
    let current = current?;
    let actual = match current {
        ScopeValue::String(v) | ScopeValue::Version(v) => serde_json::json!(v),
        ScopeValue::Integer(v) => serde_json::json!(v),
        ScopeValue::Boolean(v) => serde_json::json!(v),
    };
    match operator {
        Equals => Some(actual == *value),
        NotEquals => Some(actual != *value),
        GreaterThan => Some(actual.as_i64()? > value.as_i64()?),
        GreaterThanOrEqual => Some(actual.as_i64()? >= value.as_i64()?),
        LessThan => Some(actual.as_i64()? < value.as_i64()?),
        LessThanOrEqual => Some(actual.as_i64()? <= value.as_i64()?),
        Contains => Some(actual.as_str()?.contains(value.as_str()?)),
        _ => None,
    }
}
/// Equal highest-trust disagreements and expired/version-mismatched claims stay unknown.
pub fn trusted_state(
    claims: &[StateClaim],
    versions: &BTreeMap<String, String>,
    now: DateTime<Utc>,
) -> KnowledgeContext {
    let mut grouped: BTreeMap<&str, Vec<&StateClaim>> = BTreeMap::new();
    for c in claims {
        if c.freshness.observed_at > now
            || c.freshness.expires_at.is_some_and(|t| t <= now)
            || c.freshness
                .external_version
                .as_ref()
                .is_some_and(|v| versions.get(&v.resource) != Some(&v.version))
        {
            continue;
        }
        if c.source.trust() < crate::knowledge_runtime::ContextValueSource::AdapterObserved {
            continue;
        }
        grouped.entry(&c.key).or_default().push(c);
    }
    let mut result = KnowledgeContext::default();
    for (key, mut values) in grouped {
        values.sort_by_key(|v| std::cmp::Reverse(v.source.trust()));
        let first = values[0];
        if values
            .iter()
            .take_while(|v| v.source.trust() == first.source.trust())
            .all(|v| v.value == first.value)
        {
            result.values.insert(key.into(), first.value.clone());
        }
    }
    result
}
pub fn handoff_fresh(
    h: &StateHandoff,
    requirement: &StateFreshnessRequirement,
    current: &CompositionStepId,
    versions: &BTreeMap<String, String>,
    now: DateTime<Utc>,
) -> bool {
    if &h.to != current
        || h.external_state_versions
            .iter()
            .any(|v| versions.get(&v.resource) != Some(&v.version))
    {
        return false;
    }
    !h.facts.is_empty()
        && h.facts.iter().all(|f| {
            if f.source.trust() < crate::knowledge_runtime::ContextValueSource::AdapterObserved
                || f.freshness.observed_at > now
                || f.freshness.expires_at.is_some_and(|t| t <= now)
                || f.freshness
                    .external_version
                    .as_ref()
                    .is_some_and(|v| versions.get(&v.resource) != Some(&v.version))
            {
                return false;
            }
            match requirement {
                StateFreshnessRequirement::None => true,
                StateFreshnessRequirement::SameStep => f.freshness.step.as_ref() == Some(current),
                StateFreshnessRequirement::BeforeNextMutation => {
                    f.freshness.step.as_ref() == Some(&h.from)
                        && f.freshness.external_version.is_some()
                }
                StateFreshnessRequirement::MaxAge(max) => now
                    .signed_duration_since(f.freshness.observed_at)
                    .to_std()
                    .is_ok_and(|age| age <= *max),
                StateFreshnessRequirement::AuthoritativeRefreshRequired => {
                    matches!(
                        f.source,
                        StateClaimSource::RuntimeObservation
                            | StateClaimSource::EffectReceipt
                            | StateClaimSource::ToolAttestation
                    ) && f.freshness.step.as_ref() == Some(current)
                }
            }
        })
}
pub fn invariant_active(
    c: &Composition,
    i: &SequenceInvariant,
    step: &CompositionStepId,
    phase: SequenceEvaluationPhase,
    completed: &[CompositionStepId],
    commit: &CompositionCommitState,
) -> bool {
    match &i.scope {
        SequenceScope::EntireComposition => true,
        SequenceScope::Before { step: s } => {
            s == step && phase == SequenceEvaluationPhase::BeforeStep
        }
        SequenceScope::After { step: s } => {
            s == step && phase == SequenceEvaluationPhase::AfterStep
        }
        SequenceScope::Between { from, to } => {
            ((completed.contains(from)
                || from == step && phase == SequenceEvaluationPhase::AfterStep)
                && !completed.contains(to))
                && (step == to || precedes(c, from, step) || step == from)
        }
        SequenceScope::UntilCommitPoint { commit_point } => !commit.reached.contains(commit_point),
    }
}
pub fn capability_plan(c: &Composition) -> CompositionCapabilityPlan {
    let mut findings = vec![];
    for s in &c.steps {
        for b in &s.input_bindings {
            if let Some(from) = &b.from {
                let sensitive = matches!(
                    b.resource,
                    CapabilityFlowResource::Secret
                        | CapabilityFlowResource::Credential
                        | CapabilityFlowResource::EffectAuthority
                );
                let classification = if !b.allowed
                    || sensitive
                        && (s.capabilities.network.mode != NetworkMode::None
                            || !c.contract.capability_policy.allow_sensitive_handoffs)
                    || b.resource == CapabilityFlowResource::EffectAuthority
                {
                    CapabilityFlowClassification::Forbidden
                } else if sensitive {
                    CapabilityFlowClassification::Sensitive
                } else if matches!(b.resource, CapabilityFlowResource::Custom(_)) {
                    CapabilityFlowClassification::Unknown
                } else {
                    CapabilityFlowClassification::Allowed
                };
                findings.push(CapabilityFlow {
                    source_step: from.clone(),
                    target_step: s.id.clone(),
                    resource: b.resource.clone(),
                    classification,
                });
            }
        }
    }
    CompositionCapabilityPlan {
        steps: c
            .steps
            .iter()
            .map(|s| (s.id.clone(), s.capabilities.clone()))
            .collect(),
        persistent_capabilities: c.contract.capability_policy.persistent_capabilities.clone(),
        findings,
    }
}
pub trait CompositionPreflightAnalyzer {
    fn analyze(
        &self,
        composition: &Composition,
        context: &KnowledgeContext,
    ) -> Result<CompositionPreflightReport>;
}
pub struct DefaultCompositionPreflightAnalyzer;
impl CompositionPreflightAnalyzer for DefaultCompositionPreflightAnalyzer {
    fn analyze(
        &self,
        c: &Composition,
        context: &KnowledgeContext,
    ) -> Result<CompositionPreflightReport> {
        ordered_steps(c)?;
        let mut findings = vec![];
        let mut add = |kind, steps, reason: String, severity| {
            findings.push(CompositionCompatibilityFinding {
                kind,
                steps,
                reason,
                severity,
            })
        };
        use CompositionCompatibilityFindingKind::*;
        let p = &c.contract.capability_policy.persistent_capabilities;
        p.validate()?;
        if !p.credentials.is_empty()
            || p.network.mode != NetworkMode::None
            || p.effects.commit
            || p.effects.prepare
            || p.effects.propose
            || !p.filesystem.writable.is_empty()
            || p.process.allow_exec
            || !p.environment.values.is_empty()
        {
            add(CapabilityConflict,vec![],"Persistent coordinator authority must be minimal; never union component capabilities".into(),Severity::Critical)
        }
        for condition in &c.contract.preconditions {
            match condition_value(condition, context) {
                Some(true) => {}
                Some(false) => add(
                    UnsatisfiedPrecondition,
                    vec![],
                    "Composition precondition is false".into(),
                    Severity::High,
                ),
                None => add(
                    UnknownCompatibility,
                    vec![],
                    "Composition precondition requires observation".into(),
                    Severity::Medium,
                ),
            }
        }
        for step in &c.steps {
            let mut requirements: Vec<_> = c
                .cross_step_preconditions
                .iter()
                .filter(|p| p.required_by == step.id)
                .map(|p| &p.condition)
                .collect();
            requirements.extend(
                c.assumptions
                    .iter()
                    .filter(|a| {
                        a.owner == step.component.component
                            && DeterministicApplicabilityEvaluator
                                .evaluate(&a.scope, context)
                                .status
                                != ApplicabilityStatus::Inapplicable
                    })
                    .map(|a| &a.condition),
            );
            for condition in requirements {
                for before in c.steps.iter().filter(|s| precedes(c, &s.id, &step.id)) {
                    for output in &before.expected_outputs {
                        let mut predicted = context.clone();
                        predicted
                            .values
                            .insert(output.key.clone(), output.value.clone());
                        if matches!(condition,BehavioralCondition::StatePredicate{path,..} if path==&output.key)
                            && condition_value(condition, &predicted) == Some(false)
                        {
                            add(
                                InvalidatedAssumption,
                                vec![before.id.clone(), step.id.clone()],
                                format!(
                                    "Predicted {} from {} invalidates a requirement of {}; empirical test required",
                                    output.key, before.id, step.id
                                ),
                                Severity::High,
                            )
                        }
                    }
                }
            }
        }
        for (index, a) in c.steps.iter().enumerate() {
            for b in c.steps.iter().skip(index + 1) {
                if !precedes(c, &a.id, &b.id)
                    && !precedes(c, &b.id, &a.id)
                    && a.expected_outputs
                        .iter()
                        .any(|x| b.expected_outputs.iter().any(|y| x.key == y.key))
                {
                    add(
                        OrderingRequirement,
                        vec![a.id.clone(), b.id.clone()],
                        "Unordered writes share state; choose and test an explicit order".into(),
                        Severity::High,
                    )
                }
            }
        }
        let capability_plan = capability_plan(c);
        for flow in &capability_plan.findings {
            if flow.classification != CapabilityFlowClassification::Allowed {
                add(
                    CapabilityConflict,
                    vec![flow.source_step.clone(), flow.target_step.clone()],
                    format!(
                        "{:?} capability flow: {:?}",
                        flow.classification, flow.resource
                    ),
                    if flow.classification == CapabilityFlowClassification::Forbidden {
                        Severity::Critical
                    } else {
                        Severity::High
                    },
                )
            }
        }
        if matches!(
            c.contract.effect_policy.atomicity,
            CompositionAtomicity::FullyAtomic | CompositionAtomicity::AdapterLocalGroups
        ) {
            add(
                EffectConflict,
                vec![],
                "Atomic group guarantees are not verified by this composition analyzer".into(),
                Severity::Critical,
            )
        }
        for recovery in &c.recoveries {
            for a in &recovery.recoveries {
                for b in &recovery.recoveries {
                    for required in &b.requires {
                        for established in &a.establishes {
                            if conditions_conflict(required, established) {
                                add(
                                    RecoveryConflict,
                                    vec![recovery.failure_point.clone()],
                                    "One recovery invalidates another recovery's requirement"
                                        .into(),
                                    Severity::High,
                                )
                            }
                        }
                    }
                }
            }
        }
        let conditions = c
            .contract
            .preconditions
            .iter()
            .chain(
                c.contract
                    .sequence_invariants
                    .iter()
                    .filter(|i| matches!(i.scope, SequenceScope::EntireComposition))
                    .map(|i| &i.condition),
            )
            .collect::<Vec<_>>();
        for (index, a) in conditions.iter().enumerate() {
            for b in conditions.iter().skip(index + 1) {
                if conditions_conflict(a, b) {
                    add(
                        ConstraintConflict,
                        vec![],
                        "Joint state constraints cannot both hold".into(),
                        Severity::High,
                    )
                }
            }
        }
        let status = if findings.iter().any(|f| {
            matches!(
                f.kind,
                CapabilityConflict
                    | EffectConflict
                    | RecoveryConflict
                    | ConstraintConflict
                    | UnsatisfiedPrecondition
                    | OrderingRequirement
            )
        }) {
            CompositionCompatibilityStatus::Conflict
        } else if findings.iter().any(|f| f.kind == InvalidatedAssumption) {
            CompositionCompatibilityStatus::CompatibleWithConditions
        } else if !findings.is_empty() {
            CompositionCompatibilityStatus::Unknown
        } else {
            CompositionCompatibilityStatus::Compatible
        };
        let pairs = DefaultInteractionCandidateGenerator
            .generate(c)?
            .into_iter()
            .filter_map(|p| {
                if p.steps.len() == 2 {
                    Some((p.steps[0].clone(), p.steps[1].clone()))
                } else {
                    None
                }
            })
            .collect();
        Ok(CompositionPreflightReport {
            composition: c.id.clone(),
            revision: c.revision,
            status,
            findings,
            untested_pairs: pairs,
            capability_plan,
            empirical_validation_required: true,
        })
    }
}
pub fn conditions_conflict(a: &BehavioralCondition, b: &BehavioralCondition) -> bool {
    if let (
        BehavioralCondition::StatePredicate {
            path: p,
            operator: PredicateOperator::Equals,
            value: a,
        },
        BehavioralCondition::StatePredicate {
            path: q,
            operator: PredicateOperator::Equals,
            value: b,
        },
    ) = (a, b)
    {
        p == q && a != b
    } else {
        false
    }
}
pub trait InteractionCandidateGenerator {
    fn generate(&self, c: &Composition) -> Result<Vec<InteractionCandidate>>;
}
pub struct DefaultInteractionCandidateGenerator;
impl InteractionCandidateGenerator for DefaultInteractionCandidateGenerator {
    fn generate(&self, c: &Composition) -> Result<Vec<InteractionCandidate>> {
        let mut pairs = BTreeMap::new();
        for r in &c.relations {
            pairs.insert(
                (r.from.clone(), r.to.clone()),
                InteractionCandidate {
                    steps: vec![r.from.clone(), r.to.clone()],
                    reason: format!("Explicit {:?} relation", r.kind),
                    kind: if r.kind == CompositionRelationKind::InvalidatesAssumptionOf {
                        InteractionFailureKind::AssumptionInvalidation
                    } else {
                        InteractionFailureKind::Unknown
                    },
                },
            );
        }
        for s in &c.steps {
            for b in &s.input_bindings {
                if let Some(from) = &b.from {
                    pairs
                        .entry((from.clone(), s.id.clone()))
                        .or_insert(InteractionCandidate {
                            steps: vec![from.clone(), s.id.clone()],
                            reason: format!("Shared state {}", b.key),
                            kind: InteractionFailureKind::StateInvalidation,
                        });
                }
            }
        }
        Ok(pairs.into_values().collect())
    }
}

/// A successful local procedure does not prove that the composition was restored.
pub fn composition_recovery_outcome(
    local_success: bool,
    global_conditions: &[Option<bool>],
    compensated: bool,
) -> CompositionRecoveryOutcome {
    if !local_success {
        return CompositionRecoveryOutcome::Failed;
    }
    if global_conditions.is_empty() || global_conditions.contains(&None) {
        return CompositionRecoveryOutcome::Inconclusive;
    }
    if global_conditions.contains(&Some(false)) {
        return CompositionRecoveryOutcome::LocallyRecovered;
    }
    if compensated {
        CompositionRecoveryOutcome::Compensated
    } else {
        CompositionRecoveryOutcome::FullyRecovered
    }
}
