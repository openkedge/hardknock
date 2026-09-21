// SPDX-License-Identifier: Apache-2.0
//! Transport moves signed envelopes. Import semantics remain in the store layer.

use super::{PeerEndpoint, SyncCursor, SyncEnvelope, SyncPeer};
use crate::{Error, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncTransportReceipt {
    pub envelope: crate::core::SyncEnvelopeId,
    pub location: String,
    pub bytes: u64,
    pub recorded_at: DateTime<Utc>,
}

pub trait SyncTransport {
    fn push(&self, peer: &SyncPeer, envelope: &SyncEnvelope) -> Result<SyncTransportReceipt>;
    fn pull(&self, peer: &SyncPeer, cursor: Option<&SyncCursor>) -> Result<Vec<SyncEnvelope>>;
}

#[derive(Clone, Debug)]
pub struct FilesystemSyncTransport {
    max_envelope_bytes: u64,
}

impl FilesystemSyncTransport {
    pub fn new(max_envelope_bytes: u64) -> Result<Self> {
        if !(1024..=1024 * 1024 * 1024).contains(&max_envelope_bytes) {
            return Err(Error::InvalidInput(
                "Sync envelope byte limit is out of range".into(),
            ));
        }
        Ok(Self { max_envelope_bytes })
    }

    fn root(peer: &SyncPeer) -> Result<PathBuf> {
        let PeerEndpoint::Filesystem(path) = &peer.endpoint else {
            return Err(Error::InvalidInput(
                "Filesystem transport requires a filesystem peer endpoint".into(),
            ));
        };
        let root = Path::new(path);
        if root.exists() && fs::symlink_metadata(root)?.file_type().is_symlink() {
            return Err(Error::Intervention(
                "Sync repository must not be a symlink".into(),
            ));
        }
        fs::create_dir_all(root)?;
        let envelopes = root.join("envelopes");
        if envelopes.exists() && fs::symlink_metadata(&envelopes)?.file_type().is_symlink() {
            return Err(Error::Intervention(
                "Sync envelope directory must not be a symlink".into(),
            ));
        }
        fs::create_dir_all(&envelopes)?;
        Ok(root.canonicalize()?)
    }
}

impl SyncTransport for FilesystemSyncTransport {
    fn push(&self, peer: &SyncPeer, envelope: &SyncEnvelope) -> Result<SyncTransportReceipt> {
        let root = Self::root(peer)?;
        let bytes = serde_json::to_vec_pretty(envelope)?;
        if bytes.len() as u64 > self.max_envelope_bytes {
            return Err(Error::InvalidInput(
                "Sync envelope size limit exceeded".into(),
            ));
        }
        let file = format!(
            "{}-{}.hksync",
            envelope.created_at.timestamp_micros(),
            envelope.id
        );
        let destination = root.join("envelopes").join(file);
        if destination.exists() {
            if fs::read(&destination)? == bytes {
                return Ok(SyncTransportReceipt {
                    envelope: envelope.id.clone(),
                    location: destination.display().to_string(),
                    bytes: bytes.len() as u64,
                    recorded_at: Utc::now(),
                });
            }
            return Err(Error::InvalidInput(
                "Immutable sync envelope path contains different data".into(),
            ));
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        let mut output = options.open(&destination)?;
        output.write_all(&bytes)?;
        output.sync_all()?;
        Ok(SyncTransportReceipt {
            envelope: envelope.id.clone(),
            location: destination.display().to_string(),
            bytes: bytes.len() as u64,
            recorded_at: Utc::now(),
        })
    }

    fn pull(&self, peer: &SyncPeer, cursor: Option<&SyncCursor>) -> Result<Vec<SyncEnvelope>> {
        let root = Self::root(peer)?;
        let mut files = fs::read_dir(root.join("envelopes"))?
            .map(|entry| entry.map(|value| value.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        files.sort();
        let position = cursor.map(|value| value.position.as_str());
        files
            .into_iter()
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.ends_with(".hksync") && position.is_none_or(|value| name > value)
                    })
            })
            .map(|path| {
                let metadata = fs::symlink_metadata(&path)?;
                if !metadata.is_file()
                    || metadata.file_type().is_symlink()
                    || metadata.len() > self.max_envelope_bytes
                {
                    return Err(Error::InvalidInput(
                        "Sync envelope is not a bounded regular file".into(),
                    ));
                }
                Ok(serde_json::from_slice(&fs::read(path)?)?)
            })
            .collect()
    }
}
