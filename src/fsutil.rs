use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{ClauckError, ErrorCode, Result};

pub const MAX_METADATA_BYTES: u64 = 256 * 1024;
pub const MAX_INSTRUCTION_BYTES: u64 = 2 * 1024 * 1024;

pub const TMP_MARKER: &str = ".clauck-tmp-";

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

pub fn random_hex(bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::getrandom(&mut buf).map_err(|e| {
        ClauckError::new(ErrorCode::Internal, format!("OS random source failed: {e}"))
    })?;
    Ok(hex::encode(buf))
}

pub fn read_bounded(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ClauckError::io("inspect", path, &e)),
    };
    if meta.file_type().is_symlink() {
        return Err(ClauckError::new(
            ErrorCode::UnsafePath,
            format!("refusing to follow symlink {}", path.display()),
        ));
    }
    if !meta.is_file() {
        return Err(ClauckError::new(
            ErrorCode::UnsafePath,
            format!("{} is not a regular file", path.display()),
        ));
    }
    if meta.len() > limit {
        return Err(too_large(path, limit));
    }
    let file = File::open(path).map_err(|e| ClauckError::io("open", path, &e))?;
    let mut buf = Vec::with_capacity(meta.len() as usize);
    file.take(limit + 1)
        .read_to_end(&mut buf)
        .map_err(|e| ClauckError::io("read", path, &e))?;
    if buf.len() as u64 > limit {
        return Err(too_large(path, limit));
    }
    Ok(Some(buf))
}

pub fn read_bounded_string(path: &Path, limit: u64) -> Result<Option<String>> {
    match read_bounded(path, limit)? {
        None => Ok(None),
        Some(bytes) => String::from_utf8(bytes).map(Some).map_err(|_| {
            ClauckError::new(
                ErrorCode::InvalidMetadata,
                format!("{} is not valid UTF-8", path.display()),
            )
        }),
    }
}

fn too_large(path: &Path, limit: u64) -> ClauckError {
    ClauckError::new(
        ErrorCode::InvalidMetadata,
        format!("{} exceeds the {limit}-byte limit", path.display()),
    )
}

pub fn stage_file(target: &Path, data: &[u8]) -> Result<PathBuf> {
    let dir = target
        .parent()
        .ok_or_else(|| ClauckError::new(ErrorCode::UnsafePath, "target has no parent directory"))?;
    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| ClauckError::new(ErrorCode::UnsafePath, "target has no file name"))?;
    let tmp = dir.join(format!(".{name}{TMP_MARKER}{}", random_hex(6)?));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| ClauckError::io("create", &tmp, &e))?;
    let perms = fs::metadata(target).map(|m| m.permissions());
    let written = file
        .write_all(data)
        .and_then(|_| match perms {
            Ok(p) => file.set_permissions(p),
            Err(_) => Ok(()),
        })
        .and_then(|_| file.sync_all());
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(ClauckError::io("write", &tmp, &e));
    }
    Ok(tmp)
}

pub fn atomic_write(target: &Path, data: &[u8]) -> Result<()> {
    let tmp = stage_file(target, data)?;
    if let Err(e) = fs::rename(&tmp, target) {
        let _ = fs::remove_file(&tmp);
        return Err(ClauckError::io("replace", target, &e));
    }
    sync_dir(target.parent());
    Ok(())
}

pub fn sync_dir(dir: Option<&Path>) {
    #[cfg(unix)]
    if let Some(d) = dir {
        if let Ok(f) = File::open(d) {
            let _ = f.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
}

pub fn safe_join(root: &Path, rel: &str) -> Result<PathBuf> {
    let rel_path = Path::new(rel);
    if rel.is_empty() || rel_path.is_absolute() {
        return Err(unsafe_path(rel));
    }
    let mut out = root.to_path_buf();
    for comp in rel_path.components() {
        match comp {
            Component::Normal(part) => {
                out.push(part);
                if let Ok(meta) = fs::symlink_metadata(&out) {
                    if meta.file_type().is_symlink() {
                        return Err(ClauckError::new(
                            ErrorCode::UnsafePath,
                            format!("refusing to write through symlink {}", out.display()),
                        )
                        .with_hint(
                            "Clauck never modifies files through symbolic links. \
                             Replace the link with a regular file or remove it.",
                        ));
                    }
                }
            }
            _ => return Err(unsafe_path(rel)),
        }
    }
    Ok(out)
}

fn unsafe_path(rel: &str) -> ClauckError {
    ClauckError::new(
        ErrorCode::UnsafePath,
        format!("path {rel:?} escapes the project directory"),
    )
}

pub fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

pub fn create_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|e| ClauckError::io("create directory", path, &e))?;
    restrict_permissions(path, true);
    Ok(())
}

pub fn restrict_permissions(path: &Path, is_dir: bool) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if is_dir { 0o700 } else { 0o600 };
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    let _ = (path, is_dir);
}

pub fn is_group_or_world_accessible(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o077 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_rejects_traversal_and_absolute() {
        let dir = tempfile::tempdir().unwrap();
        assert!(safe_join(dir.path(), "../x").is_err());
        assert!(safe_join(dir.path(), "a/../../x").is_err());
        assert!(safe_join(dir.path(), "").is_err());
        let abs = if cfg!(windows) {
            "C:\\x"
        } else {
            "/etc/passwd"
        };
        assert!(safe_join(dir.path(), abs).is_err());
        assert!(safe_join(dir.path(), ".clauck/policy.md").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn safe_join_rejects_symlinked_component() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join(".clauck")).unwrap();
        let err = safe_join(dir.path(), ".clauck/policy.md").unwrap_err();
        assert_eq!(err.code, ErrorCode::UnsafePath);
    }

    #[test]
    fn bounded_read_enforces_limit() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        fs::write(&p, vec![b'a'; 100]).unwrap();
        assert!(read_bounded(&p, 100).unwrap().is_some());
        let err = read_bounded(&p, 99).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidMetadata);
        assert!(read_bounded(&dir.path().join("missing"), 10)
            .unwrap()
            .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_preserves_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        fs::write(&p, "{}").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
        atomic_write(&p, b"{\"a\":1}").unwrap();
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("naïve file.txt");
        atomic_write(&p, b"one").unwrap();
        atomic_write(&p, b"two").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(TMP_MARKER))
            .collect();
        assert!(leftovers.is_empty());
    }
}
