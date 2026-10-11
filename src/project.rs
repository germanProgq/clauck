use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{ClauckError, ErrorCode, Result};
use crate::fsutil::safe_join;

pub const CLAUCK_DIR: &str = ".clauck";
pub const STATE_FILE: &str = ".clauck/state.json";
pub const MANIFEST_FILE: &str = ".clauck/manifest.json";
pub const POLICY_FILE: &str = ".clauck/policy.md";
pub const GITIGNORE_FILE: &str = ".clauck/.gitignore";
pub const JOURNAL_FILE: &str = ".clauck/journal.json";
pub const LOCK_FILE: &str = ".clauck/.lock";
pub const AGENTS_MD: &str = "AGENTS.md";
pub const CLAUDE_MD: &str = "CLAUDE.md";
pub const CLAUDE_LOCAL_MD: &str = "CLAUDE.local.md";
pub const CLAUDE_DIR_MD: &str = ".claude/CLAUDE.md";

pub const MANAGED_PATHS: [&str; 6] = [
    STATE_FILE,
    MANIFEST_FILE,
    POLICY_FILE,
    GITIGNORE_FILE,
    AGENTS_MD,
    CLAUDE_MD,
];

pub fn is_managed_path(rel: &str) -> bool {
    MANAGED_PATHS.contains(&rel)
}

#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
}

impl Project {
    pub fn open(path: Option<&Path>) -> Result<Self> {
        let raw = match path {
            Some(p) => p.to_path_buf(),
            None => std::env::current_dir().map_err(|e| {
                ClauckError::new(ErrorCode::Io, format!("cannot read current directory: {e}"))
            })?,
        };
        let root = fs::canonicalize(&raw).map_err(|e| {
            ClauckError::new(
                ErrorCode::InvalidInput,
                format!("project directory {} is not accessible: {e}", raw.display()),
            )
        })?;
        if !root.is_dir() {
            return Err(ClauckError::new(
                ErrorCode::InvalidInput,
                format!("{} is not a directory", root.display()),
            ));
        }
        Ok(Self { root })
    }

    pub fn path(&self, rel: &str) -> Result<PathBuf> {
        safe_join(&self.root, rel)
    }

    pub fn clauck_dir(&self) -> Result<PathBuf> {
        self.path(CLAUCK_DIR)
    }

    pub fn is_initialized(&self) -> bool {
        self.root.join(STATE_FILE).is_file()
    }

    pub fn display_root(&self) -> String {
        display_path(&self.root)
    }

    pub fn root_key(&self) -> String {
        crate::fsutil::sha256_hex(self.root.to_string_lossy().as_bytes())
    }
}

pub fn display_path(path: &Path) -> String {
    let s = path.display().to_string();
    s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_unicode_path_with_spaces() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("my project ✓ ünï");
        fs::create_dir(&p).unwrap();
        let proj = Project::open(Some(&p)).unwrap();
        assert!(proj.root.ends_with("my project ✓ ünï"));
        assert_eq!(proj.root_key().len(), 64);
    }

    #[test]
    fn rejects_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let err = Project::open(Some(&dir.path().join("nope"))).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn managed_paths_are_relative_and_inside() {
        for p in MANAGED_PATHS {
            assert!(!p.contains(".."));
            assert!(!Path::new(p).is_absolute());
        }
        assert!(!is_managed_path("src/main.rs"));
    }
}
