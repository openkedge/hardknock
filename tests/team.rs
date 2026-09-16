// SPDX-License-Identifier: Apache-2.0
use chrono::{Duration, Utc};
use hardknock::{core::*, hierarchy::*, store::Store, team::*};
use std::collections::{BTreeMap, BTreeSet};
fn fixture() -> AgentTeam {
    let now = Utc::now();
    let role = AgentRole::builtin(BuiltInAgentRole::Executor);
    let members: Vec<_> = (0..3)
        .map(|_| AgentTeamMember {
            id: TeamMemberId::new(),
            session: HardknockSessionId::new(),
            agent: AgentIdentity {
                kind: "test".into(),
                executable: "test".into(),
                version: None,
                model: Some("shared-model".into()),
            },
        })
        .collect();
    AgentTeam {
        id: AgentTeamId::new(),
        revision: 1,
        role_assignments: members
            .iter()
            .map(|m| RoleAssignment {
                id: RoleAssignmentId::new(),
                member: m.id.clone(),
                role: role.id.clone(),
                scope: KnowledgeScope::default(),
                valid_from: now - Duration::minutes(1),
                valid_until: now + Duration::hours(1),
            })
            .collect(),
        members,
        authority: role.authority(),
        roles: vec![role],
        max_delegation_depth: 2,
        created_at: now,
    }
}
fn delegation(t: &AgentTeam) -> Delegation {
    Delegation {
        id: DelegationId::new(),
        team: t.id.clone(),
        team_revision: t.revision,
        delegator: t.members[0].id.clone(),
        delegate: t.members[1].id.clone(),
        source_assignment: t.role_assignments[0].id.clone(),
        parent: None,
        role: t.roles[0].id.clone(),
        task_scope: KnowledgeScope::default(),
        authority: [RoleActionClass::Observe].into(),
        issued_at: Utc::now(),
        expires_at: Utc::now() + Duration::minutes(10),
    }
}
#[test]
fn builtins_do_not_grant_planner_commit_or_recovery_executor() {
    assert!(
        !AgentRole::builtin(BuiltInAgentRole::Planner)
            .authority()
            .contains(&RoleActionClass::Commit)
    );
    assert!(
        !AgentRole::builtin(BuiltInAgentRole::Reviewer)
            .authority()
            .contains(&RoleActionClass::Execute)
    );
    assert!(
        !AgentRole::builtin(BuiltInAgentRole::Recovery)
            .authority()
            .contains(&RoleActionClass::Execute)
    );
}
#[test]
fn duplicate_authenticated_session_is_not_an_independent_member() {
    let mut t = fixture();
    t.members[1].session = t.members[0].session.clone();
    assert!(t.validate().is_err());
}
#[test]
fn planner_cannot_delegate_commit_to_executor() {
    let mut t = fixture();
    let planner = AgentRole::builtin(BuiltInAgentRole::Planner);
    t.role_assignments[0].role = planner.id.clone();
    t.roles.push(planner);
    let mut d = delegation(&t);
    d.authority = [RoleActionClass::Commit].into();
    let all = BTreeMap::from([(d.id.clone(), d.clone())]);
    assert!(
        t.delegated_authority(&d.id, &all, &BTreeSet::new(), Utc::now())
            .is_err()
    );
}
#[test]
fn parent_revocation_expiry_and_revision_invalidate_child() {
    let t = fixture();
    let p = delegation(&t);
    let mut d = p.clone();
    d.id = DelegationId::new();
    d.parent = Some(p.id.clone());
    d.delegator = p.delegate.clone();
    d.delegate = t.members[2].id.clone();
    let all = BTreeMap::from([(p.id.clone(), p.clone()), (d.id.clone(), d.clone())]);
    assert!(
        t.delegated_authority(&d.id, &all, &BTreeSet::new(), Utc::now())
            .is_ok()
    );
    assert!(
        t.delegated_authority(&d.id, &all, &[p.id.clone()].into(), Utc::now())
            .is_err()
    );
    assert!(
        t.delegated_authority(&d.id, &all, &BTreeSet::new(), p.expires_at)
            .is_err()
    );
    let mut revised = t;
    revised.revision += 1;
    assert!(
        revised
            .delegated_authority(&d.id, &all, &BTreeSet::new(), Utc::now())
            .is_err()
    );
}
#[test]
fn delegation_cannot_expand_scope_or_lifetime() {
    let mut t = fixture();
    t.role_assignments[0].scope = KnowledgeScope {
        predicates: vec![ScopePredicate::Equals {
            key: "namespace".into(),
            value: ScopeValue::String("staging".into()),
        }],
    };
    let mut d = delegation(&t);
    let mut all = BTreeMap::from([(d.id.clone(), d.clone())]);
    assert!(
        t.delegated_authority(&d.id, &all, &BTreeSet::new(), Utc::now())
            .is_err()
    );
    d.task_scope = t.role_assignments[0].scope.clone();
    all.insert(d.id.clone(), d.clone());
    assert!(
        t.delegated_authority(&d.id, &all, &BTreeSet::new(), Utc::now())
            .is_ok()
    );
    d.expires_at = t.role_assignments[0].valid_until + Duration::seconds(1);
    all.insert(d.id.clone(), d.clone());
    assert!(
        t.delegated_authority(&d.id, &all, &BTreeSet::new(), Utc::now())
            .is_err()
    );
}
#[test]
fn authority_intersection_and_session_binding_are_required() {
    let t = fixture();
    let mut r = TeamAuthorityRequest {
        action: RoleActionClass::Commit,
        context: KnowledgeContext::default(),
        runtime_grant: t.authority.clone(),
        external_grant: BTreeSet::new(),
        now: Utc::now(),
    };
    let a = &t.role_assignments[0];
    let m = &t.members[0];
    assert!(
        t.authorize_assignment(&a.id, &m.id, &m.session, &r)
            .is_err()
    );
    r.external_grant = t.authority.clone();
    assert!(t.authorize_assignment(&a.id, &m.id, &m.session, &r).is_ok());
    assert!(
        t.authorize_assignment(&a.id, &m.id, &HardknockSessionId::new(), &r)
            .is_err()
    );
}
#[test]
fn delegation_depth_zero_and_cycles_fail_closed() {
    let mut t = fixture();
    let mut d = delegation(&t);
    let all = BTreeMap::from([(d.id.clone(), d.clone())]);
    t.max_delegation_depth = 0;
    assert!(
        t.delegated_authority(&d.id, &all, &BTreeSet::new(), Utc::now())
            .is_err()
    );
    t.max_delegation_depth = 2;
    d.parent = Some(d.id.clone());
    let all = BTreeMap::from([(d.id.clone(), d.clone())]);
    assert!(
        t.delegated_authority(&d.id, &all, &BTreeSet::new(), Utc::now())
            .is_err()
    );
}
#[test]
fn persistence_preserves_revocation_and_team_revision_history() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    let mut t = fixture();
    store.save_agent_team(&t).unwrap();
    let d = delegation(&t);
    store.record_delegation(&d).unwrap();
    assert!(store.validate_delegation(&t.id, &d.id, Utc::now()).is_ok());
    store.revoke_delegation(&d.id, "scope withdrawn").unwrap();
    assert!(store.validate_delegation(&t.id, &d.id, Utc::now()).is_err());
    assert!(store.record_delegation(&d).is_err());
    t.revision += 1;
    store.save_agent_team(&t).unwrap();
    assert!(store.save_agent_team(&t).is_err());
    assert_eq!(store.team_history(&t.id).unwrap().len(), 4);
    drop(store);
    let reopened = Store::open(home.path()).unwrap();
    assert!(
        reopened
            .validate_delegation(&t.id, &d.id, Utc::now())
            .is_err()
    );
    assert_eq!(reopened.agent_teams().unwrap().len(), 1);
}

fn runtime_context() -> hardknock::runtime::RuntimeDecisionContext {
    serde_json::from_str::<hardknock::runtime::RuntimeScenario>(include_str!(
        "../fixtures/runtime-scenarios/known-safe.json"
    ))
    .unwrap()
    .decision_context()
    .unwrap()
}
#[test]
fn runtime_discards_forged_assessment_and_checks_actual_action() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    let mut t = fixture();
    let planner = AgentRole::builtin(BuiltInAgentRole::Planner);
    t.role_assignments[0].role = planner.id.clone();
    t.roles.push(planner);
    let mut context = runtime_context();
    context.proposed_action = Some(hardknock::bridge::protocol::NormalizedAction::Shell {
        command: "true".into(),
        cwd: ".".into(),
    });
    context.session_id = t.members[0].session.clone();
    context.agent = t.members[0].agent.clone();
    context.team = Some(TeamRuntimeContext {
        review: None,
        team: t.id.clone(),
        revision: t.revision,
        member: t.members[0].id.clone(),
        assignment: t.role_assignments[0].id.clone(),
        delegation: None,
        assessment: Some(TeamAuthorityAssessment {
            review: None,
            allowed: true,
            action: RoleActionClass::Observe,
            reasons: vec![],
        }),
    });
    store.save_agent_team(&t).unwrap();
    store.attach_team_authority(&mut context).unwrap();
    assert!(
        !context
            .team
            .as_ref()
            .unwrap()
            .assessment
            .as_ref()
            .unwrap()
            .allowed
    );
    context.proposed_action = Some(hardknock::bridge::protocol::NormalizedAction::FileRead {
        path: "readme".into(),
    });
    store.attach_team_authority(&mut context).unwrap();
    assert!(
        context
            .team
            .as_ref()
            .unwrap()
            .assessment
            .as_ref()
            .unwrap()
            .allowed
    );
    context.team = None;
    assert!(store.attach_team_authority(&mut context).is_err());
}
#[test]
fn runtime_rechecks_revision_revocation_and_session() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    let mut t = fixture();
    store.save_agent_team(&t).unwrap();
    let mut d = delegation(&t);
    d.authority = [RoleActionClass::Execute].into();
    store.record_delegation(&d).unwrap();
    let mut context = runtime_context();
    context.proposed_action = Some(hardknock::bridge::protocol::NormalizedAction::Shell {
        command: "true".into(),
        cwd: ".".into(),
    });
    context.session_id = t.members[1].session.clone();
    context.agent = t.members[1].agent.clone();
    context.team = Some(TeamRuntimeContext {
        review: None,
        team: t.id.clone(),
        revision: 1,
        member: t.members[1].id.clone(),
        assignment: t.role_assignments[1].id.clone(),
        delegation: Some(d.id.clone()),
        assessment: None,
    });
    store.attach_team_authority(&mut context).unwrap();
    assert!(
        context
            .team
            .as_ref()
            .unwrap()
            .assessment
            .as_ref()
            .unwrap()
            .allowed
    );
    store.revoke_delegation(&d.id, "withdrawn").unwrap();
    store.attach_team_authority(&mut context).unwrap();
    assert!(
        !context
            .team
            .as_ref()
            .unwrap()
            .assessment
            .as_ref()
            .unwrap()
            .allowed
    );
    context.team.as_mut().unwrap().delegation = None;
    t.revision = 2;
    store.save_agent_team(&t).unwrap();
    store.attach_team_authority(&mut context).unwrap();
    assert!(
        !context
            .team
            .as_ref()
            .unwrap()
            .assessment
            .as_ref()
            .unwrap()
            .allowed
    );
    context.team.as_mut().unwrap().revision = 2;
    context.session_id = HardknockSessionId::new();
    store.attach_team_authority(&mut context).unwrap();
    assert!(
        !context
            .team
            .as_ref()
            .unwrap()
            .assessment
            .as_ref()
            .unwrap()
            .allowed
    );
}

#[test]
fn publication_rechecks_team_revision_without_rewriting_original_decision() {
    use hardknock::store::RuntimeStore;
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    let mut t = fixture();
    store.save_agent_team(&t).unwrap();
    let mut context = runtime_context();
    context.proposed_action = Some(hardknock::bridge::protocol::NormalizedAction::Shell {
        command: "true".into(),
        cwd: ".".into(),
    });
    context.session_id = t.members[0].session.clone();
    context.agent = t.members[0].agent.clone();
    context.team = Some(TeamRuntimeContext {
        review: None,
        team: t.id.clone(),
        revision: 1,
        member: t.members[0].id.clone(),
        assignment: t.role_assignments[0].id.clone(),
        delegation: None,
        assessment: None,
    });
    let decision = store
        .record_runtime_decision(&context, Default::default())
        .unwrap();
    assert!(
        decision
            .context
            .team
            .as_ref()
            .unwrap()
            .assessment
            .as_ref()
            .unwrap()
            .allowed
    );
    t.revision = 2;
    store.save_agent_team(&t).unwrap();
    assert!(
        store
            .persist_runtime_decision(&decision, Default::default())
            .is_err()
    );
    assert_eq!(decision.context.team.as_ref().unwrap().revision, 1);
}

#[test]
fn runtime_scope_requires_observation_even_without_knowledge_hierarchy() {
    use hardknock::knowledge_runtime::{ContextValue, ContextValueSource};
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    let mut t = fixture();
    t.role_assignments[0].scope = KnowledgeScope {
        predicates: vec![ScopePredicate::Equals {
            key: "namespace".into(),
            value: ScopeValue::String("staging".into()),
        }],
    };
    store.save_agent_team(&t).unwrap();
    let mut context = runtime_context();
    context.session_id = t.members[0].session.clone();
    context.agent = t.members[0].agent.clone();
    context.team = Some(TeamRuntimeContext {
        review: None,
        team: t.id.clone(),
        revision: 1,
        member: t.members[0].id.clone(),
        assignment: t.role_assignments[0].id.clone(),
        delegation: None,
        assessment: None,
    });
    context.context_observations.insert(
        "namespace".into(),
        vec![ContextValue {
            value: ScopeValue::String("staging".into()),
            source: ContextValueSource::AgentReported,
        }],
    );
    store.attach_team_authority(&mut context).unwrap();
    assert!(
        !context
            .team
            .as_ref()
            .unwrap()
            .assessment
            .as_ref()
            .unwrap()
            .allowed
    );
    context.context_observations.get_mut("namespace").unwrap()[0].source =
        ContextValueSource::RuntimeObserved;
    store.attach_team_authority(&mut context).unwrap();
    assert!(
        context
            .team
            .as_ref()
            .unwrap()
            .assessment
            .as_ref()
            .unwrap()
            .allowed
    );
}
