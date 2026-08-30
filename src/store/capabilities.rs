// SPDX-License-Identifier: Apache-2.0

use super::Store;
use crate::{
    Error, Result,
    capability::{
        CapabilityEvent, CapabilityManifest, IssuedCredential, SecurityBenchmarkReport,
        SignedRealityCapabilityToken,
    },
    core::{CapabilityManifestId, RealityId},
};
use chrono::Utc;
use rusqlite::{OptionalExtension, params, params_from_iter};
use serde::{Serialize, de::DeserializeOwned};

pub trait CapabilityStore {
    fn insert_capability_manifest(
        &self,
        reality_id: &RealityId,
        manifest: &CapabilityManifest,
    ) -> Result<()>;
    fn capability_manifest(&self, id: &CapabilityManifestId) -> Result<CapabilityManifest>;
    fn effective_capability_manifest(&self, reality_id: &RealityId) -> Result<CapabilityManifest>;
    fn append_capability_event(&self, event: &CapabilityEvent) -> Result<()>;
    fn capability_events(&self, reality_id: Option<&RealityId>) -> Result<Vec<CapabilityEvent>>;
    fn put_provider_runtime<T: Serialize>(
        &self,
        reality_id: &RealityId,
        provider: &str,
        runtime: &T,
    ) -> Result<()>;
    fn provider_runtime<T: DeserializeOwned>(&self, reality_id: &RealityId) -> Result<T>;
    fn insert_issued_credential(&self, credential: &IssuedCredential) -> Result<()>;
    fn issued_credentials(&self, reality_id: &RealityId) -> Result<Vec<IssuedCredential>>;
    fn revoke_issued_credential(&self, credential: &IssuedCredential) -> Result<()>;
    fn audit_capability_token(&self, token: &SignedRealityCapabilityToken) -> Result<()>;
    fn revoke_capability_tokens(&self, reality_id: &RealityId) -> Result<()>;
    fn capability_token_revoked(&self, token_hash: &str) -> Result<bool>;
    fn insert_capability_benchmark(&self, report: &SecurityBenchmarkReport) -> Result<()>;
    fn latest_capability_benchmark(&self) -> Result<Option<SecurityBenchmarkReport>>;
}

impl CapabilityStore for Store {
    fn insert_capability_manifest(
        &self,
        reality_id: &RealityId,
        manifest: &CapabilityManifest,
    ) -> Result<()> {
        let hash = manifest.hash()?;
        let tx = self.connection.unchecked_transaction()?;
        let data = serde_json::to_string(manifest)?;
        tx.execute(
            "INSERT OR IGNORE INTO capability_manifests(id,profile,revision,manifest_hash,created_at,data) VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                manifest.id.to_string(),
                manifest.profile,
                manifest.revision,
                &hash,
                manifest.created_at.to_rfc3339(),
                &data
            ],
        )?;
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT manifest_hash,data FROM capability_manifests WHERE id=?1",
                [manifest.id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if existing.as_ref() != Some(&(hash, data)) {
            return Err(Error::Intervention(
                "Capability manifest identifier/hash collision".into(),
            ));
        }
        tx.execute(
            "UPDATE reality_manifest_history SET revoked_at=?2 WHERE reality_id=?1 AND revoked_at IS NULL",
            params![reality_id.to_string(), Utc::now().to_rfc3339()],
        )?;
        tx.execute(
            "INSERT INTO reality_manifest_history(reality_id,manifest_id,revision,effective_at) VALUES(?1,?2,?3,?4)",
            params![
                reality_id.to_string(),
                manifest.id.to_string(),
                manifest.revision,
                Utc::now().to_rfc3339()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn capability_manifest(&self, id: &CapabilityManifestId) -> Result<CapabilityManifest> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM capability_manifests WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(serde_json::from_str(&data.ok_or_else(|| {
            Error::NotFound(format!("Capability manifest {id} not found"))
        })?)?)
    }

    fn effective_capability_manifest(&self, reality_id: &RealityId) -> Result<CapabilityManifest> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT m.data FROM reality_manifest_history h JOIN capability_manifests m ON m.id=h.manifest_id WHERE h.reality_id=?1 AND h.revoked_at IS NULL ORDER BY h.revision DESC LIMIT 1",
                [reality_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(serde_json::from_str(&data.ok_or_else(|| {
            Error::NotFound(format!(
                "Reality {reality_id} has no effective capability manifest"
            ))
        })?)?)
    }

    fn append_capability_event(&self, event: &CapabilityEvent) -> Result<()> {
        self.connection.execute(
            "INSERT INTO capability_events(id,reality_id,manifest_id,kind,created_at,data) VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                event.id.to_string(),
                event.reality_id.to_string(),
                event.manifest_id.to_string(),
                format!("{:?}", event.kind).to_ascii_lowercase(),
                event.created_at.to_rfc3339(),
                serde_json::to_string(event)?
            ],
        )?;
        Ok(())
    }

    fn capability_events(&self, reality_id: Option<&RealityId>) -> Result<Vec<CapabilityEvent>> {
        let (sql, parameters): (&str, Vec<String>) = if let Some(id) = reality_id {
            (
                "SELECT data FROM capability_events WHERE reality_id=?1 ORDER BY created_at,id",
                vec![id.to_string()],
            )
        } else {
            (
                "SELECT data FROM capability_events ORDER BY created_at,id",
                vec![],
            )
        };
        let mut statement = self.connection.prepare(sql)?;
        statement
            .query_map(params_from_iter(parameters), |row| row.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }

    fn put_provider_runtime<T: Serialize>(
        &self,
        reality_id: &RealityId,
        provider: &str,
        runtime: &T,
    ) -> Result<()> {
        self.connection.execute(
            "INSERT INTO reality_provider_runtime(reality_id,provider,created_at,data) VALUES(?1,?2,?3,?4)",
            params![
                reality_id.to_string(),
                provider,
                Utc::now().to_rfc3339(),
                serde_json::to_string(runtime)?
            ],
        )?;
        Ok(())
    }

    fn provider_runtime<T: DeserializeOwned>(&self, reality_id: &RealityId) -> Result<T> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM reality_provider_runtime WHERE reality_id=?1",
                [reality_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(serde_json::from_str(&data.ok_or_else(|| {
            Error::NotFound(format!(
                "Provider runtime for Reality {reality_id} not found"
            ))
        })?)?)
    }

    fn insert_issued_credential(&self, credential: &IssuedCredential) -> Result<()> {
        self.connection.execute(
            "INSERT INTO issued_credentials(id,reality_id,provider,issued_at,revoked_at,data) VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                credential.id.to_string(),
                credential.reality_id.to_string(),
                credential.provider,
                credential.issued_at.to_rfc3339(),
                credential.revoked_at.map(|value| value.to_rfc3339()),
                serde_json::to_string(credential)?
            ],
        )?;
        Ok(())
    }

    fn issued_credentials(&self, reality_id: &RealityId) -> Result<Vec<IssuedCredential>> {
        let mut statement = self.connection.prepare(
            "SELECT data FROM issued_credentials WHERE reality_id=?1 ORDER BY issued_at,id",
        )?;
        statement
            .query_map([reality_id.to_string()], |row| row.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }

    fn revoke_issued_credential(&self, credential: &IssuedCredential) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE issued_credentials SET revoked_at=?2,data=?3 WHERE id=?1 AND revoked_at IS NULL",
            params![
                credential.id.to_string(),
                credential.revoked_at.map(|value| value.to_rfc3339()),
                serde_json::to_string(credential)?
            ],
        )?;
        if changed != 1 {
            return Err(Error::Intervention(format!(
                "Credential {} was already revoked or missing",
                credential.id
            )));
        }
        Ok(())
    }

    fn audit_capability_token(&self, token: &SignedRealityCapabilityToken) -> Result<()> {
        self.connection.execute(
            "INSERT INTO capability_token_audit(token_hash,reality_id,manifest_id,expires_at) VALUES(?1,?2,?3,?4)",
            params![
                token_hash(token)?,
                token.claims.reality_id.to_string(),
                token.claims.manifest_id.to_string(),
                token.claims.expires_at.to_rfc3339()
            ],
        )?;
        Ok(())
    }

    fn revoke_capability_tokens(&self, reality_id: &RealityId) -> Result<()> {
        self.connection.execute(
            "UPDATE capability_token_audit SET revoked_at=?2 WHERE reality_id=?1 AND revoked_at IS NULL",
            params![reality_id.to_string(), Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    fn capability_token_revoked(&self, hash: &str) -> Result<bool> {
        let revoked: Option<Option<String>> = self
            .connection
            .query_row(
                "SELECT revoked_at FROM capability_token_audit WHERE token_hash=?1",
                [hash],
                |row| row.get(0),
            )
            .optional()?;
        Ok(revoked.is_none_or(|value| value.is_some()))
    }

    fn insert_capability_benchmark(&self, report: &SecurityBenchmarkReport) -> Result<()> {
        self.connection.execute(
            "INSERT INTO capability_benchmark_runs(id,created_at,data) VALUES(?1,?2,?3)",
            params![
                report.id.to_string(),
                report.created_at.to_rfc3339(),
                serde_json::to_string(report)?
            ],
        )?;
        Ok(())
    }

    fn latest_capability_benchmark(&self) -> Result<Option<SecurityBenchmarkReport>> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM capability_benchmark_runs ORDER BY created_at DESC,id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        data.map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose()
    }
}

pub fn token_hash(token: &SignedRealityCapabilityToken) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(token)?)
        .to_hex()
        .to_string())
}
