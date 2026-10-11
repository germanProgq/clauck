use serde::{Deserialize, Serialize};

use crate::error::{ClauckError, ErrorCode, Result};
use crate::fsutil::{self, MAX_METADATA_BYTES};
use crate::project::{self, Project, MANIFEST_FILE};

pub const MANIFEST_SCHEMA: &str = "clauck.manifest";
pub const MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestEntry {
    pub path: String,
    pub kind: EntryKind,
    pub created_file: bool,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: String,
    pub version: u32,
    pub entries: Vec<ManifestEntry>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            schema: MANIFEST_SCHEMA.into(),
            version: MANIFEST_VERSION,
            entries: Vec::new(),
        }
    }
}

impl Manifest {
    pub fn get(&self, path: &str) -> Option<&ManifestEntry> {
        self.entries.iter().find(|e| e.path == path)
    }

    pub fn with_entry(&self, entry: ManifestEntry) -> Self {
        let mut entries: Vec<_> = self
            .entries
            .iter()
            .filter(|e| e.path != entry.path)
            .cloned()
            .collect();
        entries.push(entry);
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Self {
            entries,
            ..self.clone()
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = serde_json::to_vec_pretty(self).expect("manifest serializes");
        out.push(b'\n');
        out
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let m: Self = serde_json::from_slice(bytes).map_err(|e| invalid(&e.to_string()))?;
        if m.schema != MANIFEST_SCHEMA {
            return Err(invalid("unexpected schema identifier"));
        }
        if m.version != MANIFEST_VERSION {
            return Err(invalid(&format!("unsupported version {}", m.version)));
        }
        if let Some(bad) = m
            .entries
            .iter()
            .find(|e| !project::is_managed_path(&e.path))
        {
            return Err(invalid(&format!(
                "entry {:?} is not a path Clauck manages",
                bad.path
            )));
        }
        Ok(m)
    }
}

fn invalid(detail: &str) -> ClauckError {
    ClauckError::new(
        ErrorCode::InvalidMetadata,
        format!("{MANIFEST_FILE} is invalid: {detail}"),
    )
    .with_hint("Run `clauck doctor`. No project files were changed.")
}

pub fn load(project: &Project) -> Result<Option<Manifest>> {
    let path = project.path(MANIFEST_FILE)?;
    match fsutil::read_bounded(&path, MAX_METADATA_BYTES)? {
        None => Ok(None),
        Some(bytes) => Manifest::parse(&bytes).map(Some),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unmanaged_paths() {
        let bad = br#"{"schema":"clauck.manifest","version":1,"entries":[
            {"path":"src/main.rs","kind":"file","created_file":true,"sha256":null}]}"#;
        let err = Manifest::parse(bad).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidMetadata);
        let traversal = br#"{"schema":"clauck.manifest","version":1,"entries":[
            {"path":"../../x","kind":"file","created_file":true,"sha256":null}]}"#;
        assert!(Manifest::parse(traversal).is_err());
    }

    #[test]
    fn with_entry_replaces_without_mutating_original() {
        let m = Manifest::default();
        let e = ManifestEntry {
            path: "AGENTS.md".into(),
            kind: EntryKind::Block,
            created_file: true,
            sha256: Some("a".into()),
        };
        let m2 = m.with_entry(e.clone());
        let m3 = m2.with_entry(ManifestEntry {
            sha256: Some("b".into()),
            ..e
        });
        assert!(m.entries.is_empty());
        assert_eq!(m3.entries.len(), 1);
        assert_eq!(m3.get("AGENTS.md").unwrap().sha256.as_deref(), Some("b"));
        assert_eq!(Manifest::parse(&m3.to_bytes()).unwrap(), m3);
    }
}
