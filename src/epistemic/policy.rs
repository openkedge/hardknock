// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use crate::{
    Error, Result, budget::ExperienceBudget, core::*, experimentation::StrategyExperiment,
};

use super::*;

pub trait EvidenceDiversityPolicy: Send + Sync {
    fn assess(&self, paths: &[EvidencePath]) -> EvidenceDiversityAssessment;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicEvidenceDiversityPolicy;

fn insert_value(
    values: &mut Vec<DependencyValue>,
    kind: EpistemicDependencyKind,
    value: Option<&str>,
) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        values.push(DependencyValue {
            kind,
            value: value.to_owned(),
        });
    }
}

pub fn dependency_values(path: &EvidencePath) -> Vec<DependencyValue> {
    let dependencies = &path.dependencies;
    let mut values = Vec::new();
    insert_value(
        &mut values,
        EpistemicDependencyKind::Model,
        dependencies.model_family.as_deref(),
    );
    insert_value(
        &mut values,
        EpistemicDependencyKind::Model,
        dependencies.model_version.as_deref(),
    );
    insert_value(
        &mut values,
        EpistemicDependencyKind::AgentRuntime,
        dependencies.agent_runtime.as_deref(),
    );
    insert_value(
        &mut values,
        EpistemicDependencyKind::Prompt,
        dependencies.system_prompt_family.as_deref(),
    );
    insert_value(
        &mut values,
        EpistemicDependencyKind::Experience,
        dependencies.experience_profile.as_deref(),
    );
    for value in &dependencies.retrieval_sources {
        insert_value(
            &mut values,
            EpistemicDependencyKind::RetrievalSource,
            Some(value),
        );
    }
    for value in &dependencies.external_documents {
        insert_value(
            &mut values,
            EpistemicDependencyKind::ExternalEvidence,
            Some(value),
        );
    }
    for tool in &dependencies.tools {
        insert_value(
            &mut values,
            EpistemicDependencyKind::Tool,
            Some(&format!("{}@{}", tool.name, tool.version)),
        );
    }
    for value in &dependencies.evaluators {
        insert_value(&mut values, EpistemicDependencyKind::Evaluator, Some(value));
    }
    for evaluator in &dependencies.evaluator_identities {
        insert_value(
            &mut values,
            EpistemicDependencyKind::Evaluator,
            Some(&evaluator.dependency_key()),
        );
    }
    insert_value(
        &mut values,
        EpistemicDependencyKind::Environment,
        dependencies.environment_family.as_deref(),
    );
    for lesson in &dependencies.originating_lessons {
        insert_value(
            &mut values,
            EpistemicDependencyKind::Experience,
            Some(&lesson.to_string()),
        );
    }
    for node in &dependencies.originating_federated_nodes {
        insert_value(
            &mut values,
            EpistemicDependencyKind::ExternalEvidence,
            Some(&node.to_string()),
        );
    }
    values.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.value.cmp(&right.value))
    });
    values.dedup();
    values
}

fn distinct(values: impl Iterator<Item = String>) -> usize {
    values
        .filter(|value| !value.trim().is_empty())
        .collect::<BTreeSet<_>>()
        .len()
}

impl EvidenceDiversityPolicy for DeterministicEvidenceDiversityPolicy {
    fn assess(&self, paths: &[EvidencePath]) -> EvidenceDiversityAssessment {
        let source_type_count = paths
            .iter()
            .map(|path| path.source.kind())
            .collect::<BTreeSet<_>>()
            .len();
        let model_family_count = distinct(
            paths
                .iter()
                .filter_map(|path| path.dependencies.model_family.clone()),
        );
        let agent_runtime_count = distinct(
            paths
                .iter()
                .filter_map(|path| path.dependencies.agent_runtime.clone()),
        );
        let retrieval_source_count = distinct(
            paths
                .iter()
                .flat_map(|path| path.dependencies.retrieval_sources.clone()),
        );
        let evaluator_count = distinct(paths.iter().flat_map(|path| {
            path.dependencies.evaluators.iter().cloned().chain(
                path.dependencies
                    .evaluator_identities
                    .iter()
                    .map(EvaluatorIdentity::dependency_key),
            )
        }));
        let environment_count = distinct(
            paths
                .iter()
                .filter_map(|path| path.dependencies.environment_family.clone()),
        );

        let mut indexed: BTreeMap<(EpistemicDependencyKind, String), Vec<EvidencePathId>> =
            BTreeMap::new();
        for path in paths {
            for value in dependency_values(path) {
                indexed
                    .entry((value.kind, value.value))
                    .or_default()
                    .push(path.id.clone());
            }
        }
        let mut dependency_overlaps = indexed
            .into_iter()
            .filter_map(|((kind, shared_value), mut members)| {
                members.sort();
                members.dedup();
                (members.len() > 1).then_some(DependencyOverlap {
                    kind,
                    shared_value,
                    paths: members,
                })
            })
            .collect::<Vec<_>>();
        dependency_overlaps.sort_by(|left, right| {
            right
                .paths
                .len()
                .cmp(&left.paths.len())
                .then_with(|| left.kind.cmp(&right.kind))
                .then_with(|| left.shared_value.cmp(&right.shared_value))
        });

        let mut fingerprints: BTreeMap<String, Vec<EvidencePathId>> = BTreeMap::new();
        for path in paths {
            if !path.context.fingerprint.hash.is_empty() {
                fingerprints
                    .entry(path.context.fingerprint.hash.clone())
                    .or_default()
                    .push(path.id.clone());
            }
        }
        fingerprints.retain(|_, members| members.len() > 1);

        let mut missing_metadata = Vec::new();
        for path in paths {
            if matches!(path.source, EvidenceSource::Agent { .. })
                && path.dependencies.model_family.is_none()
            {
                missing_metadata.push(format!("{} model family", path.id));
            }
            if matches!(path.source, EvidenceSource::Agent { .. })
                && path.dependencies.retrieval_sources.is_empty()
            {
                missing_metadata.push(format!("{} retrieval sources", path.id));
            }
            if path.dependencies.evaluators.is_empty()
                && path.dependencies.evaluator_identities.is_empty()
            {
                missing_metadata.push(format!("{} evaluator", path.id));
            }
        }

        let empirical = paths.iter().any(|path| {
            matches!(
                path.source,
                EvidenceSource::Experiment { .. }
                    | EvidenceSource::Chaos { .. }
                    | EvidenceSource::RecoveryTrial { .. }
            )
        });
        let all_path_overlap = dependency_overlaps.iter().any(|overlap| {
            overlap.paths.len() == paths.len()
                && matches!(
                    overlap.kind,
                    EpistemicDependencyKind::Experience
                        | EpistemicDependencyKind::Tool
                        | EpistemicDependencyKind::Evaluator
                        | EpistemicDependencyKind::ExternalEvidence
                )
        });
        let positive_dimensions = usize::from(source_type_count > 1)
            + usize::from(model_family_count > 1)
            + usize::from(agent_runtime_count > 1)
            + usize::from(retrieval_source_count > 1)
            + usize::from(evaluator_count > 1)
            + usize::from(environment_count > 1);
        let complete_enough = missing_metadata.len() < paths.len().saturating_mul(2);
        let diversity_class = if paths.is_empty() {
            DiversityClass::Unknown
        } else if paths.len() == 1 {
            DiversityClass::Low
        } else if !complete_enough && positive_dimensions < 2 {
            DiversityClass::Unknown
        } else if !fingerprints.is_empty()
            || (all_path_overlap && positive_dimensions < 2)
            || (source_type_count == 1 && positive_dimensions < 2)
        {
            DiversityClass::Low
        } else if empirical
            && source_type_count >= 2
            && evaluator_count >= 2
            && positive_dimensions >= 3
            && !all_path_overlap
        {
            DiversityClass::High
        } else {
            DiversityClass::Moderate
        };
        let mut caveats = Vec::new();
        if all_path_overlap {
            caveats.push(
                "A known dependency is shared by every path; this is a common-mode risk".into(),
            );
        }
        if !fingerprints.is_empty() {
            caveats.push("Multiple observations have the same epistemic context fingerprint (high correlation risk)".into());
        }
        if !missing_metadata.is_empty() {
            caveats.push("Dependency metadata is incomplete; unknown dependencies were not credited as diversity".into());
        }
        if model_family_count > 1 && all_path_overlap {
            caveats.push("Different model families still share another dominant dependency".into());
        }
        EvidenceDiversityAssessment {
            path_count: paths.len(),
            source_type_count,
            model_family_count,
            agent_runtime_count,
            retrieval_source_count,
            evaluator_count,
            environment_count,
            dependency_overlaps,
            diversity_class,
            duplicate_fingerprints: fingerprints,
            missing_metadata,
            caveats,
        }
    }
}

pub fn context_fingerprint(
    dependencies: &EpistemicDependencySet,
) -> Result<EpistemicContextFingerprint> {
    fn hash(values: impl IntoIterator<Item = String>) -> String {
        let mut values = values.into_iter().collect::<Vec<_>>();
        values.sort();
        values.dedup();
        blake3::hash(values.join("\u{1f}").as_bytes())
            .to_hex()
            .to_string()
    }
    let active_experience_hash = hash(
        dependencies
            .originating_lessons
            .iter()
            .map(ToString::to_string)
            .chain(dependencies.experience_profile.clone()),
    );
    let retrieval_source_hash = hash(dependencies.retrieval_sources.clone());
    let toolset_hash = hash(
        dependencies
            .tools
            .iter()
            .map(|tool| format!("{}@{}", tool.name, tool.version)),
    );
    let evaluator_hash = hash(
        dependencies.evaluators.iter().cloned().chain(
            dependencies
                .evaluator_identities
                .iter()
                .map(EvaluatorIdentity::dependency_key),
        ),
    );
    let material = serde_json::to_vec(&(
        dependencies.model_family.clone(),
        &active_experience_hash,
        &retrieval_source_hash,
        &toolset_hash,
        &evaluator_hash,
    ))?;
    Ok(EpistemicContextFingerprint {
        hash: blake3::hash(&material).to_hex().to_string(),
        model_family: dependencies.model_family.clone(),
        active_experience_hash,
        retrieval_source_hash,
        toolset_hash,
        evaluator_hash,
    })
}

pub fn dependency_graph(paths: &[EvidencePath]) -> EpistemicDependencyGraph {
    let mut nodes = BTreeMap::new();
    let mut edges = BTreeSet::new();
    let mut memberships: BTreeMap<(EpistemicDependencyKind, String), Vec<String>> = BTreeMap::new();
    for path in paths {
        let path_id = path.id.to_string();
        nodes.insert(
            path_id.clone(),
            EpistemicDependencyNode {
                id: path_id.clone(),
                kind: EpistemicDependencyNodeKind::EvidencePath,
                label: path.source.label(),
            },
        );
        let source_id = format!("source:{}", path.source.label());
        nodes
            .entry(source_id.clone())
            .or_insert(EpistemicDependencyNode {
                id: source_id.clone(),
                kind: EpistemicDependencyNodeKind::Source,
                label: path.source.label(),
            });
        edges.insert((
            path_id.clone(),
            source_id,
            EpistemicDependencyEdgeKind::DerivedFrom,
        ));
        for value in dependency_values(path) {
            let dependency_id = format!(
                "dependency:{:?}:{}",
                value.kind,
                blake3::hash(value.value.as_bytes()).to_hex()
            );
            nodes
                .entry(dependency_id.clone())
                .or_insert(EpistemicDependencyNode {
                    id: dependency_id.clone(),
                    kind: EpistemicDependencyNodeKind::Dependency,
                    label: format!("{:?}: {}", value.kind, value.value),
                });
            let edge_kind = match value.kind {
                EpistemicDependencyKind::RetrievalSource
                | EpistemicDependencyKind::ExternalEvidence => {
                    EpistemicDependencyEdgeKind::RetrievedFrom
                }
                EpistemicDependencyKind::Evaluator => EpistemicDependencyEdgeKind::EvaluatedBy,
                _ => EpistemicDependencyEdgeKind::Uses,
            };
            edges.insert((path_id.clone(), dependency_id, edge_kind));
            memberships
                .entry((value.kind, value.value))
                .or_default()
                .push(path_id.clone());
        }
    }
    for members in memberships.values_mut().filter(|members| members.len() > 1) {
        members.sort();
        members.dedup();
        for pair in members.windows(2) {
            edges.insert((
                pair[0].clone(),
                pair[1].clone(),
                EpistemicDependencyEdgeKind::SharesWith,
            ));
        }
    }
    EpistemicDependencyGraph {
        nodes: nodes.into_values().collect(),
        edges: edges
            .into_iter()
            .map(|(from, to, kind)| EpistemicDependencyEdge { from, to, kind })
            .collect(),
    }
}

pub fn fault_domains(paths: &[EvidencePath]) -> Vec<EpistemicFaultDomain> {
    DeterministicEvidenceDiversityPolicy
        .assess(paths)
        .dependency_overlaps
        .into_iter()
        .map(|overlap| {
            let material = format!("{:?}\u{1f}{}", overlap.kind, overlap.shared_value);
            let digest = blake3::hash(material.as_bytes());
            let mut bytes = [0_u8; 16];
            bytes.copy_from_slice(&digest.as_bytes()[..16]);
            bytes[6] = (bytes[6] & 0x0f) | 0x40;
            bytes[8] = (bytes[8] & 0x3f) | 0x80;
            let id = format!("fault-domain-{}", uuid::Uuid::from_bytes(bytes))
                .parse()
                .expect("derived canonical fault-domain ID");
            EpistemicFaultDomain {
                id,
                kind: overlap.kind,
                dependency: overlap.shared_value,
                members: overlap.paths,
            }
        })
        .collect()
}

pub trait EvidenceFusionPolicy: Send + Sync {
    fn fuse(
        &self,
        claim: &Claim,
        paths: &[EvidencePath],
        diversity: &EvidenceDiversityAssessment,
    ) -> Result<FusedEvidenceAssessment>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicEvidenceFusionPolicy;

impl EvidenceFusionPolicy for DeterministicEvidenceFusionPolicy {
    fn fuse(
        &self,
        claim: &Claim,
        paths: &[EvidencePath],
        diversity: &EvidenceDiversityAssessment,
    ) -> Result<FusedEvidenceAssessment> {
        if paths.iter().any(|path| path.claim.id != claim.id) {
            return Err(Error::InvalidInput(
                "Cannot fuse evidence paths for another Claim".into(),
            ));
        }
        let ids = |outcome| {
            paths
                .iter()
                .filter(|path| path.outcome == outcome)
                .map(|path| path.id.clone())
                .collect::<Vec<_>>()
        };
        let support_paths = ids(EvidenceOutcome::Supports);
        let contradiction_paths = ids(EvidenceOutcome::Contradicts);
        let inconclusive_paths = ids(EvidenceOutcome::Inconclusive);
        let status = if !support_paths.is_empty() && !contradiction_paths.is_empty() {
            FusedEvidenceStatus::Disputed
        } else if support_paths.is_empty() {
            FusedEvidenceStatus::Inconclusive
        } else if diversity.diversity_class == DiversityClass::High {
            FusedEvidenceStatus::DiverseSupport
        } else if support_paths.len() == 1 {
            FusedEvidenceStatus::WeakSupport
        } else {
            FusedEvidenceStatus::Supported
        };
        let mut caveats = diversity.caveats.clone();
        if status == FusedEvidenceStatus::Disputed {
            caveats.push(
                "Contradictory evidence is preserved; majority count does not resolve the dispute"
                    .into(),
            );
        }
        Ok(FusedEvidenceAssessment {
            claim: claim.id.clone(),
            support_paths,
            contradiction_paths,
            inconclusive_paths,
            diversity: diversity.clone(),
            status,
            caveats,
        })
    }
}

pub fn evidence_echo_assessment(paths: &[EvidencePath]) -> EvidenceEchoAssessment {
    let immediate_nodes = paths
        .iter()
        .filter_map(|path| match &path.source {
            EvidenceSource::Federation { node_id } => Some(node_id.to_string()),
            _ => None,
        })
        .collect::<BTreeSet<_>>()
        .len();
    let mut roots = BTreeMap::<String, usize>::new();
    for root in paths
        .iter()
        .flat_map(|path| path.context.root_evidence_origins.iter())
    {
        *roots.entry(root.clone()).or_default() += 1;
    }
    let dominant = roots
        .iter()
        .max_by_key(|(_, count)| **count)
        .map(|(root, count)| (root.clone(), *count));
    let status = if immediate_nodes <= 1 {
        EvidenceEchoStatus::None
    } else if roots.len() <= 1
        || dominant
            .as_ref()
            .is_some_and(|(_, count)| *count * 4 >= paths.len().saturating_mul(3))
    {
        EvidenceEchoStatus::Strong
    } else if roots.len() < immediate_nodes {
        EvidenceEchoStatus::Possible
    } else {
        EvidenceEchoStatus::None
    };
    let mut caveats = Vec::new();
    if immediate_nodes > 0 && roots.is_empty() {
        caveats.push(
            "Federated root-origin metadata is missing; unknown origin is not credited as diverse"
                .into(),
        );
    }
    EvidenceEchoAssessment {
        origin_diversity: OriginDiversity {
            immediate_nodes,
            root_evidence_origins: roots.len(),
        },
        dominant_root: dominant.as_ref().map(|(root, _)| root.clone()),
        dominant_root_paths: dominant.map_or(0, |(_, count)| count),
        status,
        caveats,
    }
}

pub fn contrast(left: &EvidencePath, right: &EvidencePath) -> EvidenceContrast {
    let left_values = dependency_values(left)
        .into_iter()
        .map(|value| ((value.kind, value.value.clone()), value))
        .collect::<BTreeMap<_, _>>();
    let right_values = dependency_values(right)
        .into_iter()
        .map(|value| ((value.kind, value.value.clone()), value))
        .collect::<BTreeMap<_, _>>();
    let shared = left_values
        .iter()
        .filter(|(key, _)| right_values.contains_key(key))
        .map(|(_, value)| value.clone())
        .collect();
    let left_kinds = left_values
        .keys()
        .map(|(kind, _)| *kind)
        .collect::<BTreeSet<_>>();
    let right_kinds = right_values
        .keys()
        .map(|(kind, _)| *kind)
        .collect::<BTreeSet<_>>();
    let mut different = left_kinds
        .symmetric_difference(&right_kinds)
        .copied()
        .collect::<Vec<_>>();
    for kind in left_kinds.intersection(&right_kinds) {
        let left_for_kind = left_values
            .keys()
            .filter(|(candidate, _)| candidate == kind)
            .collect::<BTreeSet<_>>();
        let right_for_kind = right_values
            .keys()
            .filter(|(candidate, _)| candidate == kind)
            .collect::<BTreeSet<_>>();
        if left_for_kind != right_for_kind {
            different.push(*kind);
        }
    }
    different.sort();
    different.dedup();
    let mut missing = Vec::new();
    if left.dependencies.model_family.is_none() {
        missing.push(format!("{} model family", left.id));
    }
    if right.dependencies.model_family.is_none() {
        missing.push(format!("{} model family", right.id));
    }
    EvidenceContrast {
        left: left.id.clone(),
        right: right.id.clone(),
        shared,
        different,
        missing,
    }
}

pub trait AgentRegistry: Send + Sync {
    fn profiles(&self) -> Result<Vec<AgentCapabilityProfile>>;
}

pub trait EvidenceAcquisitionPlanner: Send + Sync {
    fn plan(
        &self,
        claim: &Claim,
        current: &[EvidencePath],
        budget: &ExperienceBudget,
    ) -> Result<EvidenceAcquisitionPlan>;
}

#[derive(Clone, Debug, Default)]
pub struct DeterministicEvidenceAcquisitionPlanner {
    pub agents: Vec<AgentCapabilityProfile>,
    pub minimum: Option<DiversityClass>,
}

impl EvidenceAcquisitionPlanner for DeterministicEvidenceAcquisitionPlanner {
    fn plan(
        &self,
        claim: &Claim,
        current: &[EvidencePath],
        budget: &ExperienceBudget,
    ) -> Result<EvidenceAcquisitionPlan> {
        let diversity = DeterministicEvidenceDiversityPolicy.assess(current);
        let minimum = self.minimum.unwrap_or(DiversityClass::Moderate);
        if diversity.diversity_class.satisfies(minimum) {
            return Ok(EvidenceAcquisitionPlan {
                claim: claim.id.clone(),
                actions: vec![],
                rationale: vec![format!(
                    "Current {:?} evidence diversity satisfies the {:?} requirement",
                    diversity.diversity_class, minimum
                )],
                requirement_satisfied: true,
                stop_reason: Some("evidence requirement satisfied".into()),
            });
        }
        if budget.max_realities == 0 && budget.max_agent_runs == 0 {
            return Ok(EvidenceAcquisitionPlan {
                claim: claim.id.clone(),
                actions: vec![],
                rationale: vec!["Experience budget is exhausted".into()],
                requirement_satisfied: false,
                stop_reason: Some("budget exhausted".into()),
            });
        }
        let dominant = diversity.dependency_overlaps.first();
        let mut actions = Vec::new();
        let mut rationale = Vec::new();
        if let Some(overlap) = dominant {
            rationale.push(format!(
                "Challenge dominant {:?} dependency {} shared by {} paths",
                overlap.kind,
                overlap.shared_value,
                overlap.paths.len()
            ));
            actions.push(EvidenceAcquisitionAction::ChallengeClaim {
                strategy: match overlap.kind {
                    EpistemicDependencyKind::Experience => {
                        ChallengeStrategy::RemoveDominantExperience
                    }
                    EpistemicDependencyKind::Evaluator => ChallengeStrategy::AlternativeEvaluator,
                    EpistemicDependencyKind::Tool => ChallengeStrategy::AlternativeTool,
                    EpistemicDependencyKind::Environment => {
                        ChallengeStrategy::AlternativeEnvironment
                    }
                    _ => ChallengeStrategy::CounterfactualExperiment,
                },
            });
        } else {
            actions.push(EvidenceAcquisitionAction::ChallengeClaim {
                strategy: ChallengeStrategy::CounterfactualExperiment,
            });
            rationale.push(
                "Acquire a controlled disconfirming path instead of another agreeing vote".into(),
            );
        }
        if diversity.evaluator_count <= 1 && actions.len() < budget.max_realities {
            actions.push(EvidenceAcquisitionAction::RunAlternativeEvaluator {
                evaluator: "alternative-evaluator-required".into(),
            });
            rationale.push("Evaluator diversity is absent or insufficient".into());
        }
        if budget.max_agent_runs > 0
            && let Some(agent) = self
                .agents
                .iter()
                .filter(|candidate| candidate.available)
                .max_by_key(|candidate| {
                    let novel_model = candidate.model_family.as_ref().is_some_and(|model| {
                        !current
                            .iter()
                            .any(|path| path.dependencies.model_family.as_ref() == Some(model))
                    });
                    (novel_model, &candidate.integration_mode, &candidate.runtime)
                })
        {
            let excluded = dominant
                .filter(|overlap| overlap.kind == EpistemicDependencyKind::Experience)
                .map(|overlap| {
                    vec![ExperienceRef {
                        kind: "experience".into(),
                        id: overlap.shared_value.clone(),
                    }]
                })
                .unwrap_or_default();
            actions.push(EvidenceAcquisitionAction::AskAgent {
                agent: agent.identity.clone(),
                context_mode: if excluded.is_empty() {
                    EvidenceContextMode::IndependentRetrieval
                } else {
                    EvidenceContextMode::BlindToSelectedExperience { excluded }
                },
            });
            rationale.push("Selected the available agent/context expected to add a known dependency difference".into());
        }
        let limit = budget
            .max_realities
            .saturating_add(budget.max_agent_runs)
            .max(1);
        actions.truncate(limit);
        Ok(EvidenceAcquisitionPlan {
            claim: claim.id.clone(),
            actions,
            rationale,
            requirement_satisfied: false,
            stop_reason: None,
        })
    }
}

pub trait ControlledExperimentExecutor: Send + Sync {
    fn execute(&self, request: &ExperimentRequest) -> Result<StrategyExperiment>;
}

pub struct MultiAgentExperimentCoordinator {
    pub planner: Arc<dyn EvidenceAcquisitionPlanner>,
    pub agent_registry: Arc<dyn AgentRegistry>,
    pub experiment_engine: Arc<dyn ControlledExperimentExecutor>,
    pub diversity_policy: Arc<dyn EvidenceDiversityPolicy>,
}

impl MultiAgentExperimentCoordinator {
    pub fn plan(
        &self,
        claim: &Claim,
        current: &[EvidencePath],
        budget: &ExperienceBudget,
    ) -> Result<EvidenceAcquisitionPlan> {
        let _available_agents = self.agent_registry.profiles()?;
        let assessment = self.diversity_policy.assess(current);
        if assessment.diversity_class == DiversityClass::High {
            return Ok(EvidenceAcquisitionPlan {
                claim: claim.id.clone(),
                actions: vec![],
                rationale: vec!["High known evidence diversity already exists".into()],
                requirement_satisfied: true,
                stop_reason: Some("evidence requirement satisfied".into()),
            });
        }
        self.planner.plan(claim, current, budget)
    }
}

pub fn epistemic_gaps(
    paths: &[EvidencePath],
    assessment: &EvidenceDiversityAssessment,
) -> Vec<String> {
    let mut gaps = Vec::new();
    if !paths
        .iter()
        .any(|path| matches!(path.source, EvidenceSource::Experiment { .. }))
    {
        gaps.push("controlled empirical path".into());
    }
    if assessment.evaluator_count < 2 {
        gaps.push("alternative evaluator".into());
    }
    if assessment.source_type_count < 2 {
        gaps.push("second evidence source type".into());
    }
    if assessment.retrieval_source_count < 2 {
        gaps.push("independent or varied retrieval source".into());
    }
    gaps.extend(
        assessment
            .missing_metadata
            .iter()
            .map(|item| format!("missing dependency metadata: {item}")),
    );
    gaps
}

pub fn build_report(
    claim: Claim,
    paths: Vec<EvidencePath>,
    budget: &ExperienceBudget,
) -> Result<EpistemicReport> {
    let diversity = DeterministicEvidenceDiversityPolicy.assess(&paths);
    let fused = DeterministicEvidenceFusionPolicy.fuse(&claim, &paths, &diversity)?;
    let graph = dependency_graph(&paths);
    let domains = fault_domains(&paths);
    let echoes = evidence_echo_assessment(&paths);
    let gaps = epistemic_gaps(&paths, &diversity);
    let challenge =
        DeterministicEvidenceAcquisitionPlanner::default().plan(&claim, &paths, budget)?;
    Ok(EpistemicReport {
        claim,
        paths,
        graph,
        diversity,
        domains,
        fused,
        echoes,
        gaps,
        challenge,
    })
}
