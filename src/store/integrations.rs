use serde::{Deserialize, Serialize};

use super::{read_json, remove_if_exists, write_private_json, Store};
use crate::error::Result;

const SCHEMA: &str = "clauck.integration";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationRecord {
    pub schema: String,
    pub version: u32,
    pub agent: String,
    pub settings_path: String,
    pub command: String,
    pub fail_closed_on_hook_failure: bool,
    pub installed_unix: u64,
}

impl IntegrationRecord {
    pub fn new(
        agent: &str,
        settings_path: String,
        command: String,
        fail_closed: bool,
        now: u64,
    ) -> Self {
        Self {
            schema: SCHEMA.into(),
            version: 1,
            agent: agent.into(),
            settings_path,
            command,
            fail_closed_on_hook_failure: fail_closed,
            installed_unix: now,
        }
    }
}

fn path_for(store: &Store, agent: &str) -> std::path::PathBuf {
    store
        .root
        .join("integrations")
        .join(format!("{agent}.json"))
}

pub fn load(store: &Store, agent: &str) -> Result<Option<IntegrationRecord>> {
    let rec: Option<IntegrationRecord> = read_json(&path_for(store, agent))?;
    Ok(rec.filter(|r| r.schema == SCHEMA))
}

pub fn save(store: &Store, record: &IntegrationRecord) -> Result<()> {
    store.ensure_dir("integrations".as_ref())?;
    write_private_json(&path_for(store, &record.agent), record)
}

pub fn remove(store: &Store, agent: &str) -> Result<bool> {
    remove_if_exists(&path_for(store, agent))
}
