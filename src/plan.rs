use crate::adapters::entry_files::{self, ClaudeContext, EntryAdapter};
use crate::blocks;
use crate::error::{ClauckError, ErrorCode, Result};
use crate::fsutil::{self, sha256_hex, MAX_INSTRUCTION_BYTES};
use crate::manifest::{EntryKind, Manifest, ManifestEntry};
use crate::policy;
use crate::project::{
    Project, AGENTS_MD, CLAUDE_DIR_MD, CLAUDE_LOCAL_MD, CLAUDE_MD, GITIGNORE_FILE, MANIFEST_FILE,
    POLICY_FILE, STATE_FILE,
};
use crate::state::ProjectState;
use crate::txn::{Change, ChangeKind};

pub const GITIGNORE_CONTENT: &str =
    "# Local Clauck runtime files\n.lock\njournal.json\n.*.clauck-tmp-*\n";

#[derive(Debug, Default)]
pub struct Plan {
    pub changes: Vec<Change>,
    pub notes: Vec<String>,
    pub conflicts: Vec<String>,
}

impl Plan {
    pub fn require_no_conflicts(&self) -> Result<()> {
        if self.conflicts.is_empty() {
            return Ok(());
        }
        Err(ClauckError::new(
            ErrorCode::Conflict,
            format!(
                "managed content was edited by hand; nothing was changed:\n  - {}",
                self.conflicts.join("\n  - ")
            ),
        )
        .with_hint(
            "Review the files listed above. Rerun with --force to replace or remove them anyway.",
        ))
    }
}

pub enum EntryRead {
    Missing,
    Symlink,
    Text(String),
}

pub fn read_entry(project: &Project, rel: &str) -> Result<EntryRead> {
    let path = project.root.join(rel);
    if fsutil::is_symlink(&path) {
        return Ok(EntryRead::Symlink);
    }
    Ok(
        match fsutil::read_bounded_string(&path, MAX_INSTRUCTION_BYTES)? {
            None => EntryRead::Missing,
            Some(t) => EntryRead::Text(t),
        },
    )
}

fn hash_opt(data: &Option<String>) -> Option<String> {
    data.as_ref().map(|d| sha256_hex(d.as_bytes()))
}

fn read_owned(project: &Project, rel: &str) -> Result<Option<String>> {
    let path = project.path(rel)?;
    fsutil::read_bounded_string(&path, MAX_INSTRUCTION_BYTES)
}

fn has_user_content(text: &str) -> bool {
    match blocks::find(text) {
        Ok(Some(span)) => !blocks::remove(text, &span).trim().is_empty(),
        Ok(None) => !text.trim().is_empty(),
        Err(_) => true,
    }
}

fn claude_context(project: &Project, manifest: &Manifest) -> Result<ClaudeContext> {
    let claude_created = match manifest.get(CLAUDE_MD) {
        Some(e) => e.created_file,
        None => matches!(read_entry(project, CLAUDE_MD)?, EntryRead::Missing),
    };
    let other_memory = [CLAUDE_LOCAL_MD, CLAUDE_DIR_MD]
        .iter()
        .any(|p| project.root.join(p).exists());
    let agents_content = match read_entry(project, AGENTS_MD)? {
        EntryRead::Text(t) => has_user_content(&t),
        _ => false,
    };
    Ok(ClaudeContext {
        import_agents_md: claude_created && !other_memory && agents_content,
    })
}

pub fn plan_sync(
    project: &Project,
    state: &ProjectState,
    manifest: &Manifest,
    force: bool,
) -> Result<Plan> {
    let mut plan = Plan::default();
    let mut new_manifest = manifest.clone();

    let current_state = read_owned(project, STATE_FILE)?;
    let state_text = String::from_utf8(state.to_bytes()).expect("utf8");
    push_write(&mut plan, STATE_FILE, &current_state, &state_text);
    new_manifest = new_manifest.with_entry(owned_entry(STATE_FILE, None));

    for (rel, desired) in [
        (POLICY_FILE, policy::render(state)),
        (GITIGNORE_FILE, GITIGNORE_CONTENT.to_string()),
    ] {
        let current = read_owned(project, rel)?;
        let entry = owned_entry(rel, Some(sha256_hex(desired.as_bytes())));
        if current.as_deref().map(normalize_eol) == Some(desired.clone()) {
            new_manifest = new_manifest.with_entry(entry);
            continue;
        }
        let edited = current
            .as_deref()
            .is_some_and(|c| !is_clauck_owned_text(rel, c, manifest));
        if edited && !force {
            if rel == POLICY_FILE {
                plan.conflicts
                    .push(format!("{rel} differs from what Clauck last wrote"));
            } else {
                plan.notes.push(format!("left hand-edited {rel} unchanged"));
            }
            continue;
        }
        push_write(&mut plan, rel, &current, &desired);
        new_manifest = new_manifest.with_entry(entry);
    }

    let ctx = claude_context(project, manifest)?;
    for adapter in EntryAdapter::ALL {
        if let Some(entry) = plan_block(project, manifest, adapter, ctx, force, &mut plan)? {
            new_manifest = new_manifest.with_entry(entry);
        }
    }

    let current_manifest = read_owned(project, MANIFEST_FILE)?;
    let manifest_text = String::from_utf8(new_manifest.to_bytes()).expect("utf8");
    push_write(&mut plan, MANIFEST_FILE, &current_manifest, &manifest_text);
    Ok(plan)
}

fn normalize_eol(text: &str) -> String {
    text.replace("\r\n", "\n")
}

fn is_clauck_owned_text(rel: &str, text: &str, manifest: &Manifest) -> bool {
    let norm = normalize_eol(text);
    let recorded = manifest.get(rel).and_then(|e| e.sha256.as_deref());
    if recorded == Some(sha256_hex(norm.as_bytes()).as_str()) {
        return true;
    }
    match rel {
        POLICY_FILE => policy::is_generated(&norm),
        GITIGNORE_FILE => norm == GITIGNORE_CONTENT,
        _ => true,
    }
}

fn owned_entry(rel: &str, sha256: Option<String>) -> ManifestEntry {
    ManifestEntry {
        path: rel.into(),
        kind: EntryKind::File,
        created_file: true,
        sha256,
    }
}

fn push_write(plan: &mut Plan, rel: &'static str, current: &Option<String>, desired: &str) {
    if current.as_deref() == Some(desired) {
        return;
    }
    plan.changes.push(Change {
        path: rel,
        kind: ChangeKind::Write(desired.as_bytes().to_vec()),
        before_sha256: hash_opt(current),
    });
}

fn plan_block(
    project: &Project,
    manifest: &Manifest,
    adapter: EntryAdapter,
    ctx: ClaudeContext,
    force: bool,
    plan: &mut Plan,
) -> Result<Option<ManifestEntry>> {
    let rel = adapter.file();
    let recorded = manifest.get(rel);
    let desired = blocks::wrap(&entry_files::body(adapter, ctx));
    let desired_hash = blocks::block_hash(&desired);
    let entry = |created_file: bool| ManifestEntry {
        path: rel.into(),
        kind: EntryKind::Block,
        created_file,
        sha256: Some(desired_hash.clone()),
    };
    let text = match read_entry(project, rel)? {
        EntryRead::Symlink => {
            plan.notes.push(format!(
                "skipped {rel}: it is a symbolic link, and Clauck does not write through links"
            ));
            return Ok(recorded.cloned());
        }
        EntryRead::Missing => {
            if recorded.is_some() {
                plan.notes
                    .push(format!("recreated {rel}, which had been deleted"));
            }
            push_write(plan, rel, &None, &blocks::insert(None, &desired));
            return Ok(Some(entry(true)));
        }
        EntryRead::Text(t) => t,
    };
    let created_file = recorded.is_some_and(|e| e.created_file);
    let span = match blocks::find(&text) {
        Ok(s) => s,
        Err(e) => {
            plan.conflicts.push(format!("{rel}: {}", e.message));
            return Ok(recorded.cloned());
        }
    };
    let Some(span) = span else {
        if recorded.is_some() {
            plan.notes
                .push(format!("re-added the Clauck block to {rel}"));
        }
        push_write(
            plan,
            rel,
            &Some(text.clone()),
            &blocks::insert(Some(&text), &desired),
        );
        return Ok(Some(entry(created_file)));
    };
    let found_hash = blocks::block_hash(span.text(&text));
    if found_hash == desired_hash {
        return Ok(Some(entry(created_file)));
    }
    let known = recorded.and_then(|e| e.sha256.as_deref()) == Some(found_hash.as_str())
        || is_known_rendering(adapter, &found_hash);
    if !known && !force {
        plan.notes.push(format!(
            "the Clauck block in {rel} was edited by hand; left unchanged (use --force to replace it)"
        ));
        return Ok(recorded.cloned());
    }
    push_write(
        plan,
        rel,
        &Some(text.clone()),
        &blocks::replace(&text, &span, &desired),
    );
    Ok(Some(entry(created_file)))
}

fn is_known_rendering(adapter: EntryAdapter, hash: &str) -> bool {
    entry_files::known_bodies(adapter)
        .iter()
        .any(|b| blocks::block_hash(&blocks::wrap(b)) == hash)
}

pub fn plan_uninstall(project: &Project, manifest: &Manifest, force: bool) -> Result<Plan> {
    let mut plan = Plan::default();
    for adapter in EntryAdapter::ALL {
        plan_block_removal(project, manifest, adapter, force, &mut plan)?;
    }
    for rel in [POLICY_FILE, GITIGNORE_FILE, STATE_FILE, MANIFEST_FILE] {
        let Some(current) = read_owned(project, rel)? else {
            continue;
        };
        let hash = sha256_hex(current.as_bytes());
        let ours = is_clauck_owned_text(rel, &current, manifest);
        if !ours && !force {
            plan.conflicts.push(format!("{rel} was edited by hand"));
            continue;
        }
        plan.changes.push(Change {
            path: rel,
            kind: ChangeKind::Delete,
            before_sha256: Some(hash),
        });
    }
    Ok(plan)
}

fn plan_block_removal(
    project: &Project,
    manifest: &Manifest,
    adapter: EntryAdapter,
    force: bool,
    plan: &mut Plan,
) -> Result<()> {
    let rel = adapter.file();
    let text = match read_entry(project, rel)? {
        EntryRead::Text(t) => t,
        EntryRead::Missing => return Ok(()),
        EntryRead::Symlink => {
            plan.notes.push(format!("skipped {rel}: symbolic link"));
            return Ok(());
        }
    };
    let span = match blocks::find(&text) {
        Ok(Some(s)) => s,
        Ok(None) => return Ok(()),
        Err(e) => {
            plan.conflicts.push(format!("{rel}: {}", e.message));
            return Ok(());
        }
    };
    let recorded = manifest.get(rel);
    let found_hash = blocks::block_hash(span.text(&text));
    let known = recorded.and_then(|e| e.sha256.as_deref()) == Some(found_hash.as_str())
        || is_known_rendering(adapter, &found_hash);
    if !known && !force {
        plan.conflicts
            .push(format!("the Clauck block in {rel} was edited by hand"));
        return Ok(());
    }
    let remaining = blocks::remove(&text, &span);
    let created = recorded.is_some_and(|e| e.created_file);
    let before = Some(sha256_hex(text.as_bytes()));
    let kind = if created && remaining.trim().is_empty() {
        ChangeKind::Delete
    } else {
        ChangeKind::Write(remaining.into_bytes())
    };
    plan.changes.push(Change {
        path: rel,
        kind,
        before_sha256: before,
    });
    Ok(())
}
