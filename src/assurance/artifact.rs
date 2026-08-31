// SPDX-License-Identifier: Apache-2.0

use std::{fs, path::Path};

use chrono::{Duration, Utc};

use super::*;
use crate::{
    Error, Result,
    core::SkillCertificationId,
    federation::{NodeIdentity, node_id, parse_public_key, verify_detached},
};

const CERTIFICATION_SIGNING_DOMAIN: &[u8] = b"hardknock-certification-v1\0";
const MAX_CERTIFICATION_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;

impl EvidenceManifest {
    pub fn seal(&mut self) -> Result<()> {
        self.normalize()?;
        self.evidence_hash = self.recompute_hash()?;
        Ok(())
    }

    pub fn verify_hash(&self) -> Result<()> {
        if self.evidence_hash != self.recompute_hash()? {
            return Err(Error::InvalidInput(
                "Evidence Manifest hash mismatch".into(),
            ));
        }
        Ok(())
    }

    pub fn recompute_hash(&self) -> Result<String> {
        let mut canonical = self.clone();
        canonical.normalize()?;
        canonical.evidence_hash.clear();
        canonical.generated_at = chrono::DateTime::<Utc>::UNIX_EPOCH;
        // Manifest identity and generation time describe the local record, not
        // the selected evidence graph. Excluding them makes regeneration stable.
        let mut value = serde_json::to_value(canonical)?;
        value
            .as_object_mut()
            .ok_or_else(|| Error::InvalidInput("Manifest must serialize as an object".into()))?
            .remove("id");
        Ok(blake3::hash(&serde_json::to_vec(&value)?)
            .to_hex()
            .to_string())
    }

    fn normalize(&mut self) -> Result<()> {
        macro_rules! sort_dedup {
            ($field:expr) => {{
                $field.sort();
                $field.dedup();
            }};
        }
        sort_dedup!(self.experiences);
        sort_dedup!(self.experiments);
        sort_dedup!(self.chaos_campaigns);
        sort_dedup!(self.attestations);
        sort_dedup!(self.lessons);
        sort_dedup!(self.reflexes);
        sort_dedup!(self.recoveries);
        sort_dedup!(self.envelopes);
        sort_dedup!(self.capability_manifests);
        sort_dedup!(self.effect_receipts);
        self.summary.known_unknowns.sort();
        self.summary.known_unknowns.dedup();
        self.summary
            .attestation_assurance
            .sort_by_key(|value| match value {
                crate::tool::AttestationAssurance::Observed => 0,
                crate::tool::AttestationAssurance::IsolatedObserved => 1,
                crate::tool::AttestationAssurance::RuntimeVerified => 2,
                crate::tool::AttestationAssurance::HardwareBacked => 3,
            });
        self.summary.attestation_assurance.dedup();
        self.summary.observed_capabilities.sort_by(|left, right| {
            let left = serde_json::to_string(left).unwrap_or_default();
            let right = serde_json::to_string(right).unwrap_or_default();
            left.cmp(&right)
        });
        self.summary.observed_capabilities.dedup();
        self.summary.contract_evaluations.sort_by(|left, right| {
            let left = serde_json::to_string(left).unwrap_or_default();
            let right = serde_json::to_string(right).unwrap_or_default();
            left.cmp(&right)
        });
        self.summary.contradictions.sort_by(|left, right| {
            (&left.description, left.severity, &left.evidence_ids).cmp(&(
                &right.description,
                right.severity,
                &right.evidence_ids,
            ))
        });
        Ok(())
    }
}

pub fn issue_certification(
    skill: SkillRevisionRef,
    contract: BehavioralContractRef,
    profile: &AssuranceProfile,
    manifest: &EvidenceManifest,
    evaluation: &CertificationEvaluation,
    expires_after_days: Option<u32>,
) -> Result<SkillCertification> {
    manifest.verify_hash()?;
    profile.validate()?;
    if evaluation.recommendation != CertificationRecommendation::Eligible
        || !evaluation.blockers.is_empty()
        || evaluation
            .requirements
            .iter()
            .any(|requirement| requirement.status != AssuranceRequirementStatus::Satisfied)
    {
        return Err(Error::Intervention(format!(
            "Certification cannot be issued: {:?}",
            evaluation.recommendation
        )));
    }
    let issued_at = Utc::now();
    Ok(SkillCertification {
        id: SkillCertificationId::new(),
        skill,
        contract,
        profile: AssuranceProfileRef {
            id: profile.id.clone(),
            version: profile.version.clone(),
        },
        status: CertificationStatus::Certified,
        evidence_manifest: manifest.id.clone(),
        issued_at,
        expires_at: expires_after_days.map(|days| issued_at + Duration::days(i64::from(days))),
        supersedes: None,
        policy_versions: manifest.policy_versions.clone(),
        tool_artifact_hashes: manifest.summary.tool_artifact_hashes.clone(),
        runtime_digests: manifest.summary.runtime_digests.clone(),
    })
}

impl CertificationArtifact {
    pub fn new(
        certification: SkillCertification,
        contract: BehavioralContractRevision,
        profile: AssuranceProfile,
        evidence_manifest: EvidenceManifest,
        provenance: crate::federation::ProvenanceGraph,
    ) -> Result<Self> {
        let artifact = Self {
            schema_version: super::CERTIFICATION_SCHEMA_V1.into(),
            certification,
            contract,
            profile,
            evidence_manifest,
            provenance,
            signature: None,
        };
        artifact.validate_internal()?;
        Ok(artifact)
    }

    pub fn sign(&mut self, identity: &NodeIdentity) -> Result<()> {
        self.validate_internal()?;
        self.signature = None;
        let payload = self.canonical_bytes()?;
        self.signature = Some(CertificationSignature {
            algorithm: "ed25519".into(),
            producer: identity.node.id.clone(),
            producer_name: identity.node.name.clone(),
            public_key: identity.node.public_identity.public_key.clone(),
            signature: identity.sign_detached(CERTIFICATION_SIGNING_DOMAIN, &payload),
        });
        Ok(())
    }

    pub fn verify(&self) -> CertificationVerification {
        let mut reasons = vec![];
        let schema_valid = self.schema_version == super::CERTIFICATION_SCHEMA_V1;
        if !schema_valid {
            reasons.push(format!("unsupported schema {}", self.schema_version));
        }
        let manifest_intact = self.evidence_manifest.verify_hash().is_ok();
        if !manifest_intact {
            reasons.push("Evidence Manifest hash mismatch".into());
        }
        let internally_consistent = self.validate_internal().is_ok();
        if !internally_consistent {
            reasons.push("artifact references are internally inconsistent".into());
        }
        let signature_valid = self.signature.as_ref().is_some_and(|signature| {
            let key = parse_public_key(&signature.public_key);
            let producer_matches = key
                .as_ref()
                .ok()
                .and_then(|key| node_id(key.as_bytes()).ok())
                .is_some_and(|producer| producer == signature.producer);
            let signature_matches = self.canonical_bytes().is_ok_and(|payload| {
                verify_detached(
                    &signature.public_key,
                    CERTIFICATION_SIGNING_DOMAIN,
                    &payload,
                    &signature.signature,
                )
                .is_ok()
            });
            signature.algorithm == "ed25519" && producer_matches && signature_matches
        });
        if !signature_valid {
            reasons.push("certification signature absent or invalid".into());
        }
        let authentic = schema_valid && manifest_intact && internally_consistent && signature_valid;
        CertificationVerification {
            schema_valid,
            signature_valid,
            manifest_intact,
            internally_consistent,
            authentic,
            local_certification_established: false,
            local_reproduction_performed: false,
            reasons,
        }
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        if path.extension().and_then(|value| value.to_str()) != Some("hkcert") {
            return Err(Error::InvalidInput(
                "Certification artifacts must use the .hkcert extension".into(),
            ));
        }
        let data = serde_json::to_vec_pretty(self)?;
        if data.len() as u64 > MAX_CERTIFICATION_ARTIFACT_BYTES {
            return Err(Error::InvalidInput(
                "Certification artifact exceeds 16 MiB".into(),
            ));
        }
        if path.exists() {
            return Err(Error::Intervention(format!(
                "Refusing to replace existing artifact {}",
                path.display()
            )));
        }
        fs::write(path, data)?;
        Ok(())
    }

    pub fn read(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > MAX_CERTIFICATION_ARTIFACT_BYTES
        {
            return Err(Error::InvalidInput(
                "Certification artifact must be a regular file of at most 16 MiB".into(),
            ));
        }
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }

    fn canonical_bytes(&self) -> Result<Vec<u8>> {
        let mut unsigned = self.clone();
        unsigned.signature = None;
        Ok(serde_json::to_vec(&unsigned)?)
    }

    fn validate_internal(&self) -> Result<()> {
        self.contract.contract.validate()?;
        self.profile.validate()?;
        self.evidence_manifest.verify_hash()?;
        if self.certification.contract.contract_id != self.contract.contract_id
            || self.certification.contract.revision != self.contract.revision
            || self.contract.contract.id != self.contract.contract_id
            || self.certification.profile.id != self.profile.id
            || self.certification.profile.version != self.profile.version
            || self.certification.evidence_manifest != self.evidence_manifest.id
            || self.evidence_manifest.subject
                != EvidenceSubject::Skill(self.certification.skill.clone())
            || self.certification.policy_versions != self.evidence_manifest.policy_versions
            || self.certification.tool_artifact_hashes
                != self.evidence_manifest.summary.tool_artifact_hashes
            || self.certification.runtime_digests != self.evidence_manifest.summary.runtime_digests
        {
            return Err(Error::InvalidInput(
                "Certification artifact contains mismatched revisions, profile, policies, or evidence"
                    .into(),
            ));
        }
        Ok(())
    }
}
