// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{Error, Result};
use std::{collections::BTreeMap, path::Path, sync::Arc};

/// Provider-specific effect contract. V0.8 adapters are deterministic and local, so the
/// interface is synchronous; a future remote adapter can place its own bounded runtime behind it.
pub trait EffectAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    fn schemes(&self) -> &'static [&'static str];
    fn capabilities(&self) -> EffectAdapterCapabilities;
    fn classify(&self, request: &EffectRequest) -> Result<EffectClassification>;
    fn observe(&self, effect: &Effect) -> Result<ExternalStateSnapshot>;
    fn prepare(&self, effect: &Effect) -> Result<PreparedEffect>;
    fn commit(&self, effect: &Effect, prepared: &PreparedEffect) -> Result<AdapterCommitOutcome>;
    fn discard(&self, effect: &Effect, prepared: &PreparedEffect) -> Result<()>;
    fn compensate(&self, effect: &Effect, receipt: &CommitReceipt) -> Result<CompensationReceipt>;
    fn reconcile(&self, effect: &Effect) -> Result<ReconciliationResult>;
}

pub struct EffectAdapterRegistry {
    adapters: BTreeMap<String, Arc<dyn EffectAdapter>>,
    schemes: BTreeMap<String, String>,
}
impl EffectAdapterRegistry {
    pub fn new() -> Self {
        Self {
            adapters: BTreeMap::new(),
            schemes: BTreeMap::new(),
        }
    }
    pub fn deterministic(home: &Path) -> Result<Self> {
        let mut registry = Self::new();
        registry.register(Arc::new(MockHttpEffectAdapter::new(home)?))?;
        registry.register(Arc::new(MockDatabaseEffectAdapter::new(home)?))?;
        registry.register(Arc::new(MockMessageEffectAdapter::new(home)?))?;
        registry.register(Arc::new(ShadowDeploymentEffectAdapter::new(home)?))?;
        if let Some(adapter) = PostgresEffectAdapter::from_home(home)? {
            registry.register(Arc::new(adapter))?;
        }
        Ok(registry)
    }
    pub fn register(&mut self, adapter: Arc<dyn EffectAdapter>) -> Result<()> {
        let name = adapter.name().to_owned();
        if self.adapters.contains_key(&name) {
            return Err(Error::InvalidInput(format!(
                "Effect adapter {name} is already registered"
            )));
        }
        for scheme in adapter.schemes() {
            if self
                .schemes
                .insert((*scheme).into(), name.clone())
                .is_some()
            {
                return Err(Error::InvalidInput(format!(
                    "Effect target scheme {scheme} has multiple adapters"
                )));
            }
        }
        self.adapters.insert(name, adapter);
        Ok(())
    }
    pub fn select_request(&self, request: &EffectRequest) -> Result<Arc<dyn EffectAdapter>> {
        let name = request.adapter.as_deref().map(str::to_owned).or_else(|| {
            request
                .target
                .scheme()
                .and_then(|scheme| self.schemes.get(scheme).cloned())
        });
        self.get(
            name.as_deref().ok_or_else(|| {
                Error::InvalidInput("No adapter supports this effect target".into())
            })?,
        )
    }
    pub fn select_effect(&self, effect: &Effect) -> Result<Arc<dyn EffectAdapter>> {
        self.get(&effect.adapter)
    }
    pub fn get(&self, name: &str) -> Result<Arc<dyn EffectAdapter>> {
        self.adapters
            .get(name)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("Effect adapter {name} not found")))
    }
    pub fn capabilities(&self) -> BTreeMap<String, EffectAdapterCapabilities> {
        self.adapters
            .iter()
            .map(|(name, adapter)| (name.clone(), adapter.capabilities()))
            .collect()
    }
}
impl Default for EffectAdapterRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub trait EffectPolicy: Send + Sync {
    fn evaluate(&self, effect: &Effect, context: &EffectContext) -> EffectDecision;
}

pub struct DefaultEffectPolicy;
impl EffectPolicy for DefaultEffectPolicy {
    fn evaluate(&self, effect: &Effect, context: &EffectContext) -> EffectDecision {
        let Some(classification) = &effect.classification else {
            return EffectDecision::Reject;
        };
        if !context.capabilities.propose {
            return EffectDecision::Reject;
        }
        if matches!(
            classification.externality,
            ExternalityClass::Financial | ExternalityClass::Unknown
        ) || classification.commit_strategy == CommitStrategy::Unsupported
        {
            return EffectDecision::Reject;
        }
        if classification.isolation_requirement == IsolationRequirement::Shadow {
            return EffectDecision::RequireShadow;
        }
        if classification.externality == ExternalityClass::HumanVisible
            || classification.risk >= EffectRisk::Medium
            || !context.capabilities.commit
            || context.is_agent
        {
            return EffectDecision::RequireApproval;
        }
        if classification.risk == EffectRisk::ReadOnly {
            EffectDecision::AllowDirect
        } else {
            EffectDecision::RequirePrepare
        }
    }
}

pub trait CommitGate: Send + Sync {
    fn evaluate(
        &self,
        effect: &Effect,
        prepared: &PreparedEffect,
        current_state: &ExternalStateSnapshot,
    ) -> CommitGateDecision;
}
pub struct DeterministicCommitGate;
impl CommitGate for DeterministicCommitGate {
    fn evaluate(
        &self,
        effect: &Effect,
        prepared: &PreparedEffect,
        current_state: &ExternalStateSnapshot,
    ) -> CommitGateDecision {
        if prepared.expired(chrono::Utc::now())
            || prepared.scope_hash != effect.scope_hash().unwrap_or_default()
            || current_state.fingerprint != prepared.before.fingerprint
            || current_state.version != prepared.before.version
        {
            CommitGateDecision::Reprepare
        } else if effect
            .classification
            .as_ref()
            .is_some_and(|classification| {
                classification.externality == ExternalityClass::Financial
                    || classification.commit_strategy == CommitStrategy::Unsupported
            })
        {
            CommitGateDecision::Reject
        } else {
            CommitGateDecision::Allow
        }
    }
}
