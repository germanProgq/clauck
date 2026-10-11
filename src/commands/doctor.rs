use serde::Serialize;

use crate::adapters::claude_code;
use crate::auth::{self, Requirement};
use crate::blocks;
use crate::cli::{DoctorArgs, GlobalOpts};
use crate::commands::{status, store};
use crate::diagnostics::agent_versions;
use crate::error::{ClauckError, ErrorCode, Result};
use crate::fsutil;
use crate::manifest;
use crate::plan::{read_entry, EntryRead};
use crate::policy;
use crate::project::{Project, AGENTS_MD, CLAUDE_DIR_MD, CLAUDE_LOCAL_MD, CLAUDE_MD, POLICY_FILE};
use crate::state::{self, ProjectState};
use crate::store::{ledger, Store};
use crate::txn;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Serialize)]
pub struct Finding {
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
}

#[derive(Default)]
struct Findings(Vec<Finding>);

impl Findings {
    fn add(&mut self, severity: Severity, code: &'static str, message: impl Into<String>) {
        self.0.push(Finding {
            severity,
            code,
            message: message.into(),
        });
    }
}

pub fn run(g: &GlobalOpts, args: &DoctorArgs) -> Result<i32> {
    let project = Project::open(g.path.as_deref())?;
    let store = store();
    let findings = diagnose(&project, store.as_ref(), !args.skip_agent_versions);
    let errors = findings
        .iter()
        .filter(|f| f.severity == Severity::Error)
        .count();
    if args.json {
        let out = serde_json::json!({ "schema": "clauck.doctor", "version": 1, "errors": errors, "findings": findings });
        println!("{}", serde_json::to_string_pretty(&out).expect("json"));
    } else {
        for f in &findings {
            let tag = match f.severity {
                Severity::Error => "error  ",
                Severity::Warning => "warning",
                Severity::Info => "info   ",
            };
            println!("{tag} {:<28} {}", f.code, f.message);
        }
        if findings.iter().all(|f| f.severity == Severity::Info) {
            println!("No problems found.");
        }
    }
    if errors > 0 {
        let err = ClauckError::new(
            ErrorCode::DoctorProblems,
            format!("{errors} problem(s) need attention"),
        );
        eprintln!("clauck: {err}");
        return Ok(ErrorCode::DoctorProblems.exit_code());
    }
    Ok(0)
}

pub fn diagnose(project: &Project, store: Option<&Store>, check_agents: bool) -> Vec<Finding> {
    let mut f = Findings::default();
    let st = match state::load(project) {
        Ok(Some(s)) => Some(s),
        Ok(None) => {
            f.add(
                Severity::Warning,
                "not_initialized",
                "no .clauck/state.json; run `clauck init` or `clauck lock`",
            );
            None
        }
        Err(e) => {
            f.add(Severity::Error, "invalid_state", e.message);
            None
        }
    };
    let manifest = match manifest::load(project) {
        Ok(Some(m)) => Some(m),
        Ok(None) => {
            if st.is_some() {
                f.add(
                    Severity::Warning,
                    "missing_manifest",
                    ".clauck/manifest.json is missing; `clauck init` rebuilds it",
                );
            }
            None
        }
        Err(e) => {
            f.add(Severity::Error, "invalid_manifest", e.message);
            None
        }
    };
    if let Some(st) = &st {
        check_policy(project, st, manifest.as_ref(), &mut f);
        check_blocks(project, &mut f);
    }
    check_conflicting_instructions(project, &mut f);
    check_local_auth(project, store, st.as_ref(), &mut f);
    check_recovery(project, &mut f);
    check_hook(&mut f);
    if check_agents {
        for (sev, code, msg) in agent_versions::check() {
            f.add(sev, code, msg);
        }
    }
    f.0
}

fn check_policy(
    project: &Project,
    st: &ProjectState,
    m: Option<&manifest::Manifest>,
    f: &mut Findings,
) {
    let path = project.root.join(POLICY_FILE);
    match fsutil::read_bounded_string(&path, fsutil::MAX_INSTRUCTION_BYTES) {
        Ok(None) => f.add(
            Severity::Error,
            "missing_policy",
            ".clauck/policy.md is missing; agents have nothing to consult. Run `clauck init`.",
        ),
        Err(e) => f.add(Severity::Error, "unreadable_policy", e.message),
        Ok(Some(text)) => {
            match policy::parse_header(&text) {
                Some((locked, rev)) if locked == st.locked && rev == st.policy_revision => {}
                Some(_) | None => f.add(
                    Severity::Error,
                    "stale_policy",
                    format!(
                        ".clauck/policy.md does not match state.json ({}); rerun `clauck {}`",
                        if st.locked { "locked" } else { "unlocked" },
                        if st.locked { "lock" } else { "init" }
                    ),
                ),
            }
            let recorded = m
                .and_then(|m| m.get(POLICY_FILE))
                .and_then(|e| e.sha256.clone());
            if recorded
                .as_deref()
                .is_some_and(|h| h != fsutil::sha256_hex(text.as_bytes()))
            {
                f.add(Severity::Warning, "policy_edited", ".clauck/policy.md was edited by hand; the next lock/unlock will stop with a conflict");
            }
        }
    }
}

fn check_blocks(project: &Project, f: &mut Findings) {
    let report = status::collect(project, None);
    let Ok(report) = report else { return };
    let mut any_verified = false;
    for i in report.integrations.iter().filter(|i| i.kind == "advisory") {
        any_verified |= i.verified;
        if !i.installed {
            f.add(
                Severity::Warning,
                "missing_block",
                format!("{}: {}", i.id, i.detail),
            );
        } else if !i.verified {
            f.add(
                Severity::Warning,
                "modified_block",
                format!("{}: {}", i.id, i.detail),
            );
        }
    }
    if !any_verified {
        f.add(
            Severity::Error,
            "no_entry_point",
            "no verified AGENTS.md or CLAUDE.md block references the policy",
        );
    }
}

fn check_conflicting_instructions(project: &Project, f: &mut Findings) {
    let mut imports = 0;
    for rel in [AGENTS_MD, CLAUDE_MD, CLAUDE_LOCAL_MD, CLAUDE_DIR_MD] {
        let text = match read_entry(project, rel) {
            Ok(EntryRead::Text(t)) => t,
            Ok(EntryRead::Symlink) => {
                f.add(
                    Severity::Warning,
                    "symlinked_entry_file",
                    format!("{rel} is a symbolic link; Clauck does not manage it"),
                );
                continue;
            }
            Ok(EntryRead::Missing) => continue,
            Err(e) => {
                f.add(
                    Severity::Error,
                    "unreadable_entry_file",
                    format!("{rel}: {}", e.message),
                );
                continue;
            }
        };
        let outside = match blocks::find(&text) {
            Ok(Some(span)) => {
                if rel == CLAUDE_MD {
                    imports += span.text(&text).matches("@.clauck/policy.md").count();
                }
                blocks::remove(&text, &span)
            }
            Ok(None) => text.clone(),
            Err(e) => {
                f.add(
                    Severity::Error,
                    "damaged_markers",
                    format!("{rel}: {}", e.message),
                );
                continue;
            }
        };
        imports += outside.matches("@.clauck/policy.md").count();
        if outside.to_lowercase().contains("clauck") {
            f.add(
                Severity::Warning,
                "possible_conflicting_instructions",
                format!("{rel} mentions Clauck outside the managed block; check that it does not contradict the policy"),
            );
        }
    }
    if imports > 1 {
        f.add(
            Severity::Warning,
            "duplicate_import",
            format!("the policy is imported {imports} times across Claude memory files"),
        );
    }
}

fn check_local_auth(
    project: &Project,
    store: Option<&Store>,
    st: Option<&ProjectState>,
    f: &mut Findings,
) {
    let Some(store) = store else {
        f.add(
            Severity::Warning,
            "no_local_store",
            "cannot locate the per-user Clauck store",
        );
        return;
    };
    if store.exists() && fsutil::is_group_or_world_accessible(&store.root) {
        f.add(
            Severity::Warning,
            "store_permissions",
            format!(
                "{} is accessible to other users; expected 0700",
                store.display()
            ),
        );
    }
    let record = match ledger::load(store, project) {
        Ok(r) => r,
        Err(e) => {
            f.add(Severity::Error, "invalid_local_record", e.message);
            None
        }
    };
    match auth::requirement(Some(store), st, record.as_ref()) {
        Ok(Requirement::Unavailable) => f.add(
            Severity::Warning,
            "local_authorization_unavailable",
            "local authorization unavailable: password protection is configured but this machine has no verifier (copied project?)",
        ),
        Ok(Requirement::Password(_)) if st.is_some_and(|s| !s.password_configured) => f.add(
            Severity::Warning,
            "metadata_password_mismatch",
            "a local password exists but state.json says none; the password still applies",
        ),
        Ok(_) => {}
        Err(e) => f.add(Severity::Error, "unreadable_credential", e.message),
    }
    if let (Some(st), Some(r)) = (st, &record) {
        if st.locked != r.locked {
            f.add(
                Severity::Warning,
                "metadata_mismatch",
                "state.json lock flag differs from the local record (edited outside Clauck)",
            );
        }
        if st.project_id != r.project_id {
            f.add(
                Severity::Warning,
                "project_id_mismatch",
                "state.json project_id differs from the local record",
            );
        }
        for other in ledger::other_roots_with_id(store, project, &st.project_id) {
            f.add(
                Severity::Info,
                "copied_project",
                format!("same project id also used at {other}"),
            );
        }
    } else if st.is_some() && record.is_none() {
        f.add(
            Severity::Info,
            "no_local_record",
            "no local record for this location (new copy, or created on another machine)",
        );
    }
}

fn check_recovery(project: &Project, f: &mut Findings) {
    if txn::journal_pending(project) {
        f.add(
            Severity::Warning,
            "interrupted_operation",
            "an interrupted operation is pending; any modifying command completes it",
        );
    }
    let stray = txn::stray_temp_files(&project.root).len()
        + txn::stray_temp_files(&project.root.join(".clauck")).len();
    if stray > 0 {
        f.add(
            Severity::Warning,
            "temporary_files",
            format!("{stray} leftover temporary file(s); the next modifying command removes them"),
        );
    }
}

fn check_hook(f: &mut Findings) {
    let Ok(path) = claude_code::settings_path() else {
        return;
    };
    match claude_code::installed_commands(&path) {
        Err(e) => f.add(Severity::Warning, "unreadable_claude_settings", e.message),
        Ok(cmds) => {
            for c in cmds {
                if !claude_code::command_executable(&c).is_some_and(|p| p.is_file()) {
                    f.add(
                        Severity::Error,
                        "hook_executable_missing",
                        format!("Claude Code hook points to a missing executable: {c}"),
                    );
                }
            }
        }
    }
}
