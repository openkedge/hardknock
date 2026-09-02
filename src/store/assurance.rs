// SPDX-License-Identifier: Apache-2.0

use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use rusqlite::{OptionalExtension, params};

use super::{EpistemicStore, Store};
use crate::{
    Error, Result,
    assurance::*,
    capability::{CapabilityManifest, ExecutionCapability, NetworkEndpointPattern, NetworkMode},
    core::*,
    curriculum::{Curriculum, CurriculumGoalKind, GoalStatus, Severity},
    epistemic::{DeterministicEvidenceDiversityPolicy, EvidenceDiversityPolicy},
    evaluation::CheckStatus,
    experience::{Experience, Outcome},
    lesson::{EvidenceRef, Lesson, LessonStatus},
    resilience::{ChaosTrialOutcome, RecoveryStatus},
    store::{CapabilityStore, EffectStore, ExperimentStore, ToolStore},
};

pub trait AssuranceStore {
    fn register_behavioral_contract(
        &self,
        contract: BehavioralContract,
        reason: Option<String>,
    ) -> Result<BehavioralContractRevision>;
    fn behavioral_contracts(&self) -> Result<Vec<BehavioralContractRevision>>;
    fn behavioral_contract(&self, selector: &str) -> Result<BehavioralContractRevision>;
    fn behavioral_contract_revision(
        &self,
        id: &BehavioralContractId,
        revision: u64,
    ) -> Result<BehavioralContractRevision>;
    fn behavioral_contract_history(
        &self,
        id: &BehavioralContractId,
    ) -> Result<Vec<BehavioralContractRevision>>;
    fn bind_skill_contract(&self, skill: &SkillId, contract: &BehavioralContractRef) -> Result<()>;
    fn skill_contract_binding(&self, skill: &SkillId) -> Result<Option<BehavioralContractRef>>;
    fn insert_evidence_manifest(&self, manifest: &EvidenceManifest) -> Result<()>;
    fn evidence_manifest(&self, id: &EvidenceManifestId) -> Result<EvidenceManifest>;
    fn collect_certification_evidence(
        &self,
        skill: &SkillRevisionRef,
        contract: &BehavioralContractRevision,
        profile: &AssuranceProfile,
    ) -> Result<EvidenceManifest>;
    fn insert_skill_certification(&self, certification: &SkillCertification) -> Result<()>;
    fn skill_certification(&self, id: &SkillCertificationId) -> Result<SkillCertification>;
    fn skill_certifications(&self, skill: &SkillId) -> Result<Vec<SkillCertification>>;
    fn revoke_skill_certification(
        &self,
        id: &SkillCertificationId,
        reason: &str,
    ) -> Result<CertificationRevocation>;
    fn certification_revocation(
        &self,
        id: &SkillCertificationId,
    ) -> Result<Option<CertificationRevocation>>;
}

impl AssuranceStore for Store {
    fn register_behavioral_contract(
        &self,
        mut contract: BehavioralContract,
        reason: Option<String>,
    ) -> Result<BehavioralContractRevision> {
        contract.validate()?;
        let subject = subject_parts(&contract.subject);
        let existing: Option<(String, i64)> = self
            .connection
            .query_row(
                "SELECT contract_id,revision FROM behavioral_contract_revisions WHERE name=?1 AND subject_kind=?2 AND subject_id=?3 ORDER BY revision DESC LIMIT 1",
                params![contract.name, subject.0, subject.1],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (contract_id, revision, parent_revision) = if let Some((id, revision)) = existing {
            let id: BehavioralContractId = id.parse()?;
            contract.id = id.clone();
            let previous = u64::try_from(revision)
                .map_err(|_| Error::InvalidInput("Invalid contract revision".into()))?;
            (id, previous + 1, Some(previous))
        } else {
            (contract.id.clone(), 1, None)
        };
        contract.created_at = Utc::now();
        let revision_record = BehavioralContractRevision {
            contract_id: contract_id.clone(),
            revision,
            contract,
            parent_revision,
            reason,
            created_at: Utc::now(),
        };
        self.connection.execute(
            "INSERT INTO behavioral_contract_revisions(contract_id,revision,name,subject_kind,subject_id,created_at,data) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                contract_id.to_string(),
                sql_u64(revision)?,
                revision_record.contract.name,
                subject.0,
                subject.1,
                revision_record.created_at.to_rfc3339(),
                serde_json::to_string(&revision_record)?
            ],
        )?;
        Ok(revision_record)
    }

    fn behavioral_contracts(&self) -> Result<Vec<BehavioralContractRevision>> {
        self.list("SELECT r.data FROM behavioral_contract_revisions r WHERE revision=(SELECT max(revision) FROM behavioral_contract_revisions x WHERE x.contract_id=r.contract_id) ORDER BY name,contract_id")
    }

    fn behavioral_contract(&self, selector: &str) -> Result<BehavioralContractRevision> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM behavioral_contract_revisions WHERE contract_id=?1 OR name=?1 ORDER BY revision DESC LIMIT 1",
                [selector],
                |row| row.get(0),
            )
            .optional()?;
        Ok(serde_json::from_str(&data.ok_or_else(|| {
            Error::NotFound(format!("Behavioral Contract {selector} not found"))
        })?)?)
    }

    fn behavioral_contract_revision(
        &self,
        id: &BehavioralContractId,
        revision: u64,
    ) -> Result<BehavioralContractRevision> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM behavioral_contract_revisions WHERE contract_id=?1 AND revision=?2",
                params![id.to_string(), sql_u64(revision)?],
                |row| row.get(0),
            )
            .optional()?;
        Ok(serde_json::from_str(&data.ok_or_else(|| {
            Error::NotFound(format!("Behavioral Contract {id}@{revision} not found"))
        })?)?)
    }

    fn behavioral_contract_history(
        &self,
        id: &BehavioralContractId,
    ) -> Result<Vec<BehavioralContractRevision>> {
        let mut statement = self.connection.prepare(
            "SELECT data FROM behavioral_contract_revisions WHERE contract_id=?1 ORDER BY revision",
        )?;
        statement
            .query_map([id.to_string()], |row| row.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }

    fn bind_skill_contract(&self, skill: &SkillId, contract: &BehavioralContractRef) -> Result<()> {
        self.behavioral_contract_revision(&contract.contract_id, contract.revision)?;
        self.skill(&skill.to_string())?;
        let binding_revision: i64 = self.connection.query_row(
            "SELECT coalesce(max(binding_revision),0)+1 FROM skill_contract_bindings WHERE skill_id=?1",
            [skill.to_string()],
            |row| row.get(0),
        )?;
        let data = serde_json::json!({
            "skill_id": skill,
            "binding_revision": binding_revision,
            "contract": contract,
            "created_at": Utc::now(),
        });
        self.connection.execute(
            "INSERT INTO skill_contract_bindings(skill_id,binding_revision,contract_id,contract_revision,created_at,data) VALUES(?1,?2,?3,?4,?5,?6)",
            params![skill.to_string(), binding_revision, contract.contract_id.to_string(), sql_u64(contract.revision)?, Utc::now().to_rfc3339(), serde_json::to_string(&data)?],
        )?;
        Ok(())
    }

    fn skill_contract_binding(&self, skill: &SkillId) -> Result<Option<BehavioralContractRef>> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM skill_contract_bindings WHERE skill_id=?1 ORDER BY binding_revision DESC LIMIT 1",
                [skill.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        data.map(|data| {
            let value: serde_json::Value = serde_json::from_str(&data)?;
            Ok(serde_json::from_value(value["contract"].clone())?)
        })
        .transpose()
    }

    fn insert_evidence_manifest(&self, manifest: &EvidenceManifest) -> Result<()> {
        manifest.verify_hash()?;
        validate_manifest_references(self, manifest)?;
        let (subject_kind, subject_id, subject_revision) = match &manifest.subject {
            EvidenceSubject::Skill(skill) => (
                "skill",
                skill.skill_id.to_string(),
                Some(sql_u64(skill.revision)?),
            ),
            EvidenceSubject::Tool(tool) => ("tool", tool.to_string(), None),
        };
        self.connection.execute(
            "INSERT INTO evidence_manifests(id,subject_kind,subject_id,subject_revision,evidence_hash,generated_at,data) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![manifest.id.to_string(), subject_kind, subject_id, subject_revision, manifest.evidence_hash, manifest.generated_at.to_rfc3339(), serde_json::to_string(manifest)?],
        )?;
        Ok(())
    }

    fn evidence_manifest(&self, id: &EvidenceManifestId) -> Result<EvidenceManifest> {
        let manifest: EvidenceManifest = self.get(
            "SELECT data FROM evidence_manifests WHERE id=?1",
            &id.to_string(),
        )?;
        manifest.verify_hash()?;
        Ok(manifest)
    }

    fn collect_certification_evidence(
        &self,
        skill_ref: &SkillRevisionRef,
        contract: &BehavioralContractRevision,
        _profile: &AssuranceProfile,
    ) -> Result<EvidenceManifest> {
        if contract.contract.subject != ContractSubject::Skill(skill_ref.skill_id.clone()) {
            return Err(Error::InvalidInput(
                "Behavioral Contract subject does not match Skill".into(),
            ));
        }
        let skill = self.skill(&skill_ref.skill_id.to_string())?;
        let revision = self
            .skill_revisions(&skill_ref.skill_id)?
            .into_iter()
            .find(|revision| revision.revision == skill_ref.revision)
            .ok_or_else(|| Error::NotFound("Skill revision not found".into()))?;
        let mut experience_ids = BTreeSet::from([revision.source_experience.clone()]);
        for evidence in &revision.evidence {
            if let EvidenceRef::Experience { experience_id, .. } = evidence {
                experience_ids.insert(experience_id.clone());
            }
        }
        let campaigns = self
            .campaigns()?
            .into_iter()
            .filter(|campaign| {
                matches!(&campaign.plan.target, crate::resilience::ChaosTarget::Skill(id) if id == &skill_ref.skill_id)
            })
            .collect::<Vec<_>>();
        for campaign in &campaigns {
            if let Some(control) = &campaign.control {
                experience_ids.insert(control.experience_id.clone());
            }
            experience_ids.extend(
                campaign
                    .trials
                    .iter()
                    .map(|trial| trial.experience_id.clone()),
            );
        }
        let experiences = experience_ids
            .iter()
            .map(|id| self.experience(id))
            .collect::<Result<Vec<_>>>()?;
        let experience_set = experiences
            .iter()
            .map(|experience| experience.id.clone())
            .collect::<BTreeSet<_>>();

        let experiments = self
            .experiments()?
            .into_iter()
            .filter(|experiment| {
                experiment
                    .trials
                    .iter()
                    .any(|trial| experience_set.contains(&trial.experience_id))
            })
            .collect::<Vec<_>>();
        let strategy_experiments = ExperimentStore::list(self, None)?
            .into_iter()
            .filter(|experiment| {
                experiment.result.as_ref().is_some_and(|result| {
                    result
                        .created_experience
                        .iter()
                        .any(|id| experience_set.contains(id))
                })
            })
            .collect::<Vec<_>>();
        let lessons: Vec<Lesson> = self
            .list("SELECT data FROM lessons ORDER BY created_at,id")?
            .into_iter()
            .filter(|lesson: &Lesson| experience_set.contains(&lesson.source_experience))
            .collect();
        let campaign_trial_ids = campaigns
            .iter()
            .flat_map(|campaign| campaign.trials.iter())
            .map(|trial| trial.id.clone())
            .collect::<BTreeSet<_>>();
        let reflexes = self
            .reflexes()?
            .into_iter()
            .filter(|reflex| campaign_trial_ids.contains(&reflex.source_trial))
            .collect::<Vec<_>>();
        let recoveries = self
            .recoveries()?
            .into_iter()
            .filter(|recovery| campaign_trial_ids.contains(&recovery.source_trial))
            .collect::<Vec<_>>();

        let mut summary = AssuranceEvidenceSummary {
            controlled_experiments: experiments.len()
                + strategy_experiments
                    .iter()
                    .filter(|experiment| {
                        experiment.result.as_ref().is_some_and(|result| {
                            result.quality == crate::experimentation::ExperimentQuality::Controlled
                        })
                    })
                    .count(),
            attestations_intact: true,
            declared_tool_manifests_only: true,
            ..Default::default()
        };
        summary.oldest_required_evidence = experiences.iter().map(|value| value.created_at).min();
        summary.newest_evidence = experiences.iter().map(|value| value.created_at).max();
        summary.known_unknowns = skill
            .coverage
            .dimensions
            .iter()
            .flat_map(|dimension| dimension.unknown.clone())
            .collect();
        summary.declared_tool_manifests_only = experiences
            .iter()
            .all(|experience| !experience.evidence.attestations.is_empty());
        if let (Some(name), Some(coverage)) = (
            skill.coverage.profile.clone(),
            skill.coverage.profile_coverage,
        ) {
            summary.perturbation_profile_coverage.insert(name, coverage);
        }
        if !campaigns.is_empty() {
            let configured = campaigns
                .iter()
                .map(|campaign| campaign.plan.perturbations.len())
                .sum::<usize>();
            let tested = campaigns
                .iter()
                .flat_map(|campaign| &campaign.trials)
                .filter(|trial| trial.outcome != ChaosTrialOutcome::Inconclusive)
                .count();
            if configured > 0 {
                summary.perturbation_profile_coverage.insert(
                    "resilience-basic-v1".into(),
                    (tested as f64 / configured as f64).min(1.0),
                );
            }
        }
        summary.high_severity_recovery_classes = recoveries
            .iter()
            .filter(|recovery| recovery.status == RecoveryStatus::Validated)
            .map(|recovery| recovery.failure_signature.signature.clone())
            .collect();
        let tests = self
            .resilience_tests()?
            .into_iter()
            .filter(|test| campaign_trial_ids.contains(&test.source_trial))
            .collect::<Vec<_>>();
        summary.reflex_checks = tests
            .iter()
            .filter(|test| test.false_positive.is_some())
            .count();
        summary.reflex_false_positives = tests
            .iter()
            .filter(|test| test.false_positive == Some(true))
            .count();
        summary.contradictions = lessons
            .iter()
            .filter(|lesson| lesson.status == LessonStatus::Contradicted)
            .map(|lesson| EvidenceContradiction {
                description: lesson.claim.clone(),
                severity: Severity::Critical,
                evidence_ids: vec![lesson.id.to_string()],
                resolved: false,
            })
            .collect();

        let mut capability_manifest_ids = BTreeSet::new();
        let mut attestation_ids = BTreeSet::new();
        let mut effect_receipt_ids = BTreeSet::new();
        let contract_evaluator = DeterministicContractEvaluator;
        for experience in &experiences {
            attestation_ids.extend(experience.evidence.attestations.iter().cloned());
            let reality = self.reality(&experience.reality_id)?;
            let capability_manifest = reality
                .execution_boundary
                .manifest_id
                .as_ref()
                .map(|id| self.capability_manifest(id))
                .transpose()?;
            if let Some(manifest) = &capability_manifest {
                capability_manifest_ids.insert(manifest.id.clone());
                summary.capability_observed = true;
                summary
                    .observed_capabilities
                    .extend(manifest_capabilities(manifest));
                summary.ambient_credentials_observed |= !manifest.credentials.is_empty();
                summary.effect_commit_granted |= manifest.effects.commit;
            }
            let mut execution_evidence =
                execution_evidence(experience, capability_manifest.as_ref());
            for effect in self.effects(Some(&experience.reality_id))? {
                if let Some(receipt) = self.commit_receipt_for_effect(&effect.id)? {
                    effect_receipt_ids.insert(receipt.id);
                    execution_evidence.effects.observable = true;
                    execution_evidence.effects.committed_effect = true;
                    if experience.experiment.is_some()
                        || experience.relations.iter().any(|relation| {
                            matches!(
                                relation,
                                crate::application::ExperienceRelation::CounterfactualOf(_)
                            )
                        })
                    {
                        execution_evidence.effects.experimental_effect_leak = true;
                        summary.experimental_effect_leak = true;
                    }
                }
            }
            for attestation_id in &experience.evidence.attestations {
                let attestation = self.execution_attestation(attestation_id)?;
                summary.attestation_assurance.push(attestation.assurance);
                if let Some(hash) = &attestation.tool_artifact_hash {
                    summary.tool_artifact_hashes.insert(hash.clone());
                }
                if let Some(digest) = &attestation.runtime.image_or_runtime_digest {
                    summary.runtime_digests.insert(digest.clone());
                }
                let definition = self.tool_definition(&attestation.tool.id).ok();
                let reality_hash = self
                    .effective_capability_manifest(&attestation.reality_id)
                    .ok()
                    .and_then(|manifest| manifest.hash().ok());
                summary.attestations_intact &= attestation
                    .verify(definition.as_ref(), reality_hash.as_deref())?
                    .valid;
                for effect_id in &attestation.effect_refs {
                    if let Some(receipt) = self.commit_receipt_for_effect(effect_id)? {
                        effect_receipt_ids.insert(receipt.id);
                        execution_evidence.effects.committed_effect = true;
                        execution_evidence.effects.observable = true;
                        if experience.experiment.is_some() {
                            execution_evidence.effects.experimental_effect_leak = true;
                            summary.experimental_effect_leak = true;
                        }
                    }
                }
            }
            summary
                .contract_evaluations
                .push(contract_evaluator.evaluate(&contract.contract, &execution_evidence)?);
        }
        let curricula: Vec<Curriculum> =
            self.list("SELECT data FROM curricula ORDER BY created_at,id")?;
        summary.capability_minimization_validated = curricula.iter().any(|curriculum| {
            curriculum.goals.iter().any(|goal| {
                goal.kind == CurriculumGoalKind::MinimizeCapability
                    && goal.status == GoalStatus::Completed
            })
        });
        let minimal = summary.capability_observed
            && !summary.ambient_credentials_observed
            && !summary.effect_commit_granted
            && summary.declared_tool_manifests_only
            && summary
                .attestation_assurance
                .iter()
                .all(|assurance| *assurance != crate::tool::AttestationAssurance::Observed);
        if minimal {
            summary
                .capability_profiles_satisfied
                .insert("capability-minimal-v1".into());
        }

        let relevant_ids = experiences
            .iter()
            .map(|value| value.id.to_string())
            .chain(experiments.iter().map(|value| value.id.to_string()))
            .chain(
                strategy_experiments
                    .iter()
                    .map(|value| value.id.to_string()),
            )
            .chain(lessons.iter().map(|value| value.id.to_string()))
            .chain(std::iter::once(skill_ref.skill_id.to_string()))
            .collect::<BTreeSet<_>>();
        let mut epistemic_paths = Vec::new();
        for claim in self.claims()? {
            epistemic_paths.extend(self.evidence_paths(&claim.id)?.into_iter().filter(|path| {
                path.evidence_refs
                    .iter()
                    .any(|reference| relevant_ids.contains(&reference.id))
            }));
        }
        if !epistemic_paths.is_empty() {
            let diversity = DeterministicEvidenceDiversityPolicy.assess(&epistemic_paths);
            summary.evidence_diversity = Some(diversity.diversity_class);
            summary.evidence_source_types = diversity.source_type_count;
            summary.evaluator_kinds = epistemic_paths
                .iter()
                .flat_map(|path| &path.dependencies.evaluator_identities)
                .map(|evaluator| evaluator.kind)
                .collect::<BTreeSet<_>>()
                .len();
            summary.root_evidence_origins = epistemic_paths
                .iter()
                .flat_map(|path| &path.context.root_evidence_origins)
                .collect::<BTreeSet<_>>()
                .len();
            summary.epistemic_dependency_caveats = diversity.caveats;
        }

        let mut manifest = EvidenceManifest {
            id: EvidenceManifestId::new(),
            subject: EvidenceSubject::Skill(skill_ref.clone()),
            generated_at: Utc::now(),
            experiences: experience_set.into_iter().collect(),
            experiments: experiments
                .iter()
                .map(|value| value.id.clone())
                .chain(strategy_experiments.iter().map(|value| value.id.clone()))
                .collect(),
            chaos_campaigns: campaigns.iter().map(|value| value.id.clone()).collect(),
            attestations: attestation_ids.into_iter().collect(),
            lessons: lessons.iter().map(|value| value.id.clone()).collect(),
            reflexes: reflexes.iter().map(|value| value.id.clone()).collect(),
            recoveries: recoveries.iter().map(|value| value.id.clone()).collect(),
            envelopes: skill.operating_envelope.into_iter().collect(),
            capability_manifests: capability_manifest_ids.into_iter().collect(),
            effect_receipts: effect_receipt_ids.into_iter().collect(),
            policy_versions: PolicyVersions::default(),
            summary,
            evidence_hash: String::new(),
        };
        manifest.seal()?;
        Ok(manifest)
    }

    fn insert_skill_certification(&self, certification: &SkillCertification) -> Result<()> {
        if certification.status != CertificationStatus::Certified {
            return Err(Error::InvalidInput(
                "Only explicitly issued Certified records may be persisted".into(),
            ));
        }
        let contract = self.behavioral_contract_revision(
            &certification.contract.contract_id,
            certification.contract.revision,
        )?;
        if contract.contract.id != certification.contract.contract_id {
            return Err(Error::InvalidInput("Contract reference mismatch".into()));
        }
        let manifest = self.evidence_manifest(&certification.evidence_manifest)?;
        if manifest.subject != EvidenceSubject::Skill(certification.skill.clone())
            || manifest.policy_versions != certification.policy_versions
        {
            return Err(Error::InvalidInput(
                "Certificate subject or policy versions do not match its Evidence Manifest".into(),
            ));
        }
        let profile = builtin_profiles()
            .into_iter()
            .find(|profile| {
                profile.id == certification.profile.id
                    && profile.version == certification.profile.version
            })
            .ok_or_else(|| Error::InvalidInput("Unknown Assurance Profile revision".into()))?;
        let evaluation = DeterministicCertificationEvaluator {
            now: certification.issued_at,
        }
        .evaluate(
            &certification.skill,
            &contract.contract,
            &profile,
            &manifest,
        )?;
        if evaluation.recommendation != CertificationRecommendation::Eligible {
            return Err(Error::Intervention(
                "Certificate evidence is no longer eligible".into(),
            ));
        }
        self.connection.execute(
            "INSERT INTO skill_certifications(id,skill_id,skill_revision,contract_id,contract_revision,profile_id,profile_version,evidence_manifest_id,issued_at,expires_at,status,data) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'certified',?11)",
            params![certification.id.to_string(), certification.skill.skill_id.to_string(), sql_u64(certification.skill.revision)?, certification.contract.contract_id.to_string(), sql_u64(certification.contract.revision)?, certification.profile.id.to_string(), certification.profile.version, certification.evidence_manifest.to_string(), certification.issued_at.to_rfc3339(), certification.expires_at.map(|value| value.to_rfc3339()), serde_json::to_string(certification)?],
        )?;
        Ok(())
    }

    fn skill_certification(&self, id: &SkillCertificationId) -> Result<SkillCertification> {
        self.get(
            "SELECT data FROM skill_certifications WHERE id=?1",
            &id.to_string(),
        )
    }

    fn skill_certifications(&self, skill: &SkillId) -> Result<Vec<SkillCertification>> {
        let mut statement = self.connection.prepare(
            "SELECT data FROM skill_certifications WHERE skill_id=?1 ORDER BY issued_at,id",
        )?;
        statement
            .query_map([skill.to_string()], |row| row.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }

    fn revoke_skill_certification(
        &self,
        id: &SkillCertificationId,
        reason: &str,
    ) -> Result<CertificationRevocation> {
        self.skill_certification(id)?;
        if reason.trim().is_empty() || reason.len() > 2048 || reason.contains('\0') {
            return Err(Error::InvalidInput(
                "Revocation reason must be bounded and nonempty".into(),
            ));
        }
        let revocation = CertificationRevocation {
            certification_id: id.clone(),
            reason: reason.into(),
            revoked_at: Utc::now(),
        };
        self.connection.execute(
            "INSERT INTO certification_revocations(certification_id,revoked_at,reason,data) VALUES(?1,?2,?3,?4)",
            params![id.to_string(), revocation.revoked_at.to_rfc3339(), reason, serde_json::to_string(&revocation)?],
        )?;
        Ok(revocation)
    }

    fn certification_revocation(
        &self,
        id: &SkillCertificationId,
    ) -> Result<Option<CertificationRevocation>> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM certification_revocations WHERE certification_id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        data.map(|data| Ok(serde_json::from_str(&data)?))
            .transpose()
    }
}

fn subject_parts(subject: &ContractSubject) -> (&'static str, String) {
    match subject {
        ContractSubject::Skill(id) => ("skill", id.to_string()),
        ContractSubject::Tool(id) => ("tool", id.to_string()),
        ContractSubject::Recovery(id) => ("recovery", id.to_string()),
        ContractSubject::EffectPlan(id) => ("effect_plan", id.to_string()),
    }
}

fn sql_u64(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| Error::InvalidInput("Revision exceeds SQLite range".into()))
}

fn execution_evidence(
    experience: &Experience,
    capability_manifest: Option<&CapabilityManifest>,
) -> ExecutionEvidence {
    let mut evaluator_results = BTreeMap::new();
    evaluator_results.insert(
        ExecutionEvidence::evaluator_key("hardknock.outcome", "success"),
        if experience.outcome == Outcome::Success {
            ContractEvaluationStatus::Satisfied
        } else {
            ContractEvaluationStatus::Violated
        },
    );
    for check in &experience.evaluation.checks {
        let status = match check.status {
            CheckStatus::Passed => ContractEvaluationStatus::Satisfied,
            CheckStatus::Failed => ContractEvaluationStatus::Violated,
            CheckStatus::Interrupted | CheckStatus::TimedOut | CheckStatus::NotRun => {
                ContractEvaluationStatus::Inconclusive
            }
        };
        evaluator_results.insert(
            ExecutionEvidence::evaluator_key("shell", &check.command),
            status,
        );
        evaluator_results.insert(
            ExecutionEvidence::evaluator_key("hardknock.check", &check.name),
            status,
        );
    }
    let capabilities = capability_manifest.map_or_else(CapabilityEvidence::default, |manifest| {
        CapabilityEvidence {
            observable: true,
            observed: manifest_capabilities(manifest),
            ambient_credentials: !manifest.credentials.is_empty(),
            effect_commit_granted: manifest.effects.commit,
            declared_tool_manifests_only: !experience.evidence.attestations.is_empty(),
            minimization_validated: false,
            maximum_exceeded: false,
        }
    });
    let externalized = capability_manifest.is_some_and(|manifest| !manifest.effects.commit);
    ExecutionEvidence {
        evaluator_results,
        capabilities,
        effects: EffectEvidence {
            observable: experience
                .evidence
                .execution_assurance
                .as_ref()
                .is_some_and(|assurance| assurance.external_effect_gating),
            commit_authority_externalized: externalized,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn manifest_capabilities(manifest: &CapabilityManifest) -> Vec<ExecutionCapability> {
    let mut capabilities = vec![];
    capabilities.extend(
        manifest
            .filesystem
            .readable
            .iter()
            .cloned()
            .map(ExecutionCapability::FilesystemRead),
    );
    capabilities.extend(
        manifest
            .filesystem
            .writable
            .iter()
            .cloned()
            .map(ExecutionCapability::FilesystemWrite),
    );
    capabilities.extend(
        manifest
            .process
            .allowed_executables
            .iter()
            .cloned()
            .map(ExecutionCapability::ProcessExecute),
    );
    match manifest.network.mode {
        NetworkMode::None => {}
        NetworkMode::LoopbackOnly => capabilities.push(ExecutionCapability::NetworkConnect(
            NetworkEndpointPattern {
                host: "localhost".into(),
                port: u16::MAX,
            },
        )),
        NetworkMode::AllowList => capabilities.extend(
            manifest
                .network
                .allow
                .iter()
                .cloned()
                .map(ExecutionCapability::NetworkConnect),
        ),
        NetworkMode::Unrestricted => capabilities.push(ExecutionCapability::NetworkConnect(
            NetworkEndpointPattern {
                host: "*".into(),
                port: u16::MAX,
            },
        )),
    }
    capabilities.extend(
        manifest
            .environment
            .readable
            .iter()
            .cloned()
            .map(ExecutionCapability::EnvironmentRead),
    );
    capabilities.extend(
        manifest
            .credentials
            .iter()
            .cloned()
            .map(ExecutionCapability::CredentialUse),
    );
    if manifest.effects.propose {
        capabilities.push(ExecutionCapability::EffectPropose(
            manifest.effects.scope.clone(),
        ));
    }
    if manifest.effects.prepare {
        capabilities.push(ExecutionCapability::EffectPrepare(
            manifest.effects.scope.clone(),
        ));
    }
    if manifest.effects.commit {
        capabilities.push(ExecutionCapability::EffectCommit(
            manifest.effects.scope.clone(),
        ));
    }
    capabilities
}

fn validate_manifest_references(store: &Store, manifest: &EvidenceManifest) -> Result<()> {
    match &manifest.subject {
        EvidenceSubject::Skill(skill) => require_composite(
            store,
            "skill_revisions",
            "skill_id",
            &skill.skill_id.to_string(),
            "revision",
            sql_u64(skill.revision)?,
        )?,
        EvidenceSubject::Tool(tool) => require_id(store, "tool_definitions", &tool.to_string())?,
    }
    for id in &manifest.experiences {
        require_id(store, "experiences", &id.to_string())?;
    }
    for id in &manifest.experiments {
        let found = exists_id(store, "experiments", &id.to_string())?
            || exists_id(store, "experiment_requests", &id.to_string())?;
        if !found {
            return Err(Error::NotFound(format!(
                "Evidence Manifest references missing Experiment {id}"
            )));
        }
    }
    for (table, ids) in [
        (
            "chaos_campaigns",
            manifest
                .chaos_campaigns
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
        ),
        (
            "execution_attestations",
            manifest
                .attestations
                .iter()
                .map(ToString::to_string)
                .collect(),
        ),
        (
            "lessons",
            manifest.lessons.iter().map(ToString::to_string).collect(),
        ),
        (
            "reflexes",
            manifest.reflexes.iter().map(ToString::to_string).collect(),
        ),
        (
            "recoveries",
            manifest
                .recoveries
                .iter()
                .map(ToString::to_string)
                .collect(),
        ),
        (
            "operating_envelopes",
            manifest.envelopes.iter().map(ToString::to_string).collect(),
        ),
        (
            "capability_manifests",
            manifest
                .capability_manifests
                .iter()
                .map(ToString::to_string)
                .collect(),
        ),
        (
            "commit_receipts",
            manifest
                .effect_receipts
                .iter()
                .map(ToString::to_string)
                .collect(),
        ),
    ] {
        for id in ids {
            require_id(store, table, &id)?;
        }
    }
    Ok(())
}

fn exists_id(store: &Store, table: &str, id: &str) -> Result<bool> {
    let allowed = [
        "experiences",
        "experiments",
        "experiment_requests",
        "chaos_campaigns",
        "execution_attestations",
        "lessons",
        "reflexes",
        "recoveries",
        "operating_envelopes",
        "capability_manifests",
        "commit_receipts",
        "tool_definitions",
    ];
    if !allowed.contains(&table) {
        return Err(Error::InvalidInput("Unsupported evidence table".into()));
    }
    let sql = format!("SELECT 1 FROM {table} WHERE id=?1");
    Ok(store
        .connection
        .query_row(&sql, [id], |_| Ok(()))
        .optional()?
        .is_some())
}

fn require_id(store: &Store, table: &str, id: &str) -> Result<()> {
    if exists_id(store, table, id)? {
        Ok(())
    } else {
        Err(Error::NotFound(format!(
            "Evidence Manifest references missing {table} record {id}"
        )))
    }
}

fn require_composite(
    store: &Store,
    table: &str,
    first_column: &str,
    first: &str,
    second_column: &str,
    second: i64,
) -> Result<()> {
    if table != "skill_revisions" || first_column != "skill_id" || second_column != "revision" {
        return Err(Error::InvalidInput(
            "Unsupported composite reference".into(),
        ));
    }
    let found = store
        .connection
        .query_row(
            "SELECT 1 FROM skill_revisions WHERE skill_id=?1 AND revision=?2",
            params![first, second],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if found {
        Ok(())
    } else {
        Err(Error::NotFound(format!(
            "Evidence Manifest references missing Skill revision {first}@{second}"
        )))
    }
}
