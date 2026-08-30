// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::{Error, Result, core::CredentialId, store::CapabilityStore, store::Store};
use chrono::Utc;
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct CredentialRequest {
    pub provider: String,
    pub name: String,
    pub resource: String,
    pub permission: String,
    pub secret: Vec<u8>,
}

/// Credentials are issued by the trusted host plane. The V0.9 implementation is
/// synchronous because the only concrete broker is local and deterministic.
pub trait CredentialBroker {
    fn issue(
        &self,
        request: CredentialRequest,
        reality: &crate::core::Reality,
        manifest: &CapabilityManifest,
    ) -> Result<IssuedCredential>;
    fn revoke(&self, credential: &mut IssuedCredential) -> Result<()>;
}

pub struct StaticTestCredentialBroker<'a> {
    store: &'a Store,
    directory: PathBuf,
}

impl<'a> StaticTestCredentialBroker<'a> {
    pub fn new(store: &'a Store) -> Result<Self> {
        let directory = store.home.join("run").join("credentials");
        if directory.exists() && fs::symlink_metadata(&directory)?.file_type().is_symlink() {
            return Err(Error::Intervention(
                "Credential directory must not be a symlink".into(),
            ));
        }
        fs::create_dir_all(&directory)?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        Ok(Self { store, directory })
    }

    fn path(&self, id: &CredentialId) -> PathBuf {
        self.directory.join(format!("{id}.secret"))
    }

    pub fn secret(&self, credential: &IssuedCredential) -> Result<Vec<u8>> {
        if credential.revoked_at.is_some() {
            return Err(Error::Intervention("Credential has been revoked".into()));
        }
        let path = self.path(&credential.id);
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(Error::Intervention(
                "Credential secret must remain a regular 0600 file".into(),
            ));
        }
        Ok(fs::read(path)?)
    }

    pub fn revoke_reality(&self, reality_id: &crate::core::RealityId) -> Result<()> {
        for mut credential in self.store.issued_credentials(reality_id)? {
            if credential.revoked_at.is_none() {
                self.revoke(&mut credential)?;
            }
        }
        Ok(())
    }

    pub fn materialize_for_action(
        &self,
        reality: &crate::core::Reality,
    ) -> Result<MaterializedCredentials> {
        if reality.execution_boundary.provider != "container" || reality.execution_boundary.frozen {
            return Err(Error::Intervention(
                "Credentials can be materialized only for an active container Reality".into(),
            ));
        }
        let base = self
            .store
            .home
            .join("run")
            .join("realities")
            .join(reality.id.to_string())
            .join("credentials");
        if base.exists() && fs::symlink_metadata(&base)?.file_type().is_symlink() {
            return Err(Error::Intervention(
                "Per-Reality credential directory must not be a symlink".into(),
            ));
        }
        fs::create_dir_all(&base)?;
        fs::set_permissions(&base, fs::Permissions::from_mode(0o755))?;
        let action_directory = uuid::Uuid::new_v4().simple().to_string();
        let directory = base.join(&action_directory);
        fs::create_dir(&directory)?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755))?;
        let mut materialized = MaterializedCredentials {
            environment: BTreeMap::new(),
            secrets: Vec::new(),
            files: Vec::new(),
            directory,
        };
        for mut credential in self.store.issued_credentials(&reality.id)? {
            if credential.revoked_at.is_some() {
                continue;
            }
            if credential
                .expires_at
                .is_some_and(|expiry| expiry <= Utc::now())
            {
                self.revoke(&mut credential)?;
                continue;
            }
            let secret = self.secret(&credential)?;
            let filename = format!("{}.secret", credential.id);
            let host_path = materialized.directory.join(&filename);
            let mut options = OpenOptions::new();
            options.write(true).create_new(true).mode(0o444);
            let mut file = options.open(&host_path)?;
            materialized.files.push(host_path.clone());
            file.write_all(&secret)?;
            file.sync_all()?;
            let name = credential_environment_name(&credential);
            let container_path =
                format!("/run/hardknock/credentials/{action_directory}/{filename}");
            if materialized
                .environment
                .insert(name, container_path)
                .is_some()
            {
                return Err(Error::Intervention(
                    "Issued credentials produced an ambiguous environment reference".into(),
                ));
            }
            materialized.secrets.push(secret);
        }
        Ok(materialized)
    }
}

pub struct MaterializedCredentials {
    environment: BTreeMap<String, String>,
    secrets: Vec<Vec<u8>>,
    files: Vec<PathBuf>,
    directory: PathBuf,
}

impl MaterializedCredentials {
    pub fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }

    pub fn secrets(&self) -> &[Vec<u8>] {
        &self.secrets
    }
}

impl Drop for MaterializedCredentials {
    fn drop(&mut self) {
        for path in &self.files {
            let _ = fs::remove_file(path);
        }
        let _ = fs::remove_dir(&self.directory);
    }
}

impl CredentialBroker for StaticTestCredentialBroker<'_> {
    fn issue(
        &self,
        request: CredentialRequest,
        reality: &crate::core::Reality,
        manifest: &CapabilityManifest,
    ) -> Result<IssuedCredential> {
        manifest.validate()?;
        let manifest_hash = manifest.hash()?;
        if reality.execution_boundary.provider != "container"
            || reality.execution_boundary.frozen
            || reality.execution_boundary.manifest_id.as_ref() != Some(&manifest.id)
            || reality.execution_boundary.manifest_hash.as_deref() != Some(manifest_hash.as_str())
            || reality.execution_boundary.manifest_revision != manifest.revision
        {
            return Err(Error::Intervention(
                "Credential manifest is not the current manifest for this Reality".into(),
            ));
        }
        if request.secret.is_empty() || request.secret.len() > 64 * 1024 {
            return Err(Error::InvalidInput(
                "Test credential must contain 1–65536 bytes".into(),
            ));
        }
        let evaluation = DenyByDefaultCapabilityPolicy.evaluate(
            &CapabilityRequest::Credential {
                provider: request.provider.clone(),
                name: request.name.clone(),
                resource: request.resource.clone(),
                permission: request.permission,
            },
            manifest,
        );
        if evaluation.decision != CapabilityDecision::Allow {
            return Err(Error::Intervention(evaluation.reason));
        }
        let configured = manifest
            .credentials
            .iter()
            .find(|item| item.provider == request.provider && item.name == request.name)
            .ok_or_else(|| Error::Intervention("Credential is outside manifest scope".into()))?;
        let id = CredentialId::new();
        let path = self.path(&id);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        let write = options.open(&path).and_then(|mut file| {
            file.write_all(&request.secret)?;
            file.sync_all()
        });
        if let Err(primary) = write {
            let _ = fs::remove_file(&path);
            return Err(primary.into());
        }
        let mut credential = IssuedCredential {
            id,
            reality_id: reality.id.clone(),
            provider: request.provider,
            name: request.name,
            scope: configured.scope.clone(),
            expires_at: configured.expires_at,
            secret_ref: "hardknock-local-secret".into(),
            issued_at: Utc::now(),
            revoked_at: None,
        };
        if let Err(primary) = self.store.insert_issued_credential(&credential) {
            let _ = fs::remove_file(&path);
            return Err(primary);
        }
        if let Err(primary) = self.store.append_capability_event(&CapabilityEvent {
            id: crate::core::CapabilityEventId::new(),
            reality_id: reality.id.clone(),
            manifest_id: manifest.id.clone(),
            kind: CapabilityEventKind::CredentialIssued,
            request: None,
            reason: format!(
                "issued scoped {} credential {} without persisting raw secret",
                credential.provider, credential.name
            ),
            created_at: Utc::now(),
        }) {
            let _ = fs::remove_file(&path);
            credential.revoked_at = Some(Utc::now());
            let _ = self.store.revoke_issued_credential(&credential);
            return Err(primary);
        }
        Ok(credential)
    }

    fn revoke(&self, credential: &mut IssuedCredential) -> Result<()> {
        let path = self.path(&credential.id);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(Error::Io(error)),
        }
        credential.revoked_at = Some(Utc::now());
        self.store.revoke_issued_credential(credential)?;
        let manifest = self
            .store
            .effective_capability_manifest(&credential.reality_id)?;
        self.store.append_capability_event(&CapabilityEvent {
            id: crate::core::CapabilityEventId::new(),
            reality_id: credential.reality_id.clone(),
            manifest_id: manifest.id,
            kind: CapabilityEventKind::CredentialRevoked,
            request: None,
            reason: format!(
                "revoked scoped {} credential {} and removed local secret",
                credential.provider, credential.name
            ),
            created_at: Utc::now(),
        })?;
        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct SecretRedactor {
    secrets: Vec<Vec<u8>>,
}

impl SecretRedactor {
    pub fn new(secrets: impl IntoIterator<Item = Vec<u8>>) -> Self {
        let mut secrets: Vec<_> = secrets
            .into_iter()
            .filter(|secret| !secret.is_empty())
            .collect();
        secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        secrets.dedup();
        Self { secrets }
    }

    pub fn redact(&self, input: &[u8]) -> Vec<u8> {
        let mut output = input.to_vec();
        for secret in &self.secrets {
            output = replace_all(&output, secret, b"[REDACTED]");
        }
        output
    }

    pub fn including(&self, secrets: impl IntoIterator<Item = Vec<u8>>) -> Self {
        Self::new(self.secrets.iter().cloned().chain(secrets))
    }

    pub fn redact_file(&self, path: &Path, maximum: u64) -> Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > maximum {
            return Err(Error::Intervention(
                "Refusing to redact an unsafe or oversized output file".into(),
            ));
        }
        let redacted = self.redact(&fs::read(path)?);
        fs::write(path, redacted)?;
        Ok(())
    }
}

fn credential_environment_name(credential: &IssuedCredential) -> String {
    let suffix: String = format!("{}_{}", credential.provider, credential.name)
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("HARDKNOCK_CREDENTIAL_{suffix}")
}

fn replace_all(input: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    let mut cursor = 0;
    while cursor < input.len() {
        if input[cursor..].starts_with(needle) {
            output.extend_from_slice(replacement);
            cursor += needle.len();
        } else {
            output.push(input[cursor]);
            cursor += 1;
        }
    }
    output
}
