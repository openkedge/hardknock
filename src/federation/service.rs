// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{
    Error, Result,
    bridge::config::Config,
    budget::ExperienceBudget,
    cancellation::Cancellation,
    core::{
        AgentIdentity, CandidateId, FederatedConflictId, FederatedObjectId,
        FederationReproductionId, StateRef,
    },
    experience::{Experience, Outcome},
    experimentation::{
        CandidateExecution, ComparisonCriteria, ExperimentCandidate, ExperimentCapabilities,
        ExperimentIntent, ExperimentOrchestrator, ExperimentOrigin, ExperimentRequest,
        ExperimentStartingState, SnapshotSource,
    },
    lesson::{ActionPattern, LessonStatus},
    resilience::{RecoveryStep, ReflexResponse},
    retrieval::QueryContext,
    store::Store,
};
use chrono::Utc;
use std::{
    collections::BTreeSet,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

pub trait ExperiencePublishPolicy {
    fn can_publish(&self, object: &ExperienceObjectRef) -> Result<()>;
}
pub struct ConservativePublishPolicy<'a> {
    pub store: &'a Store,
}
impl ExperiencePublishPolicy for ConservativePublishPolicy<'_> {
    fn can_publish(&self, object: &ExperienceObjectRef) -> Result<()> {
        match object.kind.as_str() {
            "lesson" => {
                let lesson = self.store.lesson(&object.id.parse()?)?;
                if lesson.status != LessonStatus::Validated {
                    return Err(Error::Intervention(
                        "Default federation policy publishes only locally validated Lessons".into(),
                    ));
                }
            }
            "skill" => {
                let skill = self.store.skill(&object.id)?;
                if !matches!(
                    skill.status,
                    crate::resilience::SkillStatus::Supported
                        | crate::resilience::SkillStatus::Validated
                ) {
                    return Err(Error::Intervention(
                        "Only supported or validated Skills are exportable".into(),
                    ));
                }
            }
            _ => {
                return Err(Error::InvalidInput(
                    "V0.7 exports Lesson objects and Skill packages".into(),
                ));
            }
        }
        Ok(())
    }
}

#[allow(async_fn_in_trait)]
pub trait FederationService {
    fn export_lesson(
        &self,
        id: &crate::core::LessonId,
        labels: Vec<String>,
    ) -> Result<SignedExperienceBundle>;
    fn export_skill(&self, name: &str, labels: Vec<String>) -> Result<SignedExperienceBundle>;
    fn import(&self, signed: SignedExperienceBundle, local: &QueryContext) -> Result<ImportReport>;
    fn evaluate_external(
        &self,
        id: &FederatedObjectId,
        local: &QueryContext,
    ) -> Result<FederatedObject>;
    async fn reproduce(
        &self,
        id: &FederatedObjectId,
        state: StateRef,
        checks: Vec<String>,
        cancel: &Cancellation,
    ) -> Result<FederationReproduction>;
    fn promote(
        &self,
        id: &FederatedObjectId,
        application: &crate::core::ExperienceId,
    ) -> Result<FederatedObject>;
}

pub struct LocalFederationService<'a> {
    pub store: &'a Store,
    pub config: &'a Config,
}
impl LocalFederationService<'_> {
    pub fn identity(&self) -> Result<NodeIdentity> {
        let identity = NodeIdentity::load_or_create(
            &self.store.home,
            &self.config.federation.node_name,
            ExperienceNodeType::LocalDeveloper,
        )?;
        let node = self.store.save_experience_node(&identity.node)?;
        if node.id != identity.node.id {
            return Err(Error::Intervention(
                "Stored federation node does not match signing identity".into(),
            ));
        }
        Ok(identity)
    }
    fn context(exp: &Experience) -> EvidenceContext {
        let versions = exp
            .context
            .environment
            .facts
            .iter()
            .filter(|(k, _)| k.contains("version"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        EvidenceContext {
            repository_family: Some(exp.context.repository.name.clone()),
            markers: exp.context.markers.clone(),
            tags: exp.context.tags.clone(),
            os: Some(exp.context.environment.os.clone()),
            arch: Some(exp.context.environment.arch.clone()),
            versions,
            environment_fingerprint: Some(exp.context.environment.fingerprint.clone()),
        }
    }
    fn dependencies(exp: &Experience) -> EpistemicDependencies {
        EpistemicDependencies {
            model_family: exp.agent.model.clone(),
            agent_runtime: exp.agent.version.clone(),
            evaluator: Some("hardknock-required-checks-v1".into()),
            environment_family: exp
                .context
                .tags
                .iter()
                .find(|t| t.starts_with("fixture-family:"))
                .cloned(),
            external_sources: vec![],
        }
    }
    fn hash(value: &impl serde::Serialize) -> Result<String> {
        Ok(blake3::hash(&serde_json::to_vec(value)?)
            .to_hex()
            .to_string())
    }
    fn provenance_id(parts: &impl serde::Serialize) -> Result<ProvenanceNodeId> {
        ProvenanceNodeId::from_digest(&Self::hash(parts)?)
    }
    fn identity_for(
        node: &ExperienceNodeId,
        kind: &str,
        id: &str,
        lineage_value: &impl serde::Serialize,
    ) -> Result<FederatedObjectIdentity> {
        Ok(FederatedObjectIdentity {
            origin_node: node.clone(),
            origin_object_id: id.into(),
            lineage_hash: Self::hash(&(kind, lineage_value))?,
        })
    }
    fn portable_experience(
        &self,
        node: &ExperienceNodeId,
        exp: &Experience,
    ) -> Result<(PortableExperience, ProvenanceNode)> {
        let identity = Self::identity_for(
            node,
            "experience",
            &exp.id.to_string(),
            &(
                exp.starting_state.tree_hash.clone(),
                exp.context.environment.fingerprint.clone(),
                &exp.outcome,
                exp.evaluation.status,
                exp.evaluation.summary.clone(),
            ),
        )?;
        let provenance_ref = Self::provenance_id(&(node, "experience", &identity.lineage_hash))?;
        let portable = PortableExperience {
            identity: identity.clone(),
            created_at: exp.created_at,
            goal: "normalized execution evidence".into(),
            context: Self::context(exp),
            starting_state_hash: exp.starting_state.tree_hash.clone(),
            outcome: exp.outcome,
            evaluation_summary: exp.evaluation.summary.clone(),
            originating_agent: exp.agent.clone(),
            dependencies: Self::dependencies(exp),
            provenance_ref: provenance_ref.clone(),
        };
        let pnode = ProvenanceNode {
            id: provenance_ref,
            kind: ProvenanceNodeKind::Experience,
            external_id: exp.id.to_string(),
            node: node.clone(),
            lineage_hash: Some(identity.lineage_hash),
            summary: format!("{:?}: {}", exp.outcome, exp.evaluation.summary),
        };
        Ok((portable, pnode))
    }
    fn base_manifest(
        node: &ExperienceNodeId,
        scope: ExportScope,
        labels: Vec<String>,
        evidence_count: usize,
    ) -> Result<ExperienceBundleManifest> {
        Ok(ExperienceBundleManifest {
            bundle_id: BundleId::from_digest(&"0".repeat(64))?,
            schema_version: BUNDLE_SCHEMA_V1.into(),
            producer: node.clone(),
            created_at: Utc::now(),
            scope,
            evidence_count,
            minimum_hardknock_version: Some(env!("CARGO_PKG_VERSION").into()),
            labels,
            visibility: FederationVisibility::Team,
            ancestry: BundleAncestry {
                parent_bundles: vec![],
                source_nodes: vec![node.clone()],
            },
        })
    }
    fn finish_bundle(
        &self,
        mut bundle: ExperienceBundle,
        repo: Option<&Path>,
    ) -> Result<SignedExperienceBundle> {
        bundle = DeterministicFederationRedaction { repository: repo }.redact(bundle)?;
        bundle.manifest.bundle_id = bundle.computed_id()?;
        bundle.validate(&self.config.federation.limits)?;
        validate_bundle_references(&bundle)?;
        validate_safe_payload(
            &serde_json::to_value(&bundle)?,
            self.config.federation.limits.max_nesting_depth,
        )?;
        let identity = self.identity()?;
        identity.sign(bundle)
    }
    pub fn read_bundle(&self, path: &Path) -> Result<SignedExperienceBundle> {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > self.config.federation.limits.max_bundle_bytes
        {
            return Err(Error::InvalidInput(
                "Import requires a bounded regular .hkexp file".into(),
            ));
        }
        let mut bytes = Vec::new();
        File::open(path)?
            .take(self.config.federation.limits.max_bundle_bytes + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > self.config.federation.limits.max_bundle_bytes {
            return Err(Error::InvalidInput("Bundle size limit exceeded".into()));
        }
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        validate_safe_payload(&value, self.config.federation.limits.max_nesting_depth)?;
        Ok(serde_json::from_value(value)?)
    }
    pub fn write_bundle(&self, signed: &SignedExperienceBundle, output: &Path) -> Result<PathBuf> {
        if output.exists() {
            return Err(Error::InvalidInput(
                "Refusing to overwrite an existing bundle".into(),
            ));
        }
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?
        }
        let mut options = std::fs::OpenOptions::new();
        use std::os::unix::fs::OpenOptionsExt;
        options.write(true).create_new(true).mode(0o600);
        let mut file = options.open(output)?;
        serde_json::to_writer_pretty(&mut file, signed)?;
        use std::io::Write;
        file.flush()?;
        file.sync_all()?;
        self.store.save_federation_bundle(
            signed,
            "published",
            AuthenticityStatus::SignatureValid,
            Some(&output.display().to_string()),
        )?;
        self.store.audit(
            "bundle_exported",
            Some(&signed.manifest.bundle_id.to_string()),
            &format!("Signed, redacted bundle written to {}", output.display()),
        )?;
        Ok(output.into())
    }
    pub fn publish(&self, signed: &SignedExperienceBundle, target: &Path) -> Result<PathBuf> {
        let transport =
            FilesystemTransport::new(target, self.config.federation.limits.max_bundle_bytes)?;
        let path = transport.publish(signed)?;
        self.store.save_federation_bundle(
            signed,
            "published",
            AuthenticityStatus::SignatureValid,
            Some(&path.display().to_string()),
        )?;
        self.store.audit(
            "bundle_published",
            Some(&signed.manifest.bundle_id.to_string()),
            &format!(
                "Published through filesystem transport to {}",
                path.display()
            ),
        )?;
        Ok(path)
    }
    pub fn reexport(
        &self,
        id: &FederatedObjectId,
        labels: Vec<String>,
    ) -> Result<SignedExperienceBundle> {
        let object = self.store.federated_object(id)?;
        let identity = self.identity()?;
        let mut lessons = vec![];
        let mut skills = vec![];
        let mut reflexes = vec![];
        let mut recoveries = vec![];
        let mut envelopes = vec![];
        match object.object_type.as_str() {
            "lesson" => lessons.push(serde_json::from_value(object.object.clone())?),
            "skill" => skills.push(serde_json::from_value(object.object.clone())?),
            "reflex" => reflexes.push(serde_json::from_value(object.object.clone())?),
            "recovery" => recoveries.push(serde_json::from_value(object.object.clone())?),
            "envelope" => envelopes.push(serde_json::from_value(object.object.clone())?),
            _ => {
                return Err(Error::InvalidInput(
                    "This external object type cannot be re-exported".into(),
                ));
            }
        }
        let mut graph = self
            .store
            .provenance_graph(&object.identity.origin_object_id)?;
        let local_node_id = Self::provenance_id(&(identity.node.id.clone(), "node"))?;
        if !graph.nodes.iter().any(|n| n.id == local_node_id) {
            graph.nodes.push(ProvenanceNode {
                id: local_node_id,
                kind: ProvenanceNodeKind::Node,
                external_id: identity.node.id.to_string(),
                node: identity.node.id.clone(),
                lineage_hash: None,
                summary: identity.node.name.clone(),
            });
        }
        let mut experiences = vec![];
        for reproduction in self
            .store
            .reproductions()?
            .into_iter()
            .filter(|r| r.object_id == *id)
        {
            for experience_id in reproduction.experience_ids {
                let exp = self.store.experience(&experience_id.parse()?)?;
                let (portable, node) = self.portable_experience(&identity.node.id, &exp)?;
                if !graph.nodes.iter().any(|n| n.id == node.id) {
                    graph.nodes.push(node);
                }
                experiences.push(portable);
            }
        }
        let evidence_count = experiences.len() + 1;
        let manifest = ExperienceBundleManifest {
            ancestry: BundleAncestry {
                parent_bundles: vec![object.origin_bundle.clone()],
                source_nodes: vec![
                    object.identity.origin_node.clone(),
                    identity.node.id.clone(),
                ],
            },
            ..Self::base_manifest(
                &identity.node.id,
                ExportScope::Object,
                labels,
                evidence_count,
            )?
        };
        self.finish_bundle(
            ExperienceBundle {
                manifest,
                experiences,
                lessons,
                skills,
                experiments: vec![],
                reflexes,
                recoveries,
                envelopes,
                provenance: graph,
            },
            None,
        )
    }
    pub fn export_reflex(
        &self,
        id: &crate::core::ReflexId,
        labels: Vec<String>,
    ) -> Result<SignedExperienceBundle> {
        let reflex = self.store.reflex(id)?;
        if !matches!(
            reflex.status,
            crate::resilience::ReflexStatus::Supported | crate::resilience::ReflexStatus::Active
        ) {
            return Err(Error::Intervention(
                "Only locally supported Reflex evidence is exportable".into(),
            ));
        }
        let identity = self.identity()?;
        let trial = self.store.chaos_trial(&reflex.source_trial)?;
        let source = self.store.experience(&trial.experience_id)?;
        let (exp, exp_node) = self.portable_experience(&identity.node.id, &source)?;
        let rid = Self::identity_for(
            &identity.node.id,
            "reflex",
            &reflex.id.to_string(),
            &(&reflex.trigger, reflex.response, reflex.version),
        )?;
        let prov = Self::provenance_id(&(&identity.node.id, "reflex", &rid.lineage_hash))?;
        let portable = PortableReflex {
            identity: rid.clone(),
            trigger_context: Self::context(&source),
            proposed_action: reflex.trigger.proposed_action,
            requested_response: reflex.response,
            effective_response: ReflexResponse::Advise,
            source_status: reflex.status,
            confidence: reflex.confidence,
            evidence_hashes: source
                .evidence
                .artifacts
                .iter()
                .map(|a| a.blake3.clone())
                .collect(),
            provenance_ref: prov.clone(),
        };
        let manifest = Self::base_manifest(&identity.node.id, ExportScope::Object, labels, 2)?;
        self.finish_bundle(
            ExperienceBundle {
                manifest,
                experiences: vec![exp.clone()],
                lessons: vec![],
                skills: vec![],
                experiments: vec![],
                reflexes: vec![portable],
                recoveries: vec![],
                envelopes: vec![],
                provenance: ProvenanceGraph {
                    nodes: vec![
                        exp_node,
                        ProvenanceNode {
                            id: prov.clone(),
                            kind: ProvenanceNodeKind::Reflex,
                            external_id: rid.origin_object_id,
                            node: identity.node.id.clone(),
                            lineage_hash: Some(rid.lineage_hash),
                            summary: format!(
                                "Remote requested {:?}; local effective ADVISE",
                                reflex.response
                            ),
                        },
                    ],
                    edges: vec![ProvenanceEdge {
                        source: prov,
                        target: exp.provenance_ref,
                        relationship: ProvenanceRelationship::DerivedFrom,
                    }],
                },
            },
            Some(&source.context.repository.path),
        )
    }
}

impl FederationService for LocalFederationService<'_> {
    fn export_lesson(
        &self,
        id: &crate::core::LessonId,
        labels: Vec<String>,
    ) -> Result<SignedExperienceBundle> {
        ConservativePublishPolicy { store: self.store }.can_publish(&ExperienceObjectRef {
            kind: "lesson".into(),
            id: id.to_string(),
        })?;
        let identity = self.identity()?;
        let lesson = self.store.lesson(id)?;
        let source = self.store.experience(&lesson.source_experience)?;
        let experiments: Vec<_> = self
            .store
            .experiments()?
            .into_iter()
            .filter(|e| {
                e.lesson_id == *id && e.status == crate::experiment::ExperimentStatus::Completed
            })
            .collect();
        let summary = self.store.lesson_evidence_summary(id)?;
        let (source_portable, source_node) =
            self.portable_experience(&identity.node.id, &source)?;
        let lesson_identity = Self::identity_for(
            &identity.node.id,
            "lesson",
            &lesson.id.to_string(),
            &(
                lesson.claim.clone(),
                &lesson.context_match,
                &lesson.avoid,
                &lesson.prefer,
                lesson.created_at,
            ),
        )?;
        let lesson_prov =
            Self::provenance_id(&(&identity.node.id, "lesson", &lesson_identity.lineage_hash))?;
        let portable_lesson = PortableLesson {
            identity: lesson_identity.clone(),
            claim: lesson.claim.clone(),
            context: Self::context(&source),
            avoid: lesson.avoid.clone(),
            prefer: lesson.prefer.clone(),
            evaluation_checks: source.evaluation.spec.checks.clone(),
            source_status: lesson.status,
            source_confidence: lesson.confidence,
            evidence_summary: PortableEvidenceSummary {
                support_count: summary.controlled_supports.len()
                    + summary
                        .applications
                        .iter()
                        .filter(|a| a.success && a.observed)
                        .count(),
                contradiction_count: summary.controlled_contradictions.len()
                    + summary
                        .applications
                        .iter()
                        .filter(|a| !a.success && a.observed)
                        .count(),
                experiment_count: experiments.len(),
                application_count: summary.applications.len(),
                evaluation_summaries: vec![source.evaluation.summary.clone()],
                evidence_hashes: source
                    .evidence
                    .artifacts
                    .iter()
                    .map(|a| a.blake3.clone())
                    .collect(),
            },
            originating_agents: lesson.discovered_by.clone(),
            freshness: "source-reported; local receiver must reassess".into(),
            dependencies: Self::dependencies(&source),
            provenance_ref: lesson_prov.clone(),
        };
        let mut experiences = vec![source_portable];
        let mut pnodes = vec![
            ProvenanceNode {
                id: Self::provenance_id(&(&identity.node.id, "node"))?,
                kind: ProvenanceNodeKind::Node,
                external_id: identity.node.id.to_string(),
                node: identity.node.id.clone(),
                lineage_hash: None,
                summary: identity.node.name.clone(),
            },
            source_node,
            ProvenanceNode {
                id: lesson_prov.clone(),
                kind: ProvenanceNodeKind::Lesson,
                external_id: lesson.id.to_string(),
                node: identity.node.id.clone(),
                lineage_hash: Some(lesson_identity.lineage_hash.clone()),
                summary: lesson.claim.clone(),
            },
        ];
        let mut pedges = vec![ProvenanceEdge {
            source: lesson_prov.clone(),
            target: experiences[0].provenance_ref.clone(),
            relationship: ProvenanceRelationship::DerivedFrom,
        }];
        let mut portable_experiments = vec![];
        for experiment in experiments {
            let lineage = Self::hash(&(
                experiment.starting_state.tree_hash.clone(),
                experiment.conclusion,
                experiment
                    .trials
                    .iter()
                    .map(|t| t.outcome)
                    .collect::<Vec<_>>(),
            ))?;
            let eid = Self::identity_for(
                &identity.node.id,
                "experiment",
                &experiment.id.to_string(),
                &lineage,
            )?;
            let prov = Self::provenance_id(&(&identity.node.id, "experiment", &lineage))?;
            let mut hashes = vec![];
            for trial in &experiment.trials {
                let exp = self.store.experience(&trial.experience_id)?;
                hashes.extend(exp.evidence.artifacts.iter().map(|a| a.blake3.clone()));
                let (portable, node) = self.portable_experience(&identity.node.id, &exp)?;
                pedges.push(ProvenanceEdge {
                    source: prov.clone(),
                    target: node.id.clone(),
                    relationship: if experiment.conclusion
                        == crate::experiment::ExperimentConclusion::ContradictsHypothesis
                    {
                        ProvenanceRelationship::Contradicts
                    } else {
                        ProvenanceRelationship::Supports
                    },
                });
                if !experiences
                    .iter()
                    .any(|e| e.identity.lineage_hash == portable.identity.lineage_hash)
                {
                    experiences.push(portable);
                    pnodes.push(node)
                }
            }
            pnodes.push(ProvenanceNode {
                id: prov.clone(),
                kind: ProvenanceNodeKind::Experiment,
                external_id: experiment.id.to_string(),
                node: identity.node.id.clone(),
                lineage_hash: Some(eid.lineage_hash.clone()),
                summary: format!("{:?}", experiment.conclusion),
            });
            pedges.push(ProvenanceEdge {
                source: lesson_prov.clone(),
                target: prov.clone(),
                relationship: match experiment.conclusion {
                    crate::experiment::ExperimentConclusion::ContradictsHypothesis => {
                        ProvenanceRelationship::Contradicts
                    }
                    _ => ProvenanceRelationship::Supports,
                },
            });
            portable_experiments.push(PortableExperimentSummary {
                identity: eid,
                conclusion: format!("{:?}", experiment.conclusion),
                trial_outcomes: experiment.trials.iter().map(|t| t.outcome).collect(),
                starting_state_hash: experiment.starting_state.tree_hash,
                evidence_hashes: hashes,
                provenance_ref: prov,
            });
        }
        let manifest = Self::base_manifest(
            &identity.node.id,
            ExportScope::Object,
            labels,
            experiences.len() + portable_experiments.len(),
        )?;
        self.finish_bundle(
            ExperienceBundle {
                manifest,
                experiences,
                lessons: vec![portable_lesson],
                skills: vec![],
                experiments: portable_experiments,
                reflexes: vec![],
                recoveries: vec![],
                envelopes: vec![],
                provenance: ProvenanceGraph {
                    nodes: pnodes,
                    edges: pedges,
                },
            },
            Some(&source.context.repository.path),
        )
    }
    fn export_skill(&self, name: &str, labels: Vec<String>) -> Result<SignedExperienceBundle> {
        let skill = self.store.skill(name)?;
        ConservativePublishPolicy { store: self.store }.can_publish(&ExperienceObjectRef {
            kind: "skill".into(),
            id: skill.id.to_string(),
        })?;
        let identity = self.identity()?;
        let source = self.store.experience(&skill.source_experience)?;
        let (exp, pnode) = self.portable_experience(&identity.node.id, &source)?;
        let sid = Self::identity_for(
            &identity.node.id,
            "skill",
            &skill.id.to_string(),
            &(skill.name.clone(), &skill.procedure, &skill.context),
        )?;
        let prov = Self::provenance_id(&(&identity.node.id, "skill", &sid.lineage_hash))?;
        let portable = PortableSkill {
            identity: sid.clone(),
            name: skill.name,
            description: skill.description,
            context: Self::context(&source),
            procedure: skill.procedure,
            source_status: skill.status,
            evidence_hashes: source
                .evidence
                .artifacts
                .iter()
                .map(|a| a.blake3.clone())
                .collect(),
            provenance_ref: prov.clone(),
        };
        let manifest =
            Self::base_manifest(&identity.node.id, ExportScope::SkillPackage, labels, 2)?;
        self.finish_bundle(
            ExperienceBundle {
                manifest,
                experiences: vec![exp.clone()],
                lessons: vec![],
                skills: vec![portable],
                experiments: vec![],
                reflexes: vec![],
                recoveries: vec![],
                envelopes: vec![],
                provenance: ProvenanceGraph {
                    nodes: vec![
                        pnode,
                        ProvenanceNode {
                            id: prov.clone(),
                            kind: ProvenanceNodeKind::Skill,
                            external_id: sid.origin_object_id,
                            node: identity.node.id.clone(),
                            lineage_hash: Some(sid.lineage_hash),
                            summary: "Portable Skill package".into(),
                        },
                    ],
                    edges: vec![ProvenanceEdge {
                        source: prov,
                        target: exp.provenance_ref,
                        relationship: ProvenanceRelationship::DerivedFrom,
                    }],
                },
            },
            Some(&source.context.repository.path),
        )
    }
    fn import(&self, signed: SignedExperienceBundle, local: &QueryContext) -> Result<ImportReport> {
        signed.bundle.validate(&self.config.federation.limits)?;
        validate_bundle_references(&signed.bundle)?;
        validate_safe_payload(
            &serde_json::to_value(&signed.bundle)?,
            self.config.federation.limits.max_nesting_depth,
        )?;
        let embedded = embedded_verifying_key(&signed)?;
        verify_signed_bundle(&signed, &embedded)?;
        let peer = self.store.peer_by_node(&signed.signer)?;
        if peer
            .as_ref()
            .is_some_and(|p| p.trust == ProducerTrust::Blocked)
        {
            return Err(Error::Intervention(
                "Bundle producer is locally blocked".into(),
            ));
        }
        if let Some(peer) = &peer
            && peer.public_key != signed.signer_public_key
        {
            return Err(Error::Intervention(
                "Known peer key changed; update identity manually after verification".into(),
            ));
        }
        let authenticity = if peer.is_some() {
            AuthenticityStatus::SignatureValid
        } else {
            AuthenticityStatus::UnknownKey
        };
        let producer_trust = peer
            .as_ref()
            .map(|p| p.trust)
            .unwrap_or(ProducerTrust::Unknown);
        let local_node = self.identity()?.node;
        let mut candidates = Vec::new();
        for (kind, identity, context, mut value) in portable_values(&signed.bundle)? {
            if kind == "reflex" {
                value["effective_response"] = serde_json::json!(ReflexResponse::Advise);
            }
            let compatibility = DeterministicContextCompatibility.compare(&context, local);
            let state = if compatibility.score >= self.config.federation.minimum_context_match {
                FederatedExperienceState::ReproductionRecommended
            } else if compatibility.score >= 0.50 {
                FederatedExperienceState::ContextMatched
            } else {
                FederatedExperienceState::Received
            };
            let object = FederatedObject {
                id: FederatedObjectId::new(),
                identity: identity.clone(),
                origin_bundle: signed.manifest.bundle_id.clone(),
                object_type: kind.into(),
                state,
                trust: ExperienceTrust {
                    authenticity,
                    producer_trust,
                    local_reproduction: ReproductionStatus::NotAttempted,
                    context_compatibility: compatibility,
                    contradiction_status: ContradictionStatus::None,
                },
                object: value,
                received_at: Utc::now(),
            };
            let remote = provenance_ref_for(&object.object)?;
            let local_prov = Self::provenance_id(&(local_node.id.clone(), object.id.to_string()))?;
            let graph = ProvenanceGraph {
                nodes: vec![ProvenanceNode {
                    id: local_prov.clone(),
                    kind: provenance_kind(kind)?,
                    external_id: object.id.to_string(),
                    node: local_node.id.clone(),
                    lineage_hash: Some(identity.lineage_hash.clone()),
                    summary: format!("External {kind}; advisory"),
                }],
                edges: vec![ProvenanceEdge {
                    source: local_prov,
                    target: remote,
                    relationship: ProvenanceRelationship::ImportedFrom,
                }],
            };
            candidates.push((object, graph));
        }
        let stored = self
            .store
            .import_federation_bundle(&signed, authenticity, &candidates)?;
        let imported = stored.iter().filter(|(_, new)| *new).count();
        let duplicates = stored.len() - imported;
        let object_ids: Vec<_> = stored.into_iter().map(|(id, _)| id).collect();
        Ok(ImportReport {
            bundle_id: signed.manifest.bundle_id,
            producer: signed.signer,
            authenticity,
            producer_trust,
            imported,
            duplicates,
            state: "imported as advisory evidence".into(),
            objects: object_ids.clone(),
            recommended_action: object_ids
                .first()
                .map(|id| format!("hardknock federate test {id}")),
        })
    }
    fn evaluate_external(
        &self,
        id: &FederatedObjectId,
        local: &QueryContext,
    ) -> Result<FederatedObject> {
        let mut object = self.store.federated_object(id)?;
        let context = context_for_value(&object.object)?;
        object.trust.context_compatibility =
            DeterministicContextCompatibility.compare(&context, local);
        if matches!(
            object.state,
            FederatedExperienceState::Received
                | FederatedExperienceState::ContextMatched
                | FederatedExperienceState::ReproductionRecommended
        ) {
            object.state = if object.trust.context_compatibility.score
                >= self.config.federation.minimum_context_match
            {
                FederatedExperienceState::ReproductionRecommended
            } else if object.trust.context_compatibility.score >= 0.5 {
                FederatedExperienceState::ContextMatched
            } else {
                FederatedExperienceState::Received
            };
        }
        self.store.update_federated_object(&object)?;
        Ok(object)
    }
    async fn reproduce(
        &self,
        id: &FederatedObjectId,
        state: StateRef,
        checks: Vec<String>,
        cancel: &Cancellation,
    ) -> Result<FederationReproduction> {
        let mut object = self.store.federated_object(id)?;
        if object.object_type != "lesson" {
            return Err(Error::InvalidInput(
                "V0.7 controlled reproduction currently supports external Lessons".into(),
            ));
        }
        let lesson: PortableLesson = serde_json::from_value(object.object.clone())?;
        let baseline = lesson
            .avoid
            .as_ref()
            .and_then(ActionPattern::shell_script)
            .ok_or_else(|| {
                Error::Intervention("External Lesson has no reproducible shell baseline".into())
            })?;
        let alternative = lesson
            .prefer
            .as_ref()
            .and_then(ActionPattern::shell_script)
            .ok_or_else(|| {
                Error::Intervention("External Lesson has no reproducible shell alternative".into())
            })?;
        let checks = if checks.is_empty() {
            lesson.evaluation_checks.clone()
        } else {
            checks
        };
        if checks.is_empty() {
            return Err(Error::Intervention(
                "Local reproduction needs explicit evaluator checks".into(),
            ));
        }
        let request = ExperimentRequest {
            id: Default::default(),
            session_id: format!("federation:{id}"),
            question: format!(
                "Locally reproduce external Lesson {}",
                lesson.identity.origin_object_id
            ),
            hypothesis: Some(lesson.claim),
            candidates: vec![
                ExperimentCandidate {
                    id: CandidateId::new(),
                    name: "baseline".into(),
                    description: "Remote baseline under local controlled context".into(),
                    execution: CandidateExecution::Shell {
                        commands: vec![baseline.into()],
                    },
                    expected_outcome: Some("failure".into()),
                },
                ExperimentCandidate {
                    id: CandidateId::new(),
                    name: "alternative".into(),
                    description: "Remote alternative under local controlled context".into(),
                    execution: CandidateExecution::Shell {
                        commands: vec![alternative.into()],
                    },
                    expected_outcome: Some("success".into()),
                },
            ],
            starting_state: ExperimentStartingState {
                state_ref: state,
                expected_fingerprint: None,
                parent_reality: None,
                source: SnapshotSource::RepositoryCommit,
            },
            evaluator: crate::evaluation::EvaluationSpec { checks },
            budget: ExperienceBudget {
                max_realities: 2,
                max_agent_runs: 0,
                max_duration_ms: Some(120_000),
                max_commands_per_reality: Some(20),
                max_curriculum_trials: None,
                max_parallel_trials: Some(1),
            },
            requested_by: AgentIdentity {
                kind: "hardknock-federation".into(),
                executable: "hardknock".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
                model: None,
            },
            created_at: Utc::now(),
            criteria: ComparisonCriteria::default(),
            origin: ExperimentOrigin::FederationReproduction,
            intent: ExperimentIntent::ReproduceFederatedExperience,
            capabilities: ExperimentCapabilities::default(),
        };
        let experiment = ExperimentOrchestrator {
            store: self.store,
            config: self.config,
        }
        .run(request, cancel)
        .await?;
        let result = experiment.result.as_ref().ok_or_else(|| {
            Error::Intervention("Reproduction experiment has no terminal result".into())
        })?;
        let baseline = result.candidates.iter().find(|c| c.name == "baseline");
        let alternative = result.candidates.iter().find(|c| c.name == "alternative");
        let reproduction_result = match (
            baseline.map(|c| Outcome::from_evaluation(&c.evaluation)),
            alternative.map(|c| Outcome::from_evaluation(&c.evaluation)),
        ) {
            (Some(Outcome::Failure), Some(Outcome::Success)) => ReproductionResult::Supports,
            (Some(Outcome::Success), Some(Outcome::Failure)) => ReproductionResult::Contradicts,
            (Some(_), Some(_)) => ReproductionResult::Inconclusive,
            _ => ReproductionResult::CannotReproduce,
        };
        let experience_ids = result
            .candidates
            .iter()
            .map(|c| c.experience_id.to_string())
            .collect::<Vec<_>>();
        let reproduction = FederationReproduction {
            id: FederationReproductionId::new(),
            object_id: id.clone(),
            experiment_id: Some(experiment.id.to_string()),
            result: reproduction_result,
            experience_ids: experience_ids.clone(),
            explanation: format!(
                "baseline {:?}; alternative {:?}",
                baseline.map(|c| Outcome::from_evaluation(&c.evaluation)),
                alternative.map(|c| Outcome::from_evaluation(&c.evaluation))
            ),
            created_at: Utc::now(),
        };
        self.store.save_reproduction(&reproduction)?;
        object.trust.local_reproduction = match reproduction_result {
            ReproductionResult::Supports => ReproductionStatus::Supports,
            ReproductionResult::Contradicts => ReproductionStatus::Contradicts,
            ReproductionResult::Inconclusive => ReproductionStatus::Inconclusive,
            ReproductionResult::CannotReproduce => ReproductionStatus::CannotReproduce,
        };
        object.state = match reproduction_result {
            ReproductionResult::Supports => FederatedExperienceState::LocallySupported,
            ReproductionResult::Contradicts => FederatedExperienceState::LocallyContradicted,
            _ => object.state,
        };
        if reproduction_result == ReproductionResult::Contradicts {
            object.trust.contradiction_status = ContradictionStatus::LocalConflict;
            let conflict = FederatedConflict {
                id: FederatedConflictId::new(),
                external_object: id.clone(),
                local: ExperienceObjectRef {
                    kind: "experiment".into(),
                    id: experiment.id.to_string(),
                },
                remote: ExperienceObjectRef {
                    kind: "lesson".into(),
                    id: lesson.identity.origin_object_id.clone(),
                },
                conflict_type: FederatedConflictType::ActionConflict,
                status: ConflictStatus::SupportedLocal,
                local_evidence: experience_ids.clone(),
                remote_evidence: lesson.evidence_summary.evidence_hashes.clone(),
                created_at: Utc::now(),
            };
            self.store.save_federated_conflict(&conflict, id)?;
        }
        self.store.update_federated_object(&object)?;
        let local_node = self.identity()?.node;
        let exp_prov = Self::provenance_id(&(local_node.id.clone(), experiment.id.to_string()))?;
        let remote = lesson.provenance_ref;
        let mut nodes = vec![ProvenanceNode {
            id: exp_prov.clone(),
            kind: ProvenanceNodeKind::Experiment,
            external_id: experiment.id.to_string(),
            node: local_node.id.clone(),
            lineage_hash: Some(Self::hash(&experience_ids)?),
            summary: format!("Local reproduction: {:?}", reproduction_result),
        }];
        let mut edges = vec![ProvenanceEdge {
            source: exp_prov.clone(),
            target: remote,
            relationship: ProvenanceRelationship::ReproducedBy,
        }];
        for experience_id in &experience_ids {
            let prov = Self::provenance_id(&(local_node.id.clone(), experience_id))?;
            nodes.push(ProvenanceNode {
                id: prov.clone(),
                kind: ProvenanceNodeKind::Experience,
                external_id: experience_id.clone(),
                node: local_node.id.clone(),
                lineage_hash: None,
                summary: "Local reproduction trial Experience".into(),
            });
            edges.push(ProvenanceEdge {
                source: exp_prov.clone(),
                target: prov,
                relationship: if reproduction_result == ReproductionResult::Contradicts {
                    ProvenanceRelationship::Contradicts
                } else {
                    ProvenanceRelationship::Supports
                },
            });
        }
        self.store
            .save_provenance_graph(&ProvenanceGraph { nodes, edges })?;
        self.store.audit(
            "local_reproduction",
            Some(&id.to_string()),
            &format!("Result: {:?}", reproduction_result),
        )?;
        Ok(reproduction)
    }
    fn promote(
        &self,
        id: &FederatedObjectId,
        application: &crate::core::ExperienceId,
    ) -> Result<FederatedObject> {
        let mut object = self.store.federated_object(id)?;
        if object.state != FederatedExperienceState::LocallySupported {
            return Err(Error::Intervention("Only locally supported external evidence can be promoted after a separate application".into()));
        }
        let experience = self.store.experience(application)?;
        if experience.outcome != Outcome::Success {
            return Err(Error::Intervention(
                "Local validation requires a successful later Experience".into(),
            ));
        }
        let query = QueryContext::new(&experience.context, &experience.goal, vec![]);
        let compatibility =
            DeterministicContextCompatibility.compare(&context_for_value(&object.object)?, &query);
        if compatibility.score < self.config.federation.minimum_context_match {
            return Err(Error::Intervention(
                "Successful Experience is not context-compatible with the external evidence".into(),
            ));
        }
        if self
            .store
            .reproductions()?
            .iter()
            .any(|r| r.object_id == *id && r.experience_ids.contains(&application.to_string()))
        {
            return Err(Error::Intervention(
                "Local validation must use a later application, not a reproduction trial".into(),
            ));
        }
        object.state = FederatedExperienceState::LocallyValidated;
        object.trust.local_reproduction = ReproductionStatus::Supports;
        object.trust.context_compatibility = compatibility;
        self.store.update_federated_object(&object)?;
        self.store.audit(
            "external_promoted",
            Some(&id.to_string()),
            &format!("Locally validated by later Experience {application}"),
        )?;
        Ok(object)
    }
}

fn context_for_value(value: &serde_json::Value) -> Result<EvidenceContext> {
    serde_json::from_value(
        value
            .get("context")
            .or_else(|| value.get("trigger_context"))
            .cloned()
            .unwrap_or_else(|| serde_json::json!({})),
    )
    .map_err(Into::into)
}
fn provenance_ref_for(value: &serde_json::Value) -> Result<ProvenanceNodeId> {
    serde_json::from_value(
        value.get("provenance_ref").cloned().ok_or_else(|| {
            Error::InvalidInput("Portable object lacks provenance reference".into())
        })?,
    )
    .map_err(Into::into)
}
fn provenance_kind(kind: &str) -> Result<ProvenanceNodeKind> {
    match kind {
        "experience" => Ok(ProvenanceNodeKind::Experience),
        "experiment" => Ok(ProvenanceNodeKind::Experiment),
        "lesson" => Ok(ProvenanceNodeKind::Lesson),
        "skill" => Ok(ProvenanceNodeKind::Skill),
        "reflex" => Ok(ProvenanceNodeKind::Reflex),
        "recovery" => Ok(ProvenanceNodeKind::Recovery),
        "envelope" => Ok(ProvenanceNodeKind::Skill),
        _ => Err(Error::InvalidInput("Unknown portable object kind".into())),
    }
}
fn portable_values(
    bundle: &ExperienceBundle,
) -> Result<
    Vec<(
        &'static str,
        FederatedObjectIdentity,
        EvidenceContext,
        serde_json::Value,
    )>,
> {
    let mut out = vec![];
    for v in &bundle.experiences {
        out.push((
            "experience",
            v.identity.clone(),
            v.context.clone(),
            serde_json::to_value(v)?,
        ))
    }
    for v in &bundle.lessons {
        out.push((
            "lesson",
            v.identity.clone(),
            v.context.clone(),
            serde_json::to_value(v)?,
        ))
    }
    for v in &bundle.skills {
        out.push((
            "skill",
            v.identity.clone(),
            v.context.clone(),
            serde_json::to_value(v)?,
        ))
    }
    for v in &bundle.experiments {
        out.push((
            "experiment",
            v.identity.clone(),
            EvidenceContext::default(),
            serde_json::to_value(v)?,
        ))
    }
    for v in &bundle.reflexes {
        out.push((
            "reflex",
            v.identity.clone(),
            v.trigger_context.clone(),
            serde_json::to_value(v)?,
        ))
    }
    for v in &bundle.recoveries {
        out.push((
            "recovery",
            v.identity.clone(),
            v.context.clone(),
            serde_json::to_value(v)?,
        ))
    }
    for v in &bundle.envelopes {
        out.push((
            "envelope",
            v.identity.clone(),
            v.context.clone(),
            serde_json::to_value(v)?,
        ))
    }
    Ok(out)
}
fn validate_bundle_references(bundle: &ExperienceBundle) -> Result<()> {
    let mut ids = BTreeSet::new();
    for node in &bundle.provenance.nodes {
        if !ids.insert(node.id.to_string()) {
            return Err(Error::InvalidInput("Duplicate provenance node ID".into()));
        }
    }
    for edge in &bundle.provenance.edges {
        if !ids.contains(&edge.source.to_string()) || !ids.contains(&edge.target.to_string()) {
            return Err(Error::InvalidInput(
                "Invalid provenance edge reference".into(),
            ));
        }
    }
    for (_, identity, _, value) in portable_values(bundle)? {
        if identity.origin_object_id.is_empty()
            || identity.origin_object_id.len() > 512
            || identity.lineage_hash.len() != 64
            || !ids.contains(&provenance_ref_for(&value)?.to_string())
        {
            return Err(Error::InvalidInput(
                "Invalid portable object identity or provenance reference".into(),
            ));
        }
    }
    let unique: BTreeSet<_> = bundle
        .experiences
        .iter()
        .map(|e| e.identity.lineage_hash.as_str())
        .collect();
    if bundle.manifest.evidence_count < unique.len()
        || bundle.manifest.evidence_count > bundle.object_count()
    {
        return Err(Error::InvalidInput(
            "Bundle evidence count does not match portable contents".into(),
        ));
    }
    Ok(())
}

pub fn requested_and_effective_reflex(value: &PortableReflex) -> (ReflexResponse, ReflexResponse) {
    (value.requested_response, ReflexResponse::Advise)
}
pub fn recovery_step_summary(step: &RecoveryStep) -> String {
    match step {
        RecoveryStep::ShellCommand { .. } => {
            "suggested shell recovery (not executable from federation)".into()
        }
        RecoveryStep::SetEnvironmentVariable { key, .. } => {
            format!("suggested environment change: {key}=[REDACTED]")
        }
        RecoveryStep::Replan => "suggest replan".into(),
    }
}
