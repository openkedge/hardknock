// SPDX-License-Identifier: Apache-2.0

use crate::{
    Error, Result,
    core::{AgentIdentity, ExperienceId, HypothesisId},
    experience::{Experience, Outcome},
    lesson::{ActionPattern, ContextSelector},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CandidateHypothesis {
    pub id: HypothesisId,
    pub source_experience: ExperienceId,
    pub created_at: DateTime<Utc>,
    pub claim: String,
    pub rationale: String,
    pub context_match: ContextSelector,
    pub avoid: ActionPattern,
    pub prefer: ActionPattern,
    pub generated_by: AgentIdentity,
}

pub trait ReflectionProvider {
    fn reflect(&self, experience: &Experience) -> Result<Vec<CandidateHypothesis>>;
}

pub struct ManualReflection {
    pub claim: String,
    pub avoid: String,
    pub prefer: String,
}
impl ReflectionProvider for ManualReflection {
    fn reflect(&self, experience: &Experience) -> Result<Vec<CandidateHypothesis>> {
        if [&self.claim, &self.avoid, &self.prefer]
            .iter()
            .any(|s| s.trim().is_empty() || s.contains('\0'))
            || self.avoid.trim() == self.prefer.trim()
        {
            return Err(Error::InvalidInput(
                "A hypothesis needs a claim and distinct, nonempty avoid/prefer scripts".into(),
            ));
        }
        Ok(vec![CandidateHypothesis {
            id: HypothesisId::new(),
            source_experience: experience.id.clone(),
            created_at: Utc::now(),
            claim: self.claim.clone(),
            rationale: "Human-proposed candidate; causality has not been established".into(),
            context_match: ContextSelector::from_context(&experience.context),
            avoid: ActionPattern::shell(&self.avoid),
            prefer: ActionPattern::shell(&self.prefer),
            generated_by: AgentIdentity {
                kind: "manual-reflection".into(),
                executable: "hardknock".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
                model: None,
            },
        }])
    }
}

/// Deliberately fixture-specific. Output patterns alone never imply a universal rule.
pub struct DeterministicReflection;
impl ReflectionProvider for DeterministicReflection {
    fn reflect(&self, experience: &Experience) -> Result<Vec<CandidateHypothesis>> {
        if experience.agent.kind != "test-agent"
            || experience.outcome != Outcome::Failure
            || !experience
                .context
                .markers
                .iter()
                .any(|m| m == "pnpm-workspace.yaml")
            || !experience
                .failure_signatures
                .iter()
                .any(|s| s.signature == "package_manager_conflict")
            || !experience
                .replay
                .as_ref()
                .is_some_and(|r| r.script == "./agent-script.sh baseline")
        {
            return Ok(vec![]);
        }
        let mut hypotheses = ManualReflection { claim: "The simulated npm workflow may create conflicting lockfile state in this pnpm workspace".into(), avoid: "./agent-script.sh baseline".into(), prefer: "./agent-script.sh alternative".into() }.reflect(experience)?;
        for h in &mut hypotheses {
            h.rationale = "Rule fixture-pnpm-v1 observed package_manager_conflict; compare the two explicit fixture modes from the same snapshot".into();
            h.generated_by.kind = "deterministic-reflection".into();
        }
        Ok(hypotheses)
    }
}
