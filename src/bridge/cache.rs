// SPDX-License-Identifier: Apache-2.0
use super::{config::BridgeConfig, privacy::redact, protocol::*};
use crate::{
    Result,
    experience::ExperienceContext,
    lesson::{ActionPattern, Lesson, LessonStatus},
    resilience::{Reflex, ReflexStatus},
    retrieval::{
        DeterministicRelevance, QueryContext, Recommendation, RelevancePolicy, RetrievedLesson,
    },
    store::{LessonQuery, LessonStore, Store},
};

#[derive(Clone, Default)]
pub struct ExperienceHotCache {
    pub lessons: Vec<Lesson>,
    pub reflexes: Vec<Reflex>,
}
impl ExperienceHotCache {
    pub fn load(store: &Store) -> Result<Self> {
        let lessons = LessonStore::list(store, LessonQuery::default())?
            .into_iter()
            .map(|l| store.lesson(&l.id))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|l| {
                matches!(
                    l.status,
                    LessonStatus::CounterfactuallySupported | LessonStatus::Validated
                )
            })
            .collect();
        Ok(Self {
            lessons,
            reflexes: store.reflexes()?,
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
                let (relevance, matched_context) = DeterministicRelevance.score(lesson, &query);
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
        for reflex in &self.reflexes {
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
        hardknock_session_id: id.into(),
        context_document: (!briefs.is_empty()).then_some(document),
        relevant_experience: briefs,
    }
}
