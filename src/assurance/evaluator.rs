// SPDX-License-Identifier: Apache-2.0

use std::{cmp::Ordering, collections::BTreeSet, str::FromStr};

use chrono::{DateTime, Duration, Utc};
use serde_json::Value;

use super::*;
use crate::{
    Error, Result,
    core::{AssuranceProfileId, CurriculumGoalId},
    curriculum::{
        CurriculumDecision, CurriculumGoal, CurriculumGoalKind, EvidenceGap, GoalStatus, Priority,
        PriorityScore, Severity, TrialSafety,
    },
    tool::AttestationAssurance,
};

pub trait BehavioralContractEvaluator {
    fn evaluate(
        &self,
        contract: &BehavioralContract,
        evidence: &ExecutionEvidence,
    ) -> Result<ContractEvaluation>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicContractEvaluator;

impl BehavioralContractEvaluator for DeterministicContractEvaluator {
    fn evaluate(
        &self,
        contract: &BehavioralContract,
        evidence: &ExecutionEvidence,
    ) -> Result<ContractEvaluation> {
        contract.validate()?;
        let preconditions = contract
            .preconditions
            .iter()
            .map(|condition| condition_result(condition, evidence))
            .collect::<Result<Vec<_>>>()?;
        if preconditions
            .iter()
            .any(|result| result.status == ContractEvaluationStatus::Violated)
        {
            return Ok(ContractEvaluation {
                contract_id: contract.id.clone(),
                preconditions,
                postconditions: vec![],
                invariants: vec![],
                forbidden_outcomes: vec![],
                overall: ContractEvaluationStatus::NotApplicable,
            });
        }
        let postconditions = contract
            .postconditions
            .iter()
            .map(|condition| condition_result(condition, evidence))
            .collect::<Result<Vec<_>>>()?;
        let invariants = contract
            .invariants
            .iter()
            .map(|invariant| {
                let result = condition_result(&invariant.condition, evidence)?;
                Ok(InvariantResult {
                    invariant_id: invariant.id.clone(),
                    severity: invariant.severity,
                    status: result.status,
                    reason: result.reason,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let forbidden_outcomes = contract
            .forbidden_outcomes
            .iter()
            .map(|outcome| {
                let detected = condition_result(&outcome.detector, evidence)?;
                let (status, reason) = match detected.status {
                    ContractEvaluationStatus::Satisfied => (
                        ContractEvaluationStatus::Violated,
                        format!("forbidden outcome detected: {}", outcome.description),
                    ),
                    ContractEvaluationStatus::Violated => (
                        ContractEvaluationStatus::Satisfied,
                        format!("forbidden outcome not observed: {}", outcome.description),
                    ),
                    status => (status, detected.reason),
                };
                Ok(ForbiddenOutcomeResult {
                    outcome_id: outcome.id.clone(),
                    severity: outcome.severity,
                    status,
                    reason,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let statuses = preconditions
            .iter()
            .map(|result| result.status)
            .chain(postconditions.iter().map(|result| result.status))
            .chain(invariants.iter().map(|result| result.status))
            .chain(forbidden_outcomes.iter().map(|result| result.status));
        let mut overall = ContractEvaluationStatus::Satisfied;
        for status in statuses {
            overall = match status {
                ContractEvaluationStatus::Violated => ContractEvaluationStatus::Violated,
                ContractEvaluationStatus::Inconclusive
                    if overall != ContractEvaluationStatus::Violated =>
                {
                    ContractEvaluationStatus::Inconclusive
                }
                _ => overall,
            };
        }
        Ok(ContractEvaluation {
            contract_id: contract.id.clone(),
            preconditions,
            postconditions,
            invariants,
            forbidden_outcomes,
            overall,
        })
    }
}

fn condition_result(
    condition: &BehavioralCondition,
    evidence: &ExecutionEvidence,
) -> Result<ConditionResult> {
    let fingerprint = condition.fingerprint()?;
    let (status, reason) = match condition {
        BehavioralCondition::EvaluatorCheck {
            evaluator,
            expression,
        } => evidence
            .evaluator_results
            .get(&ExecutionEvidence::evaluator_key(evaluator, expression))
            .copied()
            .map(|status| (status, format!("deterministic evaluator {evaluator}")))
            .unwrap_or((
                ContractEvaluationStatus::Inconclusive,
                format!("no result from evaluator {evaluator}"),
            )),
        BehavioralCondition::StatePredicate {
            path,
            operator,
            value,
        } => state_predicate(evidence, path, operator, value),
        BehavioralCondition::EffectPredicate { predicate } => {
            effect_predicate(&evidence.effects, predicate)
        }
        BehavioralCondition::CapabilityPredicate { predicate } => {
            capability_predicate(&evidence.capabilities, predicate)
        }
        BehavioralCondition::Custom { .. } => evidence
            .custom_results
            .get(&fingerprint)
            .copied()
            .map(|status| (status, "registered deterministic custom evaluator".into()))
            .unwrap_or((
                ContractEvaluationStatus::Inconclusive,
                "no registered deterministic custom evaluator".into(),
            )),
    };
    Ok(ConditionResult {
        condition: condition.id()?,
        status,
        reason,
    })
}

fn state_predicate(
    evidence: &ExecutionEvidence,
    path: &str,
    operator: &PredicateOperator,
    expected: &Value,
) -> (ContractEvaluationStatus, String) {
    let actual = evidence.states.get(path);
    if matches!(
        operator,
        PredicateOperator::Exists | PredicateOperator::NotExists
    ) {
        if !evidence.complete_state_snapshot && actual.is_none() {
            return (
                ContractEvaluationStatus::Inconclusive,
                format!("state path {path} absence is not observable"),
            );
        }
        let satisfied = matches!(operator, PredicateOperator::Exists) == actual.is_some();
        return predicate_status(satisfied, format!("state existence check for {path}"));
    }
    let Some(actual) = actual else {
        return (
            ContractEvaluationStatus::Inconclusive,
            format!("state path {path} was not observed"),
        );
    };
    let satisfied = match operator {
        PredicateOperator::Equals => actual == expected,
        PredicateOperator::NotEquals => actual != expected,
        PredicateOperator::GreaterThan => compare_json(actual, expected) == Some(Ordering::Greater),
        PredicateOperator::GreaterThanOrEqual => {
            compare_json(actual, expected).is_some_and(|order| order != Ordering::Less)
        }
        PredicateOperator::LessThan => compare_json(actual, expected) == Some(Ordering::Less),
        PredicateOperator::LessThanOrEqual => {
            compare_json(actual, expected).is_some_and(|order| order != Ordering::Greater)
        }
        PredicateOperator::Contains => match actual {
            Value::Array(values) => values.contains(expected),
            Value::String(value) => expected.as_str().is_some_and(|part| value.contains(part)),
            Value::Object(values) => expected
                .as_str()
                .is_some_and(|key| values.contains_key(key)),
            _ => false,
        },
        PredicateOperator::Exists | PredicateOperator::NotExists => unreachable!(),
    };
    predicate_status(satisfied, format!("state predicate for {path}"))
}

fn compare_json(left: &Value, right: &Value) -> Option<Ordering> {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => left.as_f64()?.partial_cmp(&right.as_f64()?),
        (Value::String(left), Value::String(right)) => Some(left.cmp(right)),
        _ => None,
    }
}

fn predicate_status(satisfied: bool, reason: String) -> (ContractEvaluationStatus, String) {
    (
        if satisfied {
            ContractEvaluationStatus::Satisfied
        } else {
            ContractEvaluationStatus::Violated
        },
        reason,
    )
}

fn effect_predicate(
    evidence: &EffectEvidence,
    predicate: &EffectPredicate,
) -> (ContractEvaluationStatus, String) {
    if !evidence.observable {
        return (
            ContractEvaluationStatus::Inconclusive,
            "external effects were not observable".into(),
        );
    }
    let satisfied = match predicate {
        EffectPredicate::CommittedEffect => evidence.committed_effect,
        EffectPredicate::NoCommittedEffect => !evidence.committed_effect,
        EffectPredicate::ExperimentalEffectLeak => evidence.experimental_effect_leak,
        EffectPredicate::NoExperimentalEffectLeak => !evidence.experimental_effect_leak,
        EffectPredicate::CommitAuthorityExternalized => evidence.commit_authority_externalized,
        EffectPredicate::CommitAuthorityNotExternalized => !evidence.commit_authority_externalized,
        EffectPredicate::EffectKind { effect_kind } => evidence.effect_kinds.contains(effect_kind),
        EffectPredicate::NoEffectKind { effect_kind } => {
            !evidence.effect_kinds.contains(effect_kind)
        }
    };
    predicate_status(satisfied, format!("effect predicate {predicate:?}"))
}

fn capability_predicate(
    evidence: &CapabilityEvidence,
    predicate: &CapabilityPredicate,
) -> (ContractEvaluationStatus, String) {
    if !evidence.observable {
        return (
            ContractEvaluationStatus::Inconclusive,
            "execution capabilities were not observable".into(),
        );
    }
    let satisfied = match predicate {
        CapabilityPredicate::NoAmbientCredentials => !evidence.ambient_credentials,
        CapabilityPredicate::NoEffectCommit => !evidence.effect_commit_granted,
        CapabilityPredicate::MaximumNotExceeded => !evidence.maximum_exceeded,
        CapabilityPredicate::DeclaredToolManifestsOnly => evidence.declared_tool_manifests_only,
        CapabilityPredicate::MinimizationValidated => evidence.minimization_validated,
    };
    predicate_status(satisfied, format!("capability predicate {predicate:?}"))
}

pub fn contract_observability(contract: &BehavioralContract) -> Result<ContractObservability> {
    let requirements = &contract.evaluation_requirements;
    let mut observable = vec![];
    let mut unobservable = vec![];
    for condition in contract
        .preconditions
        .iter()
        .chain(&contract.postconditions)
        .chain(contract.invariants.iter().map(|value| &value.condition))
        .chain(
            contract
                .forbidden_outcomes
                .iter()
                .map(|value| &value.detector),
        )
    {
        let supported = match condition {
            BehavioralCondition::EvaluatorCheck { evaluator, .. } => {
                requirements.evaluators.contains(evaluator)
            }
            BehavioralCondition::StatePredicate { path, .. } => {
                requirements.observable_state_paths.contains(path)
            }
            BehavioralCondition::EffectPredicate { .. } => requirements.effects_observable,
            BehavioralCondition::CapabilityPredicate { .. } => requirements.capabilities_observable,
            BehavioralCondition::Custom { kind, .. } => {
                requirements.custom_condition_kinds.contains(kind)
            }
        };
        let target = if supported {
            &mut observable
        } else {
            &mut unobservable
        };
        target.push(condition.id()?);
    }
    observable.sort();
    observable.dedup();
    unobservable.sort();
    unobservable.dedup();
    Ok(ContractObservability {
        observable,
        unobservable,
    })
}

pub trait CertificationEvaluator {
    fn evaluate(
        &self,
        skill: &SkillRevisionRef,
        contract: &BehavioralContract,
        profile: &AssuranceProfile,
        evidence: &EvidenceManifest,
    ) -> Result<CertificationEvaluation>;
}

#[derive(Clone, Copy, Debug)]
pub struct DeterministicCertificationEvaluator {
    pub now: DateTime<Utc>,
}

impl Default for DeterministicCertificationEvaluator {
    fn default() -> Self {
        Self { now: Utc::now() }
    }
}

impl CertificationEvaluator for DeterministicCertificationEvaluator {
    fn evaluate(
        &self,
        skill: &SkillRevisionRef,
        contract: &BehavioralContract,
        profile: &AssuranceProfile,
        evidence: &EvidenceManifest,
    ) -> Result<CertificationEvaluation> {
        profile.validate()?;
        evidence.verify_hash()?;
        if evidence.subject != EvidenceSubject::Skill(skill.clone()) {
            return Err(Error::InvalidInput(
                "Evidence Manifest subject does not match Skill revision".into(),
            ));
        }
        let observability = contract_observability(contract)?;
        let mut gaps = evidence
            .summary
            .known_unknowns
            .iter()
            .map(|unknown| AssuranceGap {
                kind: AssuranceGapKind::UntestedCondition,
                description: unknown.clone(),
                severity: None,
            })
            .collect::<Vec<_>>();
        if !observability.unobservable.is_empty() {
            gaps.push(AssuranceGap {
                kind: AssuranceGapKind::ContractInconclusive,
                description: format!(
                    "{} required contract conditions are not observable",
                    observability.unobservable.len()
                ),
                severity: Some(Severity::High),
            });
        }

        let mut blockers = hard_blockers(contract, evidence);
        let capability = capability_compliance(contract, evidence);
        if let Err(description) = capability {
            blockers.push(CertificationBlocker {
                description,
                severity: Severity::Critical,
                evidence_ids: evidence
                    .capability_manifests
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            });
        }

        let mut requirements = vec![];
        for requirement in &profile.requirements {
            let (status, detail, gap) = evaluate_requirement(requirement, evidence, self.now);
            requirements.push(AssuranceRequirementResult {
                requirement: requirement.clone(),
                status,
                evidence: detail,
            });
            if let Some(gap) = gap {
                gaps.push(gap);
            }
        }
        if !observability.unobservable.is_empty() {
            requirements.push(AssuranceRequirementResult {
                requirement: AssuranceRequirement::Custom {
                    kind: "contract_observability".into(),
                    payload: serde_json::json!({"required":true}),
                },
                status: AssuranceRequirementStatus::Inconclusive,
                evidence: format!(
                    "{} contract conditions unobservable",
                    observability.unobservable.len()
                ),
            });
        }
        let recommendation = if !blockers.is_empty() {
            CertificationRecommendation::Blocked
        } else if requirements
            .iter()
            .all(|result| result.status == AssuranceRequirementStatus::Satisfied)
        {
            CertificationRecommendation::Eligible
        } else {
            CertificationRecommendation::AdditionalEvidenceRequired
        };
        Ok(CertificationEvaluation {
            dimensions: assurance_dimensions(&requirements, evidence),
            requirements,
            gaps,
            blockers,
            recommendation,
        })
    }
}

fn hard_blockers(
    contract: &BehavioralContract,
    evidence: &EvidenceManifest,
) -> Vec<CertificationBlocker> {
    let critical_invariants = contract
        .invariants
        .iter()
        .filter(|invariant| invariant.severity == Severity::Critical)
        .map(|invariant| invariant.id.clone())
        .collect::<BTreeSet<_>>();
    let critical_forbidden = contract
        .forbidden_outcomes
        .iter()
        .filter(|outcome| outcome.severity == Severity::Critical)
        .map(|outcome| outcome.id.clone())
        .collect::<BTreeSet<_>>();
    let mut blockers = vec![];
    for evaluation in &evidence.summary.contract_evaluations {
        for result in &evaluation.invariants {
            if result.status == ContractEvaluationStatus::Violated
                && critical_invariants.contains(&result.invariant_id)
            {
                blockers.push(CertificationBlocker {
                    description: format!("Critical invariant violated: {}", result.reason),
                    severity: Severity::Critical,
                    evidence_ids: vec![],
                });
            }
        }
        for result in &evaluation.forbidden_outcomes {
            if result.status == ContractEvaluationStatus::Violated
                && critical_forbidden.contains(&result.outcome_id)
            {
                blockers.push(CertificationBlocker {
                    description: format!("Critical forbidden outcome: {}", result.reason),
                    severity: Severity::Critical,
                    evidence_ids: vec![],
                });
            }
        }
    }
    if evidence.summary.experimental_effect_leak {
        blockers.push(CertificationBlocker {
            description: "losing experimental branch committed an external effect".into(),
            severity: Severity::Critical,
            evidence_ids: evidence
                .effect_receipts
                .iter()
                .map(ToString::to_string)
                .collect(),
        });
    }
    if !evidence.summary.attestations_intact && !evidence.attestations.is_empty() {
        blockers.push(CertificationBlocker {
            description: "execution attestation integrity failed".into(),
            severity: Severity::Critical,
            evidence_ids: evidence
                .attestations
                .iter()
                .map(ToString::to_string)
                .collect(),
        });
    }
    blockers
}

fn capability_compliance(
    contract: &BehavioralContract,
    evidence: &EvidenceManifest,
) -> std::result::Result<(), String> {
    let summary = &evidence.summary;
    if (!contract.capability_requirements.forbidden.is_empty()
        || !contract.capability_requirements.required.is_empty()
        || contract.capability_requirements.maximum.is_some())
        && !summary.capability_observed
    {
        return Err("required capability surface was not observable".into());
    }
    for forbidden in &contract.capability_requirements.forbidden {
        if summary
            .observed_capabilities
            .iter()
            .any(|capability| forbidden.matches(capability))
        {
            return Err(format!("forbidden capability observed: {forbidden:?}"));
        }
    }
    for required in &contract.capability_requirements.required {
        if !summary
            .observed_capabilities
            .iter()
            .any(|capability| required.matches(capability))
        {
            return Err(format!("required capability not observed: {required:?}"));
        }
    }
    if let Some(maximum) = &contract.capability_requirements.maximum {
        if maximum.deny_ambient_credentials && summary.ambient_credentials_observed {
            return Err("ambient credentials exceed the contract maximum".into());
        }
        if maximum.deny_effect_commit && summary.effect_commit_granted {
            return Err("effect commit authority exceeds the contract maximum".into());
        }
        if summary.capability_maximum_exceeded
            || summary
                .observed_capabilities
                .iter()
                .any(|capability| !maximum.permits(capability))
        {
            return Err("observed capability exceeds the declared maximum".into());
        }
    }
    Ok(())
}

fn evaluate_requirement(
    requirement: &AssuranceRequirement,
    evidence: &EvidenceManifest,
    now: DateTime<Utc>,
) -> (AssuranceRequirementStatus, String, Option<AssuranceGap>) {
    let summary = &evidence.summary;
    match requirement {
        AssuranceRequirement::ContractSatisfied { minimum_runs } => {
            let satisfied = summary
                .contract_evaluations
                .iter()
                .filter(|evaluation| evaluation.overall == ContractEvaluationStatus::Satisfied)
                .count();
            let inconclusive = summary
                .contract_evaluations
                .iter()
                .any(|evaluation| evaluation.overall == ContractEvaluationStatus::Inconclusive);
            count_requirement(
                satisfied,
                *minimum_runs,
                inconclusive,
                "contract-satisfying runs",
            )
        }
        AssuranceRequirement::ControlledExperiments { minimum } => count_requirement(
            summary.controlled_experiments,
            *minimum,
            false,
            "controlled experiments",
        ),
        AssuranceRequirement::PerturbationProfileCoverage {
            profile,
            minimum_fraction,
        } => {
            let Some(actual) = summary.perturbation_profile_coverage.get(profile) else {
                return (
                    AssuranceRequirementStatus::Inconclusive,
                    format!("profile {profile} has no observed distinct conditions"),
                    Some(AssuranceGap {
                        kind: AssuranceGapKind::UntestedCondition,
                        description: format!("perturbation profile {profile} is untested"),
                        severity: Some(Severity::High),
                    }),
                );
            };
            let status = if actual >= minimum_fraction {
                AssuranceRequirementStatus::Satisfied
            } else {
                AssuranceRequirementStatus::Violated
            };
            (
                status,
                format!("distinct profile coverage {actual:.3}, required {minimum_fraction:.3}"),
                (status != AssuranceRequirementStatus::Satisfied).then(|| AssuranceGap {
                    kind: AssuranceGapKind::InsufficientEvidence,
                    description: format!("profile {profile} coverage below requirement"),
                    severity: Some(Severity::High),
                }),
            )
        }
        AssuranceRequirement::RecoveryCoverage {
            minimum_high_severity_classes,
        } => count_requirement(
            summary.high_severity_recovery_classes.len(),
            *minimum_high_severity_classes,
            false,
            "high-severity recovery classes",
        ),
        AssuranceRequirement::ReflexFalsePositiveMaximum { maximum } => {
            if summary.reflex_checks == 0 {
                return (
                    AssuranceRequirementStatus::Inconclusive,
                    "no Reflex negative controls".into(),
                    Some(AssuranceGap {
                        kind: AssuranceGapKind::InsufficientEvidence,
                        description: "Reflex false-positive behavior is untested".into(),
                        severity: Some(Severity::Medium),
                    }),
                );
            }
            let actual = summary.reflex_false_positives as f64 / summary.reflex_checks as f64;
            (
                if actual <= *maximum {
                    AssuranceRequirementStatus::Satisfied
                } else {
                    AssuranceRequirementStatus::Violated
                },
                format!("false-positive rate {actual:.3}, maximum {maximum:.3}"),
                None,
            )
        }
        AssuranceRequirement::CapabilityProfile { required_profile } => {
            let satisfied = summary
                .capability_profiles_satisfied
                .contains(required_profile);
            (
                if !summary.capability_observed {
                    AssuranceRequirementStatus::Inconclusive
                } else if satisfied {
                    AssuranceRequirementStatus::Satisfied
                } else {
                    AssuranceRequirementStatus::Violated
                },
                format!("required capability profile {required_profile}"),
                (!satisfied).then(|| AssuranceGap {
                    kind: AssuranceGapKind::UnsupportedIsolation,
                    description: format!("capability profile {required_profile} not established"),
                    severity: Some(Severity::High),
                }),
            )
        }
        AssuranceRequirement::CapabilityMinimizationValidated => (
            if summary.capability_minimization_validated {
                AssuranceRequirementStatus::Satisfied
            } else {
                AssuranceRequirementStatus::Inconclusive
            },
            "capability minimization curriculum evidence".into(),
            (!summary.capability_minimization_validated).then(|| AssuranceGap {
                kind: AssuranceGapKind::InsufficientEvidence,
                description: "capability minimization has not been validated".into(),
                severity: Some(Severity::High),
            }),
        ),
        AssuranceRequirement::NoUnresolvedCriticalContradictions => {
            let contradictions = summary
                .contradictions
                .iter()
                .filter(|value| value.severity == Severity::Critical && !value.resolved)
                .count();
            (
                if contradictions == 0 {
                    AssuranceRequirementStatus::Satisfied
                } else {
                    AssuranceRequirementStatus::Violated
                },
                format!("{contradictions} unresolved Critical contradictions"),
                (contradictions > 0).then(|| AssuranceGap {
                    kind: AssuranceGapKind::ContradictoryEvidence,
                    description: format!("{contradictions} unresolved Critical contradictions"),
                    severity: Some(Severity::Critical),
                }),
            )
        }
        AssuranceRequirement::EvidenceFreshness { maximum_age_days } => {
            let Some(oldest) = summary.oldest_required_evidence else {
                return (
                    AssuranceRequirementStatus::Inconclusive,
                    "required evidence has no timestamp".into(),
                    Some(AssuranceGap {
                        kind: AssuranceGapKind::StaleEvidence,
                        description: "evidence freshness cannot be established".into(),
                        severity: Some(Severity::Medium),
                    }),
                );
            };
            let fresh = maximum_age_days.is_none_or(|days| {
                now.signed_duration_since(oldest) <= Duration::days(days.into())
            });
            (
                if fresh {
                    AssuranceRequirementStatus::Satisfied
                } else {
                    AssuranceRequirementStatus::Violated
                },
                format!("oldest required evidence {oldest}"),
                (!fresh).then(|| AssuranceGap {
                    kind: AssuranceGapKind::StaleEvidence,
                    description: "required evidence exceeds profile freshness".into(),
                    severity: Some(Severity::Medium),
                }),
            )
        }
        AssuranceRequirement::ExecutionAttestation { minimum_assurance } => {
            let total = summary.attestation_assurance.len();
            let qualifying = summary
                .attestation_assurance
                .iter()
                .filter(|assurance| {
                    assurance_rank(**assurance) >= assurance_rank(*minimum_assurance)
                })
                .count();
            (
                if !summary.attestations_intact {
                    AssuranceRequirementStatus::Violated
                } else if total == 0 {
                    AssuranceRequirementStatus::Inconclusive
                } else if qualifying != total {
                    AssuranceRequirementStatus::Violated
                } else {
                    AssuranceRequirementStatus::Satisfied
                },
                format!("{qualifying}/{total} attestations at {minimum_assurance:?} or stronger"),
                (total == 0 || qualifying != total).then(|| AssuranceGap {
                    kind: AssuranceGapKind::UnsupportedIsolation,
                    description: format!("no qualifying {minimum_assurance:?} attestation"),
                    severity: Some(Severity::High),
                }),
            )
        }
        AssuranceRequirement::EvidenceDiversity { minimum } => {
            let Some(actual) = summary.evidence_diversity else {
                return (
                    AssuranceRequirementStatus::Inconclusive,
                    format!("evidence diversity metadata is unavailable; required {minimum:?}"),
                    Some(AssuranceGap {
                        kind: AssuranceGapKind::InsufficientEvidence,
                        description: "known evidence dependencies have not been assessed".into(),
                        severity: Some(Severity::High),
                    }),
                );
            };
            let satisfied = actual.satisfies(*minimum);
            (
                if satisfied {
                    AssuranceRequirementStatus::Satisfied
                } else {
                    AssuranceRequirementStatus::Violated
                },
                format!(
                    "evidence diversity {actual:?}, required {minimum:?}; {} source types, {} evaluator families, {} root origins",
                    summary.evidence_source_types,
                    summary.evaluator_kinds,
                    summary.root_evidence_origins,
                ),
                (!satisfied).then(|| AssuranceGap {
                    kind: AssuranceGapKind::InsufficientEvidence,
                    description: format!(
                        "evidence diversity {actual:?} is below {minimum:?}; inspect known dependency overlap"
                    ),
                    severity: Some(Severity::High),
                }),
            )
        }
        AssuranceRequirement::CausalFailureCoverage {
            severity,
            minimum_supported_mechanisms,
        } => {
            let count = summary
                .causal_mechanisms
                .values()
                .filter(|s| *s >= severity)
                .count();
            let satisfied = count >= *minimum_supported_mechanisms;
            (
                if satisfied {
                    AssuranceRequirementStatus::Satisfied
                } else {
                    AssuranceRequirementStatus::Inconclusive
                },
                format!(
                    "{count} locally supported mechanisms at {severity:?} or higher; required {minimum_supported_mechanisms}"
                ),
                (!satisfied).then(|| AssuranceGap {
                    kind: AssuranceGapKind::InsufficientEvidence,
                    description: "Causal failure coverage is incomplete under this profile".into(),
                    severity: Some(*severity),
                }),
            )
        }
        AssuranceRequirement::Custom { kind, .. } => (
            AssuranceRequirementStatus::Inconclusive,
            format!("no deterministic evaluator registered for custom requirement {kind}"),
            Some(AssuranceGap {
                kind: AssuranceGapKind::InsufficientEvidence,
                description: format!("custom requirement {kind} cannot be evaluated"),
                severity: None,
            }),
        ),
    }
}

fn count_requirement(
    actual: usize,
    required: usize,
    inconclusive: bool,
    label: &str,
) -> (AssuranceRequirementStatus, String, Option<AssuranceGap>) {
    let status = if actual >= required {
        AssuranceRequirementStatus::Satisfied
    } else if inconclusive || actual == 0 {
        AssuranceRequirementStatus::Inconclusive
    } else {
        AssuranceRequirementStatus::Violated
    };
    (
        status,
        format!("{actual} {label}; {required} required"),
        (status != AssuranceRequirementStatus::Satisfied).then(|| AssuranceGap {
            kind: AssuranceGapKind::InsufficientEvidence,
            description: format!("{label}: {actual}/{required}"),
            severity: Some(Severity::High),
        }),
    )
}

fn assurance_rank(value: AttestationAssurance) -> u8 {
    match value {
        AttestationAssurance::Observed => 0,
        AttestationAssurance::IsolatedObserved => 1,
        AttestationAssurance::RuntimeVerified => 2,
        AttestationAssurance::HardwareBacked => 3,
    }
}

fn assurance_dimensions(
    requirements: &[AssuranceRequirementResult],
    evidence: &EvidenceManifest,
) -> Vec<AssuranceDimensionResult> {
    let dimension_status = |matches: fn(&AssuranceRequirement) -> bool| {
        requirements
            .iter()
            .filter(|result| matches(&result.requirement))
            .map(|result| result.status)
            .reduce(combine_status)
            .unwrap_or(AssuranceRequirementStatus::Inconclusive)
    };
    let dimensions = [
        (
            AssuranceDimension::Behavior,
            dimension_status(|r| matches!(r, AssuranceRequirement::ContractSatisfied { .. })),
        ),
        (
            AssuranceDimension::Resilience,
            dimension_status(|r| {
                matches!(
                    r,
                    AssuranceRequirement::ControlledExperiments { .. }
                        | AssuranceRequirement::PerturbationProfileCoverage { .. }
                        | AssuranceRequirement::ReflexFalsePositiveMaximum { .. }
                )
            }),
        ),
        (
            AssuranceDimension::Recovery,
            dimension_status(|r| matches!(r, AssuranceRequirement::RecoveryCoverage { .. })),
        ),
        (
            AssuranceDimension::CapabilityDiscipline,
            dimension_status(|r| {
                matches!(
                    r,
                    AssuranceRequirement::CapabilityProfile { .. }
                        | AssuranceRequirement::CapabilityMinimizationValidated
                        | AssuranceRequirement::ExecutionAttestation { .. }
                )
            }),
        ),
        (
            AssuranceDimension::EffectDiscipline,
            if evidence.summary.experimental_effect_leak {
                AssuranceRequirementStatus::Violated
            } else {
                AssuranceRequirementStatus::Satisfied
            },
        ),
        (
            AssuranceDimension::EvidenceFreshness,
            dimension_status(|r| matches!(r, AssuranceRequirement::EvidenceFreshness { .. })),
        ),
        (
            AssuranceDimension::EvidenceDiversity,
            dimension_status(|r| matches!(r, AssuranceRequirement::EvidenceDiversity { .. })),
        ),
    ];
    dimensions
        .into_iter()
        .map(|(dimension, status)| AssuranceDimensionResult {
            dimension,
            status,
            explanation: format!("profile-relative {dimension:?} evidence is {status:?}"),
        })
        .collect()
}

fn combine_status(
    left: AssuranceRequirementStatus,
    right: AssuranceRequirementStatus,
) -> AssuranceRequirementStatus {
    if left == AssuranceRequirementStatus::Violated || right == AssuranceRequirementStatus::Violated
    {
        AssuranceRequirementStatus::Violated
    } else if left == AssuranceRequirementStatus::Inconclusive
        || right == AssuranceRequirementStatus::Inconclusive
    {
        AssuranceRequirementStatus::Inconclusive
    } else {
        AssuranceRequirementStatus::Satisfied
    }
}

pub trait CertificationFreshnessPolicy {
    fn status(
        &self,
        certification: &SkillCertification,
        current: &CurrentAssuranceContext,
    ) -> CertificationFreshness;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicFreshnessPolicy;

impl CertificationFreshnessPolicy for DeterministicFreshnessPolicy {
    fn status(
        &self,
        certification: &SkillCertification,
        current: &CurrentAssuranceContext,
    ) -> CertificationFreshness {
        if current.invalidated
            || current.skill_revision != certification.skill.revision
            || current.contract_revision != certification.contract.revision
        {
            return CertificationFreshness::Invalidated;
        }
        if certification
            .expires_at
            .is_some_and(|expires| current.now >= expires)
        {
            return CertificationFreshness::Expired;
        }
        if current.tool_artifact_hashes != certification.tool_artifact_hashes
            || current.runtime_digests != certification.runtime_digests
        {
            return CertificationFreshness::ReviewRecommended;
        }
        CertificationFreshness::Current
    }
}

pub fn builtin_profiles() -> Vec<AssuranceProfile> {
    let created_at = DateTime::<Utc>::UNIX_EPOCH;
    let id = |value: &str| {
        AssuranceProfileId::from_str(value).expect("built-in Assurance Profile IDs are valid")
    };
    vec![
        AssuranceProfile {
            id: id("assurance-profile-00000000-0000-4000-8000-000000000001"),
            name: "basic-behavior-v1".into(),
            version: "1".into(),
            requirements: vec![
                AssuranceRequirement::ContractSatisfied { minimum_runs: 1 },
                AssuranceRequirement::NoUnresolvedCriticalContradictions,
                AssuranceRequirement::EvidenceFreshness {
                    maximum_age_days: Some(90),
                },
            ],
            created_at,
        },
        AssuranceProfile {
            id: id("assurance-profile-00000000-0000-4000-8000-000000000002"),
            name: "resilience-basic-v1".into(),
            version: "1".into(),
            requirements: vec![
                AssuranceRequirement::ContractSatisfied { minimum_runs: 1 },
                AssuranceRequirement::ControlledExperiments { minimum: 1 },
                AssuranceRequirement::PerturbationProfileCoverage {
                    profile: "resilience-basic-v1".into(),
                    minimum_fraction: 0.5,
                },
                AssuranceRequirement::RecoveryCoverage {
                    minimum_high_severity_classes: 1,
                },
                AssuranceRequirement::ReflexFalsePositiveMaximum { maximum: 0.05 },
                AssuranceRequirement::NoUnresolvedCriticalContradictions,
                AssuranceRequirement::EvidenceFreshness {
                    maximum_age_days: Some(60),
                },
            ],
            created_at,
        },
        AssuranceProfile {
            id: id("assurance-profile-00000000-0000-4000-8000-000000000003"),
            name: "capability-minimal-v1".into(),
            version: "1".into(),
            requirements: vec![
                AssuranceRequirement::ContractSatisfied { minimum_runs: 1 },
                AssuranceRequirement::CapabilityProfile {
                    required_profile: "capability-minimal-v1".into(),
                },
                AssuranceRequirement::CapabilityMinimizationValidated,
                AssuranceRequirement::ExecutionAttestation {
                    minimum_assurance: AttestationAssurance::IsolatedObserved,
                },
                AssuranceRequirement::NoUnresolvedCriticalContradictions,
            ],
            created_at,
        },
        AssuranceProfile {
            id: id("assurance-profile-00000000-0000-4000-8000-000000000004"),
            name: "epistemic-diversity-basic-v1".into(),
            version: "1".into(),
            requirements: vec![
                AssuranceRequirement::ContractSatisfied { minimum_runs: 1 },
                AssuranceRequirement::ControlledExperiments { minimum: 1 },
                AssuranceRequirement::EvidenceDiversity {
                    minimum: crate::epistemic::DiversityClass::Moderate,
                },
                AssuranceRequirement::NoUnresolvedCriticalContradictions,
                AssuranceRequirement::EvidenceFreshness {
                    maximum_age_days: Some(60),
                },
            ],
            created_at,
        },
    ]
}

pub fn builtin_profile(name: &str) -> Result<AssuranceProfile> {
    builtin_profiles()
        .into_iter()
        .find(|profile| profile.name == name)
        .ok_or_else(|| Error::NotFound(format!("Assurance Profile {name} not found")))
}

pub fn certification_curriculum(evaluation: &CertificationEvaluation) -> Vec<CurriculumGoal> {
    evaluation
        .gaps
        .iter()
        .enumerate()
        .map(|(index, gap)| {
            let severity = gap.severity.unwrap_or(Severity::Medium);
            let priority = if severity >= Severity::High {
                Priority::High
            } else if severity >= Severity::Medium {
                Priority::Medium
            } else {
                Priority::Low
            };
            CurriculumGoal {
                id: CurriculumGoalId::new(),
                kind: if gap.kind == AssuranceGapKind::ContractInconclusive {
                    CurriculumGoalKind::ChallengeInvariant
                } else {
                    CurriculumGoalKind::SatisfyAssuranceRequirement
                },
                description: gap.description.clone(),
                priority,
                score: PriorityScore {
                    score: 10_000_u64.saturating_sub(index as u64),
                    priority,
                    explanation: "Missing profile evidence; distinct conditions take precedence over repeated easy trials".into(),
                },
                evidence_gap: EvidenceGap {
                    dimension: format!("{:?}", gap.kind),
                    known_values: vec![],
                    unknown_values: vec![gap.description.clone()],
                    rationale: "Certification remains ineligible until this named evidence gap is resolved".into(),
                },
                status: GoalStatus::Planned,
                decision: CurriculumDecision::RequiresApproval,
                reason: "Certification never runs or commits consequential effects automatically".into(),
                severity,
                safety: TrialSafety::RequiresIsolation,
            }
        })
        .collect()
}
