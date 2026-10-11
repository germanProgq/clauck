use std::io::{BufRead, IsTerminal, Read, Write};

use crate::adapters::claude_code::{self, AGENT_ID};
use crate::cli::{GlobalOpts, InstallArgs, IntegrationCmd};
use crate::commands::{status, store};
use crate::error::{ClauckError, ErrorCode, Result};
use crate::project::display_path;
use crate::store::integrations::{self, IntegrationRecord};
use crate::store::Store;
use crate::timeutil::now_unix;

pub fn run(g: &GlobalOpts, cmd: &IntegrationCmd) -> Result<()> {
    match cmd {
        IntegrationCmd::Install(a) => install(g, a),
        IntegrationCmd::Uninstall(_) => uninstall(g),
        IntegrationCmd::Status => show(),
    }
}

fn require_store() -> Result<Store> {
    store().ok_or_else(|| {
        ClauckError::new(
            ErrorCode::IntegrationMissing,
            "no local Clauck store is available",
        )
    })
}

fn install(g: &GlobalOpts, args: &InstallArgs) -> Result<()> {
    let store = require_store()?;
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| {
            ClauckError::new(
                ErrorCode::Io,
                format!("cannot resolve the clauck executable: {e}"),
            )
        })?;
    let settings = claude_code::settings_path()?;
    let command = claude_code::hook_command(&exe, args.strict);
    let group = claude_code::hook_group(&command, args.fail_closed_on_hook_failure);

    println!("This adds a Claude Code PreToolUse hook to:");
    println!("  {}", display_path(&settings));
    println!(
        "It will execute, before every Claude Code tool call in every project on this account:"
    );
    println!("  {command}");
    println!("Settings entry:");
    println!("{}", serde_json::to_string_pretty(&group).expect("json"));
    println!("While a project is locked it denies Bash, PowerShell, Edit, Write, NotebookEdit, MCP and unknown");
    println!("tools for sessions without a grant. It never approves anything the host would otherwise ask about.");
    if args.strict {
        println!(
            "Strict mode: while ANY project is locked, shell, MCP and unknown tools are denied in"
        );
        println!("every Claude Code session without a grant, including sessions in other folders.");
    } else {
        println!("Shell commands in other folders are checked only for the locked project's path (not a sandbox).");
    }
    println!("It does not cover other agents, processes started outside Claude Code, or people.");
    if !args.fail_closed_on_hook_failure {
        println!("Claude Code lets a tool call proceed if a hook crashes or times out; see --fail-closed-on-hook-failure.");
    }
    if exe.to_string_lossy().contains("/target/debug/") {
        println!("warning: this is a development build path; install clauck permanently first.");
    }
    if g.dry_run {
        println!("Dry run: settings not changed.");
        return Ok(());
    }
    if !args.yes && !confirm(g.non_interactive)? {
        println!("Not installed.");
        return Ok(());
    }
    claude_code::install(&settings, &command, args.fail_closed_on_hook_failure)?;
    integrations::save(
        &store,
        &IntegrationRecord::new(
            AGENT_ID,
            display_path(&settings),
            command,
            args.fail_closed_on_hook_failure,
            now_unix(),
        ),
    )?;
    println!("Installed. Claude Code picks up settings changes automatically; restart running sessions to be sure.");
    Ok(())
}

fn confirm(non_interactive: bool) -> Result<bool> {
    if non_interactive || !std::io::stdin().is_terminal() {
        return Err(ClauckError::new(
            ErrorCode::ConsentRequired,
            "installing a hook needs explicit consent",
        )
        .with_hint("Review the output above, then rerun in a terminal or pass --yes."));
    }
    print!("Install this hook? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .take(256)
        .read_line(&mut line)
        .map_err(|e| ClauckError::new(ErrorCode::Io, e.to_string()))?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "Yes"))
}

fn uninstall(g: &GlobalOpts) -> Result<()> {
    let settings = claude_code::settings_path()?;
    if g.dry_run {
        println!(
            "Would remove Clauck's hook from {}.",
            display_path(&settings)
        );
        return Ok(());
    }
    let removed = claude_code::uninstall(&settings)?;
    if let Some(s) = store() {
        integrations::remove(&s, AGENT_ID)?;
    }
    println!(
        "Removed {removed} Clauck hook handler(s) from {}; other settings were kept.",
        display_path(&settings)
    );
    Ok(())
}

fn show() -> Result<()> {
    let project = crate::project::Project::open(None).ok();
    let store = store();
    let info = match project {
        Some(p) => status::collect(&p, store.as_ref())?.integrations,
        None => Vec::new(),
    };
    for i in info.iter().filter(|i| i.kind == "controlled_agent") {
        println!(
            "{}: {} - {}",
            i.id,
            if i.installed && i.verified {
                "installed, verified (partial enforcement)"
            } else if i.installed {
                "installed, NOT verified"
            } else {
                "not installed"
            },
            i.detail
        );
    }
    Ok(())
}
