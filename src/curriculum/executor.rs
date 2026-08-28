// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{
    Error, Result,
    bridge::config::Config,
    cancellation::Cancellation,
    core::*,
    experimentation::{ExperimentOrchestrator, ExperimentStatus},
    resilience::{campaign, testing, *},
    store::{CurriculumStore, Store},
};
use fs2::FileExt;
use std::{
    fs::{File, OpenOptions},
    time::{Duration, Instant},
};

pub struct CurriculumExecutor<'a> {
    pub store: &'a Store,
    pub config: &'a Config,
}
fn lock(store: &Store, name: &str) -> Result<File> {
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(store.home.join("locks").join(name))?;
    FileExt::try_lock_exclusive(&f).map_err(|_| {
        Error::Intervention("Curriculum executor or provider slot is already in use".into())
    })?;
    Ok(f)
}
impl CurriculumExecutor<'_> {
    pub fn plan(
        &self,
        target: CurriculumTarget,
        profile: &str,
        budget: &crate::budget::ExperienceBudget,
    ) -> Result<Curriculum> {
        let ctx = inventory(self.store, &target, profile, &self.config.curriculum)?;
        let c = DeterministicCurriculumPlanner.plan(&target, &ctx, budget)?;
        self.validate(&c)?;
        CurriculumStore::insert(self.store, &c)?;
        Ok(c)
    }
    fn save(&self, c: &mut Curriculum) -> Result<()> {
        c.revision += 1;
        c.updated_at = chrono::Utc::now();
        CurriculumStore::update(self.store, c)
    }
    pub fn plan_replication(
        &self,
        target: CurriculumTarget,
        profile: &str,
        budget: &crate::budget::ExperienceBudget,
    ) -> Result<Curriculum> {
        let mut ctx = inventory(self.store, &target, profile, &self.config.curriculum)?;
        let before = ctx.packages.clone();
        ctx.history.clear();
        for p in &mut ctx.packages {
            for d in &mut p.coverage.dimensions {
                d.tested.clear();
            }
        }
        let mut c = DeterministicCurriculumPlanner.plan(&target, &ctx, budget)?;
        c.before = before;
        for t in &mut c.trials {
            t.intent = TrialIntent::Replication;
        }
        for g in &mut c.goals {
            g.evidence_gap.rationale="Explicit replication requested; existing observations are retained and novelty suppression is intentionally bypassed".into();
        }
        self.validate(&c)?;
        CurriculumStore::insert(self.store, &c)?;
        Ok(c)
    }
    pub fn validate(&self, c: &Curriculum) -> Result<()> {
        self.config.curriculum.validate()?;
        if c.trials.len() > c.budget.max_curriculum_trials.unwrap_or(0)
            || c.max_rounds > self.config.curriculum.max_rounds
            || c.budget.max_realities > self.config.curriculum.max_realities
            || c.budget.max_agent_runs > self.config.curriculum.max_agent_runs
            || c.budget.max_parallel_trials != Some(1)
            || c.budget
                .max_duration_ms
                .is_none_or(|ms| ms == 0 || ms > self.config.curriculum.max_duration_seconds * 1000)
            || c.budget.max_commands_per_reality.is_some()
        {
            return Err(Error::Intervention(
                "Curriculum exceeds current local policy/budget".into(),
            ));
        }
        let total = c
            .trials
            .iter()
            .map(|t| t.estimated_budget.realities)
            .sum::<usize>();
        let agents = c
            .trials
            .iter()
            .map(|t| t.estimated_budget.agent_runs)
            .sum::<usize>();
        if total > c.budget.max_realities || agents > c.budget.max_agent_runs {
            return Err(Error::InvalidInput(
                "Aggregate curriculum budget exceeded".into(),
            ));
        }
        for t in &c.trials {
            self.validate_trial(t)?;
        }
        Ok(())
    }
    pub fn validate_trial(&self, t: &CurriculumTrial) -> Result<()> {
        let capabilities = RealityCapabilities::default();
        let needed = &t.required_isolation;
        if needed.filesystem_isolation > capabilities.filesystem_isolation
            || needed.process_isolation > capabilities.process_isolation
            || needed.network_isolation > capabilities.network_isolation
            || needed.external_effect_isolation > capabilities.external_effect_isolation
        {
            return Err(Error::Intervention(
                "Trial requires stronger Reality isolation than Git worktrees provide".into(),
            ));
        }
        let (realities, agents) = match &t.execution {
            TrialExecution::Experiment { request } => (request.candidates.len(), 0),
            _ => (2, 2),
        };
        if t.estimated_budget.realities != realities || t.estimated_budget.agent_runs != agents {
            return Err(Error::InvalidInput(
                "Trial cost must include every control and response arm".into(),
            ));
        }
        let mut scripts = vec![];
        match &t.execution {
            TrialExecution::Chaos { plan } => {
                campaign::validate(plan)?;
                if !plan.active_reflexes.is_empty()
                    || plan.perturbations.len() != 1
                    || plan.trial_budget != 1
                {
                    return Err(Error::InvalidInput("Curriculum chaos must compare one condition with a healthy unassisted control".into()));
                }
                scripts.extend(plan.command.args.iter().cloned());
                scripts.extend(plan.evaluation.checks.clone());
                if let Some(kind) = plan.fixture {
                    self.verify_fixture(&plan.starting_state, kind)?;
                }
            }
            TrialExecution::Experiment { request } => {
                if !request.capabilities.external_effects.is_empty()
                    || request.capabilities.allow_network
                    || request.capabilities.allow_external_mutations
                    || !request.capabilities.filesystem_scope.is_empty()
                {
                    return Err(Error::Intervention(
                        "Unsupported external effect or isolation requirement".into(),
                    ));
                }
                request.evaluator.validate()?;
                if request.evaluator.checks.is_empty() {
                    return Err(Error::InvalidInput(
                        "Curriculum needs an explicit evaluator".into(),
                    ));
                }
                for candidate in &request.candidates {
                    if let crate::experimentation::CandidateExecution::Shell { commands } =
                        &candidate.execution
                    {
                        scripts.extend(commands.clone());
                    } else {
                        return Err(Error::InvalidInput("Curriculum revalidation uses recorded shell recipes; opaque agent replay is unsupported".into()));
                    }
                }
                scripts.extend(request.evaluator.checks.clone());
            }
            TrialExecution::Recovery {
                recovery_id,
                version,
            } => {
                let r = self.store.recovery(recovery_id)?;
                if r.version != *version || r.status == RecoveryStatus::Retired {
                    return Err(Error::Intervention(
                        "Recovery changed after planning; create a fresh plan".into(),
                    ));
                }
                let source = self.store.chaos_trial(&r.source_trial)?;
                let campaign = self.store.campaign(&source.campaign_id)?;
                if let Some(kind) = campaign.plan.fixture {
                    self.verify_fixture(&campaign.plan.starting_state, kind)?;
                } else {
                    return Err(Error::InvalidInput(
                        "Recovery runtime requires a supported local fixture".into(),
                    ));
                }
                scripts.extend(campaign.plan.command.args);
                scripts.extend(campaign.plan.evaluation.checks);
                for s in r.steps {
                    if let RecoveryStep::ShellCommand { command } = s {
                        scripts.extend(command.args);
                    }
                }
            }
            TrialExecution::Reflex {
                reflex_id,
                version,
                conditions,
            } => {
                let r = self.store.reflex(reflex_id)?;
                if r.version != *version || r.status == ReflexStatus::Retired {
                    return Err(Error::Intervention(
                        "Reflex changed after planning; create a fresh plan".into(),
                    ));
                }
                for p in conditions {
                    p.validate()?;
                }
                let source = self.store.chaos_trial(&r.source_trial)?;
                let campaign = self.store.campaign(&source.campaign_id)?;
                if let Some(kind) = campaign.plan.fixture {
                    self.verify_fixture(&campaign.plan.starting_state, kind)?;
                } else {
                    return Err(Error::InvalidInput(
                        "Reflex runtime requires a supported local fixture".into(),
                    ));
                }
                scripts.extend(campaign.plan.command.args);
                scripts.extend(campaign.plan.evaluation.checks);
            }
        }
        let forbidden=regex::Regex::new(r"(?i)(\bgit\s+push\b|\b(sendmail|aws|gcloud|az|terraform)\b|\bsend[ -]email\b|\bcharge[ -](card|api)\b)").map_err(|e|Error::InvalidInput(e.to_string()))?;
        for script in scripts {
            if forbidden.is_match(&script)
                || self
                    .config
                    .bridge
                    .policy
                    .blocked_shell_commands
                    .iter()
                    .chain(&self.config.bridge.policy.approval_shell_commands)
                    .any(|s| s.trim() == script.trim())
            {
                return Err(Error::Intervention("Curriculum requires unsupported external effects or explicit approval; unattended work cannot grant approval".into()));
            }
        }
        Ok(())
    }
    pub(crate) fn verify_fixture(&self, state: &StateRef, kind: FixtureKind) -> Result<()> {
        // Git verifies exact commits at fork. For new hardening fixtures also pin all executable
        // files: a user-modified marker alone must not opt arbitrary code into an agent request.
        if matches!(
            kind,
            FixtureKind::SkillHardening | FixtureKind::SkillHardeningTransfer
        ) {
            for (name, body) in crate::resilience::fixture::hardening_files(kind) {
                if !name.ends_with(".sh") {
                    continue;
                }
                let output = std::process::Command::new("git")
                    .arg("-C")
                    .arg(&state.repo_path)
                    .args(["show", &format!("{}:{name}", state.git_commit)])
                    .output()?;
                if !output.status.success() || output.stdout != body.as_bytes() {
                    return Err(Error::Intervention("Hardening fixture executable differs from the bundled deterministic contract".into()));
                }
            }
        }
        Ok(())
    }
    pub async fn run(&self, id: &CurriculumId, external: &Cancellation) -> Result<Curriculum> {
        let _lease = lock(self.store, "curriculum-executor.lock")?;
        let mut c = self.store.curriculum(id)?;
        if c.status.terminal() {
            return Ok(c);
        }
        if c.status == CurriculumStatus::Running {
            return Err(Error::Intervention("Running curriculum cannot be resumed automatically; inspect linked engine records and create a new plan".into()));
        }
        let started = Instant::now();
        let cancel = Cancellation::default();
        if external.is_cancelled() || self.store.curriculum_cancel_requested(id)? {
            cancel.cancel();
        }
        c.status = CurriculumStatus::Running;
        self.save(&mut c)?;
        let duration = c.budget.max_duration_ms.unwrap_or(300_000);
        let result = {
            let work = self.run_inner(&mut c, &cancel);
            tokio::pin!(work);
            loop {
                tokio::select! {
                    result=&mut work=>break result,
                    _=tokio::time::sleep(Duration::from_millis(20))=>{
                        if external.is_cancelled() || started.elapsed().as_millis()>=duration as u128 || self.store.curriculum_cancel_requested(id).unwrap_or(true) {cancel.cancel();}
                    }
                }
            }
        };
        // The in-flight engine was awaited, including process teardown and Reality cleanup.
        c.usage.duration_ms = started.elapsed().as_millis() as u64;
        if cancel.is_cancelled() {
            c.status = CurriculumStatus::Cancelled;
            c.stop_reason = Some(
                "Cancellation or aggregate duration limit; in-flight engine cleanup was awaited"
                    .into(),
            );
        } else if let Err(error) = result {
            c.status = CurriculumStatus::PartiallyCompleted;
            c.quality = CurriculumQuality::Invalid;
            c.stop_reason = Some(error.to_string());
        } else if c.trials.iter().any(|t| t.status != GoalStatus::Completed) {
            c.status = CurriculumStatus::PartiallyCompleted;
        } else {
            c.status = CurriculumStatus::Completed;
        }
        match inventory(self.store, &c.target, &c.profile, &self.config.curriculum) {
            Ok(ctx) => {
                c.after = ctx.packages;
                for p in &c.after {
                    self.store.save_skill_package(p)?;
                }
            }
            Err(e) => {
                c.stop_reason = Some(format!("Evidence retained; package refresh failed: {e}"));
                if c.status != CurriculumStatus::Cancelled {
                    c.status = CurriculumStatus::PartiallyCompleted;
                }
            }
        }
        self.save(&mut c)?;
        Ok(c)
    }
    async fn run_inner(&self, c: &mut Curriculum, cancel: &Cancellation) -> Result<()> {
        self.validate(c)?;
        let mut index = 0;
        loop {
            while index < c.trials.len() {
                if cancel.is_cancelled() {
                    return Ok(());
                }
                let mut t = c.trials[index].clone();
                self.validate_trial(&t)?;
                if c.reserved.realities + t.estimated_budget.realities > c.budget.max_realities
                    || c.reserved.agent_runs + t.estimated_budget.agent_runs
                        > c.budget.max_agent_runs
                    || c.trials_executed >= c.budget.max_curriculum_trials.unwrap_or(0)
                {
                    c.stop_reason = Some("Aggregate budget exhausted".into());
                    return Ok(());
                }
                t.status = GoalStatus::Running;
                c.trials[index] = t.clone();
                c.trials_executed += 1;
                c.reserved.realities += t.estimated_budget.realities;
                c.reserved.agent_runs += t.estimated_budget.agent_runs;
                if let Some(g) = c.goals.iter_mut().find(|g| g.id == t.goal_id) {
                    g.status = GoalStatus::Running;
                }
                self.save(c)?;
                let result = self.execute_trial(&t, cancel).await;
                let (evidence, learning) = self.evidence(&t)?;
                t.status = if result.is_ok()
                    && evidence
                        .outcome
                        .is_some_and(|o| o != ChaosTrialOutcome::Inconclusive)
                {
                    GoalStatus::Completed
                } else {
                    GoalStatus::Inconclusive
                };
                c.usage.realities += evidence.experiences.len();
                if !matches!(t.execution, TrialExecution::Experiment { .. }) {
                    c.usage.agent_runs += evidence.experiences.len();
                }
                for id in &evidence.experiences {
                    c.usage.commands += self.store.experience(id)?.actions.len();
                }
                t.result = Some(evidence);
                t.learning_outcome = Some(learning);
                if let Some(g) = c.goals.iter_mut().find(|g| g.id == t.goal_id) {
                    g.status = t.status;
                }
                c.trials[index] = t.clone();
                self.save(c)?;
                if t.condition.starts_with("contradiction:")
                    && let Some(id) = t.condition.split(':').nth(1)
                {
                    self.store.mark_curriculum_review(&id.parse()?,&t.id,"Compared recorded contexts. Inspect linked Experiments and consider a narrower scope; no Lesson scope or confidence was automatically rewritten")?;
                }
                result?;
                index += 1;
            }
            if c.rounds >= c.max_rounds || cancel.is_cancelled() {
                break;
            }
            c.rounds += 1;
            let new_recoveries: Vec<_> = c
                .trials
                .iter()
                .filter_map(|t| t.learning_outcome.as_ref())
                .flat_map(|o| o.recoveries_created.clone())
                .collect();
            if new_recoveries.is_empty() {
                break;
            }
            let mut ctx = inventory(self.store, &c.target, &c.profile, &self.config.curriculum)?;
            ctx.config.max_rounds = 1;
            let mut remaining = c.budget.clone();
            remaining.max_realities = remaining.max_realities.saturating_sub(c.reserved.realities);
            remaining.max_agent_runs = remaining
                .max_agent_runs
                .saturating_sub(c.reserved.agent_runs);
            remaining.max_curriculum_trials = Some(
                remaining
                    .max_curriculum_trials
                    .unwrap_or(0)
                    .saturating_sub(c.trials_executed),
            );
            if remaining.max_curriculum_trials == Some(0)
                || remaining.max_realities < 2
                || remaining.max_agent_runs < 2
            {
                break;
            }
            let next = DeterministicCurriculumPlanner.plan(&c.target, &ctx, &remaining)?;
            for mut t in next.trials {
                if !matches!(&t.execution,TrialExecution::Recovery {recovery_id,..} if new_recoveries.contains(recovery_id))
                {
                    continue;
                }
                t.round = c.rounds;
                if let Some(g) = next.goals.iter().find(|g| g.id == t.goal_id) {
                    c.goals.push(g.clone());
                }
                c.trials.push(t);
            }
            self.save(c)?;
            if index == c.trials.len() {
                break;
            }
        }
        Ok(())
    }
    async fn execute_trial(&self, t: &CurriculumTrial, cancel: &Cancellation) -> Result<()> {
        // Chaos/reflex/recovery run serial arms, holding one shared provider slot throughout.
        let _capacity = if matches!(t.execution, TrialExecution::Experiment { .. }) {
            None
        } else {
            let mut lease = None;
            for n in 0..self.config.experiments.provider_capacity {
                if let Ok(f) = lock(self.store, &format!("experiment-capacity-{n}.lock")) {
                    lease = Some(f);
                    break;
                }
            }
            Some(lease.ok_or_else(|| {
                Error::Intervention(
                    "Provider capacity exhausted before curriculum Reality creation".into(),
                )
            })?)
        };
        match &t.execution {
            TrialExecution::Experiment { request } => {
                let engine = ExperimentOrchestrator {
                    store: self.store,
                    config: self.config,
                };
                let exp = engine.submit((**request).clone())?;
                self.store
                    .link_curriculum_engine(&t.id, "experiment", &exp.id.to_string())?;
                let exp = engine.execute(&exp.id, cancel).await?;
                if exp.status != ExperimentStatus::Completed {
                    return Err(Error::Intervention(
                        exp.failure
                            .unwrap_or_else(|| "Experiment did not complete".into()),
                    ));
                }
            }
            TrialExecution::Chaos { plan } => {
                let observer = |e: &campaign::CampaignEvent| {
                    if let campaign::CampaignEvent::ChaosCampaignStarted { campaign_id } = e {
                        self.store.link_curriculum_engine(
                            &t.id,
                            "chaos",
                            &campaign_id.to_string(),
                        )?;
                    }
                    Ok(())
                };
                let c =
                    campaign::run_observed(self.store, (**plan).clone(), cancel, Some(&observer))
                        .await?;
                if c.result != CampaignStatus::Completed {
                    return Err(Error::Intervention(c.stop_reason.unwrap_or_else(|| {
                        "Unhealthy control or incomplete campaign".into()
                    })));
                }
            }
            TrialExecution::Recovery { recovery_id, .. } => {
                let observer = |test: &ResilienceTest| {
                    self.store.link_curriculum_engine(
                        &t.id,
                        "resilience_test",
                        &test.id.to_string(),
                    )
                };
                testing::curriculum_test(
                    self.store,
                    None,
                    Some(self.store.recovery(recovery_id)?),
                    None,
                    cancel,
                    &observer,
                )
                .await?;
            }
            TrialExecution::Reflex {
                reflex_id,
                conditions,
                ..
            } => {
                let observer = |test: &ResilienceTest| {
                    self.store.link_curriculum_engine(
                        &t.id,
                        "resilience_test",
                        &test.id.to_string(),
                    )
                };
                testing::curriculum_test(
                    self.store,
                    Some(self.store.reflex(reflex_id)?),
                    None,
                    Some(conditions.clone()),
                    cancel,
                    &observer,
                )
                .await?;
            }
        }
        Ok(())
    }
    fn evidence(&self, t: &CurriculumTrial) -> Result<(TrialEvidence, LearningOutcome)> {
        let mut e = TrialEvidence::default();
        let mut l = LearningOutcome::default();
        if let Some((kind, id)) = self.store.curriculum_engine_link(&t.id)? {
            match kind.as_str() {
                "experiment" => {
                    let exp = self.store.strategy_experiment(&id.parse()?)?;
                    e.experiment_id = Some(exp.id);
                    e.reason = exp.failure;
                    if let Some(r) = exp.result {
                        e.experiences = r.created_experience;
                        l.lessons_created = r.candidate_lessons;
                        e.outcome = Some(
                            if r.candidates.is_empty()
                                || r.candidates.iter().any(|c| {
                                    matches!(
                                        c.execution_status,
                                        ProcessStatus::Interrupted | ProcessStatus::TimedOut
                                    )
                                })
                            {
                                ChaosTrialOutcome::Inconclusive
                            } else if r.candidates.iter().all(|c| {
                                c.evaluation.success
                                    && c.execution_status == ProcessStatus::Succeeded
                            }) {
                                ChaosTrialOutcome::Pass
                            } else {
                                ChaosTrialOutcome::Fail
                            },
                        );
                    }
                }
                "chaos" => {
                    let c = self.store.campaign(&id.parse()?)?;
                    e.campaign_id = Some(c.id);
                    e.reason = c.stop_reason;
                    e.experiences = c
                        .control
                        .iter()
                        .chain(c.trials.iter())
                        .map(|t| t.experience_id.clone())
                        .collect();
                    e.outcome = c.trials.last().map(|t| t.outcome);
                    if let Some(id) = c.envelope_id {
                        l.envelope_updates.push(id);
                    }
                    for t in c.trials {
                        l.lessons_created.extend(t.lessons);
                        l.reflexes_created.extend(t.reflexes);
                        l.recoveries_created.extend(t.recoveries);
                    }
                }
                "resilience_test" => {
                    let test = self
                        .store
                        .resilience_tests()?
                        .into_iter()
                        .find(|t| t.id.to_string() == id)
                        .ok_or_else(|| Error::NotFound("Linked resilience test missing".into()))?;
                    e.resilience_test_id = Some(test.id);
                    e.reason = Some(test.reason);
                    e.experiences = test.without.into_iter().chain(test.with).collect();
                    e.outcome = Some(match test.status {
                        ResilienceTestStatus::Supported
                        | ResilienceTestStatus::NegativeControlPassed => ChaosTrialOutcome::Pass,
                        ResilienceTestStatus::FalsePositive
                        | ResilienceTestStatus::Contradicted => ChaosTrialOutcome::Fail,
                        _ => ChaosTrialOutcome::Inconclusive,
                    });
                }
                _ => {
                    return Err(Error::InvalidInput(
                        "Unknown curriculum engine reference".into(),
                    ));
                }
            }
        }
        l.new_experiences = e.experiences.clone();
        Ok((e, l))
    }
}
