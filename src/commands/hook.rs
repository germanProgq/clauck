use std::io::{Read, Write};
use std::panic;

use crate::hook::{self, Decision, Options};
use crate::project::STATE_FILE;
use crate::store::Store;
use crate::timeutil::now_unix;

const MAX_INPUT_BYTES: u64 = 32 * 1024 * 1024;

const BLOCK: i32 = 2;

pub fn run(strict: bool) -> i32 {
    let mut raw = Vec::new();
    let read_ok = std::io::stdin()
        .take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut raw)
        .is_ok();
    panic::set_hook(Box::new(|_| {}));
    let outcome = panic::catch_unwind(|| evaluate(read_ok, &raw, Options { strict }));
    match outcome {
        Ok(Some(Decision::Deny(reason))) => {
            let mut out = std::io::stdout().lock();
            match writeln!(out, "{}", hook::deny_output(&reason)).and_then(|_| out.flush()) {
                Ok(()) => 0,
                Err(_) => {
                    eprintln!("{reason}");
                    BLOCK
                }
            }
        }
        Ok(Some(Decision::Defer)) => 0,
        Ok(None) => {
            eprintln!("Clauck could not read the hook input; denying because a Clauck project may be locked.");
            BLOCK
        }
        Err(_) => {
            eprintln!("Clauck hook failed internally; denying this tool call to stay safe.");
            BLOCK
        }
    }
}

fn evaluate(read_ok: bool, raw: &[u8], opts: Options) -> Option<Decision> {
    let parsed = (read_ok && raw.len() as u64 <= MAX_INPUT_BYTES)
        .then(|| hook::parse_input(raw))
        .flatten();
    match parsed {
        Some(input) => Some(hook::decide(
            &input,
            Store::locate().ok().as_ref(),
            now_unix(),
            opts,
        )),
        None if clauck_project_nearby() => None,
        None => Some(Decision::Defer),
    }
}

fn clauck_project_nearby() -> bool {
    let Ok(cwd) = std::env::current_dir() else {
        return true;
    };
    cwd.ancestors().any(|d| d.join(STATE_FILE).exists())
}
