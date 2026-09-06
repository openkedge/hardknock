// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::runtime::RuntimeAutonomy;

pub trait PreventiveInterventionPolicy {
    fn select(
        &self,
        forecast: &FailureForecast,
        candidates: &[PreventiveIntervention],
        context: &crate::runtime::RuntimeDecisionContext,
    ) -> crate::Result<InterventionSelection>;

    fn decide(
        &self,
        forecast: &FailureForecast,
        context: &PreventivePolicyContext,
    ) -> PreventiveDecision;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicPreventivePolicy;

fn action_rank(action: &InterventionAction) -> u8 {
    match action {
        InterventionAction::Observe => 0,
        InterventionAction::Warn(_) => 1,
        InterventionAction::RefreshAuthoritativeState | InterventionAction::RefreshCredential => 2,
        InterventionAction::Replan | InterventionAction::ReprepareEffect => 3,
        InterventionAction::ReconcileEffect
        | InterventionAction::SwitchTool(_)
        | InterventionAction::ReduceCapability
        | InterventionAction::DelayAction
        | InterventionAction::AbortPreparedEffect
        | InterventionAction::ApplyRecoveryEarly(_)
        | InterventionAction::RunExperiment(_) => 4,
        InterventionAction::Custom(_) => 5,
    }
}

impl PreventiveInterventionPolicy for DeterministicPreventivePolicy {
    fn select(
        &self,
        forecast: &FailureForecast,
        candidates: &[PreventiveIntervention],
        context: &crate::runtime::RuntimeDecisionContext,
    ) -> crate::Result<InterventionSelection> {
        let decision = self.decide(
            forecast,
            &PreventivePolicyContext {
                runtime: context.clone(),
                failure_severity: context.risk.severity,
                available_interventions: candidates.to_vec(),
                false_positive_rate: None,
                adequate_evidence_diversity: true,
            },
        );
        let selected = match &decision {
            PreventiveDecision::Intervene(id) | PreventiveDecision::RequireApproval(id) => {
                Some(id.clone())
            }
            _ => None,
        };
        Ok(InterventionSelection {
            selected,
            considered: candidates.iter().map(|item| item.id.clone()).collect(),
            reasons: vec![format!(
                "transparent least-disruption decision: {decision:?}"
            )],
            decision,
        })
    }

    fn decide(
        &self,
        forecast: &FailureForecast,
        context: &PreventivePolicyContext,
    ) -> PreventiveDecision {
        if !forecast.status.is_actionable()
            || forecast.strength == ForecastStrength::InsufficientEvidence
        {
            return PreventiveDecision::Observe;
        }
        let mut candidates: Vec<_> = context
            .available_interventions
            .iter()
            .filter(|intervention| {
                intervention.status == PreventiveInterventionStatus::Validated
                    && intervention.origin == PredictiveOrigin::Local
                    && intervention.signature == forecast.signature
                    && intervention.target_failure == forecast.failure
            })
            .collect();
        candidates.sort_by_key(|item| {
            (
                item.disruption,
                item.cost,
                action_rank(&item.action),
                item.id.clone(),
            )
        });
        let Some(intervention) = candidates.first() else {
            return if forecast.strength >= ForecastStrength::Moderate {
                PreventiveDecision::Experiment
            } else {
                PreventiveDecision::Observe
            };
        };
        if intervention.window.as_ref().is_some_and(|window| {
            window.opens_at.0 == forecast.trajectory_id
                && window
                    .closes_at
                    .as_ref()
                    .is_some_and(|(_, closes)| forecast.warning_sequence > *closes)
        }) {
            return PreventiveDecision::Warn("Intervention window has already closed".into());
        }
        if intervention.requires_commit_authority
            && !context.runtime.capability_context.commit_authority
        {
            return PreventiveDecision::RequireApproval(intervention.id.clone());
        }
        if context.false_positive_rate.is_some_and(|rate| rate > 0.25) {
            return PreventiveDecision::Warn(
                "Warning history has excessive false positives; revalidate before intervention"
                    .into(),
            );
        }
        if context.failure_severity <= crate::curriculum::Severity::Low
            && intervention.cost >= InterventionCost::High
        {
            return PreventiveDecision::Warn(
                "Forecasted failure is low severity and the validated action is disruptive".into(),
            );
        }
        if context.failure_severity >= crate::curriculum::Severity::High
            && !context.adequate_evidence_diversity
        {
            return PreventiveDecision::RequireApproval(intervention.id.clone());
        }
        match context
            .runtime
            .capability_context
            .governance
            .hard_policy_blocked
        {
            true => PreventiveDecision::Abstain,
            false => match context.runtime_policy_mode() {
                RuntimeAutonomy::Observe => PreventiveDecision::Observe,
                RuntimeAutonomy::Advise => PreventiveDecision::Warn(format!(
                    "Validated preventive action available: {:?}",
                    intervention.action
                )),
                RuntimeAutonomy::Adaptive | RuntimeAutonomy::Governed => {
                    if intervention.cost <= InterventionCost::Low
                        && intervention.reversibility
                            == crate::effects::ReversibilityClass::NaturallyReversible
                        && !intervention.requires_commit_authority
                    {
                        PreventiveDecision::Intervene(intervention.id.clone())
                    } else {
                        PreventiveDecision::Warn(format!(
                            "Preventive action requires explicit execution: {:?}",
                            intervention.action
                        ))
                    }
                }
            },
        }
    }
}

impl PreventivePolicyContext {
    fn runtime_policy_mode(&self) -> RuntimeAutonomy {
        // The runtime context is policy-neutral. Callers set this marker through the
        // explicit task tags rather than allowing the forecast engine to choose control mode.
        if self
            .runtime
            .task
            .tags
            .iter()
            .any(|tag| tag == "runtime-mode:observe")
        {
            RuntimeAutonomy::Observe
        } else if self
            .runtime
            .task
            .tags
            .iter()
            .any(|tag| tag == "runtime-mode:adaptive")
        {
            RuntimeAutonomy::Adaptive
        } else if self
            .runtime
            .task
            .tags
            .iter()
            .any(|tag| tag == "runtime-mode:governed")
        {
            RuntimeAutonomy::Governed
        } else {
            RuntimeAutonomy::Advise
        }
    }
}
