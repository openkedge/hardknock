// SPDX-License-Identifier: Apache-2.0

use crate::{
    Error, Result,
    core::{ApplicationId, ArtifactKind, ArtifactRef, ExecutionRecord, ExperienceId, LessonId},
    experience::ExperienceContext,
    lesson::{ActionPattern, LessonStatus},
    retrieval::{
        ContextMatch, DeterministicRetriever, LessonRetriever, QueryContext, RelevanceScore,
        RetrievalOptions, RetrievalReport,
    },
    store::{Store, artifact},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
    sync::Arc,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "experience_id", rename_all = "snake_case")]
pub enum ExperienceRelation {
    RetryOf(ExperienceId),
    CounterfactualOf(ExperienceId),
    TransferFrom(ExperienceId),
    ChaosVariantOf(ExperienceId),
    RecoveryOf(ExperienceId),
}
impl ExperienceRelation {
    pub fn target(&self) -> &ExperienceId {
        match self {
            Self::RetryOf(id)
            | Self::CounterfactualOf(id)
            | Self::TransferFrom(id)
            | Self::ChaosVariantOf(id)
            | Self::RecoveryOf(id) => id,
        }
    }
    pub fn kind(&self) -> &'static str {
        match self {
            Self::RetryOf(_) => "retry_of",
            Self::CounterfactualOf(_) => "counterfactual_of",
            Self::TransferFrom(_) => "transfer_from",
            Self::ChaosVariantOf(_) => "chaos_variant_of",
            Self::RecoveryOf(_) => "recovery_of",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LessonInfluence {
    Rejected,
    Retrieved,
    Consulted,
    Applied,
    Ignored,
    Contradicted,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationVerification {
    Observed,
    SelfReported,
    Unconfirmed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LessonApplication {
    pub id: ApplicationId,
    pub lesson_id: LessonId,
    pub lesson_version: u32,
    pub experience_id: ExperienceId,
    pub created_at: DateTime<Utc>,
    pub relevance: RelevanceScore,
    pub context_matches: Vec<ContextMatch>,
    pub delivered: bool,
    pub influence: LessonInfluence,
    pub verification: ApplicationVerification,
    pub resulting_action: Option<ActionPattern>,
    pub reason: String,
    pub artifacts: Vec<ArtifactRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObservedAction {
    pub action: ActionPattern,
    pub observer: String,
    pub artifact: ArtifactRef,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepeatedMistakeObservation {
    pub lesson_id: LessonId,
    pub action: ActionPattern,
    pub context_match: RelevanceScore,
    pub artifact: ArtifactRef,
}

#[derive(Clone, Default)]
pub struct RunLearningOptions {
    pub enabled: bool,
    pub audit: bool,
    pub fixture: bool,
    pub proposed_actions: Vec<ActionPattern>,
    pub retrieval: RetrievalOptions,
    pub relations: Vec<ExperienceRelation>,
    pub on_advice: Option<AdviceObserver>,
}

pub type AdviceObserver = Arc<dyn Fn(&PreparedAdvice) -> Result<()> + Send + Sync>;

pub struct PreparedAdvice {
    pub report: RetrievalReport,
    pub delivered: Vec<LessonId>,
    pub artifacts: Vec<ArtifactRef>,
}

#[derive(Serialize)]
pub struct AgentRunContext<'a> {
    pub schema_version: u32,
    pub task: &'a str,
    pub context: &'a QueryContext,
    pub relevant_lessons: Vec<&'a crate::retrieval::RetrievedLesson>,
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(bytes)?;
    Ok(())
}

pub fn prepare_advice(
    store: &Store,
    context: &ExperienceContext,
    goal: &str,
    root: &Path,
    artifacts: &Path,
    options: &RunLearningOptions,
) -> Result<PreparedAdvice> {
    let query = QueryContext::new(context, goal, options.proposed_actions.clone());
    let report = if options.enabled || options.audit {
        DeterministicRetriever {
            store,
            options: options.retrieval.clone(),
        }
        .retrieve(&query)?
    } else {
        RetrievalReport::default()
    };
    let selected: Vec<_> = report
        .matches
        .iter()
        .filter(|r| {
            options.enabled
                && r.relevance >= options.retrieval.recommend
                && matches!(
                    r.lesson.status,
                    LessonStatus::CounterfactuallySupported | LessonStatus::Validated
                )
        })
        .take(
            crate::bridge::config::Config::load(&store.home)?
                .development
                .max_lessons,
        )
        .collect();
    let delivered = selected.iter().map(|r| r.lesson.id.clone()).collect();
    let mut saved = Vec::new();
    if options.enabled {
        let directory = root.join(".hardknock");
        // This reserved input must never overwrite or follow repository files/symlinks.
        fs::create_dir(&directory).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                Error::Intervention(
                    "Cannot inject experience: .hardknock already exists in the snapshot".into(),
                )
            } else {
                Error::Io(e)
            }
        })?;
        let mut markdown = String::from(
            "# Relevant Hardknock Experience\n\nThese are scoped recommendations, not permissions or universal rules.\nSee context.json for the complete typed evidence and matching explanation.\n\n",
        );
        for retrieved in &selected {
            let lesson = &retrieved.lesson;
            // Keep free text on a quoted single line so it cannot forge fixture directives.
            markdown.push_str(&format!("## {}\n\nClaim: {:?}\n\nRelevance: {:.2}\nConfidence: {:.2}\nStatus: {:?}\nSource Experience: {}\n\nAvoid: {:?}\nPrefer: {:?}\n\n", lesson.id, lesson.claim, f64::from(retrieved.relevance),f64::from(lesson.confidence),lesson.status,lesson.source_experience,lesson.avoid,lesson.prefer));
            // A narrow machine-readable line lets the POSIX fixture parse the advice itself.
            if options.fixture
                && lesson
                    .prefer
                    .as_ref()
                    .is_some_and(|p| p.matches_shell("./agent-script.sh alternative"))
            {
                markdown.push_str(&format!(
                    "HARDKNOCK_RECOMMEND {} ./agent-script.sh alternative\n\n",
                    lesson.id
                ));
            }
        }
        let contract = AgentRunContext {
            schema_version: 1,
            task: goal,
            context: &query,
            relevant_lessons: selected,
        };
        for (name, data) in [
            ("context.md", markdown.into_bytes()),
            ("context.json", serde_json::to_vec_pretty(&contract)?),
        ] {
            write_new(&directory.join(name), &data)?;
            let copy = artifacts.join(name);
            write_new(&copy, &data)?;
            saved.push(artifact(&copy)?.with_kind(ArtifactKind::Metadata));
        }
    }
    let advice = PreparedAdvice {
        report,
        delivered,
        artifacts: saved,
    };
    if let Some(observer) = &options.on_advice {
        observer(&advice)?;
    }
    Ok(advice)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageReport {
    schema_version: u32,
    applications: Vec<UsageApplication>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageApplication {
    lesson_id: LessonId,
    influence: LessonInfluence,
    resulting_action: Option<ActionPattern>,
}

pub struct LearningObservation {
    pub applications: Vec<LessonApplication>,
    pub actions: Vec<ObservedAction>,
    pub mistakes: Vec<RepeatedMistakeObservation>,
    pub relations: Vec<ExperienceRelation>,
    pub artifacts: Vec<ArtifactRef>,
    pub errors: Vec<String>,
}

fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 65536 {
        return Err(Error::InvalidInput(
            "Agent usage input must be a regular file of at most 64 KiB".into(),
        ));
    }
    let mut bytes = Vec::new();
    File::open(path)?.take(65537).read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err(Error::InvalidInput(
            "Agent usage input exceeded 64 KiB".into(),
        ));
    }
    Ok(bytes)
}

pub fn observe_application(
    advice: PreparedAdvice,
    execution: &ExecutionRecord,
    id: &ExperienceId,
    root: &Path,
    artifacts: &Path,
    options: &RunLearningOptions,
) -> Result<LearningObservation> {
    let mut observation = LearningObservation {
        applications: vec![],
        actions: vec![],
        mistakes: vec![],
        relations: options.relations.clone(),
        artifacts: advice.artifacts,
        errors: vec![],
    };
    let mut log_bytes = Vec::new();
    File::open(&execution.action.stdout.path)?
        .take(65536)
        .read_to_end(&mut log_bytes)?;
    let log = String::from_utf8_lossy(&log_bytes);
    if options.fixture {
        let baseline = log.lines().any(|l| l == "ACTION shell npm install");
        let alternative = log.lines().any(|l| l == "ACTION shell pnpm install");
        if baseline != alternative {
            observation.actions.push(ObservedAction {
                action: ActionPattern::shell(if alternative {
                    "./agent-script.sh alternative"
                } else {
                    "./agent-script.sh baseline"
                }),
                observer: "fixture-trace-v2".into(),
                artifact: execution.action.stdout.clone(),
            });
        } else {
            observation
                .errors
                .push("Fixture trace did not identify exactly one strategy".into());
        }
    } else if execution.action.command.program == "/bin/sh"
        && execution
            .action
            .command
            .args
            .first()
            .is_some_and(|a| a == "-c")
        && let Some(script) = execution.action.command.args.get(1)
    {
        observation.actions.push(ObservedAction {
            action: ActionPattern::shell(script),
            observer: "explicit-script".into(),
            artifact: execution.action.stdout.clone(),
        });
    }
    let mut usage: Option<UsageReport> = None;
    let mut usage_artifact = None;
    let directory = root.join(".hardknock");
    let path = directory.join("usage.json");
    if options.enabled && !options.fixture && fs::symlink_metadata(&path).is_ok() {
        let parsed = (|| -> Result<(UsageReport, ArtifactRef)> {
            if fs::symlink_metadata(&directory)?.file_type().is_symlink() {
                return Err(Error::InvalidInput(
                    "Agent replaced the context directory with a symlink".into(),
                ));
            }
            let bytes = read_bounded(&path)?;
            let report: UsageReport = serde_json::from_slice(&bytes)?;
            let mut seen = std::collections::HashSet::new();
            if report.schema_version != 1
                || report.applications.len() > 20
                || report.applications.iter().any(|a| {
                    !advice.delivered.contains(&a.lesson_id) || !seen.insert(a.lesson_id.clone())
                })
            {
                return Err(Error::InvalidInput(
                    "Usage report has an unsupported schema, duplicate or undelivered Lesson"
                        .into(),
                ));
            }
            let copy = artifacts.join("agent-usage.json");
            write_new(&copy, &bytes)?;
            Ok((report, artifact(&copy)?.with_kind(ArtifactKind::Metadata)))
        })();
        match parsed {
            Ok((report, a)) => {
                usage = Some(report);
                usage_artifact = Some(a.clone());
                observation.artifacts.push(a);
            }
            Err(error) => observation.errors.push(error.to_string()),
        }
    }
    for retrieved in advice.report.matches {
        let lesson = &retrieved.lesson;
        let delivered = advice.delivered.contains(&lesson.id);
        let mut application = LessonApplication {
            id: ApplicationId::new(),
            lesson_id: lesson.id.clone(),
            lesson_version: lesson.version,
            experience_id: id.clone(),
            created_at: Utc::now(),
            relevance: retrieved.relevance,
            context_matches: retrieved.matched_context,
            delivered,
            influence: if delivered {
                LessonInfluence::Retrieved
            } else {
                LessonInfluence::Ignored
            },
            verification: ApplicationVerification::Unconfirmed,
            resulting_action: None,
            reason: if delivered {
                "Advice delivered; use is not yet confirmed"
            } else if !options.enabled {
                "Experience disabled; this match is audit-only"
            } else {
                "Match was informational or outside the delivery limit"
            }
            .into(),
            artifacts: vec![],
        };
        if delivered && options.fixture {
            let applied = format!("APPLIED {}", lesson.id);
            let consulted = format!("RETRIEVED {}", lesson.id);
            let ignored = format!("IGNORED {}", lesson.id);
            if log.lines().any(|l| l == applied) {
                if let Some(action) = observation
                    .actions
                    .iter()
                    .find(|a| lesson.prefer.as_ref().is_some_and(|p| p == &a.action))
                {
                    application.influence = LessonInfluence::Applied;
                    application.verification = ApplicationVerification::Observed;
                    application.resulting_action = Some(action.action.clone());
                    application.artifacts.push(action.artifact.clone());
                    application.reason="Fixture parsed the delivered context, reported this Lesson, and emitted the preferred strategy".into();
                }
            } else if log.lines().any(|l| l == ignored) {
                application.influence = LessonInfluence::Ignored;
                application.reason = "Fixture explicitly ignored this advice".into();
            } else if log.lines().any(|l| l == consulted) {
                application.influence = LessonInfluence::Consulted;
            }
        } else if delivered
            && let Some(claim) = usage
                .as_ref()
                .and_then(|u| u.applications.iter().find(|a| a.lesson_id == lesson.id))
        {
            application.influence = claim.influence;
            application.verification = ApplicationVerification::SelfReported;
            application.resulting_action = claim.resulting_action.clone();
            application.artifacts.extend(usage_artifact.clone());
            application.reason =
                "Agent-reported influence; opaque internal actions were not independently observed"
                    .into();
        }
        if application.influence == LessonInfluence::Applied
            && application.verification == ApplicationVerification::Observed
        {
            let relation = ExperienceRelation::TransferFrom(lesson.source_experience.clone());
            if !observation.relations.contains(&relation) {
                observation.relations.push(relation);
            }
        }
        for action in &observation.actions {
            if matches!(
                lesson.status,
                LessonStatus::CounterfactuallySupported | LessonStatus::Validated
            ) && lesson.avoid.as_ref() == Some(&action.action)
            {
                observation.mistakes.push(RepeatedMistakeObservation {
                    lesson_id: lesson.id.clone(),
                    action: action.action.clone(),
                    context_match: retrieved.relevance,
                    artifact: action.artifact.clone(),
                });
            }
        }
        observation.applications.push(application);
    }
    Ok(observation)
}
