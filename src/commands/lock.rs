use crate::auth::{self, PasswordSource, Requirement};
use crate::cli::{GlobalOpts, LockArgs};
use crate::commands::{load_effective, load_manifest, print_plan, record_local, store, Session};
use crate::error::{ClauckError, ErrorCode, Result};
use crate::plan::plan_sync;
use crate::store::credential::{self, CredentialRecord};
use crate::store::{grants, Store};
use crate::timeutil::now_unix;
use crate::txn;

pub fn run(g: &GlobalOpts, args: &LockArgs) -> Result<()> {
    let session = Session::open(g)?;
    let project = &session.project;
    let store = store();
    let loaded = load_effective(project, store.as_ref())?;
    let state = &loaded.state;
    let manifest = load_manifest(project)?;
    let req = auth::requirement(store.as_ref(), Some(state), loaded.record.as_ref())?;
    let now = now_unix();

    let new_credential = if args.password && !g.dry_run {
        store.as_ref().ok_or_else(|| {
            ClauckError::new(
                ErrorCode::AuthUnavailable,
                "no local application-data folder to store the password in",
            )
        })?;
        let mut source = PasswordSource::detect(g.password_stdin, g.non_interactive)?;
        if !matches!(req, Requirement::None) {
            eprintln!("A password is already set. Enter it to replace it.");
            auth::authenticate(&req, &mut source)?;
        }
        let pw = auth::ask_new_password(&mut source)?;
        let label = args
            .username
            .as_ref()
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty());
        Some(CredentialRecord::create(
            &state.project_id,
            &pw,
            label,
            now,
        )?)
    } else {
        None
    };

    let password_configured =
        state.password_configured || !matches!(req, Requirement::None) || args.password;
    let new_state = state.with_lock(true).with_password(password_configured);
    let plan = plan_sync(project, &new_state, &manifest, args.force)?;
    plan.require_no_conflicts()?;
    print_plan(&plan, g.dry_run);
    if g.dry_run {
        if args.password {
            println!(
                "Would prompt for a password and store its Argon2id verifier outside the project."
            );
        }
        return Ok(());
    }

    if let Some(s) = &store {
        grants::revoke_all(s, project)?;
    }
    match (&store, &new_credential) {
        (Some(s), Some(cred)) => commit_with_credential(s, project, &plan, cred)?,
        _ => {
            txn::commit(project, "lock", &plan.changes)?;
        }
    }
    if let Some(s) = &store {
        record_local(s, project, &new_state, true, now).map_err(|e| {
            ClauckError::new(
                e.code,
                format!(
                    "project files are locked, but the local record could not be updated: {}",
                    e.message
                ),
            )
        })?;
    }

    println!(
        "Locked (advisory): cooperating AI agents are asked not to modify {}.",
        project.display_root()
    );
    println!("This is not enforcement. People, and tools that ignore .clauck/policy.md, can still edit files.");
    if password_configured {
        println!("Unlocking requires the Clauck password.");
    }
    if let (Some(s), Some(_)) = (&store, &new_credential) {
        println!(
            "Password verifier stored outside the project: {}",
            credential::path_display(s, &state.project_id)
        );
    }
    println!("Agent sessions that are already running may need a restart to see the new policy.");
    Ok(())
}

fn commit_with_credential(
    store: &Store,
    project: &crate::project::Project,
    plan: &crate::plan::Plan,
    cred: &CredentialRecord,
) -> Result<()> {
    let previous = credential::load(store, &cred.project_id)?;
    credential::save(store, cred)?;
    if let Err(e) = txn::commit(project, "lock", &plan.changes) {
        let restored = match &previous {
            Some(old) => credential::save(store, old),
            None => credential::remove(store, &cred.project_id).map(|_| ()),
        };
        if let Err(r) = restored {
            eprintln!(
                "warning: could not restore the previous password record: {}",
                r.message
            );
        }
        return Err(e);
    }
    Ok(())
}
