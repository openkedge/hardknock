// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{
    Result,
    hierarchy::{KnowledgeContext, ScopeValue},
    runtime::RuntimeDecisionContext,
};
pub trait KnowledgeContextBuilder {
    fn build(&self, runtime: &RuntimeDecisionContext) -> Result<KnowledgeContext>;
}
#[derive(Default)]
pub struct DefaultKnowledgeContextBuilder;
impl DefaultKnowledgeContextBuilder {
    pub fn trusted(&self, r: &RuntimeDecisionContext) -> TrustedKnowledgeContext {
        let mut observations = r.context_observations.clone();
        let mut observed = |key: &str, value: ScopeValue| {
            observations
                .entry(key.into())
                .or_default()
                .push(ContextValue {
                    value,
                    source: ContextValueSource::RuntimeObserved,
                });
        };
        if let Some(family) = &r.task.family {
            observed("task_family", ScopeValue::String(family.clone()));
        }
        observed("runtime", ScopeValue::String(r.agent.kind.clone()));
        observed(
            "os",
            ScopeValue::String(r.query_context.environment.os.clone()),
        );
        observed(
            "arch",
            ScopeValue::String(r.query_context.environment.arch.clone()),
        );
        if let Some(effect) = &r.proposed_effect {
            // A proposed effect is agent intent, not proof of idempotency or target state.
            observed(
                "effect_type",
                ScopeValue::String(
                    serde_json::to_value(effect.kind)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_default(),
                ),
            );
        }
        if let Some(action) = &r.proposed_action {
            let kind = match action {
                crate::bridge::protocol::NormalizedAction::Shell { .. } => "shell",
                crate::bridge::protocol::NormalizedAction::FileRead { .. } => "file_read",
                crate::bridge::protocol::NormalizedAction::FileWrite { .. } => "file_write",
                crate::bridge::protocol::NormalizedAction::FileDelete { .. } => "file_delete",
                crate::bridge::protocol::NormalizedAction::Network { method, .. } => {
                    if matches!(method.to_uppercase().as_str(), "GET" | "HEAD" | "OPTIONS") {
                        "http_read"
                    } else {
                        "http_mutation"
                    }
                }
                _ => "tool",
            };
            observed("action_type", ScopeValue::String(kind.into()));
        }
        let mut result = TrustedKnowledgeContext {
            context: KnowledgeContext::default(),
            sources: Default::default(),
            conflicts: vec![],
            unverified: vec![],
        };
        for (key, mut values) in observations {
            values.sort_by(|a, b| b.source.cmp(&a.source).then(a.value.cmp(&b.value)));
            values.dedup();
            let strongest = values.first().cloned();
            let effective = strongest.filter(|top| {
                !values
                    .iter()
                    .any(|v| v.source == top.source && v.value != top.value)
            });
            if values
                .iter()
                .any(|v| Some(&v.value) != values.first().map(|v| &v.value))
            {
                result.conflicts.push(ContextConflict {
                    key: key.clone(),
                    values: values.clone(),
                    effective: effective.clone(),
                });
            }
            if let Some(value) = effective {
                // Constraint-relaxing contexts require observation. User/agent/imported
                // statements remain recorded but cannot establish exception authority.
                if value.source >= ContextValueSource::AdapterObserved {
                    result.sources.insert(key.clone(), value.source);
                    result.context.values.insert(key, value.value);
                } else {
                    result.unverified.push(key);
                }
            } else {
                result.unverified.push(key);
            }
        }
        result
    }
}
impl KnowledgeContextBuilder for DefaultKnowledgeContextBuilder {
    fn build(&self, r: &RuntimeDecisionContext) -> Result<KnowledgeContext> {
        Ok(self.trusted(r).context)
    }
}
