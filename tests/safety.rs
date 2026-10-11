mod common;

use clauck::fsutil::sha256_hex;
use clauck::project::Project;
use clauck::txn::{self, Change, ChangeKind, ProjectLock};
use common::Env;
use std::fs;

#[test]
fn malformed_state_is_reported_and_never_overwritten() {
    let env = Env::new();
    env.ok(&["init"]);
    for bad in [
        "{not json",
        "[]",
        "",
        r#"{"schema":"clauck.state","version":99}"#,
    ] {
        env.write(".clauck/state.json", bad);
        let before = common::snapshot(&env.project, &[".clauck/.lock"]);
        for args in [&["lock"][..], &["unlock"], &["status"], &["init"]] {
            let out = env.run(args);
            assert_eq!(out.code, 12, "clauck {args:?} on {bad:?}: {}", out.all());
            assert!(out.stderr.contains("E_INVALID_METADATA"));
        }
        assert_eq!(before, common::snapshot(&env.project, &[".clauck/.lock"]));
        let doctor = env.run(&["doctor", "--skip-agent-versions"]);
        assert_eq!(doctor.code, 16);
        assert!(doctor.stdout.contains("invalid_state"));
    }
}

#[test]
fn oversized_metadata_is_rejected_without_reading_it_all() {
    let env = Env::new();
    env.ok(&["init"]);
    let huge = format!("{{\"pad\":\"{}\"}}", "x".repeat(400 * 1024));
    env.write(".clauck/manifest.json", &huge);
    let out = env.run(&["lock"]);
    assert_eq!(out.code, 12, "{}", out.all());
    assert!(out.stderr.contains("exceeds"));
    env.write(".clauck/manifest.json", "{}");
    env.write(".clauck/state.json", &huge);
    assert_eq!(env.run(&["status"]).code, 12);
}

#[test]
fn oversized_instruction_file_is_rejected() {
    let env = Env::new();
    env.write("AGENTS.md", &"a".repeat(3 * 1024 * 1024));
    let out = env.run(&["lock"]);
    assert_eq!(out.code, 12, "{}", out.all());
    assert_eq!(
        fs::metadata(env.project.join("AGENTS.md")).unwrap().len(),
        3 * 1024 * 1024
    );
}

#[test]
fn manifest_with_foreign_paths_is_rejected() {
    let env = Env::new();
    env.ok(&["init"]);
    env.write(
        ".clauck/manifest.json",
        r#"{"schema":"clauck.manifest","version":1,"entries":[{"path":"../outside.txt","kind":"file","created_file":true,"sha256":null}]}"#,
    );
    let out = env.run(&["uninstall"]);
    assert_eq!(out.code, 0, "{}", out.all());
    assert!(env
        .tmp
        .path()
        .read_dir()
        .unwrap()
        .all(|e| e.unwrap().file_name() != "outside.txt"));
    let env2 = Env::new();
    env2.ok(&["init"]);
    env2.write(".clauck/manifest.json", r#"{"schema":"clauck.manifest","version":1,"entries":[{"path":"src/app.py","kind":"file","created_file":true,"sha256":null}]}"#);
    assert_eq!(env2.run(&["lock"]).code, 12);
    assert!(env2.read("src/app.py").contains("def add"));
}

#[test]
fn missing_manifest_and_policy_are_rebuilt() {
    let env = Env::new().with_instructions();
    env.ok(&["lock"]);
    fs::remove_file(env.project.join(".clauck/manifest.json")).unwrap();
    fs::remove_file(env.project.join(".clauck/policy.md")).unwrap();
    env.ok(&["lock"]);
    assert!(env
        .read(".clauck/policy.md")
        .starts_with("# Clauck policy: LOCKED"));
    assert_eq!(common::count(&env.read("AGENTS.md"), "clauck:begin"), 1);
    assert_eq!(env.run(&["doctor", "--skip-agent-versions"]).code, 0);
}

#[test]
fn deleted_state_reuses_project_identity_from_local_record() {
    let env = Env::new();
    env.lock_with_password();
    let id = env.status_json()["project"]["project_id"].clone();
    fs::remove_file(env.project.join(".clauck/state.json")).unwrap();
    env.ok(&["lock"]);
    let s = env.status_json();
    assert_eq!(s["project"]["project_id"], id);
    assert_eq!(
        s["password"]["configured"], true,
        "deleting state.json must not drop the password"
    );
}

fn plan_lock(env: &Env) -> (Project, Vec<Change>) {
    let project = Project::open(Some(&env.project)).unwrap();
    let st = clauck::state::load(&project)
        .unwrap()
        .unwrap()
        .with_lock(true);
    let manifest = clauck::manifest::load(&project).unwrap().unwrap();
    let plan = clauck::plan::plan_sync(&project, &st, &manifest, false).unwrap();
    (project, plan.changes)
}

#[test]
fn interrupted_operation_is_completed_by_next_command() {
    let env = Env::new().with_instructions();
    env.ok(&["init"]);
    let (project, changes) = plan_lock(&env);
    drop(txn::prepare(&project, "lock", &changes).unwrap());
    assert!(env.project.join(".clauck/journal.json").exists());
    let s = env.status_json();
    assert_eq!(s["pending_recovery"], true);
    let out = env.ok(&["unlock"]);
    assert!(
        out.stderr
            .contains("Recovered an interrupted `clauck lock`"),
        "{}",
        out.all()
    );
    assert!(!env.project.join(".clauck/journal.json").exists());
    assert!(txn::stray_temp_files(&env.project).is_empty());
    assert!(txn::stray_temp_files(&env.project.join(".clauck")).is_empty());
    assert_eq!(env.status_json()["advisory_policy"]["state"], "unlocked");
    assert_eq!(env.run(&["doctor", "--skip-agent-versions"]).code, 0);
}

#[test]
fn recovery_does_not_overwrite_edits_made_after_the_crash() {
    let env = Env::new().with_instructions();
    env.ok(&["init"]);
    let (project, _) = plan_lock(&env);
    let agents_before = env.read("AGENTS.md");
    let change = Change {
        path: "AGENTS.md",
        kind: ChangeKind::Write(b"clauck version\n".to_vec()),
        before_sha256: Some(sha256_hex(agents_before.as_bytes())),
    };
    drop(txn::prepare(&project, "lock", &[change]).unwrap());
    env.write("AGENTS.md", "a person rewrote this after the crash\n");
    let out = env.ok(&["status"]);
    assert!(out.stdout.contains("interrupted operation"));
    let out = env.ok(&["init"]);
    assert!(
        out.stderr.contains("changed by someone else"),
        "{}",
        out.all()
    );
    assert!(env
        .read("AGENTS.md")
        .starts_with("a person rewrote this after the crash\n"));
    assert!(txn::stray_temp_files(&env.project).is_empty());
}

#[test]
fn uncommitted_temp_files_are_cleaned_up() {
    let env = Env::new();
    env.ok(&["init"]);
    let tmp = clauck::fsutil::stage_file(&env.project.join("AGENTS.md"), b"half-written").unwrap();
    assert!(tmp.exists());
    env.ok(&["lock"]);
    assert!(!tmp.exists());
    assert!(!env.read("AGENTS.md").contains("half-written"));
}

#[test]
fn concurrent_modifying_commands_are_rejected() {
    let env = Env::new();
    env.ok(&["init"]);
    let project = Project::open(Some(&env.project)).unwrap();
    let held = ProjectLock::acquire(&project).unwrap();
    let out = env.run(&["lock"]);
    assert_eq!(out.code, 8, "{}", out.all());
    assert!(out.stderr.contains("E_BUSY"));
    env.ok(&["status"]);
    drop(held);
    env.ok(&["lock"]);
}

#[cfg(unix)]
#[test]
fn read_only_project_fails_cleanly() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new().with_instructions();
    if std::env::var("USER").as_deref() == Ok("root") {
        return;
    }
    let before = common::snapshot(&env.project, &[]);
    fs::set_permissions(&env.project, fs::Permissions::from_mode(0o555)).unwrap();
    let out = env.run(&["lock"]);
    fs::set_permissions(&env.project, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(out.code, 11, "{}", out.all());
    assert!(out.stderr.contains("E_READ_ONLY"));
    assert_eq!(before, common::snapshot(&env.project, &[]));

    env.ok(&["init"]);
    let before = common::snapshot(&env.project, &[".clauck/.lock"]);
    fs::set_permissions(
        env.project.join(".clauck"),
        fs::Permissions::from_mode(0o555),
    )
    .unwrap();
    let out = env.run(&["lock"]);
    fs::set_permissions(
        env.project.join(".clauck"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert_eq!(out.code, 11, "{}", out.all());
    assert_eq!(before, common::snapshot(&env.project, &[".clauck/.lock"]));
}

#[cfg(unix)]
#[test]
fn symlinks_are_never_written_through() {
    let env = Env::new();
    let outside = env.tmp.path().join("outside-notes.md");
    fs::write(&outside, "outside content\n").unwrap();
    std::os::unix::fs::symlink(&outside, env.project.join("CLAUDE.md")).unwrap();
    let out = env.ok(&["lock"]);
    assert!(out.stderr.contains("symbolic link"), "{}", out.all());
    assert_eq!(fs::read_to_string(&outside).unwrap(), "outside content\n");
    env.ok(&["uninstall"]);
    assert_eq!(fs::read_to_string(&outside).unwrap(), "outside content\n");
    assert!(fs::symlink_metadata(env.project.join("CLAUDE.md"))
        .unwrap()
        .file_type()
        .is_symlink());

    let env2 = Env::new();
    let elsewhere = env2.tmp.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, env2.project.join(".clauck")).unwrap();
    let out = env2.run(&["lock"]);
    assert_eq!(out.code, 9, "{}", out.all());
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
}

#[test]
fn rejects_missing_project_directory() {
    let env = Env::new();
    let out = assert_cmd::Command::cargo_bin("clauck")
        .unwrap()
        .env("CLAUCK_HOME", &env.home)
        .arg("--path")
        .arg(env.tmp.path().join("missing"))
        .arg("lock")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(13));
    assert!(String::from_utf8_lossy(&out.stderr).contains("E_INVALID_INPUT"));
}
