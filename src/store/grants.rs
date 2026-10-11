use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{list_json, read_json, remove_if_exists, write_private_json, Store};
use crate::error::Result;
use crate::fsutil::sha256_hex;
use crate::project::Project;

const SCHEMA: &str = "clauck.grant";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub schema: String,
    pub version: u32,
    pub project_id: String,
    pub root: String,
    pub agent: String,
    pub session_sha256: String,
    pub lock_generation: u64,
    pub created_unix: u64,
    pub expires_unix: u64,
}

impl Grant {
    pub fn is_valid(&self, generation: u64, now: u64) -> bool {
        self.schema == SCHEMA && self.lock_generation == generation && now < self.expires_unix
    }
}

fn dir_for(store: &Store, project: &Project) -> PathBuf {
    store.root.join("grants").join(project.root_key())
}

fn file_for(store: &Store, project: &Project, session_id: &str) -> PathBuf {
    dir_for(store, project).join(format!("{}.json", sha256_hex(session_id.as_bytes())))
}

pub struct NewGrant<'a> {
    pub project_id: &'a str,
    pub agent: &'a str,
    pub session_id: &'a str,
    pub lock_generation: u64,
    pub now: u64,
    pub ttl_secs: u64,
}

pub fn create(store: &Store, project: &Project, g: &NewGrant) -> Result<Grant> {
    let grant = Grant {
        schema: SCHEMA.into(),
        version: 1,
        project_id: g.project_id.into(),
        root: project.root.to_string_lossy().into_owned(),
        agent: g.agent.into(),
        session_sha256: sha256_hex(g.session_id.as_bytes()),
        lock_generation: g.lock_generation,
        created_unix: g.now,
        expires_unix: g.now.saturating_add(g.ttl_secs),
    };
    store.ensure_dir(&PathBuf::from("grants").join(project.root_key()))?;
    write_private_json(&file_for(store, project, g.session_id), &grant)?;
    Ok(grant)
}

pub fn find_valid(
    store: &Store,
    project: &Project,
    session_id: &str,
    generation: u64,
    now: u64,
) -> Option<Grant> {
    store.check_private().ok()?;
    let grant: Grant = read_json(&file_for(store, project, session_id))
        .ok()
        .flatten()?;
    let expected = sha256_hex(session_id.as_bytes());
    (grant.session_sha256 == expected && grant.is_valid(generation, now)).then_some(grant)
}

pub fn list(store: &Store, project: &Project) -> Vec<Grant> {
    list_json(&dir_for(store, project), 1_000)
        .into_iter()
        .filter_map(|p| read_json::<Grant>(&p).ok().flatten())
        .collect()
}

pub fn revoke_session(store: &Store, project: &Project, session_id: &str) -> Result<bool> {
    remove_if_exists(&file_for(store, project, session_id))
}

pub fn revoke_all(store: &Store, project: &Project) -> Result<usize> {
    let mut removed = 0;
    for path in list_json(&dir_for(store, project), 10_000) {
        if remove_if_exists(&path)? {
            removed += 1;
        }
    }
    let _ = std::fs::remove_dir(dir_for(store, project));
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Store, Project) {
        let dir = tempfile::tempdir().unwrap();
        let proj_dir = dir.path().join("p");
        std::fs::create_dir(&proj_dir).unwrap();
        let store = Store {
            root: dir.path().join("store"),
        };
        let project = Project::open(Some(&proj_dir)).unwrap();
        (dir, store, project)
    }

    fn new<'a>(session: &'a str, generation: u64) -> NewGrant<'a> {
        NewGrant {
            project_id: "0123456789abcdef0123456789abcdef",
            agent: "claude-code",
            session_id: session,
            lock_generation: generation,
            now: 1_000,
            ttl_secs: 60,
        }
    }

    #[test]
    fn grant_is_scoped_to_session_generation_and_time() {
        let (_d, store, project) = setup();
        create(&store, &project, &new("s1", 3)).unwrap();
        assert!(find_valid(&store, &project, "s1", 3, 1_030).is_some());
        assert!(find_valid(&store, &project, "s2", 3, 1_030).is_none());
        assert!(find_valid(&store, &project, "s1", 4, 1_030).is_none());
        assert!(find_valid(&store, &project, "s1", 3, 1_060).is_none());
    }

    #[test]
    fn revocation_is_immediate() {
        let (_d, store, project) = setup();
        create(&store, &project, &new("s1", 1)).unwrap();
        create(&store, &project, &new("s2", 1)).unwrap();
        assert!(revoke_session(&store, &project, "s1").unwrap());
        assert!(find_valid(&store, &project, "s1", 1, 1_001).is_none());
        assert_eq!(revoke_all(&store, &project).unwrap(), 1);
        assert!(list(&store, &project).is_empty());
    }

    #[test]
    fn session_id_is_not_stored_in_clear() {
        let (_d, store, project) = setup();
        create(&store, &project, &new("very-identifiable-session", 1)).unwrap();
        let dir = dir_for(&store, &project);
        for p in list_json(&dir, 10) {
            let text = std::fs::read_to_string(p).unwrap();
            assert!(!text.contains("very-identifiable-session"));
        }
    }
}
