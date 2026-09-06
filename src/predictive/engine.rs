// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{
    Error, Result,
    causal::CausalHypothesisRef,
    core::{FailureForecastId, FailureTrajectoryId},
};
use chrono::Utc;
use std::collections::{BTreeMap, BTreeSet};

pub trait EarlyWarningMatcher {
    fn match_active(
        &self,
        trajectory: &ExecutionTrajectory,
        events: &[TrajectoryEvent],
        signatures: &[EarlyWarningSignature],
        window: &TrajectoryWindow,
    ) -> Vec<EarlyWarningMatch>;
}

pub trait TrajectoryMatcher {
    fn match_trajectory(
        &self,
        current: &ExecutionTrajectory,
        history: &[ExecutionTrajectory],
    ) -> Vec<TrajectoryMatch>;

    fn match_pattern(
        &self,
        trajectory: &ExecutionTrajectory,
        pattern: &FailureTrajectory,
    ) -> FailureTrajectoryMatch;
}

pub trait FailureForecaster {
    fn forecast(
        &self,
        trajectory: &ExecutionTrajectory,
        known_failures: &[FailureTrajectory],
        context: &ForecastContext,
    ) -> Result<Vec<FailureForecast>>;
}

pub trait EarlyWarningCandidateGenerator {
    fn generate(
        &self,
        failures: &[ExecutionTrajectory],
        successes: &[ExecutionTrajectory],
        causal_model: Option<&crate::causal::ContextualCausalModel>,
    ) -> Result<Vec<EarlyWarningSignature>>;
}

pub trait ForecastPolicy {
    fn classify(
        &self,
        match_result: &FailureTrajectoryMatch,
        causal_context: Option<&CausalHypothesisRef>,
        envelope: EnvelopeProximity,
        risk: crate::curriculum::Severity,
        config: &ForecastPolicyConfig,
    ) -> ForecastStatus;
}

pub trait FailureForecastEngine {
    fn forecast(
        &self,
        trajectory: &ExecutionTrajectory,
        context: &ForecastContext,
    ) -> Result<Vec<FailureForecast>>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicTrajectoryMatcher;

pub(crate) fn scope_compatible(
    expected: &crate::lesson::ContextSelector,
    actual: &TrajectoryContext,
) -> bool {
    expected
        .repository
        .as_ref()
        .is_none_or(|value| actual.scope.repository.as_ref() == Some(value))
        && expected
            .os
            .as_ref()
            .is_none_or(|value| actual.scope.os.as_ref() == Some(value))
        && expected
            .arch
            .as_ref()
            .is_none_or(|value| actual.scope.arch.as_ref() == Some(value))
        && expected
            .required_markers
            .iter()
            .all(|value| actual.scope.required_markers.contains(value))
        && expected
            .tags
            .iter()
            .all(|value| actual.scope.tags.contains(value))
}

impl TrajectoryMatcher for DeterministicTrajectoryMatcher {
    fn match_trajectory(
        &self,
        current: &ExecutionTrajectory,
        history: &[ExecutionTrajectory],
    ) -> Vec<TrajectoryMatch> {
        let mut matches = Vec::new();
        for prior in history {
            if prior.id == current.id || prior.outcome.is_none() {
                continue;
            }
            let compatibility = if prior.context == current.context {
                ContextCompatibility::Exact
            } else if current.task_family == prior.task_family
                && scope_compatible(&prior.context.scope, &current.context)
                && scope_compatible(&current.context.scope, &prior.context)
            {
                ContextCompatibility::Compatible
            } else {
                ContextCompatibility::OutOfScope
            };
            if compatibility == ContextCompatibility::OutOfScope {
                continue;
            }
            let matched_events = current
                .fingerprint
                .event_kinds
                .iter()
                .zip(&prior.fingerprint.event_kinds)
                .take_while(|(left, right)| left == right)
                .count();
            if matched_events == 0 {
                continue;
            }
            matches.push(TrajectoryMatch {
                trajectory: prior.id.clone(),
                matched_events,
                matching_indicators: Vec::new(),
                context_compatibility: compatibility,
                outcome: prior.outcome.clone().expect("checked above"),
            });
        }
        matches.sort_by_key(|item| {
            (
                std::cmp::Reverse(item.matched_events),
                item.trajectory.clone(),
            )
        });
        matches
    }

    fn match_pattern(
        &self,
        trajectory: &ExecutionTrajectory,
        pattern: &FailureTrajectory,
    ) -> FailureTrajectoryMatch {
        let mut cursor = 0_usize;
        let mut previous = None::<usize>;
        let mut matched = Vec::new();
        let mut missing = Vec::new();
        for (step_index, step) in pattern.sequence.iter().enumerate() {
            let found =
                trajectory
                    .points
                    .iter()
                    .enumerate()
                    .skip(cursor)
                    .find(|(point_index, point)| {
                        let condition_matches =
                            step.condition
                                .as_ref()
                                .is_none_or(|condition| match condition {
                                    TrajectoryCondition::EventObserved {
                                        predicate: expected,
                                    } => {
                                        point.event == expected.kind
                                            && expected.feature.as_ref().is_none_or(|predicate| {
                                                point
                                                    .state
                                                    .variables
                                                    .get(&predicate.feature)
                                                    .is_some_and(|observed| {
                                                        compare(
                                                            observed,
                                                            &predicate.operator,
                                                            &predicate.value,
                                                        )
                                                    })
                                            })
                                    }
                                    TrajectoryCondition::FeatureCondition { predicate } => point
                                        .state
                                        .variables
                                        .get(&predicate.feature)
                                        .is_some_and(|observed| {
                                            compare(observed, &predicate.operator, &predicate.value)
                                        }),
                                    _ => true,
                                });
                        let event_matches = step
                            .event_pattern
                            .as_ref()
                            .is_none_or(|expected| expected.kind == point.event);
                        let state_matches =
                            step.state_predicates.iter().all(|predicate| {
                                point.state.variables.get(&predicate.feature).is_some_and(
                                    |observed| {
                                        compare(observed, &predicate.operator, &predicate.value)
                                    },
                                )
                            });
                        let temporal_matches = step.temporal.as_ref().is_none_or(|constraint| {
                            previous.is_none_or(|prior| {
                                let event_gap = point_index.saturating_sub(prior + 1);
                                let event_ok = constraint
                                    .max_events_between
                                    .is_none_or(|max| event_gap <= max as usize);
                                let duration_ok = constraint.max_duration_ms.is_none_or(|max| {
                                    point
                                        .timestamp
                                        .signed_duration_since(trajectory.points[prior].timestamp)
                                        .num_milliseconds()
                                        .try_into()
                                        .is_ok_and(|elapsed: u64| elapsed <= max)
                                });
                                event_ok && duration_ok
                            })
                        });
                        condition_matches && event_matches && state_matches && temporal_matches
                    });
            if let Some((point_index, _)) = found {
                matched.push(step_index);
                previous = Some(point_index);
                cursor = point_index.saturating_add(1);
            } else {
                missing.push(step_index);
            }
        }
        let total_steps = pattern.sequence.len();
        let status = if matched.is_empty() {
            TrajectoryMatchStatus::Weak
        } else if matched.len() == total_steps {
            TrajectoryMatchStatus::Strong
        } else if matched.len() * 2 >= total_steps {
            TrajectoryMatchStatus::Partial
        } else {
            TrajectoryMatchStatus::Weak
        };
        FailureTrajectoryMatch {
            failure_trajectory: pattern.id.clone(),
            matched_steps: matched.len(),
            total_steps,
            matched,
            missing,
            contradicted: Vec::new(),
            status,
            last_match_index: previous.and_then(|value| u64::try_from(value).ok()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicForecastPolicy;

impl ForecastPolicy for DeterministicForecastPolicy {
    fn classify(
        &self,
        match_result: &FailureTrajectoryMatch,
        causal_context: Option<&CausalHypothesisRef>,
        envelope: EnvelopeProximity,
        risk: crate::curriculum::Severity,
        config: &ForecastPolicyConfig,
    ) -> ForecastStatus {
        if match_result.status == TrajectoryMatchStatus::Contradicted
            || match_result.matched_steps == 0
        {
            return ForecastStatus::Cleared;
        }
        if match_result.matched_steps == match_result.total_steps {
            return if causal_context.is_some() && risk >= crate::curriculum::Severity::High {
                ForecastStatus::Imminent
            } else {
                ForecastStatus::Actionable
            };
        }
        if match_result.matched_steps >= config.actionable_matched_steps
            && (causal_context.is_some()
                || matches!(
                    envelope,
                    EnvelopeProximity::AtBoundary | EnvelopeProximity::OutsideKnownSafeRegion
                ))
        {
            ForecastStatus::Actionable
        } else if match_result.matched_steps >= config.elevated_matched_steps {
            ForecastStatus::Elevated
        } else {
            ForecastStatus::Watch
        }
    }
}

/// Deliberately small candidate generation over observable, meaningful event/state
/// properties. It finds ordered properties shared by the failure set and rejects
/// properties that occur in half or more of the successful controls. Candidates
/// still require held-out and prospective validation before runtime use.
#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicEarlyWarningCandidateGenerator;

impl EarlyWarningCandidateGenerator for DeterministicEarlyWarningCandidateGenerator {
    fn generate(
        &self,
        failures: &[ExecutionTrajectory],
        successes: &[ExecutionTrajectory],
        causal_model: Option<&crate::causal::ContextualCausalModel>,
    ) -> Result<Vec<EarlyWarningSignature>> {
        let Some(first) = failures.first() else {
            return Ok(Vec::new());
        };
        if successes.is_empty() {
            return Err(Error::InvalidInput(
                "Early-warning candidate generation requires successful negative controls".into(),
            ));
        }
        let Some(TrajectoryOutcome::Failure(target)) = &first.outcome else {
            return Err(Error::InvalidInput(
                "Candidate generation requires completed failure trajectories".into(),
            ));
        };
        if failures.iter().any(|trajectory| {
            !matches!(&trajectory.outcome, Some(TrajectoryOutcome::Failure(observed)) if observed == target)
                || !scope_compatible(&first.context.scope, &trajectory.context)
        }) || successes.iter().any(|trajectory| {
            trajectory.outcome != Some(TrajectoryOutcome::Success)
                || !scope_compatible(&first.context.scope, &trajectory.context)
        }) {
            return Err(Error::InvalidInput(
                "Candidate generation requires one failure class and compatible failure/success scopes"
                    .into(),
            ));
        }

        let keys = |trajectory: &ExecutionTrajectory| {
            trajectory
                .points
                .iter()
                .filter(|point| point.event != TrajectoryEventKind::FailureObserved)
                .flat_map(|point| {
                    point.state.variables.iter().map(move |(feature, value)| {
                        (
                            serde_json::to_string(&(&point.event, feature, value))
                                .expect("trajectory properties serialize"),
                            point.event.clone(),
                            feature.clone(),
                            value.clone(),
                        )
                    })
                })
                .collect::<Vec<_>>()
        };
        let failure_sets = failures
            .iter()
            .map(|trajectory| {
                keys(trajectory)
                    .into_iter()
                    .map(|(key, _, _, _)| key)
                    .collect::<BTreeSet<_>>()
            })
            .collect::<Vec<_>>();
        let success_sets = successes
            .iter()
            .map(|trajectory| {
                keys(trajectory)
                    .into_iter()
                    .map(|(key, _, _, _)| key)
                    .collect::<BTreeSet<_>>()
            })
            .collect::<Vec<_>>();

        let mut conditions = Vec::new();
        for (key, kind, feature, value) in keys(first) {
            let shared = failure_sets.iter().all(|set| set.contains(&key));
            let control_matches = success_sets.iter().filter(|set| set.contains(&key)).count();
            if shared && control_matches.saturating_mul(2) < successes.len() {
                conditions.push(TrajectoryCondition::EventObserved {
                    predicate: EventPredicate {
                        kind,
                        feature: Some(FeaturePredicate {
                            feature,
                            operator: ComparisonOperator::Equals,
                            value,
                        }),
                    },
                });
            }
            if conditions.len() == 4 {
                break;
            }
        }
        if conditions.is_empty() {
            return Ok(Vec::new());
        }
        let causal_basis = causal_model
            .into_iter()
            .flat_map(|model| model.edges.iter())
            .filter(|edge| edge.status.supported())
            .map(|edge| edge.hypothesis.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let now = Utc::now();
        Ok(vec![EarlyWarningSignature {
            id: crate::core::EarlyWarningSignatureId::new(),
            failure: target.clone(),
            ordered_conditions: conditions,
            failure_trajectory: None,
            horizon: ForecastHorizon::BeforeTaskCompletion,
            scope: first.context.scope.clone(),
            evidence: failures
                .iter()
                .map(|trajectory| TrajectoryEvidenceRef::Trajectory(trajectory.id.clone()))
                .collect(),
            status: RiskIndicatorStatus::Candidate,
            revision: 1,
            origin: PredictiveOrigin::Local,
            causal_basis,
            required_runtime_version: first.context.runtime_version.clone(),
            precision: None,
            recall: None,
            created_at: now,
            updated_at: now,
        }])
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicEarlyWarningMatcher;

fn compare(left: &TrajectoryValue, operator: &ComparisonOperator, right: &TrajectoryValue) -> bool {
    use ComparisonOperator::*;
    match operator {
        Equals => left == right,
        NotEquals => left != right,
        AtLeast | AtMost | GreaterThan | LessThan => {
            let numbers = match (left, right) {
                (TrajectoryValue::Integer(a), TrajectoryValue::Integer(b)) => {
                    Some((*a as f64, *b as f64))
                }
                (TrajectoryValue::Decimal(a), TrajectoryValue::Decimal(b)) => {
                    a.parse::<f64>().ok().zip(b.parse::<f64>().ok())
                }
                _ => None,
            };
            numbers.is_some_and(|(a, b)| match operator {
                AtLeast => a >= b,
                AtMost => a <= b,
                GreaterThan => a > b,
                LessThan => a < b,
                _ => false,
            })
        }
    }
}

fn predicate(event: &TrajectoryEvent, value: &FeaturePredicate) -> bool {
    event
        .observation
        .features
        .get(&value.feature)
        .is_some_and(|observed| compare(observed, &value.operator, &value.value))
}

fn direct_match(condition: &TrajectoryCondition, event: &TrajectoryEvent) -> bool {
    match condition {
        TrajectoryCondition::EventObserved {
            predicate: expected,
        } => {
            event.kind == expected.kind
                && expected
                    .feature
                    .as_ref()
                    .is_none_or(|feature| predicate(event, feature))
        }
        TrajectoryCondition::FeatureCondition {
            predicate: expected,
        } => predicate(event, expected),
        _ => false,
    }
}

fn match_positions(condition: &TrajectoryCondition, events: &[TrajectoryEvent]) -> Vec<usize> {
    match condition {
        TrajectoryCondition::EventObserved { .. }
        | TrajectoryCondition::FeatureCondition { .. } => events
            .iter()
            .enumerate()
            .filter_map(|(index, event)| direct_match(condition, event).then_some(index))
            .collect(),
        TrajectoryCondition::Before { first, second } => {
            let first = match_positions(first, events);
            match_positions(second, events)
                .into_iter()
                .filter(|second| first.iter().any(|first| first < second))
                .collect()
        }
        TrajectoryCondition::After { first, second } => {
            let second = match_positions(second, events);
            match_positions(first, events)
                .into_iter()
                .filter(|first| second.iter().any(|second| second < first))
                .collect()
        }
        TrajectoryCondition::Repeated { condition, minimum } => {
            let positions = match_positions(condition, events);
            if positions.len() >= *minimum {
                positions
            } else {
                Vec::new()
            }
        }
        TrajectoryCondition::Within {
            condition,
            duration,
        } => {
            let Some(last) = events.last() else {
                return Vec::new();
            };
            match_positions(condition, events)
                .into_iter()
                .filter(|position| {
                    last.timestamp
                        .signed_duration_since(events[*position].timestamp)
                        .to_std()
                        .is_ok_and(|age| age <= *duration)
                })
                .collect()
        }
    }
}

pub fn windowed_events(
    events: &[TrajectoryEvent],
    window: &TrajectoryWindow,
) -> Vec<TrajectoryEvent> {
    let mut result: Vec<_> = events
        .iter()
        .rev()
        .take(window.max_events)
        .cloned()
        .collect();
    result.reverse();
    if let (Some(duration), Some(last)) = (window.max_duration, result.last()) {
        let end = last.timestamp;
        result.retain(|event| {
            end.signed_duration_since(event.timestamp)
                .to_std()
                .is_ok_and(|age| age <= duration)
        });
    }
    result
}

fn condition_label(condition: &TrajectoryCondition) -> String {
    serde_json::to_string(condition).unwrap_or_else(|_| "condition".into())
}

pub fn signature_matches(
    signature: &EarlyWarningSignature,
    events: &[TrajectoryEvent],
) -> EarlyWarningMatch {
    let mut cursor = 0_usize;
    let mut matched = Vec::new();
    let mut unmatched = Vec::new();
    for condition in &signature.ordered_conditions {
        let position = match_positions(condition, events)
            .into_iter()
            .find(|position| *position >= cursor);
        if let Some(position) = position {
            cursor = position.saturating_add(1);
            matched.push(condition_label(condition));
        } else {
            unmatched.push(condition_label(condition));
        }
    }
    EarlyWarningMatch {
        signature: signature.id.clone(),
        matched_conditions: matched,
        unmatched_conditions: unmatched,
    }
}

impl EarlyWarningMatcher for DeterministicEarlyWarningMatcher {
    fn match_active(
        &self,
        trajectory: &ExecutionTrajectory,
        events: &[TrajectoryEvent],
        signatures: &[EarlyWarningSignature],
        window: &TrajectoryWindow,
    ) -> Vec<EarlyWarningMatch> {
        let events = windowed_events(events, window);
        let mut result = signatures
            .iter()
            .filter(|signature| {
                ((signature.status == RiskIndicatorStatus::Validated
                    && signature.origin == PredictiveOrigin::Local)
                    || signature.origin == PredictiveOrigin::FederatedAdvisory)
                    && scope_compatible(&signature.scope, &trajectory.context)
                    && signature
                        .required_runtime_version
                        .as_ref()
                        .is_none_or(|version| {
                            trajectory.context.runtime_version.as_ref() == Some(version)
                        })
            })
            .map(|signature| signature_matches(signature, &events))
            .filter(|matched| !matched.matched_conditions.is_empty())
            .collect::<Vec<_>>();
        result.sort_by_key(|item| item.signature.clone());
        result
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicForecastEngine;

impl FailureForecastEngine for DeterministicForecastEngine {
    fn forecast(
        &self,
        trajectory: &ExecutionTrajectory,
        context: &ForecastContext,
    ) -> Result<Vec<FailureForecast>> {
        if trajectory.ended_at.is_some() {
            return Err(Error::InvalidInput(
                "Cannot create a live forecast for an ended trajectory".into(),
            ));
        }
        if context
            .events
            .iter()
            .any(|event| event.kind == TrajectoryEventKind::FailureObserved)
        {
            return Err(Error::InvalidInput(
                "A forecast must be emitted before the observed failure".into(),
            ));
        }
        let matches = DeterministicEarlyWarningMatcher.match_active(
            trajectory,
            &context.events,
            &context.signatures,
            &context.window,
        );
        let historical =
            DeterministicTrajectoryMatcher.match_trajectory(trajectory, &context.history);
        let supported: BTreeSet<_> = context.supported_causal_hypotheses.iter().collect();
        let mut result = Vec::new();
        for warning in matches {
            let signature = context
                .signatures
                .iter()
                .find(|signature| signature.id == warning.signature)
                .ok_or_else(|| Error::InvalidInput("Compiled signature disappeared".into()))?;
            let causal_basis: Vec<_> = signature
                .causal_basis
                .iter()
                .filter(|id| supported.contains(id))
                .cloned()
                .collect();
            let has_causal_basis = !causal_basis.is_empty();
            let pattern_match = signature
                .failure_trajectory
                .as_ref()
                .and_then(|id| {
                    context
                        .failure_trajectories
                        .iter()
                        .find(|pattern| &pattern.id == id)
                })
                .map(|pattern| DeterministicTrajectoryMatcher.match_pattern(trajectory, pattern));
            let history: Vec<_> = historical
                .iter()
                .filter(|item| {
                    matches!(&item.outcome, TrajectoryOutcome::Failure(f) if f == &signature.failure)
                        && item.matched_events >= signature.ordered_conditions.len()
                })
                .map(|item| item.trajectory.clone())
                .collect();
            let indicators: Vec<_> = context
                .indicators
                .iter()
                .filter(|indicator| {
                    matches!(
                        indicator.status,
                        RiskIndicatorStatus::Supported | RiskIndicatorStatus::Validated
                    ) && indicator.origin == PredictiveOrigin::Local
                        && indicator.associated_failures.contains(&signature.failure)
                        && indicator.condition.conditions.iter().all(|condition| {
                            !match_positions(condition, &context.events).is_empty()
                        })
                })
                .map(|indicator| indicator.id.clone())
                .collect();
            let evidence_kind = match (history.is_empty(), causal_basis.is_empty()) {
                (false, false) => ForecastEvidenceKind::Mixed,
                (true, false) => ForecastEvidenceKind::Causal,
                _ => ForecastEvidenceKind::Correlational,
            };
            let matched_count = warning.matched_conditions.len();
            let total_count = matched_count + warning.unmatched_conditions.len();
            let fallback_match = FailureTrajectoryMatch {
                failure_trajectory: signature
                    .failure_trajectory
                    .clone()
                    .unwrap_or_else(FailureTrajectoryId::new),
                matched_steps: matched_count,
                total_steps: total_count,
                matched: (0..matched_count).collect(),
                missing: (matched_count..total_count).collect(),
                contradicted: Vec::new(),
                status: if matched_count == total_count {
                    TrajectoryMatchStatus::Strong
                } else if matched_count * 2 >= total_count {
                    TrajectoryMatchStatus::Partial
                } else {
                    TrajectoryMatchStatus::Weak
                },
                last_match_index: context.events.last().map(|event| event.sequence),
            };
            let match_result = pattern_match.as_ref().unwrap_or(&fallback_match);
            let mut status = DeterministicForecastPolicy.classify(
                match_result,
                causal_basis.first(),
                context.envelope_proximity,
                context.risk,
                &context.policy,
            );
            let advisory = signature.origin == PredictiveOrigin::FederatedAdvisory;
            if advisory && status > ForecastStatus::Elevated {
                status = ForecastStatus::Elevated;
            }
            if status == ForecastStatus::Cleared {
                continue;
            }
            let strength = if !causal_basis.is_empty() && history.len() >= 2 {
                ForecastStrength::Strong
            } else if !causal_basis.is_empty()
                || history.len() >= 2
                || signature.evidence.len() >= 4
            {
                ForecastStrength::Moderate
            } else if history.len() == 1 {
                ForecastStrength::Weak
            } else {
                ForecastStrength::InsufficientEvidence
            };
            if strength == ForecastStrength::InsufficientEvidence && !advisory {
                continue;
            }
            let recommended_interventions = if status.is_actionable() && !advisory {
                context
                    .interventions
                    .iter()
                    .filter(|intervention| {
                        intervention.signature == signature.id
                            && intervention.status == PreventiveInterventionStatus::Validated
                            && intervention.origin == PredictiveOrigin::Local
                    })
                    .map(|intervention| intervention.id.clone())
                    .collect()
            } else {
                Vec::new()
            };
            result.push(FailureForecast {
                id: FailureForecastId::new(),
                trajectory_id: trajectory.id.clone(),
                failure: signature.failure.clone(),
                matched_trajectory: signature.failure_trajectory.clone(),
                horizon: signature.horizon.clone(),
                evidence: signature.evidence.clone(),
                indicators,
                matched_signals: trajectory
                    .points
                    .iter()
                    .flat_map(|point| point.derived_signals.iter().map(|signal| signal.id.clone()))
                    .collect(),
                missing_signals: warning
                    .unmatched_conditions
                    .iter()
                    .map(|description| ExpectedSignal {
                        description: description.clone(),
                    })
                    .collect(),
                recommended_interventions,
                signature: signature.id.clone(),
                causal_basis,
                evidence_kind,
                strength,
                status,
                historical_matches: history,
                warning_sequence: match_result.last_match_index.unwrap_or(0),
                lead: ForecastLead::default(),
                matcher_version: "deterministic-trajectory-matcher-v1".into(),
                policy_version: context.policy.version.clone(),
                warning_revision: signature.revision,
                causal_model_revision: has_causal_basis.then_some(1),
                advisory,
                created_at: Utc::now(),
            });
        }
        result.sort_by_key(|forecast| {
            (
                std::cmp::Reverse(forecast.strength),
                forecast.failure.signature.clone(),
            )
        });
        Ok(result)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicFailureForecaster;

impl FailureForecaster for DeterministicFailureForecaster {
    fn forecast(
        &self,
        trajectory: &ExecutionTrajectory,
        known_failures: &[FailureTrajectory],
        context: &ForecastContext,
    ) -> Result<Vec<FailureForecast>> {
        let mut context = context.clone();
        context.failure_trajectories = known_failures.to_vec();
        DeterministicForecastEngine.forecast(trajectory, &context)
    }
}

fn salient(feature: &str, value: &TrajectoryValue) -> String {
    if feature.ends_with("_ms")
        && let TrajectoryValue::Integer(value) = value
    {
        return match value {
            ..=0 => "0".into(),
            1..=999 => "1-999".into(),
            1000..=4999 => "1000-4999".into(),
            _ => "5000+".into(),
        };
    }
    value.normalized()
}

pub fn fingerprint(events: &[TrajectoryEvent]) -> Result<TrajectoryFingerprint> {
    let event_kinds: Vec<_> = events.iter().map(|event| event.kind.clone()).collect();
    let mut salient_features = BTreeMap::new();
    for event in events {
        for (feature, value) in &event.observation.features {
            salient_features.insert(feature.clone(), salient(feature, value));
        }
    }
    let hash = blake3::hash(&serde_json::to_vec(&(&event_kinds, &salient_features))?)
        .to_hex()
        .to_string();
    Ok(TrajectoryFingerprint {
        hash,
        event_kinds,
        salient_features,
    })
}
