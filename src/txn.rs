use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{ClauckError, ErrorCode, Result};
use crate::fsutil::{self, sha256_hex, MAX_METADATA_BYTES, TMP_MARKER};
use crate::project::{self, Project, CLAUCK_DIR, JOURNAL_FILE, LOCK_FILE};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    Write(Vec<u8>),
    Delete,
}

#[derive(Debug, Clone)]
pub struct Change {
    pub path: &'static str,
    pub kind: ChangeKind,
    pub before_sha256: Option<String>,
}

impl Change {
    pub fn describe(&self) -> String {
        match (&self.kind, &self.before_sha256) {
            (ChangeKind::Write(_), None) => format!("create {}", self.path),
            (ChangeKind::Write(_), Some(_)) => format!("update {}", self.path),
            (ChangeKind::Delete, _) => format!("remove {}", self.path),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Journal {
    schema: String,
    version: u32,
    operation: String,
    entries: Vec<JournalEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct JournalEntry {
    path: String,
    action: String,
    staged: Option<String>,
    before_sha256: Option<String>,
    after_sha256: Option<String>,
}

const JOURNAL_SCHEMA: &str = "clauck.journal";

pub struct ProjectLock {
    _file: File,
}

impl ProjectLock {
    pub fn acquire(project: &Project) -> Result<Self> {
        let dir = project.clauck_dir()?;
        fs::create_dir_all(&dir).map_err(|e| ClauckError::io("create", &dir, &e))?;
        let path = project.path(LOCK_FILE)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| ClauckError::io("open lock file", &path, &e))?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => Err(ClauckError::new(
                ErrorCode::Busy,
                "another clauck command is modifying this project",
            )
            .with_hint("Wait for it to finish, then retry.")),
            Err(TryLockError::Error(e)) => Err(ClauckError::io("lock", &path, &e)),
        }
    }
}

fn current_hash(path: &Path) -> Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ClauckError::io("inspect", path, &e)),
        Ok(m) if m.file_type().is_symlink() || !m.is_file() => Err(ClauckError::new(
            ErrorCode::UnsafePath,
            format!("{} is not a regular file", path.display()),
        )),
        Ok(_) => {
            let data = fs::read(path).map_err(|e| ClauckError::io("read", path, &e))?;
            Ok(Some(sha256_hex(&data)))
        }
    }
}

pub struct Prepared<'a> {
    project: &'a Project,
    journal: Journal,
}

pub fn prepare<'a>(
    project: &'a Project,
    operation: &str,
    changes: &[Change],
) -> Result<Prepared<'a>> {
    let mut entries = Vec::with_capacity(changes.len());
    let cleanup = |entries: &[JournalEntry]| {
        for e in entries {
            if let Some(staged) = &e.staged {
                if let Ok(target) = project.path(&e.path) {
                    if let Some(dir) = target.parent() {
                        let _ = fs::remove_file(dir.join(staged));
                    }
                }
            }
        }
    };
    for change in changes {
        let target = project.path(change.path)?;
        let now = current_hash(&target)?;
        if now != change.before_sha256 {
            cleanup(&entries);
            return Err(ClauckError::new(
                ErrorCode::Conflict,
                format!("{} changed while clauck was running", change.path),
            )
            .with_hint("Rerun the command."));
        }
        let entry = match &change.kind {
            ChangeKind::Write(data) => {
                if let Some(dir) = target.parent() {
                    fs::create_dir_all(dir).map_err(|e| ClauckError::io("create", dir, &e))?;
                }
                let staged = match fsutil::stage_file(&target, data) {
                    Ok(p) => p,
                    Err(e) => {
                        cleanup(&entries);
                        return Err(e);
                    }
                };
                JournalEntry {
                    path: change.path.to_string(),
                    action: "write".into(),
                    staged: staged.file_name().map(|n| n.to_string_lossy().into_owned()),
                    before_sha256: change.before_sha256.clone(),
                    after_sha256: Some(sha256_hex(data)),
                }
            }
            ChangeKind::Delete => JournalEntry {
                path: change.path.to_string(),
                action: "delete".into(),
                staged: None,
                before_sha256: change.before_sha256.clone(),
                after_sha256: None,
            },
        };
        entries.push(entry);
    }
    let dirs: std::collections::BTreeSet<_> = journal_dirs(project, &entries);
    for d in &dirs {
        fsutil::sync_dir(Some(d));
    }
    let journal = Journal {
        schema: JOURNAL_SCHEMA.into(),
        version: 1,
        operation: operation.into(),
        entries,
    };
    let bytes = serde_json::to_vec_pretty(&journal)
        .map_err(|e| ClauckError::new(ErrorCode::Internal, e.to_string()))?;
    let journal_path = project.path(JOURNAL_FILE)?;
    if let Err(e) = fsutil::atomic_write(&journal_path, &bytes) {
        cleanup(&journal.entries);
        return Err(e);
    }
    Ok(Prepared { project, journal })
}

fn journal_dirs(
    project: &Project,
    entries: &[JournalEntry],
) -> std::collections::BTreeSet<std::path::PathBuf> {
    entries
        .iter()
        .filter_map(|e| project.path(&e.path).ok())
        .filter_map(|p| p.parent().map(Path::to_path_buf))
        .collect()
}

impl Prepared<'_> {
    pub fn apply(self) -> Result<RecoveryReport> {
        let report = roll_forward(self.project, &self.journal)?;
        remove_journal(self.project)?;
        if report.conflicts.is_empty() {
            return Ok(report);
        }
        Err(ClauckError::new(
            ErrorCode::Conflict,
            format!(
                "some files changed while clauck was writing and were left as they are:\n  - {}",
                report.conflicts.join("\n  - ")
            ),
        )
        .with_hint("Run `clauck doctor`, then rerun the command."))
    }
}

pub fn commit(project: &Project, operation: &str, changes: &[Change]) -> Result<RecoveryReport> {
    if changes.is_empty() {
        return Ok(RecoveryReport::default());
    }
    prepare(project, operation, changes)?.apply()
}

#[derive(Debug, Default)]
pub struct RecoveryReport {
    pub recovered_operation: Option<String>,
    pub applied: Vec<String>,
    pub conflicts: Vec<String>,
    pub removed_temp_files: usize,
}

fn roll_forward(project: &Project, journal: &Journal) -> Result<RecoveryReport> {
    let mut report = RecoveryReport::default();
    for entry in &journal.entries {
        if !project::is_managed_path(&entry.path) {
            report
                .conflicts
                .push(format!("ignored unexpected journal path {:?}", entry.path));
            continue;
        }
        let target = project.path(&entry.path)?;
        let now = current_hash(&target)?;
        let staged = entry
            .staged
            .as_deref()
            .filter(|s| s.contains(TMP_MARKER) && !s.contains(['/', '\\']))
            .and_then(|s| target.parent().map(|d| d.join(s)));
        match entry.action.as_str() {
            "write" => {
                if now == entry.after_sha256 {
                    if let Some(s) = &staged {
                        let _ = fs::remove_file(s);
                    }
                } else if now == entry.before_sha256 && staged.as_ref().is_some_and(|s| s.is_file())
                {
                    let s = staged.as_ref().expect("checked above");
                    fs::rename(s, &target).map_err(|e| ClauckError::io("replace", &target, &e))?;
                    fsutil::sync_dir(target.parent());
                    report.applied.push(entry.path.clone());
                } else {
                    if let Some(s) = &staged {
                        let _ = fs::remove_file(s);
                    }
                    report.conflicts.push(format!(
                        "{} was changed by someone else; left as is",
                        entry.path
                    ));
                }
            }
            "delete" => {
                if now.is_none() {
                    continue;
                }
                if now == entry.before_sha256 {
                    fs::remove_file(&target).map_err(|e| ClauckError::io("remove", &target, &e))?;
                    fsutil::sync_dir(target.parent());
                    report.applied.push(entry.path.clone());
                } else {
                    report.conflicts.push(format!(
                        "{} was changed by someone else; not removed",
                        entry.path
                    ));
                }
            }
            other => report
                .conflicts
                .push(format!("ignored unknown journal action {other:?}")),
        }
    }
    Ok(report)
}

fn remove_journal(project: &Project) -> Result<()> {
    let path = project.path(JOURNAL_FILE)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ClauckError::io("remove", &path, &e)),
    }
}

pub fn journal_pending(project: &Project) -> bool {
    project.root.join(JOURNAL_FILE).is_file()
}

pub fn recover(project: &Project) -> Result<RecoveryReport> {
    let journal_path = project.path(JOURNAL_FILE)?;
    let mut report = match fsutil::read_bounded(&journal_path, MAX_METADATA_BYTES) {
        Ok(None) => RecoveryReport::default(),
        Ok(Some(bytes)) => match serde_json::from_slice::<Journal>(&bytes) {
            Ok(j) if j.schema == JOURNAL_SCHEMA && j.version == 1 => {
                let mut r = roll_forward(project, &j)?;
                r.recovered_operation = Some(j.operation.clone());
                remove_journal(project)?;
                r
            }
            _ => {
                remove_journal(project)?;
                RecoveryReport {
                    conflicts: vec!["discarded an unreadable journal; no files were changed".into()],
                    ..RecoveryReport::default()
                }
            }
        },
        Err(e) if e.code == ErrorCode::InvalidMetadata => {
            remove_journal(project)?;
            RecoveryReport {
                conflicts: vec![format!("discarded an invalid journal: {}", e.message)],
                ..RecoveryReport::default()
            }
        }
        Err(e) => return Err(e),
    };
    report.removed_temp_files = remove_stray_temp_files(project);
    Ok(report)
}

fn remove_stray_temp_files(project: &Project) -> usize {
    let mut removed = 0;
    for dir in [project.root.clone(), project.root.join(CLAUCK_DIR)] {
        removed += stray_temp_files(&dir)
            .into_iter()
            .filter(|p| fs::remove_file(p).is_ok())
            .count();
    }
    removed
}

pub fn stray_temp_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with('.') && name.contains(TMP_MARKER)
        })
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.path())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{AGENTS_MD, POLICY_FILE};

    fn project() -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        let p = Project::open(Some(dir.path())).unwrap();
        fs::create_dir_all(p.root.join(CLAUCK_DIR)).unwrap();
        (dir, p)
    }

    fn write(path: &'static str, data: &str, before: Option<&str>) -> Change {
        Change {
            path,
            kind: ChangeKind::Write(data.as_bytes().to_vec()),
            before_sha256: before.map(|b| sha256_hex(b.as_bytes())),
        }
    }

    #[test]
    fn commit_applies_all_changes() {
        let (_d, p) = project();
        commit(
            &p,
            "t",
            &[write(AGENTS_MD, "a", None), write(POLICY_FILE, "b", None)],
        )
        .unwrap();
        assert_eq!(fs::read_to_string(p.root.join(AGENTS_MD)).unwrap(), "a");
        assert!(!journal_pending(&p));
    }

    #[test]
    fn stale_plan_is_rejected_before_any_write() {
        let (_d, p) = project();
        fs::write(p.root.join(AGENTS_MD), "human").unwrap();
        let err = commit(
            &p,
            "t",
            &[write(POLICY_FILE, "b", None), write(AGENTS_MD, "x", None)],
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict);
        assert!(!p.root.join(POLICY_FILE).exists());
        assert!(stray_temp_files(&p.root.join(CLAUCK_DIR)).is_empty());
    }

    #[test]
    fn interrupted_commit_rolls_forward() {
        let (_d, p) = project();
        let prepared = prepare(&p, "lock", &[write(AGENTS_MD, "new", None)]).unwrap();
        drop(prepared);
        assert!(journal_pending(&p));
        let report = recover(&p).unwrap();
        assert_eq!(report.applied, vec![AGENTS_MD.to_string()]);
        assert_eq!(fs::read_to_string(p.root.join(AGENTS_MD)).unwrap(), "new");
        assert!(!journal_pending(&p));
    }

    #[test]
    fn recovery_never_overwrites_newer_human_edit() {
        let (_d, p) = project();
        let prepared = prepare(&p, "lock", &[write(AGENTS_MD, "clauck", None)]).unwrap();
        drop(prepared);
        fs::write(p.root.join(AGENTS_MD), "human wrote this").unwrap();
        let report = recover(&p).unwrap();
        assert_eq!(report.conflicts.len(), 1);
        assert_eq!(
            fs::read_to_string(p.root.join(AGENTS_MD)).unwrap(),
            "human wrote this"
        );
        assert!(stray_temp_files(&p.root).is_empty());
    }

    #[test]
    fn uncommitted_staging_is_cleaned_up() {
        let (_d, p) = project();
        fsutil::stage_file(&p.root.join(AGENTS_MD), b"partial").unwrap();
        let report = recover(&p).unwrap();
        assert_eq!(report.removed_temp_files, 1);
        assert!(!p.root.join(AGENTS_MD).exists());
    }

    #[test]
    fn garbage_journal_is_discarded_safely() {
        let (_d, p) = project();
        fs::write(p.root.join(AGENTS_MD), "keep").unwrap();
        fs::write(p.root.join(JOURNAL_FILE), "{not json").unwrap();
        let report = recover(&p).unwrap();
        assert_eq!(report.conflicts.len(), 1);
        assert_eq!(fs::read_to_string(p.root.join(AGENTS_MD)).unwrap(), "keep");
    }

    #[test]
    fn journal_with_foreign_path_is_ignored() {
        let (_d, p) = project();
        let j = r#"{"schema":"clauck.journal","version":1,"operation":"x","entries":[
            {"path":"../evil","action":"delete","staged":null,"before_sha256":null,"after_sha256":null}]}"#;
        fs::write(p.root.join(JOURNAL_FILE), j).unwrap();
        let report = recover(&p).unwrap();
        assert_eq!(report.conflicts.len(), 1);
    }

    #[test]
    fn apply_reports_targets_changed_after_prepare() {
        let (_d, p) = project();
        let prepared = prepare(
            &p,
            "t",
            &[
                write(AGENTS_MD, "clauck", None),
                write(POLICY_FILE, "b", None),
            ],
        )
        .unwrap();
        fs::write(p.root.join(AGENTS_MD), "human").unwrap();
        let err = prepared.apply().unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict);
        assert_eq!(fs::read_to_string(p.root.join(AGENTS_MD)).unwrap(), "human");
        assert!(!journal_pending(&p));
    }

    #[test]
    fn second_lock_is_busy() {
        let (_d, p) = project();
        let _held = ProjectLock::acquire(&p).unwrap();
        let err = ProjectLock::acquire(&p).err().unwrap();
        assert_eq!(err.code, ErrorCode::Busy);
    }
}
