use serde::Serialize;

use crate::adapters::claude_code;
use crate::adapters::entry_files::{self, EntryAdapter};
use crate::auth::{self, Requirement};
use crate::blocks;
use crate::cli::GlobalOpts;
use crate::commands::store;
use crate::error::Result;
use crate::manifest;
use crate::plan::{read_entry, EntryRead};
use crate::project::Project;
use crate::state::{self, ProjectState};
use crate::store::{grants, integrations, ledger, Store};
use crate::timeutil::now_unix;
use crate::txn;

#[derive(Debug, Serialize)]
pub struct StatusReport {
    pub schema: &'static str,
    pub version: u32,
    pub project: ProjectInfo,
    pub advisory_policy: PolicyInfo,
    pub password: PasswordInfo,
    pub integrations: Vec<IntegrationInfo>,
    pub active_session_grants: usize,
    pub pending_recovery: bool,
}

#[derive(Debug, Serialize)]
pub struct ProjectInfo {
    pub root: String,
    pub initialized: bool,
    pub project_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PolicyInfo {
    pub state: &'static str,
    pub enforced: bool,
    pub policy_revision: Option<u64>,
    pub metadata_locked: Option<bool>,
    pub local_record_locked: Option<bool>,
    pub metadata_matches_local_record: bool,
}

#[derive(Debug, Serialize)]
pub struct PasswordInfo {
    pub configured: bool,
    pub local_authorization: &'static str,
    pub label: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct IntegrationInfo {
    pub id: &'static str,
    pub kind: &'static str,
    pub agents: &'static str,
    pub installed: bool,
    pub verified: bool,
    pub enforcement: &'static str,
    pub detail: String,
}

pub fn collect(project: &Project, store: Option<&Store>) -> Result<StatusReport> {
    let st = state::load(project)?;
    let manifest = manifest::load(project)?.unwrap_or_default();
    let record = store.and_then(|s| ledger::load(s, project).ok().flatten());
    let effective = match (&st, &record) {
        (None, None) => None,
        _ => {
            Some(st.as_ref().is_some_and(|s| s.locked) || record.as_ref().is_some_and(|r| r.locked))
        }
    };
    let req = auth::requirement(store, st.as_ref(), record.as_ref())?;
    let password = match &req {
        Requirement::None => PasswordInfo {
            configured: false,
            local_authorization: "not_configured",
            label: None,
        },
        Requirement::Unavailable => PasswordInfo {
            configured: true,
            local_authorization: "unavailable",
            label: None,
        },
        Requirement::Password(c) => PasswordInfo {
            configured: true,
            local_authorization: "available",
            label: c.label.clone(),
        },
    };
    let mut integrations: Vec<IntegrationInfo> = EntryAdapter::ALL
        .iter()
        .map(|a| entry_integration(project, &manifest, *a))
        .collect();
    integrations.push(hook_integration(store));
    let grants_active = match (store, &record) {
        (Some(s), Some(r)) => grants::list(s, project)
            .iter()
            .filter(|g| g.is_valid(r.lock_generation, now_unix()))
            .count(),
        _ => 0,
    };
    Ok(StatusReport {
        schema: "clauck.status",
        version: 1,
        project: ProjectInfo {
            root: project.display_root(),
            initialized: st.is_some(),
            project_id: st.as_ref().map(|s| s.project_id.clone()),
        },
        advisory_policy: PolicyInfo {
            state: match effective {
                None => "not_initialized",
                Some(true) => "locked",
                Some(false) => "unlocked",
            },
            enforced: false,
            policy_revision: st.as_ref().map(|s: &ProjectState| s.policy_revision),
            metadata_locked: st.as_ref().map(|s| s.locked),
            local_record_locked: record.as_ref().map(|r| r.locked),
            metadata_matches_local_record: match (&st, &record) {
                (Some(s), Some(r)) => s.locked == r.locked && s.project_id == r.project_id,
                _ => true,
            },
        },
        password,
        integrations,
        active_session_grants: grants_active,
        pending_recovery: txn::journal_pending(project),
    })
}

fn entry_integration(
    project: &Project,
    manifest: &manifest::Manifest,
    adapter: EntryAdapter,
) -> IntegrationInfo {
    let (installed, verified, detail) = match read_entry(project, adapter.file()) {
        Ok(EntryRead::Missing) => (false, false, format!("{} not present", adapter.file())),
        Ok(EntryRead::Symlink) => (
            false,
            false,
            format!("{} is a symlink; not managed", adapter.file()),
        ),
        Err(e) => (false, false, e.message),
        Ok(EntryRead::Text(t)) => match blocks::find(&t) {
            Ok(None) => (
                false,
                false,
                format!("no Clauck block in {}", adapter.file()),
            ),
            Err(e) => (true, false, e.message),
            Ok(Some(span)) => {
                let hash = blocks::block_hash(span.text(&t));
                let recorded = manifest
                    .get(adapter.file())
                    .and_then(|e| e.sha256.as_deref())
                    == Some(hash.as_str());
                let known = entry_files::known_bodies(adapter)
                    .iter()
                    .any(|b| blocks::block_hash(&blocks::wrap(b)) == hash);
                if recorded && known {
                    (
                        true,
                        true,
                        format!("block in {} via {}", adapter.file(), adapter.mechanism()),
                    )
                } else {
                    (
                        true,
                        false,
                        format!("block in {} was edited by hand", adapter.file()),
                    )
                }
            }
        },
    };
    IntegrationInfo {
        id: adapter.id(),
        kind: "advisory",
        agents: adapter.agents(),
        installed,
        verified,
        enforcement: "none",
        detail,
    }
}

fn hook_integration(store: Option<&Store>) -> IntegrationInfo {
    let base = |installed, verified, detail: String| IntegrationInfo {
        id: "claude-code-hook",
        kind: "controlled_agent",
        agents: "Claude Code (sessions on this account that load user settings)",
        installed,
        verified,
        enforcement: if installed && verified {
            "partial"
        } else {
            "none"
        },
        detail,
    };
    let Ok(path) = claude_code::settings_path() else {
        return base(false, false, "cannot locate Claude Code settings".into());
    };
    match claude_code::installed_commands(&path) {
        Err(e) => base(false, false, e.message),
        Ok(cmds) if cmds.is_empty() => base(
            false,
            false,
            "not installed (`clauck integration install claude-code`)".into(),
        ),
        Ok(cmds) => {
            let exe_ok = cmds
                .iter()
                .filter_map(|c| claude_code::command_executable(c))
                .all(|p| p.is_file());
            let recorded = store
                .and_then(|s| integrations::load(s, claude_code::AGENT_ID).ok().flatten())
                .is_some();
            let detail = if !exe_ok {
                "hook points to a missing clauck executable".to_string()
            } else if !recorded {
                "hook present but not installed by this Clauck store".to_string()
            } else {
                "PreToolUse hook in user settings; see the README for coverage gaps"
                    .to_string()
            };
            base(true, exe_ok && recorded, detail)
        }
    }
}

pub fn run(g: &GlobalOpts, json: bool) -> Result<()> {
    let project = Project::open(g.path.as_deref())?;
    let store = store();
    let report = collect(&project, store.as_ref())?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("status serializes")
        );
        return Ok(());
    }
    print_text(&report);
    Ok(())
}

fn print_text(r: &StatusReport) {
    println!("Project:          {}", r.project.root);
    let p = &r.advisory_policy;
    match p.state {
        "not_initialized" => {
            println!("Advisory policy:  not initialized (run `clauck init` or `clauck lock`)")
        }
        s => println!(
            "Advisory policy:  {} (revision {})",
            s.to_uppercase(),
            p.policy_revision
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".into())
        ),
    }
    if p.state == "locked" {
        println!("                  Cooperating agents are asked not to modify files. This is a request,");
        println!(
            "                  not enforcement: people and non-cooperating tools can still edit."
        );
    }
    if !p.metadata_matches_local_record {
        println!("                  warning: .clauck/state.json disagrees with this machine's local record;");
        println!(
            "                  it was changed outside Clauck. The stricter (locked) state applies."
        );
    }
    let pw = &r.password;
    let label = pw
        .label
        .as_deref()
        .map(|l| format!(", local label \"{l}\""))
        .unwrap_or_default();
    match pw.local_authorization {
        "available" => {
            println!("Password:         configured (local authorization available{label})")
        }
        "unavailable" => println!(
            "Password:         configured, but local authorization unavailable on this machine"
        ),
        _ => println!("Password:         not configured"),
    }
    println!("Integrations:");
    for i in &r.integrations {
        let state = match (i.installed, i.verified) {
            (true, true) => "installed, verified",
            (true, false) => "installed, NOT verified",
            _ => "not installed",
        };
        let kind = match (i.kind, i.installed && i.verified) {
            ("advisory", _) => "advisory",
            (_, true) => "controlled-agent, partial enforcement",
            _ => "controlled-agent",
        };
        println!("  {:<17} {state} [{kind}] - {}", i.id, i.detail);
    }
    if r.active_session_grants > 0 {
        println!(
            "Session grants:   {} active (`clauck revoke` ends them)",
            r.active_session_grants
        );
    }
    if r.pending_recovery {
        println!("Pending:          an interrupted operation will be completed by the next modifying command");
    }
}
