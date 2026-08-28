// SPDX-License-Identifier: Apache-2.0
use super::{config::BridgeConfig, privacy::redact, protocol::*};
use crate::{
    Result,
    experience::ExperienceContext,
    lesson::{ActionPattern, Lesson, LessonStatus},
    resilience::{Reflex, ReflexStatus},
    retrieval::{QueryContext, Recommendation, RetrievedLesson, freshness_score},
    store::Store,
};

#[derive(Clone, Default)]
pub struct ExperienceHotCache {
    pub lessons: Vec<Lesson>,
    pub reflexes: Vec<Reflex>,
    pub freshness:
        std::collections::HashMap<crate::core::LessonId, crate::development::FreshnessBasis>,
    pub development: crate::development::DevelopmentConfig,
    pub reflex_freshness:
        std::collections::HashMap<crate::core::ReflexId, crate::development::FreshnessBasis>,
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
