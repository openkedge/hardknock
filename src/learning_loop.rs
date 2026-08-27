// SPDX-License-Identifier: Apache-2.0

use crate::{
    Result,
    application::{ExperienceRelation, RunLearningOptions},
    cancellation::Cancellation,
    experience::Outcome,
    experiment::{Experiment, ExperimentEngine},
    lesson::{HeuristicConfidence, Lesson, LessonStatus},
    reflection::{DeterministicReflection, ReflectionProvider},
    retrieval::{DeterministicRetriever, LessonRetriever, QueryContext},
    store::{LessonStore, Store},
    workflow::{RunRequest, RunResult, run_with_learning},
};

pub struct LearningRunOptions {
    pub learning: RunLearningOptions,
    pub auto_reflect: bool,
    pub retry: bool,
    pub max_retries: u32,
}
pub struct LearningRunResult {
    pub initial: RunResult,
    pub retries: Vec<RunResult>,
    pub lessons: Vec<Lesson>,
    pub experiments: Vec<Experiment>,
    pub retry_stop_reason: String,
    pub interrupted: bool,
}

pub async fn execute_learning_run(
    store: &Store,
    request: RunRequest,
    options: LearningRunOptions,
    cancel: &Cancellation,
) -> Result<LearningRunResult> {
    if options.max_retries > 10 || (options.retry && !options.learning.enabled) {
        return Err(crate::Error::InvalidInput(
            "Retry requires experience enabled and a budget of at most 10 attempts".into(),
        ));
    }
    let initial = run_with_learning(store, request.clone(), &options.learning, cancel).await?;
    let mut lesson_ids = initial
        .experience
        .lesson_applications
        .iter()
        .map(|a| a.lesson_id.clone())
        .collect::<Vec<_>>();
    let mut experiments = Vec::new();
    if options.auto_reflect && !cancel.is_cancelled() {
        for hypothesis in DeterministicReflection.reflect(&initial.experience)? {
            store.insert_hypothesis(&hypothesis)?;
            let lesson = Lesson::candidate(&hypothesis, &HeuristicConfidence);
            LessonStore::insert(store, &lesson)?;
            experiments.push(
                ExperimentEngine { store }
                    .execute(&lesson.id, cancel)
                    .await?,
            );
            lesson_ids.push(lesson.id);
        }
    }
    let mut retries: Vec<RunResult> = Vec::new();
    let mut reason = if options.retry {
        "Retry budget exhausted"
    } else {
        "Retry was not requested"
    }
    .to_owned();
    if options.retry {
        for _ in 0..options.max_retries {
            let previous = retries.last().unwrap_or(&initial);
            if cancel.is_cancelled() {
                reason = "Interrupted; no further attempts".into();
                break;
            }
            if previous.experience.outcome != Outcome::Failure {
                reason = "No failed task remains eligible for retry".into();
                break;
            }
            let query = QueryContext::new(
                &initial.experience.context,
                &request.goal,
                options.learning.proposed_actions.clone(),
            );
            let matches = DeterministicRetriever {
                store,
                options: options.learning.retrieval.clone(),
            }
            .retrieve(&query)?;
            if !matches.matches.iter().any(|r| {
                r.relevance >= options.learning.retrieval.recommend
                    && matches!(
                        r.lesson.status,
                        LessonStatus::CounterfactuallySupported | LessonStatus::Validated
                    )
            }) {
                reason = "No applicable supported Lesson; retry skipped".into();
                break;
            }
            let mut learning = options.learning.clone();
            learning.enabled = true;
            learning.relations = vec![ExperienceRelation::RetryOf(previous.experience.id.clone())];
            let result = run_with_learning(store, request.clone(), &learning, cancel).await?;
            lesson_ids.extend(
                result
                    .experience
                    .lesson_applications
                    .iter()
                    .map(|a| a.lesson_id.clone()),
            );
            let success = result.experience.outcome == Outcome::Success;
            retries.push(result);
            if success {
                reason = "Retry succeeded".into();
                break;
            }
        }
    }
    lesson_ids.sort_by_key(|id| id.to_string());
    lesson_ids.dedup();
    let lessons = lesson_ids
        .iter()
        .map(|id| store.lesson(id))
        .collect::<Result<Vec<_>>>()?;
    Ok(LearningRunResult {
        initial,
        retries,
        lessons,
        experiments,
        retry_stop_reason: reason,
        interrupted: cancel.is_cancelled(),
    })
}
