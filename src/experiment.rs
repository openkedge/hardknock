// SPDX-License-Identifier: Apache-2.0

use crate::{
    Error, Result,
    core::{
        ArtifactRef, EnvironmentMode, ExecutionRecord, ExperienceId, ExperimentId, HypothesisId,
        LessonId, RealityId, StateRef, TrialId,
    },
    evaluation::{Evaluation, EvaluationSpec},
    experience::{Experience, Outcome},
    lesson::{Lesson, LessonStatus},
};
use crate::{
    cancellation::Cancellation,
    core::{AgentIdentity, CommandSpec},
    experience::{EnvironmentContext, Perturbation, ReplaySpec},
    store::Store,
    workflow::{RunRequest, run_once},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentConclusion {
    SupportsHypothesis,
    ContradictsHypothesis,
    Inconclusive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentStatus {
    Running,
    Completed,
    Interrupted,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrialMutation {
    ReplaceCommand { from: String, to: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialSpec {
    pub id: TrialId,
    pub name: String,
    pub command: String,
    pub mutations: Vec<TrialMutation>,
    pub evaluation: EvaluationSpec,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrialResult {
    pub spec: TrialSpec,
    pub experience_id: ExperienceId,
    pub reality_id: RealityId,
    pub execution: ExecutionRecord,
    pub evaluation: Evaluation,
    pub outcome: Outcome,
    pub starting_state: StateRef,
    pub environment_fingerprint: String,
    pub artifacts: Vec<ArtifactRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterfactualPlan {
    pub starting_state: StateRef,
    pub environment_fingerprint: String,
    pub timeout_secs: u64,
    pub trials: Vec<TrialSpec>,
}

impl CounterfactualPlan {
    pub fn from_lesson(source: &Experience, lesson: &Lesson) -> Result<Self> {
        let invalid = |reason: &str| {
            Error::Intervention(format!(
                "Counterfactual experiment cannot guarantee equivalent starting state: {reason}"
            ))
        };
        if source.id != lesson.source_experience || !lesson.context_match.matches(&source.context) {
            return Err(invalid("Lesson source/context mismatch"));
        }
        if matches!(
            lesson.status,
            LessonStatus::Retired | LessonStatus::Validated
        ) {
            return Err(invalid("Lesson is not active for V0.1 experiments"));
        }
        let replay = source.replay.as_ref().ok_or_else(|| {
            invalid("source used an opaque agent; use run --script for explicit replay")
        })?;
        if source.context.environment.mode != EnvironmentMode::Controlled
            || source.evaluation.spec.checks.is_empty()
        {
            return Err(invalid(
                "explicit controlled script and required checks are needed",
            ));
        }
        let avoid = lesson
            .avoid
            .as_ref()
            .ok_or_else(|| invalid("missing baseline action"))?;
        let prefer = lesson
            .prefer
            .as_ref()
            .and_then(|p| p.shell_script())
            .ok_or_else(|| invalid("only explicit shell-script alternatives are supported"))?;
        if !avoid.matches_shell(&replay.script)
            || prefer.trim().is_empty()
            || prefer.trim() == replay.script.trim()
            || prefer.contains('\0')
        {
            return Err(invalid(
                "avoid must match the complete recorded script and prefer must differ",
            ));
        }
        Ok(Self {
            starting_state: source.starting_state.clone(),
            environment_fingerprint: source.context.environment.fingerprint.clone(),
            timeout_secs: replay.timeout_secs,
            trials: vec![
                TrialSpec {
                    id: TrialId::new(),
                    name: "baseline".into(),
                    command: replay.script.clone(),
                    mutations: vec![],
                    evaluation: source.evaluation.spec.clone(),
                },
                TrialSpec {
                    id: TrialId::new(),
                    name: "alternative".into(),
                    command: prefer.into(),
                    mutations: vec![TrialMutation::ReplaceCommand {
                        from: replay.script.clone(),
                        to: prefer.into(),
                    }],
                    evaluation: source.evaluation.spec.clone(),
                },
            ],
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Experiment {
    pub id: ExperimentId,
    pub created_at: DateTime<Utc>,
    pub source_experience: ExperienceId,
    pub lesson_id: LessonId,
    pub hypothesis_id: HypothesisId,
    pub starting_state: StateRef,
    pub plan: CounterfactualPlan,
    pub status: ExperimentStatus,
    pub trials: Vec<TrialResult>,
    pub conclusion: ExperimentConclusion,
    pub failure: Option<String>,
}

pub trait ExperimentConclusionPolicy {
    fn conclude(&self, trials: &[TrialResult]) -> ExperimentConclusion;
}
pub struct PairedComparison;
impl ExperimentConclusionPolicy for PairedComparison {
    fn conclude(&self, trials: &[TrialResult]) -> ExperimentConclusion {
        let [baseline, alternative] = trials else {
            return ExperimentConclusion::Inconclusive;
        };
        if baseline.spec.name != "baseline"
            || alternative.spec.name != "alternative"
            || baseline.starting_state != alternative.starting_state
            || baseline.environment_fingerprint != alternative.environment_fingerprint
            || baseline.spec.evaluation != alternative.spec.evaluation
            || baseline.reality_id == alternative.reality_id
            || baseline.experience_id == alternative.experience_id
            || baseline.spec.id == alternative.spec.id
            || baseline.spec.command == alternative.spec.command
            || !baseline.spec.mutations.is_empty()
            || alternative.spec.mutations
                != vec![TrialMutation::ReplaceCommand {
                    from: baseline.spec.command.clone(),
                    to: alternative.spec.command.clone(),
                }]
            || trials.iter().any(|t| {
                t.outcome != Outcome::from_evaluation(&t.evaluation)
                    || t.evaluation.spec != t.spec.evaluation
                    || t.evaluation.validate().is_err()
            })
        {
            return ExperimentConclusion::Inconclusive;
        }
        match (baseline.outcome, alternative.outcome) {
            (Outcome::Failure, Outcome::Success) => ExperimentConclusion::SupportsHypothesis,
            (Outcome::Success, Outcome::Failure) => ExperimentConclusion::ContradictsHypothesis,
            _ => ExperimentConclusion::Inconclusive,
        }
    }
}

pub struct ExperimentEngine<'a> {
    pub store: &'a Store,
}

impl ExperimentEngine<'_> {
    pub async fn execute(&self, lesson_id: &LessonId, cancel: &Cancellation) -> Result<Experiment> {
        let lesson = self.store.lesson(lesson_id)?;
        let source = self.store.experience(&lesson.source_experience)?;
        let plan = CounterfactualPlan::from_lesson(&source, &lesson)?;
        let current = EnvironmentContext::capture(
            &source.context.environment.cwd,
            EnvironmentMode::Controlled,
        )?;
        if current.fingerprint != plan.environment_fingerprint {
            return Err(Error::Intervention("Counterfactual experiment cannot guarantee equivalent starting state: relevant environment fingerprint changed.".into()));
        }
        let mut experiment = Experiment {
            id: ExperimentId::new(),
            created_at: Utc::now(),
            source_experience: source.id.clone(),
            lesson_id: lesson.id.clone(),
            hypothesis_id: lesson.hypothesis_id,
            starting_state: plan.starting_state.clone(),
            plan,
            status: ExperimentStatus::Running,
            trials: vec![],
            conclusion: ExperimentConclusion::Inconclusive,
            failure: None,
        };
        self.store.insert_experiment(&experiment)?;
        let result = async {
            for spec in experiment.plan.trials.clone() {
                if cancel.is_cancelled() {
                    break;
                }
                let run = run_once(
                    self.store,
                    RunRequest {
                        state: experiment.starting_state.clone(),
                        goal: source.goal.clone(),
                        agent: AgentIdentity {
                            kind: "scripted-trial".into(),
                            executable: "/bin/sh".into(),
                            version: Some(env!("CARGO_PKG_VERSION").into()),
                            model: None,
                        },
                        command: CommandSpec::shell(&spec.command, EnvironmentMode::Controlled),
                        evaluation: spec.evaluation.clone(),
                        timeout_secs: experiment.plan.timeout_secs,
                        keep: false,
                        replay: Some(ReplaySpec {
                            script: spec.command.clone(),
                            timeout_secs: experiment.plan.timeout_secs,
                        }),
                        perturbations: spec
                            .mutations
                            .iter()
                            .map(|m| match m {
                                TrialMutation::ReplaceCommand { from, to } => {
                                    Perturbation::ReplaceCommand {
                                        from: from.clone(),
                                        to: to.clone(),
                                    }
                                }
                            })
                            .collect(),
                        expected_fingerprint: Some(experiment.plan.environment_fingerprint.clone()),
                    },
                    cancel,
                )
                .await?;
                let trial = TrialResult {
                    spec,
                    experience_id: run.experience.id,
                    reality_id: run.reality.id,
                    execution: run.execution,
                    evaluation: run.experience.evaluation,
                    outcome: run.experience.outcome,
                    starting_state: run.experience.starting_state,
                    environment_fingerprint: run.experience.context.environment.fingerprint,
                    artifacts: run.experience.evidence.artifacts,
                };
                self.store.insert_trial(&experiment, &trial)?;
                experiment.trials.push(trial);
            }
            Ok::<(), Error>(())
        }
        .await;
        if let Err(error) = result {
            experiment.status = if cancel.is_cancelled() {
                ExperimentStatus::Interrupted
            } else {
                ExperimentStatus::Failed
            };
            experiment.failure = Some(error.to_string());
            self.store.finish_experiment(&experiment)?;
            return Err(Error::ExperimentFailed {
                id: experiment.id.to_string(),
                source: Box::new(error),
            });
        }
        experiment.status = if cancel.is_cancelled() {
            ExperimentStatus::Interrupted
        } else {
            ExperimentStatus::Completed
        };
        if experiment.status == ExperimentStatus::Completed {
            experiment.conclusion = PairedComparison.conclude(&experiment.trials);
        }
        self.store.finish_experiment(&experiment)?;
        Ok(experiment)
    }
}
