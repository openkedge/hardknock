// SPDX-License-Identifier: Apache-2.0

use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::de::DeserializeOwned;

use crate::{
    Error, Result,
    core::{ArtifactRef, ExecutionId, ExecutionRecord, Reality, RealityId},
};

mod capabilities;
mod effects;
pub use capabilities::{CapabilityStore, token_hash};
mod experiences;
mod experiments;
pub use effects::EffectStore;
mod federation;
pub use experiences::{ExperienceQuery, ExperienceStore, ExperienceSummary};
pub use experiments::ExperimentStore;
mod bridge;
mod curriculum;
mod development;
mod learning;
mod resilience;
mod transfer;
pub use curriculum::CurriculumStore;
pub use learning::{LessonQuery, LessonStore, LessonSummary};

pub struct Store {
    pub home: PathBuf,
    connection: Connection,
}

impl Store {
    pub fn open(home: &Path) -> Result<Self> {
        if home.exists() {
            for entry in fs::read_dir(home)? {
                let name = entry?.file_name();
                if ![
                    "hardknock.db",
                    "hardknock.db-shm",
                    "hardknock.db-wal",
                    "artifacts",
                    "realities",
                    "logs",
                    "locks",
                    "config.toml",
                    "fixtures",
                    "run",
                    "integrations",
                    "identity",
                    "federation",
                    "effects",
                ]
                .iter()
                .any(|allowed| name == *allowed)
                {
                    return Err(Error::Intervention("HARDKNOCK_HOME must be a dedicated empty directory or an existing Hardknock data directory.".into()));
                }
            }
        }
        fs::create_dir_all(home)?;
        let home = home.canonicalize()?;
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700))?;
        for child in [
            "artifacts",
            "realities",
            "logs",
            "locks",
            "fixtures",
            "run",
            "integrations",
            "identity",
            "federation",
            "effects",
        ] {
            if fs::symlink_metadata(home.join(child)).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(Error::Intervention(
                    "Hardknock data subdirectories must not be symlinks.".into(),
                ));
            }
            fs::create_dir_all(home.join(child))?;
        }
        let db = home.join("hardknock.db");
        if fs::symlink_metadata(&db).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(Error::Intervention(
                "Hardknock database must not be a symlink.".into(),
            ));
        }
        let mut connection = Connection::open(&db)?;
        fs::set_permissions(db, fs::Permissions::from_mode(0o600))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;")?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);")?;
        let version: i64 = tx.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version > 12 {
            return Err(Error::Intervention(
                "Database was created by a newer Hardknock; upgrade the CLI.".into(),
            ));
        }
        if version < 1 {
            tx.execute_batch(include_str!("../migrations/001_substrate.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (1)", [])?;
        }
        if version < 2 {
            tx.execute_batch(include_str!("../migrations/002_experiences.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (2)", [])?;
        }
        if version < 3 {
            tx.execute_batch(include_str!("../migrations/003_learning.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (3)", [])?;
        }
        if version < 4 {
            tx.execute_batch(include_str!("../migrations/004_transfer.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (4)", [])?;
        }
        if version < 5 {
            tx.execute_batch(include_str!("../migrations/005_resilience.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (5)", [])?;
        }
        if version < 6 {
            tx.execute_batch(include_str!("../migrations/006_bridge.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (6)", [])?;
        }
        if version < 7 {
            tx.execute_batch(include_str!("../migrations/007_agent_experiments.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (7)", [])?;
        }
        if version < 8 {
            tx.execute_batch(include_str!("../migrations/008_curriculum.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (8)", [])?;
        }
        if version < 9 {
            tx.execute_batch(include_str!("../migrations/009_development.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (9)", [])?;
        }
        if version < 10 {
            tx.execute_batch(include_str!("../migrations/010_federation.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (10)", [])?;
        }
        if version < 11 {
            tx.execute_batch(include_str!("../migrations/011_effects.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (11)", [])?;
        }
        if version < 12 {
            tx.execute_batch(include_str!("../migrations/012_capabilities.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (12)", [])?;
        }
        tx.commit()?;
        tracing::debug!("SQLite migrations ready");
        Ok(Self { home, connection })
    }

    pub fn insert_reality(&self, reality: &Reality) -> Result<()> {
        self.connection.execute(
            "INSERT INTO realities(id, created_at, data) VALUES (?1, ?2, ?3)",
            params![
                reality.id.to_string(),
                reality.created_at.to_rfc3339(),
                serde_json::to_string(reality)?
            ],
        )?;
        Ok(())
    }

    pub fn update_reality(&self, reality: &Reality) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE realities SET data=?2 WHERE id=?1",
            params![reality.id.to_string(), serde_json::to_string(reality)?],
        )?;
        if changed != 1 {
            return Err(Error::NotFound(format!("Reality {} not found", reality.id)));
        }
        Ok(())
    }

    pub fn reality(&self, id: &RealityId) -> Result<Reality> {
        self.get("SELECT data FROM realities WHERE id=?1", &id.to_string())
    }

    pub fn realities(&self) -> Result<Vec<Reality>> {
        self.list("SELECT data FROM realities ORDER BY created_at, id")
    }

    pub fn insert_execution(&self, record: &ExecutionRecord) -> Result<()> {
        self.connection.execute(
            "INSERT INTO executions(id, reality_id, created_at, data) VALUES (?1, ?2, ?3, ?4)",
            params![
                record.id.to_string(),
                record.reality_id.to_string(),
                record.action.started_at.to_rfc3339(),
                serde_json::to_string(record)?
            ],
        )?;
        Ok(())
    }

    pub fn execution(&self, id: &ExecutionId) -> Result<ExecutionRecord> {
        self.get("SELECT data FROM executions WHERE id=?1", &id.to_string())
    }

    pub fn executions(&self) -> Result<Vec<ExecutionRecord>> {
        self.list("SELECT data FROM executions ORDER BY created_at, id")
    }

    fn get<T: DeserializeOwned>(&self, sql: &str, id: &str) -> Result<T> {
        let data: Option<String> = self
            .connection
            .query_row(sql, [id], |r| r.get(0))
            .optional()?;
        Ok(serde_json::from_str(&data.ok_or_else(|| {
            Error::NotFound(format!("Record {id} not found"))
        })?)?)
    }

    fn list<T: DeserializeOwned>(&self, sql: &str) -> Result<Vec<T>> {
        let mut query = self.connection.prepare(sql)?;
        query
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }

    /// An advisory lock prevents cleanup/discard racing a live Hardknock run.
    pub fn lock_reality(&self, id: &RealityId) -> Result<File> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.home.join("locks").join(format!("{id}.lock")))?;
        FileExt::try_lock_exclusive(&file).map_err(|e| {
            if e.kind() == std::io::ErrorKind::WouldBlock {
                Error::Intervention(format!(
                    "Reality {id} is in use by another Hardknock process"
                ))
            } else {
                Error::Io(e)
            }
        })?;
        Ok(file)
    }
}

pub fn artifact(path: &Path) -> Result<ArtifactRef> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0; 64 * 1024];
    let mut bytes = 0;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes += read as u64;
    }
    Ok(ArtifactRef {
        kind: Default::default(),
        path: path.into(),
        blake3: hasher.finalize().to_hex().to_string(),
        bytes,
    })
}
