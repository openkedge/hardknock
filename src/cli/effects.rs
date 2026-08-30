// SPDX-License-Identifier: Apache-2.0
use super::{Cli, Commands};
use crate::{
    Error, Result,
    core::{EffectId, EffectPlanId, RealityId},
    effects::*,
    store::{EffectStore, Store},
};
use clap::{Subcommand, ValueEnum};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{IsTerminal, Read, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum KindArg {
    Filesystem,
    Process,
    Database,
    HttpApi,
    CloudResource,
    Message,
    Deployment,
    Custom,
}
impl From<KindArg> for EffectKind {
    fn from(value: KindArg) -> Self {
        match value {
            KindArg::Filesystem => Self::Filesystem,
            KindArg::Process => Self::Process,
            KindArg::Database => Self::Database,
            KindArg::HttpApi => Self::HttpApi,
            KindArg::CloudResource => Self::CloudResource,
            KindArg::Message => Self::Message,
            KindArg::Deployment => Self::Deployment,
            KindArg::Custom => Self::Custom,
        }
    }
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum OperationArg {
    Read,
    Create,
    Update,
    Delete,
    Post,
    Dispatch,
    Promote,
}
impl From<OperationArg> for EffectOperation {
    fn from(value: OperationArg) -> Self {
        match value {
            OperationArg::Read => Self::Read,
            OperationArg::Create => Self::Create,
            OperationArg::Update => Self::Update,
            OperationArg::Delete => Self::Delete,
            OperationArg::Post => Self::Post,
            OperationArg::Dispatch => Self::Dispatch,
            OperationArg::Promote => Self::Promote,
        }
    }
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum FaultArg {
    PrepareFailure,
    CommitFailureBeforeMutation,
    ResponseLossAfterMutation,
    ResponseLossWithReconciliationFailure,
    ReservationExpiry,
    DiscardFailure,
    CompensationFailure,
    ReconciliationFailure,
}
impl From<FaultArg> for EffectFault {
    fn from(value: FaultArg) -> Self {
        match value {
            FaultArg::PrepareFailure => Self::PrepareFailure,
            FaultArg::CommitFailureBeforeMutation => Self::CommitFailureBeforeMutation,
            FaultArg::ResponseLossAfterMutation => Self::ResponseLossAfterMutation,
            FaultArg::ResponseLossWithReconciliationFailure => {
                Self::ResponseLossWithReconciliationFailure
            }
            FaultArg::ReservationExpiry => Self::ReservationExpiry,
            FaultArg::DiscardFailure => Self::DiscardFailure,
            FaultArg::CompensationFailure => Self::CompensationFailure,
            FaultArg::ReconciliationFailure => Self::ReconciliationFailure,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum EffectCommand {
    List {
        #[arg(long)]
        reality: Option<RealityId>,
    },
    Show {
        id: EffectId,
    },
    Propose {
        #[arg(long, default_value = "local-cli")]
        session: String,
        #[arg(long)]
        reality: Option<RealityId>,
        #[arg(long)]
        adapter: Option<String>,
        #[arg(long, value_enum)]
        kind: KindArg,
        #[arg(long, value_enum)]
        operation: OperationArg,
        #[arg(long)]
        target: String,
        #[arg(long, default_value = "{}")]
        payload: String,
        #[arg(
            long,
            value_enum,
            help = "Deterministic fixture fault; mock adapters only"
        )]
        inject_fault: Option<FaultArg>,
        #[arg(long)]
        prepare: bool,
    },
    Prepare {
        id: EffectId,
    },
    Commit {
        id: EffectId,
        #[arg(long)]
        yes: bool,
        #[arg(long, conflicts_with = "yes")]
        authorization_file: Option<PathBuf>,
    },
    Discard {
        id: EffectId,
    },
    Compensate {
        id: EffectId,
        #[arg(long)]
        yes: bool,
    },
    Reconcile {
        id: EffectId,
    },
    Capabilities,
    Orphans,
    Cleanup,
    PlanCreate {
        #[arg(long = "effect", required = true)]
        effects: Vec<EffectId>,
        #[arg(long = "dependency", help = "Dependency as BEFORE:AFTER")]
        dependencies: Vec<String>,
        #[arg(long)]
        compensate_on_failure: bool,
    },
    PlanCommit {
        id: EffectPlanId,
        #[arg(long)]
        yes: bool,
    },
    FixtureSet {
        #[arg(long)]
        adapter: String,
        #[arg(long)]
        target: String,
        #[arg(long)]
        state: String,
        #[arg(long)]
        drift: bool,
    },
    FixtureShow {
        #[arg(long)]
        adapter: String,
        #[arg(long)]
        target: String,
    },
}

pub fn handles(command: &Commands) -> bool {
    matches!(command, Commands::Effect { .. })
}

fn authorization_file(path: &Path) -> Result<CommitAuthorization> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 64 * 1024 {
        return Err(Error::InvalidInput(
            "Authorization file must be a regular file of at most 64 KiB".into(),
        ));
    }
    let mut data = Vec::new();
    File::open(path)?
        .take(64 * 1024 + 1)
        .read_to_end(&mut data)?;
    if data.len() > 64 * 1024 {
        return Err(Error::InvalidInput(
            "Authorization file exceeded 64 KiB".into(),
        ));
    }
    Ok(serde_json::from_slice(&data)?)
}

fn confirm(cli: &Cli, effect: &Effect, prepared: &PreparedEffect) -> Result<()> {
    if cli.json || !std::io::stdin().is_terminal() {
        return Err(Error::Intervention(
            "Interactive commit confirmation unavailable; pass --yes or --authorization-file"
                .into(),
        ));
    }
    let mut stderr = std::io::stderr().lock();
    writeln!(
        stderr,
        "PRE-COMMIT CHECK\n\nEffect  {}\nTarget  {}\nPreview {}\n\nNo authoritative mutation has occurred. Proceed? [y/N] ",
        effect.id, effect.target.uri, prepared.preview.summary
    )?;
    stderr.flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    if !matches!(input.trim(), "y" | "Y" | "yes" | "YES") {
        return Err(Error::Intervention("Commit was not approved".into()));
    }
    Ok(())
}

fn parse_dependencies(values: &[String]) -> Result<Vec<EffectDependency>> {
    values
        .iter()
        .map(|value| {
            let (before, after) = value
                .split_once(':')
                .ok_or_else(|| Error::InvalidInput("Dependency must be BEFORE:AFTER".into()))?;
            Ok(EffectDependency {
                before: before.parse()?,
                after: after.parse()?,
            })
        })
        .collect()
}

pub fn execute(cli: &Cli, store: &Store) -> Result<Value> {
    let Commands::Effect { command } = &cli.command else {
        return Err(Error::InvalidInput("Effect dispatch failed".into()));
    };
    let manager = EffectManager::new(store)?;
    let user = EffectManager::user_context();
    match command {
        EffectCommand::List { reality } => Ok(json!({
            "operation":"list",
            "effects":store.effects(reality.as_ref())?
        })),
        EffectCommand::Show { id } => {
            let effect = store.effect(id)?;
            Ok(json!({
                "operation":"show",
                "effect":effect,
                "prepared":store.prepared_effect(id).ok(),
                "receipt":store.commit_receipt_for_effect(id)?,
                "events":store.effect_events(id)?
            }))
        }
        EffectCommand::Propose {
            session,
            reality,
            adapter,
            kind,
            operation,
            target,
            payload,
            inject_fault,
            prepare,
        } => {
            let request = EffectRequest {
                session_id: session.clone(),
                reality_id: reality.clone(),
                source_action: ActionRef {
                    id: format!("cli-{}", uuid::Uuid::new_v4()),
                    kind: "hardknock-effect-cli".into(),
                },
                kind: (*kind).into(),
                target: EffectTarget {
                    uri: target.clone(),
                },
                operation: (*operation).into(),
                payload: serde_json::from_str(payload)?,
                adapter: adapter.clone(),
                evidence: Vec::new(),
                fault: inject_fault.map(Into::into).unwrap_or_default(),
            };
            if *prepare {
                let (effect, prepared) = manager.propose_and_prepare(request, &user)?;
                Ok(json!({
                    "operation":"propose_and_prepare",
                    "effect":effect,
                    "prepared":prepared,
                    "committed":false,
                    "message":"The effect is prepared only. No authoritative external mutation has occurred."
                }))
            } else {
                Ok(json!({"operation":"propose","effect":manager.propose(request,&user)?}))
            }
        }
        EffectCommand::Prepare { id } => {
            let prepared = manager.prepare(id, &user)?;
            Ok(json!({
                "operation":"prepare",
                "prepared":prepared,
                "committed":false,
                "message":"The effect is prepared only. No authoritative external mutation has occurred."
            }))
        }
        EffectCommand::Commit {
            id,
            yes,
            authorization_file: file,
        } => {
            let effect = store.effect(id)?;
            let prepared = store.prepared_effect(id)?;
            let authorization = if let Some(path) = file {
                authorization_file(path)?
            } else {
                if !yes {
                    confirm(cli, &effect, &prepared)?;
                }
                manager.authorize(CommitAuthority::User, std::slice::from_ref(id))?
            };
            Ok(json!({
                "operation":"commit",
                "result":manager.commit(id,Some(&authorization),&user)?
            }))
        }
        EffectCommand::Discard { id } => {
            Ok(json!({"operation":"discard","effect":manager.discard(id,&user)?}))
        }
        EffectCommand::Compensate { id, yes } => {
            if !yes {
                return Err(Error::Intervention(
                    "Compensation is a new external mutation; pass --yes to authorize it".into(),
                ));
            }
            Ok(
                json!({"operation":"compensate","receipt":manager.compensate(id,&user)?,"rollback":false}),
            )
        }
        EffectCommand::Reconcile { id } => {
            Ok(json!({"operation":"reconcile","result":manager.reconcile(id)?}))
        }
        EffectCommand::Capabilities => {
            Ok(json!({"operation":"capabilities","adapters":manager.registry.capabilities()}))
        }
        EffectCommand::Orphans => {
            Ok(json!({"operation":"orphans","effects":store.orphaned_prepared_effects()?}))
        }
        EffectCommand::Cleanup => {
            Ok(json!({"operation":"cleanup","discarded":manager.cleanup_orphans()?}))
        }
        EffectCommand::PlanCreate {
            effects,
            dependencies,
            compensate_on_failure,
        } => {
            let plan = manager.create_plan(
                effects.clone(),
                parse_dependencies(dependencies)?,
                if *compensate_on_failure {
                    EffectAtomicity::CompensatingGroup
                } else {
                    EffectAtomicity::BestEffortGroup
                },
            )?;
            Ok(json!({"operation":"plan_create","plan":plan}))
        }
        EffectCommand::PlanCommit { id, yes } => {
            if !yes {
                return Err(Error::Intervention(
                    "Multi-effect commit requires --yes in this noninteractive flow".into(),
                ));
            }
            let plan = store.effect_plan(id)?;
            let authorization = manager.authorize(CommitAuthority::User, &plan.effects)?;
            Ok(json!({
                "operation":"plan_commit",
                "result":manager.commit_plan(&plan,&authorization,&user)?
            }))
        }
        EffectCommand::FixtureSet {
            adapter,
            target,
            state,
            drift,
        } => {
            let state: Value = serde_json::from_str(state)?;
            let fixture = MockExternalSystem::new(&store.home)?;
            if *drift {
                fixture.mutate_outside(adapter, target, &state)?;
            } else {
                fixture.seed(adapter, target, &state)?;
            }
            Ok(json!({
                "operation":"fixture_set",
                "resource":fixture.resource(adapter,target)?
            }))
        }
        EffectCommand::FixtureShow { adapter, target } => Ok(json!({
            "operation":"fixture_show",
            "resource":MockExternalSystem::new(&store.home)?.resource(adapter,target)?
        })),
    }
}

pub fn print(value: &Value, out: &mut impl Write) -> Result<()> {
    match value["operation"].as_str() {
        Some("capabilities") => {
            writeln!(
                out,
                "ADAPTER\tPREPARE\tCOMMIT\tDISCARD\tCOMPENSATE\tIDEMPOTENT"
            )?;
            if let Some(adapters) = value["adapters"].as_object() {
                for (name, capability) in adapters {
                    writeln!(
                        out,
                        "{}\t{}\t{}\t{}\t{}\t{}",
                        name,
                        capability["prepare"].as_bool().unwrap_or(false),
                        capability["commit"].as_bool().unwrap_or(false),
                        capability["discard"].as_bool().unwrap_or(false),
                        capability["compensate"].as_bool().unwrap_or(false),
                        capability["idempotency_keys"].as_bool().unwrap_or(false)
                    )?;
                }
            }
        }
        Some("list") | Some("orphans") => {
            for effect in value["effects"].as_array().into_iter().flatten() {
                writeln!(
                    out,
                    "{}\t{}\t{}\t{}",
                    effect["id"].as_str().unwrap_or("unknown"),
                    effect["lifecycle"].as_str().unwrap_or("unknown"),
                    effect["adapter"].as_str().unwrap_or("unknown"),
                    effect["target"]["uri"].as_str().unwrap_or("unknown")
                )?;
            }
        }
        _ => {
            serde_json::to_writer_pretty(&mut *out, value)?;
            writeln!(out)?;
        }
    }
    Ok(())
}
