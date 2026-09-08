// SPDX-License-Identifier: Apache-2.0

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::*;
use crate::{
    Error, Result,
    core::{
        AbstractKnowledgeId, EvidenceManifestId, ExperiencePatternId, KnowledgeDistillationId,
        KnowledgeExceptionId,
    },
    experimentation::ExperimentQuality,
    runtime::RuntimeDecisionContext,
};

pub const ABSTRACTION_PROVIDER_VERSION: &str = "deterministic-structural-abstraction-v1";
pub const PROMOTION_POLICY_VERSION: &str = "held-out-negative-control-promotion-v1";
pub const RESOLUTION_POLICY_VERSION: &str = "specific-before-general-resolution-v1";

pub trait AbstractionCandidateProvider {
    fn propose(
        &self,
        artifacts: &[KnowledgeArtifact],
        context: &AbstractionContext,
    ) -> Result<Vec<CandidateAbstraction>>;
}

pub trait AbstractionPromotionPolicy {
    fn evaluate(
        &self,
        candidate: &AbstractKnowledge,
        evidence: &[TransferEvidence],
        controls: &[TransferEvidence],
    ) -> PromotionDecision;
}

pub trait TransferContextPlanner {
    fn plan(
        &self,
        abstraction: &CandidateAbstraction,
        available_contexts: &[TransferContext],
        budget: &crate::budget::ExperienceBudget,
    ) -> Result<TransferTestPlan>;
}

pub trait KnowledgeResolutionPolicy {
    fn resolve(
        &self,
        context: &RuntimeDecisionContext,
        candidates: &[KnowledgeCandidateRef],
    ) -> Result<KnowledgeResolution>;
}

fn stable_uuid(prefix: &str, value: &impl serde::Serialize) -> Result<String> {
    let digest = blake3::hash(&serde_json::to_vec(value)?);
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    Ok(format!("{prefix}{}", uuid::Uuid::from_bytes(bytes)))
}

fn scope_key(scope: &crate::lesson::ContextSelector) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(scope)?)
        .to_hex()
        .to_string())
}

fn structural_variables(structure: &PatternStructure) -> Vec<&ContextVariable> {
    structure
        .context_variables
        .iter()
        .filter(|variable| {
            matches!(
                variable.relevance,
                ContextRelevance::Required | ContextRelevance::Suspected
            ) && !matches!(
                variable.kind,
                ContextVariableKind::Environment
                    | ContextVariableKind::Resource
                    | ContextVariableKind::SoftwareVersion
                    | ContextVariableKind::DependencyVersion
                    | ContextVariableKind::Tool
                    | ContextVariableKind::AgentRuntime
            )
        })
        .collect()
}

fn structural_signature(artifact: &KnowledgeArtifact) -> Result<String> {
    // Statements are deliberately absent. Candidate identity comes from explicit
    // mechanism, trigger, action, outcome, and required structural variables.
    let variables = structural_variables(&artifact.structure);
    let causal = artifact
        .structure
        .causal_mechanisms
        .iter()
        .map(|item| item.id.to_string())
        .collect::<Vec<_>>();
    Ok(blake3::hash(&serde_json::to_vec(&(
        artifact.artifact.kind,
        &artifact.structure.trigger,
        variables,
        &artifact.structure.action_pattern,
        &artifact.structure.outcome_pattern,
        causal,
        &artifact.structure.required_conditions,
    ))?)
    .to_hex()
    .to_string())
}

fn merged_scope(artifacts: &[&KnowledgeArtifact]) -> crate::lesson::ContextSelector {
    let first = &artifacts[0].scope;
    let repository = artifacts
        .iter()
        .all(|item| item.scope.repository == first.repository)
        .then(|| first.repository.clone())
        .flatten();
    let os = artifacts
        .iter()
        .all(|item| item.scope.os == first.os)
        .then(|| first.os.clone())
        .flatten();
    let arch = artifacts
        .iter()
        .all(|item| item.scope.arch == first.arch)
        .then(|| first.arch.clone())
        .flatten();
    let required_markers = first
        .required_markers
        .iter()
        .filter(|marker| {
            artifacts
                .iter()
                .all(|item| item.scope.required_markers.contains(marker))
        })
        .cloned()
        .collect();
    let tags = first
        .tags
        .iter()
        .filter(|tag| artifacts.iter().all(|item| item.scope.tags.contains(tag)))
        .cloned()
        .collect();
    crate::lesson::ContextSelector {
        repository,
        required_markers,
        tags,
        os,
        arch,
    }
}

fn abstraction_kind(kind: ExperiencePatternKind) -> Option<AbstractKnowledgeKind> {
    AbstractKnowledgeKind::from_pattern(kind)
}

fn risk(kind: AbstractKnowledgeKind) -> GeneralizationRisk {
    match kind {
        AbstractKnowledgeKind::AbstractConstraint => GeneralizationRisk::High,
        AbstractKnowledgeKind::AbstractAntiPattern
        | AbstractKnowledgeKind::AbstractSkill
        | AbstractKnowledgeKind::AbstractRecovery => GeneralizationRisk::Medium,
        AbstractKnowledgeKind::AbstractLesson => GeneralizationRisk::Low,
    }
}

fn applicability_from_structure(structure: &PatternStructure) -> ApplicabilityPredicate {
    let mut all_of = structural_variables(structure)
        .into_iter()
        .map(|variable| ApplicabilityClause {
            variable: variable.kind.clone(),
            name: variable.name.clone(),
            operator: PredicateOperator::Equals,
            value: Some(variable.value.clone()),
            rationale: "Shared structural condition across source artifacts".into(),
        })
        .collect::<Vec<_>>();
    all_of.extend(
        structure
            .required_conditions
            .iter()
            .map(|predicate| ApplicabilityClause {
                variable: ContextVariableKind::Custom("pattern_condition".into()),
                name: predicate.variable.clone(),
                operator: predicate.operator,
                value: predicate.value.clone(),
                rationale: "Explicit required pattern condition".into(),
            }),
    );
    all_of.sort();
    all_of.dedup();
    ApplicabilityPredicate { all_of }
}

fn statement_for(kind: AbstractKnowledgeKind, structure: &PatternStructure) -> String {
    let trigger = structure
        .trigger
        .as_ref()
        .map(|item| item.variable.as_str())
        .unwrap_or("the structured trigger occurs");
    let action = structure
        .action_pattern
        .as_ref()
        .map(|item| format!("{item:?}"))
        .unwrap_or_else(|| "the state-dependent action".into());
    match kind {
        AbstractKnowledgeKind::AbstractLesson => {
            format!("When {trigger}, preserve the shared operational mechanism before {action}")
        }
        AbstractKnowledgeKind::AbstractSkill => {
            format!("Apply the shared bounded procedure when {trigger}: {action}")
        }
        AbstractKnowledgeKind::AbstractConstraint => {
            format!("When {trigger}, the required conditions must hold before {action}")
        }
        AbstractKnowledgeKind::AbstractAntiPattern => {
            format!("Avoid {action} when {trigger} and the required conditions are unmet")
        }
        AbstractKnowledgeKind::AbstractRecovery => {
            format!("When {trigger}, restore the shared required state before continuing")
        }
    }
}

fn varying_dimensions(artifacts: &[&KnowledgeArtifact]) -> Vec<ContextVariableKind> {
    let mut values: BTreeMap<ContextVariableKind, BTreeSet<VariableValue>> = BTreeMap::new();
    for artifact in artifacts {
        for variable in &artifact.structure.context_variables {
            values
                .entry(variable.kind.clone())
                .or_default()
                .insert(variable.value.clone());
        }
    }
    values
        .into_iter()
        .filter_map(|(kind, values)| (values.len() > 1).then_some(kind))
        .collect()
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicAbstractionCandidateProvider;

impl AbstractionCandidateProvider for DeterministicAbstractionCandidateProvider {
    fn propose(
        &self,
        artifacts: &[KnowledgeArtifact],
        context: &AbstractionContext,
    ) -> Result<Vec<CandidateAbstraction>> {
        let minimum_members = context.minimum_members.max(2);
        let minimum_contexts = context.minimum_source_contexts.max(2);
        let mut groups: BTreeMap<(KnowledgeArtifactKind, String), Vec<&KnowledgeArtifact>> =
            BTreeMap::new();
        for artifact in artifacts {
            groups
                .entry((artifact.artifact.kind, structural_signature(artifact)?))
                .or_default()
                .push(artifact);
        }
        let mut candidates = Vec::new();
        for ((_, signature), mut members) in groups {
            if members.len() < minimum_members {
                continue;
            }
            members.sort_by_key(|item| item.artifact.clone());
            let pattern_kind = ExperiencePatternKind::from(members[0].artifact.kind);
            let Some(kind) = abstraction_kind(pattern_kind) else {
                continue;
            };
            let contexts = members
                .iter()
                .map(|item| scope_key(&item.scope))
                .collect::<Result<BTreeSet<_>>>()?;
            let roots = members
                .iter()
                .flat_map(|item| item.root_origins.iter().cloned())
                .collect::<BTreeSet<_>>();
            let member_refs = members
                .iter()
                .map(|item| item.artifact.clone())
                .collect::<Vec<_>>();
            let evidence = members
                .iter()
                .flat_map(|item| item.evidence.iter().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let scope = merged_scope(&members);
            let structure = members[0].structure.clone();
            let pattern_id: ExperiencePatternId =
                stable_uuid("pattern-", &(pattern_kind, &signature, &member_refs))?.parse()?;
            let now = context.now;
            let pattern = ExperiencePattern {
                id: pattern_id.clone(),
                name: format!("structural-{}", &signature[..12]),
                kind: pattern_kind,
                members: member_refs.clone(),
                structure: structure.clone(),
                scope,
                status: if contexts.len() >= minimum_contexts {
                    ExperiencePatternStatus::TransferTestable
                } else {
                    ExperiencePatternStatus::Candidate
                },
                evidence: evidence.clone(),
                created_at: now,
                updated_at: now,
            };
            let applicability = applicability_from_structure(&structure);
            let unknown = varying_dimensions(&members)
                .iter()
                .cloned()
                .map(|variable| ApplicabilityClause {
                    name: format!("{variable:?}").to_lowercase(),
                    variable,
                    operator: PredicateOperator::Present,
                    value: None,
                    rationale: "This dimension varied across sources and needs held-out evidence"
                        .into(),
                })
                .collect::<Vec<_>>();
            let abstract_id: AbstractKnowledgeId =
                stable_uuid("abstract-", &(kind, &pattern_id, &applicability))?.parse()?;
            let source_contexts = members.iter().map(|item| item.scope.clone()).collect();
            let source_artifacts = member_refs;
            let knowledge = AbstractKnowledge {
                id: abstract_id,
                revision: 1,
                kind,
                statement: statement_for(kind, &structure),
                applicability: applicability.clone(),
                generalization_boundary: GeneralizationBoundary {
                    included: applicability.all_of.clone(),
                    excluded: Vec::new(),
                    unknown,
                    evidence: Vec::new(),
                },
                supporting_patterns: vec![pattern_id],
                transfer_evidence: Vec::new(),
                specializations: Vec::new(),
                exceptions: Vec::new(),
                maturity: KnowledgeMaturity::TransferTestable,
                risk: risk(kind),
                provenance: KnowledgeProvenance {
                    source_artifacts,
                    source_contexts,
                    evidence,
                    root_origins: roots.into_iter().collect(),
                    origin: KnowledgeOrigin::CandidateProvider,
                    generator: ABSTRACTION_PROVIDER_VERSION.into(),
                    created_at: now,
                },
                updated_at: now,
            };
            let correlated = knowledge.provenance.root_origins.len() < 2;
            candidates.push(CandidateAbstraction {
                pattern,
                varying_dimensions: varying_dimensions(&members),
                rationale: vec![
                    "Candidate formed from explicit shared structure; statements were not compared"
                        .into(),
                    if correlated {
                        "Source artifacts share one known root origin; transfer support cannot claim high diversity"
                            .into()
                    } else {
                        "Multiple known root origins are preserved for epistemic assessment".into()
                    },
                ],
                knowledge,
            });
        }
        candidates.sort_by_key(|item| item.knowledge.id.clone());
        Ok(candidates)
    }
}

#[derive(Clone, Debug)]
pub struct DefaultAbstractionPromotionPolicy {
    pub minimum_source_contexts: usize,
    pub maximum_false_constraint_rate_numerator: usize,
}

impl Default for DefaultAbstractionPromotionPolicy {
    fn default() -> Self {
        Self {
            minimum_source_contexts: 2,
            maximum_false_constraint_rate_numerator: 0,
        }
    }
}

impl DefaultAbstractionPromotionPolicy {
    pub fn assess(
        &self,
        candidate: &AbstractKnowledge,
        evidence: &[TransferEvidence],
        controls: &[TransferEvidence],
    ) -> PromotionAssessment {
        let source_contexts = candidate
            .provenance
            .source_contexts
            .iter()
            .filter_map(|scope| scope_key(scope).ok())
            .collect::<BTreeSet<_>>()
            .len();
        let local_held_out = evidence
            .iter()
            .filter(|item| {
                item.context_role == TransferContextRole::HeldOut
                    && item.local
                    && item.quality.experiment_quality == ExperimentQuality::Controlled
            })
            .collect::<Vec<_>>();
        let held_out_support = local_held_out
            .iter()
            .filter(|item| item.outcome == TransferEvidenceOutcome::Supports)
            .count();
        let contradictions = local_held_out
            .iter()
            .filter(|item| item.outcome == TransferEvidenceOutcome::Contradicts)
            .count();
        let false_constraints = controls
            .iter()
            .filter(|item| !item.expected_applicable && item.application_triggered)
            .count();
        let valid_controls = controls
            .iter()
            .filter(|item| {
                item.context_role == TransferContextRole::NegativeControl
                    && item.local
                    && item.quality.experiment_quality == ExperimentQuality::Controlled
                    && matches!(
                        item.outcome,
                        TransferEvidenceOutcome::Supports | TransferEvidenceOutcome::NarrowsScope
                    )
            })
            .count();
        let mut reasons = Vec::new();
        if source_contexts < self.minimum_source_contexts {
            reasons.push("At least two distinct source contexts are required".into());
        }
        if held_out_support == 0 {
            reasons.push("No controlled local held-out transfer supports this abstraction".into());
        }
        if candidate.provenance.origin == KnowledgeOrigin::FederatedAdvisory
            && local_held_out.is_empty()
        {
            reasons.push(
                "Remote abstraction remains advisory until local transfer evidence exists".into(),
            );
        }
        let distinct_roots = candidate
            .provenance
            .root_origins
            .iter()
            .collect::<BTreeSet<_>>()
            .len();
        if distinct_roots < 2 {
            reasons.push(
                "Known source artifacts share fewer than two root origins; correlated support is not diversity"
                    .into(),
            );
        }
        let needs_control = matches!(
            candidate.kind,
            AbstractKnowledgeKind::AbstractConstraint | AbstractKnowledgeKind::AbstractAntiPattern
        );
        if needs_control && valid_controls == 0 {
            reasons.push("Abstract Constraints and AntiPatterns require a negative control".into());
        }
        if false_constraints > self.maximum_false_constraint_rate_numerator {
            reasons.push("Negative control exposed an incorrectly restrictive application".into());
        }
        let decision = if evidence
            .iter()
            .any(|item| item.outcome == TransferEvidenceOutcome::Invalid)
        {
            PromotionDecision::Reject
        } else if contradictions > 0
            || false_constraints > self.maximum_false_constraint_rate_numerator
            || evidence
                .iter()
                .any(|item| item.outcome == TransferEvidenceOutcome::NarrowsScope)
        {
            PromotionDecision::NarrowScope
        } else if source_contexts < self.minimum_source_contexts
            || held_out_support == 0
            || (needs_control && valid_controls == 0)
            || distinct_roots < 2
        {
            PromotionDecision::MoreEvidenceRequired
        } else {
            PromotionDecision::Promote
        };
        PromotionAssessment {
            decision,
            reasons,
            held_out_support,
            negative_controls: valid_controls,
            false_constraint_applications: false_constraints,
        }
    }
}

impl AbstractionPromotionPolicy for DefaultAbstractionPromotionPolicy {
    fn evaluate(
        &self,
        candidate: &AbstractKnowledge,
        evidence: &[TransferEvidence],
        controls: &[TransferEvidence],
    ) -> PromotionDecision {
        self.assess(candidate, evidence, controls).decision
    }
}

pub fn narrow_generalization_boundary(
    knowledge: &AbstractKnowledge,
    evidence: &[TransferEvidence],
) -> (AbstractKnowledge, Vec<KnowledgeException>) {
    let mut revised = knowledge.clone();
    revised.revision = revised.revision.saturating_add(1);
    revised.maturity = KnowledgeMaturity::Overgeneralized;
    revised.updated_at = Utc::now();
    let mut exceptions = Vec::new();
    for item in evidence.iter().filter(|item| {
        matches!(
            item.outcome,
            TransferEvidenceOutcome::NarrowsScope | TransferEvidenceOutcome::Contradicts
        )
    }) {
        let Some(clause) = item.boundary_clause.clone() else {
            continue;
        };
        if !revised.generalization_boundary.excluded.contains(&clause) {
            revised
                .generalization_boundary
                .excluded
                .push(clause.clone());
        }
        revised
            .generalization_boundary
            .unknown
            .retain(|unknown| unknown.name != clause.name);
        let exception = KnowledgeException {
            id: KnowledgeExceptionId::new(),
            parent: AbstractKnowledgeRef {
                id: revised.id.clone(),
                revision: revised.revision,
            },
            context: item.target_context.clone(),
            applicability: ApplicabilityPredicate {
                all_of: vec![clause],
            },
            reason: if !item.expected_applicable {
                ExceptionReason::IdempotentSemantics
            } else {
                ExceptionReason::ContradictoryTransfer
            },
            evidence: vec![crate::epistemic::EvidenceRef {
                kind: "transfer_evidence".into(),
                id: item.id.to_string(),
            }],
            created_at: Utc::now(),
        };
        revised.exceptions.push(KnowledgeExceptionRef {
            id: exception.id.clone(),
        });
        exceptions.push(exception);
    }
    revised
        .generalization_boundary
        .evidence
        .extend(evidence.iter().map(|item| TransferEvidenceRef {
            id: item.id.clone(),
        }));
    (revised, exceptions)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicTransferContextPlanner;

fn context_information(context: &TransferContext) -> (usize, String) {
    let varied = context
        .variables
        .iter()
        .filter(|variable| variable.relevance == ContextRelevance::Varies)
        .count();
    let key = serde_json::to_string(&context.selector).unwrap_or_default();
    (varied, key)
}

impl TransferContextPlanner for DeterministicTransferContextPlanner {
    fn plan(
        &self,
        abstraction: &CandidateAbstraction,
        available_contexts: &[TransferContext],
        budget: &crate::budget::ExperienceBudget,
    ) -> Result<TransferTestPlan> {
        if budget.max_trials() < 2 {
            return Err(Error::Intervention(
                "Transfer requires a baseline and an abstraction-active trial".into(),
            ));
        }
        let source = abstraction
            .knowledge
            .provenance
            .source_contexts
            .iter()
            .map(scope_key)
            .collect::<Result<BTreeSet<_>>>()?;
        let mut held_out = available_contexts
            .iter()
            .filter(|context| {
                scope_key(&context.selector)
                    .map(|key| !source.contains(&key))
                    .unwrap_or(false)
            })
            .cloned()
            .collect::<Vec<_>>();
        held_out.sort_by_key(|context| {
            let (varied, key) = context_information(context);
            (varied, key)
        });
        let context_limit = budget.max_trials() / 2;
        held_out.truncate(context_limit.max(1));
        if held_out.is_empty() {
            return Err(Error::InvalidInput(
                "No held-out context remains after excluding source contexts".into(),
            ));
        }
        Ok(TransferTestPlan {
            abstraction: abstraction.knowledge.id.clone(),
            contexts: held_out,
            budget: budget.clone(),
            intent: crate::experimentation::ExperimentIntent::ValidateTransfer,
            requires_equivalent_start: true,
            candidates: vec![
                "baseline_without_abstraction".into(),
                "abstraction_active".into(),
            ],
        })
    }
}

fn observed_values(context: &RuntimeDecisionContext) -> BTreeMap<String, VariableValue> {
    let mut values = context
        .query_context
        .environment
        .facts
        .iter()
        .map(|(key, value)| (key.clone(), VariableValue::Text(value.clone())))
        .collect::<BTreeMap<_, _>>();
    for tag in &context.query_context.tags {
        values.insert(format!("tag:{tag}"), VariableValue::Boolean(true));
    }
    for marker in &context.query_context.detected_markers {
        values.insert(format!("marker:{marker}"), VariableValue::Boolean(true));
    }
    values
}

fn clause_matches(clause: &ApplicabilityClause, values: &BTreeMap<String, VariableValue>) -> bool {
    let current = values.get(&clause.name);
    match clause.operator {
        PredicateOperator::Present => current.is_some(),
        PredicateOperator::Absent => current.is_none(),
        PredicateOperator::Equals => current == clause.value.as_ref(),
        PredicateOperator::NotEquals => current.is_some() && current != clause.value.as_ref(),
        PredicateOperator::Contains => match (current, clause.value.as_ref()) {
            (Some(VariableValue::Set(current)), Some(VariableValue::Text(expected))) => {
                current.contains(expected)
            }
            (Some(VariableValue::Text(current)), Some(VariableValue::Text(expected))) => {
                current.contains(expected)
            }
            _ => false,
        },
    }
}

fn candidate_applies(
    candidate: &KnowledgeCandidateRef,
    context: &RuntimeDecisionContext,
    values: &BTreeMap<String, VariableValue>,
) -> bool {
    matches!(
        candidate.maturity,
        KnowledgeMaturity::Supported | KnowledgeMaturity::Validated
    ) && candidate
        .scope
        .matches(&context.query_context.experience_context())
        && candidate
            .applicability
            .all_of
            .iter()
            .all(|clause| clause_matches(clause, values))
        && !candidate
            .boundary
            .excluded
            .iter()
            .any(|clause| clause_matches(clause, values))
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicKnowledgeResolutionPolicy;

impl KnowledgeResolutionPolicy for DeterministicKnowledgeResolutionPolicy {
    fn resolve(
        &self,
        context: &RuntimeDecisionContext,
        candidates: &[KnowledgeCandidateRef],
    ) -> Result<KnowledgeResolution> {
        let values = observed_values(context);
        let mut applicable = candidates
            .iter()
            .filter(|candidate| candidate_applies(candidate, context, &values))
            .collect::<Vec<_>>();
        applicable.sort_by_key(|candidate| {
            (
                Reverse(candidate.level),
                candidate.quality != ExperimentQuality::Controlled,
                Reverse(candidate.maturity),
                Reverse(candidate.updated_at),
                candidate.reference.clone(),
            )
        });
        let mut result = KnowledgeResolution::default();
        for candidate in &applicable {
            match candidate.level {
                ResolutionLevel::Abstract => {
                    result.abstract_matches.push(candidate.reference.clone())
                }
                ResolutionLevel::Specialization => result
                    .specialization_matches
                    .push(candidate.reference.clone()),
                ResolutionLevel::Specific => {
                    result.specific_matches.push(candidate.reference.clone())
                }
                ResolutionLevel::Exception => result.exceptions.push(candidate.reference.clone()),
            }
            result
                .unknown_boundary_conditions
                .extend(candidate.boundary.unknown.iter().cloned());
        }
        let exception_parents = applicable
            .iter()
            .filter(|candidate| candidate.level == ResolutionLevel::Exception)
            .filter_map(|candidate| candidate.abstract_parent.clone())
            .collect::<BTreeSet<_>>();
        if let Some(exception) = applicable
            .iter()
            .find(|candidate| candidate.level == ResolutionLevel::Exception)
        {
            result.selected.push(ResolvedKnowledge {
                reference: exception.reference.clone(),
                level: exception.level,
                statement: exception.statement.clone(),
                reason: "Specific supported exception overrides its broader abstraction".into(),
            });
        } else if let Some(specific) = applicable
            .iter()
            .find(|candidate| candidate.level == ResolutionLevel::Specific)
        {
            result.selected.push(ResolvedKnowledge {
                reference: specific.reference.clone(),
                level: specific.level,
                statement: specific.statement.clone(),
                reason: "Specific applicable evidence takes precedence over broader knowledge"
                    .into(),
            });
        } else {
            if let Some(specialization) = applicable
                .iter()
                .find(|candidate| candidate.level == ResolutionLevel::Specialization)
            {
                result.selected.push(ResolvedKnowledge {
                    reference: specialization.reference.clone(),
                    level: specialization.level,
                    statement: specialization.statement.clone(),
                    reason: "Most specific applicable specialization selected".into(),
                });
            }
            if let Some(abstract_item) = applicable.iter().find(|candidate| {
                candidate.level == ResolutionLevel::Abstract
                    && candidate
                        .abstract_parent
                        .as_ref()
                        .is_none_or(|parent| !exception_parents.contains(parent))
            }) {
                result.selected.push(ResolvedKnowledge {
                    reference: abstract_item.reference.clone(),
                    level: abstract_item.level,
                    statement: abstract_item.statement.clone(),
                    reason: "Validated abstraction supplies bounded shared guidance".into(),
                });
            }
        }
        result.suppressed_members = result.specific_matches.len().saturating_sub(usize::from(
            result
                .selected
                .iter()
                .any(|item| item.level == ResolutionLevel::Specific),
        ));
        result.unknown_boundary_conditions.sort();
        result.unknown_boundary_conditions.dedup();
        Ok(result)
    }
}

pub fn assess_abstraction_freshness(
    members: Vec<MemberKnowledgeHealth>,
    transfers: Vec<TransferEvidenceHealth>,
) -> AbstractionFreshness {
    let contradicted = members
        .iter()
        .any(|item| item.maturity == KnowledgeMaturity::Contradicted)
        || transfers.iter().any(|item| item.contradicted);
    let stale = members
        .iter()
        .filter(|item| item.maturity == KnowledgeMaturity::Stale)
        .count()
        + transfers.iter().filter(|item| item.stale).count();
    let total = members.len() + transfers.len();
    let status = if contradicted {
        AbstractionFreshnessStatus::Contradicted
    } else if stale == 0 {
        AbstractionFreshnessStatus::Fresh
    } else if stale < total {
        AbstractionFreshnessStatus::PartiallyStale
    } else {
        AbstractionFreshnessStatus::Stale
    };
    AbstractionFreshness {
        member_health: members,
        transfer_health: transfers,
        status,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GuardCandidateAssessment {
    pub eligible_for_governance_review: bool,
    pub external_governance_required: bool,
    pub automatic_hard_deny: bool,
    pub reasons: Vec<String>,
}

pub fn assess_guard_candidate(
    knowledge: &AbstractKnowledge,
    evidence: &[TransferEvidence],
) -> GuardCandidateAssessment {
    let assessment = DefaultAbstractionPromotionPolicy::default().assess(
        knowledge,
        evidence,
        &evidence
            .iter()
            .filter(|item| item.context_role == TransferContextRole::NegativeControl)
            .cloned()
            .collect::<Vec<_>>(),
    );
    let eligible = knowledge.kind == AbstractKnowledgeKind::AbstractConstraint
        && knowledge.maturity == KnowledgeMaturity::Validated
        && assessment.decision == PromotionDecision::Promote
        && knowledge.provenance.origin != KnowledgeOrigin::FederatedAdvisory;
    GuardCandidateAssessment {
        eligible_for_governance_review: eligible,
        external_governance_required: true,
        automatic_hard_deny: false,
        reasons: if eligible {
            vec![
                "Empirical support permits a GuardCandidate review, not enforcement".into(),
                "OpenKedge governance retains execution authority".into(),
            ]
        } else {
            vec!["Abstraction has not satisfied local transfer and negative-control gates".into()]
        },
    }
}

pub fn overgeneralization_report(
    knowledge: &AbstractKnowledge,
    evidence: &[TransferEvidence],
) -> OvergeneralizationReport {
    let failing_contexts = evidence
        .iter()
        .filter(|item| item.outcome == TransferEvidenceOutcome::Contradicts)
        .map(|item| item.target_context.clone())
        .collect();
    let false_positive_contexts = evidence
        .iter()
        .filter(|item| !item.expected_applicable && item.application_triggered)
        .map(|item| item.target_context.clone())
        .collect();
    let (revised, _) = narrow_generalization_boundary(knowledge, evidence);
    OvergeneralizationReport {
        candidate: knowledge.id.clone(),
        failing_contexts,
        false_positive_contexts,
        recommended_boundary_revision: (revised.generalization_boundary
            != knowledge.generalization_boundary)
            .then_some(revised.generalization_boundary),
    }
}

/// Produces a predictive *candidate* from a validated AntiPattern. The target
/// warning is deliberately inactive until the predictive subsystem validates
/// it against local positive histories and negative controls.
pub fn transfer_early_warning_candidate(
    knowledge: &AbstractKnowledge,
    source: &crate::predictive::EarlyWarningSignature,
    target: crate::lesson::ContextSelector,
) -> Result<crate::predictive::EarlyWarningSignature> {
    if knowledge.kind != AbstractKnowledgeKind::AbstractAntiPattern
        || knowledge.maturity != KnowledgeMaturity::Validated
    {
        return Err(Error::Intervention(
            "Forecast transfer requires a validated AbstractAntiPattern".into(),
        ));
    }
    let mut candidate = source.clone();
    candidate.id = crate::core::EarlyWarningSignatureId::new();
    candidate.scope = target;
    candidate.status = crate::predictive::RiskIndicatorStatus::Candidate;
    candidate.revision = 0;
    candidate.origin = crate::predictive::PredictiveOrigin::Local;
    candidate
        .evidence
        .push(crate::predictive::TrajectoryEvidenceRef::Custom(format!(
            "abstract-knowledge:{}@{}",
            knowledge.id, knowledge.revision
        )));
    candidate.created_at = Utc::now();
    candidate.updated_at = candidate.created_at;
    Ok(candidate)
}

pub fn distill(
    knowledge: &AbstractKnowledge,
    manifest: EvidenceManifestId,
) -> Result<(KnowledgeDistillation, Vec<KnowledgeRepresentation>)> {
    if knowledge.maturity != KnowledgeMaturity::Validated {
        return Err(Error::Intervention(
            "Only validated abstract knowledge may represent specific artifacts".into(),
        ));
    }
    let reference = AbstractKnowledgeRef {
        id: knowledge.id.clone(),
        revision: knowledge.revision,
    };
    let distillation = KnowledgeDistillation {
        id: KnowledgeDistillationId::new(),
        inputs: knowledge.provenance.source_artifacts.clone(),
        outputs: vec![reference],
        preserved_exceptions: knowledge
            .exceptions
            .iter()
            .map(|item| item.id.clone())
            .collect(),
        evidence_manifest: manifest,
        created_at: Utc::now(),
    };
    let representations = knowledge
        .provenance
        .source_artifacts
        .iter()
        .cloned()
        .map(|artifact| KnowledgeRepresentation {
            artifact,
            state: KnowledgeRepresentationState::RepresentedByAbstract(knowledge.id.clone()),
            changed_at: Utc::now(),
            reason: "Validated abstraction is active; specific evidence remains retained".into(),
        })
        .collect();
    Ok((distillation, representations))
}

pub fn reactivate_represented_members(
    knowledge: &AbstractKnowledge,
    representations: &[KnowledgeRepresentation],
) -> Vec<KnowledgeRepresentation> {
    representations
        .iter()
        .filter(|record| {
            matches!(
                &record.state,
                KnowledgeRepresentationState::RepresentedByAbstract(id) if id == &knowledge.id
            )
        })
        .cloned()
        .map(|mut record| {
            record.state = KnowledgeRepresentationState::DirectActive;
            record.changed_at = Utc::now();
            record.reason =
                "Parent abstraction contradicted or retired; still-valid specific knowledge reactivated"
                    .into();
            record
        })
        .collect()
}

pub fn abstraction_metrics(
    knowledge: &[AbstractKnowledge],
    transfers: &[TransferEvidence],
    negative: &[NegativeTransferEvent],
    representations: &[KnowledgeRepresentation],
) -> AbstractionMetrics {
    let false_constraints = negative
        .iter()
        .filter(|event| event.outcome == NegativeTransferOutcome::FalseConstraint)
        .count();
    AbstractionMetrics {
        evaluated_transfers: transfers.len(),
        supported_transfers: transfers
            .iter()
            .filter(|item| item.outcome == TransferEvidenceOutcome::Supports)
            .count(),
        negative_transfers: negative.len(),
        false_abstract_constraints: false_constraints,
        constraint_applications: transfers
            .iter()
            .filter(|item| item.application_triggered)
            .count(),
        specific_artifacts_before: representations.len(),
        runtime_items_after: knowledge
            .iter()
            .filter(|item| item.maturity == KnowledgeMaturity::Validated)
            .count(),
        specializations: knowledge
            .iter()
            .map(|item| item.specializations.len())
            .sum(),
        exceptions: knowledge.iter().map(|item| item.exceptions.len()).sum(),
        unknown_contexts: knowledge
            .iter()
            .map(|item| item.generalization_boundary.unknown.len())
            .sum(),
    }
}
