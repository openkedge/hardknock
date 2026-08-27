// SPDX-License-Identifier: Apache-2.0

use crate::{
    Error, Result,
    core::{AgentIdentity, ExperienceId, ExperimentId, HypothesisId, LessonId, TrialId},
    experience::ExperienceContext,
    experiment::{
        Experiment, ExperimentConclusion, ExperimentConclusionPolicy, ExperimentStatus,
        PairedComparison,
    },
    reflection::CandidateHypothesis,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct ConfidenceScore(f64);

impl TryFrom<f64> for ConfidenceScore {
    type Error = Error;
    fn try_from(value: f64) -> Result<Self> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(Error::InvalidInput(
                "Confidence must be finite and between 0 and 1".into(),
            ))
        }
    }
}
impl From<ConfidenceScore> for f64 {
    fn from(value: ConfidenceScore) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActionPattern {
    ShellCommand { pattern: String },
    FileOperation { pattern: String },
    Custom { kind: String, value: String },
}

impl ActionPattern {
    pub fn shell(script: &str) -> Self {
        Self::ShellCommand {
            pattern: script.trim().into(),
        }
    }
    /// Exact full-script matching after trimming outer whitespace only.
    pub fn matches_shell(&self, script: &str) -> bool {
        self.shell_script()
            .is_some_and(|p| p.trim() == script.trim())
    }
    pub fn shell_script(&self) -> Option<&str> {
        match self {
            Self::ShellCommand { pattern } => Some(pattern),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSelector {
    pub repository: Option<PathBuf>,
    pub required_markers: Vec<String>,
    pub tags: Vec<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
}
impl ContextSelector {
    pub fn from_context(context: &ExperienceContext) -> Self {
        Self {
            repository: Some(context.repository.path.clone()),
            required_markers: context.markers.clone(),
            tags: vec![],
            os: Some(context.environment.os.clone()),
            arch: Some(context.environment.arch.clone()),
        }
    }
    pub fn matches(&self, context: &ExperienceContext) -> bool {
        self.repository
            .as_ref()
            .is_none_or(|p| *p == context.repository.path)
            && self
                .required_markers
                .iter()
                .all(|m| context.markers.contains(m))
            && self.tags.iter().all(|t| context.tags.contains(t))
            && self
                .os
                .as_ref()
                .is_none_or(|s| *s == context.environment.os)
            && self
                .arch
                .as_ref()
                .is_none_or(|s| *s == context.environment.arch)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LessonStatus {
    Candidate,
    CounterfactuallySupported,
    Validated,
    Contradicted,
    Retired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceRelationship {
    Supports,
    Contradicts,
    Origin,
    Inconclusive,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceRef {
    Experience {
        experience_id: ExperienceId,
        relationship: EvidenceRelationship,
    },
    Trial {
        experiment_id: ExperimentId,
        trial_id: TrialId,
        relationship: EvidenceRelationship,
    },
}

pub trait ConfidencePolicy {
    fn initial(&self) -> ConfidenceScore;
    fn update(&self, lesson: &Lesson, conclusion: ExperimentConclusion) -> ConfidenceScore;
}
/// Transparent evidence indicators, not statistically calibrated probabilities.
pub struct HeuristicConfidence;
impl ConfidencePolicy for HeuristicConfidence {
    fn initial(&self) -> ConfidenceScore {
        ConfidenceScore(0.42)
    }
    fn update(&self, lesson: &Lesson, conclusion: ExperimentConclusion) -> ConfidenceScore {
        match conclusion {
            ExperimentConclusion::ContradictsHypothesis => ConfidenceScore(0.20),
            ExperimentConclusion::SupportsHypothesis
                if lesson.status != LessonStatus::Contradicted =>
            {
                ConfidenceScore(0.78)
            }
            _ => lesson.confidence,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lesson {
    pub id: LessonId,
    pub version: u32,
    pub source_experience: ExperienceId,
    pub hypothesis_id: HypothesisId,
    pub status: LessonStatus,
    pub context_match: ContextSelector,
    pub claim: String,
    pub rationale: String,
    pub avoid: Option<ActionPattern>,
    pub prefer: Option<ActionPattern>,
    pub confidence: ConfidenceScore,
    pub evidence: Vec<EvidenceRef>,
    pub discovered_by: Vec<AgentIdentity>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Lesson {
    pub fn candidate(h: &CandidateHypothesis, policy: &dyn ConfidencePolicy) -> Self {
        let now = Utc::now();
        Self {
            id: LessonId::new(),
            version: 1,
            source_experience: h.source_experience.clone(),
            hypothesis_id: h.id.clone(),
            status: LessonStatus::Candidate,
            context_match: h.context_match.clone(),
            claim: h.claim.clone(),
            rationale: h.rationale.clone(),
            avoid: Some(h.avoid.clone()),
            prefer: Some(h.prefer.clone()),
            confidence: policy.initial(),
            evidence: vec![EvidenceRef::Experience {
                experience_id: h.source_experience.clone(),
                relationship: EvidenceRelationship::Origin,
            }],
            discovered_by: vec![h.generated_by.clone()],
            created_at: now,
            updated_at: now,
        }
    }

    pub fn apply_experiment(
        &mut self,
        experiment: &Experiment,
        policy: &dyn ConfidencePolicy,
    ) -> Result<()> {
        if experiment.lesson_id != self.id
            || experiment.source_experience != self.source_experience
            || experiment.hypothesis_id != self.hypothesis_id
        {
            return Err(Error::InvalidInput(
                "Experiment provenance does not match this Lesson".into(),
            ));
        }
        if experiment.status != ExperimentStatus::Completed
            || matches!(self.status, LessonStatus::Retired | LessonStatus::Validated)
        {
            return Err(Error::Intervention(
                "Only completed experiments may update active V0.1 Lessons".into(),
            ));
        }
        if experiment.trials.len() != 2
            || PairedComparison.conclude(&experiment.trials) != experiment.conclusion
        {
            return Err(Error::InvalidInput(
                "Experiment conclusion does not match its paired trial evidence".into(),
            ));
        }
        if experiment.plan.trials.len() != 2
            || !self
                .avoid
                .as_ref()
                .is_some_and(|a| a.matches_shell(&experiment.trials[0].spec.command))
            || !self
                .prefer
                .as_ref()
                .is_some_and(|a| a.matches_shell(&experiment.trials[1].spec.command))
            || experiment
                .trials
                .iter()
                .zip(&experiment.plan.trials)
                .any(|(t, s)| {
                    t.spec != *s
                        || t.starting_state != experiment.starting_state
                        || t.environment_fingerprint != experiment.plan.environment_fingerprint
                })
        {
            return Err(Error::InvalidInput(
                "Trial actions/state do not test the current Lesson and plan".into(),
            ));
        }
        if self.evidence.iter().any(|e| matches!(e, EvidenceRef::Trial { experiment_id, .. } if *experiment_id == experiment.id)) {
            return Err(Error::InvalidInput("Experiment evidence already applied".into()));
        }
        let relationship = match experiment.conclusion {
            ExperimentConclusion::SupportsHypothesis => EvidenceRelationship::Supports,
            ExperimentConclusion::ContradictsHypothesis => EvidenceRelationship::Contradicts,
            ExperimentConclusion::Inconclusive => EvidenceRelationship::Inconclusive,
        };
        let next_version = self
            .version
            .checked_add(1)
            .ok_or_else(|| Error::InvalidInput("Lesson version overflow".into()))?;
        self.confidence = policy.update(self, experiment.conclusion);
        match experiment.conclusion {
            ExperimentConclusion::SupportsHypothesis if self.status == LessonStatus::Candidate => {
                self.status = LessonStatus::CounterfactuallySupported
            }
            ExperimentConclusion::ContradictsHypothesis => self.status = LessonStatus::Contradicted,
            _ => {}
        }
        self.evidence
            .extend(experiment.trials.iter().map(|t| EvidenceRef::Trial {
                experiment_id: experiment.id.clone(),
                trial_id: t.spec.id.clone(),
                relationship,
            }));
        self.version = next_version;
        self.updated_at = Utc::now();
        Ok(())
    }
}
