// SPDX-License-Identifier: Apache-2.0
use super::{runtime::RunResilienceOptions, *};
use crate::{
    Error, Result,
    application::{ExperienceRelation, RunLearningOptions},
    cancellation::Cancellation,
    core::*,
    perturbation::Perturbation,
    store::Store,
    workflow::run_with_resilience,
};
use chrono::Utc;

pub async fn test_reflex(
    store: &Store,
    id: &ReflexId,
    conditions: Option<Vec<Perturbation>>,
    cancel: &Cancellation,
) -> Result<ResilienceTest> {
    let reflex = store.reflex(id)?;
    if reflex.status == ReflexStatus::Retired {
        return Err(Error::Intervention(
            "Retired Reflex cannot be tested".into(),
        ));
    }
    test(store, Some(reflex), None, conditions, cancel).await
}
pub async fn test_recovery(
    store: &Store,
    id: &RecoveryId,
    cancel: &Cancellation,
) -> Result<ResilienceTest> {
    let recovery = store.recovery(id)?;
    if recovery.status == RecoveryStatus::Retired {
        return Err(Error::Intervention(
            "Retired Recovery cannot be tested".into(),
        ));
    }
    test(store, None, Some(recovery), None, cancel).await
}
async fn test(
    store: &Store,
    reflex: Option<Reflex>,
    recovery: Option<Recovery>,
    conditions: Option<Vec<Perturbation>>,
    cancel: &Cancellation,
) -> Result<ResilienceTest> {
    let source_trial = reflex
        .as_ref()
        .map(|r| r.source_trial.clone())
        .or_else(|| recovery.as_ref().map(|r| r.source_trial.clone()))
        .ok_or_else(|| Error::InvalidInput("Test needs a target".into()))?;
    let trial = store.chaos_trial(&source_trial)?;
    let campaign = store.campaign(&trial.campaign_id)?;
    let mut test = ResilienceTest {
        id: ResilienceTestId::new(),
        reflex_id: reflex.as_ref().map(|r| r.id.clone()),
        recovery_id: recovery.as_ref().map(|r| r.id.clone()),
        source_trial,
        perturbations: conditions.unwrap_or(trial.perturbations),
        without: None,
        with: None,
        status: ResilienceTestStatus::Running,
        false_positive: None,
        created_at: Utc::now(),
        reason: "Paired local replay is running".into(),
    };
    if test.perturbations.is_empty() || test.perturbations.len() > 16 {
        return Err(Error::InvalidInput(
            "Replay requires 1..16 perturbations".into(),
        ));
    }
    store.register_perturbations(&test.perturbations)?;
    store.save_resilience_test(&test, true)?;
    let result:Result<()> = async {
        let options=RunResilienceOptions{perturbations:test.perturbations.clone(),fixture:campaign.plan.fixture,baseline:campaign.control.as_ref().map(|c|c.metrics.clone()),..Default::default()};
        let learning=RunLearningOptions{relations:vec![ExperienceRelation::CounterfactualOf(trial.experience_id.clone())],..Default::default()};
        let without=run_with_resilience(store,campaign::request(&campaign.plan),&learning,&options,cancel).await?;
        test.without=Some(without.experience.id.clone());store.save_resilience_test(&test,false)?;
        if matches!(without.experience.outcome,crate::experience::Outcome::Interrupted|crate::experience::Outcome::TimedOut) || cancel.is_cancelled() { test.status=ResilienceTestStatus::Inconclusive;test.reason="Baseline replay interrupted or timed out".into();return Ok(()) }
        if recovery.is_some() && !without.experience.resilience.as_ref().is_some_and(|r|r.outcome==ChaosTrialOutcome::Fail) { test.status=ResilienceTestStatus::Inconclusive;test.reason="Failure-only replay did not reproduce a failed task; restoration was not attempted".into();return Ok(()) }
        let options=RunResilienceOptions{reflexes:reflex.clone().into_iter().collect(),testing_reflex:reflex.is_some(),recovery:recovery.clone(),..options};
        let learning=RunLearningOptions{relations:vec![if recovery.is_some(){ExperienceRelation::RecoveryOf(without.experience.id.clone())}else{ExperienceRelation::CounterfactualOf(without.experience.id.clone())}],..Default::default()};
        let with=run_with_resilience(store,campaign::request(&campaign.plan),&learning,&options,cancel).await?;
        test.with=Some(with.experience.id.clone());
        let b=without.experience.resilience.as_ref().ok_or_else(||Error::InvalidInput("Missing replay observation".into()))?;
        let a=with.experience.resilience.as_ref().ok_or_else(||Error::InvalidInput("Missing response observation".into()))?;
        if without.experience.starting_state!=with.experience.starting_state || without.experience.context.environment.fingerprint!=with.experience.context.environment.fingerprint || without.experience.evaluation.spec!=with.experience.evaluation.spec || b.perturbation_ids!=a.perturbation_ids {return Err(Error::Intervention("Paired replay conditions differ".into()))}
        if reflex.is_some() {
            let fired=!a.reflex_matches.is_empty();
            // The negative fixture has identical prefixes through the trigger point:
            // its next original action succeeds. Only call that a false positive if
            // the persisted without-arm trace demonstrates that exact action/context.
            let false_positive=a.reflex_matches.first().is_some_and(|m| b.temporal.iter().any(|t| !t.failed && t.attempt==m.observed.consecutive_failures+1 && t.action==m.observed.proposed_action && t.state_before==m.observed.state_fingerprint)) && matches!(b.outcome,ChaosTrialOutcome::Pass|ChaosTrialOutcome::Degraded);
            test.false_positive=fired.then_some(false_positive);
            test.status=if false_positive{ResilienceTestStatus::FalsePositive}else if fired && b.outcome==ChaosTrialOutcome::Fail && matches!(a.outcome,ChaosTrialOutcome::Pass|ChaosTrialOutcome::Degraded){ResilienceTestStatus::Supported}else if fired && matches!(b.outcome,ChaosTrialOutcome::Pass|ChaosTrialOutcome::Degraded) && a.outcome==ChaosTrialOutcome::Fail{ResilienceTestStatus::Contradicted}else{ResilienceTestStatus::Inconclusive};
            test.reason=match test.status{ResilienceTestStatus::Supported=>"Without Reflex failed; with matched Reflex replanned and passed",ResilienceTestStatus::FalsePositive=>"Reflex fired, but the next original action under the same observed prefix succeeded without it",ResilienceTestStatus::Contradicted=>"Reflex response failed where the original behavior succeeded",_=>"Paired evidence did not establish useful Reflex influence"}.into();
        } else {
            let recovered=a.recovery_attempt.as_ref().is_some_and(|r|r.reproduced_failure&&r.attempted&&r.succeeded);
            let contradicted=a.recovery_attempt.as_ref().is_some_and(|r|r.reproduced_failure&&r.attempted&&!r.succeeded) && a.outcome==ChaosTrialOutcome::Fail;
            test.status=if b.outcome==ChaosTrialOutcome::Fail&&recovered{ResilienceTestStatus::Supported}else if contradicted{ResilienceTestStatus::Contradicted}else{ResilienceTestStatus::Inconclusive};
            test.reason=if recovered{"Failure reproduced and checked in the response Reality; recovery steps and final evaluation passed"}else{"Recovery was unsuccessful, not attempted, or failure could not be reproduced"}.into();
        }
        Ok(())
    }.await;
    if let Err(error) = &result {
        test.status = ResilienceTestStatus::Failed;
        test.reason = error.to_string();
    }
    store.finish_resilience_test(&test)?;
    result?;
    Ok(test)
}
