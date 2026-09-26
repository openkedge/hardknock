// SPDX-License-Identifier: Apache-2.0

use hardknock::integrations::manifest::{
    INTEGRATION_MANIFEST_SCHEMA, INTEGRATION_MANIFEST_VERSION, MAX_INTEGRATION_MANIFEST_BYTES,
    MCP_PROTOCOL_VERSION, manifest, to_bounded_json, validate,
};
use serde_json::{Value, json};
use std::process::Command;

const FORBIDDEN: &[&str] = &[
    "agent_execution",
    "approval_grant",
    "experiment_creation",
    "external_effect_commit",
    "unrestricted_command_execution",
    "unrestricted_filesystem_access",
];

#[test]
fn production_manifest_is_stable_bounded_and_secret_free() {
    let manifest = manifest();
    validate(&manifest).unwrap();
    let bytes = to_bounded_json(&manifest).unwrap();
    assert!(bytes.len() <= MAX_INTEGRATION_MANIFEST_BYTES);

    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["schema"], INTEGRATION_MANIFEST_SCHEMA);
    assert_eq!(value["manifest_version"], INTEGRATION_MANIFEST_VERSION);
    assert_eq!(value["package"]["name"], env!("CARGO_PKG_NAME"));
    assert_eq!(value["package"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(value["stability"], "preview");
    assert_eq!(value["runtime"]["command"], "hardknock");
    assert_eq!(value["runtime"]["args"], json!(["mcp", "serve", "--stdio"]));
    assert_eq!(value["runtime"]["required_environment"], json!([]));
    assert_eq!(value["transport"]["kind"], "stdio");
    assert_eq!(value["transport"]["protocol_version"], MCP_PROTOCOL_VERSION);
    assert_eq!(value["tools"].as_array().unwrap().len(), 3);
    assert_eq!(
        value["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        hardknock::mcp::PORTABLE_TOOLS
            .iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>()
    );

    let serialized = String::from_utf8(bytes).unwrap();
    for local_marker in ["/Users/", "/home/", "\\Users\\", "TOKEN", "PASSWORD"] {
        assert!(!serialized.contains(local_marker), "{local_marker}");
    }
}

#[test]
fn committed_schema_pins_the_public_contract() {
    let schema: Value = serde_json::from_str(include_str!(
        "../schemas/integration-manifest-v1.schema.json"
    ))
    .unwrap();
    assert_eq!(schema["$id"], INTEGRATION_MANIFEST_SCHEMA);
    assert_eq!(
        schema["properties"]["manifest_version"]["const"],
        INTEGRATION_MANIFEST_VERSION
    );
    assert_eq!(
        schema["properties"]["transport"]["properties"]["protocol_version"]["const"],
        MCP_PROTOCOL_VERSION
    );

    let required = schema["required"].as_array().unwrap();
    for field in [
        "schema",
        "manifest_version",
        "package",
        "stability",
        "runtime",
        "transport",
        "tools",
        "capabilities",
        "security",
        "support",
    ] {
        assert!(required.contains(&json!(field)), "{field}");
    }

    let exclusions = &schema["properties"]["security"]["properties"]["exclusions"];
    for forbidden in FORBIDDEN {
        assert!(
            exclusions["allOf"]
                .as_array()
                .unwrap()
                .iter()
                .any(|rule| rule["contains"]["const"] == *forbidden),
            "{forbidden}"
        );
    }
}

#[test]
fn conformance_rejects_forbidden_or_unstable_surfaces() {
    for forbidden in FORBIDDEN {
        let mut unsafe_manifest = manifest();
        unsafe_manifest.capabilities[0].id = (*forbidden).into();
        unsafe_manifest.tools[0].capability = (*forbidden).into();
        assert!(validate(&unsafe_manifest).is_err(), "{forbidden}");
    }

    let mut wrong_schema = manifest();
    wrong_schema.schema = "https://example.invalid/local-schema.json".into();
    assert!(validate(&wrong_schema).is_err());

    let mut wrong_version = manifest();
    wrong_version.manifest_version = "2.0.0".into();
    assert!(validate(&wrong_version).is_err());

    let mut unbounded = manifest();
    unbounded.capabilities.extend(manifest().capabilities);
    unbounded.capabilities.extend(manifest().capabilities);
    unbounded.capabilities.extend(manifest().capabilities);
    unbounded.capabilities.extend(manifest().capabilities);
    assert!(validate(&unbounded).is_err());
}

#[test]
fn cli_emits_the_same_manifest_without_creating_state() {
    let temporary = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_hardknock"))
        .env("HOME", temporary.path())
        .args(["integration", "manifest"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let emitted: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        emitted,
        serde_json::to_value(manifest()).expect("serialize manifest")
    );
    assert!(
        std::fs::read_dir(temporary.path())
            .unwrap()
            .next()
            .is_none()
    );
}
