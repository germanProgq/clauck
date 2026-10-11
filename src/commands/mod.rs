pub mod authorize;
pub mod doctor;
pub mod hook;
pub mod init;
pub mod integration;
pub mod lock;
pub mod status;
pub mod uninstall;
pub mod unlock;

use crate::cli::GlobalOpts;
use crate::error::Result;
use crate::manifest::{self, Manifest};
use crate::plan::Plan;
use crate::project::{Project, CLAUCK_DIR, LOCK_FILE, STATE_FILE};
use crate::state::{self, ProjectState};
use crate::store::{credential, ledger, Store};
use crate::txn::{self, ProjectLock, RecoveryReport};

pub struct Session {
    pub project: Project,
    lock: Option<ProjectLock>,
    created_dir: bool,
    remove_runtime: bool,
}

impl Session {
    pub fn open(g: &GlobalOpts) -> Result<Self> {
        let project = Project::open(g.path.as_deref())?;
        if g.dry_run {
            return Ok(Self {
                project,
                lock: None,
                created_dir: false,
                remove_runtime: false,
            });
        }
        let created_dir = !project.root.join(CLAUCK_DIR).exists();
        let lock = ProjectLock::acquire(&project)?;
        let session = Self {
            project,
            lock: Some(lock),
            created_dir,
            remove_runtime: false,
        };
        let report = txn::recover(&session.project)?;
        print_recovery(&report);
        Ok(session)
    }
}

impl Session {
    pub fn close_and_remove_runtime(mut self) {
        self.remove_runtime = true;
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let created_nothing = self.created_dir && !self.project.root.join(STATE_FILE).exists();
        let remove = self.lock.is_some() && (self.remove_runtime || created_nothing);
        let lock_file = self.project.root.join(LOCK_FILE);
        if remove {
            let _ = std::fs::remove_file(&lock_file);
        }
        drop(self.lock.take());
        if remove {
            let _ = std::fs::remove_file(&lock_file);
            let _ = std::fs::remove_dir(self.project.root.join(CLAUCK_DIR));
        }
    }
}

pub fn print_recovery(report: &RecoveryReport) {
    if let Some(op) = &report.recovered_operation {
        eprintln!(
            "Recovered an interrupted `clauck {op}`: {} file(s) completed.",
            report.applied.len()
        );
    }
    for c in &report.conflicts {
        eprintln!("Recovery note: {c}");
    }
    if report.removed_temp_files > 0 {
        eprintln!(
            "Removed {} leftover temporary file(s) from an interrupted run.",
            report.removed_temp_files
        );
    }
}

pub fn print_plan(plan: &Plan, dry_run: bool) {
    let verb = if dry_run { "Would" } else { "Will" };
    if plan.changes.is_empty() {
        if dry_run {
            println!("Dry run: no project files would change.");
        }
    } else if dry_run {
        println!("Dry run: {verb} change these project files:");
        for c in &plan.changes {
            println!("  {}", c.describe());
        }
    }
    for n in &plan.notes {
        eprintln!("note: {n}");
    }
}

pub fn store() -> Option<Store> {
    Store::locate().ok()
}

pub fn load_manifest(project: &Project) -> Result<Manifest> {
    Ok(manifest::load(project)?.unwrap_or_default())
}

pub struct Loaded {
    pub state: ProjectState,
    pub record: Option<ledger::RootRecord>,
    pub created: bool,
}

pub fn load_effective(project: &Project, store: Option<&Store>) -> Result<Loaded> {
    let raw = state::load(project)?;
    let record = match store {
        Some(s) => ledger::load(s, project)?,
        None => None,
    };
    let created = raw.is_none();
    let base = match (&raw, &record) {
        (Some(s), _) => s.clone(),
        (None, Some(r)) => ProjectState::new(r.project_id.clone()),
        (None, None) => ProjectState::new(state::new_project_id()?),
    };
    let Some(rec) = &record else {
        return Ok(Loaded {
            state: base,
            record,
            created,
        });
    };
    if base.project_id != rec.project_id {
        eprintln!("warning: .clauck/state.json has a different project id than this machine's record; using the local record");
    }
    if raw.is_some() && rec.locked && !base.locked {
        eprintln!("warning: .clauck/state.json says unlocked, but this machine's record says locked; keeping the lock");
    }
    let has_credential = store
        .and_then(|s| credential::load(s, &rec.project_id).ok().flatten())
        .is_some();
    let state = ProjectState {
        project_id: rec.project_id.clone(),
        locked: base.locked || rec.locked,
        password_configured: base.password_configured || rec.password_configured || has_credential,
        policy_revision: base.policy_revision.max(rec.policy_revision).max(1),
        ..base
    };
    Ok(Loaded {
        state,
        record,
        created,
    })
}

pub fn record_local(
    store: &Store,
    project: &Project,
    st: &ProjectState,
    bump_generation: bool,
    now: u64,
) -> Result<ledger::RootRecord> {
    let prev = ledger::load(store, project)?
        .unwrap_or_else(|| ledger::RootRecord::new(project, &st.project_id));
    let rec = ledger::RootRecord {
        locked: st.locked,
        password_configured: st.password_configured,
        policy_revision: st.policy_revision,
        lock_generation: prev.lock_generation + u64::from(bump_generation),
        updated_unix: now,
        ..prev
    };
    ledger::save(store, project, &rec)?;
    Ok(rec)
}
