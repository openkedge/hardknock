// SPDX-License-Identifier: Apache-2.0
use crate::{Error, Result, cli::integrations::AdapterCommand};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    env, fs,
    io::Write,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

const MAX_INTEGRATION_JSON_BYTES: u64 = 1024 * 1024;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;

const CLAUDE_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
    "SessionEnd",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationAction {
    Install,
    Uninstall,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct IntegrationPlan {
    pub agent: String,
    pub action: IntegrationAction,
    pub action_summary: String,
    pub home_path: PathBuf,
    pub target_path: PathBuf,
    pub config_path: PathBuf,
    pub manifest_path: PathBuf,
    pub managed_paths: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hardknock_executable: Option<PathBuf>,
}

impl IntegrationPlan {
    pub fn description(&self) -> Result<Value> {
        Ok(serde_json::to_value(self)?)
    }
}

fn invalid(s: &str) -> Error {
    Error::InvalidInput(s.into())
}
pub fn find_executable(name: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths).map(|p| p.join(name)).find(|p| {
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

fn validate_existing_private_directory(path: &Path, label: &str) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(invalid(&format!("{label} must be a regular directory")));
    }
    if metadata.uid() != nix::unistd::geteuid().as_raw() {
        return Err(invalid(&format!(
            "{label} must be owned by the effective user"
        )));
    }
    if metadata.permissions().mode() & 0o7777 != PRIVATE_DIRECTORY_MODE {
        return Err(invalid(&format!(
            "{label} permissions must be {PRIVATE_DIRECTORY_MODE:04o}"
        )));
    }
    Ok(())
}

fn validate_existing_user_file(path: &Path, label: &str) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(invalid(&format!("{label} must be a regular file")));
    }
    if metadata.uid() != nix::unistd::geteuid().as_raw() || metadata.nlink() != 1 {
        return Err(invalid(&format!(
            "{label} must be singly linked and owned by the effective user"
        )));
    }
    Ok(())
}

fn read_json(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_INTEGRATION_JSON_BYTES
    {
        return Err(invalid("Refusing symlink or oversized integration config"));
    }
    let value: Value = serde_json::from_slice(&fs::read(path)?)?;
    if !value.is_object() {
        return Err(invalid("Integration configuration must be a JSON object"));
    }
    Ok(value)
}

fn read_manifest(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    validate_existing_user_file(path, "Managed integration manifest")?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.permissions().mode() & 0o7777 != PRIVATE_FILE_MODE {
        return Err(invalid(
            "Managed integration manifest permissions must be 0600",
        ));
    }
    read_json(path)
}

fn validate_existing_private_file(path: &Path, label: &str) -> Result<()> {
    validate_existing_user_file(path, label)?;
    if path.exists()
        && fs::symlink_metadata(path)?.permissions().mode() & 0o7777 != PRIVATE_FILE_MODE
    {
        return Err(invalid(&format!("{label} permissions must be 0600")));
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8], private: bool) -> Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(invalid("Refusing symlink integration file"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("Integration path has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    let permissions = if private {
        fs::Permissions::from_mode(PRIVATE_FILE_MODE)
    } else {
        path.metadata()
            .map(|m| m.permissions())
            .unwrap_or_else(|_| fs::Permissions::from_mode(PRIVATE_FILE_MODE))
    };
    file.as_file().set_permissions(permissions)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| Error::Io(e.error))?;
    Ok(())
}

fn manifest_path(home: &Path, agent: &str) -> PathBuf {
    home.join("integrations").join(format!("{agent}.json"))
}

fn resolve_target(path: &Path) -> Result<PathBuf> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(invalid("Refusing symlink integration target"));
    }
    crate::dojo::resolve_home(path)
}

fn validate_hardknock_executable(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
    {
        return Err(invalid(
            "Hardknock executable path must be absolute and normalized",
        ));
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.permissions().mode() & 0o111 == 0
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(invalid(
            "Hardknock executable must be a regular, executable, non-writable-by-others file",
        ));
    }
    let uid = nix::unistd::geteuid().as_raw();
    if metadata.uid() != uid && metadata.uid() != 0 {
        return Err(invalid(
            "Hardknock executable must be owned by the effective user or root",
        ));
    }
    Ok(path.to_owned())
}

fn adapter_files(agent: &str) -> Result<Vec<(&'static str, &'static str)>> {
    Ok(match agent {
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
    })
}

pub fn plan(
    agent: &str,
    home: &Path,
    command: &AdapterCommand,
    hardknock_executable: &Path,
) -> Result<IntegrationPlan> {
    let action = match command {
        AdapterCommand::Check => {
            return Err(invalid(
                "Check is observational and does not produce an integration mutation plan",
            ));
        }
        AdapterCommand::Install { .. } => IntegrationAction::Install,
        AdapterCommand::Uninstall { .. } => IntegrationAction::Uninstall,
    };
    if !matches!(agent, "claude" | "hermes" | "openclaw") {
        return Err(invalid("Unknown adapter"));
    }

    let home = crate::dojo::resolve_home(home)?;
    validate_existing_private_directory(&home, "HARDKNOCK_HOME")?;
    let integration_directory = home.join("integrations");
    validate_existing_private_directory(&integration_directory, "Managed integration directory")?;
    let manifest_path = manifest_path(&home, agent);
    let previous = read_manifest(&manifest_path)?;
    let override_path = match command {
        AdapterCommand::Install { config } | AdapterCommand::Uninstall { config } => config,
        AdapterCommand::Check => unreachable!(),
    };
    let requested_path = if action == IntegrationAction::Uninstall && override_path.is_none() {
        previous["path"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or(path_for(agent, override_path)?)
    } else {
        path_for(agent, override_path)?
    };
    let target_path = resolve_target(&requested_path)?;
    if let Some(previous_path) = previous["path"].as_str() {
        let previous_path = resolve_target(Path::new(previous_path))?;
        if previous_path != target_path {
            return Err(invalid(
                "Uninstall the existing managed integration before changing its location",
            ));
        }
    }

    let executable = if agent == "claude" && action == IntegrationAction::Install {
        Some(validate_hardknock_executable(hardknock_executable)?)
    } else {
        None
    };
    let (config_path, managed_paths) = if agent == "claude" {
        validate_existing_user_file(&target_path, "Claude configuration")?;
        read_json(&target_path)?;
        (
            target_path.clone(),
            vec![target_path.clone(), manifest_path.clone()],
        )
    } else {
        if target_path.exists() {
            if previous["path"].is_null()
                && action == IntegrationAction::Install
                && fs::read_dir(&target_path)?.next().is_some()
            {
                return Err(invalid(
                    "Refusing to overwrite an unmanaged plugin directory",
                ));
            }
            validate_existing_private_directory(&target_path, "Managed plugin directory")?;
        }
        let files = adapter_files(agent)?;
        let config_name = if agent == "hermes" {
            "plugin.yaml"
        } else {
            "openclaw.plugin.json"
        };
        let mut managed_paths = files
            .iter()
            .map(|(name, _)| target_path.join(name))
            .collect::<Vec<_>>();
        for managed_path in &managed_paths {
            validate_existing_private_file(managed_path, "Managed plugin file")?;
        }
        managed_paths.push(manifest_path.clone());
        (target_path.join(config_name), managed_paths)
    };

    let action_summary = match (action, agent) {
        (IntegrationAction::Install, "claude") => {
            "Install managed Claude lifecycle hooks".to_owned()
        }
        (IntegrationAction::Uninstall, "claude") => {
            "Remove managed Claude lifecycle hooks".to_owned()
        }
        (IntegrationAction::Install, _) => format!("Install managed {agent} plugin files"),
        (IntegrationAction::Uninstall, _) => format!("Remove managed {agent} plugin files"),
    };
    Ok(IntegrationPlan {
        agent: agent.to_owned(),
        action,
        action_summary,
        home_path: home,
        target_path,
        config_path,
        manifest_path,
        managed_paths,
        hardknock_executable: executable,
    })
}

pub fn describe_plan(
    agent: &str,
    home: &Path,
    command: &AdapterCommand,
    hardknock_executable: &Path,
) -> Result<Value> {
    plan(agent, home, command, hardknock_executable)?.description()
}

pub fn installed(agent: &str, home: &Path) -> bool {
    if agent == "codex" {
        return find_executable("codex").is_some();
    }
    let Ok(manifest) = read_manifest(&manifest_path(home, agent)) else {
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
    let Ok(files) = adapter_files(agent) else {
        return false;
    };
    files.iter().all(|(name, expected)| {
        let target = Path::new(path).join(name);
        validate_existing_private_file(&target, "Managed plugin file").is_ok()
            && fs::read(target).is_ok_and(|bytes| bytes == expected.as_bytes())
    })
}

pub fn manage(agent: &str, home: &Path, command: &AdapterCommand) -> Result<Value> {
    if matches!(command, AdapterCommand::Check) {
        return Ok(
            json!({"agent":agent,"executable_found":find_executable(agent).is_some(),"installed":installed(agent,home)}),
        );
    }
    let executable = env::current_exe()?;
    manage_with_executable(agent, home, command, &executable)
}

pub fn manage_with_executable(
    agent: &str,
    home: &Path,
    command: &AdapterCommand,
    hardknock_executable: &Path,
) -> Result<Value> {
    if matches!(command, AdapterCommand::Check) {
        return manage(agent, home, command);
    }
    let integration_plan = plan(agent, home, command, hardknock_executable)?;
    apply(&integration_plan)
}

pub fn apply(integration_plan: &IntegrationPlan) -> Result<Value> {
    let command = match integration_plan.action {
        IntegrationAction::Install => AdapterCommand::Install {
            config: Some(integration_plan.target_path.clone()),
        },
        IntegrationAction::Uninstall => AdapterCommand::Uninstall {
            config: Some(integration_plan.target_path.clone()),
        },
    };
    let executable = integration_plan
        .hardknock_executable
        .as_deref()
        .unwrap_or(Path::new("/"));
    let current = plan(
        &integration_plan.agent,
        &integration_plan.home_path,
        &command,
        executable,
    )?;
    if current != *integration_plan {
        return Err(invalid(
            "Integration plan no longer matches the filesystem; create a new plan",
        ));
    }

    let install = integration_plan.action == IntegrationAction::Install;
    let path = &integration_plan.target_path;
    let manifest_path = &integration_plan.manifest_path;
    let previous = read_manifest(manifest_path)?;
    let mut preserved_modified = Vec::new();

    // Do not open a domain Store from an adapter installer.
    if install {
        let home = manifest_path
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| invalid("Managed integration manifest has no home directory"))?;
        fs::create_dir_all(home)?;
        fs::set_permissions(home, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE))?;
        let integration_directory = manifest_path
            .parent()
            .ok_or_else(|| invalid("Managed integration manifest has no parent"))?;
        fs::create_dir_all(integration_directory)?;
        fs::set_permissions(
            integration_directory,
            fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE),
        )?;
    }

    if integration_plan.agent == "claude" {
        let mut value = read_json(path)?;
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
            shell_words::quote(
                &integration_plan
                    .hardknock_executable
                    .as_deref()
                    .unwrap_or(executable)
                    .to_string_lossy()
            ),
            shell_words::quote(&integration_plan.home_path.to_string_lossy())
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
        atomic_write(path, &serde_json::to_vec_pretty(&value)?, false)?;
        if install {
            atomic_write(
                manifest_path,
                &serde_json::to_vec(&json!({"path":path,"command":command}))?,
                true,
            )?;
        }
    } else {
        let files = adapter_files(&integration_plan.agent)?;
        if install {
            fs::create_dir_all(path)?;
            fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE))?;
            for (name, contents) in &files {
                atomic_write(&path.join(name), contents.as_bytes(), true)?;
            }
            atomic_write(
                manifest_path,
                &serde_json::to_vec(&json!({
                    "path":path,
                    "files":files.iter().map(|(name, contents)| json!({
                        "name":name,
                        "blake3":blake3::hash(contents.as_bytes()).to_hex().to_string()
                    })).collect::<Vec<_>>()
                }))?,
                true,
            )?;
        } else if !previous["path"].is_null() {
            // Never recursively delete a plugin directory that might contain user files.
            for (name, expected) in &files {
                let target = path.join(name);
                if target.is_file() {
                    if fs::read(&target)? == expected.as_bytes() {
                        fs::remove_file(target)?;
                    } else {
                        preserved_modified.push(target);
                    }
                }
            }
            if path.is_dir() && fs::read_dir(path)?.next().is_none() {
                fs::remove_dir(path)?;
            }
        }
    }
    if !install && manifest_path.exists() {
        fs::remove_file(manifest_path)?;
    }
    Ok(
        json!({"agent":integration_plan.agent,"installed":install,"path":path,"preserved_modified":preserved_modified,"note":if integration_plan.agent=="openclaw"{"Files installed. Enable hardknock with OpenClaw plugin allow/enable configuration; this command does not broaden trust."}else{"Restart the agent to load changed hooks/plugins."}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn executable(root: &Path, name: &str) -> PathBuf {
        let path = root.join(name);
        fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[test]
    fn plan_is_exact_redacted_and_does_not_mutate() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock home");
        let config = temporary.path().join("claude settings.json");
        let executable = executable(temporary.path(), "stable hardknock");
        fs::write(&config, br#"{"api_token":"do-not-serialize","hooks":{}}"#).unwrap();

        let plan = plan(
            "claude",
            &home,
            &AdapterCommand::Install {
                config: Some(config.clone()),
            },
            &executable,
        )
        .unwrap();
        let resolved_home = crate::dojo::resolve_home(&home).unwrap();
        let resolved_config = config.canonicalize().unwrap();

        assert_eq!(plan.action, IntegrationAction::Install);
        assert_eq!(plan.home_path, resolved_home);
        assert_eq!(plan.target_path, resolved_config);
        assert_eq!(plan.config_path, resolved_config);
        assert_eq!(
            plan.manifest_path,
            resolved_home.join("integrations/claude.json")
        );
        assert_eq!(
            plan.managed_paths,
            vec![
                resolved_config,
                resolved_home.join("integrations/claude.json")
            ]
        );
        assert_eq!(
            plan.action_summary,
            "Install managed Claude lifecycle hooks"
        );
        let description = serde_json::to_string(&plan.description().unwrap()).unwrap();
        assert!(!description.contains("do-not-serialize"));
        assert!(!resolved_home.exists());
    }

    #[test]
    fn apply_uses_explicit_executable_and_secures_managed_state() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        let config = temporary.path().join("settings.json");
        let executable = executable(temporary.path(), "hardknock stable");
        let plan = plan(
            "claude",
            &home,
            &AdapterCommand::Install {
                config: Some(config.clone()),
            },
            &executable,
        )
        .unwrap();

        apply(&plan).unwrap();

        let settings = read_json(&config).unwrap();
        let command = settings["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        let executable_text = executable.to_string_lossy().into_owned();
        let home_text = home.to_string_lossy().into_owned();
        let quoted_executable = shell_words::quote(&executable_text);
        let quoted_home = shell_words::quote(&home_text);
        assert!(command.starts_with(quoted_executable.as_ref()));
        assert!(command.contains(quoted_home.as_ref()));
        assert_eq!(
            fs::symlink_metadata(home.join("integrations"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o700
        );
        assert_eq!(
            fs::symlink_metadata(home.join("integrations/claude.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o600
        );
    }

    #[test]
    fn apply_refuses_a_plan_that_became_unmanaged() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        let target = temporary.path().join("hermes");
        let executable = executable(temporary.path(), "stable-hardknock");
        let plan = plan(
            "hermes",
            &home,
            &AdapterCommand::Install {
                config: Some(target.clone()),
            },
            &executable,
        )
        .unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(target.join("user.txt"), "user data").unwrap();

        assert!(apply(&plan).is_err());
        assert!(!target.join("plugin.yaml").exists());
        assert!(!home.exists());
    }

    #[test]
    fn planning_refuses_symlinks_oversized_json_and_insecure_manifests() {
        let temporary = tempfile::tempdir().unwrap();
        let executable = executable(temporary.path(), "stable-hardknock");
        let home = temporary.path().join("hardknock");
        let target = temporary.path().join("settings.json");
        let linked = temporary.path().join("linked.json");
        fs::write(&target, "{}").unwrap();
        symlink(&target, &linked).unwrap();
        assert!(
            plan(
                "claude",
                &home,
                &AdapterCommand::Install {
                    config: Some(linked),
                },
                &executable,
            )
            .is_err()
        );

        let oversized = temporary.path().join("oversized.json");
        fs::write(
            &oversized,
            vec![b' '; MAX_INTEGRATION_JSON_BYTES as usize + 1],
        )
        .unwrap();
        assert!(
            plan(
                "claude",
                &home,
                &AdapterCommand::Install {
                    config: Some(oversized),
                },
                &executable,
            )
            .is_err()
        );

        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(home.join("integrations")).unwrap();
        fs::set_permissions(home.join("integrations"), fs::Permissions::from_mode(0o700)).unwrap();
        let manifest = home.join("integrations/hermes.json");
        fs::write(&manifest, "{}").unwrap();
        fs::set_permissions(&manifest, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            plan(
                "hermes",
                &home,
                &AdapterCommand::Uninstall { config: None },
                &executable,
            )
            .is_err()
        );
    }

    #[test]
    fn uninstall_preserves_modified_managed_plugin_files() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("hardknock");
        let target = temporary.path().join("hermes");
        let executable = executable(temporary.path(), "stable-hardknock");
        let install_plan = plan(
            "hermes",
            &home,
            &AdapterCommand::Install {
                config: Some(target.clone()),
            },
            &executable,
        )
        .unwrap();
        apply(&install_plan).unwrap();
        fs::write(target.join("plugin.yaml"), "user-modified").unwrap();

        let uninstall_plan = plan(
            "hermes",
            &home,
            &AdapterCommand::Uninstall {
                config: Some(target.clone()),
            },
            &executable,
        )
        .unwrap();
        let report = apply(&uninstall_plan).unwrap();

        assert_eq!(
            fs::read_to_string(target.join("plugin.yaml")).unwrap(),
            "user-modified"
        );
        assert!(!target.join("__init__.py").exists());
        assert_eq!(report["preserved_modified"].as_array().unwrap().len(), 1);
        assert!(!home.join("integrations/hermes.json").exists());
    }
}
