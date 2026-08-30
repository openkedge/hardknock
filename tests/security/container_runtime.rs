// SPDX-License-Identifier: Apache-2.0

use crate::support::Fixture;
use chrono::Duration;
use hardknock::{
    capability::*,
    core::{CommandSpec, EnvironmentMode},
    dojo::{RealityProvider, capture_state},
    store::{CapabilityStore, Store},
};
use std::fs;

#[tokio::test]
async fn optional_live_container_denies_host_secret_socket_ambient_credentials_and_network() {
    if std::env::var_os("HARDKNOCK_TEST_CONTAINER").is_none() {
        eprintln!(
            "skipping optional live container integration: HARDKNOCK_TEST_CONTAINER is unset"
        );
        return;
    }
    let fixture = Fixture::new();
    let host_secret = fixture.temp.path().join("host-secret.txt");
    fs::write(&host_secret, "must-not-be-readable").unwrap();
    let store = Store::open(&fixture.home).unwrap();
    let runtime = ContainerRuntime::detect().expect("container integration requires Docker/Podman");
    let image =
        std::env::var("HARDKNOCK_TEST_CONTAINER_IMAGE").unwrap_or_else(|_| "alpine:3.20".into());
    let provider = ContainerRealityProvider::with_runtime(&store, runtime, image).unwrap();
    let mut manifest = builtin_profile("coding-offline").unwrap();
    manifest.profile = "live-container-credential".into();
    manifest.credentials.push(CredentialCapability {
        provider: "fixture".into(),
        name: "read-only".into(),
        scope: CredentialScope {
            resource: "inventory/*".into(),
        },
        permissions: vec!["read".into()],
        expires_at: Some(chrono::Utc::now() + Duration::minutes(5)),
    });
    manifest.validate().unwrap();
    let mut reality = provider
        .create_with_capabilities(&capture_state(&fixture.repo).unwrap(), &manifest)
        .unwrap();
    let authority = CapabilityTokenAuthority::load_or_create(&store.home).unwrap();
    let token = authority
        .issue(&reality, &manifest, Duration::minutes(5))
        .unwrap();
    store.audit_capability_token(&token).unwrap();
    let raw_secret = b"live-container-secret".to_vec();
    let broker = StaticTestCredentialBroker::new(&store).unwrap();
    let issued = broker
        .issue(
            CredentialRequest {
                provider: "fixture".into(),
                name: "read-only".into(),
                resource: "inventory/widget".into(),
                permission: "read".into(),
                secret: raw_secret.clone(),
            },
            &reality,
            &manifest,
        )
        .unwrap();
    let proxy = CapabilityExecutionProxy::new(&store, SecretRedactor::new(Vec::new())).unwrap();
    let script = format!(
        "set -eu; \
         test \"$(id -u)\" != 0; \
         test ! -e /var/run/docker.sock; \
         test -z \"${{AWS_ACCESS_KEY_ID:-}}\"; \
         test ! -e '{}'; \
         if wget -q -T 2 -O - http://1.1.1.1 >/dev/null 2>&1; then exit 90; fi; \
         test -f \"$HARDKNOCK_CREDENTIAL_FIXTURE_READ_ONLY\"; \
         cat \"$HARDKNOCK_CREDENTIAL_FIXTURE_READ_ONLY\"; \
         printf 'inside-container\\n' > /workspace/container-write.txt; \
         printf 'isolated\\n'",
        host_secret.display()
    );
    let result = proxy
        .execute(
            &reality,
            &token,
            &NormalizedAction::Shell(CommandSpec::shell(&script, EnvironmentMode::Controlled)),
            &fixture.temp.path().join("container-output"),
        )
        .await
        .unwrap();
    let action = match result {
        ActionResult::Process { status, action } => {
            assert_eq!(status, hardknock::core::ProcessStatus::Succeeded);
            action
        }
        other => panic!("expected process result, got {other:?}"),
    };
    let captured = fs::read(&action.stdout.path).unwrap();
    assert!(
        !captured
            .windows(raw_secret.len())
            .any(|value| value == raw_secret)
    );
    assert!(String::from_utf8_lossy(&captured).contains("[REDACTED]"));
    assert_eq!(
        fs::read_to_string(reality.root.join("container-write.txt")).unwrap(),
        "inside-container\n"
    );
    provider.discard(&mut reality).unwrap();
    assert!(broker.secret(&issued).is_err());
    fixture.assert_source_unchanged();
}
