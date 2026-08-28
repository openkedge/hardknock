// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{
    budget::ExperienceBudget,
    experience::{Experience, ExperienceContext},
    resilience::Skill,
};

pub trait CurriculumPolicy {
    fn evaluate(
        &self,
        goal: &CurriculumGoal,
        capabilities: &RealityCapabilities,
        budget: &ExperienceBudget,
    ) -> CurriculumDecision;
}
pub struct LocalCurriculumPolicy;
/// Suggestions are candidates only. Execution still requires the ordinary planner and executor.
pub fn validate_suggestions(
    context: &CurriculumContext,
    suggestions: Vec<CurriculumSuggestion>,
) -> crate::Result<Vec<(CurriculumSuggestion, CurriculumDecision)>> {
    if suggestions.len() > 32 {
        return Err(crate::Error::InvalidInput(
            "At most 32 curriculum suggestions are accepted".into(),
        ));
    }
    Ok(suggestions
        .into_iter()
        .map(|s| {
            let decision = if s.rationale.is_empty()
                || s.rationale.len() > 1024
                || !context
                    .profile
                    .conditions
                    .iter()
                    .any(|c| c.name == s.condition && c.supported)
            {
                CurriculumDecision::Rejected
            } else {
                CurriculumDecision::RequiresApproval
            };
            (s, decision)
        })
        .collect())
}
impl CurriculumPolicy for LocalCurriculumPolicy {
    fn evaluate(
        &self,
        g: &CurriculumGoal,
        c: &RealityCapabilities,
        b: &ExperienceBudget,
    ) -> CurriculumDecision {
        if g.safety == TrialSafety::Unsupported
            || c.filesystem_isolation < IsolationLevel::Partial
            || b.max_realities == 0
            || b.max_duration_ms == Some(0)
        {
            CurriculumDecision::Rejected
        } else if g.safety == TrialSafety::RequiresApproval {
            CurriculumDecision::RequiresApproval
        } else if g.safety == TrialSafety::RequiresIsolation
            && (c.network_isolation < IsolationLevel::Isolated
                || c.external_effect_isolation < IsolationLevel::Isolated)
        {
            CurriculumDecision::Rejected
        } else {
            CurriculumDecision::Approved
        }
    }
}
pub trait CurriculumPriorityPolicy {
    fn priority(&self, gap: &EvidenceGap, context: &CurriculumContext) -> PriorityScore;
}
pub struct TransparentPriorityPolicy;
impl CurriculumPriorityPolicy for TransparentPriorityPolicy {
    fn priority(&self, gap: &EvidenceGap, context: &CurriculumContext) -> PriorityScore {
        let severity = match gap.dimension.as_str() {
            "recovery" | "contradiction" => 5,
            "credential_state" | "configuration" => 4,
            "freshness" | "reflex" => 3,
            "latency" => 1,
            _ => 2,
        };
        let exposure = context
            .packages
            .iter()
            .map(|p| p.evidence.usage.execution_count)
            .sum::<u64>()
            .min(20)
            + 1;
        let score = severity * 100 + exposure + gap.unknown_values.len().min(32) as u64;
        PriorityScore {
            score,
            priority: if severity >= 4 {
                Priority::High
            } else if severity >= 2 {
                Priority::Medium
            } else {
                Priority::Low
            },
            explanation: format!(
                "severity weight {severity} × 100 + capped observed executions {exposure} + {} unknown values; heuristic, not a probability",
                gap.unknown_values.len()
            ),
        }
    }
}
pub trait SkillMaturityPolicy {
    fn evaluate(&self, skill: &Skill, evidence: &SkillEvidenceSummary) -> SkillMaturity;
}
pub struct ConfiguredMaturityPolicy<'a>(pub &'a CurriculumConfig);
impl SkillMaturityPolicy for ConfiguredMaturityPolicy<'_> {
    fn evaluate(&self, s: &Skill, e: &SkillEvidenceSummary) -> SkillMaturity {
        if s.status == crate::resilience::SkillStatus::Retired {
            return SkillMaturity::Retired;
        }
        if e.unresolved_critical > 0 || e.base_failed {
            return SkillMaturity::Degraded;
        }
        if e.base_successes < 2 || e.freshness.stale {
            return if e.base_successes > 0 {
                SkillMaturity::Supported
            } else {
                SkillMaturity::Observed
            };
        }
        if e.tested_dimensions >= self.0.min_hardening_dimensions
            && (!self.0.require_high_severity_recovery || e.high_failure_recovery_gaps.is_empty())
            && (!self.0.require_reflex_negative_controls || e.reflex_check_gaps.is_empty())
        {
            SkillMaturity::Hardened
        } else {
            SkillMaturity::Validated
        }
    }
}
#[derive(Clone, Debug)]
pub struct EvidenceSummary {
    pub last_supported_at: chrono::DateTime<chrono::Utc>,
    pub environment: ExperienceContext,
    pub agent: crate::core::AgentIdentity,
}
pub type FreshnessStatus = EvidenceFreshness;
pub trait EvidenceFreshnessPolicy {
    fn evaluate(
        &self,
        evidence: &EvidenceSummary,
        current_context: &crate::retrieval::QueryContext,
    ) -> FreshnessStatus;
}
pub struct ConservativeFreshnessPolicy {
    pub now: chrono::DateTime<chrono::Utc>,
    pub age_days: u32,
}
impl EvidenceFreshnessPolicy for ConservativeFreshnessPolicy {
    fn evaluate(&self, e: &EvidenceSummary, c: &crate::retrieval::QueryContext) -> FreshnessStatus {
        let mut reasons = vec![];
        if e.environment.environment.fingerprint != c.environment.fingerprint {
            reasons.push("Runtime/environment fingerprint changed".into());
        }
        if e.environment.repository.commit != c.repository.commit {
            reasons.push(
                "Repository commit changed; revalidation recommended, not proof of invalidity"
                    .into(),
            );
        }
        if self.now.signed_duration_since(e.last_supported_at)
            > chrono::Duration::days(self.age_days as i64)
        {
            reasons.push(
                "Evidence age threshold exceeded; recommend a replication, do not delete evidence"
                    .into(),
            );
        }
        EvidenceFreshness {
            last_supported_at: e.last_supported_at,
            environment_version: Some(e.environment.repository.commit.clone()),
            stale: !reasons.is_empty(),
            reasons,
        }
    }
}
pub type NoveltyScore = f64;
pub trait TrialNoveltyPolicy {
    fn score(&self, proposed: &CurriculumTrial, history: &[Experience]) -> NoveltyScore;
}
/// Exact semantic signatures joined to immutable Experience IDs by the curriculum store.
pub struct ExactNoveltyPolicy {
    pub observations: Vec<(String, Vec<crate::core::ExperienceId>)>,
}
impl TrialNoveltyPolicy for ExactNoveltyPolicy {
    fn score(&self, p: &CurriculumTrial, h: &[Experience]) -> NoveltyScore {
        if p.intent != TrialIntent::NovelExploration {
            return 1.0;
        }
        if self.observations.iter().any(|(fingerprint, ids)| {
            fingerprint == &p.fingerprint
                && !ids.is_empty()
                && ids.iter().all(|id| {
                    h.iter().any(|e| {
                        e.id == *id
                            && matches!(
                                e.outcome,
                                crate::experience::Outcome::Success
                                    | crate::experience::Outcome::Failure
                            )
                    })
                })
        }) {
            0.0
        } else {
            1.0
        }
    }
}
