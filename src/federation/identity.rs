// SPDX-License-Identifier: Apache-2.0
use super::{
    BUNDLE_SCHEMA_V1, ExperienceBundle, ExperienceNode, ExperienceNodeId, ExperienceNodeType,
    NodeCapabilities, NodePublicIdentity, SIGNING_DOMAIN, SignedExperienceBundle,
};
use crate::{Error, Result};
use chrono::Utc;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

const PRIVATE_HEADER: &str = "hardknock-ed25519-private-v1 ";
const PUBLIC_HEADER: &str = "hardknock-ed25519-public-v1 ";

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        value.push(HEX[(byte >> 4) as usize] as char);
        value.push(HEX[(byte & 15) as usize] as char);
    }
    value
}
fn decode_hex<const N: usize>(value: &str) -> Result<[u8; N]> {
    if value.len() != N * 2 {
        return Err(Error::InvalidInput(format!(
            "Expected {} bytes of lowercase hexadecimal data",
            N
        )));
    }
    let mut result = [0u8; N];
    for (slot, pair) in result.iter_mut().zip(value.as_bytes().as_chunks::<2>().0) {
        let digit = |b: u8| match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            _ => None,
        };
        *slot = (digit(pair[0]).ok_or_else(|| {
            Error::InvalidInput("Invalid lowercase hexadecimal key material".into())
        })? << 4)
            | digit(pair[1]).ok_or_else(|| {
                Error::InvalidInput("Invalid lowercase hexadecimal key material".into())
            })?;
    }
    Ok(result)
}
pub(crate) fn node_id(public: &[u8; 32]) -> Result<ExperienceNodeId> {
    ExperienceNodeId::from_digest(blake3::hash(public).to_hex().as_ref())
}

pub struct NodeIdentity {
    pub node: ExperienceNode,
    signing_key: SigningKey,
    pub private_key_path: PathBuf,
    pub public_key_path: PathBuf,
}
impl NodeIdentity {
    pub fn load_or_create(home: &Path, name: &str, node_type: ExperienceNodeType) -> Result<Self> {
        if name.trim().is_empty() || name.len() > 120 || name.contains(['\0', '\n', '\r']) {
            return Err(Error::InvalidInput(
                "Node name must be 1–120 single-line characters".into(),
            ));
        }
        let directory = home.join("identity");
        fs::create_dir_all(&directory)?;
        if fs::symlink_metadata(&directory)?.file_type().is_symlink() {
            return Err(Error::Intervention(
                "Identity directory must not be a symlink".into(),
            ));
        }
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        let private_key_path = directory.join("node.key");
        let public_key_path = directory.join("node.pub");
        let signing_key = if private_key_path.exists() {
            let metadata = fs::symlink_metadata(&private_key_path)?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.permissions().mode() & 0o077 != 0
            {
                return Err(Error::Intervention(
                    "Node private key must be a regular 0600 file".into(),
                ));
            }
            let data = fs::read_to_string(&private_key_path)?;
            let value = data
                .trim_end()
                .strip_prefix(PRIVATE_HEADER)
                .ok_or_else(|| Error::InvalidInput("Unsupported node private key format".into()))?;
            SigningKey::from_bytes(&decode_hex::<32>(value)?)
        } else {
            let mut bytes = [0u8; 32];
            File::open("/dev/urandom")?.read_exact(&mut bytes)?;
            let key = SigningKey::from_bytes(&bytes);
            let mut options = OpenOptions::new();
            options.write(true).create_new(true).mode(0o600);
            let mut file = options.open(&private_key_path)?;
            writeln!(file, "{PRIVATE_HEADER}{}", encode_hex(&bytes))?;
            file.sync_all()?;
            key
        };
        let verifying = signing_key.verifying_key();
        if public_key_path.exists() {
            let stored = read_public_key(&public_key_path)?;
            if stored != verifying {
                return Err(Error::Intervention(
                    "Node public key does not match private key".into(),
                ));
            }
        } else {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true).mode(0o644);
            let mut file = options.open(&public_key_path)?;
            writeln!(file, "{PUBLIC_HEADER}{}", encode_hex(verifying.as_bytes()))?;
            file.sync_all()?;
        }
        let id = node_id(verifying.as_bytes())?;
        let node = ExperienceNode {
            id,
            name: name.into(),
            node_type,
            public_identity: NodePublicIdentity {
                algorithm: "ed25519".into(),
                public_key: encode_hex(verifying.as_bytes()),
            },
            capabilities: NodeCapabilities {
                schemas: vec![BUNDLE_SCHEMA_V1.into()],
                transports: vec!["filesystem".into()],
            },
            created_at: Utc::now(),
        };
        Ok(Self {
            node,
            signing_key,
            private_key_path,
            public_key_path,
        })
    }
    pub fn sign(&self, bundle: ExperienceBundle) -> Result<SignedExperienceBundle> {
        if bundle.manifest.producer != self.node.id {
            return Err(Error::InvalidInput(
                "Bundle producer does not match signing node".into(),
            ));
        }
        let canonical = bundle.canonical_bytes()?;
        let payload_hash = blake3::hash(&canonical).to_hex().to_string();
        let mut message = Vec::with_capacity(SIGNING_DOMAIN.len() + canonical.len());
        message.extend_from_slice(SIGNING_DOMAIN);
        message.extend_from_slice(&canonical);
        let signature = self.signing_key.sign(&message);
        Ok(SignedExperienceBundle {
            manifest: bundle.manifest.clone(),
            payload_hash,
            signer: self.node.id.clone(),
            signer_public_key: public_key_hex(&self.signing_key.verifying_key()),
            signature: encode_hex(&signature.to_bytes()),
            bundle,
        })
    }
}

pub fn read_public_key(path: &Path) -> Result<VerifyingKey> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024 {
        return Err(Error::InvalidInput(
            "Peer public key must be a small regular file".into(),
        ));
    }
    let data = fs::read_to_string(path)?;
    let value = data
        .trim_end()
        .strip_prefix(PUBLIC_HEADER)
        .unwrap_or(data.trim());
    VerifyingKey::from_bytes(&decode_hex::<32>(value)?)
        .map_err(|_| Error::InvalidInput("Invalid Ed25519 public key".into()))
}
pub fn public_key_hex(key: &VerifyingKey) -> String {
    encode_hex(key.as_bytes())
}
pub fn parse_public_key(value: &str) -> Result<VerifyingKey> {
    VerifyingKey::from_bytes(&decode_hex::<32>(value)?)
        .map_err(|_| Error::InvalidInput("Invalid Ed25519 public key".into()))
}

pub fn verify_signed_bundle(signed: &SignedExperienceBundle, key: &VerifyingKey) -> Result<()> {
    if signed.signer != signed.manifest.producer || signed.manifest != signed.bundle.manifest {
        return Err(Error::InvalidInput(
            "Signed manifest or signer mismatch".into(),
        ));
    }
    if node_id(key.as_bytes())? != signed.signer {
        return Err(Error::InvalidInput(
            "Signing key does not identify declared producer".into(),
        ));
    }
    let canonical = signed.bundle.canonical_bytes()?;
    if blake3::hash(&canonical).to_hex().as_str() != signed.payload_hash {
        return Err(Error::InvalidInput("Bundle payload hash mismatch".into()));
    }
    let mut message = Vec::with_capacity(SIGNING_DOMAIN.len() + canonical.len());
    message.extend_from_slice(SIGNING_DOMAIN);
    message.extend_from_slice(&canonical);
    let signature = Signature::from_bytes(&decode_hex::<64>(&signed.signature)?);
    key.verify(&message, &signature)
        .map_err(|_| Error::InvalidInput("Bundle signature invalid".into()))
}

pub fn embedded_verifying_key(signed: &SignedExperienceBundle) -> Result<VerifyingKey> {
    let key = parse_public_key(&signed.signer_public_key)?;
    if node_id(key.as_bytes())? != signed.signer {
        return Err(Error::InvalidInput(
            "Embedded public key does not identify declared signer".into(),
        ));
    }
    Ok(key)
}
