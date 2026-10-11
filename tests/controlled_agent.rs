mod common;

use common::{Env, PASSWORD};
use serde_json::{json, Value};
use std::fs;

fn hook(env: &Env, tool: &str, session: &str, tool_input: Value) -> common::Out {
    let input = json!({
        "session_id": session,
        "transcript_path": "/dev/null",
        "cwd": env.project,
        "permission_mode": "default",
        "hook_event_name": "PreToolUse",
        "tool_name": tool,
        "tool_input": tool_input,
        "tool_use_id": "toolu_test"
    });
    let out = assert_cmd::Command::cargo_bin("clauck")
        .unwrap()
        .env("CLAUCK_HOME", &env.home)
        .env("CLAUDE_CONFIG_DIR", &env.claude)
        .current_dir(&env.project)
        .args(["hook", "claude-code"])
        .write_stdin(input.to_string())
        .output()
        .unwrap();
    common::Out {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn denied(out: &common::Out) -> bool {
    if out.code == 2 {
        return true;
    }
    assert_eq!(out.code, 0, "{}", out.all());
    if out.stdout.trim().is_empty() {
        return false;
    }
    let v: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    v["hookSpecificOutput"]["permissionDecision"] == "deny"
}

fn edit(env: &Env) -> Value {
    json!({ "file_path": env.project.join("src/app.py"), "old_string": "a", "new_string": "b" })
}

fn install(env: &Env) {
    let out = env.ok(&["integration", "install", "claude-code", "--yes"]);
    assert!(
        out.stdout.contains("hook claude-code"),
        "install must show what executes"
    );
}

#[test]
fn install_requires_consent_and_shows_what_will_run() {
    let env = Env::new();
    let out = env.run(&["integration", "install", "claude-code"]);
    assert_eq!(out.code, 15, "{}", out.all());
    assert!(out.stdout.contains("It will execute"));
    assert!(!env.claude.join("settings.json").exists());
    let out = env.ok(&["integration", "install", "claude-code", "--dry-run"]);
    assert!(out.stdout.contains("Dry run"));
    assert!(!env.claude.join("settings.json").exists());
    install(&env);
    let settings: Value =
        serde_json::from_str(&fs::read_to_string(env.claude.join("settings.json")).unwrap())
            .unwrap();
    let handler = &settings["hooks"]["PreToolUse"][0]["hooks"][0];
    assert_eq!(handler["type"], "command");
    assert!(handler["command"]
        .as_str()
        .unwrap()
        .ends_with("hook claude-code"));
    assert!(!env.project.join(".claude").exists());
}

#[test]
fn integration_uninstall_keeps_other_settings() {
    let env = Env::new();
    fs::create_dir_all(&env.claude).unwrap();
    fs::write(env.claude.join("settings.json"), r#"{"theme":"dark","hooks":{"Stop":[{"hooks":[{"type":"command","command":"say done"}]}]}}"#).unwrap();
    install(&env);
    install(&env);
    env.ok(&["integration", "uninstall", "claude-code"]);
    let v: Value =
        serde_json::from_str(&fs::read_to_string(env.claude.join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(
        v,
        json!({"theme":"dark","hooks":{"Stop":[{"hooks":[{"type":"command","command":"say done"}]}]}})
    );
}

#[test]
fn status_reports_hook_separately_and_as_partial() {
    let env = Env::new();
    env.ok(&["lock"]);
    install(&env);
    let s = env.status_json();
    let hook = &s["integrations"][2];
    assert_eq!(hook["id"], "claude-code-hook");
    assert_eq!(hook["installed"], true);
    assert_eq!(hook["verified"], true);
    assert_eq!(hook["enforcement"], "partial");
    assert_eq!(s["advisory_policy"]["enforced"], false);
}

#[test]
fn locked_project_denies_write_paths_and_allows_reads() {
    let env = Env::new();
    env.ok(&["lock"]);
    for tool in [
        "Edit",
        "Write",
        "NotebookEdit",
        "Bash",
        "PowerShell",
        "Monitor",
        "mcp__fs__write_file",
        "SomeFutureTool",
    ] {
        let out = hook(&env, tool, "s1", edit(&env));
        assert!(denied(&out), "{tool} should be denied while locked");
        if out.code == 0 {
            assert!(out
                .stdout
                .contains("AI edits are restricted for this project. Authorization is required."));
            assert!(out.stdout.contains("clauck authorize --session s1"));
        }
    }
    for tool in ["Read", "Glob", "Grep", "WebSearch", "Agent"] {
        let out = hook(
            &env,
            tool,
            "s1",
            json!({ "file_path": env.project.join("src/app.py") }),
        );
        assert!(!denied(&out), "{tool} should be allowed while locked");
        assert!(
            out.stdout.is_empty(),
            "allowed calls defer to the host, never auto-approve"
        );
    }
}

#[test]
fn unlocked_and_unrelated_projects_are_not_affected() {
    let env = Env::new();
    env.ok(&["init"]);
    assert!(!denied(&hook(&env, "Edit", "s1", edit(&env))));
    assert!(!denied(&hook(
        &env,
        "Bash",
        "s1",
        json!({ "command": "make" })
    )));
    env.ok(&["lock"]);
    assert!(denied(&hook(
        &env,
        "Bash",
        "s1",
        json!({ "command": "make" })
    )));
    env.ok(&["unlock"]);
    assert!(!denied(&hook(
        &env,
        "Bash",
        "s1",
        json!({ "command": "make" })
    )));
    let plain = Env::new();
    assert!(!denied(&hook(&plain, "Write", "s1", edit(&plain))));
}

#[test]
fn file_paths_into_a_locked_project_are_denied_from_elsewhere() {
    let locked = Env::new();
    locked.ok(&["lock"]);
    let other = Env::new();
    let out = hook(
        &other,
        "Write",
        "s1",
        json!({ "file_path": locked.project.join("src/new.py"), "content": "x" }),
    );
    assert!(denied(&out), "{}", out.all());
}

#[test]
fn editing_public_metadata_does_not_unlock_the_hook() {
    let env = Env::new();
    env.ok(&["lock"]);
    let tampered = env
        .read(".clauck/state.json")
        .replace("\"locked\": true", "\"locked\": false");
    env.write(".clauck/state.json", &tampered);
    assert!(denied(&hook(&env, "Edit", "s1", edit(&env))));
    env.write(".clauck/state.json", "garbage");
    assert!(denied(&hook(&env, "Edit", "s1", edit(&env))));
}

#[test]
fn malformed_hook_input_fails_closed_near_a_project() {
    let env = Env::new();
    env.ok(&["lock"]);
    for input in ["not json", "{}", ""] {
        let out = assert_cmd::Command::cargo_bin("clauck")
            .unwrap()
            .env("CLAUCK_HOME", &env.home)
            .current_dir(&env.project)
            .args(["hook", "claude-code"])
            .write_stdin(input)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "input {input:?}");
    }
}

#[test]
fn reading_the_credential_store_is_denied() {
    let env = Env::new();
    env.lock_with_password();
    let cred = fs::read_dir(env.home.join("credentials"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let out = hook(&env, "Read", "s1", json!({ "file_path": cred }));
    assert!(denied(&out), "{}", out.all());
}

#[test]
fn session_grant_is_scoped_expiring_revocable_and_reset_by_lock() {
    let env = Env::new();
    env.lock_with_password();
    let out = env.run_stdin(
        &["authorize", "--session", "sess-A", "--password-stdin"],
        Some(&format!("{PASSWORD}\n")),
    );
    assert_eq!(out.code, 14, "{}", out.all());
    install(&env);

    let out = env.run_stdin(
        &["authorize", "--session", "sess-A", "--password-stdin"],
        Some("I already unlocked it\n"),
    );
    assert_eq!(out.code, 5, "{}", out.all());
    let out = env.run(&["authorize", "--session", "sess-A"]);
    assert_eq!(
        out.code,
        4,
        "noninteractive authorize must not hang: {}",
        out.all()
    );
    env.ok_stdin(
        &[
            "authorize",
            "--session",
            "sess-A",
            "--ttl",
            "15m",
            "--password-stdin",
        ],
        &format!("{PASSWORD}\n"),
    );
    assert_eq!(env.status_json()["active_session_grants"], 1);

    assert!(
        !denied(&hook(&env, "Edit", "sess-A", edit(&env))),
        "granted session may edit"
    );
    assert!(
        denied(&hook(&env, "Edit", "sess-B", edit(&env))),
        "other sessions stay locked"
    );
    assert_eq!(env.status_json()["advisory_policy"]["state"], "locked");

    env.ok(&["revoke", "--session", "sess-A"]);
    assert!(
        denied(&hook(&env, "Edit", "sess-A", edit(&env))),
        "revocation is immediate"
    );

    env.ok_stdin(
        &["authorize", "--session", "sess-A", "--password-stdin"],
        &format!("{PASSWORD}\n"),
    );
    assert!(!denied(&hook(&env, "Edit", "sess-A", edit(&env))));
    env.ok(&["lock"]);
    assert!(
        denied(&hook(&env, "Edit", "sess-A", edit(&env))),
        "locking again invalidates grants"
    );
    assert_eq!(env.status_json()["active_session_grants"], 0);
}

#[test]
fn expired_grant_is_rejected() {
    let env = Env::new();
    env.ok(&["lock"]);
    install(&env);
    env.ok(&["authorize", "--session", "sess-T", "--ttl", "1m"]);
    assert!(!denied(&hook(&env, "Write", "sess-T", edit(&env))));
    let dir = fs::read_dir(env.home.join("grants"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let file = fs::read_dir(&dir).unwrap().next().unwrap().unwrap().path();
    let mut grant: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    grant["expires_unix"] = json!(1);
    fs::write(&file, grant.to_string()).unwrap();
    assert!(denied(&hook(&env, "Write", "sess-T", edit(&env))));
}

#[test]
fn authorize_rejects_bad_input_and_unlocked_projects() {
    let env = Env::new();
    env.ok(&["init"]);
    install(&env);
    assert_eq!(env.run(&["authorize", "--session", "x"]).code, 17);
    env.ok(&["lock"]);
    assert_eq!(env.run(&["authorize", "--session", "../../etc"]).code, 13);
    assert_eq!(
        env.run(&["authorize", "--session", "ok", "--ttl", "7d"])
            .code,
        13
    );
}
