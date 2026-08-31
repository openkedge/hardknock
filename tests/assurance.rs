// SPDX-License-Identifier: Apache-2.0

mod support;

use std::{collections::BTreeSet, time::Instant};

use chrono::{Duration, Utc};
use hardknock::{
    assurance::*,
    capability::{ExecutionCapability, NetworkEndpointPattern},
    core::*,
    curriculum::{CurriculumGoalKind, Severity},
    federation::{ExperienceNodeType, NodeIdentity, ProvenanceGraph},
    store::{AssuranceStore, Store},
};
use support::Fixture;

fn condition() -> BehavioralCondition {
    BehavioralCondition::EvaluatorCheck {
        evaluator: "hardknock.outcome".into(),
        expression: "success".into(),
    }
}

fn contract() -> BehavioralContract {
    BehavioralContract {
        id: BehavioralContractId::new(),
        name: "deploy".into(),
        version: "1".into(),
        subject: ContractSubject::Skill(SkillId::new()),
        preconditions: vec![],
        postconditions: vec![condition()],
        invariants: vec![],
        forbidden_outcomes: vec![],
        capability_requirements: Default::default(),
        evaluation_requirements: EvaluationRequirementSet {
            evaluators: vec!["hardknock.outcome".into()],
            ..Default::default()
        },
        created_at: Utc::now(),
    }
}

fn evidence(status: ContractEvaluationStatus) -> ExecutionEvidence {
    ExecutionEvidence {
        evaluator_results: [(
            ExecutionEvidence::evaluator_key("hardknock.outcome", "success"),
            status,
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    }
}

fn manifest(skill: SkillRevisionRef, evaluations: Vec<ContractEvaluation>) -> EvidenceManifest {
    let now = Utc::now();
    let mut manifest = EvidenceManifest {
        id: EvidenceManifestId::new(),
        subject: EvidenceSubject::Skill(skill),
        generated_at: now,
        experiences: vec![],
        experiments: vec![],
        chaos_campaigns: vec![],
        attestations: vec![],
        lessons: vec![],
        reflexes: vec![],
        recoveries: vec![],
        envelopes: vec![],
        capability_manifests: vec![],
        effect_receipts: vec![],
        policy_versions: Default::default(),
        summary: AssuranceEvidenceSummary {
            contract_evaluations: evaluations,
            attestations_intact: true,
            oldest_required_evidence: Some(now),
            newest_evidence: Some(now),
            ..Default::default()
        },
        evidence_hash: String::new(),
    };
    manifest.seal().unwrap();
    manifest
}

fn profile(requirements: Vec<AssuranceRequirement>) -> AssuranceProfile {
    AssuranceProfile {
        id: AssuranceProfileId::new(),
        name: "test-profile-v1".into(),
        version: "1".into(),
        requirements,
        created_at: Utc::now(),
    }
}

#[test]
fn missing_observation_is_unknown_and_not_a_pass() {
    let contract = contract();
    let evaluation = DeterministicContractEvaluator
        .evaluate(&contract, &ExecutionEvidence::default())
        .unwrap();
    assert_eq!(evaluation.overall, ContractEvaluationStatus::Inconclusive);
    assert_eq!(
        evaluation.postconditions[0].status,
        ContractEvaluationStatus::Inconclusive
    );
}

#[test]
fn deterministic_contract_evaluation_satisfies_observed_condition() {
    let contract = contract();
    let evaluation = DeterministicContractEvaluator
        .evaluate(&contract, &evidence(ContractEvaluationStatus::Satisfied))
        .unwrap();
    assert_eq!(evaluation.overall, ContractEvaluationStatus::Satisfied);
    assert!(
        contract_observability(&contract)
            .unwrap()
            .unobservable
            .is_empty()
    );
}

#[test]
fn one_critical_violation_blocks_after_ninety_nine_successes() {
    let mut contract = contract();
    let invariant = BehavioralInvariant {
        id: BehavioralInvariantId::new(),
        description: "losing candidate effects never escape".into(),
        condition: BehavioralCondition::EvaluatorCheck {
            evaluator: "effects".into(),
            expression: "no-losing-commit".into(),
        },
        severity: Severity::Critical,
        phases: vec![InvariantEvaluationPhase::AfterEffectCommit],
    };
    contract.invariants.push(invariant);
    contract
        .evaluation_requirements
        .evaluators
        .push("effects".into());
    let evaluator = DeterministicContractEvaluator;
    let run = |status| {
        let mut evidence = evidence(ContractEvaluationStatus::Satisfied);
        evidence.evaluator_results.insert(
            ExecutionEvidence::evaluator_key("effects", "no-losing-commit"),
            status,
        );
        evaluator.evaluate(&contract, &evidence).unwrap()
    };
    let mut evaluations = (0..99)
        .map(|_| run(ContractEvaluationStatus::Satisfied))
        .collect::<Vec<_>>();
    evaluations.push(run(ContractEvaluationStatus::Violated));
    let skill = SkillRevisionRef {
        skill_id: match &contract.subject {
            ContractSubject::Skill(id) => id.clone(),
            _ => unreachable!(),
        },
        revision: 7,
    };
    let manifest = manifest(skill.clone(), evaluations);
    let evaluation = DeterministicCertificationEvaluator::default()
        .evaluate(
            &skill,
            &contract,
            &profile(vec![AssuranceRequirement::ContractSatisfied {
                minimum_runs: 99,
            }]),
            &manifest,
        )
        .unwrap();
    assert_eq!(
        evaluation.recommendation,
        CertificationRecommendation::Blocked
    );
    assert!(evaluation.blockers.iter().any(|blocker| {
        blocker.severity == Severity::Critical && blocker.description.contains("Critical invariant")
    }));
}

#[test]
fn behavior_passes_but_overprivilege_blocks_certification() {
    let mut contract = contract();
    contract.capability_requirements.maximum = Some(CapabilityEnvelope {
        allowed: vec![ExecutionCapabilityPattern::NetworkEndpoint {
            endpoint: NetworkEndpointPattern {
                host: "registry.example".into(),
                port: 443,
            },
        }],
        deny_ambient_credentials: true,
        deny_effect_commit: true,
    });
    let skill = SkillRevisionRef {
        skill_id: match &contract.subject {
            ContractSubject::Skill(id) => id.clone(),
            _ => unreachable!(),
        },
        revision: 1,
    };
    let run = DeterministicContractEvaluator
        .evaluate(&contract, &evidence(ContractEvaluationStatus::Satisfied))
        .unwrap();
    assert_eq!(run.overall, ContractEvaluationStatus::Satisfied);
    let mut manifest = manifest(skill.clone(), vec![run]);
    manifest.summary.capability_observed = true;
    manifest.summary.observed_capabilities = vec![ExecutionCapability::NetworkConnect(
        NetworkEndpointPattern {
            host: "*".into(),
            port: u16::MAX,
        },
    )];
    manifest.seal().unwrap();
    let evaluation = DeterministicCertificationEvaluator::default()
        .evaluate(
            &skill,
            &contract,
            &profile(vec![AssuranceRequirement::ContractSatisfied {
                minimum_runs: 1,
            }]),
            &manifest,
        )
        .unwrap();
    assert_eq!(
        evaluation.requirements[0].status,
        AssuranceRequirementStatus::Satisfied
    );
    assert_eq!(
        evaluation.recommendation,
        CertificationRecommendation::Blocked
    );
    assert!(evaluation.blockers[0].description.contains("maximum"));
}

#[test]
fn missing_profile_evidence_generates_targeted_curriculum() {
    let contract = contract();
    let skill = SkillRevisionRef {
        skill_id: match &contract.subject {
            ContractSubject::Skill(id) => id.clone(),
            _ => unreachable!(),
        },
        revision: 1,
    };
    let run = DeterministicContractEvaluator
        .evaluate(&contract, &evidence(ContractEvaluationStatus::Satisfied))
        .unwrap();
    let manifest = manifest(skill.clone(), vec![run]);
    let evaluation = DeterministicCertificationEvaluator::default()
        .evaluate(
            &skill,
            &contract,
            &profile(vec![AssuranceRequirement::PerturbationProfileCoverage {
                profile: "network-partition".into(),
                minimum_fraction: 1.0,
            }]),
            &manifest,
        )
        .unwrap();
    assert_eq!(
        evaluation.recommendation,
        CertificationRecommendation::AdditionalEvidenceRequired
    );
    let curriculum = certification_curriculum(&evaluation);
    assert!(!curriculum.is_empty());
    assert!(curriculum.iter().all(|goal| matches!(
        goal.kind,
        CurriculumGoalKind::SatisfyAssuranceRequirement | CurriculumGoalKind::ChallengeInvariant
    )));
}

#[test]
fn tool_change_recommends_review_and_revision_changes_invalidate() {
    let skill = SkillRevisionRef {
        skill_id: SkillId::new(),
        revision: 7,
    };
    let certification = SkillCertification {
        id: SkillCertificationId::new(),
        skill: skill.clone(),
        contract: BehavioralContractRef {
            contract_id: BehavioralContractId::new(),
            revision: 3,
        },
        profile: AssuranceProfileRef {
            id: AssuranceProfileId::new(),
            version: "1".into(),
        },
        status: CertificationStatus::Certified,
        evidence_manifest: EvidenceManifestId::new(),
        issued_at: Utc::now(),
        expires_at: None,
        supersedes: None,
        policy_versions: Default::default(),
        tool_artifact_hashes: BTreeSet::from(["H1".into()]),
        runtime_digests: BTreeSet::new(),
    };
    let context = |skill_revision, contract_revision, hash: &str| CurrentAssuranceContext {
        now: Utc::now(),
        skill_revision,
        contract_revision,
        tool_artifact_hashes: BTreeSet::from([hash.into()]),
        runtime_digests: BTreeSet::new(),
        invalidated: false,
    };
    assert_eq!(
        DeterministicFreshnessPolicy.status(&certification, &context(7, 3, "H2")),
        CertificationFreshness::ReviewRecommended
    );
    assert_eq!(
        DeterministicFreshnessPolicy.status(&certification, &context(8, 3, "H1")),
        CertificationFreshness::Invalidated
    );
    assert_eq!(
        DeterministicFreshnessPolicy.status(&certification, &context(7, 4, "H1")),
        CertificationFreshness::Invalidated
    );
}

#[test]
fn signed_artifact_detects_manifest_tampering_and_stays_advisory() {
    let contract = contract();
    let skill = SkillRevisionRef {
        skill_id: match &contract.subject {
            ContractSubject::Skill(id) => id.clone(),
            _ => unreachable!(),
        },
        revision: 1,
    };
    let run = DeterministicContractEvaluator
        .evaluate(&contract, &evidence(ContractEvaluationStatus::Satisfied))
        .unwrap();
    let manifest = manifest(skill.clone(), vec![run]);
    let profile = profile(vec![AssuranceRequirement::ContractSatisfied {
        minimum_runs: 1,
    }]);
    let evaluation = DeterministicCertificationEvaluator::default()
        .evaluate(&skill, &contract, &profile, &manifest)
        .unwrap();
    let certification = issue_certification(
        skill,
        BehavioralContractRef {
            contract_id: contract.id.clone(),
            revision: 1,
        },
        &profile,
        &manifest,
        &evaluation,
        None,
    )
    .unwrap();
    let revision = BehavioralContractRevision {
        contract_id: contract.id.clone(),
        revision: 1,
        contract,
        parent_revision: None,
        reason: None,
        created_at: Utc::now(),
    };
    let temp = tempfile::tempdir().unwrap();
    let identity =
        NodeIdentity::load_or_create(temp.path(), "node-a", ExperienceNodeType::Team).unwrap();
    let mut artifact = CertificationArtifact::new(
        certification,
        revision,
        profile,
        manifest,
        ProvenanceGraph::default(),
    )
    .unwrap();
    artifact.sign(&identity).unwrap();
    let verification = artifact.verify();
    assert!(verification.authentic);
    assert!(!verification.local_certification_established);
    assert!(!verification.local_reproduction_performed);
    artifact
        .evidence_manifest
        .summary
        .known_unknowns
        .push("tampered".into());
    let verification = artifact.verify();
    assert!(!verification.authentic);
    assert!(!verification.manifest_intact);
    assert!(!verification.signature_valid);
}

#[test]
fn manifest_hash_is_stable_and_mature_skill_generation_is_practical() {
    let skill = SkillRevisionRef {
        skill_id: SkillId::new(),
        revision: 9,
    };
    let mut first = manifest(skill.clone(), vec![]);
    first.experiences = (0..1_000).map(|_| ExperienceId::new()).collect();
    first.experiments = (0..100).map(|_| ExperimentId::new()).collect();
    first.attestations = (0..120).map(|_| ExecutionAttestationId::new()).collect();
    let start = Instant::now();
    first.seal().unwrap();
    assert!(start.elapsed().as_secs_f64() < 2.0);
    let expected = first.evidence_hash.clone();
    let mut second = first.clone();
    second.id = EvidenceManifestId::new();
    second.generated_at += Duration::hours(2);
    second.experiences.reverse();
    second.seal().unwrap();
    assert_eq!(second.evidence_hash, expected);
}

#[test]
fn prompt_style_contract_toml_is_structured_and_observable() {
    let source = r#"
schema = "hardknock.contract.v1"
name = "deploy"
version = "1"

[evaluation_requirements]
observable_state_paths = ["deployment.healthy_replicas"]

[[invariants]]
description = "At least two replicas remain healthy"
severity = "high"
phases = ["during_execution"]
type = "state-predicate"
path = "deployment.healthy_replicas"
operator = "greater-than-or-equal"
value = 2
"#;
    let file: ContractFile = toml::from_str(source).unwrap();
    let contract = file
        .into_contract(ContractSubject::Skill(SkillId::new()))
        .unwrap();
    assert!(
        contract_observability(&contract)
            .unwrap()
            .unobservable
            .is_empty()
    );
}

#[test]
fn store_rejects_manifest_references_that_do_not_exist() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let skill = SkillRevisionRef {
        skill_id: SkillId::new(),
        revision: 1,
    };
    let mut manifest = manifest(skill, vec![]);
    manifest.experiences.push(ExperienceId::new());
    manifest.seal().unwrap();
    assert!(store.insert_evidence_manifest(&manifest).is_err());
}

#[test]
fn losing_candidate_effect_leak_and_stale_evidence_are_not_certifiable() {
    let contract = contract();
    let skill = SkillRevisionRef {
        skill_id: match &contract.subject {
            ContractSubject::Skill(id) => id.clone(),
            _ => unreachable!(),
        },
        revision: 1,
    };
    let run = DeterministicContractEvaluator
        .evaluate(&contract, &evidence(ContractEvaluationStatus::Satisfied))
        .unwrap();
    let mut manifest = manifest(skill.clone(), vec![run]);
    manifest.summary.experimental_effect_leak = true;
    manifest.summary.oldest_required_evidence = Some(Utc::now() - Duration::days(31));
    manifest.seal().unwrap();
    let evaluation = DeterministicCertificationEvaluator::default()
        .evaluate(
            &skill,
            &contract,
            &profile(vec![AssuranceRequirement::EvidenceFreshness {
                maximum_age_days: Some(30),
            }]),
            &manifest,
        )
        .unwrap();
    assert_eq!(
        evaluation.recommendation,
        CertificationRecommendation::Blocked
    );
    assert!(
        evaluation
            .blockers
            .iter()
            .any(|blocker| blocker.description.contains("losing experimental branch"))
    );
    assert!(
        evaluation
            .gaps
            .iter()
            .any(|gap| gap.kind == AssuranceGapKind::StaleEvidence)
    );
}

#[test]
fn cli_registers_contract_certifies_exports_and_verifies() {
    let fixture = Fixture::new();
    let run = fixture.cli(
        &[
            "run",
            "--script",
            "true",
            "--check",
            "test -f tracked.txt",
            "observe deploy",
        ],
        0,
    );
    let experience = run["experience"]["id"].as_str().unwrap();
    fixture.cli(
        &["skill", "register", "deploy", "--experience", experience],
        0,
    );
    let contract_path = fixture.temp.path().join("deploy-contract.toml");
    std::fs::write(
        &contract_path,
        r#"
schema = "hardknock.contract.v1"
name = "deploy-contract"
version = "1"

[evaluation_requirements]
evaluators = ["hardknock.outcome"]

[[postconditions]]
type = "evaluator-check"
evaluator = "hardknock.outcome"
expression = "success"
"#,
    )
    .unwrap();
    fixture.cli(
        &[
            "contract",
            "register",
            contract_path.to_str().unwrap(),
            "--skill",
            "deploy",
        ],
        0,
    );
    let dry_run = fixture.cli(
        &[
            "skill",
            "certify",
            "deploy",
            "--profile",
            "basic-behavior-v1",
            "--dry-run",
        ],
        0,
    );
    assert_eq!(
        dry_run["result"]["evaluation"]["recommendation"],
        "eligible"
    );
    assert_eq!(dry_run["result"]["certified"], false);
    let issued = fixture.cli(
        &[
            "skill",
            "certify",
            "deploy",
            "--profile",
            "basic-behavior-v1",
        ],
        0,
    );
    assert_eq!(issued["result"]["certified"], true);
    let output = fixture.temp.path().join("deploy.hkcert");
    fixture.cli(
        &[
            "assurance",
            "export",
            "deploy",
            "--profile",
            "basic-behavior-v1",
            "--output",
            output.to_str().unwrap(),
        ],
        0,
    );
    let verified = fixture.cli(&["assurance", "verify", output.to_str().unwrap()], 0);
    assert_eq!(verified["result"]["verification"]["authentic"], true);
    assert_eq!(
        verified["result"]["verification"]["local_certification_established"],
        false
    );
    std::fs::write(
        &contract_path,
        r#"
schema = "hardknock.contract.v1"
name = "deploy-contract"
version = "2"

[evaluation_requirements]
evaluators = ["hardknock.outcome"]

[[postconditions]]
type = "evaluator-check"
evaluator = "hardknock.outcome"
expression = "success"

[[invariants]]
description = "New v2 invariant"
severity = "high"
phases = ["after_execution"]
type = "evaluator-check"
evaluator = "hardknock.outcome"
expression = "success"
"#,
    )
    .unwrap();
    fixture.cli(
        &[
            "contract",
            "register",
            contract_path.to_str().unwrap(),
            "--skill",
            "deploy",
        ],
        0,
    );
    let show = fixture.cli(&["assurance", "show", "deploy"], 0);
    assert_eq!(show["result"]["behavioral_contract"]["revision"], 2);
    assert_eq!(
        show["result"]["certifications"][0]["certification"]["contract"]["revision"],
        1
    );
    assert_eq!(
        show["result"]["certifications"][0]["freshness"],
        "invalidated"
    );
    let certification_id = issued["result"]["certification"]["id"].as_str().unwrap();
    fixture.cli(
        &[
            "assurance",
            "revoke",
            certification_id,
            "--reason",
            "v2 contract requires re-evaluation",
        ],
        0,
    );
    let history = fixture.cli(&["assurance", "history", "deploy"], 0);
    assert_eq!(history["result"]["history"].as_array().unwrap().len(), 1);
    assert_eq!(
        history["result"]["history"][0]["revocation"]["reason"],
        "v2 contract requires re-evaluation"
    );
    fixture.assert_source_unchanged();
}
