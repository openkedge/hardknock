// SPDX-License-Identifier: Apache-2.0
use super::Store;
use crate::{
    Error, Result,
    core::KnowledgeHierarchyId,
    hierarchy::{KnowledgeHierarchy, validate_hierarchy},
};
use rusqlite::{OptionalExtension, params};
impl Store {
    /// Atomic revision-checked aggregate. Resolved knowledge is never stored.
    pub fn save_knowledge_hierarchy(&self, h: &KnowledgeHierarchy) -> Result<()> {
        let report = validate_hierarchy(h);
        if !report.valid {
            return Err(Error::InvalidInput(serde_json::to_string(&report)?));
        }
        let revision = i64::try_from(h.revision)
            .map_err(|_| Error::InvalidInput("Hierarchy revision overflow".into()))?;
        if revision < 1 {
            return Err(Error::InvalidInput(
                "Hierarchy revision must be positive".into(),
            ));
        }
        let tx = if self.connection.is_autocommit() {
            Some(rusqlite::Transaction::new_unchecked(
                &self.connection,
                rusqlite::TransactionBehavior::Immediate,
            )?)
        } else {
            None
        };
        let changed=self.connection.execute("INSERT INTO knowledge_hierarchies(id,revision,data) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,data=excluded.data WHERE knowledge_hierarchies.revision=excluded.revision-1",params![h.id.to_string(),revision,serde_json::to_string(h)?])?;
        if changed != 1 {
            return Err(Error::InvalidInput(
                "Hierarchy revision must advance exactly once".into(),
            ));
        }
        self.record_hierarchy_health(h)?;
        if let Some(tx) = tx {
            tx.commit()?;
        }
        Ok(())
    }
    pub fn knowledge_hierarchy(&self, id: &KnowledgeHierarchyId) -> Result<KnowledgeHierarchy> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM knowledge_hierarchies WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(serde_json::from_str(&data.ok_or_else(|| {
            Error::NotFound(format!("Hierarchy {id} not found"))
        })?)?)
    }
    pub fn knowledge_hierarchies(&self) -> Result<Vec<KnowledgeHierarchy>> {
        self.connection
            .prepare("SELECT data FROM knowledge_hierarchies ORDER BY id")?
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
}
