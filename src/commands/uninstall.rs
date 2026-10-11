use crate::auth::{self, PasswordSource};
use crate::cli::GlobalOpts;
use crate::commands::{print_plan, store, Session};
use crate::error::{ClauckError, ErrorCode, Result};
use crate::fsutil::{self, MAX_METADATA_BYTES};
use crate::manifest;
use crate::plan::plan_uninstall;
use crate::project::{display_path, Project, AGENTS_MD, CLAUCK_DIR, CLAUDE_MD, STATE_FILE};
use crate::state::{self, is_valid_project_id, ProjectState};
use crate::store::{credential, grants, ledger};
use crate::txn::{self, ChangeKind};

fn load_state_for_removal(project: &Project, has_record: bool) -> Result<Option<ProjectState>> {
    match state::load(project) {
        Ok(s) => Ok(s),
        Err(e) => {
            let lenient = fsutil::read_bounded(&project.root.join(STATE_FILE), MAX_METADATA_BYTES)
                .ok()
                .flatten()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .and_then(|v| {
                    v.get("project_id")
                        .and_then(|i| i.as_str())
                        .map(String::from)
                })
                .filter(|id| is_valid_project_id(id));
            match lenient {
                Some(id) => Ok(Some(ProjectState::new(id).with_password(true))),
                None if has_record => Ok(None),
                None => Err(ClauckError::new(
                    ErrorCode::InvalidMetadata,
                    format!(
                        "{}; cannot tell whether a password protects this project",
                        e.message
                    ),
                )
                .with_hint(
                    "Clauck will not skip authentication on damaged metadata. Fix the file, \
                     or remove the clauck blocks and the .clauck folder by hand.",
                )),
            }
        }
    }
}

pub fn run(g: &GlobalOpts, force: bool) -> Result<()> {
    let session = Session::open(g)?;
    let project = session.project.clone();
    let store = store();
    let record = match &store {
        Some(s) => ledger::load(s, &project)?,
        None => None,
    };
    let st = load_state_for_removal(&project, record.is_some())?;
    let manifest = manifest::load(&project).ok().flatten().unwrap_or_default();
    if st.is_none() && record.is_none() && !project.root.join(CLAUCK_DIR).exists() {
        println!(
            "Nothing to uninstall: {} is not a Clauck project.",
            project.display_root()
        );
        return Ok(());
    }
    let req = auth::requirement(store.as_ref(), st.as_ref(), record.as_ref())?;
    let plan = plan_uninstall(&project, &manifest, force)?;
    plan.require_no_conflicts()?;
    print_plan(&plan, g.dry_run);
    if g.dry_run {
        return Ok(());
    }

    let mut source = PasswordSource::detect(g.password_stdin, g.non_interactive)?;
    auth::authenticate(&req, &mut source)?;

    txn::commit(&project, "uninstall", &plan.changes)?;
    if let Some(s) = &store {
        grants::revoke_all(s, &project)?;
        ledger::remove(s, &project)?;
        if let Some(id) = auth::governing_id(st.as_ref(), record.as_ref()) {
            if ledger::other_roots_with_id(s, &project, id).is_empty() {
                credential::remove(s, id)?;
            }
        }
    }
    session.close_and_remove_runtime();
    let dir = project.root.join(CLAUCK_DIR);
    if dir.exists() {
        eprintln!(
            "note: kept {} because it contains files Clauck did not create",
            display_path(&dir)
        );
    }

    println!("Removed Clauck from {}.", project.display_root());
    if plan
        .changes
        .iter()
        .any(|c| matches!(c.kind, ChangeKind::Write(_)))
    {
        println!("Content outside the Clauck blocks in {AGENTS_MD} and {CLAUDE_MD} was kept.");
    }
    println!(
        "Note: this password check only governs the clauck CLI. Anyone with access to these files \
         could have removed them by hand."
    );
    Ok(())
}
