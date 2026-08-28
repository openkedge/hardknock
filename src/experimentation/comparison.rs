// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{
    Result,
    core::ProcessStatus,
    evaluation::{CheckStatus, EvaluationStatus},
};

pub trait CandidateComparisonPolicy {
    fn compare(&self, candidates: &[CandidateResult]) -> Result<ExperimentComparison>;
}

pub struct EvaluatorSuccessFirst(pub ComparisonCriteria);
impl CandidateComparisonPolicy for EvaluatorSuccessFirst {
    fn compare(&self, candidates: &[CandidateResult]) -> Result<ExperimentComparison> {
        let mut result = ExperimentComparison {
            policy: "evaluator-success-first".into(),
            recommendation: None,
            reasons: vec![],
            evidence_weight: "inconclusive".into(),
        };
        if candidates.len() < 2
            || candidates.iter().any(|c| {
                c.evaluation.status != EvaluationStatus::Completed
                    || matches!(
                        c.execution_status,
                        ProcessStatus::Interrupted | ProcessStatus::TimedOut
                    )
            })
        {
            result.reasons.push(
                "Fewer than two complete, evaluated candidates; no comparative recommendation"
                    .into(),
            );
            return Ok(result);
        }
        for c in candidates {
            c.evaluation.validate()?;
        }
        if candidates.iter().any(|c| {
            c.starting_fingerprint != candidates[0].starting_fingerprint
                || c.evaluation.spec != candidates[0].evaluation.spec
        }) {
            result.reasons.push(
                "Starting fingerprints or evaluator specifications differ; comparison refused"
                    .into(),
            );
            return Ok(result);
        }
        let use_diff = self.0.minimize_diff_size
            && candidates
                .iter()
                .all(|c| c.diff_summary.as_ref().is_some_and(|d| d.binary_files == 0));
        if self.0.minimize_diff_size && !use_diff {
            result.reasons.push(
                "Diff-size criterion unavailable for missing or binary diffs; it was not used"
                    .into(),
            );
        }
        let rank = |c: &CandidateResult| {
            let failed = c
                .evaluation
                .checks
                .iter()
                .filter(|x| x.status == CheckStatus::Failed)
                .count();
            let diff = if use_diff {
                c.diff_summary
                    .as_ref()
                    .map(|d| d.insertions + d.deletions)
                    .unwrap_or(usize::MAX)
            } else {
                0
            };
            let duration = if self.0.minimize_duration {
                c.duration_ms
            } else {
                0
            };
            (!c.evaluation.success, failed, diff, duration)
        };
        let mut ordered: Vec<_> = candidates.iter().collect();
        ordered.sort_by_key(|c| rank(c));
        let best = ordered[0];
        if self.0.require_success && !best.evaluation.success {
            result
                .reasons
                .push("No candidate passed all required checks".into());
        } else if best.execution_status != ProcessStatus::Succeeded {
            result
                .reasons
                .push("Best evaluated candidate did not complete execution successfully".into());
        } else if rank(best) == rank(ordered[1]) {
            result.reasons.push("No definitive winner under the configured criteria; ties are not broken by candidate order".into());
        } else {
            result.recommendation = Some(best.candidate_id.clone());
            result.reasons.push(format!(
                "{} ranked first under the configured criteria in this starting state: {}",
                best.name, best.evaluation.summary
            ));
            if use_diff {
                result.reasons.push(
                    "Diff size was an explicitly enabled secondary criterion (text lines only)"
                        .into(),
                );
            }
            if self.0.minimize_duration {
                result.reasons.push("Observed duration was an explicitly enabled secondary criterion; timing is noisy".into());
            }
            result.evidence_weight = "single-trial observation".into();
        }
        for c in candidates {
            result
                .reasons
                .push(format!("{}: {}", c.name, c.evaluation.summary));
        }
        Ok(result)
    }
}

pub fn classify_quality(
    variables: &[ExperimentVariable],
    inherited_environment: bool,
) -> ExperimentQuality {
    if variables.len() > 1 {
        ExperimentQuality::Confounded
    } else if inherited_environment {
        ExperimentQuality::PartiallyControlled
    } else {
        ExperimentQuality::Controlled
    }
}

pub fn summarize_diff(bytes: &[u8]) -> DiffSummary {
    let mut summary = DiffSummary::default();
    let mut in_hunk = false;
    for line in bytes.split(|b| *b == b'\n') {
        if line.starts_with(b"diff --git ") {
            summary.files_changed += 1;
            in_hunk = false;
        } else if line.starts_with(b"@@ ") {
            in_hunk = true;
        } else if line.starts_with(b"GIT binary patch") {
            summary.binary_files += 1;
        } else if in_hunk && line.starts_with(b"+") {
            summary.insertions += 1;
        } else if in_hunk && line.starts_with(b"-") {
            summary.deletions += 1;
        }
    }
    summary
}
