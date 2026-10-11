use serde::{Deserialize, Serialize};

use crate::error::{ClauckError, ErrorCode, Result};
use crate::fsutil::{self, MAX_METADATA_BYTES};
use crate::project::{Project, STATE_FILE};

pub const STATE_SCHEMA: &str = "clauck.state";
pub const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectState {
    pub schema: String,
    pub version: u32,
    pub project_id: String,
    pub locked: bool,
    pub password_configured: bool,
    pub policy_revision: u64,
}

impl ProjectState {
    pub fn new(project_id: String) -> Self {
        Self {
            schema: STATE_SCHEMA.into(),
            version: STATE_VERSION,
            project_id,
            locked: false,
            password_configured: false,
            policy_revision: 1,
        }
    }

    pub fn with_lock(&self, locked: bool) -> Self {
        Self {
            locked,
            policy_revision: if locked == self.locked {
                self.policy_revision
            } else {
                self.policy_revision + 1
            },
            ..self.clone()
        }
    }

    pub fn with_password(&self, configured: bool) -> Self {
        Self {
            password_configured: configured,
            ..self.clone()
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = serde_json::to_vec_pretty(self).expect("state serializes");
        out.push(b'\n');
        out
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let state: Self = serde_json::from_slice(bytes).map_err(|e| invalid(&e.to_string()))?;
        if state.schema != STATE_SCHEMA {
            return Err(invalid("unexpected schema identifier"));
        }
        if state.version != STATE_VERSION {
            return Err(invalid(&format!(
                "unsupported version {} (this clauck understands {STATE_VERSION})",
                state.version
            )));
        }
        if !is_valid_project_id(&state.project_id) {
            return Err(invalid("project_id must be 32 lowercase hex characters"));
        }
        Ok(state)
    }
}

fn invalid(detail: &str) -> ClauckError {
    ClauckError::new(
        ErrorCode::InvalidMetadata,
        format!("{STATE_FILE} is invalid: {detail}"),
    )
    .with_hint("Run `clauck doctor`. No project files were changed.")
}

pub fn is_valid_project_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn new_project_id() -> Result<String> {
    fsutil::random_hex(16)
}

pub fn load(project: &Project) -> Result<Option<ProjectState>> {
    let path = project.path(STATE_FILE)?;
    match fsutil::read_bounded(&path, MAX_METADATA_BYTES)? {
        None => Ok(None),
        Some(bytes) => ProjectState::parse(&bytes).map(Some),
    }
}

pub fn load_required(project: &Project) -> Result<ProjectState> {
    load(project)?.ok_or_else(|| {
        ClauckError::new(
            ErrorCode::NotInitialized,
            format!("{} is not a Clauck project", project.display_root()),
        )
        .with_hint("Run `clauck init` or `clauck lock` first.")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let s = ProjectState::new("0123456789abcdef0123456789abcdef".into());
        assert_eq!(ProjectState::parse(&s.to_bytes()).unwrap(), s);
    }

    #[test]
    fn lock_bumps_revision_only_on_change() {
        let s = ProjectState::new("0123456789abcdef0123456789abcdef".into());
        let l = s.with_lock(true);
        assert_eq!(l.policy_revision, 2);
        assert_eq!(l.with_lock(true).policy_revision, 2);
    }

    #[test]
    fn rejects_traversal_project_id_and_bad_versions() {
        let bad_id = br#"{"schema":"clauck.state","version":1,"project_id":"../../etc","locked":true,"password_configured":false,"policy_revision":1}"#;
        assert_eq!(
            ProjectState::parse(bad_id).unwrap_err().code,
            ErrorCode::InvalidMetadata
        );
        let bad_ver = br#"{"schema":"clauck.state","version":9,"project_id":"0123456789abcdef0123456789abcdef","locked":true,"password_configured":false,"policy_revision":1}"#;
        assert!(ProjectState::parse(bad_ver)
            .unwrap_err()
            .message
            .contains("unsupported version"));
        assert!(ProjectState::parse(b"[]").is_err());
        assert!(ProjectState::parse(b"").is_err());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let extra = br#"{"schema":"clauck.state","version":1,"project_id":"0123456789abcdef0123456789abcdef","locked":false,"password_configured":false,"policy_revision":1,"password":"x"}"#;
        assert!(ProjectState::parse(extra).is_err());
    }
}
