// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{Result, lesson::ActionPattern};

pub trait ReflexMatcher {
    fn evaluate(&self, context: &ActionContext, reflexes: &[Reflex]) -> Result<Vec<ReflexMatch>>;
}
pub struct DeterministicReflexMatcher;
impl ReflexMatcher for DeterministicReflexMatcher {
    fn evaluate(&self, context: &ActionContext, reflexes: &[Reflex]) -> Result<Vec<ReflexMatch>> {
        let mut matches = Vec::new();
        for reflex in reflexes {
            if reflex.status != ReflexStatus::Active || reflex.response == ReflexResponse::Block {
                continue;
            }
            let trigger = &reflex.trigger;
            if trigger.context.matches(&context.context)
                && trigger.proposed_action == context.proposed_action
                && trigger
                    .repeated_failures
                    .is_none_or(|n| context.consecutive_failures >= n)
                && (!trigger.no_state_change || context.no_state_change)
                && (!trigger.config_changed || context.config_changed)
            {
                matches.push(ReflexMatch {
                    reflex_id: reflex.id.clone(),
                    reflex_version: reflex.version,
                    trigger: trigger.clone(),
                    response: reflex.response,
                    confidence: reflex.confidence,
                    source_lessons: reflex.source_lessons.clone(),
                    source_trial: reflex.source_trial.clone(),
                    observed: context.clone(),
                    test_only: false,
                });
            }
        }
        Ok(matches)
    }
}
pub fn fixture_action() -> ActionPattern {
    ActionPattern::shell("./operation.sh")
}
