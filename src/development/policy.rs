// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{
    Error, Result,
    core::*,
    lesson::{EvidenceRef, EvidenceRelationship, Lesson, LessonStatus},
    retrieval::QueryContext,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DevelopmentConfig {
    pub aging_after_days: u32,
    pub stale_after_days: u32,
    pub min_trend_samples: u64,
    pub rate_change_threshold: f64,
    pub max_lessons: usize,
    pub max_reflexes: usize,
    pub max_recoveries: usize,
    pub bridge_context: bool,
}
impl Default for DevelopmentConfig {
    fn default() -> Self {
        Self {
            aging_after_days: 30,
            stale_after_days: 90,
            min_trend_samples: 5,
            rate_change_threshold: 0.05,
            max_lessons: 5,
            max_reflexes: 3,
            max_recoveries: 3,
            bridge_context: false,
        }
    }
}
impl DevelopmentConfig {
    pub fn validate(&self) -> Result<()> {
        if self.aging_after_days == 0
            || self.stale_after_days < self.aging_after_days
            || self.stale_after_days > 36500
            || self.min_trend_samples < 2
            || !self.rate_change_threshold.is_finite()
            || !(0.0..=1.0).contains(&self.rate_change_threshold)
            || self.max_lessons > 20
            || self.max_reflexes > 10
            || self.max_recoveries > 10
        {
            return Err(Error::InvalidInput(
                "Development policy limits out of range".into(),
            ));
        }
        Ok(())
    }
    pub fn versions(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("confidence".into(), "heuristic-confidence-v1".into()),
            ("relevance".into(), "scoped-context-freshness-v2".into()),
            ("freshness".into(), "age-and-context-v1".into()),
            ("validation".into(), "distinct-application-v1".into()),
            ("maturity".into(), "configured-skill-maturity-v1".into()),
            ("curriculum_priority".into(), "dimension-exposure-v1".into()),
            (
                "development_metrics".into(),
                "explicit-denominators-v1".into(),
            ),
            ("trend".into(), "disjoint-windows-v1".into()),
        ])
    }
    pub fn hash(&self) -> Result<String> {
        Ok(blake3::hash(&serde_json::to_vec(&(self, self.versions()))?)
            .to_hex()
            .to_string())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FreshnessBasis {
    pub last_supported_at: DateTime<Utc>,
    pub context: crate::experience::ExperienceContext,
    /// Retain the origin repository when the latest support came from a transfer.
    #[serde(default)]
    pub origin_context: Option<crate::experience::ExperienceContext>,
    pub agent: AgentIdentity,
    pub contradicted: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FreshnessAssessment {
    pub state: EvidenceState,
    pub multiplier: f64,
    pub reasons: Vec<String>,
}
pub fn assess_freshness(
    basis: &FreshnessBasis,
    query: &QueryContext,
    agent: Option<&AgentIdentity>,
    now: DateTime<Utc>,
    cfg: &DevelopmentConfig,
) -> FreshnessAssessment {
    let mut reasons = vec![];
    let origin_changed = basis.context.repository.path != query.repository.path
        && basis.origin_context.as_ref().is_some_and(|c| {
            c.repository.path == query.repository.path
                && c.repository.commit != query.repository.commit
        });
    let environment_changed = origin_changed
        || basis.context.environment.fingerprint != query.environment.fingerprint
        || (basis.context.repository.path == query.repository.path
            && basis.context.repository.commit != query.repository.commit);
    if environment_changed {
        reasons.push("Repository/runtime context changed; revalidation recommended".into());
    }
    let agent_changed = agent.is_some_and(|a| a != &basis.agent);
    if agent_changed {
        reasons.push("Agent/model/configuration identity changed; provenance survives and independence is not assumed".into());
    }
    let age = now.signed_duration_since(basis.last_supported_at);
    let old = age > Duration::days(cfg.stale_after_days as i64);
    if age > Duration::days(cfg.aging_after_days as i64) {
        reasons.push(
            "No recent supporting observation; age alone does not invalidate evidence".into(),
        );
    }
    let (state, multiplier) = if basis.contradicted {
        reasons.push("Conflicting evidence exists; inspect before applying".into());
        (EvidenceState::Contradicted, 0.0)
    } else if old && environment_changed {
        (EvidenceState::Stale, 0.4)
    } else if age > Duration::days(cfg.aging_after_days as i64)
        || environment_changed
        || agent_changed
    {
        (EvidenceState::Aging, 0.9)
    } else {
        (EvidenceState::Fresh, 1.0)
    };
    FreshnessAssessment {
        state,
        multiplier,
        reasons,
    }
}
pub fn support_ids(evidence: &[EvidenceRef]) -> Vec<ExperienceId> {
    evidence
        .iter()
        .filter_map(|e| match e {
            EvidenceRef::Experience {
                experience_id,
                relationship: EvidenceRelationship::Supports,
            } => Some(experience_id.clone()),
            _ => None,
        })
        .collect()
}

pub trait TrendPolicy {
    fn trend(&self, snapshots: &[ProfileSnapshot], metric: DevelopmentMetricKind) -> MetricTrend;
}
pub struct WindowTrendPolicy<'a>(pub &'a DevelopmentConfig);
impl TrendPolicy for WindowTrendPolicy<'_> {
    fn trend(&self, s: &[ProfileSnapshot], k: DevelopmentMetricKind) -> MetricTrend {
        if s.len() < 2 {
            return MetricTrend::InsufficientEvidence;
        }
        compare_metric(&s[s.len() - 2], &s[s.len() - 1], k, self.0).trend
    }
}
pub fn compare_metric(
    a: &ProfileSnapshot,
    b: &ProfileSnapshot,
    k: DevelopmentMetricKind,
    cfg: &DevelopmentConfig,
) -> MetricComparison {
    let previous = a.metrics.metric(k).clone();
    let current = b.metrics.metric(k).clone();
    let delta = previous.value.zip(current.value).map(|(a, b)| b - a);
    let old: HashSet<_> = a.evidence_ids.iter().collect();
    let compatible = std::mem::discriminant(&a.window) == std::mem::discriminant(&b.window);
    let reason = if a.subject != b.subject {
        Some("Different profile subjects")
    } else if a.policy_hash != b.policy_hash {
        Some("Policy versions/configuration differ; rebuild before comparing")
    } else if a.captured_at >= b.captured_at {
        Some("Snapshots are not chronologically ordered")
    } else if !compatible {
        Some("Different window definitions")
    } else if b.evidence_ids.iter().any(|id| old.contains(id)) {
        Some("Overlapping evidence; raw deltas are descriptive, not a development trend")
    } else if previous.sample_count < cfg.min_trend_samples
        || current.sample_count < cfg.min_trend_samples
    {
        Some("Insufficient samples for configured trend policy")
    } else if delta.is_none() {
        Some("Metric is UNKNOWN")
    } else {
        None
    };
    let trend = if reason.is_some() {
        MetricTrend::InsufficientEvidence
    } else if delta.unwrap_or(0.0).abs() <= cfg.rate_change_threshold {
        MetricTrend::Stable
    } else if (delta.unwrap_or(0.0) > 0.0) != k.lower_is_better() {
        MetricTrend::Improving
    } else {
        MetricTrend::Regressing
    };
    MetricComparison {
        metric: k,
        previous,
        current,
        delta,
        trend,
        reason: reason
            .unwrap_or(
                "Disjoint observed windows; heuristic threshold, not statistical significance",
            )
            .into(),
    }
}
pub fn compare_snapshots(
    a: &ProfileSnapshot,
    b: &ProfileSnapshot,
    cfg: &DevelopmentConfig,
) -> GrowthReport {
    let comparisons: Vec<_> = DevelopmentMetricKind::ALL
        .into_iter()
        .map(|k| compare_metric(a, b, k, cfg))
        .collect();
    let regressions=comparisons.iter().filter(|c|c.trend==MetricTrend::Regressing).map(|c|DevelopmentRegression{metric:c.metric,previous:c.previous.clone(),current:c.current.clone(),detected_at:Utc::now(),recommendation:"Inspect affected task families and explicitly plan revalidation; no curriculum was started".into(),auto_run:false}).collect();
    let change = |previous: Option<f64>,
                  current: Option<f64>,
                  previous_samples,
                  current_samples| NumericChange {
        previous,
        current,
        delta: previous.zip(current).map(|(a, b)| b - a),
        previous_samples,
        current_samples,
    };
    let runtime_samples =
        |metrics: &crate::runtime::RuntimeDevelopmentMetrics| metrics.decisions.values().sum();
    GrowthReport{from:a.id.clone(),to:b.id.clone(),comparisons,regressions,median_recovery_ms:change(a.metrics.median_time_to_recovery_ms.map(|v|v as f64),b.metrics.median_time_to_recovery_ms.map(|v|v as f64),a.metrics.recovery_latency_samples,b.metrics.recovery_latency_samples),hardened_skills:change(Some(a.metrics.hardened_skill_count as f64),Some(b.metrics.hardened_skill_count as f64),a.artifact_counts.skills,b.artifact_counts.skills),runtime_adaptation:RuntimeGrowth{experiments_per_task:change(a.runtime_control.experiments_per_task,b.runtime_control.experiments_per_task,runtime_samples(&a.runtime_control),runtime_samples(&b.runtime_control)),unnecessary_intervention_rate:change(a.runtime_control.unnecessary_intervention_rate,b.runtime_control.unnecessary_intervention_rate,a.runtime_control.avoided_failures+a.runtime_control.unnecessary_interventions,b.runtime_control.avoided_failures+b.runtime_control.unnecessary_interventions),recovery_success_rate:change(a.runtime_control.recovery_success_rate,b.runtime_control.recovery_success_rate,a.runtime_control.decisions.get(&crate::runtime::RuntimeDecisionKind::Recover).copied().unwrap_or(0),b.runtime_control.decisions.get(&crate::runtime::RuntimeDecisionKind::Recover).copied().unwrap_or(0))},note:"Observed behavior and evidence, not general intelligence or causal proof. Task mixes may differ even in disjoint windows. Runtime adaptation is based on recorded decisions and feedback; it is descriptive, not automatic policy learning.".into()}
}
pub trait ExperiencePromotionPolicy {
    fn evaluate(&self, lesson: &Lesson) -> PromotionDecision;
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PromotionDecision {
    pub eligible_for_review: bool,
    pub reason: String,
    pub auto_promote: bool,
}
pub struct ConservativePromotion;
impl ExperiencePromotionPolicy for ConservativePromotion {
    fn evaluate(&self, l: &Lesson) -> PromotionDecision {
        let allowed = l.status == LessonStatus::Validated
            && l.context_match.repository.is_none()
            && l.validation
                .as_ref()
                .is_some_and(|v| v.distinct_successful_contexts >= 2)
            && !l.evidence.iter().any(|e| {
                matches!(
                    e,
                    EvidenceRef::Experience {
                        relationship: EvidenceRelationship::Contradicts,
                        ..
                    } | EvidenceRef::Trial {
                        relationship: EvidenceRelationship::Contradicts,
                        ..
                    }
                )
            });
        PromotionDecision {eligible_for_review:allowed,reason:if allowed {"Existing generalized scope has transfer evidence; review only, never widen scope automatically"}else{"Repository-specific, weak, or conflicting evidence stays scoped"}.into(),auto_promote:false}
    }
}
pub fn stable_id(prefix: &str, value: &impl Serialize) -> Result<String> {
    let hash = blake3::hash(&serde_json::to_vec(value)?);
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    Ok(format!("{prefix}{}", uuid::Uuid::from_bytes(bytes)))
}
pub fn snapshot(profile: &ExperienceProfile) -> ProfileSnapshot {
    ProfileSnapshot {
        id: ProfileSnapshotId::new(),
        profile_id: profile.id.clone(),
        subject: profile.subject.clone(),
        captured_at: profile.updated_at,
        window: profile.window.clone(),
        metrics: profile.metrics.clone(),
        coverage: profile.coverage.clone(),
        artifact_counts: ExperienceArtifactCounts {
            skills: profile.skills.len() as u64,
            lessons: profile.lessons.len() as u64,
            reflexes: profile.reflexes.len() as u64,
            recoveries: profile.recoveries.len() as u64,
        },
        evidence_ids: profile.evidence_ids.clone(),
        policy_hash: profile.policy_hash.clone(),
        policy_versions: profile.policy_versions.clone(),
        runtime_control: profile.runtime_control.clone(),
    }
}
