// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::{Error, Result, effects::EffectKind};
use chrono::Utc;
use std::collections::BTreeMap;

pub const BUILTIN_PROFILES: &[&str] = &[
    "coding-offline",
    "coding-networked",
    "effect-test",
    "staging-agent",
    "coding-effect-test",
];

fn workspace() -> FilesystemCapabilities {
    FilesystemCapabilities {
        readable: vec![
            FilesystemScope {
                root: "/workspace".into(),
                recursive: true,
            },
            FilesystemScope {
                root: "/tmp".into(),
                recursive: true,
            },
        ],
        writable: vec![
            FilesystemScope {
                root: "/workspace".into(),
                recursive: true,
            },
            FilesystemScope {
                root: "/tmp".into(),
                recursive: true,
            },
        ],
    }
}

fn environment() -> EnvironmentCapabilities {
    EnvironmentCapabilities {
        readable: vec!["PATH".into(), "HOME".into(), "TERM".into(), "LANG".into()],
        values: BTreeMap::from([
            ("HOME".into(), "/tmp/hardknock".into()),
            ("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()),
            ("LANG".into(), "C.UTF-8".into()),
        ]),
    }
}

fn disabled_effects() -> EffectCapabilities {
    EffectCapabilities {
        propose: false,
        prepare: false,
        commit: false,
        scope: EffectCapabilityScope {
            kinds: vec![],
            target_patterns: vec![],
            operations: vec![],
        },
    }
}

fn base(profile: &str) -> CapabilityManifest {
    CapabilityManifest {
        id: crate::core::CapabilityManifestId::new(),
        profile: profile.into(),
        revision: 1,
        filesystem: workspace(),
        process: ProcessCapabilities {
            allow_exec: true,
            allowed_executables: vec![],
            denied_executables: vec![],
            max_processes: Some(256),
        },
        network: NetworkCapabilities {
            mode: NetworkMode::None,
            allow: vec![],
        },
        environment: environment(),
        credentials: vec![],
        effects: disabled_effects(),
        resources: ResourceLimits::default(),
        created_at: Utc::now(),
    }
}

pub fn builtin_profile(name: &str) -> Result<CapabilityManifest> {
    let mut manifest = base(name);
    match name {
        "coding-offline" => {}
        "coding-networked" => {
            manifest.network = NetworkCapabilities {
                mode: NetworkMode::AllowList,
                allow: vec![
                    NetworkEndpointPattern {
                        host: "registry.npmjs.org".into(),
                        port: 443,
                    },
                    NetworkEndpointPattern {
                        host: "api.github.com".into(),
                        port: 443,
                    },
                ],
            };
        }
        "effect-test" => {
            manifest.effects.propose = true;
            manifest.effects.prepare = true;
            manifest.effects.scope.kinds = vec![
                EffectKind::Database,
                EffectKind::HttpApi,
                EffectKind::Message,
                EffectKind::Deployment,
            ];
            manifest.effects.scope.target_patterns = vec![
                "mock://*".into(),
                "mock-db://*".into(),
                "mock-message://*".into(),
                "shadow://*".into(),
            ];
            manifest.effects.scope.operations = all_standard_operations();
        }
        "coding-effect-test" => {
            manifest.effects.propose = true;
            manifest.effects.prepare = true;
            manifest.effects.scope.kinds = vec![EffectKind::Database];
            manifest.effects.scope.target_patterns = vec![
                "postgres://inventory_test/*".into(),
                "mock-db://inventory/*".into(),
            ];
            manifest.effects.scope.operations = vec![
                EffectOperationPattern(crate::effects::EffectOperation::Create),
                EffectOperationPattern(crate::effects::EffectOperation::Update),
                EffectOperationPattern(crate::effects::EffectOperation::Delete),
            ];
        }
        "staging-agent" => {
            manifest.network.mode = NetworkMode::AllowList;
            manifest.network.allow = vec![];
            manifest.effects.propose = true;
            manifest.effects.prepare = true;
            manifest.effects.scope.kinds = vec![EffectKind::Deployment];
            manifest.effects.scope.target_patterns = vec!["shadow://*".into()];
            manifest.effects.scope.operations = vec![
                EffectOperationPattern(crate::effects::EffectOperation::Update),
                EffectOperationPattern(crate::effects::EffectOperation::Promote),
            ];
        }
        _ => {
            return Err(Error::NotFound(format!(
                "Capability profile {name} not found"
            )));
        }
    }
    manifest.validate()?;
    Ok(manifest)
}

fn all_standard_operations() -> Vec<EffectOperationPattern> {
    use crate::effects::EffectOperation;
    vec![
        EffectOperationPattern(EffectOperation::Read),
        EffectOperationPattern(EffectOperation::Create),
        EffectOperationPattern(EffectOperation::Update),
        EffectOperationPattern(EffectOperation::Delete),
        EffectOperationPattern(EffectOperation::Post),
        EffectOperationPattern(EffectOperation::Dispatch),
        EffectOperationPattern(EffectOperation::Promote),
    ]
}
