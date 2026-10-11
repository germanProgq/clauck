use crate::auth::{self, PasswordSource, Requirement};
use crate::cli::{GlobalOpts, UnlockArgs};
use crate::commands::{load_effective, load_manifest, print_plan, record_local, store, Session};
use crate::error::Result;
use crate::plan::plan_sync;
use crate::state;
use crate::store::{credential, grants};
use crate::timeutil::now_unix;
use crate::txn;

pub fn run(g: &GlobalOpts, args: &UnlockArgs) -> Result<()> {
    let session = Session::open(g)?;
    let project = &session.project;
    let store = store();
    state::load_required(project)?;
    let loaded = load_effective(project, store.as_ref())?;
    let state = loaded.state.clone();
    let manifest = load_manifest(project)?;
    let record = loaded.record;
    let locked = state.locked;
    if !locked && !args.remove_password {
        println!(
            "Already unlocked: no Clauck edit restriction is active in {}.",
            project.display_root()
        );
        return Ok(());
    }
    let req = auth::requirement(store.as_ref(), Some(&state), record.as_ref())?;
    let has_password = !matches!(req, Requirement::None);
    let new_state = state
        .with_lock(false)
        .with_password(has_password && !args.remove_password);
    let plan = plan_sync(project, &new_state, &manifest, args.force)?;
    plan.require_no_conflicts()?;
    print_plan(&plan, g.dry_run);
    if g.dry_run {
        if has_password {
            println!("Would ask for the Clauck password before unlocking.");
        }
        return Ok(());
    }

    let mut source = PasswordSource::detect(g.password_stdin, g.non_interactive)?;
    auth::authenticate(&req, &mut source)?;

    txn::commit(project, "unlock", &plan.changes)?;
    if let Some(s) = &store {
        record_local(s, project, &new_state, false, now_unix())?;
        grants::revoke_all(s, project)?;
        if args.remove_password {
            credential::remove(s, &state.project_id)?;
        }
    }
    println!(
        "Unlocked: the Clauck edit restriction is removed for {}.",
        project.display_root()
    );
    if args.remove_password && has_password {
        println!("Password removed; future locks will not require one unless you run `clauck lock --password`.");
    } else if has_password {
        println!("The password stays configured, so the next `clauck lock` will again require it to unlock.");
    }
    println!("Agents do not resume previously blocked work automatically.");
    Ok(())
}
