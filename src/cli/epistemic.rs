// SPDX-License-Identifier: Apache-2.0

use std::{fs, io::Write, path::PathBuf};

use clap::{Subcommand, ValueEnum};
use serde_json::{Value, json};

use super::{Cli, Commands, LessonCommand};
use crate::{
    Error, Result,
    core::{ClaimId, EvidencePathId},
    epistemic::{Claim, ClaimKind, EvidencePath, ExperienceActivationState, contrast},
    lesson::ContextSelector,
    store::{EpistemicStore, Store},
};

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ClaimKindArg {
    StrategyPreference,
    LessonClaim,
    RecoveryClaim,
    SkillBehavior,
    FailureCause,
    OperatingEnvelopeClaim,
    RuntimeDecisionClaim,
    Custom,
}

impl From<ClaimKindArg> for ClaimKind {
    fn from(value: ClaimKindArg) -> Self {
        match value {
            ClaimKindArg::StrategyPreference => Self::StrategyPreference,
            ClaimKindArg::LessonClaim => Self::LessonClaim,
            ClaimKindArg::RecoveryClaim => Self::RecoveryClaim,
            ClaimKindArg::SkillBehavior => Self::SkillBehavior,
            ClaimKindArg::FailureCause => Self::FailureCause,
            ClaimKindArg::OperatingEnvelopeClaim => Self::OperatingEnvelopeClaim,
            ClaimKindArg::RuntimeDecisionClaim => Self::RuntimeDecisionClaim,
            ClaimKindArg::Custom => Self::Custom,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum EpistemicCommand {
    /// Register a deterministically scoped evidence target.
    Create {
        #[arg(long, value_enum)]
        kind: ClaimKindArg,
        #[arg(long)]
        statement: String,
        #[arg(long)]
        repository: Option<PathBuf>,
        #[arg(long = "marker")]
        markers: Vec<String>,
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long)]
        os: Option<String>,
        #[arg(long)]
        arch: Option<String>,
    },
    /// Record a structured conclusion and observable context, never chain-of-thought.
    Report {
        file: PathBuf,
    },
    Show {
        claim: ClaimId,
    },
    Graph {
        claim: ClaimId,
    },
    Diversity {
        claim: ClaimId,
    },
    Gaps {
        claim: ClaimId,
    },
    Challenge {
        claim: ClaimId,
    },
    Compare {
        left: EvidencePathId,
        right: EvidencePathId,
    },
    Domains {
        claim: ClaimId,
    },
    Echoes {
        claim: ClaimId,
    },
}

pub fn handles(command: &Commands) -> bool {
    matches!(
        command,
        Commands::Epistemic { .. }
            | Commands::Lesson {
                command: LessonCommand::Impact { .. }
                    | LessonCommand::Quarantine { .. }
                    | LessonCommand::Restore { .. }
            }
    )
}

pub fn execute(cli: &Cli, store: &Store) -> Result<Value> {
    match &cli.command {
        Commands::Epistemic { command } => match command {
            EpistemicCommand::Create {
                kind,
                statement,
                repository,
                markers,
                tags,
                os,
                arch,
            } => {
                let claim = Claim {
                    id: ClaimId::new(),
                    kind: (*kind).into(),
                    statement: statement.clone(),
                    scope: ContextSelector {
                        repository: repository.clone(),
                        required_markers: markers.clone(),
                        tags: tags.clone(),
                        os: os.clone(),
                        arch: arch.clone(),
                    },
                    created_at: chrono::Utc::now(),
                };
                store.insert_claim(&claim)?;
                Ok(json!({"kind":"claim_created", "claim":claim}))
            }
            EpistemicCommand::Report { file } => {
                let metadata = fs::symlink_metadata(file)?;
                if !metadata.is_file()
                    || metadata.file_type().is_symlink()
                    || metadata.len() > 1024 * 1024
                {
                    return Err(Error::InvalidInput(
                        "Evidence report must be a regular non-symlink file no larger than 1 MiB"
                            .into(),
                    ));
                }
                let path: EvidencePath = serde_json::from_slice(&fs::read(file)?)?;
                let stored = store.insert_evidence_path(&path)?;
                Ok(json!({"kind":"evidence_path_recorded", "path":stored}))
            }
            EpistemicCommand::Show { claim } => {
                Ok(json!({"kind":"epistemic_show", "report":store.epistemic_report(claim)?}))
            }
            EpistemicCommand::Graph { claim } => {
                let report = store.epistemic_report(claim)?;
                Ok(
                    json!({"kind":"epistemic_graph", "claim":report.claim, "graph":report.graph, "disclaimer":"Known dependency overlap is not proof of statistical dependence or causality."}),
                )
            }
            EpistemicCommand::Diversity { claim } => {
                let report = store.epistemic_report(claim)?;
                Ok(
                    json!({"kind":"epistemic_diversity", "claim":report.claim, "support":report.fused.support_paths.len(), "contradictions":report.fused.contradiction_paths.len(), "diversity":report.diversity}),
                )
            }
            EpistemicCommand::Gaps { claim } => {
                let report = store.epistemic_report(claim)?;
                Ok(json!({"kind":"epistemic_gaps", "claim":report.claim, "gaps":report.gaps}))
            }
            EpistemicCommand::Challenge { claim } => {
                let report = store.epistemic_report(claim)?;
                Ok(
                    json!({"kind":"epistemic_challenge", "claim":report.claim, "plan":report.challenge, "principle":"The best next evidence is often the evidence most likely to prove us wrong."}),
                )
            }
            EpistemicCommand::Compare { left, right } => {
                let left_path = store.evidence_path(left)?;
                let right_path = store.evidence_path(right)?;
                if left_path.claim.id != right_path.claim.id {
                    return Err(Error::InvalidInput(
                        "Evidence comparison requires paths for the same Claim".into(),
                    ));
                }
                Ok(
                    json!({"kind":"epistemic_compare", "claim":left_path.claim.id, "contrast":contrast(&left_path,&right_path)}),
                )
            }
            EpistemicCommand::Domains { claim } => {
                let report = store.epistemic_report(claim)?;
                Ok(
                    json!({"kind":"epistemic_domains", "claim":report.claim, "domains":report.domains}),
                )
            }
            EpistemicCommand::Echoes { claim } => {
                let report = store.epistemic_report(claim)?;
                Ok(json!({"kind":"epistemic_echoes", "claim":report.claim, "echoes":report.echoes}))
            }
        },
        Commands::Lesson { command } => match command {
            LessonCommand::Impact { id } => {
                Ok(json!({"kind":"lesson_impact", "impact":store.lesson_impact(id)?}))
            }
            LessonCommand::Quarantine { id, reason } => Ok(json!({
                "kind":"lesson_activation_changed",
                "event":store.set_lesson_activation(id, ExperienceActivationState::Quarantined, reason.clone())?,
                "automatic_retrieval":false,
                "historical_evidence_retained":true,
            })),
            LessonCommand::Restore { id, reason } => Ok(json!({
                "kind":"lesson_activation_changed",
                "event":store.set_lesson_activation(id, ExperienceActivationState::Active, reason.clone())?,
                "automatic_retrieval":true,
                "historical_evidence_retained":true,
            })),
            _ => Err(Error::InvalidInput("Epistemic dispatch failed".into())),
        },
        _ => Err(Error::InvalidInput("Epistemic dispatch failed".into())),
    }
}

pub fn print(value: &Value, out: &mut impl Write) -> Result<()> {
    match value["kind"].as_str() {
        Some("epistemic_show") => {
            let report = &value["report"];
            writeln!(
                out,
                "Claim\n  {}",
                report["claim"]["statement"].as_str().unwrap_or("UNKNOWN")
            )?;
            writeln!(
                out,
                "\nAssessment\n  {}",
                display_enum(&report["fused"]["status"])
            )?;
            writeln!(
                out,
                "\nDiversity\n  {}",
                display_enum(&report["diversity"]["diversity_class"])
            )?;
            writeln!(
                out,
                "\nEvidence paths\n  {}",
                report["diversity"]["path_count"]
            )?;
            print_overlaps(report["diversity"]["dependency_overlaps"].as_array(), out)?;
            print_gaps(report["gaps"].as_array(), out)?;
        }
        Some("epistemic_diversity") => {
            writeln!(
                out,
                "Claim\n  {}",
                value["claim"]["statement"].as_str().unwrap_or("UNKNOWN")
            )?;
            writeln!(
                out,
                "\nEvidence paths\n  {}",
                value["diversity"]["path_count"]
            )?;
            writeln!(out, "\nSupport\n  {}", value["support"])?;
            print_overlaps(value["diversity"]["dependency_overlaps"].as_array(), out)?;
            writeln!(
                out,
                "\nDiversity\n  {}",
                display_enum(&value["diversity"]["diversity_class"])
            )?;
            for caveat in value["diversity"]["caveats"]
                .as_array()
                .into_iter()
                .flatten()
            {
                writeln!(out, "  {}", caveat.as_str().unwrap_or("UNKNOWN"))?;
            }
        }
        Some("epistemic_gaps") => print_gaps(value["gaps"].as_array(), out)?,
        Some("epistemic_challenge") => {
            writeln!(
                out,
                "Claim\n  {}",
                value["claim"]["statement"].as_str().unwrap_or("UNKNOWN")
            )?;
            writeln!(out, "\nRecommended challenge")?;
            for action in value["plan"]["actions"].as_array().into_iter().flatten() {
                writeln!(out, "  {}", serde_json::to_string(action)?)?;
            }
            for reason in value["plan"]["rationale"].as_array().into_iter().flatten() {
                writeln!(out, "  why: {}", reason.as_str().unwrap_or("UNKNOWN"))?;
            }
        }
        Some("epistemic_graph")
        | Some("epistemic_compare")
        | Some("epistemic_domains")
        | Some("epistemic_echoes")
        | Some("claim_created")
        | Some("evidence_path_recorded")
        | Some("lesson_impact")
        | Some("lesson_activation_changed") => {
            serde_json::to_writer_pretty(&mut *out, value)?;
            writeln!(out)?;
        }
        _ => return Err(Error::InvalidInput("Unknown epistemic response".into())),
    }
    Ok(())
}

fn print_overlaps(overlaps: Option<&Vec<Value>>, out: &mut impl Write) -> Result<()> {
    if let Some(overlaps) = overlaps.filter(|overlaps| !overlaps.is_empty()) {
        writeln!(out, "\nKnown overlap")?;
        for overlap in overlaps {
            writeln!(
                out,
                "  {} · {} · {} paths",
                display_enum(&overlap["kind"]),
                overlap["shared_value"].as_str().unwrap_or("UNKNOWN"),
                overlap["paths"].as_array().map_or(0, Vec::len)
            )?;
        }
    }
    Ok(())
}

fn print_gaps(gaps: Option<&Vec<Value>>, out: &mut impl Write) -> Result<()> {
    writeln!(out, "\nMissing evidence")?;
    match gaps {
        Some(gaps) if !gaps.is_empty() => {
            for gap in gaps {
                writeln!(out, "  {}", gap.as_str().unwrap_or("UNKNOWN"))?;
            }
        }
        _ => writeln!(out, "  none identified by the deterministic policy")?,
    }
    Ok(())
}

fn display_enum(value: &Value) -> String {
    value
        .as_str()
        .unwrap_or("unknown")
        .replace('_', " ")
        .to_uppercase()
}
