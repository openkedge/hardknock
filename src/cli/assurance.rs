// SPDX-License-Identifier: Apache-2.0

use std::{fs, io::Write, path::PathBuf};

use chrono::Utc;
use clap::{Args, Subcommand};
use serde_json::{Value, json};

use super::{Cli, Commands};
use crate::{
    Error, Result,
    assurance::*,
    core::SkillCertificationId,
    federation::{ExperienceNodeType, NodeIdentity, ProvenanceGraph},
    store::{AssuranceStore, Store, ToolStore},
};

#[derive(Debug, Subcommand)]
pub enum ContractCommand {
    List,
    Show {
        id: String,
    },
    Validate {
        target: String,
    },
    History {
        id: String,
    },
    Diff {
        id: String,
        #[arg(long)]
        from: u64,
        #[arg(long)]
        to: u64,
    },
    /// Accept a project contract file as a new immutable revision and bind it to a Skill.
    Register {
        file: PathBuf,
        #[arg(long)]
        skill: String,
        #[arg(long)]
        reason: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum AssuranceCommand {
    Show {
        skill: String,
    },
    Gaps {
        skill: String,
        #[arg(long, default_value = "basic-behavior-v1")]
        profile: String,
    },
    History {
        skill: String,
    },
    Diff {
        from: SkillCertificationId,
        to: SkillCertificationId,
    },
    Export {
        skill: String,
        #[arg(long)]
        profile: String,
        #[arg(long)]
        output: PathBuf,
    },
    Verify {
        file: PathBuf,
    },
    Revoke {
        id: SkillCertificationId,
        #[arg(long)]
        reason: String,
    },
}

#[derive(Debug, Args)]
pub struct CertifyArgs {
    pub name: String,
    #[arg(long, default_value = "basic-behavior-v1")]
    pub profile: String,
    #[arg(long)]
    pub dry_run: bool,
    /// Maximum trials for a suggested follow-up curriculum. Certification never auto-runs them.
    #[arg(long)]
    pub budget: Option<u32>,
}

pub fn handles(command: &Commands) -> bool {
    matches!(
        command,
        Commands::Contract { .. }
            | Commands::Assurance { .. }
            | Commands::Skill {
                command: super::resilience::SkillCommand::Certify(_)
            }
    )
}

pub fn execute(cli: &Cli, store: &Store) -> Result<Value> {
    match &cli.command {
        Commands::Contract { command } => execute_contract(store, command),
        Commands::Assurance { command } => execute_assurance(store, command),
        Commands::Skill {
            command: super::resilience::SkillCommand::Certify(args),
        } => certify(store, args),
        _ => Err(Error::InvalidInput("Assurance dispatch failed".into())),
    }
}

fn execute_contract(store: &Store, command: &ContractCommand) -> Result<Value> {
    match command {
        ContractCommand::List => Ok(json!({"contracts": store.behavioral_contracts()?})),
        ContractCommand::Show { id } => {
            let contract = store.behavioral_contract(id)?;
            Ok(json!({
                "contract": contract,
                "observability": contract_observability(&contract.contract)?,
            }))
        }
        ContractCommand::Validate { target } => {
            if PathBuf::from(target).is_file() {
                let file = read_contract_file(&PathBuf::from(target))?;
                let contract =
                    file.into_contract(ContractSubject::Skill(crate::core::SkillId::new()))?;
                Ok(json!({
                    "valid": true,
                    "source": target,
                    "contract": contract,
                    "observability": contract_observability(&contract)?,
                    "persisted": false,
                }))
            } else {
                let revision = store.behavioral_contract(target)?;
                revision.contract.validate()?;
                Ok(json!({
                    "valid": true,
                    "contract": revision,
                    "observability": contract_observability(&revision.contract)?,
                    "persisted": true,
                }))
            }
        }
        ContractCommand::History { id } => {
            let latest = store.behavioral_contract(id)?;
            Ok(json!({
                "contract_id": latest.contract_id,
                "history": store.behavioral_contract_history(&latest.contract_id)?,
            }))
        }
        ContractCommand::Diff { id, from, to } => {
            let latest = store.behavioral_contract(id)?;
            let from = store.behavioral_contract_revision(&latest.contract_id, *from)?;
            let to = store.behavioral_contract_revision(&latest.contract_id, *to)?;
            Ok(contract_diff(&from, &to)?)
        }
        ContractCommand::Register {
            file,
            skill,
            reason,
        } => {
            let skill = store.skill(skill)?;
            let contract_file = read_contract_file(file)?;
            let contract = contract_file.into_contract(ContractSubject::Skill(skill.id.clone()))?;
            let observability = contract_observability(&contract)?;
            let revision = store.register_behavioral_contract(contract, reason.clone())?;
            let reference = BehavioralContractRef {
                contract_id: revision.contract_id.clone(),
                revision: revision.revision,
            };
            store.bind_skill_contract(&skill.id, &reference)?;
            Ok(json!({
                "contract": revision,
                "binding": {"skill_id": skill.id, "contract": reference},
                "observability": observability,
                "warning": (!observability.unobservable.is_empty()).then_some("Unobservable required conditions will block certification"),
            }))
        }
    }
}

fn execute_assurance(store: &Store, command: &AssuranceCommand) -> Result<Value> {
    match command {
        AssuranceCommand::Show { skill } => assurance_show(store, skill),
        AssuranceCommand::Gaps { skill, profile } => {
            let candidate = evaluate_candidate(store, skill, profile)?;
            Ok(json!({
                "skill": candidate.skill,
                "contract": candidate.contract,
                "profile": candidate.profile,
                "gaps": candidate.evaluation.gaps,
                "blockers": candidate.evaluation.blockers,
                "recommendation": candidate.evaluation.recommendation,
                "suggested_curriculum": certification_curriculum(&candidate.evaluation),
            }))
        }
        AssuranceCommand::History { skill } => assurance_history(store, skill),
        AssuranceCommand::Diff { from, to } => certification_diff(store, from, to),
        AssuranceCommand::Export {
            skill,
            profile,
            output,
        } => export_certification(store, skill, profile, output),
        AssuranceCommand::Verify { file } => verify_artifact(file),
        AssuranceCommand::Revoke { id, reason } => Ok(json!({
            "revocation": store.revoke_skill_certification(id, reason)?,
            "certificate_deleted": false,
        })),
    }
}

pub fn verify_artifact(file: &PathBuf) -> Result<Value> {
    let artifact = CertificationArtifact::read(file)?;
    let verification = artifact.verify();
    Ok(json!({
        "artifact": file,
        "producer": artifact.signature.as_ref().map(|value| &value.producer_name),
        "skill": artifact.certification.skill,
        "profile": artifact.profile,
        "verification": verification,
        "remote_certification": if verification.authentic {"authentic"} else {"invalid"},
        "local_certification": "not_established",
    }))
}

struct Candidate {
    skill: SkillRevisionRef,
    contract: BehavioralContractRevision,
    profile: AssuranceProfile,
    manifest: EvidenceManifest,
    evaluation: CertificationEvaluation,
}

fn evaluate_candidate(store: &Store, name: &str, profile: &str) -> Result<Candidate> {
    let skill = store.skill(name)?;
    let revision = store
        .skill_revisions(&skill.id)?
        .last()
        .cloned()
        .ok_or_else(|| Error::NotFound("Skill revision missing".into()))?;
    let skill_ref = SkillRevisionRef {
        skill_id: skill.id.clone(),
        revision: revision.revision,
    };
    let contract_ref = skill
        .behavioral_contract
        .ok_or_else(|| Error::Intervention(format!(
            "Skill {} has no Behavioral Contract; register one with `hardknock contract register FILE --skill {}`",
            skill.name, skill.name
        )))?;
    let contract =
        store.behavioral_contract_revision(&contract_ref.contract_id, contract_ref.revision)?;
    let profile = builtin_profile(profile)?;
    let manifest = store.collect_certification_evidence(&skill_ref, &contract, &profile)?;
    let evaluation = DeterministicCertificationEvaluator::default().evaluate(
        &skill_ref,
        &contract.contract,
        &profile,
        &manifest,
    )?;
    Ok(Candidate {
        skill: skill_ref,
        contract,
        profile,
        manifest,
        evaluation,
    })
}

fn certify(store: &Store, args: &CertifyArgs) -> Result<Value> {
    if args
        .budget
        .is_some_and(|budget| budget == 0 || budget > 100)
    {
        return Err(Error::InvalidInput(
            "Certification curriculum budget must be 1–100 trials".into(),
        ));
    }
    let candidate = evaluate_candidate(store, &args.name, &args.profile)?;
    let suggested_curriculum = certification_curriculum(&candidate.evaluation);
    if args.dry_run || candidate.evaluation.recommendation != CertificationRecommendation::Eligible
    {
        return Ok(json!({
            "mode": "dry_run",
            "certified": false,
            "skill": candidate.skill,
            "contract": {"id": candidate.contract.contract_id, "revision": candidate.contract.revision},
            "profile": candidate.profile,
            "evidence_manifest": candidate.manifest,
            "evaluation": candidate.evaluation,
            "suggested_curriculum": suggested_curriculum,
            "curriculum_budget": args.budget,
            "effects_committed": false,
        }));
    }
    store.insert_evidence_manifest(&candidate.manifest)?;
    let certification = issue_certification(
        candidate.skill.clone(),
        BehavioralContractRef {
            contract_id: candidate.contract.contract_id.clone(),
            revision: candidate.contract.revision,
        },
        &candidate.profile,
        &candidate.manifest,
        &candidate.evaluation,
        None,
    )?;
    store.insert_skill_certification(&certification)?;
    Ok(json!({
        "mode": "issued",
        "certified": true,
        "certification": certification,
        "contract": candidate.contract,
        "profile": candidate.profile,
        "evidence_manifest": candidate.manifest,
        "evaluation": candidate.evaluation,
        "known_unknowns": candidate.evaluation.gaps,
    }))
}

fn assurance_show(store: &Store, name: &str) -> Result<Value> {
    let skill = store.skill(name)?;
    let revisions = store.skill_revisions(&skill.id)?;
    let current_revision = revisions.last().map(|value| value.revision).unwrap_or(0);
    let contract = skill.behavioral_contract.clone();
    let current_contract_revision = contract.as_ref().map(|value| value.revision).unwrap_or(0);
    let mut rows = vec![];
    for certification in store.skill_certifications(&skill.id)? {
        let manifest = store.evidence_manifest(&certification.evidence_manifest)?;
        let freshness = DeterministicFreshnessPolicy.status(
            &certification,
            &CurrentAssuranceContext {
                now: Utc::now(),
                skill_revision: current_revision,
                contract_revision: current_contract_revision,
                tool_artifact_hashes: current_tool_hashes(store, &manifest)?,
                runtime_digests: manifest.summary.runtime_digests.clone(),
                invalidated: store.certification_revocation(&certification.id)?.is_some(),
            },
        );
        rows.push(json!({
            "certification": certification,
            "freshness": freshness,
            "revocation": store.certification_revocation(&certification.id)?,
            "known_unknowns": manifest.summary.known_unknowns,
        }));
    }
    Ok(json!({
        "skill": skill,
        "skill_revision": current_revision,
        "behavioral_contract": contract,
        "certifications": rows,
        "semantic_scope": "Certified means this Skill revision satisfied this contract under this named profile and evidence manifest; it is not universal correctness.",
    }))
}

fn current_tool_hashes(
    store: &Store,
    manifest: &EvidenceManifest,
) -> Result<std::collections::BTreeSet<String>> {
    let mut hashes = std::collections::BTreeSet::new();
    for id in &manifest.attestations {
        let attestation = store.execution_attestation(id)?;
        if let Ok(definition) = store.tool_definition_by_name(&attestation.tool.name)
            && let Some(hash) = definition.artifact_hash()?
        {
            hashes.insert(hash);
        }
    }
    if manifest.attestations.is_empty() {
        hashes = manifest.summary.tool_artifact_hashes.clone();
    }
    Ok(hashes)
}

fn assurance_history(store: &Store, name: &str) -> Result<Value> {
    let skill = store.skill(name)?;
    let history = store
        .skill_certifications(&skill.id)?
        .into_iter()
        .map(|certification| {
            let revocation = store.certification_revocation(&certification.id)?;
            Ok(json!({
                "certification": certification,
                "revocation": revocation,
                "historical": true,
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({"skill": skill.id, "history": history}))
}

fn export_certification(
    store: &Store,
    name: &str,
    profile_name: &str,
    output: &PathBuf,
) -> Result<Value> {
    let skill = store.skill(name)?;
    let profile = builtin_profile(profile_name)?;
    let certification = store
        .skill_certifications(&skill.id)?
        .into_iter()
        .rev()
        .find(|certification| {
            certification.profile.id == profile.id
                && certification.profile.version == profile.version
                && store
                    .certification_revocation(&certification.id)
                    .ok()
                    .flatten()
                    .is_none()
        })
        .ok_or_else(|| {
            Error::NotFound(format!("No active {profile_name} certification for {name}"))
        })?;
    let manifest = store.evidence_manifest(&certification.evidence_manifest)?;
    let contract = store.behavioral_contract_revision(
        &certification.contract.contract_id,
        certification.contract.revision,
    )?;
    let identity = NodeIdentity::load_or_create(
        &store.home,
        "local-hardknock",
        ExperienceNodeType::LocalDeveloper,
    )?;
    let mut artifact = CertificationArtifact::new(
        certification.clone(),
        contract,
        profile,
        manifest,
        ProvenanceGraph::default(),
    )?;
    artifact.sign(&identity)?;
    artifact.write(output)?;
    Ok(json!({
        "output": output,
        "certification": certification.id,
        "producer": identity.node,
        "artifact_hash": blake3::hash(&fs::read(output)?).to_hex().to_string(),
    }))
}

fn certification_diff(
    store: &Store,
    from: &SkillCertificationId,
    to: &SkillCertificationId,
) -> Result<Value> {
    let from_certification = store.skill_certification(from)?;
    let to_certification = store.skill_certification(to)?;
    let from_manifest = store.evidence_manifest(&from_certification.evidence_manifest)?;
    let to_manifest = store.evidence_manifest(&to_certification.evidence_manifest)?;
    Ok(json!({
        "from": from_certification,
        "to": to_certification,
        "skill_revision_changed": from_certification.skill != to_certification.skill,
        "contract_revision_changed": from_certification.contract != to_certification.contract,
        "profile_changed": from_certification.profile != to_certification.profile,
        "evidence_hash_changed": from_manifest.evidence_hash != to_manifest.evidence_hash,
        "evidence_counts": {
            "from": evidence_counts(&from_manifest),
            "to": evidence_counts(&to_manifest),
        },
        "known_gaps": {
            "from": from_manifest.summary.known_unknowns,
            "to": to_manifest.summary.known_unknowns,
        },
    }))
}

fn evidence_counts(manifest: &EvidenceManifest) -> Value {
    json!({
        "experiences": manifest.experiences.len(),
        "experiments": manifest.experiments.len(),
        "chaos_campaigns": manifest.chaos_campaigns.len(),
        "attestations": manifest.attestations.len(),
        "recoveries": manifest.recoveries.len(),
        "capability_manifests": manifest.capability_manifests.len(),
        "effect_receipts": manifest.effect_receipts.len(),
    })
}

fn contract_diff(
    from: &BehavioralContractRevision,
    to: &BehavioralContractRevision,
) -> Result<Value> {
    let clauses = |contract: &BehavioralContract| -> Result<std::collections::BTreeSet<String>> {
        contract
            .preconditions
            .iter()
            .chain(&contract.postconditions)
            .chain(contract.invariants.iter().map(|value| &value.condition))
            .chain(
                contract
                    .forbidden_outcomes
                    .iter()
                    .map(|value| &value.detector),
            )
            .map(BehavioralCondition::fingerprint)
            .collect()
    };
    let before = clauses(&from.contract)?;
    let after = clauses(&to.contract)?;
    let removed = before.difference(&after).cloned().collect::<Vec<_>>();
    let added = after.difference(&before).cloned().collect::<Vec<_>>();
    let weakened = !removed.is_empty()
        || to.contract.invariants.len() < from.contract.invariants.len()
        || to.contract.forbidden_outcomes.len() < from.contract.forbidden_outcomes.len();
    Ok(json!({
        "contract_id": from.contract_id,
        "from": from.revision,
        "to": to.revision,
        "added_condition_fingerprints": added,
        "removed_condition_fingerprints": removed,
        "capability_requirements_changed": from.contract.capability_requirements != to.contract.capability_requirements,
        "possible_weakening": weakened,
        "warning": weakened.then_some("Behavioral Contract may have been weakened; inspect removed clauses and capability changes"),
    }))
}

fn read_contract_file(path: &PathBuf) -> Result<ContractFile> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024 * 1024 {
        return Err(Error::InvalidInput(
            "Contract must be a regular TOML file of at most 1 MiB".into(),
        ));
    }
    toml::from_str(&fs::read_to_string(path)?)
        .map_err(|error| Error::InvalidInput(format!("Invalid Behavioral Contract TOML: {error}")))
}

pub fn exit_code(result: &Value) -> u8 {
    match result
        .pointer("/evaluation/recommendation")
        .and_then(Value::as_str)
    {
        Some("blocked") => 4,
        Some("additional_evidence_required") => 3,
        _ => 0,
    }
}

pub fn print(result: &Value, out: &mut impl Write) -> Result<()> {
    if let Some(recommendation) = result
        .pointer("/evaluation/recommendation")
        .and_then(Value::as_str)
    {
        writeln!(out, "Behavioral assurance")?;
        writeln!(out, "Recommendation  {}", recommendation.to_uppercase())?;
        if let Some(blockers) = result
            .pointer("/evaluation/blockers")
            .and_then(Value::as_array)
        {
            writeln!(out, "Blockers        {}", blockers.len())?;
        }
        if let Some(gaps) = result.pointer("/evaluation/gaps").and_then(Value::as_array) {
            writeln!(out, "Known gaps      {}", gaps.len())?;
        }
        writeln!(out)?;
    }
    serde_json::to_writer_pretty(&mut *out, result)?;
    writeln!(out)?;
    Ok(())
}
