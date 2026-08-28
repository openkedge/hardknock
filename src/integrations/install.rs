// SPDX-License-Identifier: Apache-2.0
use crate::{Error, Result, cli::integrations::AdapterCommand};
use serde_json::{Value, json};
use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};
const CLAUDE_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
    "SessionEnd",
];
fn invalid(s: &str) -> Error {
    Error::InvalidInput(s.into())
}
pub fn find_executable(name: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths).map(|p| p.join(name)).find(|p| {
            use std::os::unix::fs::PermissionsExt;
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}
fn user_home() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| invalid("HOME unavailable; provide --config"))
}
fn path_for(agent: &str, override_path: &Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = override_path {
        return Ok(path.clone());
    }
    Ok(match agent {
        "claude" => user_home()?.join(".claude/settings.json"),
        "hermes" => user_home()?.join(".hermes/plugins/hardknock"),
        "openclaw" => user_home()?.join(".openclaw/extensions/hardknock"),
        _ => return Err(invalid("Unknown adapter")),
    })
}
fn read_json(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || metadata.len() > 1024 * 1024 {
        return Err(invalid("Refusing symlink or oversized integration config"));
    }
    let value: Value = serde_json::from_slice(&fs::read(path)?)?;
    if !value.is_object() {
        return Err(invalid("Integration configuration must be a JSON object"));
    }
    Ok(value)
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(invalid("Refusing symlink integration file"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("Integration path has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    let permissions = path
        .metadata()
        .map(|m| m.permissions())
        .unwrap_or_else(|_| fs::Permissions::from_mode(0o600));
    file.as_file().set_permissions(permissions)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| Error::Io(e.error))?;
    Ok(())
}
fn manifest_path(home: &Path, agent: &str) -> PathBuf {
    home.join("integrations").join(format!("{agent}.json"))
}
pub fn installed(agent: &str, home: &Path) -> bool {
    if agent == "codex" {
        return find_executable("codex").is_some();
    }
    let Ok(manifest) = read_json(&manifest_path(home, agent)) else {
        return false;
    };
    let Some(path) = manifest["path"].as_str() else {
        return false;
    };
    if agent == "claude" {
        let Ok(settings) = read_json(Path::new(path)) else {
            return false;
        };
        let Some(command) = manifest["command"].as_str() else {
            return false;
        };
        return CLAUDE_EVENTS.iter().all(|event| {
            settings["hooks"][event].as_array().is_some_and(|groups| {
                groups.iter().any(|g| {
                    g["hooks"]
                        .as_array()
                        .is_some_and(|h| h.iter().any(|h| h["command"] == command))
                })
            })
        });
    }
    let files: &[&str] = if agent == "hermes" {
        &["plugin.yaml", "__init__.py"]
    } else {
        &[
            "openclaw.plugin.json",
            "package.json",
            "index.ts",
            "bridge.mjs",
            "hooks.mjs",
        ]
    };
    files.iter().all(|file| {
        Path::new(path)
            .join(file)
            .symlink_metadata()
            .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
    })
}
pub fn manage(agent: &str, home: &Path, command: &AdapterCommand) -> Result<Value> {
    if matches!(command, AdapterCommand::Check) {
        return Ok(
            json!({"agent":agent,"executable_found":find_executable(agent).is_some(),"installed":installed(agent,home)}),
        );
    }
    let (install, override_path) = match command {
        AdapterCommand::Install { config } => (true, config),
        AdapterCommand::Uninstall { config } => (false, config),
        AdapterCommand::Check => return Err(invalid("Unexpected check")),
    };
    // Do not open a domain Store from an adapter installer.
    fs::create_dir_all(home.join("integrations"))?;
    let manifest_path = manifest_path(home, agent);
    let previous = read_json(&manifest_path)?;
    let path = if !install && override_path.is_none() {
        previous["path"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or(path_for(agent, override_path)?)
    } else {
        path_for(agent, override_path)?
    };
    if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(invalid("Refusing symlink integration target"));
    }
    let path = crate::dojo::resolve_home(&path)?;
    if let Some(previous_path) = previous["path"].as_str()
        && Path::new(previous_path) != path
    {
        return Err(invalid(
            "Uninstall the existing managed integration before changing its location",
        ));
    }
    if agent == "claude" {
        let mut value = read_json(&path)?;
        if let Some(hooks) = value.get("hooks")
            && !hooks.is_object()
        {
            return Err(invalid("Claude hooks must be an object"));
        }
        if value.get("hooks").is_none() {
            value["hooks"] = json!({});
        }
        let hooks = value["hooks"]
            .as_object_mut()
            .ok_or_else(|| invalid("Invalid hooks"))?;
        // Remove only the exact previously installed command, retaining other hooks in each group.
        if let Some(old) = previous["command"].as_str() {
            for groups in hooks.values_mut() {
                if let Some(groups) = groups.as_array_mut() {
                    for group in groups.iter_mut() {
                        if let Some(items) = group["hooks"].as_array_mut() {
                            items.retain(|h| h["command"] != old);
                        }
                    }
                    groups.retain(|group| {
                        group["hooks"]
                            .as_array()
                            .is_none_or(|items| !items.is_empty())
                    });
                }
            }
        }
        let command = format!(
            "{} --home {} integration-event --agent claude",
            shell_words::quote(&env::current_exe()?.to_string_lossy()),
            shell_words::quote(&home.to_string_lossy())
        );
        if install {
            for event in CLAUDE_EVENTS {
                let groups = hooks
                    .entry((*event).to_owned())
                    .or_insert_with(|| json!([]))
                    .as_array_mut()
                    .ok_or_else(|| invalid("Claude hook event must be an array"))?;
                groups.push(json!({"matcher":"","hooks":[{"type":"command","command":command,"timeout":10}]}));
            }
        }
        // Validate and fully serialize before replacing user settings.
        atomic_write(&path, &serde_json::to_vec_pretty(&value)?)?;
        if install {
            atomic_write(
                &manifest_path,
                &serde_json::to_vec(&json!({"path":path,"command":command}))?,
            )?;
        }
    } else {
        let files: Vec<(&str, &str)> = match agent {
            "hermes" => vec![
                (
                    "plugin.yaml",
                    include_str!("../../integrations/hermes/plugin.yaml"),
                ),
                (
                    "__init__.py",
                    include_str!("../../integrations/hermes/__init__.py"),
                ),
            ],
            "openclaw" => vec![
                (
                    "package.json",
                    include_str!("../../integrations/openclaw/package.json"),
                ),
                (
                    "openclaw.plugin.json",
                    include_str!("../../integrations/openclaw/openclaw.plugin.json"),
                ),
                (
                    "index.ts",
                    include_str!("../../integrations/openclaw/index.ts"),
                ),
                (
                    "hooks.mjs",
                    include_str!("../../integrations/openclaw/hooks.mjs"),
                ),
                (
                    "bridge.mjs",
                    include_str!("../../integrations/openclaw/bridge.mjs"),
                ),
            ],
            _ => return Err(invalid("Unknown plugin")),
        };
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(invalid("Refusing symlink plugin directory"));
        }
        if install {
            if path.exists() && previous["path"].is_null() && fs::read_dir(&path)?.next().is_some()
            {
                return Err(invalid(
                    "Refusing to overwrite an unmanaged plugin directory",
                ));
            }
            fs::create_dir_all(&path)?;
            for (name, contents) in &files {
                atomic_write(&path.join(name), contents.as_bytes())?;
            }
            atomic_write(
                &manifest_path,
                &serde_json::to_vec(
                    &json!({"path":path,"files":files.iter().map(|(name,_)|name).collect::<Vec<_>>()}),
                )?,
            )?;
        } else if !previous["path"].is_null() {
            // Never recursively delete a plugin directory that might contain user files.
            for (name, _) in &files {
                let target = path.join(name);
                if target.is_file() {
                    fs::remove_file(target)?;
                }
            }
            if path.is_dir() && fs::read_dir(&path)?.next().is_none() {
                fs::remove_dir(&path)?;
            }
        }
    }
    if !install && manifest_path.exists() {
        fs::remove_file(manifest_path)?;
    }
    Ok(
        json!({"agent":agent,"installed":install,"path":path,"note":if agent=="openclaw"{"Files installed. Enable hardknock with OpenClaw plugin allow/enable configuration; this command does not broaden trust."}else{"Restart the agent to load changed hooks/plugins."}}),
    )
}
