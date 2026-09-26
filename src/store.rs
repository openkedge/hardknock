// SPDX-License-Identifier: Apache-2.0

use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

use fs2::FileExt;
use nix::unistd::geteuid;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::de::DeserializeOwned;

use crate::{
    Error, Result,
    core::{ArtifactRef, ExecutionId, ExecutionRecord, ExperimentId, Reality, RealityId},
};

mod abstraction;
mod assurance;
mod capabilities;
mod composition;
mod hierarchy;
mod knowledge_runtime;
mod plan;
mod team;
mod team_governance;
mod team_handoff;
mod team_review;
pub use assurance::AssuranceStore;
mod causal;
mod predictive;
pub use predictive::{NewTrajectory, NewTrajectoryEvent};
mod economics;
mod effects;
mod epistemic;
pub use capabilities::{CapabilityStore, token_hash};
mod experiences;
mod experiments;
pub use effects::EffectStore;
pub use epistemic::EpistemicStore;
mod federation;
pub use experiences::{ExperienceQuery, ExperienceStore, ExperienceSummary};
pub use experiments::ExperimentStore;
mod bridge;
mod curriculum;
mod development;
mod learning;
mod resilience;
mod runtime;
mod sync;
pub use runtime::RuntimeStore;
mod tools;
mod transfer;
pub use curriculum::CurriculumStore;
pub use learning::{LessonQuery, LessonStore, LessonSummary};
pub use tools::ToolStore;

/// Schema version produced by all migrations compiled into this binary.
pub const LATEST_SCHEMA_VERSION: i64 = 30;

pub(crate) const HOME_ENTRIES: &[&str] = &[
    "hardknock.db",
    "hardknock.db-shm",
    "hardknock.db-wal",
    "artifacts",
    "backups",
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
    "tools",
    "setup",
];

const HOME_DIRECTORIES: &[&str] = &[
    "artifacts",
    "backups",
    "realities",
    "logs",
    "locks",
    "fixtures",
    "run",
    "integrations",
    "identity",
    "federation",
    "effects",
    "tools",
    "setup",
];

pub struct Store {
    pub home: PathBuf,
    connection: Connection,
}

#[derive(Debug)]
pub struct ArtifactCapacityReservation {
    report: crate::storage_policy::CapacityReport,
    home: PathBuf,
    name: OsString,
    lease: File,
}

impl ArtifactCapacityReservation {
    pub fn report(&self) -> &crate::storage_policy::CapacityReport {
        &self.report
    }
}

impl Drop for ArtifactCapacityReservation {
    fn drop(&mut self) {
        if let Err(error) =
            crate::storage::release_artifact_reservation(&self.home, &self.name, &self.lease)
        {
            tracing::error!(%error, "Could not release artifact capacity reservation");
        }
    }
}

pub(crate) fn validate_dedicated_home(home: &Path) -> Result<()> {
    if home.exists() {
        for entry in fs::read_dir(home)? {
            let name = entry?.file_name();
            if !HOME_ENTRIES.iter().any(|allowed| name == *allowed) {
                return Err(Error::Intervention("HARDKNOCK_HOME must be a dedicated empty directory or an existing Hardknock data directory.".into()));
            }
        }
    }
    Ok(())
}

fn query_applied_schema_version(connection: &Connection) -> Result<i64> {
    Ok(connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?)
}

impl Store {
    pub fn open(home: &Path) -> Result<Self> {
        validate_dedicated_home(home)?;
        fs::create_dir_all(home)?;
        let home = home.canonicalize()?;
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700))?;
        for child in HOME_DIRECTORIES {
            if fs::symlink_metadata(home.join(child)).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(Error::Intervention(
                    "Hardknock data subdirectories must not be symlinks.".into(),
                ));
            }
            fs::create_dir_all(home.join(child))?;
            fs::set_permissions(home.join(child), fs::Permissions::from_mode(0o700))?;
        }
        let transient_artifacts = home.join("artifacts/transient");
        if fs::symlink_metadata(&transient_artifacts).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(Error::Intervention(
                "Hardknock transient artifact directory must not be a symlink.".into(),
            ));
        }
        fs::create_dir_all(&transient_artifacts)?;
        fs::set_permissions(&transient_artifacts, fs::Permissions::from_mode(0o700))?;
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
        let observed_version = crate::storage::current_schema_version(&connection)?;
        if observed_version > LATEST_SCHEMA_VERSION {
            return Err(Error::Intervention(format!(
                "Database schema {observed_version} is newer than the latest supported schema {LATEST_SCHEMA_VERSION}; upgrade the CLI."
            )));
        }
        let _maintenance_lock = if observed_version > 0 && observed_version < LATEST_SCHEMA_VERSION
        {
            Some(crate::storage::acquire_home_maintenance_lock(&home)?)
        } else {
            None
        };
        let _artifact_capacity_lock =
            if observed_version > 0 && observed_version < LATEST_SCHEMA_VERSION {
                Some(crate::storage::acquire_artifact_capacity_lock(&home)?)
            } else {
                None
            };
        if _artifact_capacity_lock.is_some() {
            crate::storage::require_no_active_artifact_reservations_locked(
                &home,
                "Database migration",
            )?;
        }
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);")?;
        let version = query_applied_schema_version(&tx)?;
        if version > LATEST_SCHEMA_VERSION {
            return Err(Error::Intervention(format!(
                "Database schema {version} is newer than the latest supported schema {LATEST_SCHEMA_VERSION}; upgrade the CLI."
            )));
        }
        if version > 0 && version < LATEST_SCHEMA_VERSION {
            if _maintenance_lock.is_none() || _artifact_capacity_lock.is_none() {
                return Err(Error::Intervention(
                    "Database became migration-eligible while opening; retry so Hardknock can acquire the maintenance and artifact locks before upgrade.".into(),
                ));
            }
            let backup = crate::storage::backup_before_migration_locked(
                &home,
                version,
                LATEST_SCHEMA_VERSION,
            )?;
            tracing::info!(
                current_schema = version,
                target_schema = LATEST_SCHEMA_VERSION,
                backup = %backup.display(),
                "Created and verified pre-migration backup"
            );
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
        if version < 13 {
            tx.execute_batch(include_str!("../migrations/013_tools.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (13)", [])?;
        }
        if version < 14 {
            tx.execute_batch(include_str!("../migrations/014_assurance.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (14)", [])?;
        }
        if version < 15 {
            tx.execute_batch(include_str!("../migrations/015_runtime.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (15)", [])?;
        }
        if version < 16 {
            tx.execute_batch(include_str!("../migrations/016_epistemic.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (16)", [])?;
        }
        if version < 17 {
            tx.execute_batch(include_str!("../migrations/017_causal.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (17)", [])?;
        }
        if version < 18 {
            tx.execute_batch(include_str!("../migrations/018_predictive.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (18)", [])?;
        }
        if version < 19 {
            tx.execute_batch(include_str!(
                "../migrations/019_predictive_trajectory_models.sql"
            ))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (19)", [])?;
        }
        if version < 20 {
            tx.execute_batch(include_str!("../migrations/020_experience_economics.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (20)", [])?;
        }
        if version < 21 {
            tx.execute_batch(include_str!("../migrations/021_experience_abstraction.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (21)", [])?;
        }
        if version < 22 {
            tx.execute_batch(include_str!("../migrations/022_knowledge_hierarchy.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (22)", [])?;
        }
        if version < 23 {
            tx.execute_batch(include_str!("../migrations/023_runtime_knowledge.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (23)", [])?;
        }
        if version < 24 {
            tx.execute_batch(include_str!("../migrations/024_composition.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (24)", [])?;
        }
        if version < 25 {
            tx.execute_batch(include_str!("../migrations/025_plan.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (25)", [])?;
        }
        if version < 26 {
            tx.execute_batch(include_str!("../migrations/026_team.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (26)", [])?;
        }
        if version < 27 {
            tx.execute_batch(include_str!("../migrations/027_team_review.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (27)", [])?;
        }
        if version < 28 {
            tx.execute_batch(include_str!("../migrations/028_team_handoff.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (28)", [])?;
        }
        if version < 29 {
            tx.execute_batch(include_str!("../migrations/029_team_governance.sql"))?;
            tx.execute("INSERT INTO schema_migrations(version) VALUES (29)", [])?;
        }
        if version < LATEST_SCHEMA_VERSION {
            tx.execute_batch(include_str!("../migrations/030_distributed_sync.sql"))?;
            tx.execute(
                "INSERT INTO schema_migrations(version) VALUES (?1)",
                [LATEST_SCHEMA_VERSION],
            )?;
        }
        let applied_schema_version = query_applied_schema_version(&tx)?;
        if applied_schema_version != LATEST_SCHEMA_VERSION {
            return Err(Error::Intervention(format!(
                "Database migration reached schema {applied_schema_version}; this Hardknock expects schema {LATEST_SCHEMA_VERSION}."
            )));
        }
        tx.commit()?;
        tracing::debug!(
            schema_version = applied_schema_version,
            latest_schema_version = LATEST_SCHEMA_VERSION,
            "SQLite migrations ready"
        );
        Ok(Self { home, connection })
    }

    /// Returns the highest database migration recorded as applied.
    pub fn applied_schema_version(&self) -> Result<i64> {
        query_applied_schema_version(&self.connection)
    }

    /// Refuse a new artifact-producing operation when configured quotas or
    /// minimum free space cannot accommodate its bounded reservation.
    pub fn ensure_artifact_capacity(
        &self,
        requested_bytes: u64,
        requested_files: u64,
    ) -> Result<crate::storage_policy::CapacityReport> {
        Ok(self
            .reserve_artifact_capacity(requested_bytes, requested_files)?
            .report()
            .clone())
    }

    /// Hold the capacity lock for the complete artifact-producing operation so
    /// concurrent producers and retention cannot all consume the same
    /// observed headroom.
    pub fn reserve_artifact_capacity(
        &self,
        requested_bytes: u64,
        requested_files: u64,
    ) -> Result<ArtifactCapacityReservation> {
        let _capacity = crate::storage::acquire_artifact_capacity_lock(&self.home)?;
        let active = crate::storage::active_artifact_reservations_locked(&self.home)?;
        let report = crate::bridge::config::Config::load(&self.home)?
            .storage
            .ensure_capacity_with_reservations(
                self.home.join("artifacts"),
                requested_bytes,
                requested_files,
                active.usage,
            )
            .map_err(|error| Error::Intervention(error.to_string()))?;
        let (lease, name) = crate::storage::create_artifact_reservation_locked(
            &self.home,
            crate::storage_policy::StorageUsage {
                bytes: requested_bytes,
                files: requested_files,
            },
        )?;
        Ok(ArtifactCapacityReservation {
            report,
            home: self.home.clone(),
            name,
            lease,
        })
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
        let path = self.home.join("locks").join(format!("{id}.lock"));
        self.lock_owned_record(
            &path,
            "Reality lock",
            format!("Reality {id} is in use by another Hardknock process"),
        )
    }

    /// An advisory lock distinguishes an active strategy experiment from a
    /// non-resumable record left Running after process loss.
    pub fn lock_experiment(&self, id: &ExperimentId) -> Result<File> {
        let path = self.home.join("locks").join(format!("{id}.lock"));
        self.lock_owned_record(
            &path,
            "Experiment lock",
            format!("Experiment {id} is in use by another Hardknock process"),
        )
    }

    pub fn lock_experiment_capacity(&self, slot: usize) -> Result<File> {
        let path = self
            .home
            .join("locks")
            .join(format!("experiment-capacity-{slot}.lock"));
        self.lock_owned_record(
            &path,
            "Experiment capacity lock",
            format!("Experiment capacity slot {slot} is already in use"),
        )
    }

    fn lock_owned_record(&self, path: &Path, label: &str, busy: String) -> Result<File> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
            .open(path)?;
        let opened = file.metadata()?;
        if !opened.is_file() || opened.uid() != geteuid().as_raw() || opened.nlink() != 1 {
            return Err(Error::Intervention(format!(
                "{label} must be an owned regular file with one link: {}",
                path.display()
            )));
        }
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        let current = fs::symlink_metadata(path)?;
        if current.file_type().is_symlink()
            || !current.is_file()
            || current.uid() != opened.uid()
            || current.nlink() != 1
            || current.dev() != opened.dev()
            || current.ino() != opened.ino()
            || current.permissions().mode() & 0o777 != 0o600
        {
            return Err(Error::Intervention(format!(
                "{label} changed or is unsafe: {}",
                path.display()
            )));
        }
        FileExt::try_lock_exclusive(&file).map_err(|e| {
            if e.kind() == std::io::ErrorKind::WouldBlock {
                Error::Intervention(busy)
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
