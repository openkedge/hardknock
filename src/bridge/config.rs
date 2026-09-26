// SPDX-License-Identifier: Apache-2.0
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BridgeConfig {
    pub autostart: bool,
    pub timeout_ms: u64,
    pub max_context_bytes: usize,
    pub max_context_lessons: usize,
    pub max_sessions: usize,
    pub max_actions: usize,
    pub evaluator_timeout_secs: u64,
    pub max_verification_retries: u32,
    /// Checks are selected by a canonical workspace path in local user configuration.
    /// Wire requests cannot supply executable evaluators.
    pub evaluators: BTreeMap<String, Vec<String>>,
    pub policy: EnforcementPolicy,
    pub experiment_budget: super::protocol::ExperienceBudget,
}
impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            autostart: true,
            timeout_ms: 200,
            max_context_bytes: 32768,
            max_context_lessons: 5,
            max_sessions: 256,
            max_actions: 2048,
            evaluator_timeout_secs: 30,
            max_verification_retries: 1,
            evaluators: BTreeMap::new(),
            policy: EnforcementPolicy::default(),
            experiment_budget: Default::default(),
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EnforcementPolicy {
    /// Only exact whole shell commands; not a security sandbox or shell parser.
    pub blocked_shell_commands: Vec<String>,
    pub approval_shell_commands: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IntegrationConfig {
    pub enabled: bool,
    pub max_context_lessons: usize,
    pub mode: Option<String>,
}
impl Default for IntegrationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_context_lessons: 5,
            mode: None,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub bridge: BridgeConfig,
    pub storage: crate::storage_policy::StoragePolicy,
    pub integrations: BTreeMap<String, IntegrationConfig>,
    pub experiments: crate::experimentation::ExperimentsConfig,
    pub experience_budget: crate::experimentation::ExperienceBudgetConfig,
    pub curriculum: crate::curriculum::CurriculumConfig,
    pub development: crate::development::DevelopmentConfig,
    pub federation: crate::federation::FederationConfig,
    pub effects: crate::effects::EffectConfig,
    pub runtime: RuntimeConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeConfig {
    pub mode: crate::runtime::RuntimeAutonomy,
    pub policy: crate::runtime::RuntimePolicyProfile,
    pub experiment: RuntimeExperimentConfig,
    pub external_experience: crate::runtime::ExternalExperienceRuntimePolicy,
    pub forecast: crate::runtime::RuntimeForecastConfig,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            mode: crate::runtime::RuntimeAutonomy::Advise,
            policy: crate::runtime::RuntimePolicyProfile::Balanced,
            experiment: Default::default(),
            external_experience: Default::default(),
            forecast: Default::default(),
        }
    }
}

impl RuntimeConfig {
    pub fn policy_config(&self) -> crate::runtime::RuntimePolicyConfig {
        let mut config = crate::runtime::RuntimePolicyConfig {
            profile: self.policy,
            autonomy: self.mode,
            experiment_mode: self.experiment.mode,
            external_experience: self.external_experience.clone(),
            forecast: self.forecast.clone(),
            version: crate::runtime::RUNTIME_POLICY_VERSION.into(),
        };
        config.refresh_version();
        config
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeExperimentConfig {
    pub mode: crate::runtime::ExperimentMode,
}

struct OpenedConfig {
    file: File,
    metadata: fs::Metadata,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidInput(message.into())
}

fn same_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

fn open_config(path: &Path) -> Result<Option<OpenedConfig>> {
    let path_metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if path_metadata.file_type().is_symlink() {
        return Err(invalid("Configuration must not be a symlink"));
    }
    if !path_metadata.is_file() {
        return Err(invalid("Configuration must be a regular file"));
    }
    if path_metadata.len() > MAX_CONFIG_BYTES {
        return Err(invalid("Configuration exceeds 1 MiB"));
    }

    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || !same_identity(&path_metadata, &metadata) {
        return Err(invalid("Configuration changed while it was being opened"));
    }
    if metadata.len() > MAX_CONFIG_BYTES {
        return Err(invalid("Configuration exceeds 1 MiB"));
    }
    Ok(Some(OpenedConfig { file, metadata }))
}

fn read_opened_config(path: &Path, opened: &mut OpenedConfig) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(
        usize::try_from(opened.metadata.len())
            .unwrap_or(0)
            .min(8192),
    );
    (&mut opened.file)
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)?;

    let descriptor_metadata = opened.file.metadata()?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES || descriptor_metadata.len() > MAX_CONFIG_BYTES {
        return Err(invalid("Configuration exceeds 1 MiB"));
    }
    let path_metadata = fs::symlink_metadata(path)?;
    if path_metadata.file_type().is_symlink()
        || !path_metadata.is_file()
        || !same_identity(&opened.metadata, &descriptor_metadata)
        || !same_identity(&opened.metadata, &path_metadata)
        || opened.metadata.len() != descriptor_metadata.len()
        || opened.metadata.mtime() != descriptor_metadata.mtime()
        || opened.metadata.mtime_nsec() != descriptor_metadata.mtime_nsec()
        || opened.metadata.ctime() != descriptor_metadata.ctime()
        || opened.metadata.ctime_nsec() != descriptor_metadata.ctime_nsec()
        || opened.metadata.len() != path_metadata.len()
        || opened.metadata.mtime() != path_metadata.mtime()
        || opened.metadata.mtime_nsec() != path_metadata.mtime_nsec()
        || opened.metadata.ctime() != path_metadata.ctime()
        || opened.metadata.ctime_nsec() != path_metadata.ctime_nsec()
    {
        return Err(invalid("Configuration changed while it was being read"));
    }
    Ok(bytes)
}

fn read_config(path: &Path) -> Result<Option<Vec<u8>>> {
    let Some(mut opened) = open_config(path)? else {
        return Ok(None);
    };
    read_opened_config(path, &mut opened).map(Some)
}

impl Config {
    pub fn load(home: &Path) -> Result<Self> {
        let path = home.join("config.toml");
        let config: Self = if let Some(bytes) = read_config(&path)? {
            let text = std::str::from_utf8(&bytes)
                .map_err(|_| invalid("Configuration must be valid UTF-8"))?;
            toml::from_str(text)
                .map_err(|e| Error::InvalidInput(format!("Invalid Hardknock configuration: {e}")))?
        } else {
            Self::default()
        };
        let b = &config.bridge;
        config.experiments.validate(&config.experience_budget)?;
        config.curriculum.validate()?;
        config.development.validate()?;
        config.federation.validate()?;
        config.effects.validate()?;
        config.runtime.policy_config().validate()?;
        config
            .storage
            .validate()
            .map_err(|error| Error::InvalidInput(error.to_string()))?;
        if !(1024..=32768).contains(&b.max_context_bytes)
            || !(1..=5).contains(&b.max_context_lessons)
            || !(1..=10000).contains(&b.max_actions)
            || !(1..=1024).contains(&b.max_sessions)
            || !(10..=10000).contains(&b.timeout_ms)
            || !(1..=300).contains(&b.evaluator_timeout_secs)
            || b.max_verification_retries > 1
        {
            return Err(Error::InvalidInput("Bridge limits out of range".into()));
        }
        for (agent, adapter) in &config.integrations {
            let expected_mode = match agent.as_str() {
                "claude" => "hooks",
                "codex" => "app-server",
                "hermes" | "openclaw" => "plugin",
                _ => {
                    return Err(Error::InvalidInput(
                        "Unknown integration configuration".into(),
                    ));
                }
            };
            if adapter
                .mode
                .as_deref()
                .is_some_and(|mode| mode != expected_mode)
                || adapter.max_context_lessons > 5
            {
                return Err(Error::InvalidInput(
                    "Unsupported integration mode or context limit".into(),
                ));
            }
        }
        for (path, checks) in &b.evaluators {
            if !PathBuf::from(path).is_absolute() || checks.len() > 16 {
                return Err(Error::InvalidInput(
                    "Evaluator requires an absolute workspace path and at most 16 checks".into(),
                ));
            }
            crate::evaluation::EvaluationSpec {
                checks: checks.clone(),
            }
            .validate()?;
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        os::unix::fs::{MetadataExt, symlink},
        process::Command,
        time::Duration,
    };

    #[test]
    fn config_read_rejects_symlinks_and_sparse_oversized_files() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("home");
        fs::create_dir(&home).unwrap();
        let target = temporary.path().join("target.toml");
        fs::write(&target, "").unwrap();
        let path = home.join("config.toml");
        symlink(&target, &path).unwrap();

        assert!(
            Config::load(&home)
                .unwrap_err()
                .to_string()
                .contains("symlink")
        );

        fs::remove_file(&path).unwrap();
        File::create(&path)
            .unwrap()
            .set_len(MAX_CONFIG_BYTES + 1)
            .unwrap();
        assert!(
            Config::load(&home)
                .unwrap_err()
                .to_string()
                .contains("exceeds 1 MiB")
        );
    }

    #[test]
    fn config_read_enforces_the_limit_when_the_open_file_grows() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("config.toml");
        fs::write(&path, "").unwrap();
        let mut opened = open_config(&path).unwrap().unwrap();
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_CONFIG_BYTES + 1)
            .unwrap();

        assert!(
            read_opened_config(&path, &mut opened)
                .unwrap_err()
                .to_string()
                .contains("exceeds 1 MiB")
        );
    }

    #[test]
    fn config_read_rejects_path_replacement_after_open() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("config.toml");
        let original = temporary.path().join("original.toml");
        fs::write(&path, "").unwrap();
        let mut opened = open_config(&path).unwrap().unwrap();
        fs::rename(&path, &original).unwrap();
        fs::write(&path, "").unwrap();

        assert!(
            read_opened_config(&path, &mut opened)
                .unwrap_err()
                .to_string()
                .contains("changed while it was being read")
        );
    }

    #[test]
    fn config_read_rejects_same_inode_rewrite_with_restored_mtime() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("config.toml");
        let timestamp_reference = temporary.path().join("timestamp-reference");
        fs::write(&path, b"aaaa").unwrap();
        assert!(
            Command::new("touch")
                .arg("-r")
                .arg(&path)
                .arg(&timestamp_reference)
                .status()
                .unwrap()
                .success()
        );
        let mut opened = open_config(&path).unwrap().unwrap();
        std::thread::sleep(Duration::from_millis(20));

        let mut writer = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        writer.write_all(b"bbbb").unwrap();
        writer.sync_all().unwrap();
        drop(writer);
        assert!(
            Command::new("touch")
                .arg("-r")
                .arg(&timestamp_reference)
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );

        let rewritten = fs::metadata(&path).unwrap();
        assert_eq!(opened.metadata.ino(), rewritten.ino());
        assert_eq!(opened.metadata.len(), rewritten.len());
        assert_eq!(opened.metadata.mtime(), rewritten.mtime());
        assert_eq!(opened.metadata.mtime_nsec(), rewritten.mtime_nsec());
        assert_ne!(
            (opened.metadata.ctime(), opened.metadata.ctime_nsec()),
            (rewritten.ctime(), rewritten.ctime_nsec())
        );
        assert!(
            read_opened_config(&path, &mut opened)
                .unwrap_err()
                .to_string()
                .contains("changed while it was being read")
        );
    }
}
