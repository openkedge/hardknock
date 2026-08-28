// SPDX-License-Identifier: Apache-2.0

use crate::{
    Error, Result,
    core::{
        ArtifactRef, EnvironmentMode, ExecutionRecord, ExperienceId, ExperimentId, HypothesisId,
        LessonId, RealityId, StateRef, TrialId,
    },
    evaluation::{Evaluation, EvaluationSpec},
    experience::{Experience, ExperienceContext, Outcome},
    lesson::{Lesson, LessonStatus},
};
use crate::{
    application::{ExperienceRelation, RunLearningOptions},
    cancellation::Cancellation,
    core::{AgentIdentity, CommandSpec},
    experience::{EnvironmentContext, Perturbation, ReplaySpec},
    store::Store,
    workflow::{RunRequest, run_with_learning},
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
    /// Paired controlled reconstruction of a native observation, not a replay of its inherited environment.
    #[serde(default)]
    pub external_reconstruction: bool,
    pub starting_state: StateRef,
    pub environment_fingerprint: String,
    pub timeout_secs: u64,
    pub trials: Vec<TrialSpec>,
    #[serde(default)]
    pub retest: Option<RetestContext>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetestContext {
    pub goal: String,
    pub context: ExperienceContext,
    pub evaluation: EvaluationSpec,
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
        if matches!(lesson.status, LessonStatus::Retired) {
            return Err(invalid("Lesson is not active for V0.1 experiments"));
        }
        if source.tags.iter().any(|t| t == "bridge-clean-start-v1") {
            return Self::from_observed_action(source, lesson);
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
            external_reconstruction: false,
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
            retest: None,
        })
    }

    fn from_observed_action(source: &Experience, lesson: &Lesson) -> Result<Self> {
        let baseline = lesson
            .avoid
            .as_ref()
            .and_then(|a| a.shell_script())
            .ok_or_else(|| {
                Error::InvalidInput("Observed reconstruction requires a shell baseline".into())
            })?;
        let alternative = lesson
            .prefer
            .as_ref()
            .and_then(|a| a.shell_script())
            .ok_or_else(|| {
                Error::InvalidInput("Observed reconstruction requires a shell alternative".into())
            })?;
        if baseline == alternative
            || alternative.trim().is_empty()
            || alternative.contains('\0')
            || source.evaluation.spec.checks.is_empty()
            || source.starting_state.git_commit == "unversioned"
            || std::iter::once(baseline)
                .chain(std::iter::once(alternative))
                .chain(source.evaluation.spec.checks.iter().map(String::as_str))
                .any(|text| text.contains("[REDACTED]") || text.contains("…[truncated]"))
            || !source
                .observed_actions
                .iter()
                .any(|a| a.observer == "bridge-lifecycle-v1" && a.action.matches_shell(baseline))
        {
            return Err(Error::Intervention("Controlled reconstruction needs a clean recorded Git start, observed baseline, distinct alternative and configured evaluator".into()));
        }
        let environment = EnvironmentContext::capture(
            &source.context.environment.cwd,
            EnvironmentMode::Controlled,
        )?;
        Ok(Self {
            external_reconstruction: true,
            starting_state: source.starting_state.clone(),
            environment_fingerprint: environment.fingerprint,
            timeout_secs: 30,
            retest: None,
            trials: vec![
                TrialSpec {
                    id: TrialId::new(),
                    name: "baseline".into(),
                    command: baseline.into(),
                    mutations: vec![],
                    evaluation: source.evaluation.spec.clone(),
                },
                TrialSpec {
                    id: TrialId::new(),
                    name: "alternative".into(),
                    command: alternative.into(),
                    mutations: vec![TrialMutation::ReplaceCommand {
                        from: baseline.into(),
                        to: alternative.into(),
                    }],
                    evaluation: source.evaluation.spec.clone(),
                },
            ],
        })
    }

    pub fn for_retest(
        source: &Experience,
        lesson: &Lesson,
        state: StateRef,
        retest: RetestContext,
    ) -> Result<Self> {
        let mut plan = Self::from_lesson(source, lesson)?;
        retest.evaluation.validate()?;
        if retest.evaluation.checks.is_empty()
            || !lesson.context_match.matches(&retest.context)
            || state.repo_path != retest.context.repository.path
            || state.git_commit != retest.context.repository.commit
        {
            return Err(Error::Intervention(
                "Retest requires matching Lesson scope, snapshot and required checks".into(),
            ));
        }
        plan.starting_state = state;
        plan.environment_fingerprint = retest.context.environment.fingerprint.clone();
        for trial in &mut plan.trials {
            trial.evaluation = retest.evaluation.clone();
        }
        plan.retest = Some(retest);
        Ok(plan)
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
        self.execute_at(lesson_id, None, cancel).await
    }
    pub async fn execute_at(
        &self,
        lesson_id: &LessonId,
        target: Option<(StateRef, EvaluationSpec, String)>,
        cancel: &Cancellation,
    ) -> Result<Experiment> {
        let lesson = self.store.lesson(lesson_id)?;
        let source = self.store.experience(&lesson.source_experience)?;
        let plan = if let Some((state, evaluation, goal)) = target {
            let context =
                ExperienceContext::capture(&state, &state.repo_path, EnvironmentMode::Controlled)?;
            CounterfactualPlan::for_retest(
                &source,
                &lesson,
                state,
                RetestContext {
                    goal,
                    context,
                    evaluation,
                },
            )?
        } else {
            CounterfactualPlan::from_lesson(&source, &lesson)?
        };
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
                let run = run_with_learning(
                    self.store,
                    RunRequest {
                        state: experiment.starting_state.clone(),
                        goal: experiment
                            .plan
                            .retest
                            .as_ref()
                            .map(|r| r.goal.clone())
                            .unwrap_or_else(|| source.goal.clone()),
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
                    &RunLearningOptions {
                        relations: vec![ExperienceRelation::CounterfactualOf(source.id.clone())],
                        ..Default::default()
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
