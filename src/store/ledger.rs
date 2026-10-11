use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{list_json, read_json, remove_if_exists, write_private_json, Store};
use crate::error::Result;
use crate::project::Project;
use crate::state::is_valid_project_id;

const SCHEMA: &str = "clauck.local-record";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootRecord {
    pub schema: String,
    pub version: u32,
    pub project_id: String,
    pub root: String,
    pub locked: bool,
    pub password_configured: bool,
    pub policy_revision: u64,
    pub lock_generation: u64,
    pub updated_unix: u64,
}

impl RootRecord {
    pub fn new(project: &Project, project_id: &str) -> Self {
        Self {
            schema: SCHEMA.into(),
            version: 1,
            project_id: project_id.into(),
            root: project.root.to_string_lossy().into_owned(),
            locked: false,
            password_configured: false,
            policy_revision: 0,
            lock_generation: 0,
            updated_unix: 0,
        }
    }
}

fn path_for(store: &Store, root_key: &str) -> PathBuf {
    store.root.join("roots").join(format!("{root_key}.json"))
}

pub fn load(store: &Store, project: &Project) -> Result<Option<RootRecord>> {
    let rec: Option<RootRecord> = read_json(&path_for(store, &project.root_key()))?;
    Ok(rec.filter(|r| r.schema == SCHEMA && is_valid_project_id(&r.project_id)))
}

pub fn save(store: &Store, project: &Project, record: &RootRecord) -> Result<()> {
    store.ensure_dir("roots".as_ref())?;
    write_private_json(&path_for(store, &project.root_key()), record)
}

pub fn remove(store: &Store, project: &Project) -> Result<bool> {
    remove_if_exists(&path_for(store, &project.root_key()))
}

pub fn list(store: &Store) -> Vec<RootRecord> {
    list_json(&store.root.join("roots"), 10_000)
        .into_iter()
        .filter_map(|p| read_json::<RootRecord>(&p).ok().flatten())
        .filter(|r| r.schema == SCHEMA && is_valid_project_id(&r.project_id))
        .collect()
}

pub fn other_roots_with_id(store: &Store, project: &Project, project_id: &str) -> Vec<String> {
    let own = project.root.to_string_lossy();
    list_json(&store.root.join("roots"), 10_000)
        .into_iter()
        .filter_map(|p| read_json::<RootRecord>(&p).ok().flatten())
        .filter(|r| r.project_id == project_id && r.root != own)
        .map(|r| r.root)
        .collect()
}
