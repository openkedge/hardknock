// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{
    Error, Result,
    perturbation::{Perturbation, PerturbationParameters},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CurriculumConfig {
    pub max_rounds: usize,
    pub max_trials: usize,
    pub max_realities: usize,
    pub max_agent_runs: usize,
    pub max_duration_seconds: u64,
    pub max_parallel_trials: usize,
    pub min_hardening_dimensions: usize,
    pub require_high_severity_recovery: bool,
    pub require_reflex_negative_controls: bool,
    pub stale_after_days: u32,
    pub agent_requests: bool,
    pub max_agent_session_trials: usize,
    pub profiles: BTreeMap<String, ProfileConfig>,
}
impl Default for CurriculumConfig {
    fn default() -> Self {
        Self {
            max_rounds: 1,
            max_trials: 8,
            max_realities: 16,
            max_agent_runs: 16,
            max_duration_seconds: 300,
            max_parallel_trials: 1,
            min_hardening_dimensions: 3,
            require_high_severity_recovery: true,
            require_reflex_negative_controls: true,
            stale_after_days: 30,
            agent_requests: false,
            max_agent_session_trials: 2,
            profiles: BTreeMap::new(),
        }
    }
}
impl CurriculumConfig {
    pub fn validate(&self) -> Result<()> {
        if !(1..=2).contains(&self.max_rounds)
            || !(1..=32).contains(&self.max_trials)
            || !(1..=64).contains(&self.max_realities)
            || self.max_agent_runs > 64
            || !(1..=3600).contains(&self.max_duration_seconds)
            || self.max_parallel_trials != 1
            || !(1..=32).contains(&self.min_hardening_dimensions)
            || self.stale_after_days == 0
            || self.max_agent_session_trials > 32
            || self.profiles.len() > 32
        {
            return Err(Error::InvalidInput("Curriculum limits out of range; V0.5 executes trials serially, max_rounds is 1 or 2".into()));
        }
        for (name, p) in &self.profiles {
            if name.is_empty()
                || name.len() > 64
                || p.conditions.is_empty()
                || p.conditions.len() > 32
                || p.conditions.iter().any(|c| c.len() > 128)
            {
                return Err(Error::InvalidInput(
                    "Profile needs a bounded name and 1..32 condition names".into(),
                ));
            }
        }
        Ok(())
    }
    pub fn budget(&self, trials: usize) -> Result<crate::budget::ExperienceBudget> {
        self.validate()?;
        if trials == 0 || trials > self.max_trials {
            return Err(Error::InvalidInput(format!(
                "Curriculum trial budget must be 1..{}",
                self.max_trials
            )));
        }
        Ok(crate::budget::ExperienceBudget {
            max_realities: self.max_realities,
            max_agent_runs: self.max_agent_runs,
            max_duration_ms: Some(self.max_duration_seconds * 1000),
            max_curriculum_trials: Some(trials),
            max_parallel_trials: Some(1),
            max_commands_per_reality: None,
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileConfig {
    pub conditions: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PerturbationCatalog {
    pub profiles: Vec<PerturbationProfile>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PerturbationProfile {
    pub name: String,
    pub conditions: Vec<CatalogCondition>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogCondition {
    pub name: String,
    pub dimension: String,
    pub value: String,
    pub severity: Severity,
    pub parameters: Option<PerturbationParameters>,
    pub fixture_only: bool,
    pub supported: bool,
}
impl PerturbationCatalog {
    pub fn configured(config: &CurriculumConfig) -> Result<Self> {
        config.validate()?;
        let mut profiles = BTreeMap::from([
            (
                "resilience-basic".into(),
                vec![
                    "control",
                    "delay:500",
                    "command-failure:6",
                    "env:missing",
                    "config:drift",
                    "dependency:unavailable",
                    "input:stale",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>(),
            ),
            (
                "credential-lifecycle".into(),
                vec![
                    "control".into(),
                    "credential:stale".into(),
                    "credential:expired".into(),
                    "credential:revoked".into(),
                ],
            ),
            (
                "latency-basic".into(),
                vec![
                    "control".into(),
                    "delay:500".into(),
                    "delay:1000".into(),
                    "delay:2000".into(),
                ],
            ),
            (
                "retry-behavior".into(),
                vec![
                    "control".into(),
                    "command-failure:1".into(),
                    "command-failure:3".into(),
                    "command-failure:6".into(),
                ],
            ),
        ]);
        for (name, p) in &config.profiles {
            profiles.insert(name.clone(), p.conditions.clone());
        }
        let mut result = vec![];
        for (name, mut values) in profiles {
            if !values.iter().any(|c| c == "control") {
                values.insert(0, "control".into());
            }
            values.sort();
            values.dedup();
            result.push(PerturbationProfile {
                name,
                conditions: values.iter().map(|v| condition(v)).collect::<Result<_>>()?,
            });
        }
        Ok(Self { profiles: result })
    }
    pub fn profile(&self, name: &str) -> Result<PerturbationProfile> {
        self.profiles
            .iter()
            .find(|p| p.name == name)
            .cloned()
            .ok_or_else(|| Error::InvalidInput(format!("Unknown curriculum profile: {name}")))
    }
}
pub fn condition(name: &str) -> Result<CatalogCondition> {
    let (dimension, value, parameters, severity, fixture_only, supported) = match name {
        "control" => (
            "normal",
            "control".into(),
            None,
            Severity::Informational,
            false,
            true,
        ),
        "env:missing" => (
            "credential_state",
            "missing".into(),
            Some(PerturbationParameters::EnvironmentVariable {
                key: "HK_TOKEN_STATE".into(),
                value: String::new(),
            }),
            Severity::High,
            true,
            true,
        ),
        "credential:stale" => (
            "credential_state",
            "stale".into(),
            Some(PerturbationParameters::EnvironmentVariable {
                key: "HK_TOKEN_STATE".into(),
                value: "STALE_TOKEN".into(),
            }),
            Severity::High,
            true,
            true,
        ),
        "config:drift" => (
            "configuration",
            "drift".into(),
            Some(PerturbationParameters::FileMutation {
                path: "generation".into(),
                content: "2\n".into(),
            }),
            Severity::High,
            true,
            true,
        ),
        "dependency:unavailable" => (
            "dependency",
            "unavailable".into(),
            Some(PerturbationParameters::FileMutation {
                path: "dependency".into(),
                content: "down\n".into(),
            }),
            Severity::Medium,
            true,
            true,
        ),
        "input:stale" => (
            "input",
            "stale".into(),
            Some(PerturbationParameters::FileMutation {
                path: "input-generation".into(),
                content: "0\n".into(),
            }),
            Severity::Medium,
            true,
            true,
        ),
        _ if name.starts_with("delay:") => {
            let ms = name[6..].parse::<u64>().map_err(|_| {
                Error::InvalidInput("Delay must be an integer in milliseconds".into())
            })?;
            (
                "latency",
                ms.to_string(),
                Some(PerturbationParameters::CommandDelay { milliseconds: ms }),
                Severity::Low,
                false,
                true,
            )
        }
        _ if name.starts_with("command-failure:") => {
            let failures = name[16..].parse::<u32>().map_err(|_| {
                Error::InvalidInput("Command failure count must be an integer".into())
            })?;
            (
                "command_failure",
                failures.to_string(),
                Some(PerturbationParameters::CommandFailure {
                    failures,
                    exit_code: 17,
                }),
                Severity::Medium,
                false,
                true,
            )
        }
        _ => (
            "unsupported",
            name.into(),
            None,
            Severity::High,
            true,
            false,
        ),
    };
    if let Some(p) = &parameters {
        Perturbation::new(p.clone()).validate()?;
    }
    Ok(CatalogCondition {
        name: name.into(),
        dimension: dimension.into(),
        value,
        parameters,
        severity,
        fixture_only,
        supported,
    })
}
