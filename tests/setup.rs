// SPDX-License-Identifier: Apache-2.0

use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn command(user_home: &Path, data_home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hardknock"));
    command
        .env("HOME", user_home)
        .env("HARDKNOCK_HOME", data_home)
        .env("RUST_LOG", "error")
        .arg("--json");
    command
}

fn run(user_home: &Path, data_home: &Path, args: &[&str]) -> Output {
    command(user_home, data_home).args(args).output().unwrap()
}

fn result(output: &Output) -> Value {
    let response: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid JSON result: {error}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(response["event"], "maintenance");
    response["result"].clone()
}

fn private_directory(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn backup_count(home: &Path) -> usize {
    fs::read_dir(home.join("backups"))
        .unwrap()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.path().is_dir())
        .count()
}

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temporary = tempfile::tempdir().unwrap();
    let user_home = temporary.path().join("user");
    let data_home = temporary.path().join("data");
    private_directory(&user_home);
    (temporary, user_home, data_home)
}

#[test]
fn dry_run_json_is_complete_and_does_not_create_the_home() {
    let (_temporary, user_home, data_home) = fixture();
    let output = run(
        &user_home,
        &data_home,
        &[
            "setup",
            "--agent",
            "none",
            "--mode",
            "ci",
            "--non-interactive",
            "--dry-run",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let setup = result(&output);
    assert_eq!(setup["schema"], "hardknock-setup-result-v1");
    assert_eq!(setup["operation"], "setup");
    assert_eq!(setup["dry_run"], true);
    assert_eq!(setup["plan"]["migration"]["database_exists"], false);
    assert!(
        setup["plan"]["changes"]
            .as_array()
            .is_some_and(|changes| !changes.is_empty())
    );
    assert!(setup["plan"]["detection"]["tools"].is_array());
    assert!(!data_home.exists());
    assert!(fs::read_dir(&user_home).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("hardknock-setup")
    }));
}

#[test]
fn uninstall_without_managed_state_is_a_true_no_op() {
    let (_temporary, user_home, data_home) = fixture();
    let output = run(&user_home, &data_home, &["uninstall", "--non-interactive"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let uninstall = result(&output);
    assert_eq!(uninstall["changed"], false);
    assert!(uninstall["journal"].is_null());
    assert!(!data_home.exists());
    assert!(fs::read_dir(&user_home).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("hardknock-setup")
    }));
}

#[test]
fn setup_repair_upgrade_and_uninstall_preserve_idempotent_ownership() {
    let (_temporary, user_home, data_home) = fixture();
    let arguments = [
        "setup",
        "--agent",
        "none",
        "--mode",
        "ci",
        "--non-interactive",
    ];
    let first_output = run(&user_home, &data_home, &arguments);
    assert_eq!(first_output.status.code(), Some(1));
    let first = result(&first_output);
    assert_eq!(first["operation"], "setup");
    assert_eq!(first["plan"]["agents"], json!([]));
    assert!(data_home.join("setup/manifest-v1.json").is_file());
    assert_eq!(backup_count(&data_home), 1);
    let first_journal = PathBuf::from(first["journal"].as_str().unwrap());
    assert!(first_journal.is_file());
    assert_eq!(
        fs::metadata(&first_journal).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let second_output = run(&user_home, &data_home, &arguments);
    assert_eq!(second_output.status.code(), Some(1));
    assert_eq!(backup_count(&data_home), 1);

    let repair_output = run(
        &user_home,
        &data_home,
        &[
            "repair",
            "--agent",
            "auto",
            "--mode",
            "ci",
            "--non-interactive",
        ],
    );
    assert_eq!(repair_output.status.code(), Some(1));
    assert_eq!(result(&repair_output)["operation"], "repair");
    assert_eq!(backup_count(&data_home), 1);

    let upgrade_output = run(
        &user_home,
        &data_home,
        &[
            "upgrade",
            "--agent",
            "auto",
            "--mode",
            "ci",
            "--non-interactive",
        ],
    );
    assert_eq!(upgrade_output.status.code(), Some(1));
    let upgrade = result(&upgrade_output);
    assert_eq!(upgrade["operation"], "upgrade");
    assert_eq!(upgrade["recovery_point"]["label"], "pre-upgrade");
    assert_eq!(backup_count(&data_home), 2);

    let uninstall_output = run(&user_home, &data_home, &["uninstall", "--non-interactive"]);
    assert!(uninstall_output.status.success());
    let uninstall = result(&uninstall_output);
    assert_eq!(uninstall["operation"], "uninstall");
    assert!(data_home.is_dir());
    assert!(data_home.join("hardknock.db").is_file());
    assert!(!data_home.join("setup/manifest-v1.json").exists());
    assert_eq!(backup_count(&data_home), 2);
}

#[test]
fn claude_setup_uses_the_stable_binary_and_preserves_user_hooks() {
    let (_temporary, user_home, data_home) = fixture();
    let claude = user_home.join(".claude");
    private_directory(&claude);
    let settings = claude.join("settings.json");
    fs::write(
        &settings,
        serde_json::to_vec_pretty(&json!({
            "model": "keep",
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{"type":"command","command":"echo user-hook"}]
                }]
            }
        }))
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(&settings, fs::Permissions::from_mode(0o600)).unwrap();

    for _ in 0..2 {
        let output = run(
            &user_home,
            &data_home,
            &[
                "setup",
                "--agent",
                "claude",
                "--mode",
                "ci",
                "--non-interactive",
            ],
        );
        assert_eq!(output.status.code(), Some(1));
    }
    let installed: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    assert_eq!(installed["model"], "keep");
    let groups = installed["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(groups.len(), 2);
    let hardknock_command = groups
        .iter()
        .flat_map(|group| group["hooks"].as_array().into_iter().flatten())
        .filter_map(|hook| hook["command"].as_str())
        .find(|command| command.contains("integration-event --agent claude"))
        .unwrap();
    assert!(hardknock_command.contains(env!("CARGO_BIN_EXE_hardknock")));

    let output = run(&user_home, &data_home, &["uninstall", "--non-interactive"]);
    assert!(output.status.success());
    let removed: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    assert_eq!(removed["model"], "keep");
    assert_eq!(
        removed["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "echo user-hook"
    );
    assert_eq!(removed["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
}

#[test]
fn failed_setup_restores_agent_configuration_and_managed_files() {
    let (_temporary, user_home, data_home) = fixture();
    private_directory(&data_home);
    fs::write(data_home.join("config.toml"), "not valid = [").unwrap();
    fs::set_permissions(
        data_home.join("config.toml"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let claude = user_home.join(".claude");
    private_directory(&claude);
    let settings = claude.join("settings.json");
    let original = serde_json::to_vec_pretty(&json!({"model":"unchanged"})).unwrap();
    fs::write(&settings, &original).unwrap();
    fs::set_permissions(&settings, fs::Permissions::from_mode(0o600)).unwrap();

    let output = run(
        &user_home,
        &data_home,
        &[
            "setup",
            "--agent",
            "claude",
            "--mode",
            "ci",
            "--non-interactive",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(fs::read(&settings).unwrap(), original);
    assert!(!data_home.join("setup/manifest-v1.json").exists());
    assert!(!data_home.join("integrations/claude.json").exists());
    assert!(
        fs::read_dir(data_home.parent().unwrap())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .contains("hardknock-setup"))
    );
}

#[test]
fn failed_new_setup_preserves_files_created_concurrently() {
    let (_temporary, user_home, data_home) = fixture();
    let manager_directory = user_home.join("fake-manager");
    private_directory(&manager_directory);
    let manager_script = format!(
        "#!/bin/sh\nprintf '%s\\n' keep > '{}'\nprintf '%s\\n' 'not valid = [' > '{}'\nexit 0\n",
        data_home.join("concurrent.txt").display(),
        data_home.join("config.toml").display()
    );
    for name in ["systemctl", "launchctl"] {
        let path = manager_directory.join(name);
        fs::write(&path, &manager_script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    let output = command(&user_home, &data_home)
        .env("PATH", &manager_directory)
        .env("XDG_CONFIG_HOME", user_home.join(".config"))
        .args([
            "setup",
            "--agent",
            "none",
            "--mode",
            "workstation",
            "--non-interactive",
            "--start",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        fs::read_to_string(data_home.join("concurrent.txt")).unwrap(),
        "keep\n"
    );
    assert_eq!(
        fs::read_to_string(data_home.join("config.toml")).unwrap(),
        "not valid = [\n"
    );
    assert!(!data_home.join("setup/manifest-v1.json").exists());
    assert!(data_home.join("hardknock.db").is_file());
}

#[test]
fn explicit_data_removal_requires_and_consumes_the_managed_manifest() {
    let (_temporary, user_home, data_home) = fixture();
    let setup_output = run(
        &user_home,
        &data_home,
        &[
            "setup",
            "--agent",
            "none",
            "--mode",
            "ci",
            "--non-interactive",
        ],
    );
    assert_eq!(setup_output.status.code(), Some(1));

    let uninstall_output = run(
        &user_home,
        &data_home,
        &["uninstall", "--non-interactive", "--remove-data"],
    );
    assert!(uninstall_output.status.success());
    let uninstall = result(&uninstall_output);
    assert!(!data_home.exists());
    let journal = PathBuf::from(uninstall["journal"].as_str().unwrap());
    assert!(journal.is_file());
    assert_eq!(
        journal.parent().unwrap().canonicalize().unwrap(),
        data_home.parent().unwrap().canonicalize().unwrap()
    );

    let unmanaged = user_home.join("unmanaged-data");
    private_directory(&unmanaged);
    let refused = run(
        &user_home,
        &unmanaged,
        &["uninstall", "--non-interactive", "--remove-data"],
    );
    assert_eq!(refused.status.code(), Some(5));
    assert!(unmanaged.is_dir());
}

#[test]
fn managed_home_and_journals_are_owned_and_private() {
    let (_temporary, user_home, data_home) = fixture();
    let output = run(
        &user_home,
        &data_home,
        &[
            "setup",
            "--agent",
            "none",
            "--mode",
            "ci",
            "--non-interactive",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let setup = result(&output);
    let journal = PathBuf::from(setup["journal"].as_str().unwrap());
    let effective_uid = nix::unistd::geteuid().as_raw();
    for path in [&data_home, &data_home.join("setup"), &journal] {
        let metadata = fs::metadata(path).unwrap();
        assert_eq!(metadata.uid(), effective_uid);
        assert_eq!(
            metadata.permissions().mode() & 0o022,
            0,
            "{}",
            path.display()
        );
    }
}
