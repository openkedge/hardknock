// SPDX-License-Identifier: Apache-2.0

use crate::{
    core::{AgentIdentity, ApplicationId, ExperienceId, ExperimentId},
    lesson::{Lesson, LessonStatus},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApplicationEvidence {
    pub application_id: ApplicationId,
    pub experience_id: ExperienceId,
    pub agent: AgentIdentity,
    pub context_key: String,
    pub distinct: bool,
    pub observed: bool,
    pub success: bool,
    pub relevant: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LessonEvidenceSummary {
    pub controlled_supports: Vec<ExperimentId>,
    pub controlled_contradictions: Vec<ExperimentId>,
    pub applications: Vec<ApplicationEvidence>,
}
impl LessonEvidenceSummary {
    pub fn distinct_successes(&self) -> usize {
        self.applications
            .iter()
            .filter(|a| a.distinct && a.observed && a.success && a.relevant)
            .map(|a| &a.context_key)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LessonValidationDecision {
    pub policy: String,
    pub validated: bool,
    pub distinct_successful_contexts: usize,
    pub reason: String,
}
pub trait LessonValidationPolicy {
    fn evaluate(
        &self,
        lesson: &Lesson,
        evidence: &LessonEvidenceSummary,
    ) -> LessonValidationDecision;
}
pub struct DistinctApplicationValidation;
impl LessonValidationPolicy for DistinctApplicationValidation {
    fn evaluate(
        &self,
        lesson: &Lesson,
        evidence: &LessonEvidenceSummary,
    ) -> LessonValidationDecision {
        let count = evidence.distinct_successes();
        let reason = if !matches!(
            lesson.status,
            LessonStatus::CounterfactuallySupported | LessonStatus::Validated
        ) {
            "Lesson is not in an eligible supported state"
        } else if evidence.controlled_supports.is_empty() {
            "A completed supporting controlled comparison is required"
        } else if !evidence.controlled_contradictions.is_empty() {
            "Contradicting controlled evidence requires explicit revalidation policy"
        } else if count == 0 {
            "An observed successful application in a distinct repository tree is required"
        } else {
            "Controlled support and observed successful application in a distinct tree are present"
        };
        let validated = matches!(
            lesson.status,
            LessonStatus::CounterfactuallySupported | LessonStatus::Validated
        ) && !evidence.controlled_supports.is_empty()
            && evidence.controlled_contradictions.is_empty()
            && count > 0;
        LessonValidationDecision {
            policy: "distinct-application-v1".into(),
            validated,
            distinct_successful_contexts: count,
            reason: reason.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::HypothesisId,
        experiment::ExperimentConclusion,
        lesson::{ActionPattern, ConfidencePolicy, ContextSelector, HeuristicConfidence},
        reflection::CandidateHypothesis,
    };

    fn candidate() -> Lesson {
        Lesson::candidate(
            &CandidateHypothesis {
                id: HypothesisId::new(),
                source_experience: ExperienceId::new(),
                created_at: chrono::Utc::now(),
                claim: "scoped replacement".into(),
                rationale: "test".into(),
                context_match: ContextSelector {
                    repository: Some("/fixture".into()),
                    required_markers: vec![],
                    tags: vec![],
                    os: None,
                    arch: None,
                },
                avoid: ActionPattern::shell("false"),
                prefer: ActionPattern::shell("true"),
                generated_by: AgentIdentity {
                    kind: "test-agent".into(),
                    executable: "fixture".into(),
                    version: None,
                    model: None,
                },
            },
            &HeuristicConfidence,
        )
    }
    fn evidence() -> LessonEvidenceSummary {
        LessonEvidenceSummary {
            controlled_supports: vec![ExperimentId::new()],
            controlled_contradictions: vec![],
            applications: vec![ApplicationEvidence {
                application_id: ApplicationId::new(),
                experience_id: ExperienceId::new(),
                agent: candidate().discovered_by.remove(0),
                context_key: "tree-b:env".into(),
                distinct: true,
                observed: true,
                success: true,
                relevant: true,
            }],
        }
    }

    #[test]
    fn validation_requires_controlled_support_and_every_application_predicate() {
        let mut lesson = candidate();
        lesson.status = LessonStatus::CounterfactuallySupported;
        let policy = DistinctApplicationValidation;
        assert!(policy.evaluate(&lesson, &evidence()).validated);
        let mut missing_control = evidence();
        missing_control.controlled_supports.clear();
        assert!(!policy.evaluate(&lesson, &missing_control).validated);
        for field in ["distinct", "observed", "success", "relevant"] {
            let mut missing = evidence();
            let app = &mut missing.applications[0];
            match field {
                "distinct" => app.distinct = false,
                "observed" => app.observed = false,
                "success" => app.success = false,
                "relevant" => app.relevant = false,
                _ => unreachable!(),
            }
            assert!(!policy.evaluate(&lesson, &missing).validated, "{field}");
        }
    }

    #[test]
    fn validation_deduplicates_contexts_without_depending_on_agent_brand() {
        let mut summary = evidence();
        let mut duplicate = summary.applications[0].clone();
        duplicate.application_id = ApplicationId::new();
        duplicate.experience_id = ExperienceId::new();
        duplicate.agent.kind = "another-agent".into();
        summary.applications.push(duplicate);
        assert_eq!(summary.distinct_successes(), 1);
        summary.applications[1].context_key = "tree-c:env".into();
        assert_eq!(summary.distinct_successes(), 2);
    }

    #[test]
    fn negative_evidence_and_ineligible_states_prevent_validation() {
        let mut lesson = candidate();
        for status in [
            LessonStatus::Candidate,
            LessonStatus::Contradicted,
            LessonStatus::Retired,
        ] {
            lesson.status = status;
            assert!(
                !DistinctApplicationValidation
                    .evaluate(&lesson, &evidence())
                    .validated
            );
        }
        lesson.status = LessonStatus::Validated;
        let mut contradictory = evidence();
        contradictory
            .controlled_contradictions
            .push(ExperimentId::new());
        assert!(
            !DistinctApplicationValidation
                .evaluate(&lesson, &contradictory)
                .validated
        );
    }

    #[test]
    fn confidence_stays_bounded_and_contradiction_is_not_erased_by_support() {
        let mut lesson = candidate();
        assert_eq!(f64::from(HeuristicConfidence.initial()), 0.42);
        lesson.confidence =
            HeuristicConfidence.update(&lesson, ExperimentConclusion::SupportsHypothesis);
        lesson.status = LessonStatus::CounterfactuallySupported;
        assert_eq!(f64::from(HeuristicConfidence.transfer(&lesson, 0)), 0.78);
        assert_eq!(f64::from(HeuristicConfidence.transfer(&lesson, 1)), 0.90);
        assert_eq!(f64::from(HeuristicConfidence.transfer(&lesson, 200)), 0.94);
        lesson.confidence =
            HeuristicConfidence.update(&lesson, ExperimentConclusion::ContradictsHypothesis);
        lesson.status = LessonStatus::Contradicted;
        assert_eq!(f64::from(HeuristicConfidence.transfer(&lesson, 200)), 0.20);
        assert_eq!(
            f64::from(
                HeuristicConfidence.update(&lesson, ExperimentConclusion::SupportsHypothesis)
            ),
            0.20
        );
    }
}
