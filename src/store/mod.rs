pub mod credential;
pub mod grants;
pub mod integrations;
pub mod ledger;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::{ClauckError, ErrorCode, Result};
use crate::fsutil::{self, MAX_METADATA_BYTES};
use crate::project::display_path;

pub const HOME_ENV: &str = "CLAUCK_HOME";

#[derive(Debug, Clone)]
pub struct Store {
    pub root: PathBuf,
}

impl Store {
    pub fn locate() -> Result<Self> {
        if let Some(dir) = std::env::var_os(HOME_ENV).filter(|v| !v.is_empty()) {
            return Ok(Self {
                root: PathBuf::from(dir),
            });
        }
        directories::ProjectDirs::from("dev", "clauck", "clauck")
            .map(|d| Self {
                root: d.data_local_dir().to_path_buf(),
            })
            .ok_or_else(|| {
                ClauckError::new(
                    ErrorCode::AuthUnavailable,
                    "cannot determine the per-user application-data folder",
                )
                .with_hint(format!("Set {HOME_ENV} to a private directory."))
            })
    }

    pub fn display(&self) -> String {
        display_path(&self.root)
    }

    pub(crate) fn ensure_dir(&self, sub: &Path) -> Result<PathBuf> {
        fsutil::create_private_dir(&self.root)?;
        self.check_private()?;
        let mut current = self.root.clone();
        for comp in sub.components() {
            current.push(comp);
            fsutil::create_private_dir(&current)?;
        }
        Ok(current)
    }

    pub fn exists(&self) -> bool {
        self.root.is_dir()
    }

    pub fn check_private(&self) -> Result<()> {
        check_private_dir(&self.root)
    }
}

#[cfg(unix)]
fn check_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let meta = match fs::symlink_metadata(dir) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(ClauckError::io("inspect", dir, &e)),
    };
    let euid = unsafe { libc::geteuid() };
    let writable_by_others = meta.permissions().mode() & 0o022 != 0;
    if meta.file_type().is_symlink() || !meta.is_dir() || meta.uid() != euid || writable_by_others {
        return Err(ClauckError::new(
            ErrorCode::AuthUnavailable,
            format!(
                "the Clauck store {} is not a private directory owned by you; refusing to trust it",
                display_path(dir)
            ),
        )
        .with_hint("Use a directory you own with mode 0700 (set CLAUCK_HOME to choose another)."));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_private_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

pub(crate) fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fsutil::read_bounded(path, MAX_METADATA_BYTES)? {
        None => Ok(None),
        Some(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| {
            ClauckError::new(
                ErrorCode::InvalidMetadata,
                format!("local record {} is invalid: {e}", display_path(path)),
            )
        }),
    }
}

pub(crate) fn write_private_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| ClauckError::new(ErrorCode::Internal, e.to_string()))?;
    bytes.push(b'\n');
    let tmp = fsutil::stage_file(path, &bytes)?;
    fsutil::restrict_permissions(&tmp, false);
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        ClauckError::io("replace", path, &e)
    })?;
    fsutil::sync_dir(path.parent());
    Ok(())
}

pub(crate) fn remove_if_exists(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(ClauckError::io("remove", path, &e)),
    }
}

pub(crate) fn list_json(dir: &Path, limit: usize) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .take(limit)
        .collect()
}
