// SPDX-License-Identifier: Apache-2.0
use super::{config::BridgeConfig, privacy::redact, protocol::*};
use crate::{
    Result,
    capability::{IsolationLevel, RealityRequirements},
    curriculum::Severity,
    development::{ActiveExperienceSet, EvidenceState, ExperienceRef},
    effects::{ExternalityClass, ReversibilityClass},
    experience::ExperienceContext,
    lesson::{ActionPattern, Lesson, LessonStatus},
    resilience::{Recovery, RecoveryStatus, Reflex, ReflexStatus},
    retrieval::{QueryContext, Recommendation, RetrievedLesson, freshness_score},
    store::Store,
};

#[derive(Clone, Default)]
pub struct ExperienceHotCache {
    pub lessons: Vec<Lesson>,
    pub reflexes: Vec<Reflex>,
    pub recoveries: Vec<Recovery>,
    pub freshness:
        std::collections::HashMap<crate::core::LessonId, crate::development::FreshnessBasis>,
    pub development: crate::development::DevelopmentConfig,
    pub reflex_freshness:
        std::collections::HashMap<crate::core::ReflexId, crate::development::FreshnessBasis>,
}

pub struct RuntimeEvaluationRequest<'a> {
    pub context: &'a ExperienceContext,
    pub proposed: &'a ActionProposed,
    pub failures: u32,
    pub bridge: &'a BridgeConfig,
    pub runtime: &'a super::config::RuntimeConfig,
    pub agent: &'a AgentIdentity,
    pub task: &'a str,
}

impl ExperienceHotCache {
    pub fn load(store: &Store) -> Result<Self> {
        let lessons: Vec<_> = store
            .all_lessons()?
            .into_iter()
            .filter(|l| {
                matches!(
                    l.status,
                    LessonStatus::CounterfactuallySupported | LessonStatus::Validated
                )
            })
            .collect();
        let reflexes = store.reflexes()?;
        let recoveries = store.recoveries()?;
        let observations: std::collections::HashMap<_, _> = store
            .reflex_freshness_observations()?
            .into_iter()
            .map(|e| (e.id.clone(), e))
            .collect();
        let mut reflex_freshness = std::collections::HashMap::new();
        for r in &reflexes {
            let source = store.chaos_trial(&r.source_trial)?.experience_id;
            if let Some(origin) = observations.get(&source) {
                let supports = crate::development::support_ids(&r.evidence);
                let latest = supports
                    .iter()
                    .filter_map(|id| observations.get(id))
                    .filter(|e| e.outcome == crate::experience::Outcome::Success)
                    .max_by_key(|e| e.created_at)
                    .unwrap_or(origin);
                let contradicted = r.evidence.iter().any(|e| {
                    matches!(
                        e,
                        crate::lesson::EvidenceRef::Experience {
                            relationship: crate::lesson::EvidenceRelationship::Contradicts,
                            ..
                        } | crate::lesson::EvidenceRef::Trial {
                            relationship: crate::lesson::EvidenceRelationship::Contradicts,
                            ..
                        }
                    )
                });
                reflex_freshness.insert(
                    r.id.clone(),
                    crate::development::FreshnessBasis {
                        origin_context: Some(origin.context.clone()),
                        last_supported_at: latest.created_at,
                        context: latest.context.clone(),
                        agent: latest.agent.clone(),
                        contradicted,
                    },
                );
            }
        }
        Ok(Self {
            freshness: store.lesson_freshness_bases(&lessons)?,
            development: super::config::Config::load(&store.home)?.development,
            lessons,
            reflexes,
            recoveries,
            reflex_freshness,
        })
    }
    pub fn retrieve(
        &self,
        context: &ExperienceContext,
        task: &str,
        actions: Vec<ActionPattern>,
    ) -> Vec<RetrievedLesson> {
        let query = QueryContext::new(context, task, actions);
        let mut matches: Vec<_> = self
            .lessons
            .iter()
            .filter_map(|lesson| {
                let basis = self.freshness.get(&lesson.id)?;
                if !matches!(
                    crate::development::assess_freshness(
                        basis,
                        &query,
                        None,
                        chrono::Utc::now(),
                        &self.development
                    )
                    .state,
                    crate::development::EvidenceState::Fresh
                        | crate::development::EvidenceState::Aging
                ) {
                    return None;
                }
                let (relevance, matched_context) = freshness_score(
                    lesson,
                    &query,
                    self.freshness.get(&lesson.id),
                    &self.development,
                    chrono::Utc::now(),
                );
                if !lesson.context_match.matches(context) || f64::from(relevance) < 0.4 {
                    return None;
                }
                Some(RetrievedLesson {
                    lesson: lesson.clone(),
                    relevance,
                    matched_context,
                    recommendation: if f64::from(relevance) >= 0.7 {
                        Recommendation::Recommend
                    } else {
                        Recommendation::Informational
                    },
                })
            })
            .collect();
        matches.sort_by(|a, b| {
            f64::from(b.relevance)
                .total_cmp(&f64::from(a.relevance))
                .then_with(|| a.lesson.id.to_string().cmp(&b.lesson.id.to_string()))
        });
        matches.truncate(self.development.max_lessons);
        matches
    }
    pub fn evaluate(
        &self,
        context: &ExperienceContext,
        proposed: &ActionProposed,
        failures: u32,
        config: &BridgeConfig,
    ) -> ActionDecision {
        let NormalizedAction::Shell { command, cwd } = &proposed.action else {
            return ActionDecision::Continue;
        };
        if config
            .policy
            .blocked_shell_commands
            .iter()
            .any(|c| c.trim() == command.trim())
        {
            return ActionDecision::Block {
                reason: "Exact command forbidden by local user policy".into(),
                authority: DecisionAuthority::UserPolicy,
            };
        }
        if config
            .policy
            .approval_shell_commands
            .iter()
            .any(|c| c.trim() == command.trim())
        {
            return ActionDecision::RequireApproval {
                reason: "Exact command requires user approval under local policy".into(),
                evidence: vec![],
            };
        }
        // No filesystem or database access here. Do not carry repository scope to another cwd.
        if cwd != &context.environment.cwd.to_string_lossy() {
            return ActionDecision::Continue;
        }
        let action = ActionPattern::shell(command);
        let mut supported = None;
        let mut activated = 0;
        for reflex in &self.reflexes {
            if activated >= self.development.max_reflexes {
                break;
            }
            let Some(basis) = self.reflex_freshness.get(&reflex.id) else {
                continue;
            };
            let health = crate::development::assess_freshness(
                basis,
                &QueryContext::new(context, "", vec![]),
                None,
                chrono::Utc::now(),
                &self.development,
            );
            if !matches!(
                health.state,
                crate::development::EvidenceState::Fresh | crate::development::EvidenceState::Aging
            ) {
                continue;
            }
            let t = &reflex.trigger;
            if !matches!(
                reflex.status,
                ReflexStatus::Active | ReflexStatus::Supported
            ) || !t.context.matches(context)
                || t.proposed_action != action
                || t.repeated_failures.is_some_and(|n| failures < n)
                || (t.no_state_change && !proposed.context.no_state_change)
                || (t.config_changed && !proposed.context.config_changed)
            {
                continue;
            }
            activated += 1;
            let evidence = vec![EvidenceRef {
                id: reflex.id.to_string(),
                kind: "reflex".into(),
            }];
            if reflex.status == ReflexStatus::Active {
                return ActionDecision::Replan {
                    reason: format!(
                        "Active reflex {} matched this action and observed conditions; reconsider the strategy",
                        reflex.id
                    ),
                    evidence,
                };
            }
            supported = Some(ActionDecision::Warn {
                message: format!(
                    "Supported reflex {} matched; review its tested failure conditions",
                    reflex.id
                ),
                evidence,
            });
        }
        if let Some(decision) = supported {
            return decision;
        }
        let matches = self.retrieve(context, "", vec![action]);
        if let Some(m) = matches.iter().find(|m| {
            m.lesson
                .avoid
                .as_ref()
                .is_some_and(|a| a.matches_shell(command))
        }) {
            return ActionDecision::Advise {
                message: redact(
                    &format!(
                        "{} Prefer: {}. Reconsider when context differs.",
                        m.lesson.claim,
                        m.lesson
                            .prefer
                            .as_ref()
                            .and_then(ActionPattern::shell_script)
                            .unwrap_or("review the evidence")
                    ),
                    2048,
                ),
                evidence: vec![EvidenceRef {
                    id: m.lesson.id.to_string(),
                    kind: "lesson".into(),
                }],
            };
        }
        ActionDecision::Continue
    }

    /// Central adaptive policy path used by native pre-action hooks. Everything
    /// required here is already in the hot cache; no model or database lookup
    /// occurs before the controller returns.
    pub fn evaluate_runtime(
        &self,
        request: RuntimeEvaluationRequest<'_>,
    ) -> Result<(
        crate::runtime::RuntimeDecisionContext,
        crate::runtime::RuntimeDecisionEvaluation,
    )> {
        use crate::runtime::*;
        let RuntimeEvaluationRequest {
            context,
            proposed,
            failures,
            bridge,
            runtime,
            agent,
            task,
        } = request;
        let action_pattern = match &proposed.action {
            NormalizedAction::Shell { command, .. } => Some(ActionPattern::shell(command)),
            NormalizedAction::FileWrite { path } | NormalizedAction::FileDelete { path } => {
                Some(ActionPattern::FileOperation {
                    pattern: path.clone(),
                })
            }
            NormalizedAction::FileRead { .. } => None,
            action => Some(ActionPattern::Custom {
                kind: "normalized_action".into(),
                value: serde_json::to_string(action)?,
            }),
        };
        let query = QueryContext::new(context, task, action_pattern.iter().cloned().collect());
        let lessons = self.retrieve(context, task, action_pattern.iter().cloned().collect());
        let mut stale = false;
        let mut contradicted = false;
        for lesson in &self.lessons {
            if !lesson.context_match.matches(context)
                || action_pattern.as_ref().is_some_and(|action| {
                    lesson.avoid.as_ref() != Some(action) && lesson.prefer.as_ref() != Some(action)
                })
            {
                continue;
            }
            if let Some(basis) = self.freshness.get(&lesson.id) {
                let health = crate::development::assess_freshness(
                    basis,
                    &query,
                    None,
                    chrono::Utc::now(),
                    &self.development,
                );
                stale |= health.state == EvidenceState::Stale;
                contradicted |= health.state == EvidenceState::Contradicted;
            }
        }
        let mut reflexes = Vec::new();
        let mut advisory_reflexes = Vec::new();
        let mut reflex_items = Vec::new();
        for reflex in self.reflexes.iter().filter(|reflex| {
            let trigger = &reflex.trigger;
            matches!(
                reflex.status,
                ReflexStatus::Active | ReflexStatus::Supported
            ) && trigger.context.matches(context)
                && action_pattern.as_ref() == Some(&trigger.proposed_action)
                && trigger
                    .repeated_failures
                    .is_none_or(|required| failures >= required)
                && (!trigger.no_state_change || proposed.context.no_state_change)
                && (!trigger.config_changed || proposed.context.config_changed)
        }) {
            let reference = ReflexRef {
                id: reflex.id.clone(),
                version: reflex.version,
            };
            if reflex.status == ReflexStatus::Active {
                reflexes.push(reference);
            } else {
                advisory_reflexes.push(reference);
            }
            reflex_items.push(ExperienceRef {
                kind: "reflex".into(),
                id: reflex.id.to_string(),
                revision: u64::from(reflex.version),
            });
        }
        let risk = match proposed.action {
            NormalizedAction::FileRead { .. } => RuntimeRiskAssessment {
                severity: Severity::Informational,
                ..Default::default()
            },
            NormalizedAction::Shell { .. } | NormalizedAction::FileWrite { .. } => {
                RuntimeRiskAssessment::default()
            }
            NormalizedAction::FileDelete { .. } | NormalizedAction::ToolCall { .. } => {
                RuntimeRiskAssessment {
                    severity: Severity::Medium,
                    reversibility: ReversibilityClass::Compensatable,
                    externality: ExternalityClass::HostLocal,
                    assurance_requirement: None,
                    effect_risk: None,
                    rationale: vec![
                        "Mutation or tool call requires context-sensitive review".into(),
                    ],
                }
            }
            NormalizedAction::Network { .. } => RuntimeRiskAssessment {
                severity: Severity::High,
                reversibility: ReversibilityClass::Unknown,
                externality: ExternalityClass::ExternalSystem,
                assurance_requirement: None,
                effect_risk: None,
                rationale: vec!["Direct network action may create an external effect".into()],
            },
            NormalizedAction::Custom { .. } => RuntimeRiskAssessment {
                severity: Severity::Medium,
                reversibility: ReversibilityClass::Unknown,
                externality: ExternalityClass::Unknown,
                assurance_requirement: None,
                effect_risk: None,
                rationale: vec!["Custom action has no structured risk adapter".into()],
            },
        };
        let blocked = action_pattern
            .as_ref()
            .and_then(ActionPattern::shell_script)
            .is_some_and(|command| {
                bridge
                    .policy
                    .blocked_shell_commands
                    .iter()
                    .any(|blocked| blocked.trim() == command.trim())
            });
        let approval = action_pattern
            .as_ref()
            .and_then(ActionPattern::shell_script)
            .is_some_and(|command| {
                bridge
                    .policy
                    .approval_shell_commands
                    .iter()
                    .any(|required| required.trim() == command.trim())
            });
        let experiment = ExperimentCapabilitySummary {
            mode: runtime.experiment.mode,
            safe_reality_available: matches!(proposed.action, NormalizedAction::Shell { .. }),
            // Git worktrees do not virtualize arbitrary external effects. A
            // shell action can only be suggested, never auto-run by this hook.
            effect_safe: matches!(proposed.action, NormalizedAction::Shell { .. }),
            budget: bridge.experiment_budget.clone(),
            budget_remaining: bridge.experiment_budget.max_realities > 0,
            requirements: RealityRequirements {
                filesystem_isolation: IsolationLevel::Cooperative,
                process_isolation: IsolationLevel::None,
                network_isolation: IsolationLevel::None,
                credential_isolation: IsolationLevel::None,
                effect_gating: false,
            },
        };
        let local_supported = !lessons.is_empty();
        let known_failure_precursor = action_pattern.as_ref().is_some_and(|action| {
            lessons
                .iter()
                .any(|retrieved| retrieved.lesson.avoid.as_ref() == Some(action))
        }) && !stale
            && !contradicted;
        let mut uncertainty_reasons = Vec::new();
        if !local_supported {
            uncertainty_reasons.push(UncertaintyReason::MissingExperience);
        }
        if contradicted {
            uncertainty_reasons.push(UncertaintyReason::ContradictoryEvidence);
        }
        if stale {
            uncertainty_reasons.push(UncertaintyReason::FailedPrediction);
        }
        let decision_context = RuntimeDecisionContext {
            session_id: crate::core::HardknockSessionId::from_external(
                &proposed.hardknock_session_id,
            ),
            agent: crate::core::AgentIdentity {
                kind: agent.name.clone(),
                executable: format!("bridge:{}", agent.name),
                version: agent.version.clone(),
                model: agent.model.clone(),
            },
            task: TaskDescriptor {
                description: task.into(),
                family: None,
                tags: context.tags.clone(),
            },
            query_context: query,
            proposed_action: Some(proposed.action.clone()),
            proposed_effect: None,
            relevant_experience: ActiveExperienceSet {
                lessons,
                reflexes: reflex_items,
                recoveries: Vec::new(),
            },
            assurance: Default::default(),
            operating_envelope: None,
            capability_context: CapabilityContext {
                available: Vec::new(),
                missing: Vec::new(),
                required_available: true,
                commit_authority: false,
                effect_adapter_available: true,
                isolation_sufficient: risk.severity < Severity::High,
                isolation_level: IsolationLevel::Cooperative,
                governance: GovernanceContext {
                    hard_policy_blocked: blocked,
                    block_reason: blocked
                        .then(|| "Exact command forbidden by local user policy".into()),
                    approval_required: approval,
                    approval_reason: approval
                        .then(|| "Exact command requires user approval under local policy".into()),
                },
            },
            risk,
            uncertainty: RuntimeUncertainty {
                level: if contradicted || stale || !local_supported {
                    UncertaintyLevel::High
                } else {
                    UncertaintyLevel::Low
                },
                reasons: uncertainty_reasons,
                candidate_strategies: Vec::new(),
            },
            available_recovery: Vec::new(),
            available_experiments: experiment,
            knowledge_signals: KnowledgeSignals {
                local_supported,
                local_contradicted: contradicted,
                evidence_stale: stale,
                context_in_scope: true,
                validated_skill: false,
                applicable_lesson: local_supported,
                known_failure_precursor,
                remote_advisory_only: false,
                known_gap_matches: false,
            },
            applicable_skills: Vec::new(),
            matched_reflexes: reflexes,
            advisory_reflexes,
            failure_signature: None,
            known_unknowns: Vec::new(),
            externally_supported: false,
            tool_candidates: Vec::new(),
            epistemic: None,
            diversity_requirements: Vec::new(),
        };
        let evaluation = DeterministicRuntimeController::with_config(runtime.policy_config())?
            .evaluate(&decision_context)?;
        Ok((decision_context, evaluation))
    }

    pub fn matching_recoveries(
        &self,
        context: &ExperienceContext,
        signature: &str,
    ) -> Vec<crate::runtime::RecoveryRef> {
        let stale_after = chrono::Duration::days(i64::from(self.development.stale_after_days));
        self.recoveries
            .iter()
            .filter(|recovery| {
                matches!(
                    recovery.status,
                    RecoveryStatus::Supported | RecoveryStatus::Validated
                ) && recovery.failure_signature.signature == signature
                    && recovery.context.matches(context)
            })
            .take(self.development.max_recoveries)
            .map(|recovery| crate::runtime::RecoveryRef {
                id: recovery.id.clone(),
                version: recovery.version,
                failure_signature: recovery.failure_signature.signature.clone(),
                confidence: recovery.confidence,
                fresh: chrono::Utc::now().signed_duration_since(recovery.updated_at) <= stale_after,
                scope_matches: true,
            })
            .collect()
    }
}

pub fn bridge_decision_from_runtime(
    evaluation: &crate::runtime::RuntimeDecisionEvaluation,
    autonomy: crate::runtime::RuntimeAutonomy,
) -> ActionDecision {
    use crate::runtime::{GovernanceDisposition, RuntimeAutonomy, RuntimeDecision};
    let evidence = evaluation
        .evidence
        .iter()
        .filter_map(|reference| match reference {
            crate::runtime::EvidenceRef::Lesson(reference) => Some(EvidenceRef {
                id: reference.id.to_string(),
                kind: "lesson".into(),
            }),
            crate::runtime::EvidenceRef::Reflex(reference) => Some(EvidenceRef {
                id: reference.id.to_string(),
                kind: "reflex".into(),
            }),
            crate::runtime::EvidenceRef::Recovery { id, .. } => Some(EvidenceRef {
                id: id.to_string(),
                kind: "recovery".into(),
            }),
            _ => None,
        })
        .collect::<Vec<_>>();
    if evaluation.governance == GovernanceDisposition::SecurityBlocked {
        return ActionDecision::Block {
            reason: "Hard policy or capability authority overrides runtime experience".into(),
            authority: DecisionAuthority::UserPolicy,
        };
    }
    if autonomy == RuntimeAutonomy::Observe {
        return ActionDecision::Continue;
    }
    match &evaluation.decision {
        RuntimeDecision::Act(decision) => match &decision.warning {
            None => ActionDecision::Continue,
            Some(_) if evidence.is_empty() => ActionDecision::Continue,
            Some(warning) if evidence.iter().any(|item| item.kind == "reflex") => {
                ActionDecision::Warn {
                    message: warning.clone(),
                    evidence,
                }
            }
            Some(warning) => ActionDecision::Advise {
                message: warning.clone(),
                evidence,
            },
        },
        RuntimeDecision::Experiment(decision) => ActionDecision::Advise {
            message: format!(
                "Hardknock recommends a bounded experiment: {}",
                decision.reason
            ),
            evidence,
        },
        // Active Reflex interception predates the autonomy setting. Preserve
        // that established Bridge contract while other V0.12 replans remain
        // advisory unless adaptive/governed mode is explicitly configured.
        RuntimeDecision::Replan(decision) if !decision.matched_reflexes.is_empty() => {
            ActionDecision::Replan {
                reason: decision.reason.clone(),
                evidence,
            }
        }
        // The legacy Bridge contract delivered matching Lessons as advice;
        // preserve that wire behavior while the durable runtime record retains
        // the stronger, explicit REPLAN classification.
        RuntimeDecision::Replan(decision)
            if decision.matched_reflexes.is_empty() && !decision.relevant_lessons.is_empty() =>
        {
            ActionDecision::Advise {
                message: format!("Hardknock recommends replanning: {}", decision.reason),
                evidence,
            }
        }
        RuntimeDecision::Replan(decision) if autonomy == RuntimeAutonomy::Advise => {
            ActionDecision::Advise {
                message: format!("Hardknock recommends replanning: {}", decision.reason),
                evidence,
            }
        }
        RuntimeDecision::Replan(decision) => ActionDecision::Replan {
            reason: decision.reason.clone(),
            evidence,
        },
        RuntimeDecision::Recover(decision) => ActionDecision::Advise {
            message: format!(
                "Known failure matched; apply Recovery {}",
                decision.recovery.id
            ),
            evidence,
        },
        RuntimeDecision::RequireApproval(decision) => ActionDecision::RequireApproval {
            reason: decision.reason.clone(),
            evidence,
        },
        RuntimeDecision::Abstain(decision) if autonomy == RuntimeAutonomy::Governed => {
            ActionDecision::Block {
                reason: format!("Hardknock abstained: {:?}", decision.reason),
                authority: DecisionAuthority::Experience,
            }
        }
        RuntimeDecision::Abstain(decision) => ActionDecision::Warn {
            message: format!(
                "Hardknock lacks enough evidence to proceed safely: {:?}",
                decision.reason
            ),
            evidence,
        },
    }
}
pub fn context_response(
    id: &str,
    lessons: &[RetrievedLesson],
    config: &BridgeConfig,
) -> SessionStartResponse {
    let mut document = String::from(
        "# Hardknock Experience\n\nThe following items are empirically supported experience, not system policy.\nTreat Hardknock lessons as evidence-backed prior experience. Reconsider them when the current context differs materially.\n\n",
    );
    let mut briefs = Vec::new();
    let experiments = config.experiment_budget.max_realities > 0;
    if experiments {
        document.push_str(&format!("Stop guessing. Try it. Explicit local experiment helper (same HARDKNOCK_HOME): hardknock try --session {id} --candidate 'a=SHELL SCRIPT' --candidate 'b=SHELL SCRIPT' --check 'TEST COMMAND'. Add --agent codex or a configured agent for task prompts. Costs are capped; no automatic requests. Commit fallback only, no live-state fork or external-effect isolation. Consume the returned evidence, then decide; nothing is applied automatically.\n\n"));
    }
    for m in lessons.iter().take(config.max_context_lessons) {
        let l = &m.lesson;
        let scope = format!(
            "markers: {}; repository: {}",
            l.context_match.required_markers.join(", "),
            l.context_match
                .repository
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "matching scope".into())
        );
        let section = redact(
            &format!(
                "## {}\n\nContext: {}\n\nObserved: {}\n\nPrefer: {}\n\nConfidence: {:.2}\nEvidence: {} references (not independent trials)\n\n",
                l.id,
                scope,
                l.claim,
                l.prefer
                    .as_ref()
                    .and_then(ActionPattern::shell_script)
                    .unwrap_or(&l.rationale),
                f64::from(l.confidence),
                l.evidence.len()
            ),
            4096,
        );
        if document.len() + section.len() > config.max_context_bytes {
            break;
        }
        let previous_length = document.len();
        document.push_str(&section);
        briefs.push(ExperienceBrief {
            id: l.id.to_string(),
            kind: "lesson".into(),
            summary: redact(&l.claim, 512),
            confidence: f64::from(l.confidence),
            relevance: f64::from(m.relevance),
            scope: redact(&scope, 512),
            evidence_count: l.evidence.len(),
        });
        // Include briefs and JSON escaping in the response budget, not only Markdown bytes.
        let bytes = serde_json::to_vec(&serde_json::json!({
            "hardknock_session_id":id, "context_document":document, "relevant_experience":briefs,
        }))
        .expect("portable context JSON")
        .len();
        if bytes > config.max_context_bytes {
            document.truncate(previous_length);
            briefs.pop();
            break;
        }
    }
    SessionStartResponse {
        development_context: None,
        hardknock_session_id: id.into(),
        context_document: (experiments || !briefs.is_empty()).then_some(document),
        relevant_experience: briefs,
    }
}
