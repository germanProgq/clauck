use crate::cli::GlobalOpts;
use crate::commands::{load_effective, load_manifest, print_plan, record_local, store, Session};
use crate::error::Result;
use crate::plan::plan_sync;
use crate::timeutil::now_unix;
use crate::txn;

pub fn run(g: &GlobalOpts) -> Result<()> {
    let session = Session::open(g)?;
    let project = &session.project;
    let store = store();
    let loaded = load_effective(project, store.as_ref())?;
    let (state, created) = (loaded.state, loaded.created);
    let manifest = load_manifest(project)?;
    let plan = plan_sync(project, &state, &manifest, false)?;
    plan.require_no_conflicts()?;
    print_plan(&plan, g.dry_run);
    if g.dry_run {
        return Ok(());
    }
    txn::commit(project, "init", &plan.changes)?;
    if let Some(s) = &store {
        record_local(s, project, &state, false, now_unix())?;
    }
    let lock_word = if state.locked { "LOCKED" } else { "UNLOCKED" };
    if created {
        println!("Initialized Clauck in {}.", project.display_root());
    } else if plan.changes.is_empty() {
        println!("Already initialized: {}.", project.display_root());
    } else {
        println!("Updated Clauck files in {}.", project.display_root());
    }
    println!(
        "Advisory policy: {lock_word}. Run `clauck lock` to ask cooperating AI agents not to edit."
    );
    Ok(())
}
