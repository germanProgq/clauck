use crate::project::{AGENTS_MD, CLAUDE_MD};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryAdapter {
    AgentsMd,
    ClaudeMd,
}

impl EntryAdapter {
    pub const ALL: [EntryAdapter; 2] = [EntryAdapter::AgentsMd, EntryAdapter::ClaudeMd];

    pub fn id(self) -> &'static str {
        match self {
            EntryAdapter::AgentsMd => "agents-md",
            EntryAdapter::ClaudeMd => "claude-md",
        }
    }

    pub fn file(self) -> &'static str {
        match self {
            EntryAdapter::AgentsMd => AGENTS_MD,
            EntryAdapter::ClaudeMd => CLAUDE_MD,
        }
    }

    pub fn agents(self) -> &'static str {
        match self {
            EntryAdapter::AgentsMd => "OpenAI Codex CLI and other AGENTS.md readers",
            EntryAdapter::ClaudeMd => "Claude Code",
        }
    }

    pub fn mechanism(self) -> &'static str {
        match self {
            EntryAdapter::AgentsMd => "plain-text pointer (AGENTS.md has no import syntax)",
            EntryAdapter::ClaudeMd => "@-import of .clauck/policy.md",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClaudeContext {
    pub import_agents_md: bool,
}

const POINTER: &str = "Before creating, editing, deleting, or renaming files, or running commands
that change this workspace, read `.clauck/policy.md` and follow it. It says
whether AI edits are currently paused. You do not need to mention this check.";

pub fn agents_md_body() -> String {
    format!("## AI edit policy (Clauck)\n\n{POINTER}")
}

pub fn claude_md_body(ctx: ClaudeContext) -> String {
    let agents = if ctx.import_agents_md {
        "@AGENTS.md\n"
    } else {
        ""
    };
    format!(
        "## AI edit policy (Clauck)\n\n{agents}@.clauck/policy.md\n\n\
         The policy above was loaded when this session started and may have changed.\n\
         {POINTER}"
    )
}

pub fn body(adapter: EntryAdapter, ctx: ClaudeContext) -> String {
    match adapter {
        EntryAdapter::AgentsMd => agents_md_body(),
        EntryAdapter::ClaudeMd => claude_md_body(ctx),
    }
}

pub fn known_bodies(adapter: EntryAdapter) -> Vec<String> {
    match adapter {
        EntryAdapter::AgentsMd => vec![agents_md_body()],
        EntryAdapter::ClaudeMd => [false, true]
            .into_iter()
            .map(|import_agents_md| claude_md_body(ClaudeContext { import_agents_md }))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_is_imported_exactly_once_and_only_in_claude_md() {
        assert!(!agents_md_body().contains('@'));
        for body in known_bodies(EntryAdapter::ClaudeMd) {
            assert_eq!(body.matches("@.clauck/policy.md").count(), 1);
        }
    }

    #[test]
    fn blocks_are_small() {
        for a in EntryAdapter::ALL {
            for b in known_bodies(a) {
                assert!(b.lines().count() <= 10, "{} block too long", a.id());
            }
        }
    }

    #[test]
    fn imports_are_outside_code_spans() {
        for body in known_bodies(EntryAdapter::ClaudeMd) {
            for line in body.lines().filter(|l| l.contains('@')) {
                assert!(
                    line.starts_with('@'),
                    "import must start its own line: {line}"
                );
            }
        }
    }
}
