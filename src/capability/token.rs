// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::{Error, Result, core::CapabilityTokenId};
use chrono::{DateTime, Duration, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

const DOMAIN: &[u8] = b"hardknock-reality-capability-token-v1\0";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealityTokenOperation {
    Shell,
    FileRead,
    FileWrite,
    EffectPropose,
    EffectPrepare,
    EffectStatus,
    EffectDiscard,
    EffectCommit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealityCapabilityToken {
    pub id: CapabilityTokenId,
    pub reality_id: crate::core::RealityId,
    pub manifest_id: crate::core::CapabilityManifestId,
    pub manifest_hash: String,
    pub manifest_revision: u32,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub nonce: String,
    pub operations: Vec<RealityTokenOperation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignedRealityCapabilityToken {
    pub claims: RealityCapabilityToken,
    pub signature: String,
}

pub struct CapabilityTokenAuthority {
    key: SigningKey,
    pub key_path: PathBuf,
}

impl CapabilityTokenAuthority {
    pub fn load_or_create(home: &Path) -> Result<Self> {
        let directory = home.join("identity");
        fs::create_dir_all(&directory)?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        let key_path = directory.join("capability-token.key");
        let key = if key_path.exists() {
            let metadata = fs::symlink_metadata(&key_path)?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.permissions().mode() & 0o077 != 0
            {
                return Err(Error::Intervention(
                    "Capability token key must be a regular 0600 file".into(),
                ));
            }
            let mut bytes = [0_u8; 32];
            let mut file = File::open(&key_path)?;
            if file.metadata()?.len() != 32 {
                return Err(Error::InvalidInput(
                    "Capability token key has an invalid length".into(),
                ));
            }
            file.read_exact(&mut bytes)?;
            SigningKey::from_bytes(&bytes)
        } else {
            let mut bytes = [0_u8; 32];
            File::open("/dev/urandom")?.read_exact(&mut bytes)?;
            let mut options = OpenOptions::new();
            options.write(true).create_new(true).mode(0o600);
            let mut file = options.open(&key_path)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            SigningKey::from_bytes(&bytes)
        };
        Ok(Self { key, key_path })
    }

    pub fn issue(
        &self,
        reality: &crate::core::Reality,
        manifest: &CapabilityManifest,
        ttl: Duration,
    ) -> Result<SignedRealityCapabilityToken> {
        if reality.execution_boundary.frozen {
            return Err(Error::Intervention(
                "Frozen Reality cannot receive a capability token".into(),
            ));
        }
        let hash = manifest.hash()?;
        if reality.execution_boundary.manifest_id.as_ref() != Some(&manifest.id)
            || reality.execution_boundary.manifest_hash.as_deref() != Some(hash.as_str())
            || reality.execution_boundary.manifest_revision != manifest.revision
        {
            return Err(Error::Intervention(
                "Reality and capability manifest binding do not match".into(),
            ));
        }
        if ttl <= Duration::zero() || ttl > Duration::hours(24) {
            return Err(Error::InvalidInput(
                "Capability token TTL must be positive and at most 24 hours".into(),
            ));
        }
        let issued_at = Utc::now();
        let mut operations = vec![RealityTokenOperation::FileRead];
        if manifest.process.allow_exec {
            operations.push(RealityTokenOperation::Shell);
        }
        if !manifest.filesystem.writable.is_empty() {
            operations.push(RealityTokenOperation::FileWrite);
        }
        if manifest.effects.propose {
            operations.push(RealityTokenOperation::EffectPropose);
        }
        if manifest.effects.prepare {
            operations.push(RealityTokenOperation::EffectPrepare);
            operations.push(RealityTokenOperation::EffectDiscard);
        }
        operations.push(RealityTokenOperation::EffectStatus);
        if manifest.effects.commit {
            operations.push(RealityTokenOperation::EffectCommit);
        }
        let claims = RealityCapabilityToken {
            id: CapabilityTokenId::new(),
            reality_id: reality.id.clone(),
            manifest_id: manifest.id.clone(),
            manifest_hash: hash,
            manifest_revision: manifest.revision,
            issued_at,
            expires_at: issued_at + ttl,
            nonce: uuid::Uuid::new_v4().to_string(),
            operations,
        };
        Ok(SignedRealityCapabilityToken {
            signature: encode(&self.sign(&claims)?),
            claims,
        })
    }

    fn sign(&self, claims: &RealityCapabilityToken) -> Result<[u8; 64]> {
        let data = serde_json::to_vec(claims)?;
        let mut message = Vec::with_capacity(DOMAIN.len() + data.len());
        message.extend_from_slice(DOMAIN);
        message.extend_from_slice(&data);
        Ok(self.key.sign(&message).to_bytes())
    }

    pub fn verify(
        &self,
        token: &SignedRealityCapabilityToken,
        reality: &crate::core::Reality,
        manifest: &CapabilityManifest,
        operation: RealityTokenOperation,
    ) -> Result<()> {
        let signature = Signature::from_bytes(&decode::<64>(&token.signature)?);
        let data = serde_json::to_vec(&token.claims)?;
        let mut message = Vec::with_capacity(DOMAIN.len() + data.len());
        message.extend_from_slice(DOMAIN);
        message.extend_from_slice(&data);
        self.key
            .verifying_key()
            .verify(&message, &signature)
            .map_err(|_| Error::Intervention("Capability token signature is invalid".into()))?;
        let hash = manifest.hash()?;
        if token.claims.reality_id != reality.id
            || token.claims.manifest_id != manifest.id
            || token.claims.manifest_hash != hash
            || token.claims.manifest_revision != manifest.revision
            || reality.execution_boundary.manifest_hash.as_deref()
                != Some(token.claims.manifest_hash.as_str())
            || reality.execution_boundary.manifest_revision != token.claims.manifest_revision
            || reality.execution_boundary.frozen
        {
            return Err(Error::Intervention(
                "Capability token scope no longer matches this Reality".into(),
            ));
        }
        if token.claims.expires_at <= Utc::now() || token.claims.issued_at > Utc::now() {
            return Err(Error::Intervention(
                "Capability token is expired or not yet valid".into(),
            ));
        }
        if !token.claims.operations.contains(&operation) {
            return Err(Error::Intervention(
                "Capability token does not authorize this operation".into(),
            ));
        }
        Ok(())
    }
}

fn encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode<const N: usize>(value: &str) -> Result<[u8; N]> {
    if value.len() != N * 2 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::Intervention(
            "Capability token signature encoding is invalid".into(),
        ));
    }
    let mut result = [0_u8; N];
    #[allow(clippy::chunks_exact_to_as_chunks)]
    for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        let pair = std::str::from_utf8(chunk)
            .map_err(|_| Error::Intervention("Capability token is not UTF-8".into()))?;
        result[index] = u8::from_str_radix(pair, 16)
            .map_err(|_| Error::Intervention("Capability token signature is invalid".into()))?;
    }
    Ok(result)
}
