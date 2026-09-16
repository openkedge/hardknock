// SPDX-License-Identifier: Apache-2.0
use chrono::{Duration, Utc};
use hardknock::{
    core::*,
    curriculum::Severity,
    epistemic::*,
    hierarchy::KnowledgeScope,
    lesson::ContextSelector,
    runtime::*,
    store::{EpistemicStore, RuntimeStore, Store},
    team::*,
};
use std::collections::BTreeSet;
struct Fixture {
    _home: tempfile::TempDir,
    store: Store,
    team: AgentTeam,
    review: TeamReview,
}
impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(home.path()).unwrap();
        let roles = vec![
            AgentRole::builtin(BuiltInAgentRole::Planner),
            AgentRole::builtin(BuiltInAgentRole::Reviewer),
            AgentRole::builtin(BuiltInAgentRole::Executor),
        ];
        let members: Vec<_> = (0..3)
            .map(|_| AgentTeamMember {
                id: TeamMemberId::new(),
                session: HardknockSessionId::new(),
                agent: AgentIdentity {
                    kind: "test".into(),
                    executable: "test".into(),
                    model: Some("shared-model".into()),
                    version: None,
                },
            })
            .collect();
        let now = Utc::now();
        let team = AgentTeam {
            id: AgentTeamId::new(),
            revision: 1,
            role_assignments: members
                .iter()
                .enumerate()
                .map(|(i, m)| RoleAssignment {
                    id: RoleAssignmentId::new(),
                    member: m.id.clone(),
                    role: roles[i].id.clone(),
                    scope: KnowledgeScope::default(),
                    valid_from: now - Duration::seconds(1),
                    valid_until: now + Duration::hours(2),
                })
                .collect(),
            authority: roles.iter().flat_map(|r| r.authority()).collect(),
            roles,
            members,
            max_delegation_depth: 2,
            created_at: now,
        };
        store.save_agent_team(&team).unwrap();
        let claim = Claim {
            id: ClaimId::new(),
            kind: ClaimKind::RuntimeDecisionClaim,
            statement: "This bounded action is supported".into(),
            scope: ContextSelector {
                repository: None,
                required_markers: vec![],
                tags: vec![],
                os: None,
                arch: None,
            },
            created_at: now,
        };
        store.insert_claim(&claim).unwrap();
        let review = TeamReview {
            id: TeamReviewId::new(),
            team: team.id.clone(),
            team_revision: 1,
            target: ReviewTarget {
                claim: claim.id,
                action_hash: review_action_hash(&base_context()).unwrap(),
            },
            proposer: team.members[0].id.clone(),
            executor: team.members[2].id.clone(),
            required_roles: [team.roles[1].id.clone()].into(),
            minimum_diversity: DiversityClass::Unknown,
            max_evidence_age_seconds: 3600,
            created_at: now,
            expires_at: now + Duration::hours(1),
        };
        let review = store.create_team_review(&review).unwrap();
        Self {
            _home: home,
            store,
            team,
            review,
        }
    }
    fn context(&self, member: usize) -> RuntimeDecisionContext {
        let mut c = base_context();
        c.session_id = self.team.members[member].session.clone();
        c.agent = self.team.members[member].agent.clone();
        c.team = Some(TeamRuntimeContext {
            team: self.team.id.clone(),
            revision: self.team.revision,
            member: self.team.members[member].id.clone(),
            assignment: self.team.role_assignments[member].id.clone(),
            delegation: None,
            review: Some(self.review.id.clone()),
            assessment: None,
        });
        c
    }
    fn path(&self, label: &str, outcome: EvidenceOutcome) -> EvidencePathId {
        self.path_at(label, outcome, Utc::now())
    }
    fn path_at(
        &self,
        label: &str,
        outcome: EvidenceOutcome,
        created_at: chrono::DateTime<Utc>,
    ) -> EvidencePathId {
        let p = EvidencePath {
            id: EvidencePathId::new(),
            claim: self.review.target.claim.clone().into(),
            source: EvidenceSource::StaticCheck {
                evaluator: label.into(),
            },
            context: EvidenceContext::default(),
            dependencies: EpistemicDependencySet {
                evaluators: vec![label.into()],
                environment_family: Some(label.into()),
                ..Default::default()
            },
            evidence_refs: vec![],
            outcome,
            created_at,
        };
        self.store.insert_evidence_path(&p).unwrap().id
    }
    fn submit(
        &self,
        member: usize,
        kind: ContributionType,
        paths: BTreeSet<EvidencePathId>,
        finding: Option<ReviewFindingKind>,
    ) -> AgentContribution {
        let c = AgentContribution {
            id: AgentContributionId::new(),
            review: self.review.id.clone(),
            member: self.team.members[member].id.clone(),
            assignment: self.team.role_assignments[member].id.clone(),
            contribution_type: kind,
            statement: "Structured test result".into(),
            evidence_paths: paths.clone(),
            created_at: Utc::now(),
        };
        let findings = finding
            .map(|kind| {
                vec![ReviewFinding {
                    id: ReviewFindingId::new(),
                    contribution: c.id.clone(),
                    kind,
                    statement: "Structured finding".into(),
                    evidence_paths: paths,
                }]
            })
            .unwrap_or_default();
        self.store
            .record_team_contribution(&c, &findings, &self.context(member))
            .unwrap()
    }
    fn ready(&self) -> BTreeSet<EvidencePathId> {
        self.submit(0, ContributionType::Proposal, BTreeSet::new(), None);
        let paths: BTreeSet<EvidencePathId> = [
            self.path("first", EvidenceOutcome::Supports),
            self.path("second", EvidenceOutcome::Supports),
        ]
        .into();
        self.submit(
            1,
            ContributionType::Review,
            paths.clone(),
            Some(ReviewFindingKind::NoIssueFound),
        );
        paths
    }
    fn assessment(&self) -> ReviewGateAssessment {
        self.store
            .assess_team_review(&self.review.id, &self.context(2))
            .unwrap()
    }
}
fn base_context() -> RuntimeDecisionContext {
    let mut c = serde_json::from_str::<RuntimeScenario>(include_str!(
        "../fixtures/runtime-scenarios/known-safe.json"
    ))
    .unwrap()
    .decision_context()
    .unwrap();
    c.proposed_action = Some(hardknock::bridge::protocol::NormalizedAction::Shell {
        command: "true".into(),
        cwd: ".".into(),
    });
    c.risk.severity = Severity::High;
    c
}
#[test]
fn exact_action_review_with_distinct_roles_and_diverse_evidence_passes() {
    let f = Fixture::new();
    f.ready();
    assert_eq!(f.assessment().status, ReviewGateStatus::Satisfied);
    let mut c = f.context(2);
    f.store.attach_team_authority(&mut c).unwrap();
    assert!(c.team.unwrap().assessment.unwrap().allowed);
}
#[test]
fn high_risk_missing_review_blocks_but_low_risk_has_no_mandatory_review() {
    let f = Fixture::new();
    let mut c = f.context(2);
    c.team.as_mut().unwrap().review = None;
    f.store.attach_team_authority(&mut c).unwrap();
    assert!(
        !c.team
            .as_ref()
            .unwrap()
            .assessment
            .as_ref()
            .unwrap()
            .allowed
    );
    c.risk.severity = Severity::Low;
    f.store.attach_team_authority(&mut c).unwrap();
    assert!(
        !c.team
            .as_ref()
            .unwrap()
            .assessment
            .as_ref()
            .unwrap()
            .allowed,
        "An explicit gate cannot be omitted even for low risk"
    );
    c.proposed_action = Some(hardknock::bridge::protocol::NormalizedAction::FileRead {
        path: "README.md".into(),
    });
    f.store.attach_team_authority(&mut c).unwrap();
    assert!(c.team.unwrap().assessment.unwrap().allowed);
}
#[test]
fn proposals_and_hypotheses_do_not_manufacture_evidence() {
    let f = Fixture::new();
    let p = f.path("shared", EvidenceOutcome::Supports);
    f.submit(0, ContributionType::Proposal, [p].into(), None);
    let e = f.store.team_evidence(&f.review.id).unwrap();
    assert_eq!(e.fused.diversity.path_count, 0);
    assert_ne!(f.assessment().status, ReviewGateStatus::Satisfied);
}
#[test]
fn repeating_one_path_in_many_reviews_does_not_increase_support() {
    let f = Fixture::new();
    f.submit(0, ContributionType::Proposal, BTreeSet::new(), None);
    let p = f.path("shared", EvidenceOutcome::Supports);
    for _ in 0..3 {
        f.submit(
            1,
            ContributionType::Review,
            [p.clone()].into(),
            Some(ReviewFindingKind::NoIssueFound),
        );
    }
    let e = f.store.team_evidence(&f.review.id).unwrap();
    assert_eq!(e.fused.diversity.path_count, 1);
    assert_eq!(f.assessment().status, ReviewGateStatus::EvidenceRequired);
}
#[test]
fn same_evaluator_and_environment_are_common_mode() {
    let f = Fixture::new();
    f.submit(0, ContributionType::Proposal, BTreeSet::new(), None);
    let p = [
        f.path("shared", EvidenceOutcome::Supports),
        f.path("shared", EvidenceOutcome::Supports),
    ]
    .into();
    f.submit(
        1,
        ContributionType::Review,
        p,
        Some(ReviewFindingKind::NoIssueFound),
    );
    assert_eq!(f.assessment().status, ReviewGateStatus::EvidenceRequired);
    assert!(
        !f.store
            .team_evidence(&f.review.id)
            .unwrap()
            .fault_domains
            .is_empty()
    );
}
#[test]
fn known_contradiction_cannot_be_omitted_by_a_reviewer() {
    let f = Fixture::new();
    f.ready();
    f.path("contradiction", EvidenceOutcome::Contradicts);
    assert_eq!(f.assessment().status, ReviewGateStatus::Blocked);
}
#[test]
fn stale_support_requires_new_evidence() {
    let f = Fixture::new();
    f.submit(0, ContributionType::Proposal, BTreeSet::new(), None);
    let p = [
        f.path_at(
            "old",
            EvidenceOutcome::Supports,
            Utc::now() - Duration::hours(2),
        ),
        f.path("new", EvidenceOutcome::Supports),
    ]
    .into();
    f.submit(
        1,
        ContributionType::Review,
        p,
        Some(ReviewFindingKind::NoIssueFound),
    );
    assert_eq!(f.assessment().status, ReviewGateStatus::EvidenceRequired);
}
#[test]
fn unresolved_findings_block_even_after_positive_review() {
    let f = Fixture::new();
    let paths = f.ready();
    let c = f.submit(
        1,
        ContributionType::Challenge,
        BTreeSet::new(),
        Some(ReviewFindingKind::RecoveryGap),
    );
    assert_eq!(f.assessment().status, ReviewGateStatus::Blocked);
    let finding = f
        .store
        .review_findings(&f.review.id)
        .unwrap()
        .into_iter()
        .find(|x| x.contribution == c.id)
        .unwrap();
    f.store
        .resolve_review_finding(&ReviewFindingResolution {
            finding: finding.id,
            reason: "Local user inspected current recovery proof".into(),
            evidence_paths: paths,
            created_at: Utc::now(),
        })
        .unwrap();
    assert_eq!(f.assessment().status, ReviewGateStatus::Satisfied);
}
#[test]
fn fresh_review_cannot_erase_prior_unresolved_finding() {
    let mut f = Fixture::new();
    f.ready();
    f.submit(
        1,
        ContributionType::Challenge,
        BTreeSet::new(),
        Some(ReviewFindingKind::ConstraintViolation),
    );
    f.review.id = TeamReviewId::new();
    f.review = f.store.create_team_review(&f.review).unwrap();
    f.ready();
    assert_eq!(f.assessment().status, ReviewGateStatus::Blocked);
}
#[test]
fn action_revision_and_team_revision_are_rechecked() {
    let f = Fixture::new();
    f.ready();
    let mut c = f.context(2);
    c.proposed_action = Some(hardknock::bridge::protocol::NormalizedAction::FileDelete {
        path: "other".into(),
    });
    assert_ne!(
        f.store.assess_team_review(&f.review.id, &c).unwrap().status,
        ReviewGateStatus::Satisfied
    );
    let mut t = f.team.clone();
    t.revision = 2;
    f.store.save_agent_team(&t).unwrap();
    assert_ne!(f.assessment().status, ReviewGateStatus::Satisfied);
}
#[test]
fn spoofed_contributor_and_cross_claim_evidence_are_rejected() {
    let f = Fixture::new();
    let mut c = AgentContribution {
        id: AgentContributionId::new(),
        review: f.review.id.clone(),
        member: f.team.members[1].id.clone(),
        assignment: f.team.role_assignments[1].id.clone(),
        contribution_type: ContributionType::Review,
        statement: "test".into(),
        evidence_paths: BTreeSet::new(),
        created_at: Utc::now(),
    };
    assert!(
        f.store
            .record_team_contribution(&c, &[], &f.context(0))
            .is_err()
    );
    let other = Claim {
        id: ClaimId::new(),
        kind: ClaimKind::Custom,
        statement: "different claim".into(),
        scope: ContextSelector {
            repository: None,
            required_markers: vec![],
            tags: vec![],
            os: None,
            arch: None,
        },
        created_at: Utc::now(),
    };
    f.store.insert_claim(&other).unwrap();
    let p = EvidencePath {
        id: EvidencePathId::new(),
        claim: other.id.into(),
        source: EvidenceSource::StaticCheck {
            evaluator: "test".into(),
        },
        context: EvidenceContext::default(),
        dependencies: EpistemicDependencySet::default(),
        outcome: EvidenceOutcome::Supports,
        evidence_refs: vec![],
        created_at: Utc::now(),
    };
    let p = f.store.insert_evidence_path(&p).unwrap();
    c.evidence_paths.insert(p.id);
    assert!(
        f.store
            .record_team_contribution(&c, &[], &f.context(1))
            .is_err()
    );
}
#[test]
fn new_finding_invalidates_pending_publication_and_preserves_original_context() {
    let f = Fixture::new();
    f.ready();
    let decision = f
        .store
        .record_runtime_decision(&f.context(2), Default::default())
        .unwrap();
    let original = decision
        .context
        .team
        .as_ref()
        .unwrap()
        .assessment
        .as_ref()
        .unwrap()
        .review
        .as_ref()
        .unwrap()
        .clone();
    assert_eq!(original.status, ReviewGateStatus::Satisfied);
    f.submit(
        1,
        ContributionType::Challenge,
        BTreeSet::new(),
        Some(ReviewFindingKind::MissingEvidence),
    );
    assert!(
        f.store
            .persist_runtime_decision(&decision, Default::default())
            .is_err()
    );
    assert_eq!(original.status, ReviewGateStatus::Satisfied);
}

#[test]
fn collapsed_roles_are_allowed_only_for_low_risk_review() {
    let mut f = Fixture::new();
    f.team.revision = 2;
    f.team.role_assignments[1].member = f.team.members[0].id.clone();
    f.team.role_assignments[2].member = f.team.members[0].id.clone();
    f.store.save_agent_team(&f.team).unwrap();
    f.review.id = TeamReviewId::new();
    f.review.team_revision = 2;
    f.review.executor = f.team.members[0].id.clone();
    f.review = f.store.create_team_review(&f.review).unwrap();
    f.submit(0, ContributionType::Proposal, BTreeSet::new(), None);
    let paths: BTreeSet<_> = [
        f.path("a", EvidenceOutcome::Supports),
        f.path("b", EvidenceOutcome::Supports),
    ]
    .into();
    let c = AgentContribution {
        id: AgentContributionId::new(),
        review: f.review.id.clone(),
        member: f.team.members[0].id.clone(),
        assignment: f.team.role_assignments[1].id.clone(),
        contribution_type: ContributionType::Review,
        statement: "Review under collapsed roles".into(),
        evidence_paths: paths.clone(),
        created_at: Utc::now(),
    };
    let finding = ReviewFinding {
        id: ReviewFindingId::new(),
        contribution: c.id.clone(),
        kind: ReviewFindingKind::NoIssueFound,
        statement: "Checked".into(),
        evidence_paths: paths,
    };
    f.store
        .record_team_contribution(&c, &[finding], &f.context(0))
        .unwrap();
    let mut context = f.context(0);
    context.team.as_mut().unwrap().assignment = f.team.role_assignments[2].id.clone();
    assert_ne!(
        f.store
            .assess_team_review(&f.review.id, &context)
            .unwrap()
            .status,
        ReviewGateStatus::Satisfied
    );
    context.risk.severity = Severity::Low;
    assert_eq!(
        f.store
            .assess_team_review(&f.review.id, &context)
            .unwrap()
            .status,
        ReviewGateStatus::Satisfied
    );
}
#[test]
fn action_hash_excludes_assessments_but_includes_plan_revision() {
    use hardknock::plan::PlanRuntimeContext;
    let f = Fixture::new();
    let mut context = f.context(2);
    let original = review_action_hash(&context).unwrap();
    context.team.as_mut().unwrap().assessment = Some(TeamAuthorityAssessment {
        review: None,
        allowed: true,
        action: RoleActionClass::Execute,
        reasons: vec![],
    });
    assert_eq!(original, review_action_hash(&context).unwrap());
    context.plan = Some(PlanRuntimeContext {
        run: PlanRunId::new(),
        plan: ExecutionPlanId::new(),
        revision: 1,
        next_step: PlanStepId::new(),
        validity: None,
        crossed_commitments: vec![],
    });
    let one = review_action_hash(&context).unwrap();
    context.plan.as_mut().unwrap().revision = 2;
    assert_ne!(one, review_action_hash(&context).unwrap());
}
#[test]
fn review_assessment_hash_is_stable_and_changes_when_evidence_changes() {
    let f = Fixture::new();
    f.ready();
    let before = f.assessment();
    assert_eq!(before, f.assessment());
    let p = f.path("third", EvidenceOutcome::Supports);
    f.submit(
        1,
        ContributionType::Review,
        [p].into(),
        Some(ReviewFindingKind::NoIssueFound),
    );
    let after = f.assessment();
    assert_eq!(after.status, ReviewGateStatus::Satisfied);
    assert_ne!(before.evidence_hash, after.evidence_hash);
}

#[test]
fn review_expiry_is_checked_without_sleeping() {
    let f = Fixture::new();
    f.ready();
    assert_eq!(
        f.store
            .assess_team_review_at(
                &f.review.id,
                &f.context(2),
                f.review.created_at - Duration::seconds(1)
            )
            .unwrap()
            .status,
        ReviewGateStatus::Blocked
    );
    let result = f
        .store
        .assess_team_review_at(&f.review.id, &f.context(2), f.review.expires_at)
        .unwrap();
    assert_eq!(result.status, ReviewGateStatus::Blocked);
}
#[test]
fn harmless_task_rename_cannot_change_review_identity() {
    let f = Fixture::new();
    let mut c = f.context(2);
    let hash = review_action_hash(&c).unwrap();
    c.task.description = "Renamed task".into();
    assert_eq!(hash, review_action_hash(&c).unwrap());
}

fn handoff_request(f: &Fixture, paths: BTreeSet<EvidencePathId>) -> AgentHandoffRequest {
    let source = f
        .store
        .team_contributions(&f.review.id)
        .unwrap()
        .into_iter()
        .find(|c| c.contribution_type == ContributionType::Review)
        .unwrap();
    AgentHandoffRequest {
        id: AgentHandoffId::new(),
        review: f.review.id.clone(),
        from_contribution: source.id.clone(),
        to_assignment: f.team.role_assignments[2].id.clone(),
        payload: StructuredHandoff {
            claims: [f.review.target.claim.clone()].into(),
            observations: paths,
            contributions: [source.id].into(),
        },
        classification: HandoffClassification::Operational,
        knowledge_snapshot: None,
    }
}
#[test]
fn structured_handoff_preserves_evidence_paths_without_creating_support() {
    let f = Fixture::new();
    let paths = f.ready();
    let before = f.store.team_evidence(&f.review.id).unwrap();
    let request = handoff_request(&f, paths.clone());
    let h = f
        .store
        .create_agent_handoff(&request, &f.context(1))
        .unwrap();
    h.verify().unwrap();
    let received = f
        .store
        .receive_agent_handoff(&request.id, &f.context(2))
        .unwrap();
    assert_eq!(received.content_hash, h.content_hash);
    assert_eq!(received.request.payload.observations, paths);
    assert_eq!(
        f.store
            .evidence_paths(&f.review.target.claim)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        f.store.team_evidence(&f.review.id).unwrap().fused.diversity,
        before.fused.diversity
    );
}
#[test]
fn handoff_rejects_wrong_sender_recipient_and_action() {
    let f = Fixture::new();
    let request = handoff_request(&f, f.ready());
    assert!(
        f.store
            .create_agent_handoff(&request, &f.context(0))
            .is_err()
    );
    f.store
        .create_agent_handoff(&request, &f.context(1))
        .unwrap();
    assert!(
        f.store
            .receive_agent_handoff(&request.id, &f.context(0))
            .is_err()
    );
    let mut wrong = f.context(2);
    wrong.proposed_action = None;
    assert!(f.store.receive_agent_handoff(&request.id, &wrong).is_err());
}
#[test]
fn handoff_revision_change_blocks_delivery_but_preserves_history() {
    let f = Fixture::new();
    let request = handoff_request(&f, f.ready());
    let mut h = f
        .store
        .create_agent_handoff(&request, &f.context(1))
        .unwrap();
    h.request.payload.claims.clear();
    assert!(h.verify().is_err());
    let mut t = f.team.clone();
    t.revision += 1;
    f.store.save_agent_team(&t).unwrap();
    assert!(
        f.store
            .receive_agent_handoff(&request.id, &f.context(2))
            .is_err()
    );
    assert!(f.store.agent_handoff(&request.id).is_ok());
}
#[test]
fn sensitive_handoff_and_raw_scratchpad_are_not_accepted() {
    let f = Fixture::new();
    let mut request = handoff_request(&f, f.ready());
    for class in [
        HandoffClassification::Sensitive,
        HandoffClassification::Restricted,
    ] {
        request.classification = class;
        assert!(
            f.store
                .create_agent_handoff(&request, &f.context(1))
                .is_err()
        );
    }
    let value = serde_json::json!({"claims":[],"observations":[],"contributions":[],"scratchpad":"secret token AKIAABCDEFGHIJKLMNOP"});
    assert!(serde_json::from_value::<StructuredHandoff>(value).is_err());
}
#[test]
fn handoff_cannot_add_uncited_observations_or_unbound_snapshot() {
    let f = Fixture::new();
    let mut request = handoff_request(&f, f.ready());
    request
        .payload
        .observations
        .insert(f.path("uncited", EvidenceOutcome::Supports));
    assert!(
        f.store
            .create_agent_handoff(&request, &f.context(1))
            .is_err()
    );
    request.payload.observations.clear();
    request.knowledge_snapshot = Some(KnowledgeSnapshotId::new());
    assert!(
        f.store
            .create_agent_handoff(&request, &f.context(1))
            .is_err()
    );
}
#[test]
fn handoff_requires_recipient_observation_authority() {
    let mut f = Fixture::new();
    f.team.roles[2]
        .prohibited_actions
        .insert(RoleActionClass::Observe);
    f.team.revision += 1;
    f.store.save_agent_team(&f.team).unwrap();
    f.review.id = TeamReviewId::new();
    f.review.team_revision = f.team.revision;
    f.review = f.store.create_team_review(&f.review).unwrap();
    let request = handoff_request(&f, f.ready());
    assert!(
        f.store
            .create_agent_handoff(&request, &f.context(1))
            .is_err()
    );
}
