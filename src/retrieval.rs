// SPDX-License-Identifier: Apache-2.0

use crate::{
    Error, Result,
    core::LessonId,
    experience::{EnvironmentContext, ExperienceContext, RepositoryContext},
    lesson::{ActionPattern, Lesson, LessonStatus},
    store::{EpistemicStore, Store},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct RelevanceScore(f64);
impl TryFrom<f64> for RelevanceScore {
    type Error = Error;
    fn try_from(value: f64) -> Result<Self> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(Error::InvalidInput(
                "Relevance must be finite and between 0 and 1".into(),
            ))
        }
    }
}
impl From<RelevanceScore> for f64 {
    fn from(value: RelevanceScore) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryContext {
    pub repository: RepositoryContext,
    pub environment: EnvironmentContext,
    pub detected_markers: Vec<String>,
    pub task: String,
    pub proposed_actions: Vec<ActionPattern>,
    pub tags: Vec<String>,
}
impl QueryContext {
    pub fn new(context: &ExperienceContext, task: &str, actions: Vec<ActionPattern>) -> Self {
        let mut actions = actions;
        if context
            .tags
            .iter()
            .any(|t| t == "fixture-family:pnpm-workspace-v2")
            && actions.iter().any(|a| a.matches_shell("npm install"))
        {
            actions.push(ActionPattern::shell("./agent-script.sh baseline"));
        }
        Self {
            repository: context.repository.clone(),
            environment: context.environment.clone(),
            detected_markers: context.markers.clone(),
            task: task.into(),
            proposed_actions: actions,
            tags: context.tags.clone(),
        }
    }
    pub fn experience_context(&self) -> ExperienceContext {
        ExperienceContext {
            repository: self.repository.clone(),
            environment: self.environment.clone(),
            markers: self.detected_markers.clone(),
            tags: self.tags.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextMatch {
    pub signal: String,
    pub value: String,
    pub weight: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recommendation {
    Informational,
    Recommend,
    StrongRecommendation,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RetrievedLesson {
    pub lesson: Lesson,
    pub relevance: RelevanceScore,
    pub matched_context: Vec<ContextMatch>,
    pub recommendation: Recommendation,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RejectedLesson {
    pub lesson_id: LessonId,
    pub reason: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RetrievalReport {
    pub matches: Vec<RetrievedLesson>,
    pub excluded: Vec<RejectedLesson>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RetrievalOptions {
    pub minimum: RelevanceScore,
    pub recommend: RelevanceScore,
    pub strong: RelevanceScore,
    pub include_candidates: bool,
}
impl Default for RetrievalOptions {
    fn default() -> Self {
        Self {
            minimum: RelevanceScore(0.5),
            recommend: RelevanceScore(0.7),
            strong: RelevanceScore(0.85),
            include_candidates: false,
        }
    }
}
impl RetrievalOptions {
    pub fn validate(&self) -> Result<()> {
        if self.minimum > self.recommend || self.recommend > self.strong {
            return Err(Error::InvalidInput(
                "Relevance thresholds must satisfy minimum <= recommend <= strong".into(),
            ));
        }
        Ok(())
    }
}

pub trait RelevancePolicy {
    fn score(&self, lesson: &Lesson, context: &QueryContext)
    -> (RelevanceScore, Vec<ContextMatch>);
}
pub struct DeterministicRelevance;
/// Shared by cold retrieval and the pre-tool in-memory cache. Never reads the store.
pub fn freshness_score(
    lesson: &Lesson,
    context: &QueryContext,
    basis: Option<&crate::development::FreshnessBasis>,
    config: &crate::development::DevelopmentConfig,
    now: chrono::DateTime<chrono::Utc>,
) -> (RelevanceScore, Vec<ContextMatch>) {
    let (base, mut matched) = DeterministicRelevance.score(lesson, context);
    let Some(basis) = basis else {
        return (RelevanceScore(0.0), matched);
    };
    let health = crate::development::assess_freshness(basis, context, None, now, config);
    if health.multiplier < 1.0 {
        matched.push(ContextMatch {
            signal: "freshness".into(),
            value: format!("{:?}: {}", health.state, health.reasons.join("; ")),
            weight: 0.0,
        });
    }
    (RelevanceScore(base.0 * health.multiplier), matched)
}
impl RelevancePolicy for DeterministicRelevance {
    fn score(
        &self,
        lesson: &Lesson,
        context: &QueryContext,
    ) -> (RelevanceScore, Vec<ContextMatch>) {
        // Required scope is a hard gate; unrelated contexts cannot accumulate weak matches.
        if !lesson.context_match.matches(&context.experience_context()) {
            return (RelevanceScore(0.0), vec![]);
        }
        let mut matches = Vec::new();
        let scope = &lesson.context_match;
        if !scope.required_markers.is_empty() {
            matches.push(ContextMatch {
                signal: "required_markers".into(),
                value: scope.required_markers.join(", "),
                weight: 0.40,
            });
        }
        if scope
            .repository
            .as_ref()
            .is_some_and(|p| *p == context.repository.path)
        {
            matches.push(ContextMatch {
                signal: "repository".into(),
                value: context.repository.path.display().to_string(),
                weight: 0.20,
            });
        }
        if let Some(avoid) = &lesson.avoid
            && context.proposed_actions.iter().any(|action| {
                action
                    .shell_script()
                    .is_some_and(|script| avoid.matches_shell(script))
            })
        {
            matches.push(ContextMatch {
                signal: "proposed_action".into(),
                value: avoid.shell_script().unwrap_or_default().into(),
                weight: 0.30,
            });
        }
        if !scope.tags.is_empty() {
            matches.push(ContextMatch {
                signal: "required_tags".into(),
                value: scope.tags.join(", "),
                weight: 0.10,
            });
        }
        let score = (matches.iter().map(|m| m.weight).sum::<f64>() * 100.0)
            .round()
            .clamp(0.0, 100.0)
            / 100.0;
        (RelevanceScore(score), matches)
    }
}

pub trait LessonRetriever {
    fn retrieve(&self, context: &QueryContext) -> Result<RetrievalReport>;
}
pub struct DeterministicRetriever<'a> {
    pub store: &'a Store,
    pub options: RetrievalOptions,
}
impl LessonRetriever for DeterministicRetriever<'_> {
    fn retrieve(&self, context: &QueryContext) -> Result<RetrievalReport> {
        self.options.validate()?;
        let mut report = RetrievalReport::default();
        let config = crate::bridge::config::Config::load(&self.store.home)?.development;
        let lessons = self.store.all_lessons()?;
        let bases = self.store.lesson_freshness_bases(&lessons)?;
        for lesson in lessons {
            let activation = if self
                .store
                .causal_artifact_quarantined(&lesson.id.to_string())?
            {
                crate::epistemic::ExperienceActivationState::Quarantined
            } else {
                self.store.lesson_activation_state(&lesson.id)?
            };
            let eligible = matches!(
                lesson.status,
                LessonStatus::CounterfactuallySupported | LessonStatus::Validated
            ) || (self.options.include_candidates
                && lesson.status == LessonStatus::Candidate);
            let (relevance, matched_context) = freshness_score(
                &lesson,
                context,
                bases.get(&lesson.id),
                &config,
                chrono::Utc::now(),
            );
            let reason = if matches!(
                activation,
                crate::epistemic::ExperienceActivationState::Quarantined
                    | crate::epistemic::ExperienceActivationState::Disabled
            ) {
                Some(format!(
                    "Lesson activation state {activation:?} excludes automatic retrieval"
                ))
            } else if !eligible {
                Some(format!("Lesson state {:?} is not eligible", lesson.status))
            } else if !lesson.context_match.matches(&context.experience_context()) {
                Some("Required repository/marker/tag/environment scope does not match".into())
            } else if relevance < self.options.minimum {
                Some(format!(
                    "Relevance {:.2} is below threshold {:.2}",
                    f64::from(relevance),
                    f64::from(self.options.minimum)
                ))
            } else {
                None
            };
            if let Some(reason) = reason {
                report.excluded.push(RejectedLesson {
                    lesson_id: lesson.id,
                    reason,
                });
                continue;
            }
            let recommendation = if relevance >= self.options.strong {
                Recommendation::StrongRecommendation
            } else if relevance >= self.options.recommend {
                Recommendation::Recommend
            } else {
                Recommendation::Informational
            };
            report.matches.push(RetrievedLesson {
                lesson,
                relevance,
                matched_context,
                recommendation,
            });
        }
        report.matches.sort_by(|a, b| {
            f64::from(b.relevance)
                .total_cmp(&f64::from(a.relevance))
                .then_with(|| a.lesson.id.to_string().cmp(&b.lesson.id.to_string()))
        });
        Ok(report)
    }
}
