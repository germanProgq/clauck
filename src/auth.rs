use std::collections::VecDeque;
use std::io::{IsTerminal, Read};

use zeroize::Zeroizing;

use crate::error::{ClauckError, ErrorCode, Result};
use crate::state::ProjectState;
use crate::store::credential::{self, CredentialRecord};
use crate::store::ledger::RootRecord;
use crate::store::Store;

pub type Secret = Zeroizing<String>;

const MAX_STDIN_BYTES: u64 = 16 * 1024;

pub enum PasswordSource {
    Terminal,
    Stdin(VecDeque<Secret>),
    Unavailable,
}

impl PasswordSource {
    pub fn detect(password_stdin: bool, non_interactive: bool) -> Result<Self> {
        if password_stdin {
            return read_stdin_lines().map(PasswordSource::Stdin);
        }
        if non_interactive || !std::io::stdin().is_terminal() {
            return Ok(PasswordSource::Unavailable);
        }
        Ok(PasswordSource::Terminal)
    }

    pub fn ask(&mut self, prompt: &str) -> Result<Secret> {
        match self {
            PasswordSource::Terminal => rpassword::prompt_password(prompt)
                .map(Zeroizing::new)
                .map_err(|e| {
                    ClauckError::new(
                        ErrorCode::AuthRequired,
                        format!("cannot read password: {e}"),
                    )
                }),
            PasswordSource::Stdin(lines) => lines.pop_front().ok_or_else(|| {
                ClauckError::new(
                    ErrorCode::InvalidInput,
                    "not enough lines on standard input for the requested passwords",
                )
            }),
            PasswordSource::Unavailable => Err(auth_required()),
        }
    }
}

fn read_stdin_lines() -> Result<VecDeque<Secret>> {
    let mut raw = Zeroizing::new(Vec::with_capacity(MAX_STDIN_BYTES as usize + 1));
    std::io::stdin()
        .take(MAX_STDIN_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|e| {
            ClauckError::new(ErrorCode::InvalidInput, format!("cannot read stdin: {e}"))
        })?;
    if raw.len() as u64 > MAX_STDIN_BYTES {
        return Err(ClauckError::new(
            ErrorCode::InvalidInput,
            "standard input is too large",
        ));
    }
    let text = std::str::from_utf8(&raw)
        .map_err(|_| ClauckError::new(ErrorCode::InvalidInput, "standard input is not UTF-8"))?;
    Ok(text
        .split('\n')
        .map(|l| Zeroizing::new(l.strip_suffix('\r').unwrap_or(l).to_string()))
        .collect())
}

pub fn auth_required() -> ClauckError {
    ClauckError::new(
        ErrorCode::AuthRequired,
        "authentication required, but no interactive terminal is available",
    )
    .with_hint(
        "Run this command yourself in a terminal. For scripted use, pass \
         --password-stdin and supply the password on standard input.",
    )
}

#[derive(Debug)]
pub enum Requirement {
    None,
    Password(CredentialRecord),
    Unavailable,
}

pub fn governing_id<'a>(
    state: Option<&'a ProjectState>,
    record: Option<&'a RootRecord>,
) -> Option<&'a str> {
    record
        .map(|r| r.project_id.as_str())
        .or_else(|| state.map(|s| s.project_id.as_str()))
}

pub fn requirement(
    store: Option<&Store>,
    state: Option<&ProjectState>,
    record: Option<&RootRecord>,
) -> Result<Requirement> {
    if let (Some(store), Some(id)) = (store, governing_id(state, record)) {
        if let Some(cred) = credential::load(store, id)? {
            return Ok(Requirement::Password(cred));
        }
    }
    let claimed = state.is_some_and(|s| s.password_configured)
        || record.is_some_and(|r| r.password_configured);
    Ok(if claimed {
        Requirement::Unavailable
    } else {
        Requirement::None
    })
}

pub fn unavailable_error() -> ClauckError {
    ClauckError::new(
        ErrorCode::AuthUnavailable,
        "local authorization unavailable: this project is password-protected, \
         but no password record for it exists on this machine",
    )
    .with_hint(
        "This usually means the project was copied or extracted from an archive. \
         Clauck does not reset or invent credentials. The lock is advisory: the \
         owner can remove the clauck blocks and the .clauck folder by hand.",
    )
}

pub fn authenticate(req: &Requirement, source: &mut PasswordSource) -> Result<()> {
    match req {
        Requirement::None => Ok(()),
        Requirement::Unavailable => Err(unavailable_error()),
        Requirement::Password(cred) => {
            let pw = source.ask("Clauck password: ")?;
            if cred.verify(&pw) {
                Ok(())
            } else {
                Err(ClauckError::new(
                    ErrorCode::AuthFailed,
                    "incorrect password; nothing was changed",
                ))
            }
        }
    }
}

pub fn ask_new_password(source: &mut PasswordSource) -> Result<Secret> {
    let first = source.ask("New Clauck password: ")?;
    if first.is_empty() {
        return Err(ClauckError::new(
            ErrorCode::InvalidInput,
            "password must not be empty; nothing was changed",
        ));
    }
    let second = source.ask("Confirm new Clauck password: ")?;
    if *first != *second {
        return Err(ClauckError::new(
            ErrorCode::InvalidInput,
            "passwords do not match; nothing was changed",
        ));
    }
    Ok(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0123456789abcdef0123456789abcdef";

    fn lines(items: &[&str]) -> PasswordSource {
        PasswordSource::Stdin(
            items
                .iter()
                .map(|s| Zeroizing::new(s.to_string()))
                .collect(),
        )
    }

    #[test]
    fn new_password_validation() {
        assert_eq!(
            ask_new_password(&mut lines(&["", ""])).unwrap_err().code,
            ErrorCode::InvalidInput
        );
        assert_eq!(
            ask_new_password(&mut lines(&["a", "b"])).unwrap_err().code,
            ErrorCode::InvalidInput
        );
        assert_eq!(
            ask_new_password(&mut lines(&["a"])).unwrap_err().code,
            ErrorCode::InvalidInput
        );
        assert_eq!(
            &*ask_new_password(&mut lines(&["ok pw", "ok pw"])).unwrap(),
            "ok pw"
        );
    }

    #[test]
    fn unavailable_source_fails_without_prompting() {
        let cred = CredentialRecord::create(ID, "pw", None, 0).unwrap();
        let err = authenticate(
            &Requirement::Password(cred),
            &mut PasswordSource::Unavailable,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::AuthRequired);
    }

    #[test]
    fn wrong_password_fails() {
        let cred = CredentialRecord::create(ID, "pw", None, 0).unwrap();
        let req = Requirement::Password(cred);
        assert_eq!(
            authenticate(&req, &mut lines(&["nope"])).unwrap_err().code,
            ErrorCode::AuthFailed
        );
        assert!(authenticate(&req, &mut lines(&["pw"])).is_ok());
    }

    #[test]
    fn metadata_flag_alone_is_not_authority() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store {
            root: dir.path().to_path_buf(),
        };
        credential::save(
            &store,
            &CredentialRecord::create(ID, "pw", None, 0).unwrap(),
        )
        .unwrap();
        let tampered = ProjectState::new(ID.into()).with_password(false);
        assert!(matches!(
            requirement(Some(&store), Some(&tampered), None).unwrap(),
            Requirement::Password(_)
        ));
        let other = ProjectState::new("f".repeat(32)).with_password(true);
        assert!(matches!(
            requirement(Some(&store), Some(&other), None).unwrap(),
            Requirement::Unavailable
        ));
    }

    #[test]
    fn local_record_id_wins_over_edited_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store {
            root: dir.path().to_path_buf(),
        };
        let other = "f".repeat(32);
        credential::save(
            &store,
            &CredentialRecord::create(ID, "owner", None, 0).unwrap(),
        )
        .unwrap();
        credential::save(
            &store,
            &CredentialRecord::create(&other, "attacker", None, 0).unwrap(),
        )
        .unwrap();
        let swapped = ProjectState::new(other.clone());
        let proj_dir = tempfile::tempdir().unwrap();
        let project = crate::project::Project::open(Some(proj_dir.path())).unwrap();
        let record = RootRecord::new(&project, ID);
        let Requirement::Password(cred) =
            requirement(Some(&store), Some(&swapped), Some(&record)).unwrap()
        else {
            panic!("expected password requirement");
        };
        assert!(cred.verify("owner"));
        assert!(!cred.verify("attacker"));
    }
}
