use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::commands::doctor::Severity;

pub const TESTED_CLAUDE_CODE: &str = "2.1.278";
pub const TESTED_CODEX: &str = "0.154.0";
const CLAUDE_NATIVE_AGENTS_MD: (u64, u64, u64) = (2, 1, 277);
pub const CLAUDE_HOOK_ON_FAILURE: (u64, u64, u64) = (2, 1, 295);

pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    text.split_whitespace().find_map(|tok| {
        let mut parts = tok.trim_start_matches('v').split('.');
        let a = parts.next()?.parse().ok()?;
        let b = parts.next()?.parse().ok()?;
        let c = parts
            .next()?
            .split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse()
            .ok()?;
        Some((a, b, c))
    })
}

fn run_version(bin: &str) -> Option<String> {
    let mut child = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    use std::io::Read;
    child
        .stdout
        .take()?
        .take(4096)
        .read_to_string(&mut out)
        .ok()?;
    Some(out.trim().to_string())
}

pub fn check() -> Vec<(Severity, &'static str, String)> {
    let mut out = Vec::new();
    match run_version("claude").and_then(|v| parse_version(&v).map(|p| (v, p))) {
        None => out.push((
            Severity::Info,
            "claude_code_not_found",
            "Claude Code not found on PATH".to_string(),
        )),
        Some((raw, v)) => {
            let tested = parse_version(TESTED_CLAUDE_CODE).expect("const");
            if v < CLAUDE_NATIVE_AGENTS_MD {
                out.push((
                    Severity::Warning,
                    "unsupported_agent_version",
                    format!("Claude Code {raw} predates documented AGENTS.md handling (2.1.277); the CLAUDE.md block still works"),
                ));
            } else if v != tested {
                out.push((
                    Severity::Info,
                    "untested_agent_version",
                    format!(
                        "Claude Code {raw}; documentation was checked against {TESTED_CLAUDE_CODE}"
                    ),
                ));
            }
            if v < CLAUDE_HOOK_ON_FAILURE {
                out.push((
                    Severity::Info,
                    "hook_fails_open",
                    format!("Claude Code {raw} lets tool calls proceed when a hook crashes or times out (onFailure needs 2.1.295+)"),
                ));
            }
        }
    }
    match run_version("codex").and_then(|v| parse_version(&v).map(|p| (v, p))) {
        None => out.push((
            Severity::Info,
            "codex_not_found",
            "Codex CLI not found on PATH".to_string(),
        )),
        Some((raw, v)) if Some(v) != parse_version(TESTED_CODEX) => out.push((
            Severity::Info,
            "untested_agent_version",
            format!("Codex {raw}; AGENTS.md format checked against {TESTED_CODEX}"),
        )),
        Some(_) => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_agent_version_strings() {
        assert_eq!(parse_version("2.1.278 (Claude Code)"), Some((2, 1, 278)));
        assert_eq!(parse_version("codex-cli 0.154.0"), Some((0, 154, 0)));
        assert_eq!(parse_version("v1.2.3-beta"), Some((1, 2, 3)));
        assert_eq!(parse_version("nothing"), None);
    }
}
