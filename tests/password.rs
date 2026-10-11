mod common;

use common::{Env, PASSWORD};
use std::fs;
use std::time::Instant;

fn pw_lines(lines: &[&str]) -> String {
    lines.iter().map(|l| format!("{l}\n")).collect()
}

#[test]
fn correct_password_unlocks_and_wrong_password_keeps_lock() {
    let env = Env::new();
    let out = env.lock_with_password();
    assert!(out
        .stdout
        .contains("Unlocking requires the Clauck password"));
    let s = env.status_json();
    assert_eq!(s["password"]["configured"], true);
    assert_eq!(s["password"]["local_authorization"], "available");

    let before = common::snapshot(&env.project, &[".clauck/.lock"]);
    let out = env.run_stdin(&["unlock", "--password-stdin"], Some("wrong\n"));
    assert_eq!(out.code, 5, "{}", out.all());
    assert!(out.stderr.contains("E_AUTH_FAILED"));
    assert_eq!(before, common::snapshot(&env.project, &[".clauck/.lock"]));
    assert_eq!(env.status_json()["advisory_policy"]["state"], "locked");

    env.ok_stdin(&["unlock", "--password-stdin"], &pw_lines(&[PASSWORD]));
    assert_eq!(env.status_json()["advisory_policy"]["state"], "unlocked");
}

#[test]
fn empty_or_mismatched_passwords_change_nothing() {
    for input in [
        pw_lines(&["", ""]),
        pw_lines(&["one", "two"]),
        pw_lines(&["only-one"]),
    ] {
        let env = Env::new();
        let before = common::snapshot(&env.project, &[]);
        let out = env.run_stdin(&["lock", "--password", "--password-stdin"], Some(&input));
        assert_eq!(out.code, 13, "{}", out.all());
        assert!(
            out.stderr.contains("nothing was changed") || out.stderr.contains("not enough lines")
        );
        assert_eq!(
            before,
            common::snapshot(&env.project, &[]),
            "partial changes after rejected password"
        );
        assert!(!env.project.join(".clauck").exists());
    }
}

#[test]
fn plain_lock_never_downgrades_password_protection() {
    let env = Env::new();
    env.lock_with_password();
    env.ok(&["lock"]);
    assert_eq!(env.status_json()["password"]["configured"], true);
    let out = env.run(&["unlock"]);
    assert_eq!(out.code, 4, "{}", out.all());
    env.ok_stdin(&["unlock", "--password-stdin"], &pw_lines(&[PASSWORD]));
    env.ok(&["lock"]);
    let out = env.run(&["unlock"]);
    assert_eq!(
        out.code,
        4,
        "plain lock dropped the password: {}",
        out.all()
    );
    assert_eq!(env.status_json()["advisory_policy"]["state"], "locked");
}

#[test]
fn replacing_a_password_requires_the_current_one() {
    let env = Env::new();
    env.lock_with_password();
    let out = env.run_stdin(
        &["lock", "--password", "--password-stdin"],
        Some(&pw_lines(&["guess", "new", "new"])),
    );
    assert_eq!(out.code, 5, "{}", out.all());
    env.ok_stdin(
        &["lock", "--password", "--password-stdin"],
        &pw_lines(&[PASSWORD, "new pw", "new pw"]),
    );
    assert_eq!(
        env.run_stdin(
            &["unlock", "--password-stdin"],
            Some(&pw_lines(&[PASSWORD]))
        )
        .code,
        5
    );
    env.ok_stdin(&["unlock", "--password-stdin"], &pw_lines(&["new pw"]));
}

#[test]
fn remove_password_requires_authentication() {
    let env = Env::new();
    env.lock_with_password();
    env.ok_stdin(
        &["unlock", "--remove-password", "--password-stdin"],
        &pw_lines(&[PASSWORD]),
    );
    assert_eq!(env.status_json()["password"]["configured"], false);
    env.ok(&["lock"]);
    env.ok(&["unlock"]);
}

#[test]
fn noninteractive_unlock_fails_fast_instead_of_hanging() {
    let env = Env::new();
    env.lock_with_password();
    let start = Instant::now();
    let out = env.run(&["unlock"]);
    assert_eq!(out.code, 4, "{}", out.all());
    assert!(out.stderr.contains("E_AUTH_REQUIRED"));
    assert!(start.elapsed().as_secs() < 10);
    let out = env.run(&["unlock", "--non-interactive"]);
    assert_eq!(out.code, 4);
    assert_eq!(env.status_json()["advisory_policy"]["state"], "locked");
    let out = env.run(&["uninstall"]);
    assert_eq!(
        out.code,
        4,
        "uninstall must authenticate first: {}",
        out.all()
    );
    assert!(env.project.join(".clauck/state.json").exists());
}

#[test]
fn password_is_never_accepted_or_echoed_as_an_argument() {
    let env = Env::new();
    for args in [
        &["lock", "--password", "hunter2-secret"][..],
        &["lock", "--password=hunter2-secret"],
    ] {
        let out = env.run(args);
        assert_eq!(out.code, 2);
        assert!(
            !out.all().contains("hunter2-secret"),
            "password echoed: {}",
            out.all()
        );
        assert!(!env.project.join(".clauck").exists());
    }
}

#[test]
fn no_secrets_in_project_files_metadata_or_logs() {
    let env = Env::new().with_instructions();
    let mut logs = String::new();
    logs += &env
        .run_stdin(
            &[
                "lock",
                "--password",
                "--username",
                "alice",
                "--password-stdin",
            ],
            Some(&pw_lines(&[PASSWORD, PASSWORD])),
        )
        .all();
    logs += &env.run(&["status"]).all();
    logs += &env.run(&["status", "--json"]).all();
    logs += &env.run(&["doctor", "--skip-agent-versions"]).all();
    logs += &env
        .run_stdin(&["unlock", "--password-stdin"], Some("wrong\n"))
        .all();
    logs += &env
        .run_stdin(
            &["unlock", "--password-stdin"],
            Some(&pw_lines(&[PASSWORD])),
        )
        .all();
    let project = String::from_utf8_lossy(&env.all_project_bytes()).into_owned();
    for haystack in [&logs, &project] {
        assert!(!haystack.contains(PASSWORD), "password leaked");
        assert!(!haystack.contains("$argon2"), "verifier leaked");
    }
    let creds: Vec<_> = fs::read_dir(env.home.join("credentials"))
        .unwrap()
        .collect();
    assert_eq!(creds.len(), 1);
    let path = creds[0].as_ref().unwrap().path();
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("$argon2id$v=19$m=19456,t=2,p=1$"));
    assert!(!text.contains(PASSWORD));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&env.home).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    assert!(!project.contains("alice"));
}

#[test]
fn copied_project_reports_local_authorization_unavailable() {
    let env = Env::new();
    env.lock_with_password();
    let other = Env::new();
    fs::remove_dir_all(&other.project).unwrap();
    copy_dir(&env.project, &other.project);
    let s = other.status_json();
    assert_eq!(s["advisory_policy"]["state"], "locked");
    assert_eq!(s["password"]["local_authorization"], "unavailable");
    let text = other.ok(&["status"]).stdout;
    assert!(text.contains("local authorization unavailable"));

    let before = common::snapshot(&other.project, &[".clauck/.lock"]);
    let out = other.run_stdin(
        &["unlock", "--password-stdin"],
        Some(&pw_lines(&[PASSWORD])),
    );
    assert_eq!(out.code, 6, "{}", out.all());
    assert!(out.stderr.contains("local authorization unavailable"));
    assert!(out.stderr.contains("does not reset or invent credentials"));
    assert_eq!(before, common::snapshot(&other.project, &[".clauck/.lock"]));
    let doctor = other.run(&["doctor", "--skip-agent-versions"]);
    assert!(doctor.stdout.contains("local_authorization_unavailable"));
}

#[test]
fn editing_public_metadata_does_not_bypass_the_password() {
    let env = Env::new();
    env.lock_with_password();
    let tampered = env
        .read(".clauck/state.json")
        .replace("\"locked\": true", "\"locked\": false")
        .replace(
            "\"password_configured\": true",
            "\"password_configured\": false",
        );
    env.write(".clauck/state.json", &tampered);
    let s = env.status_json();
    assert_eq!(
        s["advisory_policy"]["state"], "locked",
        "local record keeps the lock"
    );
    assert_eq!(s["advisory_policy"]["metadata_matches_local_record"], false);
    assert_eq!(s["password"]["configured"], true);
    let out = env.run(&["unlock"]);
    assert_eq!(out.code, 4, "{}", out.all());
    let out = env.run(&["uninstall"]);
    assert_eq!(out.code, 4, "{}", out.all());
}

#[cfg(unix)]
#[test]
fn interactive_terminal_prompt_works() {
    let expect = std::path::Path::new("/usr/bin/expect");
    if !expect.exists() {
        eprintln!("skipping: /usr/bin/expect not available");
        return;
    }
    let env = Env::new();
    let bin = assert_cmd::cargo::cargo_bin("clauck");
    let script = format!(
        r#"set timeout 30
spawn {{{bin}}} --path {{{proj}}} lock --password
expect "New Clauck password: "
sleep 0.3
send "tty-pass\r"
expect "Confirm new Clauck password: "
sleep 0.3
send "tty-pass\r"
expect eof
spawn {{{bin}}} --path {{{proj}}} unlock
expect "Clauck password: "
sleep 0.3
send "tty-wrong\r"
expect eof
lassign [wait] pid spawnid os_error value
if {{$value != 5}} {{ puts "WRONG_EXIT $value"; exit 1 }}
spawn {{{bin}}} --path {{{proj}}} unlock
expect "Clauck password: "
sleep 0.3
send "tty-pass\r"
expect "Unlocked"
expect eof
"#,
        bin = bin.display(),
        proj = env.project.display()
    );
    let out = std::process::Command::new(expect)
        .arg("-c")
        .arg(&script)
        .env("CLAUCK_HOME", &env.home)
        .env("CLAUDE_CONFIG_DIR", &env.claude)
        .output()
        .unwrap();
    let all = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{all}");
    assert!(!all.contains("tty-pass"), "password was echoed: {all}");
    assert_eq!(env.status_json()["advisory_policy"]["state"], "unlocked");
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        let dest = to.join(p.file_name().unwrap());
        if p.is_dir() {
            copy_dir(&p, &dest);
        } else {
            fs::copy(&p, &dest).unwrap();
        }
    }
}
