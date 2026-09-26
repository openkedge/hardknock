// SPDX-License-Identifier: Apache-2.0

mod support;

use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use fs2::FileExt;
use hardknock::{
    core::{ExecutionRecord, RealityId},
    storage::{BackupManifest, create_managed_recovery_point, migration_plan, verify_backup},
    store::{LATEST_SCHEMA_VERSION, Store, artifact},
};
use support::Fixture;

fn create_recorded_artifacts(fixture: &Fixture) -> ExecutionRecord {
    let result = fixture.cli(
        &[
            "run",
            "--script",
            "printf 'backup payload\\n'",
            "--check",
            "true",
            "--no-experience",
            "backup round trip",
        ],
        0,
    );
    serde_json::from_value(result["execution"].clone()).unwrap()
}

fn create_backup(fixture: &Fixture, name: &str) -> PathBuf {
    let destination = fixture.temp.path().join(name);
    let result = fixture.cli(&["backup", destination.to_str().unwrap()], 0);
    assert_eq!(result["result"]["kind"], "backup");
    assert_eq!(result["result"]["backup"]["verified"], true);
    assert_eq!(
        result["result"]["backup"]["schema_version"],
        LATEST_SCHEMA_VERSION
    );
    destination
}

fn create_private_nested_directories(root: &Path, count: usize) {
    let mut current = root.to_owned();
    for index in 0..count {
        current = current.join(format!("level-{index:03}"));
        fs::create_dir(&current).unwrap();
        fs::set_permissions(&current, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

fn command_error(fixture: &Fixture, args: &[&str], expected: i32) -> String {
    let output = fixture.command().arg("--json").args(args).output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(expected),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    serde_json::from_slice::<serde_json::Value>(&output.stderr).unwrap()["message"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn create_historical_home(home: &Path, target_version: i64) {
    fs::create_dir(home).unwrap();
    fs::set_permissions(home, fs::Permissions::from_mode(0o700)).unwrap();
    let database = home.join("hardknock.db");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "PRAGMA foreign_keys=ON;
             CREATE TABLE schema_migrations(
               version INTEGER PRIMARY KEY,
               applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
             );",
        )
        .unwrap();
    let mut migrations: Vec<_> =
        fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
    migrations.sort();
    for migration in migrations {
        let name = migration.file_name().unwrap().to_str().unwrap();
        let version: i64 = name[..3].parse().unwrap();
        if version > target_version {
            break;
        }
        connection
            .execute_batch(&fs::read_to_string(&migration).unwrap())
            .unwrap();
        connection
            .execute(
                "INSERT INTO schema_migrations(version) VALUES(?1)",
                [version],
            )
            .unwrap();
    }
    drop(connection);
    fs::set_permissions(database, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn backup_restore_round_trip_preserves_database_artifacts_and_permissions() {
    let fixture = Fixture::new();
    let execution = create_recorded_artifacts(&fixture);
    let backup = create_backup(&fixture, "round-trip.hkbak");
    let manifest = verify_backup(&backup).unwrap();
    assert_eq!(manifest.format, "hardknock.backup");
    assert_eq!(manifest.format_version, 1);
    assert_eq!(manifest.package_version, env!("CARGO_PKG_VERSION"));
    assert!(!manifest.artifacts.is_empty());
    assert_eq!(
        fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(backup.join("manifest.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let original = fixture.temp.path().join("original-home");
    fs::rename(&fixture.home, &original).unwrap();
    fs::create_dir(&fixture.home).unwrap();
    fs::set_permissions(&fixture.home, fs::Permissions::from_mode(0o700)).unwrap();
    let restored = fixture.cli(&["restore", "--verify", backup.to_str().unwrap()], 0);
    assert_eq!(restored["result"]["kind"], "restore");
    assert_eq!(restored["result"]["restore"]["verified"], true);
    assert_eq!(
        fs::metadata(&fixture.home).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(fixture.home.join("hardknock.db"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let store = Store::open(&fixture.home).unwrap();
    let recovered = store.execution(&execution.id).unwrap();
    assert_eq!(
        fs::read_to_string(&recovered.action.stdout.path).unwrap(),
        "backup payload\n"
    );
    assert_eq!(
        artifact(&recovered.action.stdout.path).unwrap().blake3,
        recovered.action.stdout.blake3
    );
    assert_eq!(
        fs::metadata(&recovered.action.stdout.path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn backup_and_restore_never_overwrite_existing_paths() {
    let fixture = Fixture::new();
    drop(Store::open(&fixture.home).unwrap());
    let occupied_backup = fixture.temp.path().join("occupied.hkbak");
    fs::create_dir(&occupied_backup).unwrap();
    fs::write(occupied_backup.join("sentinel"), "keep").unwrap();
    let message = command_error(&fixture, &["backup", occupied_backup.to_str().unwrap()], 5);
    assert!(message.contains("already exists"));
    assert_eq!(
        fs::read_to_string(occupied_backup.join("sentinel")).unwrap(),
        "keep"
    );

    let backup = create_backup(&fixture, "valid.hkbak");
    let saved = fixture.temp.path().join("saved-home");
    fs::rename(&fixture.home, &saved).unwrap();
    fs::create_dir(&fixture.home).unwrap();
    fs::write(fixture.home.join("sentinel"), "keep").unwrap();
    let message = command_error(
        &fixture,
        &["restore", "--verify", backup.to_str().unwrap()],
        5,
    );
    assert!(message.contains("must be empty"));
    assert_eq!(
        fs::read_to_string(fixture.home.join("sentinel")).unwrap(),
        "keep"
    );
}

#[test]
fn restore_rejects_tampering_and_manifest_traversal_before_mutation() {
    let fixture = Fixture::new();
    create_recorded_artifacts(&fixture);
    let backup = create_backup(&fixture, "tamper.hkbak");
    let manifest = verify_backup(&backup).unwrap();
    fs::write(
        backup.join(&manifest.artifacts[0].path),
        "tampered artifact",
    )
    .unwrap();
    fs::rename(&fixture.home, fixture.temp.path().join("saved-home")).unwrap();
    let message = command_error(
        &fixture,
        &["restore", "--verify", backup.to_str().unwrap()],
        5,
    );
    assert!(message.contains("hash or size verification"));
    assert!(!fixture.home.exists());

    let second = Fixture::new();
    drop(Store::open(&second.home).unwrap());
    let portable = second.home.join("artifacts").join("portable.txt");
    fs::write(&portable, "safe").unwrap();
    fs::set_permissions(&portable, fs::Permissions::from_mode(0o600)).unwrap();
    let traversal_backup = create_backup(&second, "traversal.hkbak");
    let mut manifest: BackupManifest = verify_backup(&traversal_backup).unwrap();
    manifest.artifacts[0].path = "../escape".into();
    fs::write(
        traversal_backup.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    fs::rename(&second.home, second.temp.path().join("saved-home")).unwrap();
    let message = command_error(
        &second,
        &["restore", "--verify", traversal_backup.to_str().unwrap()],
        5,
    );
    assert!(message.contains("traversal"));
    assert!(!second.home.exists());
    assert!(!second.temp.path().join("escape").exists());
}

#[test]
fn backup_rejects_symlinks_and_missing_referenced_artifacts() {
    let symlink_fixture = Fixture::new();
    drop(Store::open(&symlink_fixture.home).unwrap());
    let outside = symlink_fixture.temp.path().join("outside");
    fs::write(&outside, "outside").unwrap();
    symlink(
        &outside,
        symlink_fixture.home.join("artifacts").join("escape"),
    )
    .unwrap();
    let destination = symlink_fixture.temp.path().join("symlink.hkbak");
    let message = command_error(
        &symlink_fixture,
        &["backup", destination.to_str().unwrap()],
        5,
    );
    assert!(message.contains("must not contain symlinks"));
    assert!(!destination.exists());

    let missing_fixture = Fixture::new();
    let execution = create_recorded_artifacts(&missing_fixture);
    fs::remove_file(&execution.action.stdout.path).unwrap();
    let destination = missing_fixture.temp.path().join("missing.hkbak");
    let message = command_error(
        &missing_fixture,
        &["backup", destination.to_str().unwrap()],
        5,
    );
    assert!(message.contains("missing from the backup"));
    assert!(!destination.exists());
}

#[test]
fn backup_rejects_excessive_artifact_nesting() {
    let fixture = Fixture::new();
    drop(Store::open(&fixture.home).unwrap());
    create_private_nested_directories(&fixture.home.join("artifacts"), 70);
    let destination = fixture.temp.path().join("deep-source.hkbak");

    let message = command_error(&fixture, &["backup", destination.to_str().unwrap()], 5);
    assert!(
        message.contains("Backup source nesting exceeds the supported depth"),
        "{message}"
    );
    assert!(!destination.exists());
}

#[test]
fn verification_rejects_excessive_bundle_nesting() {
    let fixture = Fixture::new();
    drop(Store::open(&fixture.home).unwrap());
    let backup = create_backup(&fixture, "deep-bundle.hkbak");
    create_private_nested_directories(&backup.join("artifacts"), 70);

    let error = verify_backup(&backup).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Backup inventory nesting exceeds the supported depth"),
        "{error}"
    );
}

#[test]
fn backup_rejects_hardlinks_and_insecure_source_modes() {
    let hardlink_fixture = Fixture::new();
    drop(Store::open(&hardlink_fixture.home).unwrap());
    let artifact = hardlink_fixture.home.join("artifacts").join("evidence.txt");
    fs::write(&artifact, "evidence").unwrap();
    fs::set_permissions(&artifact, fs::Permissions::from_mode(0o600)).unwrap();
    fs::hard_link(&artifact, hardlink_fixture.temp.path().join("second-link")).unwrap();
    let destination = hardlink_fixture.temp.path().join("hardlink.hkbak");
    let message = command_error(
        &hardlink_fixture,
        &["backup", destination.to_str().unwrap()],
        5,
    );
    assert!(message.contains("exactly one hard link"), "{message}");
    assert!(!destination.exists());

    let mode_fixture = Fixture::new();
    drop(Store::open(&mode_fixture.home).unwrap());
    let artifact = mode_fixture.home.join("artifacts").join("public.txt");
    fs::write(&artifact, "public").unwrap();
    fs::set_permissions(&artifact, fs::Permissions::from_mode(0o666)).unwrap();
    let destination = mode_fixture.temp.path().join("mode.hkbak");
    let message = command_error(&mode_fixture, &["backup", destination.to_str().unwrap()], 5);
    assert!(
        message.contains("must not allow group or world writes"),
        "{message}"
    );
    assert!(!destination.exists());
}

#[test]
fn verify_and_restore_reject_hardlinked_or_insecure_bundle_files() {
    let hardlink_fixture = Fixture::new();
    create_recorded_artifacts(&hardlink_fixture);
    let backup = create_backup(&hardlink_fixture, "hardlinked-bundle.hkbak");
    fs::hard_link(
        backup.join("database.sqlite3"),
        hardlink_fixture.temp.path().join("database-second-link"),
    )
    .unwrap();
    let message = verify_backup(&backup).unwrap_err().to_string();
    assert!(message.contains("exactly one hard link"), "{message}");
    let saved = hardlink_fixture.temp.path().join("saved-home");
    fs::rename(&hardlink_fixture.home, &saved).unwrap();
    let message = command_error(
        &hardlink_fixture,
        &["restore", "--verify", backup.to_str().unwrap()],
        5,
    );
    assert!(message.contains("exactly one hard link"), "{message}");
    assert!(!hardlink_fixture.home.exists());

    let mode_fixture = Fixture::new();
    create_recorded_artifacts(&mode_fixture);
    let backup = create_backup(&mode_fixture, "insecure-bundle.hkbak");
    fs::set_permissions(
        backup.join("manifest.json"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let message = verify_backup(&backup).unwrap_err().to_string();
    assert!(message.contains("permissions must be 0600"), "{message}");
}

#[test]
fn automatic_migration_backup_waits_for_the_bounded_maintenance_lock() {
    let fixture = Fixture::new();
    create_historical_home(&fixture.home, LATEST_SCHEMA_VERSION - 1);
    let locks = fixture.home.join("locks");
    fs::create_dir(&locks).unwrap();
    fs::set_permissions(&locks, fs::Permissions::from_mode(0o700)).unwrap();
    let lock_path = locks.join("maintenance.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)
        .unwrap();
    lock.try_lock_exclusive().unwrap();

    let home = fixture.home.clone();
    let migration = thread::spawn(move || Store::open(&home));
    thread::sleep(Duration::from_millis(150));
    lock.unlock().unwrap();
    drop(lock);

    let store = migration.join().unwrap().unwrap();
    assert_eq!(
        store.applied_schema_version().unwrap(),
        LATEST_SCHEMA_VERSION
    );
    drop(store);
    let backups: Vec<_> = fs::read_dir(fixture.home.join("backups"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(backups.len(), 1);
    verify_backup(&backups[0]).unwrap();
}

#[test]
fn manual_backup_reports_a_bounded_maintenance_conflict() {
    let fixture = Fixture::new();
    drop(Store::open(&fixture.home).unwrap());
    let lock_path = fixture.home.join("locks").join("maintenance.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(lock_path)
        .unwrap();
    lock.try_lock_exclusive().unwrap();

    let destination = fixture.temp.path().join("blocked.hkbak");
    let started = Instant::now();
    let message = command_error(&fixture, &["backup", destination.to_str().unwrap()], 5);
    let elapsed = started.elapsed();
    assert!(message.contains("maintenance is busy"), "{message}");
    assert!(
        elapsed < Duration::from_secs(5),
        "lock wait was not bounded: {elapsed:?}"
    );
    assert!(!destination.exists());
}

#[test]
fn reality_locks_are_private_and_reject_linked_paths() {
    let fixture = Fixture::new();
    let store = Store::open(&fixture.home).unwrap();
    let id = RealityId::new();
    let lock_path = store.home.join("locks").join(format!("{id}.lock"));

    fs::write(&lock_path, "legacy").unwrap();
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o644)).unwrap();
    let lease = store.lock_reality(&id).unwrap();
    assert_eq!(
        fs::metadata(&lock_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(lease);

    fs::remove_file(&lock_path).unwrap();
    let outside = fixture.temp.path().join("outside-lock-target");
    fs::write(&outside, "retain").unwrap();
    symlink(&outside, &lock_path).unwrap();
    assert!(store.lock_reality(&id).is_err());
    assert_eq!(fs::read_to_string(&outside).unwrap(), "retain");

    fs::remove_file(&lock_path).unwrap();
    fs::write(&lock_path, "linked").unwrap();
    let second_link = fixture.temp.path().join("second-lock-link");
    fs::hard_link(&lock_path, &second_link).unwrap();
    let error = store
        .lock_reality(&id)
        .expect_err("hard-linked Reality lock must fail closed");
    assert!(error.to_string().contains("one link"), "{error}");
    assert_eq!(fs::read_to_string(second_link).unwrap(), "linked");
}

#[test]
fn artifact_capacity_reservations_allow_concurrency_and_enforce_the_aggregate() {
    let fixture = Fixture::new();
    let first_store = Store::open(&fixture.home).unwrap();
    fs::write(
        first_store.home.join("config.toml"),
        "[storage]\nmax_bytes = 10\nmax_files = 10\nmin_free_bytes = 0\nmax_scan_entries = 100\nmax_prune_items = 10\n",
    )
    .unwrap();
    let first = first_store.reserve_artifact_capacity(6, 1).unwrap();
    let home = fixture.home.clone();

    let second = thread::spawn(move || {
        let store = Store::open(&home).unwrap();
        store.reserve_artifact_capacity(3, 1)
    })
    .join()
    .unwrap()
    .expect("independent producers may hold aggregate reservations concurrently");
    assert_eq!(second.report().reserved.bytes, 6);

    let competing = first_store
        .reserve_artifact_capacity(2, 1)
        .expect_err("aggregate reservations must not exceed capacity");
    assert!(
        competing.to_string().contains("already reserved"),
        "{competing}"
    );

    drop(first);
    let third = first_store.reserve_artifact_capacity(2, 1).unwrap();
    assert_eq!(third.report().reserved.bytes, 3);
    drop((second, third));
}

#[test]
fn backup_waits_for_artifact_producers_to_finish() {
    let fixture = Fixture::new();
    let store = Store::open(&fixture.home).unwrap();
    let reservation = store.reserve_artifact_capacity(1, 1).unwrap();
    let destination = fixture.temp.path().join("backup-with-active-writer");

    let error = hardknock::storage::create_backup(&store.home, &destination)
        .expect_err("backup must not race an active artifact producer");
    assert!(error.to_string().contains("writers to be idle"), "{error}");
    assert!(!destination.exists());

    drop(reservation);
    hardknock::storage::create_backup(&store.home, &destination).unwrap();
}

#[test]
fn managed_recovery_point_supports_a_fresh_current_schema_home() {
    let fixture = Fixture::new();
    drop(Store::open(&fixture.home).unwrap());

    let report = create_managed_recovery_point(&fixture.home, "setup").unwrap();

    assert_eq!(report.label, "setup");
    assert!(report.backup.verified);
    assert_eq!(report.backup.schema_version, LATEST_SCHEMA_VERSION);
    assert_eq!(report.backup.artifact_count, 0);
    assert_eq!(
        report.backup.destination.parent(),
        Some(
            fixture
                .home
                .join("backups")
                .canonicalize()
                .unwrap()
                .as_path()
        )
    );
    assert_eq!(
        fs::metadata(&report.backup.destination)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        verify_backup(&report.backup.destination)
            .unwrap()
            .schema_version,
        LATEST_SCHEMA_VERSION
    );
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["label"], "setup");
    assert_eq!(json["verified"], true);
    assert_eq!(
        json["destination"],
        report.backup.destination.to_str().unwrap()
    );
}

#[test]
fn managed_recovery_point_captures_existing_evidence() {
    let fixture = Fixture::new();
    let execution = create_recorded_artifacts(&fixture);

    let report = create_managed_recovery_point(&fixture.home, "pre_upgrade").unwrap();
    let manifest = verify_backup(&report.backup.destination).unwrap();

    assert_eq!(report.backup.artifact_count, manifest.artifacts.len());
    assert!(!manifest.artifacts.is_empty());
    let canonical_home = fixture.home.canonicalize().unwrap();
    let canonical_stdout = execution.action.stdout.path.canonicalize().unwrap();
    let recorded_stdout = canonical_stdout
        .strip_prefix(canonical_home)
        .unwrap()
        .to_string_lossy();
    assert!(
        manifest
            .artifacts
            .iter()
            .any(|artifact| artifact.path == recorded_stdout)
    );
}

#[test]
fn managed_recovery_point_rejects_invalid_labels_without_writing() {
    let fixture = Fixture::new();
    drop(Store::open(&fixture.home).unwrap());
    let too_long = "a".repeat(65);

    for label in ["", "../escape", "contains space", "-leading", "trailing-"] {
        let error = create_managed_recovery_point(&fixture.home, label)
            .expect_err("invalid managed recovery point labels must fail");
        assert!(error.to_string().contains("label"), "{error}");
    }
    let error = create_managed_recovery_point(&fixture.home, &too_long)
        .expect_err("overlong managed recovery point labels must fail");
    assert!(error.to_string().contains("1 to 64 bytes"), "{error}");
    assert_eq!(
        fs::read_dir(fixture.home.join("backups")).unwrap().count(),
        0
    );
}

#[test]
fn repeated_managed_recovery_points_never_overwrite() {
    let fixture = Fixture::new();
    drop(Store::open(&fixture.home).unwrap());

    let first = create_managed_recovery_point(&fixture.home, "repair").unwrap();
    let first_manifest = verify_backup(&first.backup.destination).unwrap();
    let second = create_managed_recovery_point(&fixture.home, "repair").unwrap();

    assert_ne!(first.backup.destination, second.backup.destination);
    assert!(first.backup.destination.exists());
    assert!(second.backup.destination.exists());
    assert_eq!(
        verify_backup(&first.backup.destination).unwrap(),
        first_manifest
    );
    assert_eq!(
        fs::read_dir(fixture.home.join("backups")).unwrap().count(),
        2
    );
}

#[test]
fn migration_dry_run_reports_without_creating_or_changing_a_home() {
    let fixture = Fixture::new();
    let absent = fixture.cli(&["migration", "dry-run"], 0);
    assert_eq!(absent["result"]["plan"]["current_schema"], 0);
    assert_eq!(
        absent["result"]["plan"]["target_schema"],
        LATEST_SCHEMA_VERSION
    );
    assert_eq!(absent["result"]["plan"]["backup_required"], false);
    assert_eq!(absent["result"]["plan"]["migration_required"], true);
    assert!(!fixture.home.exists());

    drop(Store::open(&fixture.home).unwrap());
    let database = fixture.home.join("hardknock.db");
    let before = artifact(&database).unwrap().blake3;
    let entries_before: BTreeSet<_> = fs::read_dir(&fixture.home)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    let current = fixture.cli(&["migration", "dry-run"], 0);
    assert_eq!(
        current["result"]["plan"]["current_schema"],
        LATEST_SCHEMA_VERSION
    );
    assert_eq!(current["result"]["plan"]["migration_required"], false);
    assert_eq!(current["result"]["plan"]["backup_required"], false);
    assert_eq!(artifact(&database).unwrap().blake3, before);
    assert_eq!(
        fs::read_dir(&fixture.home)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<BTreeSet<_>>(),
        entries_before
    );
}

#[test]
fn historical_schema_dry_run_and_upgrade_create_a_recoverable_verified_backup() {
    let fixture = Fixture::new();
    create_historical_home(&fixture.home, LATEST_SCHEMA_VERSION - 1);
    let database = fixture.home.join("hardknock.db");
    let before = artifact(&database).unwrap().blake3;

    let plan = fixture.cli(&["migration", "dry-run"], 0);
    assert_eq!(
        plan["result"]["plan"]["current_schema"],
        LATEST_SCHEMA_VERSION - 1
    );
    assert_eq!(
        plan["result"]["plan"]["target_schema"],
        LATEST_SCHEMA_VERSION
    );
    assert_eq!(plan["result"]["plan"]["migration_required"], true);
    assert_eq!(plan["result"]["plan"]["backup_required"], true);
    assert_eq!(artifact(&database).unwrap().blake3, before);
    assert!(!fixture.home.join("backups").exists());

    let store = Store::open(&fixture.home).unwrap();
    assert_eq!(
        store.applied_schema_version().unwrap(),
        LATEST_SCHEMA_VERSION
    );
    drop(store);
    let backups: Vec<_> = fs::read_dir(fixture.home.join("backups"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(backups.len(), 1);
    let manifest = verify_backup(&backups[0]).unwrap();
    assert_eq!(manifest.schema_version, LATEST_SCHEMA_VERSION - 1);

    let migrated = fixture.temp.path().join("migrated-home");
    fs::rename(&fixture.home, &migrated).unwrap();
    let moved_backup = migrated
        .join("backups")
        .join(backups[0].file_name().unwrap());
    hardknock::storage::restore_backup(&moved_backup, &fixture.home).unwrap();
    let recovered_plan = migration_plan(&fixture.home).unwrap();
    assert_eq!(recovered_plan.current_schema, LATEST_SCHEMA_VERSION - 1);
    assert!(recovered_plan.backup_required);
}

#[test]
fn representative_historical_schemas_upgrade_with_verified_recovery_points() {
    for historical_version in [1, 10, 20] {
        let fixture = Fixture::new();
        create_historical_home(&fixture.home, historical_version);
        let store = Store::open(&fixture.home).unwrap();
        assert_eq!(
            store.applied_schema_version().unwrap(),
            LATEST_SCHEMA_VERSION
        );
        drop(store);
        let backups: Vec<_> = fs::read_dir(fixture.home.join("backups"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            verify_backup(&backups[0]).unwrap().schema_version,
            historical_version
        );
    }
}

#[test]
fn failed_upgrade_leaves_the_original_schema_and_verified_backup_recoverable() {
    let fixture = Fixture::new();
    create_historical_home(&fixture.home, LATEST_SCHEMA_VERSION - 1);
    let connection = rusqlite::Connection::open(fixture.home.join("hardknock.db")).unwrap();
    connection
        .execute("CREATE TABLE hardknock_nodes(conflict TEXT)", [])
        .unwrap();
    drop(connection);

    let error = Store::open(&fixture.home)
        .err()
        .expect("conflicting migration must fail");
    assert!(error.to_string().contains("already exists"));
    assert_eq!(
        migration_plan(&fixture.home).unwrap().current_schema,
        LATEST_SCHEMA_VERSION - 1
    );
    let backups: Vec<_> = fs::read_dir(fixture.home.join("backups"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(
        verify_backup(&backups[0]).unwrap().schema_version,
        LATEST_SCHEMA_VERSION - 1
    );

    let failed_home = fixture.temp.path().join("failed-upgrade-home");
    fs::rename(&fixture.home, &failed_home).unwrap();
    let moved_backup = failed_home
        .join("backups")
        .join(backups[0].file_name().unwrap());
    hardknock::storage::restore_backup(&moved_backup, &fixture.home).unwrap();
    assert_eq!(
        migration_plan(&fixture.home).unwrap().current_schema,
        LATEST_SCHEMA_VERSION - 1
    );
}
