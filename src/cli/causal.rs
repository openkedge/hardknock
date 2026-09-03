// SPDX-License-Identifier: Apache-2.0
use super::{Cli, Commands};
use crate::{
    Error, Result, bridge::config::Config, cancellation::Cancellation, causal::*, core::*,
    store::Store,
};
use clap::Subcommand;
use serde_json::{Value, json};
use std::{fs, path::PathBuf};

#[derive(Debug, Subcommand)]
pub enum CausalCommand {
    List,
    Show {
        id: CausalHypothesisId,
    },
    Explain {
        id: CausalHypothesisId,
    },
    /// Register explicit hypotheses and a trusted fixture adapter. Does not run commands.
    Investigate {
        #[arg(long)]
        spec: PathBuf,
    },
    /// Register the bundled retry fixture in --repo. Does not run commands.
    Demo,
    Plan {
        target: String,
    },
    Test {
        id: CausalHypothesisId,
        #[arg(long)]
        investigation: Option<CausalInvestigationId>,
        #[arg(long)]
        trusted_local: bool,
    },
    Compare {
        left: CausalHypothesisId,
        right: CausalHypothesisId,
    },
    Impact {
        id: CausalHypothesisId,
    },
    Replay {
        id: InterventionId,
        #[arg(long)]
        trusted_local: bool,
    },
    Refine {
        id: CausalHypothesisId,
        #[arg(long)]
        investigation: Option<CausalInvestigationId>,
    },
    Envelope {
        id: CausalInvestigationId,
    },
    Curriculum {
        id: CausalInvestigationId,
    },
    Link {
        #[arg(long)]
        spec: PathBuf,
    },
    Benchmark {
        #[arg(long)]
        trusted_local: bool,
    },
    #[command(subcommand)]
    Model(CausalModelCommand),
}
#[derive(Debug, Subcommand)]
pub enum CausalModelCommand {
    List,
    Show {
        id: CausalModelId,
    },
    History {
        id: CausalModelId,
    },
    Graph {
        id: CausalModelId,
    },
    Diff {
        id: CausalModelId,
        #[arg(long)]
        from: u64,
        #[arg(long)]
        to: u64,
    },
}
fn investigation(store: &Store, target: &str) -> Result<CausalInvestigation> {
    if let Ok(id) = target.parse()
        && let Ok(inv) = store.causal_investigation(&id)
    {
        return Ok(inv);
    }
    let found: Vec<_> = store
        .causal_investigations()?
        .into_iter()
        .filter(|inv| {
            inv.hypotheses.iter().any(|h| h.to_string() == target)
                || match &inv.target {
                    CausalTarget::FailureSignature(x) | CausalTarget::Outcome(x) => x == target,
                    CausalTarget::Claim(x) => x.to_string() == target,
                    CausalTarget::Lesson(x) => x.to_string() == target,
                    CausalTarget::RuntimeDecision(x) => x.to_string() == target,
                }
        })
        .collect();
    match found.as_slice() {
        [inv] => Ok(inv.clone()),
        [] => Err(Error::InvalidInput(
            "No registered investigation for target; use causal investigate --spec".into(),
        )),
        _ => Err(Error::InvalidInput(
            "Multiple investigations match; use the exact investigation ID for planning".into(),
        )),
    }
}
fn local(trusted: bool) -> Result<()> {
    if !trusted {
        return Err(Error::Intervention("Execution requires --trusted-local: the git-worktree provider does not isolate host, credentials or network. Only run reviewed local fixture commands; never production interventions.".into()));
    }
    eprintln!("Causal test: trusted local fixture only; worktree is not a security sandbox.");
    Ok(())
}
pub async fn execute(cli: &Cli, store: &Store, cancel: &Cancellation) -> Result<Value> {
    if let Commands::Provenance { object } = &cli.command {
        return store.causal_impact(&object.parse()?);
    }
    let Commands::Causal { command } = &cli.command else {
        return Err(Error::InvalidInput("Expected causal command".into()));
    };
    let config = Config::load(&store.home)?;
    match command {
        CausalCommand::List => Ok(
            json!({"hypotheses":store.causal_hypotheses()?,"investigations":store.causal_investigations()?}),
        ),
        CausalCommand::Show { id } | CausalCommand::Explain { id } => {
            let evidence = store.causal_evidence(id)?;
            Ok(
                json!({"hypothesis":store.causal_hypothesis(id)?,"observations":store.causal_observations_for_hypothesis(id)?,"interventional_evidence":evidence,"scope_refinement_candidates":differing_conditions(&evidence),"interpretation":"Agent explanations and remote reports are candidate/advisory claims. Only local isolated controlled interventions establish support under the tested scope; never causal certainty."}),
            )
        }
        CausalCommand::Investigate { spec } => {
            let input: CausalInvestigationInput = serde_json::from_slice(&fs::read(spec)?)?;
            Ok(json!({"investigation":store.create_causal_investigation(&input)?}))
        }
        CausalCommand::Demo => Ok(
            json!({"investigation":store.create_causal_investigation(&benchmark::stale_state_input(crate::dojo::capture_state(&cli.repo)?))?,"next":"causal plan <investigation-id>; causal test <hypothesis-id> --trusted-local"}),
        ),
        CausalCommand::Plan { target } => {
            let inv = investigation(store, target)?;
            Ok(
                json!({"investigation":inv,"plan":store.plan_causal_investigation(&inv.id,&config)?}),
            )
        }
        CausalCommand::Test {
            id,
            trusted_local,
            investigation: selected,
        } => {
            local(*trusted_local)?;
            let inv = if let Some(selected) = selected {
                store.causal_investigation(selected)?
            } else {
                investigation(store, &id.to_string())?
            };
            if !inv.hypotheses.contains(id) {
                return Err(Error::InvalidInput(
                    "Hypothesis is not in the selected investigation".into(),
                ));
            }
            let spec = store.causal_test_spec(&inv.id)?;
            let h = store.causal_hypothesis(id)?;
            let plan = store.plan_causal_investigation(&inv.id, &config)?;
            let d = plan
                .experiments
                .iter()
                .find(|p| p.intervention.variable == h.cause)
                .ok_or_else(|| {
                    Error::Intervention(
                        "No safe budgeted intervention available; hypothesis may be untestable"
                            .into(),
                    )
                })?;
            execute_causal_run(
                store,
                &config,
                compile_intervention(&inv.id, &spec, d)?,
                cancel,
            )
            .await
        }
        CausalCommand::Compare { left, right } => Ok(
            json!({"left":store.causal_hypothesis(left)?,"left_evidence":store.causal_evidence(left)?,"right":store.causal_hypothesis(right)?,"right_evidence":store.causal_evidence(right)?}),
        ),
        CausalCommand::Impact { id } => store.causal_impact(id),
        CausalCommand::Replay { id, trusted_local } => {
            local(*trusted_local)?;
            let old = store.causal_run(id)?;
            let mut spec = store.causal_test_spec(&old.investigation)?;
            let prior = store
                .experiment_for_request(&old.request.id)?
                .ok_or_else(|| Error::InvalidInput("Prior execution unavailable".into()))?;
            spec.starting_state.expected_fingerprint = prior
                .result
                .and_then(|r| r.starting_state)
                .map(|p| p.fingerprint);
            let inv = store.create_causal_investigation(&CausalInvestigationInput {
                source_experiences: vec![],
                target: CausalTarget::Outcome(format!("Replay {id}")),
                hypotheses: store.causal_investigation_hypotheses(&old.investigation)?,
                spec: spec.clone(),
            })?;
            let mut d = old.discrimination;
            d.intervention.id = InterventionId::new();
            execute_causal_run(
                store,
                &config,
                compile_intervention(&inv.id, &spec, &d)?,
                cancel,
            )
            .await
        }
        CausalCommand::Refine {
            id,
            investigation: selected,
        } => {
            let inv = if let Some(selected) = selected {
                store.causal_investigation(selected)?
            } else {
                investigation(store, &id.to_string())?
            };
            Ok(
                json!({"candidate":store.propose_causal_refinement(&inv.id,id)?,"notice":"No existing Lesson, Reflex or Recovery was changed or activated; validate candidates through existing artifact lifecycle."}),
            )
        }
        CausalCommand::Envelope { id } => Ok(
            json!({"observations":store.causal_envelope_observations(id)?,"unknown":"Every untested input combination; no interpolation"}),
        ),
        CausalCommand::Curriculum { id } => Ok(
            json!({"goals":store.causal_curriculum_goals(id)?,"execution":"recommendation only; no automatic trials"}),
        ),
        CausalCommand::Link { spec } => {
            let dep: CausalArtifactDependency = serde_json::from_slice(&fs::read(spec)?)?;
            store.link_causal_artifact(&dep)?;
            Ok(json!({"dependency":dep}))
        }
        CausalCommand::Benchmark { trusted_local } => {
            local(*trusted_local)?;
            benchmark::run(store, &config, &cli.repo, cancel).await
        }
        CausalCommand::Model(command) => match command {
            CausalModelCommand::List => Ok(json!({"models":store.causal_models()?})),
            CausalModelCommand::Show { id } | CausalModelCommand::Graph { id } => {
                Ok(json!({"model":store.causal_model(id)?}))
            }
            CausalModelCommand::History { id } => {
                Ok(json!({"revisions":store.causal_model_history(id)?}))
            }
            CausalModelCommand::Diff { id, from, to } => {
                let history = store.causal_model_history(id)?;
                let a = history
                    .iter()
                    .find(|m| m.revision == *from)
                    .ok_or_else(|| Error::InvalidInput("Unknown from revision".into()))?;
                let b = history
                    .iter()
                    .find(|m| m.revision == *to)
                    .ok_or_else(|| Error::InvalidInput("Unknown to revision".into()))?;
                Ok(
                    json!({"from":a,"to":b,"changed_edges":b.edges.iter().filter(|e|a.edges.iter().find(|old|old.hypothesis==e.hypothesis).is_none_or(|old|old.status!=e.status||old.conditions!=e.conditions)).collect::<Vec<_>>()}),
                )
            }
        },
    }
}
