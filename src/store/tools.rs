// SPDX-License-Identifier: Apache-2.0

use super::Store;
use crate::{
    Error, Result,
    core::{ExecutionAttestationId, MicroSandboxId, RealityId, ToolId},
    tool::{
        ExecutionAttestation, MicroSandbox, ToolDefinition, ToolLifecycleEvent, ToolSource,
        ToolTrust,
    },
};
use rusqlite::{OptionalExtension, params};

pub trait ToolStore {
    fn insert_tool_definition(&self, tool: &ToolDefinition) -> Result<()>;
    fn tool_definition(&self, id: &ToolId) -> Result<ToolDefinition>;
    fn tool_definition_by_name(&self, name: &str) -> Result<ToolDefinition>;
    fn tool_definitions(&self, include_disabled: bool) -> Result<Vec<ToolDefinition>>;
    fn disable_tool_definition(&self, id: &ToolId) -> Result<()>;
    fn insert_micro_sandbox(&self, sandbox: &MicroSandbox) -> Result<()>;
    fn update_micro_sandbox(&self, sandbox: &MicroSandbox) -> Result<()>;
    fn micro_sandbox(&self, id: &MicroSandboxId) -> Result<MicroSandbox>;
    fn micro_sandboxes(&self) -> Result<Vec<MicroSandbox>>;
    fn insert_execution_attestation(&self, attestation: &ExecutionAttestation) -> Result<()>;
    fn execution_attestation(&self, id: &ExecutionAttestationId) -> Result<ExecutionAttestation>;
    fn execution_attestations(
        &self,
        reality: Option<&RealityId>,
    ) -> Result<Vec<ExecutionAttestation>>;
    fn insert_tool_lifecycle_event(&self, event: &ToolLifecycleEvent) -> Result<()>;
    fn tool_lifecycle_events(
        &self,
        sandbox: Option<&MicroSandboxId>,
    ) -> Result<Vec<ToolLifecycleEvent>>;
}

impl ToolStore for Store {
    fn insert_tool_definition(&self, tool: &ToolDefinition) -> Result<()> {
        tool.validate()?;
        let mut stored = tool.clone();
        if matches!(
            stored.provenance.source,
            ToolSource::Imported | ToolSource::Federated
        ) {
            stored.disabled = true;
            stored.trust = if stored.integrity.signature.is_some() {
                ToolTrust::SignedUnknown
            } else {
                ToolTrust::Unsigned
            };
        }
        self.connection.execute(
            "INSERT INTO tool_definitions(id,name,version,manifest_hash,artifact_hash,trust,disabled,registered_at,data) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![stored.id.to_string(), stored.name, stored.version, stored.integrity.manifest_hash, stored.integrity.artifact_hash, serde_json::to_value(stored.trust)?.as_str().unwrap_or("unsigned"), stored.disabled, stored.provenance.registered_at.to_rfc3339(), serde_json::to_string(&stored)?],
        )?;
        Ok(())
    }

    fn tool_definition(&self, id: &ToolId) -> Result<ToolDefinition> {
        let data: Option<(bool, String, String)> = self
            .connection
            .query_row(
                "SELECT disabled,trust,data FROM tool_definitions WHERE id=?1",
                [id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let (disabled, trust, data) =
            data.ok_or_else(|| Error::NotFound(format!("Tool {id} not found")))?;
        stored_tool(&data, disabled, &trust)
    }

    fn tool_definition_by_name(&self, name: &str) -> Result<ToolDefinition> {
        let data: Option<(bool, String, String)> = self.connection.query_row("SELECT disabled,trust,data FROM tool_definitions WHERE name=?1 AND disabled=0 ORDER BY registered_at DESC LIMIT 1", [name], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional()?;
        let (disabled, trust, data) =
            data.ok_or_else(|| Error::NotFound(format!("Enabled tool {name} not found")))?;
        stored_tool(&data, disabled, &trust)
    }

    fn tool_definitions(&self, include_disabled: bool) -> Result<Vec<ToolDefinition>> {
        let mut statement = self.connection.prepare(
            "SELECT disabled,trust,data FROM tool_definitions WHERE (?1 OR disabled=0) ORDER BY name,version,id",
        )?;
        statement
            .query_map([include_disabled], |row| {
                Ok((
                    row.get::<_, bool>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .map(|row| {
                let (disabled, trust, data) = row?;
                stored_tool(&data, disabled, &trust)
            })
            .collect()
    }

    fn disable_tool_definition(&self, id: &ToolId) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE tool_definitions SET disabled=1,trust='blocked' WHERE id=?1",
            [id.to_string()],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(Error::NotFound(format!("Tool {id} not found")))
        }
    }

    fn insert_micro_sandbox(&self, sandbox: &MicroSandbox) -> Result<()> {
        self.connection.execute("INSERT INTO micro_sandboxes(id,reality_id,tool_id,created_at,destroyed_at,data) VALUES(?1,?2,?3,?4,?5,?6)", params![sandbox.id.to_string(), sandbox.reality_id.to_string(), sandbox.tool_id.to_string(), sandbox.created_at.to_rfc3339(), sandbox.destroyed_at.map(|time| time.to_rfc3339()), serde_json::to_string(sandbox)?])?;
        Ok(())
    }

    fn update_micro_sandbox(&self, sandbox: &MicroSandbox) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE micro_sandboxes SET destroyed_at=?2,data=?3 WHERE id=?1",
            params![
                sandbox.id.to_string(),
                sandbox.destroyed_at.map(|time| time.to_rfc3339()),
                serde_json::to_string(sandbox)?
            ],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(Error::NotFound(format!(
                "Micro-sandbox {} not found",
                sandbox.id
            )))
        }
    }

    fn micro_sandbox(&self, id: &MicroSandboxId) -> Result<MicroSandbox> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM micro_sandboxes WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(serde_json::from_str(&data.ok_or_else(|| {
            Error::NotFound(format!("Micro-sandbox {id} not found"))
        })?)?)
    }

    fn micro_sandboxes(&self) -> Result<Vec<MicroSandbox>> {
        self.list("SELECT data FROM micro_sandboxes ORDER BY created_at,id")
    }

    fn insert_execution_attestation(&self, attestation: &ExecutionAttestation) -> Result<()> {
        self.connection.execute("INSERT INTO execution_attestations(id,reality_id,sandbox_id,tool_id,attestation_hash,created_at,data) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![attestation.id.to_string(), attestation.reality_id.to_string(), attestation.sandbox_id.to_string(), attestation.tool.id.to_string(), attestation.attestation_hash()?, attestation.completed_at.to_rfc3339(), serde_json::to_string(attestation)?])?;
        Ok(())
    }

    fn execution_attestation(&self, id: &ExecutionAttestationId) -> Result<ExecutionAttestation> {
        let data: Option<(String, String)> = self
            .connection
            .query_row(
                "SELECT attestation_hash,data FROM execution_attestations WHERE id=?1",
                [id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (hash, data) =
            data.ok_or_else(|| Error::NotFound(format!("Attestation {id} not found")))?;
        let mut attestation: ExecutionAttestation = serde_json::from_str(&data)?;
        attestation.recorded_hash = Some(hash);
        Ok(attestation)
    }

    fn execution_attestations(
        &self,
        reality: Option<&RealityId>,
    ) -> Result<Vec<ExecutionAttestation>> {
        let reality = reality.map(ToString::to_string);
        let mut statement = self.connection.prepare("SELECT attestation_hash,data FROM execution_attestations WHERE (?1 IS NULL OR reality_id=?1) ORDER BY completed_at,id")?;
        statement
            .query_map([reality], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .map(|row| {
                let (hash, data) = row?;
                let mut attestation: ExecutionAttestation = serde_json::from_str(&data)?;
                attestation.recorded_hash = Some(hash);
                Ok(attestation)
            })
            .collect()
    }

    fn insert_tool_lifecycle_event(&self, event: &ToolLifecycleEvent) -> Result<()> {
        self.connection.execute("INSERT INTO tool_lifecycle_events(id,tool_id,sandbox_id,event,created_at,data) VALUES(?1,?2,?3,?4,?5,?6)", params![event.id, event.tool_id.as_ref().map(ToString::to_string), event.sandbox_id.as_ref().map(ToString::to_string), serde_json::to_value(event.kind)?.as_str().unwrap_or("unknown"), event.created_at.to_rfc3339(), serde_json::to_string(event)?])?;
        Ok(())
    }

    fn tool_lifecycle_events(
        &self,
        sandbox: Option<&MicroSandboxId>,
    ) -> Result<Vec<ToolLifecycleEvent>> {
        let sandbox = sandbox.map(ToString::to_string);
        let mut statement = self.connection.prepare("SELECT data FROM tool_lifecycle_events WHERE (?1 IS NULL OR sandbox_id=?1) ORDER BY created_at,id")?;
        statement
            .query_map([sandbox], |row| row.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }
}

fn stored_tool(data: &str, disabled: bool, trust: &str) -> Result<ToolDefinition> {
    let mut tool: ToolDefinition = serde_json::from_str(data)?;
    tool.disabled = disabled;
    tool.trust = serde_json::from_value(serde_json::Value::String(trust.to_owned()))?;
    Ok(tool)
}
