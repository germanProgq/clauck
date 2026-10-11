use crate::adapters::claude_code;
use crate::auth::{self, PasswordSource};
use crate::cli::{AuthorizeArgs, GlobalOpts, RevokeArgs};
use crate::commands::{load_effective, record_local, store};
use crate::error::{ClauckError, ErrorCode, Result};
use crate::project::Project;
use crate::state;
use crate::store::grants::{self, NewGrant};
use crate::store::{integrations, Store};
use crate::timeutil::{format_utc, now_unix, parse_ttl};

fn require_store() -> Result<Store> {
    store().ok_or_else(|| {
        ClauckError::new(
            ErrorCode::AuthUnavailable,
            "no local Clauck store is available",
        )
    })
}

fn require_integration(store: &Store) -> Result<()> {
    let recorded = integrations::load(store, claude_code::AGENT_ID)?.is_some();
    let present = claude_code::settings_path()
        .and_then(|p| claude_code::installed_commands(&p))
        .map(|c| !c.is_empty())
        .unwrap_or(false);
    if recorded && present {
        return Ok(());
    }
    Err(ClauckError::new(
        ErrorCode::IntegrationMissing,
        "no controlled-agent integration is installed, so a session grant would have no effect",
    )
    .with_hint(
        "Install one with `clauck integration install claude-code`, or use `clauck unlock`.",
    ))
}

fn validate_session_id(id: &str) -> Result<()> {
    let ok = !id.is_empty()
        && id.len() <= 200
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.:".contains(c));
    if ok {
        Ok(())
    } else {
        Err(ClauckError::new(
            ErrorCode::InvalidInput,
            "session id must be 1-200 characters of letters, digits, - _ . :",
        ))
    }
}

pub fn run(g: &GlobalOpts, args: &AuthorizeArgs) -> Result<()> {
    validate_session_id(&args.session)?;
    let ttl = parse_ttl(&args.ttl)?;
    let project = Project::open(g.path.as_deref())?;
    let store = require_store()?;
    require_integration(&store)?;
    state::load_required(&project)?;
    let loaded = load_effective(&project, Some(&store))?;
    let (st, record) = (loaded.state, loaded.record);
    if !st.locked {
        return Err(ClauckError::new(
            ErrorCode::NotLocked,
            "the project is not locked; no authorization is needed",
        ));
    }
    let req = auth::requirement(Some(&store), Some(&st), record.as_ref())?;
    if g.dry_run {
        println!("Would authorize session {} for {}.", args.session, args.ttl);
        return Ok(());
    }
    let mut source = PasswordSource::detect(g.password_stdin, g.non_interactive)?;
    auth::authenticate(&req, &mut source)?;
    let now = now_unix();
    let record = match record {
        Some(r) => r,
        None => record_local(&store, &project, &st.with_lock(true), true, now)?,
    };
    let grant = grants::create(
        &store,
        &project,
        &NewGrant {
            project_id: &record.project_id,
            agent: &args.agent,
            session_id: &args.session,
            lock_generation: record.lock_generation,
            now,
            ttl_secs: ttl,
        },
    )?;
    println!(
        "Authorized {} session {} for {} until {}.",
        args.agent,
        args.session,
        project.display_root(),
        format_utc(grant.expires_unix)
    );
    println!("The project stays locked for every other session. `clauck revoke` ends this early; `clauck lock` ends it too.");
    Ok(())
}

pub fn revoke(g: &GlobalOpts, args: &RevokeArgs) -> Result<()> {
    let project = Project::open(g.path.as_deref())?;
    let store = require_store()?;
    if g.dry_run {
        println!(
            "Would revoke session authorizations for {}.",
            project.display_root()
        );
        return Ok(());
    }
    let n = match &args.session {
        Some(s) => usize::from(grants::revoke_session(&store, &project, s)?),
        None => grants::revoke_all(&store, &project)?,
    };
    println!(
        "Revoked {n} session authorization(s) for {}.",
        project.display_root()
    );
    Ok(())
}
