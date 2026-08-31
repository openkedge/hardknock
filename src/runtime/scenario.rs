// SPDX-License-Identifier: Apache-2.0

use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    Error, Result,
    bridge::protocol::NormalizedAction,
    capability::IsolationLevel,
    core::{AgentIdentity, HardknockSessionId},
    experience::{EnvironmentContext, RepositoryContext},
    retrieval::QueryContext,
};

use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeScenarioContext {
    pub repository: PathBuf,
    pub commit: String,
    pub os: String,
    pub arch: String,
    pub markers: Vec<String>,
    pub tags: Vec<String>,
    pub facts: BTreeMap<String, String>,
}

impl Default for RuntimeScenarioContext {
    fn default() -> Self {
        Self {
            repository: PathBuf::from("/hardknock/runtime-scenario"),
            commit: "scenario-state".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            markers: Vec::new(),
            tags: Vec::new(),
            facts: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeScenario {
    pub schema: String,
    pub name: String,
    pub task: TaskDescriptor,
    pub context: RuntimeScenarioContext,
    pub proposed_action: Option<NormalizedAction>,
    pub proposed_effect: Option<crate::effects::EffectRequest>,
    pub knowledge: KnowledgeSignals,
    pub assurance: AssuranceContext,
    pub envelope: Option<OperatingEnvelopeRef>,
    pub capability: CapabilityContext,
    pub risk: RuntimeRiskAssessment,
    pub uncertainty: RuntimeUncertainty,
    pub recoveries: Vec<RecoveryRef>,
    pub experiments: ExperimentCapabilitySummary,
    pub skills: Vec<SkillRef>,
    pub reflexes: Vec<ReflexRef>,
    pub failure_signature: Option<FailureSignatureRef>,
    pub known_unknowns: Vec<String>,
    pub externally_supported: bool,
    pub tool_candidates: Vec<ToolCandidate>,
    pub expected_decision: Option<RuntimeDecisionKind>,
    pub expected_outcome: Option<DecisionOutcome>,
    pub uncontrolled_success: Option<bool>,
    pub controlled_success: Option<bool>,
    pub recovery_latency_ms: Option<u64>,
    pub baseline_recovery_latency_ms: Option<u64>,
}

impl Default for RuntimeScenario {
    fn default() -> Self {
        Self {
            schema: super::RUNTIME_SCENARIO_SCHEMA.into(),
            name: "unnamed".into(),
            task: TaskDescriptor {
                description: "runtime scenario".into(),
                family: None,
                tags: Vec::new(),
            },
            context: Default::default(),
            proposed_action: None,
            proposed_effect: None,
            knowledge: KnowledgeSignals {
                context_in_scope: true,
                ..Default::default()
            },
            assurance: Default::default(),
            envelope: None,
            capability: CapabilityContext {
                required_available: true,
                commit_authority: true,
                effect_adapter_available: true,
                isolation_sufficient: true,
                isolation_level: IsolationLevel::Container,
                ..Default::default()
            },
            risk: Default::default(),
            uncertainty: Default::default(),
            recoveries: Vec::new(),
            experiments: Default::default(),
            skills: Vec::new(),
            reflexes: Vec::new(),
            failure_signature: None,
            known_unknowns: Vec::new(),
            externally_supported: false,
            tool_candidates: Vec::new(),
            expected_decision: None,
            expected_outcome: None,
            uncontrolled_success: None,
            controlled_success: None,
            recovery_latency_ms: None,
            baseline_recovery_latency_ms: None,
        }
    }
}

impl RuntimeScenario {
    pub fn validate(&self) -> Result<()> {
        if self.schema != super::RUNTIME_SCENARIO_SCHEMA {
            return Err(Error::InvalidInput(format!(
                "Unsupported runtime scenario schema {}; expected {}",
                self.schema,
                super::RUNTIME_SCENARIO_SCHEMA
            )));
        }
        if self.name.trim().is_empty()
            || self.name.len() > 160
            || self.task.description.trim().is_empty()
            || self.task.description.len() > 4096
            || self.context.facts.len() > 128
        {
            return Err(Error::InvalidInput(
                "Runtime scenario name, task, or context exceeds its bound".into(),
            ));
        }
        if let Some(effect) = &self.proposed_effect {
            effect.validate()?;
        }
        Ok(())
    }

    pub fn decision_context(&self) -> Result<RuntimeDecisionContext> {
        self.validate()?;
        let environment = EnvironmentContext {
            os: self.context.os.clone(),
            arch: self.context.arch.clone(),
            cwd: self.context.repository.clone(),
            mode: crate::core::EnvironmentMode::Controlled,
            facts: self.context.facts.clone(),
            fingerprint: blake3::hash(&serde_json::to_vec(&(
                &self.context.os,
                &self.context.arch,
                &self.context.facts,
            ))?)
            .to_hex()
            .to_string(),
        };
        let query_context = QueryContext {
            repository: RepositoryContext {
                path: self.context.repository.clone(),
                name: self
                    .context
                    .repository
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("runtime-scenario")
                    .into(),
                commit: self.context.commit.clone(),
                branch: None,
            },
            environment,
            detected_markers: self.context.markers.clone(),
            task: self.task.description.clone(),
            proposed_actions: normalized_patterns(self.proposed_action.as_ref()),
            tags: self.context.tags.clone(),
        };
        Ok(RuntimeDecisionContext {
            session_id: HardknockSessionId::new(),
            agent: AgentIdentity {
                kind: "deterministic-scenario".into(),
                executable: "hardknock".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
                model: None,
            },
            task: self.task.clone(),
            query_context,
            proposed_action: self.proposed_action.clone(),
            proposed_effect: self.proposed_effect.clone(),
            relevant_experience: Default::default(),
            assurance: self.assurance.clone(),
            operating_envelope: self.envelope.clone(),
            capability_context: self.capability.clone(),
            risk: self.risk.clone(),
            uncertainty: self.uncertainty.clone(),
            available_recovery: self.recoveries.clone(),
            available_experiments: self.experiments.clone(),
            knowledge_signals: self.knowledge.clone(),
            applicable_skills: self.skills.clone(),
            matched_reflexes: self.reflexes.clone(),
            advisory_reflexes: Vec::new(),
            failure_signature: self.failure_signature.clone(),
            known_unknowns: self.known_unknowns.clone(),
            externally_supported: self.externally_supported,
            tool_candidates: self.tool_candidates.clone(),
        })
    }

    pub fn evaluate(&self, profile: RuntimePolicyProfile) -> Result<RuntimeDecisionEvaluation> {
        let context = self.decision_context()?;
        DeterministicRuntimeController::with_config(RuntimePolicyConfig {
            profile,
            experiment_mode: self.experiments.mode,
            ..Default::default()
        })?
        .evaluate(&context)
    }
}

fn normalized_patterns(action: Option<&NormalizedAction>) -> Vec<crate::lesson::ActionPattern> {
    match action {
        Some(NormalizedAction::Shell { command, .. }) => {
            vec![crate::lesson::ActionPattern::shell(command)]
        }
        Some(NormalizedAction::FileWrite { path })
        | Some(NormalizedAction::FileDelete { path }) => {
            vec![crate::lesson::ActionPattern::FileOperation {
                pattern: path.clone(),
            }]
        }
        Some(action) => vec![crate::lesson::ActionPattern::Custom {
            kind: "normalized_action".into(),
            value: serde_json::to_string(action).unwrap_or_else(|_| "unavailable".into()),
        }],
        None => Vec::new(),
    }
}
