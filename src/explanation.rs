// SPDX-License-Identifier: Apache-2.0

use crate::{
    Result,
    application::{ExperienceRelation, LessonApplication, RepeatedMistakeObservation},
    core::{AgentIdentity, ExperienceId, ExperimentId},
    experience::{Experience, Outcome},
    experiment::ExperimentConclusion,
    lesson::Lesson,
    store::Store,
};
use serde::Serialize;

#[derive(Serialize)]
pub struct SourceObservation {
    pub id: ExperienceId,
    pub outcome: Outcome,
    pub agent: AgentIdentity,
}
#[derive(Serialize)]
pub struct ExperimentEvidence {
    pub id: ExperimentId,
    pub conclusion: ExperimentConclusion,
    pub source_experience: ExperienceId,
}
#[derive(Serialize)]
pub struct ApplicationExplanation {
    pub application: LessonApplication,
    pub lesson_at_application: Lesson,
    pub current_lesson: Lesson,
    pub source: SourceObservation,
    pub experiments: Vec<ExperimentEvidence>,
}
#[derive(Serialize)]
pub struct Explanation {
    pub experience_id: ExperienceId,
    pub task: String,
    pub outcome: Outcome,
    pub applications: Vec<ApplicationExplanation>,
    pub agent: AgentIdentity,
    pub lineage: Vec<ExperienceRelation>,
    pub repeated_mistakes: Vec<RepeatedMistakeObservation>,
    pub resilience: Option<crate::resilience::ResilienceObservation>,
    pub reflexes: Vec<ReflexExplanation>,
}

#[derive(Serialize)]
pub struct ReflexExplanation {
    pub matched: crate::resilience::ReflexMatch,
    pub lessons: Vec<Lesson>,
    pub source_trial: crate::resilience::ChaosTrial,
    pub source_campaign: crate::core::ChaosCampaignId,
}

impl Store {
    pub fn explain(&self, id: Option<&ExperienceId>) -> Result<Explanation> {
        let experience: Experience = if let Some(id) = id {
            self.experience(id)?
        } else {
            self.latest_influenced_experience()?
        };
        let mut applications = Vec::new();
        for application in &experience.lesson_applications {
            let lesson = self.lesson_version(&application.lesson_id, application.lesson_version)?;
            let source = self.experience(&lesson.source_experience)?;
            let experiments = self
                .experiments()?
                .into_iter()
                .filter(|e| e.lesson_id == lesson.id)
                .map(|e| ExperimentEvidence {
                    id: e.id,
                    conclusion: e.conclusion,
                    source_experience: e.source_experience,
                })
                .collect();
            applications.push(ApplicationExplanation {
                application: application.clone(),
                current_lesson: self.lesson(&lesson.id)?,
                lesson_at_application: lesson,
                source: SourceObservation {
                    id: source.id,
                    outcome: source.outcome,
                    agent: source.agent,
                },
                experiments,
            });
        }
        let mut reflexes = Vec::new();
        if let Some(observation) = &experience.resilience {
            for matched in &observation.reflex_matches {
                let source_trial = self.chaos_trial(&matched.source_trial)?;
                reflexes.push(ReflexExplanation {
                    matched: matched.clone(),
                    lessons: matched
                        .source_lessons
                        .iter()
                        .map(|id| self.lesson(id))
                        .collect::<Result<_>>()?,
                    source_campaign: source_trial.campaign_id.clone(),
                    source_trial,
                });
            }
        }
        Ok(Explanation {
            experience_id: experience.id,
            task: experience.goal,
            outcome: experience.outcome,
            agent: experience.agent,
            applications,
            lineage: experience.relations,
            repeated_mistakes: experience.repeated_mistakes,
            resilience: experience.resilience,
            reflexes,
        })
    }
}
