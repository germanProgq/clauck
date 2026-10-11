use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::error::{ClauckError, ErrorCode, Result};
use crate::fsutil::{self, MAX_METADATA_BYTES};
use crate::project::display_path;

pub const AGENT_ID: &str = "claude-code";
pub const HOOK_SUBCOMMAND: &str = "hook claude-code";
pub const HOOK_TIMEOUT_SECS: u64 = 10;
const SETTINGS_LIMIT: u64 = 4 * MAX_METADATA_BYTES;

pub const READ_ONLY_TOOLS: [&str; 17] = [
    "Read",
    "Glob",
    "Grep",
    "LSP",
    "WebFetch",
    "WebSearch",
    "ToolSearch",
    "TodoWrite",
    "TaskGet",
    "TaskList",
    "TaskOutput",
    "CronList",
    "ListMcpResourcesTool",
    "ReadMcpResourceTool",
    "AskUserQuestion",
    "EnterPlanMode",
    "ExitPlanMode",
];

pub const DELEGATING_TOOLS: [&str; 2] = ["Agent", "Task"];

pub fn settings_path() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir).join("settings.json"));
    }
    directories::BaseDirs::new()
        .map(|b| b.home_dir().join(".claude").join("settings.json"))
        .ok_or_else(|| {
            ClauckError::new(
                ErrorCode::IntegrationMissing,
                "cannot locate the home directory",
            )
        })
}

pub fn hook_command(exe: &Path, strict: bool) -> String {
    let flag = if strict { " --strict" } else { "" };
    format!("\"{}\" {HOOK_SUBCOMMAND}{flag}", display_path(exe))
}

fn is_clauck_handler(handler: &Value) -> bool {
    handler
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| c.contains(&format!("\" {HOOK_SUBCOMMAND}")) && c.contains("clauck"))
}

fn load_settings(path: &Path) -> Result<Map<String, Value>> {
    let real = resolve(path);
    match fsutil::read_bounded(&real, SETTINGS_LIMIT)? {
        None => Ok(Map::new()),
        Some(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(m)) => Ok(m),
            _ => Err(ClauckError::new(
                ErrorCode::InvalidMetadata,
                format!("{} is not a JSON object; not modified", display_path(&real)),
            )),
        },
    }
}

fn resolve(path: &Path) -> PathBuf {
    if fsutil::is_symlink(path) {
        fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}

fn save_settings(path: &Path, settings: &Map<String, Value>) -> Result<()> {
    let real = resolve(path);
    if let Some(dir) = real.parent() {
        fs::create_dir_all(dir).map_err(|e| ClauckError::io("create", dir, &e))?;
    }
    let mut bytes = serde_json::to_vec_pretty(&Value::Object(settings.clone()))
        .map_err(|e| ClauckError::new(ErrorCode::Internal, e.to_string()))?;
    bytes.push(b'\n');
    fsutil::atomic_write(&real, &bytes)
}

pub fn hook_group(command: &str, fail_closed: bool) -> Value {
    let mut handler =
        json!({ "type": "command", "command": command, "timeout": HOOK_TIMEOUT_SECS });
    if fail_closed {
        handler["onFailure"] = json!("block");
    }
    json!({ "matcher": "*", "hooks": [handler] })
}

fn without_clauck(settings: &Map<String, Value>) -> (Map<String, Value>, usize) {
    let mut out = settings.clone();
    let mut removed = 0;
    let Some(groups) = out
        .get("hooks")
        .and_then(|h| h.get("PreToolUse"))
        .and_then(Value::as_array)
        .cloned()
    else {
        return (out, 0);
    };
    let kept: Vec<Value> = groups
        .into_iter()
        .filter_map(|group| {
            let handlers = group.get("hooks").and_then(Value::as_array)?.clone();
            let remaining: Vec<Value> = handlers
                .iter()
                .filter(|h| !is_clauck_handler(h))
                .cloned()
                .collect();
            removed += handlers.len() - remaining.len();
            if remaining.is_empty() && !handlers.is_empty() {
                return None;
            }
            let mut g = group.clone();
            g["hooks"] = Value::Array(remaining);
            Some(g)
        })
        .collect();
    if let Some(Value::Object(hooks)) = out.get_mut("hooks") {
        if kept.is_empty() {
            hooks.remove("PreToolUse");
        } else {
            hooks.insert("PreToolUse".into(), Value::Array(kept));
        }
        if hooks.is_empty() {
            out.remove("hooks");
        }
    }
    (out, removed)
}

pub fn install(path: &Path, command: &str, fail_closed: bool) -> Result<()> {
    let (mut settings, _) = without_clauck(&load_settings(path)?);
    let hooks = settings.entry("hooks").or_insert_with(|| json!({}));
    let Value::Object(hooks) = hooks else {
        return Err(ClauckError::new(
            ErrorCode::InvalidMetadata,
            "settings \"hooks\" is not an object; not modified",
        ));
    };
    let pre = hooks.entry("PreToolUse").or_insert_with(|| json!([]));
    let Value::Array(pre) = pre else {
        return Err(ClauckError::new(
            ErrorCode::InvalidMetadata,
            "settings \"hooks.PreToolUse\" is not an array; not modified",
        ));
    };
    pre.push(hook_group(command, fail_closed));
    save_settings(path, &settings)
}

pub fn uninstall(path: &Path) -> Result<usize> {
    let settings = load_settings(path)?;
    let (cleaned, removed) = without_clauck(&settings);
    if removed > 0 {
        save_settings(path, &cleaned)?;
    }
    Ok(removed)
}

pub fn installed_commands(path: &Path) -> Result<Vec<String>> {
    let settings = load_settings(path)?;
    Ok(settings
        .get("hooks")
        .and_then(|h| h.get("PreToolUse"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|g| g.get("hooks").and_then(Value::as_array))
        .flatten()
        .filter(|h| is_clauck_handler(h))
        .filter_map(|h| h.get("command").and_then(Value::as_str).map(String::from))
        .collect())
}

pub fn command_executable(command: &str) -> Option<PathBuf> {
    let rest = command.strip_prefix('"')?;
    rest.split_once('"').map(|(exe, _)| PathBuf::from(exe))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_preserves_other_settings_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        fs::write(
            &p,
            r#"{"model":"x","hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"my-check"}]}],"Stop":[]}}"#,
        )
        .unwrap();
        let cmd = hook_command(Path::new("/opt/my tools/clauck"), false);
        install(&p, &cmd, false).unwrap();
        install(&p, &cmd, false).unwrap();
        assert_eq!(installed_commands(&p).unwrap(), vec![cmd.clone()]);
        let v: Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
        assert_eq!(v["model"], "x");
        assert_eq!(v["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        assert_eq!(uninstall(&p).unwrap(), 1);
        let v: Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
        assert_eq!(
            v["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "my-check"
        );
        assert!(v["hooks"].get("Stop").is_some());
    }

    #[test]
    fn uninstall_removes_empty_containers_it_created() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        install(&p, &hook_command(Path::new("/bin/clauck"), true), true).unwrap();
        let v: Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
        assert_eq!(
            v["hooks"]["PreToolUse"][0]["hooks"][0]["onFailure"],
            "block"
        );
        uninstall(&p).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap().trim(), "{}");
    }

    #[test]
    fn refuses_non_object_settings() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        fs::write(&p, "[1,2]").unwrap();
        assert!(install(&p, "x", false).is_err());
        assert_eq!(fs::read_to_string(&p).unwrap(), "[1,2]");
    }

    #[test]
    fn parses_executable_with_spaces() {
        let cmd = hook_command(Path::new("/Users/a b/clauck"), false);
        assert_eq!(
            command_executable(&cmd).unwrap(),
            PathBuf::from("/Users/a b/clauck")
        );
    }
}
