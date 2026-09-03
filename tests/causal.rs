// SPDX-License-Identifier: Apache-2.0
mod support;
use hardknock::{
    bridge::config::Config, cancellation::Cancellation, causal::*, core::*, curriculum::Severity,
    dojo::capture_state, experimentation::ExperimentQuality, runtime::*, store::Store,
};
use std::time::Instant;
use support::Fixture;

fn input(f: &Fixture) -> CausalInvestigationInput {
    benchmark::stale_state_input(capture_state(&f.repo).unwrap())
}
fn var(i: &CausalInvestigationInput, name: &str) -> CausalVariableId {
    i.spec
        .variables
        .iter()
        .find(|v| v.name == name)
        .unwrap()
        .id
        .clone()
}
fn hypothesis(i: &CausalInvestigationInput, name: &str) -> CausalHypothesisId {
    let id = var(i, name);
    i.hypotheses
        .iter()
        .find(|h| h.cause == id)
        .unwrap()
        .id
        .clone()
}
fn add_input(
    i: &mut CausalInvestigationInput,
    name: &str,
    value: VariableValue,
    domain: VariableDomain,
) -> CausalVariableId {
    let id = CausalVariableId::new();
    i.spec.variables.push(CausalVariable {
        id: id.clone(),
        name: name.into(),
        kind: CausalVariableKind::Environment,
        domain,
        observable: true,
        intervenable: false,
    });
    i.spec.baseline.insert(id.clone(), value);
    i.spec
        .bindings
        .insert(id.clone(), format!("{name}.input").into());
    id
}
async fn test_variable(
    store: &Store,
    i: &CausalInvestigationInput,
    inv: &CausalInvestigation,
    name: &str,
) -> serde_json::Value {
    let config = Config::default();
    let plan = store.plan_causal_investigation(&inv.id, &config).unwrap();
    let v = var(i, name);
    let d = plan
        .experiments
        .iter()
        .find(|d| d.intervention.variable == v)
        .unwrap();
    execute_causal_run(
        store,
        &config,
        compile_intervention(&inv.id, &i.spec, d).unwrap(),
        &Cancellation::default(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn stale_state_end_to_end_discriminates_records_models_and_runtime_guidance() {
    let f = Fixture::from_fixture("causal/stale-state");
    let store = Store::open(&f.home).unwrap();
    let i = input(&f);
    let inv = store.create_causal_investigation(&i).unwrap();
    let plan = store
        .plan_causal_investigation(&inv.id, &Config::default())
        .unwrap();
    assert_eq!(
        plan.experiments[0].intervention.variable,
        var(&i, "state_refresh")
    );
    assert_eq!(plan.experiments[0].separated_pairs, 2);
    assert_eq!(plan.experiments[0].intervention.held_constant.len(), 2);
    for name in ["state_refresh", "latency", "retry_count"] {
        let report = test_variable(&store, &i, &inv, name).await;
        assert_eq!(report["pair"]["quality"]["quality"], "controlled");
    }
    assert_eq!(
        store
            .causal_hypothesis(&hypothesis(&i, "state_refresh"))
            .unwrap()
            .status,
        CausalHypothesisStatus::Supported
    );
    for name in ["latency", "retry_count"] {
        assert_eq!(
            store
                .causal_hypothesis(&hypothesis(&i, name))
                .unwrap()
                .status,
            CausalHypothesisStatus::Contradicted
        );
    }
    let models = store.causal_models().unwrap();
    assert_eq!(models.len(), 1);
    assert!(models[0].revision >= 4);
    assert_eq!(
        store.causal_model_history(&models[0].id).unwrap().len(),
        models[0].revision as usize
    );
    let candidate = store
        .propose_causal_refinement(&inv.id, &hypothesis(&i, "state_refresh"))
        .unwrap();
    assert!(candidate.requires_existing_validation);
    assert!(candidate.recovery_guidance.contains("state_refresh=true"));
    assert!(
        store.all_lessons().unwrap().is_empty(),
        "Causal tests must not fabricate Lessons"
    );
    let observations = store.causal_envelope_observations(&inv.id).unwrap();
    assert_eq!(observations.len(), 6);
    assert!(
        observations
            .iter()
            .any(|o| o.conditions["latency"] == VariableValue::Integer(1000)
                && o.outcome == PredictedOutcome::Pass)
    );
    let scenario = RuntimeScenario {
        failure_signature: Some(FailureSignatureRef {
            signature: "retry-exhaustion".into(),
        }),
        ..Default::default()
    };
    let mut ctx = scenario.decision_context().unwrap();
    ctx.query_context.repository.path = i.spec.starting_state.state_ref.repo_path.clone();
    ctx.query_context.repository.commit = i.spec.starting_state.state_ref.git_commit.clone();
    ctx.query_context.environment.os = std::env::consts::OS.into();
    ctx.query_context.environment.arch = std::env::consts::ARCH.into();
    ctx.query_context.environment.facts = i
        .spec
        .baseline
        .iter()
        .map(|(id, v)| {
            (
                i.spec
                    .variables
                    .iter()
                    .find(|v| &v.id == id)
                    .unwrap()
                    .name
                    .clone(),
                v.literal(),
            )
        })
        .collect();
    ctx.causal = store
        .causal_runtime_guidance(&ctx.query_context, Some("retry-exhaustion"))
        .unwrap();
    assert_eq!(ctx.causal.supported_interventions.len(), 1);
    let decision = DeterministicRuntimeController::default()
        .evaluate(&ctx)
        .unwrap();
    assert_eq!(decision.decision.kind(), RuntimeDecisionKind::Replan);
    ctx.query_context.repository.commit = "new-tool-version".into();
    assert!(
        store
            .causal_runtime_guidance(&ctx.query_context, Some("retry-exhaustion"))
            .unwrap()
            .supported_interventions
            .is_empty()
    );
    f.assert_source_unchanged();
}

#[tokio::test]
async fn confounding_and_wrong_agent_explanations_never_promote_lessons() {
    let f = Fixture::from_fixture("causal/confounded-latency");
    let store = Store::open(&f.home).unwrap();
    let mut i = input(&f);
    let tool = add_input(
        &mut i,
        "tool_version",
        VariableValue::Text("v1".into()),
        VariableDomain::Categorical {
            values: vec!["v1".into(), "v2".into()],
        },
    );
    i.spec.known_confounders.push(KnownConfounder {
        variable: tool,
        reason: "Latency and tool release co-varied in observations; tooling not controlled".into(),
        controlled: false,
    });
    let inv = store.create_causal_investigation(&i).unwrap();
    let result = test_variable(&store, &i, &inv, "latency").await;
    assert_eq!(result["pair"]["quality"]["quality"], "confounded");
    assert_eq!(
        store
            .causal_hypothesis(&hypothesis(&i, "latency"))
            .unwrap()
            .status,
        CausalHypothesisStatus::Inconclusive
    );
    assert_eq!(
        assess_quality(
            true,
            true,
            &[CausalVariableId::new(), CausalVariableId::new()],
            &[],
            &[],
            ExperimentQuality::Controlled
        )
        .quality,
        ExperimentQuality::Confounded
    );
    let f = Fixture::from_fixture("causal/wrong-agent-explanation");
    let store = Store::open(&f.home).unwrap();
    let mut i = input(&f);
    let id = var(&i, "latency");
    i.spec
        .variables
        .iter_mut()
        .find(|v| v.id == id)
        .unwrap()
        .name = "memory_pressure".into();
    i.hypotheses[0].statement = "The agent confidently says memory pressure caused this".into();
    let inv = store.create_causal_investigation(&i).unwrap();
    test_variable(&store, &i, &inv, "memory_pressure").await;
    assert_eq!(
        store
            .causal_hypothesis(&hypothesis(&i, "memory_pressure"))
            .unwrap()
            .status,
        CausalHypothesisStatus::Contradicted
    );
    assert!(store.all_lessons().unwrap().is_empty());
}

#[tokio::test]
async fn interaction_requires_qualified_scope_and_does_not_establish_sufficiency() {
    let f = Fixture::from_fixture("causal/interaction");
    let store = Store::open(&f.home).unwrap();
    let mut i = input(&f);
    let latency = var(&i, "latency");
    let retry = var(&i, "retry_count");
    i.hypotheses.retain(|h| h.cause == latency);
    i.hypotheses[0].claim = CausalClaim::SufficientUnderScope;
    let inv = store.create_causal_investigation(&i).unwrap();
    test_variable(&store, &i, &inv, "latency").await;
    assert_eq!(
        store.causal_hypothesis(&i.hypotheses[0].id).unwrap().status,
        CausalHypothesisStatus::Inconclusive
    );
    i.hypotheses[0].id = CausalHypothesisId::new();
    i.hypotheses[0].claim = CausalClaim::Causes;
    i.hypotheses[0].conditions = vec![CausalCondition {
        variable: retry.clone(),
        predicate: VariablePredicate::Equals(VariableValue::Integer(3)),
    }];
    let inv = store.create_causal_investigation(&i).unwrap();
    test_variable(&store, &i, &inv, "latency").await;
    assert_eq!(
        store.causal_hypothesis(&i.hypotheses[0].id).unwrap().status,
        CausalHypothesisStatus::Supported
    );
    // Same latency intervention without retry pressure has no effect.
    i.hypotheses[0].id = CausalHypothesisId::new();
    i.hypotheses[0].conditions.clear();
    i.hypotheses[0].baseline_prediction = PredictedOutcome::Pass;
    i.hypotheses[0].intervention_prediction = PredictedOutcome::Fail;
    i.spec.baseline.insert(retry, VariableValue::Integer(1));
    i.spec
        .baseline
        .insert(latency.clone(), VariableValue::Integer(0));
    i.spec
        .available_interventions
        .iter_mut()
        .find(|c| c.variable == latency)
        .unwrap()
        .supported_values = vec![VariableValue::Integer(1000)];
    let inv = store.create_causal_investigation(&i).unwrap();
    let report = test_variable(&store, &i, &inv, "latency").await;
    assert_eq!(report["evidence"][0]["baseline_outcome"], "pass");
    assert_eq!(report["evidence"][0]["intervention_outcome"], "pass");
    assert_eq!(
        store.causal_hypothesis(&i.hypotheses[0].id).unwrap().status,
        CausalHypothesisStatus::Contradicted
    );
}

#[tokio::test]
async fn scope_contradiction_preserves_history_and_propagates_review() {
    let f = Fixture::from_fixture("causal/invalidation-propagation");
    let store = Store::open(&f.home).unwrap();
    let mut i = input(&f);
    let dependency = add_input(
        &mut i,
        "dependency_available",
        VariableValue::Boolean(true),
        VariableDomain::Boolean,
    );
    let hid = hypothesis(&i, "state_refresh");
    let inv = store.create_causal_investigation(&i).unwrap();
    let report = test_variable(&store, &i, &inv, "state_refresh").await;
    let intervention: InterventionId =
        serde_json::from_value(report["evidence"][0]["intervention"].clone()).unwrap();
    // Use actual stored Runtime decisions as downstream artifacts; other artifact kinds use the same dependency gate.
    use hardknock::store::RuntimeStore;
    let decision = store
        .record_runtime_decision(
            &RuntimeScenario::default().decision_context().unwrap(),
            RuntimePolicyConfig::default(),
        )
        .unwrap();
    store
        .link_causal_artifact(&CausalArtifactDependency {
            hypothesis: hid.clone(),
            artifact: CausalArtifact::RuntimeDecision(decision.id.clone()),
            intervention: Some(intervention),
            severity: Severity::High,
        })
        .unwrap();
    assert!(
        !store
            .causal_artifact_quarantined(&decision.id.to_string())
            .unwrap()
    );
    i.spec
        .baseline
        .insert(dependency.clone(), VariableValue::Boolean(false));
    let second = store.create_causal_investigation(&i).unwrap();
    test_variable(&store, &i, &second, "state_refresh").await;
    let h = store.causal_hypothesis(&hid).unwrap();
    assert_eq!(h.status, CausalHypothesisStatus::Contradicted);
    assert_eq!(h.evidence.len(), 2);
    assert!(differing_conditions(&store.causal_evidence(&hid).unwrap()).contains_key(&dependency));
    assert!(
        store
            .causal_artifact_quarantined(&decision.id.to_string())
            .unwrap()
    );
    assert_eq!(
        store.causal_impact(&hid).unwrap()["revalidations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(store.propose_causal_refinement(&inv.id, &hid).is_err());
    assert!(
        store.causal_curriculum_goals(&inv.id).unwrap().iter().any(
            |g| g.kind == hardknock::curriculum::CurriculumGoalKind::ResolveCausalContradiction
        )
    );
    assert_eq!(
        store
            .causal_models()
            .unwrap()
            .iter()
            .flat_map(|m| &m.edges)
            .filter(|e| e.hypothesis == hid && e.status == CausalHypothesisStatus::Supported)
            .count(),
        0
    );
}

#[tokio::test]
async fn remote_supported_claim_is_advisory_until_local_intervention() {
    let f = Fixture::from_fixture("causal/federated-causal");
    let store = Store::open(&f.home).unwrap();
    let mut i = input(&f);
    let hid = hypothesis(&i, "state_refresh");
    for h in &mut i.hypotheses {
        h.status = CausalHypothesisStatus::StronglySupported;
        h.remote_origin = Some(RemoteCausalOrigin {
            node: "node-A".into(),
            root_experiment: "original-pair".into(),
            reported_status: CausalHypothesisStatus::StronglySupported,
        });
    }
    let inv = store.create_causal_investigation(&i).unwrap();
    assert_eq!(
        store.causal_hypothesis(&hid).unwrap().status,
        CausalHypothesisStatus::Candidate
    );
    test_variable(&store, &i, &inv, "state_refresh").await;
    let h = store.causal_hypothesis(&hid).unwrap();
    assert_eq!(h.status, CausalHypothesisStatus::Contradicted);
    assert_eq!(
        h.remote_origin.unwrap().reported_status,
        CausalHypothesisStatus::StronglySupported
    );
}

#[test]
fn domains_capabilities_budget_and_untestable_hypotheses_fail_closed() {
    let f = Fixture::from_fixture("causal/untestable");
    let store = Store::open(&f.home).unwrap();
    let mut i = input(&f);
    i.spec.available_interventions.clear();
    let inv = store.create_causal_investigation(&i).unwrap();
    let plan = store
        .plan_causal_investigation(&inv.id, &Config::default())
        .unwrap();
    assert_eq!(plan.untestable.len(), 3);
    assert!(plan.experiments.is_empty());
    assert!(
        store
            .causal_investigation_hypotheses(&inv.id)
            .unwrap()
            .iter()
            .all(|h| h.status == CausalHypothesisStatus::Untestable)
    );
    let mut i = input(&f);
    i.spec.available_interventions[0]
        .required_reality
        .process_isolation = hardknock::capability::IsolationLevel::StrongSandbox;
    let context = planning_context(&i.spec, &store, &Config::default()).unwrap();
    let plan = DeterministicInterventionPlanner::default()
        .plan(&i.hypotheses, &context, &i.spec.budget)
        .unwrap();
    assert!(plan.untestable.contains(&i.hypotheses[0].id));
    let mut bad = i.spec.clone();
    bad.bindings
        .insert(var(&i, "latency"), "../production.input".into());
    assert!(validate_spec(&bad).is_err());
    bad = i.spec.clone();
    bad.baseline
        .insert(var(&i, "latency"), VariableValue::Integer(-1));
    assert!(validate_spec(&bad).is_err());
    assert_eq!(
        assess_quality(
            false,
            true,
            &[CausalVariableId::new()],
            &[],
            &[],
            ExperimentQuality::Controlled
        )
        .quality,
        ExperimentQuality::Invalid
    );
    let zero = hardknock::budget::ExperienceBudget {
        max_realities: 0,
        ..Default::default()
    };
    assert!(
        DeterministicInterventionPlanner::default()
            .plan(&i.hypotheses, &context, &zero)
            .unwrap()
            .experiments
            .is_empty()
    );
}

#[tokio::test]
async fn repeated_identical_trials_do_not_establish_strong_support_and_budgets_are_cumulative() {
    let f = Fixture::from_fixture("causal/causal-diversity");
    let store = Store::open(&f.home).unwrap();
    let i = input(&f);
    let inv = store.create_causal_investigation(&i).unwrap();
    for _ in 0..3 {
        test_variable(&store, &i, &inv, "state_refresh").await;
    }
    let hid = hypothesis(&i, "state_refresh");
    assert_eq!(
        store.causal_hypothesis(&hid).unwrap().status,
        CausalHypothesisStatus::Supported
    );
    let plan = store
        .plan_causal_investigation(&inv.id, &Config::default())
        .unwrap();
    let run = compile_intervention(&inv.id, &i.spec, &plan.experiments[0]).unwrap();
    assert!(
        execute_causal_run(&store, &Config::default(), run, &Cancellation::default())
            .await
            .is_err()
    );
    let start = Instant::now();
    for _ in 0..100 {
        store.causal_hypothesis(&hid).unwrap();
        store.causal_models().unwrap();
        store.causal_impact(&hid).unwrap();
    }
    eprintln!(
        "causal lookup/model/impact 100 iterations: {:?}",
        start.elapsed()
    );
    let reopened = Store::open(&f.home).unwrap();
    assert_eq!(reopened.causal_evidence(&hid).unwrap().len(), 3);
}

#[tokio::test]
async fn measured_comparative_benchmark_uses_real_counterfactual_trials() {
    let f = Fixture::from_fixture("causal/recovery-mechanism");
    let store = Store::open(&f.home).unwrap();
    let report = benchmark::run(
        &store,
        &Config::default(),
        &f.repo,
        &Cancellation::default(),
    )
    .await
    .unwrap();
    assert_eq!(report["causal_interventions"], 3);
    assert_eq!(report["interventions_until_discrimination"], 1);
    assert_eq!(report["held_out_recovery"][0]["success"], false);
    assert_eq!(report["held_out_recovery"][1]["success"], true);
    assert_eq!(report["reflex_precision"]["healthy_cases"], 3);
    assert_eq!(report["reflex_precision"]["before_false_positives"], 2);
    assert_eq!(report["reflex_precision"]["after_false_positives"], 0);
    assert_eq!(
        report["arms"]["correlation_learner"]["task_success_rate"],
        0
    );
    assert_eq!(report["arms"]["causal_learner"]["task_success_rate"], 1);
    assert_eq!(
        report["arms"]["strategy_counterfactual_learner"]["task_success_rate"],
        1
    );
    assert!(
        report["trials"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["pair"]["baseline"]["experiment"].is_string())
    );
}

#[tokio::test]
async fn diversity_across_evaluators_and_fixture_revisions_strengthens_support() {
    let f = Fixture::from_fixture("causal/causal-diversity");
    let store = Store::open(&f.home).unwrap();
    let mut i = input(&f);
    let hid = hypothesis(&i, "state_refresh");
    for (index, check) in [
        "test \"$(cat outcome.txt)\" = PASS",
        "grep -qx PASS outcome.txt",
        "test \"$(wc -l < outcome.txt | tr -d ' ')\" = 1 && grep -q PASS outcome.txt",
    ]
    .iter()
    .enumerate()
    {
        // Separate committed fixture revisions, not fabricated dependency labels.
        std::fs::write(
            f.repo.join("variant.txt"),
            format!("related fixture revision {index}\n"),
        )
        .unwrap();
        support::git(&f.repo, &["add", "variant.txt"]);
        support::git(&f.repo, &["commit", "-m", "related fixture revision"]);
        i.spec.starting_state.state_ref = capture_state(&f.repo).unwrap();
        i.spec.evaluator.checks = vec![(*check).into()];
        let inv = store.create_causal_investigation(&i).unwrap();
        test_variable(&store, &i, &inv, "state_refresh").await;
    }
    assert_eq!(
        store.causal_hypothesis(&hid).unwrap().status,
        CausalHypothesisStatus::StronglySupported
    );
}

#[tokio::test]
async fn contradiction_quarantines_real_lesson_reflex_and_recovery_records() {
    let artifacts = Fixture::from_fixture("retry-resilience");
    artifacts.cli(
        &[
            "chaos",
            "run",
            "--agent",
            "test-agent",
            "--profile",
            "latency",
        ],
        0,
    );
    let f = Fixture::from_fixture("causal/invalidation-propagation");
    let store = Store::open(&artifacts.home).unwrap();
    let mut i = input(&f);
    let dep = add_input(
        &mut i,
        "dependency_available",
        VariableValue::Boolean(true),
        VariableDomain::Boolean,
    );
    let hid = hypothesis(&i, "state_refresh");
    let inv = store.create_causal_investigation(&i).unwrap();
    let report = test_variable(&store, &i, &inv, "state_refresh").await;
    let intervention: InterventionId =
        serde_json::from_value(report["evidence"][0]["intervention"].clone()).unwrap();
    let lesson = store.all_lessons().unwrap()[0].clone();
    let reflex = store.reflexes().unwrap()[0].clone();
    let recovery = store.recoveries().unwrap()[0].clone();
    let linked = [
        CausalArtifact::Lesson(lesson.id.clone()),
        CausalArtifact::Reflex(reflex.id.clone()),
        CausalArtifact::Recovery(recovery.id.clone()),
    ];
    for artifact in &linked {
        store
            .link_causal_artifact(&CausalArtifactDependency {
                hypothesis: hid.clone(),
                artifact: artifact.clone(),
                intervention: Some(intervention.clone()),
                severity: Severity::High,
            })
            .unwrap();
    }
    i.spec.baseline.insert(dep, VariableValue::Boolean(false));
    let inv2 = store.create_causal_investigation(&i).unwrap();
    test_variable(&store, &i, &inv2, "state_refresh").await;
    for artifact in &linked {
        assert!(store.causal_artifact_quarantined(&artifact.key()).unwrap());
    }
    assert_eq!(store.lesson(&lesson.id).unwrap().status, lesson.status);
    assert_eq!(store.reflex(&reflex.id).unwrap().status, reflex.status);
    assert_eq!(
        store.recovery(&recovery.id).unwrap().status,
        recovery.status
    );
    assert_eq!(
        store.causal_impact(&hid).unwrap()["revalidations"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let cache = hardknock::bridge::cache::ExperienceHotCache::load(&store).unwrap();
    assert!(!cache.reflexes.iter().any(|r| r.id == reflex.id));
    assert!(!cache.recoveries.iter().any(|r| r.id == recovery.id));
}

#[tokio::test]
async fn unsafe_binding_failure_is_inconclusive_and_storage_evidence_is_immutable() {
    let f = Fixture::from_fixture("causal/stale-state");
    std::os::unix::fs::symlink("tracked.txt", f.repo.join("latency.input")).unwrap();
    support::git(&f.repo, &["add", "latency.input"]);
    support::git(&f.repo, &["commit", "-m", "symlink input"]);
    let store = Store::open(&f.home).unwrap();
    let i = input(&f);
    let inv = store.create_causal_investigation(&i).unwrap();
    let report = test_variable(&store, &i, &inv, "state_refresh").await;
    assert_eq!(report["evidence"][0]["outcome"], "inconclusive");
    assert_eq!(
        std::fs::read_to_string(f.repo.join("tracked.txt")).unwrap(),
        "original\n"
    );
    let db = rusqlite::Connection::open(f.home.join("hardknock.db")).unwrap();
    for table in [
        "causal_evidence",
        "counterfactual_pairs",
        "causal_models",
        "interventions",
        "causal_hypothesis_revisions",
    ] {
        assert!(db.execute(&format!("DELETE FROM {table}"), []).is_err());
    }
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
}

#[test]
fn runtime_prioritizes_linked_recovery_and_abstains_from_unknown_high_risk_cause() {
    let generic = RecoveryRef {
        id: RecoveryId::new(),
        version: 1,
        failure_signature: "retry-exhaustion".into(),
        confidence: 0.9.try_into().unwrap(),
        fresh: true,
        scope_matches: true,
    };
    let targeted = RecoveryRef {
        id: RecoveryId::new(),
        ..generic.clone()
    };
    let scenario = RuntimeScenario {
        failure_signature: Some(FailureSignatureRef {
            signature: "retry-exhaustion".into(),
        }),
        recoveries: vec![generic, targeted.clone()],
        ..Default::default()
    };
    let mut ctx = scenario.decision_context().unwrap();
    ctx.causal
        .supported_interventions
        .push(InterventionRecommendation {
            hypothesis: CausalHypothesisId::new(),
            intervention: Intervention {
                id: InterventionId::new(),
                variable: CausalVariableId::new(),
                from: Some(VariableValue::Boolean(false)),
                to: VariableValue::Boolean(true),
                held_constant: vec![],
                rationale: "refresh state".into(),
            },
            controlled_pairs: 3,
            recovery: Some(targeted.id.clone()),
        });
    let controller = DeterministicRuntimeController::default();
    let result = controller.evaluate(&ctx).unwrap();
    assert!(matches!(result.decision,RuntimeDecision::Recover(r) if r.recovery.id==targeted.id));
    ctx.causal.supported_interventions.clear();
    ctx.causal.causal_gaps.push(CausalGap {
        description: "unknown cause".into(),
        related_variables: vec![],
        reason: CausalGapReason::Untested,
    });
    ctx.risk.severity = Severity::High;
    ctx.available_experiments = Default::default();
    assert!(matches!(
        controller.evaluate(&ctx).unwrap().decision,
        RuntimeDecision::Abstain(_) | RuntimeDecision::Experiment(_)
    ));
    ctx.capability_context.governance.hard_policy_blocked = true;
    assert_eq!(
        controller.evaluate(&ctx).unwrap().governance,
        GovernanceDisposition::SecurityBlocked
    );
}

#[test]
fn empty_causal_extensions_do_not_change_legacy_hashed_context_or_summary() {
    let context = RuntimeScenario::default().decision_context().unwrap();
    let legacy = serde_json::to_value(&context).unwrap();
    assert!(legacy.get("causal").is_none());
    let decoded: RuntimeDecisionContext = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(serde_json::to_value(&decoded).unwrap(), legacy);
    assert_eq!(
        decoded.context_hash().unwrap(),
        context.context_hash().unwrap()
    );
    let summary =
        serde_json::to_value(hardknock::assurance::AssuranceEvidenceSummary::default()).unwrap();
    assert!(summary.get("causal_mechanisms").is_none());
    assert!(summary.get("evidence_diversity").is_none());
    assert!(summary.get("evaluator_kinds").is_none());
}

#[test]
fn causal_cli_registers_plans_and_refuses_untrusted_execution() {
    let f = Fixture::from_fixture("causal/stale-state");
    let demo = f.cli(&["causal", "demo"], 0);
    let inv = demo["result"]["investigation"]["id"].as_str().unwrap();
    f.cli(&["causal", "plan", inv], 0);
    let listed = f.cli(&["causal", "list"], 0);
    let hid = listed["result"]["hypotheses"][0]["id"].as_str().unwrap();
    f.cli(&["causal", "show", hid], 0);
    let output = f.command().args(["causal", "test", hid]).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("trusted-local"));
    f.cli(&["causal", "compare", hid, hid], 0);
    f.cli(&["causal", "impact", hid], 0);
}

#[test]
fn causal_cli_executes_replays_and_exposes_model_diff_and_provenance() {
    let f = Fixture::from_fixture("causal/stale-state");
    f.cli(&["causal", "demo"], 0);
    let listed = f.cli(&["causal", "list"], 0);
    let hid = listed["result"]["hypotheses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["statement"].as_str().unwrap().contains("state_refresh"))
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let tested = f.cli(&["causal", "test", hid, "--trusted-local"], 0);
    assert_eq!(tested["result"]["evidence"][0]["outcome"], "supports");
    f.cli(&["causal", "refine", hid], 0);
    let intervention = tested["result"]["evidence"][0]["intervention"]
        .as_str()
        .unwrap();
    let replay = f.cli(&["causal", "replay", intervention, "--trusted-local"], 0);
    assert_eq!(replay["result"]["evidence"][0]["outcome"], "supports");
    assert_ne!(
        tested["result"]["evidence"][0]["id"],
        replay["result"]["evidence"][0]["id"]
    );
    let models = f.cli(&["causal", "model", "list"], 0);
    let mid = models["result"]["models"][0]["id"].as_str().unwrap();
    f.cli(&["causal", "model", "show", mid], 0);
    f.cli(&["causal", "model", "history", mid], 0);
    f.cli(
        &["causal", "model", "diff", mid, "--from", "1", "--to", "2"],
        0,
    );
    f.cli(&["provenance", hid], 0);
}

#[tokio::test]
async fn observations_remain_separate_and_untested_contexts_do_not_inherit_guidance() {
    let f = Fixture::from_fixture("causal/scope-split");
    let store = Store::open(&f.home).unwrap();
    let mut i = input(&f);
    let dependency = add_input(
        &mut i,
        "dependency_available",
        VariableValue::Boolean(true),
        VariableDomain::Boolean,
    );
    let inv = store.create_causal_investigation(&i).unwrap();
    let result = test_variable(&store, &i, &inv, "state_refresh").await;
    let source: ExperienceId =
        serde_json::from_value(result["evidence"][0]["baseline_trial"]["experience"].clone())
            .unwrap();
    i.source_experiences = vec![source];
    i.spec
        .baseline
        .insert(dependency.clone(), VariableValue::Boolean(false));
    let untested = store.create_causal_investigation(&i).unwrap();
    assert_eq!(store.causal_observations(&untested.id).unwrap().len(), 1);
    assert!(
        store
            .propose_causal_refinement(&untested.id, &hypothesis(&i, "state_refresh"))
            .is_err()
    );
    let mut query = RuntimeScenario::default()
        .decision_context()
        .unwrap()
        .query_context;
    query.repository.path = i.spec.starting_state.state_ref.repo_path.clone();
    query.repository.commit = i.spec.starting_state.state_ref.git_commit.clone();
    query.environment.os = std::env::consts::OS.into();
    query.environment.arch = std::env::consts::ARCH.into();
    query.environment.facts = i
        .spec
        .baseline
        .iter()
        .map(|(id, value)| {
            (
                i.spec
                    .variables
                    .iter()
                    .find(|v| v.id == *id)
                    .unwrap()
                    .name
                    .clone(),
                value.literal(),
            )
        })
        .collect();
    let guidance = store
        .causal_runtime_guidance(&query, Some("retry-exhaustion"))
        .unwrap();
    assert!(guidance.supported_interventions.is_empty());
    assert!(!guidance.causal_gaps.is_empty());
    // Merely registering a newer fixture snapshot cannot make old support fresh.
    std::fs::write(f.repo.join("revision.txt"), "version two\n").unwrap();
    support::git(&f.repo, &["add", "revision.txt"]);
    support::git(&f.repo, &["commit", "-m", "fixture version two"]);
    i.spec.starting_state.state_ref = capture_state(&f.repo).unwrap();
    i.spec
        .baseline
        .insert(dependency, VariableValue::Boolean(true));
    store.create_causal_investigation(&i).unwrap();
    query.repository.commit = i.spec.starting_state.state_ref.git_commit.clone();
    query
        .environment
        .facts
        .insert("dependency_available".into(), "true".into());
    assert!(
        store
            .causal_runtime_guidance(&query, Some("retry-exhaustion"))
            .unwrap()
            .supported_interventions
            .is_empty()
    );
}
