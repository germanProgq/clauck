<p align="center">
  <img src="assets/clauck-banner.png" alt="Clauck: a padlock made of code brackets in front of a terminal window" width="100%">
</p>

<h1 align="center">Clauck</h1>

<p align="center">
  <strong>Pause AI edits to your project with one command. Resume with another.</strong>
</p>

<p align="center">
  <img alt="Rust 1.89+" src="https://img.shields.io/badge/rust-1.89%2B-orange?logo=rust">
  <img alt="Platforms" src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-blue">
  <img alt="License" src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-green">
  <img alt="No telemetry" src="https://img.shields.io/badge/telemetry-none-lightgrey">
</p>

<p align="center">
  <a href="#quickstart">Quickstart</a> •
  <a href="#commands">Commands</a> •
  <a href="#how-it-works">How it works</a> •
  <a href="#claude-code-enforcement-optional">Enforcement</a> •
  <a href="#limitations">Limitations</a> •
  <a href="#development">Development</a>
</p>

---

## Overview

Clauck is a small, cross-platform CLI that tells AI coding agents a project
is off-limits for changes, and lets you lift that again when you are ready.

```sh
clauck lock      # cooperating AI agents stop editing
clauck unlock    # back to normal
```

Your own work is never affected: you keep editing, building, testing, and
running the app exactly as before. Clauck never touches your source code,
dependencies, tests, or build files.

> [!IMPORTANT]
> A lock is **advisory**. It asks agents that read `AGENTS.md` / `CLAUDE.md`
> to stop; it cannot stop people, scripts, or agents that ignore those files.
> For Claude Code you can add [partial enforcement](#claude-code-enforcement-optional).
> Clauck does not try to detect AI-written code.

## Features

- **One-command lock and unlock** for any folder, including ones extracted
  from ZIP archives. Git is not required.
- **Works with the agents you already use**: Claude Code (`CLAUDE.md`
  import) and Codex and other `AGENTS.md` readers.
- **Optional password**: Argon2id, prompted on the terminal, stored outside
  the project. Never in the repo, the chat, or command-line arguments.
- **Safe to remove**: clearly marked blocks, conflict detection, and
  `clauck uninstall` keeps everything you wrote.
- **Crash-safe**: atomic writes, a recovery journal, and a per-project
  command lock.
- **Optional Claude Code hook** that denies write tools while locked, with
  time-limited per-session grants.
- **Offline**: no network access, no telemetry.

## Installation

Requires [Rust](https://rustup.rs) 1.89 or newer.

```sh
git clone <this repository> && cd clauck
cargo install --path .
```

This puts `clauck` in `~/.cargo/bin`. To build without installing, run
`cargo build --release` and use `target/release/clauck`.

## Quickstart

```sh
cd my-project
clauck lock                 # sets the project up on first use
clauck status
```

```text
Project:          /home/me/my-project
Advisory policy:  LOCKED (revision 2)
                  Cooperating agents are asked not to modify files. This is a request,
                  not enforcement: people and non-cooperating tools can still edit.
Password:         not configured
Integrations:
  agents-md         installed, verified [advisory]
  claude-md         installed, verified [advisory]
  claude-code-hook  not installed [controlled-agent]
```

Ask an agent to change something and it will reply:

> AI edits are restricted for this project. Authorization is required.

When you are done, run `clauck unlock`. To see the whole flow on a throwaway
copy of the sample app, run `scripts/demo.sh`.

### Password protection

```sh
clauck lock --password      # prompts twice; required again to unlock
clauck unlock               # prompts for the password
clauck unlock --remove-password
```

Without a terminal (for example, when an agent runs the command), `unlock`
fails with `E_AUTH_REQUIRED` instead of waiting. A plain `clauck lock` never
removes an existing password.

## Commands

| Command | Description |
|---|---|
| `clauck init` | Set up an unlocked project. |
| `clauck lock [--password] [--username NAME]` | Activate the policy. `--username` is a local label, not an identity check. |
| `clauck unlock [--remove-password]` | Lift the policy. Asks for the password if one is set. |
| `clauck status [--json]` | Policy, password, and integrations, reported separately. |
| `clauck doctor [--json]` | Read-only checks for missing, stale, edited, or conflicting state. |
| `clauck uninstall [--force]` | Remove everything Clauck added, keeping your own edits. |
| `clauck integration install claude-code [--strict]` | Install the Claude Code hook after showing exactly what will run. |
| `clauck authorize --session ID --ttl 30m` | Let one Claude Code session edit a locked project for a while. |
| `clauck revoke [--session ID]` | End session grants immediately. |

Global flags: `--path DIR`, `--dry-run` (print the planned changes and write
nothing), `--non-interactive`, and `--password-stdin` (read passwords from
standard input, one per line, for scripts).

## How it works

```text
my-project/
├── AGENTS.md        ← small marked block: "read .clauck/policy.md first"
├── CLAUDE.md        ← small marked block: @.clauck/policy.md
└── .clauck/
    ├── policy.md    ← the current policy in plain language
    ├── state.json   ← public metadata (no secrets)
    └── manifest.json← what Clauck manages, with hashes for safe removal
```

- Clauck only ever writes these paths. Existing `AGENTS.md` and `CLAUDE.md`
  content is kept, and line endings are preserved.
- `lock` and `unlock` rewrite `policy.md`; the entry-point blocks stay the same.
- Agents load the policy automatically: Claude Code imports it from
  `CLAUDE.md`, and Codex reads `AGENTS.md`. They are asked not to announce it,
  only to mention it when it blocks a request or when you ask.
- Passwords, verifiers, and the local lock record live in your per-user
  application-data folder, never in the project. Set `CLAUCK_HOME` to choose
  another location.

## Claude Code enforcement (optional)

```sh
clauck integration install claude-code
```

This adds a `PreToolUse` hook to your Claude Code **user** settings, never to
project files. While a project is locked, it denies tools that can write
(Bash, PowerShell, Edit, Write, NotebookEdit, MCP and unknown tools); read
tools keep working. It takes the lock state from your local store, so editing
`.clauck/state.json` cannot unlock it.

- `--strict` also denies shell, MCP, and unknown tools in *every* Claude Code
  session while any project is locked. This catches commands that reach a
  locked project from another folder.
- `clauck authorize --session ID --ttl 30m` grants one session temporary
  access. Every new `clauck lock` revokes all grants.

## Limitations

| | Advisory files | + Claude Code hook |
|---|---|---|
| Agent that follows `AGENTS.md` / `CLAUDE.md` | Usually stops | Write tools denied |
| Agent that ignores the files, or a different tool | Not stopped | Stopped only if it is Claude Code |
| "I already unlocked it" typed in the chat | Up to the model | Denied: only the local store counts |
| A person, or software running as your user | Not stopped | Not stopped |

Things to know about the hook:

- **Hook failures let the call through on Claude Code before 2.1.295.** If
  the hook crashes or times out, the tool call proceeds. Use
  `--fail-closed-on-hook-failure` on 2.1.295 or later.
- **All shell use is blocked while locked.** On macOS this includes Claude
  Code's file search; the Read tool still works.
- **Commands from other folders are matched by path text only.** Relative
  paths and variables are not caught unless you install with `--strict`.
- **Processes started before the lock** (dev servers, watchers, MCP servers)
  keep running.

A copied project with no password record on the new machine reports *local
authorization unavailable*. Clauck never resets or invents credentials.

## Compatibility

| Agent | Mechanism | Verified |
|---|---|---|
| Claude Code 2.1.278 | `CLAUDE.md` import + optional `PreToolUse` hook | Against the docs, and in simulated hook tests |
| Codex CLI 0.154.0 | `AGENTS.md` | Live runs, 6 of 6 as expected |
| Other `AGENTS.md` readers | `AGENTS.md` | Not verified |

The live Codex runs covered a locked edit, an unlocked control, a question
only, "I already unlocked it", a password lock (Codex ran `clauck unlock`
itself and was refused), and creating a new file. Six runs on one model is a
small sample, not a guarantee.

## Exit codes

`0` ok · `2` usage · `3` not initialized · `4` auth required · `5` wrong
password · `6` local authorization unavailable · `7` conflict · `8` busy ·
`9` unsafe path · `10` I/O · `11` read-only · `12` invalid metadata ·
`13` invalid input · `14` integration missing · `15` consent required ·
`16` doctor found problems · `17` not locked

## Development

```sh
cargo test                                   # 130 unit + integration tests
cargo clippy --all-targets -- -D warnings
scripts/demo.sh                              # scripted walkthrough
CLAUCK_REAL_AGENT_TESTS=1 scripts/real-agent-check.sh claude codex   # live, opt-in, uses your account
```

## License

Dual-licensed under MIT or Apache-2.0, as declared in `Cargo.toml`.
