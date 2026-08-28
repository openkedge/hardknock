// SPDX-License-Identifier: Apache-2.0
use super::{runtime::RunResilienceOptions, *};
use crate::{
    Error, Result,
    application::{ExperienceRelation, RunLearningOptions},
    cancellation::Cancellation,
    core::*,
    experience::ReplaySpec,
    lesson::{
        ActionPattern, ContextSelector, EvidenceRef, EvidenceRelationship, HeuristicConfidence,
        Lesson,
    },
    reflection::CandidateHypothesis,
    store::{LessonStore, Store},
    workflow::{RunRequest, run_with_resilience},
};
use chrono::Utc;
use std::time::Instant;

#[derive(serde::Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum CampaignEvent {
    ChaosCampaignStarted {
        campaign_id: ChaosCampaignId,
    },
    ChaosTrialStarted {
        trial_id: ChaosTrialId,
        control: bool,
    },
    ChaosTrialCompleted {
        trial_id: ChaosTrialId,
        experience_id: ExperienceId,
        outcome: ChaosTrialOutcome,
    },
    OperatingEnvelopeUpdated {
        envelope_id: OperatingEnvelopeId,
    },
    ReflexCreated {
        reflex_id: ReflexId,
        status: ReflexStatus,
    },
    RecoveryCreated {
        recovery_id: RecoveryId,
        status: RecoveryStatus,
    },
}
pub type CampaignObserver<'a> = dyn Fn(&CampaignEvent) -> Result<()> + 'a;

pub fn request(plan: &CampaignPlan) -> RunRequest {
    RunRequest {
        state: plan.starting_state.clone(),
        goal: plan.goal.clone(),
        agent: plan.agent.clone(),
        command: plan.command.clone(),
        evaluation: plan.evaluation.clone(),
        timeout_secs: plan.timeout_secs,
        keep: false,
        replay: Some(ReplaySpec {
            script: if plan.fixture.is_some() {
                "/bin/sh ./operation.sh".into()
            } else {
                plan.command.args.get(1).cloned().unwrap_or_default()
            },
            timeout_secs: plan.timeout_secs,
        }),
        perturbations: vec![],
        expected_fingerprint: Some(plan.environment.fingerprint.clone()),
    }
}
pub fn validate(plan: &CampaignPlan) -> Result<()> {
    plan.evaluation.validate()?;
    if plan.evaluation.checks.is_empty()
        || plan.trial_budget == 0
        || plan.trial_budget > 100
        || plan.perturbations.len() > 100
        || plan.perturbations.is_empty()
        || plan.timeout_secs == 0
        || plan.timeout_secs > 3600
        || plan.max_duration_secs == 0
        || plan.max_duration_secs > 3600
        || plan.command.environment != EnvironmentMode::Controlled
    {
        return Err(Error::InvalidInput("Chaos requires checks, 1..100 conditions/trial budget, controlled environment, and 1..3600 second time budgets".into()));
    }
    for condition in &plan.perturbations {
        if condition.is_empty() || condition.len() > 16 {
            return Err(Error::InvalidInput(
                "A trial requires 1..16 perturbations".into(),
            ));
        }
        for p in condition {
            p.validate()?;
        }
    }
    Ok(())
}
pub async fn run(
    store: &Store,
    plan: CampaignPlan,
    cancel: &Cancellation,
) -> Result<ChaosCampaign> {
    run_observed(store, plan, cancel, None).await
}

pub async fn run_observed(
    store: &Store,
    plan: CampaignPlan,
    cancel: &Cancellation,
    observer: Option<&CampaignObserver<'_>>,
) -> Result<ChaosCampaign> {
    validate(&plan)?;
    let started = Instant::now();
    let mut campaign = ChaosCampaign {
        id: ChaosCampaignId::new(),
        plan,
        control: None,
        trials: vec![],
        result: CampaignStatus::Running,
        stop_reason: None,
        envelope_id: None,
        created_at: Utc::now(),
        completed_at: None,
    };
    store.insert_campaign(&campaign)?;
    // Diagnostics cannot invalidate evidence or strand an already persisted plan.
    let emit = |event| {
        if let Some(observer) = observer
            && let Err(error) = observer(&event)
        {
            tracing::warn!(%error, "Cannot emit campaign progress");
        }
    };
    emit(CampaignEvent::ChaosCampaignStarted {
        campaign_id: campaign.id.clone(),
    });
    let result: Result<()> = async {
        for index in 0..=campaign
            .plan
            .perturbations
            .len()
            .min(campaign.plan.trial_budget)
        {
            if cancel.is_cancelled() {
                campaign.result = CampaignStatus::Interrupted;
                campaign.stop_reason = Some("Cancelled before the next trial".into());
                break;
            }
            let remaining = campaign
                .plan
                .max_duration_secs
                .saturating_sub(started.elapsed().as_secs());
            if remaining == 0 {
                campaign.result = CampaignStatus::BudgetExhausted;
                campaign.stop_reason =
                    Some("Campaign duration budget reached; no further trials started".into());
                break;
            }
            let origin = ChaosOrigin {
                campaign_id: campaign.id.clone(),
                trial_id: ChaosTrialId::new(),
                control: campaign.control.as_ref().map(|c| c.experience_id.clone()),
                index,
            };
            emit(CampaignEvent::ChaosTrialStarted {
                trial_id: origin.trial_id.clone(),
                control: index == 0,
            });
            let options = RunResilienceOptions {
                origin: Some(origin.clone()),
                perturbations: if index == 0 {
                    vec![]
                } else {
                    campaign.plan.perturbations[index - 1].clone()
                },
                fixture: campaign.plan.fixture,
                baseline: campaign.control.as_ref().map(|c| c.metrics.clone()),
                reflexes: campaign.plan.active_reflexes.clone(),
                ..Default::default()
            };
            let learning = RunLearningOptions {
                relations: origin
                    .control
                    .iter()
                    .cloned()
                    .map(ExperienceRelation::ChaosVariantOf)
                    .collect(),
                ..Default::default()
            };
            let mut request = request(&campaign.plan);
            request.timeout_secs = request.timeout_secs.min(remaining);
            run_with_resilience(store, request, &learning, &options, cancel).await?;
            let mut trial = store.chaos_trial(&origin.trial_id)?;
            emit(CampaignEvent::ChaosTrialCompleted {
                trial_id: trial.id.clone(),
                experience_id: trial.experience_id.clone(),
                outcome: trial.outcome,
            });
            if index == 0 {
                let healthy = trial.outcome == ChaosTrialOutcome::Pass;
                campaign.control = Some(trial);
                if !healthy {
                    campaign.result = if cancel.is_cancelled() {
                        CampaignStatus::Interrupted
                    } else {
                        CampaignStatus::UnhealthyControl
                    };
                    campaign.stop_reason = Some(
                        "Target is not healthy under control conditions; no perturbations were run"
                            .into(),
                    );
                    break;
                }
            } else {
                if trial.outcome == ChaosTrialOutcome::Fail
                    && let Some(fixture) = campaign.plan.fixture
                {
                    derive_candidates(store, &trial, fixture)?;
                    trial = store.chaos_trial(&trial.id)?;
                    for id in &trial.reflexes {
                        emit(CampaignEvent::ReflexCreated {
                            reflex_id: id.clone(),
                            status: ReflexStatus::Candidate,
                        });
                    }
                    for id in &trial.recoveries {
                        emit(CampaignEvent::RecoveryCreated {
                            recovery_id: id.clone(),
                            status: RecoveryStatus::Candidate,
                        });
                    }
                }
                let inconclusive = trial.outcome == ChaosTrialOutcome::Inconclusive;
                campaign.trials.push(trial);
                if inconclusive {
                    campaign.result = if cancel.is_cancelled() {
                        CampaignStatus::Interrupted
                    } else {
                        CampaignStatus::Failed
                    };
                    campaign.stop_reason = Some(
                        "Trial was interrupted or timed out; observations remain inconclusive"
                            .into(),
                    );
                    break;
                }
            }
            store.update_campaign(&campaign, None)?;
        }
        if campaign.result == CampaignStatus::Running {
            campaign.result = if campaign.trials.len() < campaign.plan.perturbations.len() {
                CampaignStatus::BudgetExhausted
            } else {
                CampaignStatus::Completed
            };
            if campaign.result == CampaignStatus::BudgetExhausted {
                campaign.stop_reason = Some("Perturbed-trial budget reached".into());
            }
        }
        Ok(())
    }
    .await;
    if let Err(error) = &result {
        campaign.result = if cancel.is_cancelled() {
            CampaignStatus::Interrupted
        } else {
            CampaignStatus::Failed
        };
        campaign.stop_reason = Some(error.to_string());
    }
    // A failure deriving metadata must not hide a trial whose Experience committed.
    let recorded = store.campaign(&campaign.id)?;
    campaign.control = recorded.control;
    campaign.trials = recorded.trials;
    campaign.completed_at = Some(Utc::now());
    let envelope = if !campaign.trials.is_empty() {
        Some(envelope(&campaign))
    } else {
        None
    };
    campaign.envelope_id = envelope.as_ref().map(|e| e.id.clone());
    store.update_campaign(&campaign, envelope.as_ref())?;
    if let Some(envelope) = envelope {
        emit(CampaignEvent::OperatingEnvelopeUpdated {
            envelope_id: envelope.id,
        });
    }
    result?;
    Ok(campaign)
}
fn envelope(campaign: &ChaosCampaign) -> OperatingEnvelope {
    let mut envelope = OperatingEnvelope {
        id: OperatingEnvelopeId::new(),
        version: 1,
        target: campaign.plan.target.clone(),
        campaign_id: campaign.id.clone(),
        tested_conditions: vec![],
        safe_regions: vec![],
        degraded_regions: vec![],
        failure_regions: vec![],
        unknown_regions: vec![ConditionRange::AllUntestedConditions],
        evidence: vec![],
        updated_at: Utc::now(),
    };
    for trial in &campaign.trials {
        envelope.tested_conditions.push(EnvelopeCondition {
            perturbations: trial.perturbations.clone(),
            trial_id: trial.id.clone(),
            experience_id: trial.experience_id.clone(),
            outcome: trial.outcome,
        });
        let point = ConditionRange::TestedPoint {
            trial_id: trial.id.clone(),
        };
        match trial.outcome {
            ChaosTrialOutcome::Pass => envelope.safe_regions.push(point),
            ChaosTrialOutcome::Degraded => envelope.degraded_regions.push(point),
            ChaosTrialOutcome::Fail => envelope.failure_regions.push(point),
            ChaosTrialOutcome::Inconclusive => envelope.unknown_regions.push(point),
        }
        envelope.evidence.push(EvidenceRef::Experience {
            experience_id: trial.experience_id.clone(),
            relationship: EvidenceRelationship::Origin,
        });
    }
    envelope
}
fn derive_candidates(store: &Store, trial: &ChaosTrial, fixture: FixtureKind) -> Result<()> {
    let exp = store.experience(&trial.experience_id)?;
    let signature = exp.failure_signatures.iter().find(|s| {
        matches!(
            s.signature.as_str(),
            "retry_exhaustion"
                | "stale_credential"
                | "configuration_stale"
                | "transient_command_failure"
        )
    });
    let Some(signature) = signature else {
        return Ok(());
    };
    let mut scope = ContextSelector::from_context(&exp.context);
    scope.tags = vec![format!("fixture-kind:{}", fixture.name())];
    let now = Utc::now();
    let hypothesis=CandidateHypothesis { id:HypothesisId::new(),source_experience:exp.id.clone(),created_at:now,claim:format!("In {} under the recorded local conditions {}, {} occurred; the fixture recovery may help",fixture.name(),serde_json::to_string(&trial.perturbations)?,signature.signature),rationale:"One induced failure proposes a scoped hypothesis, not a general prohibition. Test a paired response before promotion.".into(),context_match:scope.clone(),avoid:ActionPattern::shell("/bin/sh ./operation.sh"),prefer:ActionPattern::shell("/bin/sh ./replan.sh"),generated_by:AgentIdentity{kind:"local-chaos-reflection".into(),executable:"hardknock".into(),version:Some(super::fixture::RUNTIME_VERSION.into()),model:None} };
    store.insert_hypothesis(&hypothesis)?;
    let lesson = Lesson::candidate(&hypothesis, &HeuristicConfidence);
    LessonStore::insert(store, &lesson)?;
    store.link_chaos_lesson(&trial.id, &lesson.id)?;
    let evidence = vec![EvidenceRef::Experience {
        experience_id: exp.id.clone(),
        relationship: EvidenceRelationship::Origin,
    }];
    if matches!(
        fixture,
        FixtureKind::RetryResilience | FixtureKind::ConfigDrift
    ) {
        let reflex = Reflex {
            id: ReflexId::new(),
            version: 1,
            source_lessons: vec![lesson.id],
            source_trial: trial.id.clone(),
            trigger: TriggerPattern {
                context: scope.clone(),
                proposed_action: super::reflex::fixture_action(),
                repeated_failures: if fixture == FixtureKind::ConfigDrift {
                    None
                } else {
                    Some(3)
                },
                no_state_change: fixture != FixtureKind::ConfigDrift,
                config_changed: fixture == FixtureKind::ConfigDrift,
            },
            response: ReflexResponse::Replan,
            confidence: 0.58.try_into()?,
            status: ReflexStatus::Candidate,
            evidence: evidence.clone(),
            created_at: now,
            updated_at: now,
        };
        store.insert_reflex(&reflex)?;
    }
    let shell = |s: &str| RecoveryStep::ShellCommand {
        command: CommandSpec::shell(s, EnvironmentMode::Controlled),
    };
    let steps = match fixture {
        FixtureKind::RetryResilience => vec![RecoveryStep::Replan],
        FixtureKind::ConfigDrift => vec![shell("/bin/sh ./read-state.sh"), RecoveryStep::Replan],
        FixtureKind::StaleCredential => vec![
            shell("/bin/sh ./refresh-token.sh"),
            RecoveryStep::SetEnvironmentVariable {
                key: "HK_TOKEN_STATE".into(),
                value: "VALID_TOKEN".into(),
            },
            shell("/bin/sh ./read-state.sh"),
            RecoveryStep::Replan,
        ],
    };
    let recovery = Recovery {
        id: RecoveryId::new(),
        version: 1,
        source_trial: trial.id.clone(),
        failure_signature: FailureSignaturePattern {
            signature: signature.signature.clone(),
        },
        context: scope,
        steps,
        status: RecoveryStatus::Candidate,
        confidence: 0.42.try_into()?,
        evidence,
        created_at: now,
        updated_at: now,
    };
    store.insert_recovery(&recovery)
}
