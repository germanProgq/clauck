use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::adapters::claude_code::{DELEGATING_TOOLS, READ_ONLY_TOOLS};
use crate::policy::BLOCKED_MESSAGE;
use crate::project::{display_path, Project, STATE_FILE};
use crate::state;
use crate::store::{grants, ledger, Store};

const MAX_STRINGS: usize = 10_000;
const MAX_PATH_CANDIDATES: usize = 256;
const SEARCH_TOOLS: [&str; 2] = ["Grep", "Glob"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Defer,
    Deny(String),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    pub strict: bool,
}

pub struct HookInput {
    pub event: Option<String>,
    pub session_id: String,
    pub cwd: Option<PathBuf>,
    pub tool_name: String,
    pub paths: Vec<PathBuf>,
    pub strings: Vec<String>,
}

fn collect_strings(v: &Value, out: &mut Vec<String>) {
    if out.len() >= MAX_STRINGS {
        return;
    }
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => items.iter().for_each(|i| collect_strings(i, out)),
        Value::Object(map) => map.values().for_each(|i| collect_strings(i, out)),
        _ => {}
    }
}

pub fn parse_input(bytes: &[u8]) -> Option<HookInput> {
    let v: Value = serde_json::from_slice(bytes).ok()?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(String::from);
    let cwd = s("cwd").map(PathBuf::from);
    let tool_input = v.get("tool_input");
    let paths = ["file_path", "notebook_path", "path"]
        .iter()
        .filter_map(|k| tool_input.and_then(|t| t.get(*k)).and_then(Value::as_str))
        .map(|p| match &cwd {
            Some(c) if Path::new(p).is_relative() => c.join(p),
            _ => PathBuf::from(p),
        })
        .collect();
    let mut strings = Vec::new();
    if let Some(t) = tool_input {
        collect_strings(t, &mut strings);
    }
    Some(HookInput {
        event: s("hook_event_name"),
        session_id: s("session_id").unwrap_or_default(),
        cwd,
        tool_name: s("tool_name")?,
        paths,
        strings,
    })
}

fn absolute_tokens(strings: &[String]) -> Vec<PathBuf> {
    let separators = |c: char| c.is_whitespace() || "\"'`;|&()<>=,".contains(c);
    strings
        .iter()
        .flat_map(|s| s.split(separators))
        .filter(|t| !t.is_empty() && Path::new(t).is_absolute())
        .take(MAX_PATH_CANDIDATES)
        .map(PathBuf::from)
        .collect()
}

pub fn resolve(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut rest = Vec::new();
    loop {
        if let Ok(c) = std::fs::canonicalize(&existing) {
            let mut out = PathBuf::from(display_path(&c));
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (
            existing.file_name().map(|n| n.to_os_string()),
            existing.parent(),
        ) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
}

fn is_within(path: &Path, root: &Path) -> bool {
    let p: Vec<Component> = path.components().collect();
    let r: Vec<Component> = root.components().collect();
    p.len() >= r.len() && p[..r.len()] == r[..]
}

struct Found {
    project: Project,
    locked: bool,
    generation: Option<u64>,
}

fn evaluate(store: Option<&Store>, project: Project) -> Found {
    let record = store.and_then(|s| ledger::load(s, &project).ok().flatten());
    let has_state = project.root.join(STATE_FILE).exists();
    let state_locked = has_state
        && state::load(&project)
            .map(|s| s.is_none_or(|s| s.locked))
            .unwrap_or(true);
    Found {
        locked: state_locked || record.as_ref().is_some_and(|r| r.locked),
        generation: record.map(|r| r.lock_generation),
        project,
    }
}

fn projects_containing(store: Option<&Store>, start: &Path, out: &mut BTreeMap<PathBuf, Found>) {
    for dir in start.ancestors().take(128) {
        if out.keys().any(|k| k == dir) {
            continue;
        }
        let Ok(project) = Project::open(Some(dir)) else {
            continue;
        };
        if out.contains_key(&project.root) {
            continue;
        }
        let known = project.root.join(STATE_FILE).exists()
            || store.is_some_and(|s| ledger::load(s, &project).ok().flatten().is_some());
        if known {
            out.insert(project.root.clone(), evaluate(store, project));
        }
    }
}

fn involved_projects(input: &HookInput, store: Option<&Store>, opts: Options) -> Vec<Found> {
    let mut found = BTreeMap::new();
    let mut candidates: Vec<PathBuf> = input.cwd.iter().cloned().collect();
    candidates.extend(input.paths.iter().cloned());
    candidates.extend(absolute_tokens(&input.strings));
    for c in candidates.iter().take(MAX_PATH_CANDIDATES) {
        projects_containing(store, c, &mut found);
        let resolved = resolve(c);
        if &resolved != c {
            projects_containing(store, &resolved, &mut found);
        }
    }
    let read_only = is_read_only(&input.tool_name);
    if let Some(store) = store {
        for rec in ledger::list(store).into_iter().filter(|r| r.locked) {
            let root = PathBuf::from(&rec.root);
            let shown = display_path(&root);
            let mentioned = input
                .strings
                .iter()
                .any(|s| s.contains(&rec.root) || s.contains(&shown));
            if (mentioned || (opts.strict && !read_only)) && !found.contains_key(&root) {
                if let Ok(project) = Project::open(Some(&root)) {
                    found.insert(project.root.clone(), evaluate(Some(store), project));
                }
            }
        }
    }
    found.into_values().filter(|f| f.locked).collect()
}

fn is_read_only(tool: &str) -> bool {
    READ_ONLY_TOOLS.contains(&tool) || DELEGATING_TOOLS.contains(&tool)
}

fn touches_store(input: &HookInput, store: &Store) -> bool {
    let store_root = resolve(&store.root);
    let mentioned = input
        .paths
        .iter()
        .cloned()
        .chain(absolute_tokens(&input.strings))
        .any(|p| is_within(&resolve(&p), &store_root));
    let search_root = SEARCH_TOOLS
        .contains(&input.tool_name.as_str())
        .then(|| input.paths.first().or(input.cwd.as_ref()).cloned())
        .flatten();
    let contains = search_root.is_some_and(|r| is_within(&store_root, &resolve(&r)));
    mentioned || contains
}

pub fn decide(input: &HookInput, store: Option<&Store>, now: u64, opts: Options) -> Decision {
    if input.event.as_deref().is_some_and(|e| e != "PreToolUse") {
        return Decision::Defer;
    }
    if store.is_some_and(|s| touches_store(input, s)) {
        return Decision::Deny(
            "Clauck: the local Clauck credential store is not available to agents.".into(),
        );
    }
    let locked = involved_projects(input, store, opts);
    let Some(first) = locked.first() else {
        return Decision::Defer;
    };
    let granted = !input.session_id.is_empty()
        && locked.iter().all(|f| match (store, f.generation) {
            (Some(s), Some(generation)) => {
                grants::find_valid(s, &f.project, &input.session_id, generation, now).is_some()
            }
            _ => false,
        });
    if granted || is_read_only(&input.tool_name) {
        return Decision::Defer;
    }
    Decision::Deny(deny_reason(
        &input.tool_name,
        &first.project,
        &input.session_id,
    ))
}

pub fn deny_reason(tool: &str, project: &Project, session_id: &str) -> String {
    let authorize = if session_id.is_empty() {
        String::new()
    } else {
        format!(" or `clauck authorize --session {session_id} --ttl 30m`")
    };
    format!(
        "{BLOCKED_MESSAGE} Clauck denied the {tool} tool because {} is locked. \
         The owner can run `clauck unlock`{authorize} in their own terminal. \
         Do not retry or work around this; tell the user the change was not made.",
        project.display_root()
    )
}

pub fn deny_output(reason: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(tool: &str, cwd: &Path) -> HookInput {
        HookInput {
            event: Some("PreToolUse".into()),
            session_id: "sess-1".into(),
            cwd: Some(cwd.to_path_buf()),
            tool_name: tool.into(),
            paths: Vec::new(),
            strings: Vec::new(),
        }
    }

    #[test]
    fn parses_documented_fields_and_all_strings() {
        let raw = br#"{"session_id":"s","cwd":"/x","hook_event_name":"PreToolUse","tool_name":"mcp__fs__move","tool_input":{"file_path":"a.rs","args":{"destination":"/y/z"}}}"#;
        let i = parse_input(raw).unwrap();
        assert_eq!(i.paths, vec![PathBuf::from("/x/a.rs")]);
        assert!(i.strings.contains(&"/y/z".to_string()));
        assert!(parse_input(b"not json").is_none());
    }

    #[test]
    fn finds_absolute_tokens_in_commands() {
        let toks = absolute_tokens(&["cd /tmp && rm -rf \"/srv/app\";echo x>/var/log/y".into()]);
        let toks: Vec<_> = toks
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert!(toks.contains(&"/tmp".into()));
        assert!(toks.contains(&"/srv/app".into()));
        assert!(toks.contains(&"/var/log/y".into()));
    }

    #[test]
    fn unrelated_directory_is_deferred() {
        let dir = tempfile::tempdir().unwrap();
        let d = decide(&input("Bash", dir.path()), None, 0, Options::default());
        assert_eq!(d, Decision::Defer);
    }

    #[test]
    fn missing_event_name_is_treated_as_pre_tool_use() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".clauck")).unwrap();
        std::fs::write(dir.path().join(STATE_FILE), "garbage").unwrap();
        let mut i = input("Write", dir.path());
        i.event = None;
        assert!(matches!(
            decide(&i, None, 0, Options::default()),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn malformed_state_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".clauck")).unwrap();
        std::fs::write(dir.path().join(STATE_FILE), "garbage").unwrap();
        let opts = Options::default();
        assert!(matches!(
            decide(&input("Write", dir.path()), None, 0, opts),
            Decision::Deny(_)
        ));
        assert_eq!(
            decide(&input("Read", dir.path()), None, 0, opts),
            Decision::Defer
        );
    }

    #[test]
    fn resolve_handles_missing_tails() {
        let dir = tempfile::tempdir().unwrap();
        let r = resolve(&dir.path().join("a/b/c.txt"));
        assert!(r.ends_with("a/b/c.txt"));
        assert!(is_within(&r, &resolve(dir.path())));
    }

    #[test]
    fn deny_output_matches_documented_shape() {
        let v: Value = serde_json::from_str(&deny_output("no")).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(v["hookSpecificOutput"]["permissionDecisionReason"], "no");
    }
}
