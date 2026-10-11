use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::commands;
use crate::error::{ClauckError, ErrorCode, Result};

#[derive(Debug, Parser)]
#[command(
    name = "clauck",
    version,
    about = "Reversible, advisory AI-edit policies for project folders",
    long_about = "Clauck asks cooperating AI agents not to modify a project while it is locked.\n\
                  The lock is ADVISORY: it does not stop people, other tools, or agents that ignore it.\n\
                  An optional Claude Code hook (`clauck integration`) adds partial enforcement for\n\
                  Claude Code sessions on this machine."
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalOpts,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Args)]
pub struct GlobalOpts {
    #[arg(
        help = "Project directory (defaults to the current directory)",
        long,
        global = true,
        value_name = "DIR"
    )]
    pub path: Option<PathBuf>,
    #[arg(
        help = "Show what would change without writing anything",
        long,
        global = true
    )]
    pub dry_run: bool,
    #[arg(
        help = "Never prompt; fail with E_AUTH_REQUIRED / E_CONSENT_REQUIRED instead",
        long,
        global = true
    )]
    pub non_interactive: bool,
    #[arg(
        help = "Read passwords from standard input, one per line (for automation)",
        long,
        global = true
    )]
    pub password_stdin: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(about = "Initialize an unlocked project and add the managed integration blocks")]
    Init,
    #[command(about = "Activate the advisory no-edit policy (initializes if needed)")]
    Lock(LockArgs),
    #[command(about = "Remove the advisory no-edit policy (asks for the password if one is set)")]
    Unlock(UnlockArgs),
    #[command(about = "Show the advisory policy, password, and integration status")]
    Status(JsonArgs),
    #[command(about = "Diagnose missing, stale, conflicting, or unreadable Clauck state")]
    Doctor(DoctorArgs),
    #[command(about = "Remove all Clauck-owned content from the project")]
    Uninstall(ForceArgs),
    #[command(about = "Authorize one controlled agent session to edit while locked")]
    Authorize(AuthorizeArgs),
    #[command(about = "Revoke session authorizations immediately")]
    Revoke(RevokeArgs),
    #[command(about = "Install, remove, or inspect a controlled-agent integration")]
    #[command(subcommand)]
    Integration(IntegrationCmd),
    #[command(
        about = "Hook entry point executed by an installed integration (reads JSON on stdin)"
    )]
    Hook(HookArgs),
}

#[derive(Debug, Args)]
pub struct LockArgs {
    #[arg(
        help = "Require a password to unlock (prompted on the terminal, never an argument)",
        long
    )]
    pub password: bool,
    #[arg(
        help = "Local display label stored with the password; not an identity check",
        long,
        value_name = "NAME",
        requires = "password"
    )]
    pub username: Option<String>,
    #[arg(help = "Replace hand-edited Clauck-managed content", long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct UnlockArgs {
    #[arg(
        help = "Also delete the password so future locks are not password-protected",
        long
    )]
    pub remove_password: bool,
    #[arg(help = "Replace a hand-edited .clauck/policy.md", long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct JsonArgs {
    #[arg(help = "Machine-readable output", long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    #[arg(help = "Machine-readable output", long)]
    pub json: bool,
    #[arg(help = "Do not run local agent binaries to read their versions", long)]
    pub skip_agent_versions: bool,
}

#[derive(Debug, Args)]
pub struct ForceArgs {
    #[arg(
        help = "Remove Clauck blocks and files even if they were edited by hand",
        long
    )]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct AuthorizeArgs {
    #[arg(
        help = "Session id reported by the integration's denial message",
        long,
        value_name = "ID"
    )]
    pub session: String,
    #[arg(
        help = "How long the authorization lasts (e.g. 15m, 1h; max 24h)",
        long,
        default_value = "30m"
    )]
    pub ttl: String,
    #[arg(help = "Integration the session belongs to", long, default_value = "claude-code", value_parser = ["claude-code"])]
    pub agent: String,
}

#[derive(Debug, Args)]
pub struct RevokeArgs {
    #[arg(
        help = "Revoke only this session (default: all sessions for the project)",
        long,
        value_name = "ID"
    )]
    pub session: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum IntegrationCmd {
    #[command(about = "Install the hook after showing exactly what will run")]
    Install(InstallArgs),
    #[command(about = "Remove the hook added by Clauck, keeping other settings")]
    Uninstall(AgentArg),
    #[command(about = "Show installed integrations")]
    Status,
}

#[derive(Debug, Args)]
pub struct InstallArgs {
    #[arg(value_parser = ["claude-code"])]
    pub agent: String,
    #[arg(help = "Consent without an interactive confirmation", long)]
    pub yes: bool,
    #[arg(
        help = "Add `onFailure: \"block\"` (Claude Code v2.1.295+) so hook errors and timeouts deny",
        long
    )]
    pub fail_closed_on_hook_failure: bool,
    #[arg(
        help = "While any project is locked, deny shell, MCP and unknown tools in every Claude Code session that holds no grant for it (not just in that project)",
        long
    )]
    pub strict: bool,
}

#[derive(Debug, Args)]
pub struct AgentArg {
    #[arg(value_parser = ["claude-code"])]
    pub agent: String,
}

#[derive(Debug, Args)]
pub struct HookArgs {
    #[arg(value_parser = ["claude-code"])]
    pub agent: String,
    #[arg(help = "Strict mode (see `integration install --strict`)", long)]
    pub strict: bool,
}

const FLAGS_AFTER_PASSWORD: [&str; 9] = [
    "--username",
    "--force",
    "--path",
    "--dry-run",
    "--non-interactive",
    "--password-stdin",
    "--help",
    "-h",
    "--",
];

fn password_argument_misuse(args: &[OsString]) -> bool {
    args.iter().enumerate().any(|(i, a)| {
        let s = a.to_string_lossy();
        (s.starts_with("--password") && s.contains('='))
            || (s == "--password"
                && args
                    .get(i + 1)
                    .map(|n| n.to_string_lossy())
                    .is_some_and(|n| !FLAGS_AFTER_PASSWORD.contains(&n.as_ref())))
    })
}

pub fn run(args: Vec<OsString>) -> i32 {
    if password_argument_misuse(&args) {
        let err = ClauckError::new(
            ErrorCode::InvalidInput,
            "--password takes no value; the password is requested on the terminal",
        )
        .with_hint(
            "The extra argument was not echoed. If it was your password, it may be in your \
             shell history; consider choosing a different one.",
        );
        eprintln!("clauck: {err}");
        return 2;
    }
    let cli = match Cli::try_parse_from(&args) {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return e.exit_code();
        }
    };
    match dispatch(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("clauck: {err}");
            err.code.exit_code()
        }
    }
}

fn dispatch(cli: Cli) -> Result<i32> {
    let g = &cli.global;
    match cli.command {
        Command::Init => commands::init::run(g).map(|_| 0),
        Command::Lock(a) => commands::lock::run(g, &a).map(|_| 0),
        Command::Unlock(a) => commands::unlock::run(g, &a).map(|_| 0),
        Command::Status(a) => commands::status::run(g, a.json).map(|_| 0),
        Command::Doctor(a) => commands::doctor::run(g, &a),
        Command::Uninstall(a) => commands::uninstall::run(g, a.force).map(|_| 0),
        Command::Authorize(a) => commands::authorize::run(g, &a).map(|_| 0),
        Command::Revoke(a) => commands::authorize::revoke(g, &a).map(|_| 0),
        Command::Integration(c) => commands::integration::run(g, &c).map(|_| 0),
        Command::Hook(a) => Ok(commands::hook::run(a.strict)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn detects_password_values_on_command_line() {
        assert!(password_argument_misuse(&os(&[
            "clauck",
            "lock",
            "--password",
            "hunter2"
        ])));
        assert!(password_argument_misuse(&os(&[
            "clauck",
            "lock",
            "--password=hunter2"
        ])));
        assert!(!password_argument_misuse(&os(&[
            "clauck",
            "lock",
            "--password"
        ])));
        assert!(!password_argument_misuse(&os(&[
            "clauck",
            "lock",
            "--password",
            "--username",
            "a"
        ])));
        assert!(password_argument_misuse(&os(&[
            "clauck",
            "unlock",
            "--password-stdin=x"
        ])));
        assert!(password_argument_misuse(&os(&[
            "clauck",
            "lock",
            "--password",
            "-secret"
        ])));
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
