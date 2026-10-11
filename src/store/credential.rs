use std::fmt;
use std::path::PathBuf;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use serde::{Deserialize, Serialize};

use super::{read_json, remove_if_exists, write_private_json, Store};
use crate::error::{ClauckError, ErrorCode, Result};
use crate::state::is_valid_project_id;

pub const ARGON2_M_COST_KIB: u32 = 19_456;
pub const ARGON2_T_COST: u32 = 2;
pub const ARGON2_P_COST: u32 = 1;
const SCHEMA: &str = "clauck.credential";

#[derive(Clone, Serialize, Deserialize)]
pub struct CredentialRecord {
    pub schema: String,
    pub version: u32,
    pub project_id: String,
    pub algorithm: String,
    verifier: String,
    pub label: Option<String>,
    pub created_unix: u64,
}

impl fmt::Debug for CredentialRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialRecord")
            .field("project_id", &self.project_id)
            .field("algorithm", &self.algorithm)
            .field("verifier", &"<redacted>")
            .field("label", &self.label)
            .finish()
    }
}

fn hasher() -> Argon2<'static> {
    let params = Params::new(ARGON2_M_COST_KIB, ARGON2_T_COST, ARGON2_P_COST, None)
        .expect("static Argon2 parameters are valid");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

impl CredentialRecord {
    pub fn create(
        project_id: &str,
        password: &str,
        label: Option<String>,
        now: u64,
    ) -> Result<Self> {
        let mut salt = [0u8; 16];
        getrandom::getrandom(&mut salt).map_err(|e| {
            ClauckError::new(ErrorCode::Internal, format!("OS random source failed: {e}"))
        })?;
        let salt = SaltString::encode_b64(&salt)
            .map_err(|e| ClauckError::new(ErrorCode::Internal, e.to_string()))?;
        let verifier = hasher()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| ClauckError::new(ErrorCode::Internal, format!("hashing failed: {e}")))?
            .to_string();
        Ok(Self {
            schema: SCHEMA.into(),
            version: 1,
            project_id: project_id.into(),
            algorithm: "argon2id".into(),
            verifier,
            label,
            created_unix: now,
        })
    }

    pub fn verify(&self, password: &str) -> bool {
        let Ok(parsed) = PasswordHash::new(&self.verifier) else {
            return false;
        };
        if parsed.algorithm.as_str() != "argon2id" {
            return false;
        }
        hasher()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    }

    #[cfg(test)]
    pub(crate) fn verifier_for_tests(&self) -> &str {
        &self.verifier
    }
}

fn path_for(store: &Store, project_id: &str) -> Option<PathBuf> {
    is_valid_project_id(project_id).then(|| {
        store
            .root
            .join("credentials")
            .join(format!("{project_id}.json"))
    })
}

pub fn load(store: &Store, project_id: &str) -> Result<Option<CredentialRecord>> {
    let Some(path) = path_for(store, project_id) else {
        return Ok(None);
    };
    store.check_private()?;
    let rec: Option<CredentialRecord> = read_json(&path)?;
    Ok(rec.filter(|r| r.schema == SCHEMA && r.project_id == project_id))
}

pub fn save(store: &Store, record: &CredentialRecord) -> Result<()> {
    let path = path_for(store, &record.project_id)
        .ok_or_else(|| ClauckError::new(ErrorCode::Internal, "invalid project id"))?;
    store.ensure_dir("credentials".as_ref())?;
    write_private_json(&path, record)
}

pub fn remove(store: &Store, project_id: &str) -> Result<bool> {
    match path_for(store, project_id) {
        Some(p) => remove_if_exists(&p),
        None => Ok(false),
    }
}

pub fn path_display(store: &Store, project_id: &str) -> String {
    path_for(store, project_id)
        .map(|p| crate::project::display_path(&p))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn verifies_correct_and_rejects_wrong() {
        let rec = CredentialRecord::create(ID, "correct horse", None, 0).unwrap();
        assert!(rec.verify("correct horse"));
        assert!(!rec.verify("correct horsf"));
        assert!(!rec.verify(""));
        assert!(rec
            .verifier_for_tests()
            .starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
    }

    #[test]
    fn salts_differ() {
        let a = CredentialRecord::create(ID, "pw", None, 0).unwrap();
        let b = CredentialRecord::create(ID, "pw", None, 0).unwrap();
        assert_ne!(a.verifier_for_tests(), b.verifier_for_tests());
    }

    #[test]
    fn debug_redacts_verifier() {
        let rec = CredentialRecord::create(ID, "pw", Some("alice".into()), 0).unwrap();
        let dbg = format!("{rec:?}");
        assert!(!dbg.contains("argon2id$"));
        assert!(dbg.contains("redacted"));
    }

    #[test]
    fn store_roundtrip_with_private_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store {
            root: dir.path().join("store"),
        };
        let rec = CredentialRecord::create(ID, "pw", None, 0).unwrap();
        save(&store, &rec).unwrap();
        assert!(load(&store, ID).unwrap().unwrap().verify("pw"));
        assert!(!crate::fsutil::is_group_or_world_accessible(&store.root));
        assert!(!crate::fsutil::is_group_or_world_accessible(
            &store.root.join("credentials").join(format!("{ID}.json"))
        ));
        assert!(load(&store, "../../etc/passwd").unwrap().is_none());
        assert!(remove(&store, ID).unwrap());
    }
}
