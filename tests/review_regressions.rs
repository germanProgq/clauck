mod common;

use common::{Env, PASSWORD};
use serde_json::{json, Value};
use std::fs;

fn lines(items: &[&str]) -> String {
    items.iter().map(|l| format!("{l}\n")).collect()
}

fn sibling(env: &Env) -> Env {
    let other = Env::new();
    Env {
        home: env.home.clone(),
        claude: env.claude.clone(),
        ..other
    }
}

fn set_state(env: &Env, from: &str, to: &str) {
    let text = env.read(".clauck/state.json");
    assert!(text.contains(from), "{text}");
    env.write(".clauck/state.json", &text.replace(from, to));
}

fn project_id(env: &Env) -> String {
    env.status_json()["project"]["project_id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn init_cannot_lower_a_password_lock() {
    let env = Env::new();
    env.lock_with_password();
    set_state(&env, "\"locked\": true", "\"locked\": false");
    env.ok(&["init"]);
    assert_eq!(env.status_json()["advisory_policy"]["state"], "locked");
    assert!(env
        .read(".clauck/policy.md")
        .starts_with("# Clauck policy: LOCKED"));
    assert_eq!(env.run(&["unlock"]).code, 4);

    fs::remove_file(env.project.join(".clauck/state.json")).unwrap();
    env.ok(&["init"]);
    let s = env.status_json();
    assert_eq!(s["advisory_policy"]["state"], "locked");
    assert_eq!(s["password"]["configured"], true);
}

#[test]
fn swapped_project_id_cannot_borrow_another_password() {
    let p = Env::new();
    p.lock_with_password();
    let q = sibling(&p);
    q.ok_stdin(
        &["lock", "--password", "--password-stdin"],
        &lines(&["evilpw", "evilpw"]),
    );
    let (p_id, q_id) = (project_id(&p), project_id(&q));
    set_state(&p, &p_id, &q_id);

    let out = p.run_stdin(&["unlock", "--password-stdin"], Some("evilpw\n"));
    assert_eq!(out.code, 5, "{}", out.all());
    let out = p.run_stdin(
        &["uninstall", "--password-stdin"],
        Some(
            "evilpw
",
        ),
    );
    assert_eq!(out.code, 5, "{}", out.all());
    p.ok_stdin(&["unlock", "--password-stdin"], &lines(&[PASSWORD]));
    assert_eq!(project_id(&p), p_id);
    p.ok(&["lock"]);
    p.ok_stdin(&["unlock", "--password-stdin"], &lines(&[PASSWORD]));
    assert_eq!(
        q.run_stdin(
            &["unlock", "--password-stdin"],
            Some(&format!("{PASSWORD}\n"))
        )
        .code,
        5
    );
}

#[cfg(unix)]
#[test]
fn failed_lock_never_changes_the_password() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new().with_instructions();
    env.lock_with_password();
    env.ok_stdin(&["unlock", "--password-stdin"], &lines(&[PASSWORD]));
    let creds_before = common::snapshot(&env.home.join("credentials"), &[]);
    fs::remove_file(env.project.join("AGENTS.md")).unwrap();
    fs::set_permissions(&env.project, fs::Permissions::from_mode(0o555)).unwrap();
    let out = env.run_stdin(
        &["lock", "--password", "--password-stdin"],
        Some(&lines(&[PASSWORD, "new-pw", "new-pw"])),
    );
    fs::set_permissions(&env.project, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(out.code, 11, "{}", out.all());
    assert_eq!(
        creds_before,
        common::snapshot(&env.home.join("credentials"), &[])
    );
    env.ok(&["lock"]);
    env.ok_stdin(&["unlock", "--password-stdin"], &lines(&[PASSWORD]));

    let fresh = Env::new().with_instructions();
    fresh.ok(&["init"]);
    fs::remove_file(fresh.project.join("CLAUDE.md")).unwrap();
    fs::set_permissions(&fresh.project, fs::Permissions::from_mode(0o555)).unwrap();
    let out = fresh.run_stdin(
        &["lock", "--password", "--password-stdin"],
        Some(&lines(&["a", "a"])),
    );
    fs::set_permissions(&fresh.project, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(out.code, 11, "{}", out.all());
    assert_eq!(fresh.status_json()["password"]["configured"], false);
    assert!(
        fs::read_dir(fresh.home.join("credentials"))
            .map(|d| d.count())
            .unwrap_or(0)
            == 0
    );
}

#[test]
fn moved_project_with_damaged_metadata_does_not_skip_authentication() {
    let env = Env::new();
    env.lock_with_password();
    let moved = env.tmp.path().join("moved project");
    fs::rename(&env.project, &moved).unwrap();
    let env = Env {
        project: moved,
        ..env
    };
    set_state(&env, "\"locked\": true", "\"locked\": true, \"junk\": 1");
    let out = env.run(&["uninstall", "--non-interactive"]);
    assert_eq!(out.code, 4, "{}", out.all());
    env.write(".clauck/state.json", "garbage");
    let out = env.run(&["uninstall", "--non-interactive"]);
    assert_eq!(out.code, 12, "{}", out.all());
    assert!(env.project.join(".clauck/policy.md").exists());
}

#[test]
fn crlf_checkout_of_owned_files_is_not_a_hand_edit() {
    let env = Env::new();
    env.ok(&["lock"]);
    for f in [".clauck/policy.md", ".clauck/.gitignore"] {
        env.write(f, &env.read(f).replace('\n', "\r\n"));
    }
    env.ok(&["lock"]);
    env.ok(&["unlock"]);
    env.ok(&["uninstall"]);
    assert!(!env.project.join(".clauck").exists());
}

#[test]
fn missing_manifest_alone_is_not_a_conflict() {
    let env = Env::new();
    env.ok(&["lock"]);
    fs::remove_file(env.project.join(".clauck/manifest.json")).unwrap();
    env.ok(&["unlock"]);
    assert!(env
        .read(".clauck/policy.md")
        .starts_with("# Clauck policy: UNLOCKED"));
}

#[cfg(unix)]
#[test]
fn store_writable_by_others_is_not_trusted() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    env.lock_with_password();
    fs::set_permissions(&env.home, fs::Permissions::from_mode(0o777)).unwrap();
    let out = env.run_stdin(&["unlock", "--password-stdin"], Some(&lines(&[PASSWORD])));
    fs::set_permissions(&env.home, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(out.code, 6, "{}", out.all());
    assert!(out.stderr.contains("not a private directory"));
    assert_eq!(env.status_json()["advisory_policy"]["state"], "locked");
}

#[test]
fn password_stdin_value_is_not_echoed() {
    let env = Env::new();
    for args in [
        &["unlock", "--password-stdin=hunter2-secret"][..],
        &["lock", "--password", "-hunter2-secret"],
    ] {
        let out = env.run(args);
        assert_eq!(out.code, 2);
        assert!(!out.all().contains("hunter2-secret"), "{}", out.all());
    }
}

#[cfg(unix)]
#[test]
fn integration_install_keeps_settings_file_private() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    fs::create_dir_all(&env.claude).unwrap();
    let settings = env.claude.join("settings.json");
    fs::write(&settings, r#"{"env":{"TOKEN":"x"}}"#).unwrap();
    fs::set_permissions(&settings, fs::Permissions::from_mode(0o600)).unwrap();
    env.ok(&["integration", "install", "claude-code", "--yes"]);
    assert_eq!(
        fs::metadata(&settings).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

fn hook_from(
    env: &Env,
    cwd: &std::path::Path,
    tool: &str,
    tool_input: Value,
    strict: bool,
) -> bool {
    let mut args = vec!["hook", "claude-code"];
    if strict {
        args.push("--strict");
    }
    let input = json!({
        "session_id": "s1", "cwd": cwd, "hook_event_name": "PreToolUse",
        "tool_name": tool, "tool_input": tool_input
    });
    let out = assert_cmd::Command::cargo_bin("clauck")
        .unwrap()
        .env("CLAUCK_HOME", &env.home)
        .current_dir(cwd)
        .args(&args)
        .write_stdin(input.to_string())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    out.status.code() == Some(2) || stdout.contains("\"deny\"")
}

#[test]
fn shell_and_mcp_arguments_naming_a_locked_project_are_denied() {
    let locked = Env::new();
    locked.ok(&["lock"]);
    let elsewhere = sibling(&locked);
    let root = locked.project.canonicalize().unwrap();
    let cmd = format!("rm -rf \"{}/src\"", root.display());
    assert!(hook_from(
        &locked,
        &elsewhere.project,
        "Bash",
        json!({ "command": cmd }),
        false
    ));
    let mcp = json!({ "source": "/tmp/x", "destination": root.join("src/app.py") });
    assert!(hook_from(
        &locked,
        &elsewhere.project,
        "mcp__fs__move",
        mcp,
        false
    ));
    assert!(!hook_from(
        &locked,
        &elsewhere.project,
        "Bash",
        json!({ "command": "make test" }),
        false
    ));
    assert!(hook_from(
        &locked,
        &elsewhere.project,
        "Bash",
        json!({ "command": "make test" }),
        true
    ));
    assert!(!hook_from(
        &locked,
        &elsewhere.project,
        "Read",
        json!({ "file_path": "/etc/hosts" }),
        true
    ));
}

#[test]
fn nested_unlocked_metadata_does_not_hide_a_locked_parent() {
    let env = Env::new();
    env.ok(&["lock"]);
    let sub = env.project.join("sub");
    fs::create_dir_all(sub.join(".clauck")).unwrap();
    fs::write(
        sub.join(".clauck/state.json"),
        r#"{"schema":"clauck.state","version":1,"project_id":"0123456789abcdef0123456789abcdef","locked":false,"password_configured":false,"policy_revision":1}"#,
    )
    .unwrap();
    assert!(hook_from(
        &env,
        &sub,
        "Write",
        json!({ "file_path": sub.join("x.txt"), "content": "x" }),
        false
    ));
}

#[cfg(unix)]
#[test]
fn writes_through_symlinks_into_a_locked_project_are_denied() {
    let locked = Env::new();
    locked.ok(&["lock"]);
    let elsewhere = Env::new();
    let link = elsewhere.project.join("link.py");
    std::os::unix::fs::symlink(locked.project.join("src/app.py"), &link).unwrap();
    assert!(hook_from(
        &locked,
        &elsewhere.project,
        "Write",
        json!({ "file_path": link, "content": "x" }),
        false
    ));
}

#[test]
fn searching_a_folder_that_contains_the_store_is_denied() {
    let env = Env::new();
    env.lock_with_password();
    let parent = env.home.parent().unwrap().to_path_buf();
    let grep = json!({ "pattern": "argon2id", "path": parent });
    assert!(hook_from(&env, &env.project, "Grep", grep, false));
    let cat = format!("cat '{}/credentials/'*", env.home.display());
    assert!(hook_from(
        &env,
        &env.project,
        "Bash",
        json!({ "command": cat }),
        false
    ));
    assert!(!hook_from(
        &env,
        &env.project,
        "Grep",
        json!({ "pattern": "x", "path": env.project.join("src") }),
        false
    ));
}
