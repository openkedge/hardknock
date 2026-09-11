// SPDX-License-Identifier: Apache-2.0
use super::Commands;
use crate::{Error, Result, hierarchy::*, store::Store};
use clap::Subcommand;
use serde_json::{Value, json};
use std::{fs, io::Write, path::PathBuf};
#[derive(Debug, Subcommand)]
pub enum KnowledgeCommand {
    Audit,
    Conflicts,
    Snapshot {
        #[command(subcommand)]
        command: SnapshotCommand,
    },
    Conflict {
        #[command(subcommand)]
        command: ConflictCommand,
    },
    /// Inspect or validate an explicit knowledge forest.
    Hierarchy {
        #[command(subcommand)]
        command: HierarchyCommand,
    },
    /// Resolve current applicability and precedence.
    Resolve {
        #[arg(long)]
        context: PathBuf,
        #[arg(long)]
        policy: Option<PathBuf>,
    },
    /// Explain resolution with the complete deterministic trace.
    Explain {
        #[arg(long)]
        context: PathBuf,
        #[arg(long)]
        policy: Option<PathBuf>,
    },
}
#[derive(Debug, Subcommand)]
pub enum HierarchyCommand {
    Show,
    Validate,
    Import { file: PathBuf },
}

#[derive(Debug, Subcommand)]
pub enum SnapshotCommand {
    Show {
        id: crate::core::KnowledgeSnapshotId,
    },
    Diff {
        a: crate::core::KnowledgeSnapshotId,
        b: crate::core::KnowledgeSnapshotId,
    },
}
#[derive(Debug, Subcommand)]
pub enum ConflictCommand {
    Show {
        id: crate::core::KnowledgeConflictId,
    },
    Plan {
        id: crate::core::KnowledgeConflictId,
        #[arg(long)]
        template: Option<PathBuf>,
        #[arg(long, requires = "template")]
        skill: Option<String>,
    },
}

pub fn execute(command: &Commands, store: &Store) -> Result<Value> {
    let Commands::Knowledge {
        hierarchy,
        id,
        command,
    } = command
    else {
        return Err(Error::InvalidInput("Knowledge dispatch failed".into()));
    };
    match command {
        KnowledgeCommand::Audit => {
            return Ok(
                json!({"schema_version":1,"kind":"knowledge_audit","audit":store.knowledge_audit()?}),
            );
        }
        KnowledgeCommand::Conflicts => {
            return Ok(
                json!({"schema_version":1,"kind":"knowledge_conflicts","conflicts":store.knowledge_conflicts()?}),
            );
        }
        KnowledgeCommand::Snapshot { command } => {
            return match command {
                SnapshotCommand::Show { id } => {
                    Ok(json!({"snapshot":store.knowledge_snapshot(id)?}))
                }
                SnapshotCommand::Diff { a, b } => store.knowledge_snapshot_diff(a, b),
            };
        }
        KnowledgeCommand::Conflict { command } => {
            return match command {
                ConflictCommand::Show { id } => {
                    Ok(json!({"conflict":store.knowledge_conflict(id)?}))
                }
                ConflictCommand::Plan {
                    id,
                    template,
                    skill,
                } => {
                    let template = template
                        .as_ref()
                        .map(|p| -> Result<_> { Ok(serde_json::from_slice(&fs::read(p)?)?) })
                        .transpose()?;
                    let plan = store.plan_knowledge_conflict(id, template, &Default::default())?;
                    let curriculum = skill
                        .as_ref()
                        .map(|skill| {
                            store.compile_knowledge_curriculum(&plan, skill, &Default::default())
                        })
                        .transpose()?;
                    Ok(json!({"plan":plan,"curriculum":curriculum}))
                }
            };
        }
        _ => {}
    }
    if let KnowledgeCommand::Hierarchy {
        command: HierarchyCommand::Import { file },
    } = command
    {
        let h: KnowledgeHierarchy = serde_json::from_slice(&fs::read(file)?)?;
        store.save_knowledge_hierarchy(&h)?;
        return Ok(
            json!({"schema_version":1,"kind":"hierarchy_imported","id":h.id,"revision":h.revision}),
        );
    }
    let hierarchies = if let Some(file) = hierarchy {
        vec![serde_json::from_slice::<KnowledgeHierarchy>(&fs::read(
            file,
        )?)?]
    } else if let Some(id) = id {
        vec![store.knowledge_hierarchy(id)?]
    } else {
        store.knowledge_hierarchies()?
    };
    let mut results = vec![];
    for h in hierarchies {
        let entry = match command {
            KnowledgeCommand::Hierarchy {
                command: HierarchyCommand::Show,
            } => json!({"hierarchy":h}),
            KnowledgeCommand::Hierarchy {
                command: HierarchyCommand::Validate,
            } => {
                json!({"id":h.id,"name":h.name,"nodes":h.nodes.len(),"edges":h.edges.len(),"validation":validate_hierarchy(&h)})
            }
            KnowledgeCommand::Resolve { context, policy }
            | KnowledgeCommand::Explain { context, policy } => {
                let c = KnowledgeContext::from_json(serde_json::from_slice(&fs::read(context)?)?)?;
                let p = match policy {
                    Some(path) => serde_json::from_slice(&fs::read(path)?)?,
                    None => KnowledgeResolutionPolicy::default(),
                };
                json!({"id":h.id,"name":h.name,"effective":DeterministicKnowledgeResolver.resolve(&h,&c,&p)?})
            }
            _ => unreachable!("import handled above"),
        };
        results.push(entry);
    }
    let kind = match command {
        KnowledgeCommand::Hierarchy {
            command: HierarchyCommand::Show,
        } => "hierarchy_show",
        KnowledgeCommand::Hierarchy { .. } => "hierarchy_validation",
        KnowledgeCommand::Resolve { .. } => "knowledge_resolution",
        KnowledgeCommand::Explain { .. } => "knowledge_explanation",
        _ => unreachable!("early command dispatch"),
    };
    Ok(json!({"schema_version":1,"kind":kind,"results":results}))
}
pub fn print(value: &Value, out: &mut impl Write) -> Result<()> {
    let kind = value["kind"].as_str().unwrap_or("");
    if kind == "hierarchy_imported" {
        writeln!(
            out,
            "Imported {} revision {}",
            value["id"], value["revision"]
        )?;
        return Ok(());
    }
    if value.get("results").is_none() {
        serde_json::to_writer_pretty(&mut *out, value)?;
        writeln!(out)?;
        return Ok(());
    }
    let results = value["results"]
        .as_array()
        .ok_or_else(|| Error::InvalidInput("Invalid knowledge response".into()))?;
    if results.is_empty() {
        writeln!(
            out,
            "No hierarchies stored. Use --hierarchy <file> or knowledge hierarchy import <file>."
        )?;
    }
    for entry in results {
        if kind == "hierarchy_show" {
            let h: KnowledgeHierarchy = serde_json::from_value(entry["hierarchy"].clone())?;
            writeln!(out, "Knowledge Hierarchy: {} ({})", h.name, h.id)?;
            let index = KnowledgeHierarchyIndex::new(&h);
            let mut roots = h.root_nodes.clone();
            roots.sort();
            let mut pending: Vec<_> = roots.into_iter().rev().map(|id| (id, 0, None)).collect();
            let mut seen = std::collections::BTreeSet::new();
            while let Some((id, depth, relation)) = pending.pop() {
                let label = h
                    .nodes
                    .get(&id)
                    .map(|n| n.artifact.id.as_str())
                    .unwrap_or("MISSING NODE");
                writeln!(
                    out,
                    "{}{}{} [{}]",
                    "  ".repeat(depth.min(40)),
                    relation
                        .map(|r: KnowledgeHierarchyRelation| format!("{r:?} → "))
                        .unwrap_or_default(),
                    label,
                    id
                )?;
                if !seen.insert(id.clone()) {
                    continue;
                }
                if let Some(edges) = index.children.get(&id) {
                    for e in edges.iter().rev() {
                        pending.push((e.child.clone(), depth + 1, Some(e.relation)))
                    }
                }
            }
            for id in h.nodes.keys().filter(|id| !seen.contains(*id)) {
                writeln!(out, "Unreachable: {id}")?;
            }
        } else if kind == "hierarchy_validation" {
            writeln!(
                out,
                "Hierarchy: {}\nNodes: {}  Edges: {}\nValid: {}",
                entry["name"], entry["nodes"], entry["edges"], entry["validation"]["valid"]
            )?;
            for group in ["errors", "warnings"] {
                for item in entry["validation"][group].as_array().into_iter().flatten() {
                    writeln!(out, "{group}: {} — {}", item["kind"], item["message"])?;
                }
            }
        } else {
            let effective: EffectiveKnowledge = serde_json::from_value(entry["effective"].clone())?;
            writeln!(out, "Effective Knowledge: {}", entry["name"])?;
            for a in &effective.applied {
                writeln!(out, "{:?}: {}", a.role, a.artifact.id)?;
            }
            for a in &effective.advisory {
                writeln!(out, "Advisory: {}", a.artifact.id)?;
            }
            for s in &effective.suppressed {
                writeln!(out, "Suppressed: {} ({:?})", s.artifact.id, s.reason)?;
            }
            for u in &effective.unknown {
                writeln!(out, "Unknown: {} ({:?})", u.artifact.id, u.reason)?;
            }
            if effective.conflicts.is_empty() {
                writeln!(out, "Conflicts: none")?;
            }
            for c in &effective.conflicts {
                writeln!(out, "Conflict {:?}: {}", c.kind, c.reason)?;
            }
            if kind == "knowledge_explanation" {
                for s in &effective.trace {
                    writeln!(
                        out,
                        "{}. {} {:?}: {}",
                        s.sequence, s.node, s.action, s.reason
                    )?;
                }
            }
        }
    }
    Ok(())
}
