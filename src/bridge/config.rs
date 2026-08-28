// SPDX-License-Identifier: Apache-2.0
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

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
    pub integrations: BTreeMap<String, IntegrationConfig>,
    pub experiments: crate::experimentation::ExperimentsConfig,
    pub experience_budget: crate::experimentation::ExperienceBudgetConfig,
}
impl Config {
    pub fn load(home: &Path) -> Result<Self> {
        let path = home.join("config.toml");
        let config: Self = if path.exists() {
            if std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
                return Err(Error::InvalidInput(
                    "Configuration must not be a symlink".into(),
                ));
            }
            let bytes = std::fs::read_to_string(path)?;
            if bytes.len() > 1024 * 1024 {
                return Err(Error::InvalidInput("Configuration exceeds 1 MiB".into()));
            }
            toml::from_str(&bytes)
                .map_err(|e| Error::InvalidInput(format!("Invalid Hardknock configuration: {e}")))?
        } else {
            Self::default()
        };
        let b = &config.bridge;
        config.experiments.validate(&config.experience_budget)?;
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
