mod claude;
mod keychain;
mod lock;

use claude::Profile;
use keychain::{Keychain, Store};
use lock::Lock;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::Duration;

const USAGE: &str = "\
ccs: switch Claude Code accounts

Usage:
  ccs                     list accounts, * marks the active one
  ccs add <name> [args]   log in and save the account as <name>
  ccs switch <name>       make <name> the active account
  ccs <name>              same as switch
  ccs rm <name>           forget <name> (the account stays logged in)

Extra args after `add <name>` go to `claude auth login`, for example --email.";

// Claude Code's credential locks go stale after 60 s, its config lock after 10 s.
const CREDENTIAL_STALE: Duration = Duration::from_secs(60);
const CONFIG_STALE: Duration = Duration::from_secs(10);
const LOCK_TIMEOUT: Duration = Duration::from_secs(9);

/// Everything a command touches, so tests can point it at a temp dir and an
/// in-memory store.
struct Ctx<'a> {
    /// Home of the default profile: `~/.claude` and `~/.claude.json` live here.
    home: PathBuf,
    /// One profile directory per account.
    ccs_home: PathBuf,
    /// Store for calls made while we hold Claude Code's locks.
    store: &'a dyn Store,
    /// Store for calls made before we take them, which may wait for an unlock.
    patient: &'a dyn Store,
}

fn main() -> ExitCode {
    let short = Keychain {
        timeout: keychain::SHORT,
    };
    let long = Keychain {
        timeout: keychain::LONG,
    };
    let ctx = Ctx {
        home: claude::home_dir(),
        ccs_home: ccs_home(),
        store: &short,
        patient: &long,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        [] | ["list"] | ["ls"] => list(&ctx),
        ["-h"] | ["--help"] | ["help"] => Ok(USAGE.to_string()),
        ["-V"] | ["--version"] => Ok(format!("ccs {}", env!("CARGO_PKG_VERSION"))),
        ["add", name, ..] => add(&ctx, name, &args[2..]),
        ["switch", name] => switch(&ctx, name),
        ["rm", name] => remove(&ctx, name),
        [name] if !name.starts_with('-') => switch(&ctx, name),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(msg) => {
            if !msg.is_empty() {
                println!("{msg}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("ccs: {e}");
            ExitCode::FAILURE
        }
    }
}

fn ccs_home() -> PathBuf {
    // The Keychain item is named after this path, so it must not depend on cwd.
    match std::env::var_os("CCS_HOME").map(PathBuf::from) {
        Some(p) if p.is_absolute() => p,
        Some(p) => std::env::current_dir().map(|d| d.join(&p)).unwrap_or(p),
        None => claude::home_dir().join(".ccs"),
    }
}

fn check_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok && !["add", "switch", "rm", "list", "ls", "help"].contains(&name) {
        Ok(())
    } else {
        Err(format!(
            "bad account name {name:?}: use letters, digits, - or _"
        ))
    }
}

/// Saved accounts, sorted by name.
fn accounts(ctx: &Ctx) -> Vec<(String, Profile)> {
    let mut out: Vec<_> = std::fs::read_dir(&ctx.ccs_home)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            check_name(&name).ok()?;
            Some((name, Profile::config_dir(&e.path())))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn find(ctx: &Ctx, name: &str) -> Result<Profile, String> {
    check_name(name)?;
    let dir = ctx.ccs_home.join(name);
    if !dir.is_dir() {
        return Err(format!("no account {name:?}, add it with `ccs add {name}`"));
    }
    Ok(Profile::config_dir(&dir))
}

fn list(ctx: &Ctx) -> Result<String, String> {
    let accounts = accounts(ctx);
    if accounts.is_empty() {
        return Ok("no accounts yet, add one with `ccs add <name>`".into());
    }
    let active = Profile::default_profile(&ctx.home).account();
    let active_id = active.as_ref().and_then(claude::identity);
    let width = accounts.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    let lines: Vec<String> = accounts
        .iter()
        .map(|(name, profile)| {
            let account = profile.account();
            let mark = match account.as_ref().and_then(claude::identity) {
                Some(id) if Some(id) == active_id => '*',
                _ => ' ',
            };
            let email = account.as_ref().map_or("(not logged in)", claude::email);
            format!("{mark} {name:width$}  {email}")
        })
        .collect();
    Ok(lines.join("\n"))
}

fn add(ctx: &Ctx, name: &str, login_args: &[String]) -> Result<String, String> {
    check_name(name)?;
    let dir = ctx.ccs_home.join(name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let status = Command::new("claude")
        .args(["auth", "login"])
        .args(login_args)
        .env("CLAUDE_CONFIG_DIR", &dir)
        .env_remove("CLAUDE_SECURESTORAGE_CONFIG_DIR")
        .status()
        .map_err(|e| format!("run `claude auth login`: {e}"))?;
    if !status.success() {
        return Err("`claude auth login` did not finish".into());
    }
    let profile = Profile::config_dir(&dir);
    let account = profile
        .account()
        .ok_or("login finished but no account was saved")?;
    if ctx.patient.read(&profile.service)?.is_none() {
        return Err(format!("login finished but {} is empty", profile.service));
    }
    let id = claude::identity(&account);
    for (other, p) in accounts(ctx) {
        if other != name && p.account().as_ref().and_then(claude::identity) == id {
            eprintln!("note: {other:?} is the same account");
        }
    }
    Ok(format!("added {name} ({})", claude::email(&account)))
}

fn switch(ctx: &Ctx, name: &str) -> Result<String, String> {
    let target = find(ctx, name)?;
    let account = target
        .account()
        .ok_or(format!("{name:?} is not logged in, run `ccs add {name}`"))?;
    let default = Profile::default_profile(&ctx.home);

    // A locked Keychain shows an unlock dialog on the first read. Let that
    // happen now, before we hold locks that Claude Code waits on.
    ctx.patient.read(&default.service)?;

    // Same order as Claude Code's token refresh: new lock, legacy lock, config lock.
    let lock_path = |p: &Profile, suffix: &str| {
        let mut s = p.home.clone().into_os_string();
        s.push(suffix);
        PathBuf::from(s)
    };
    let _locks = [
        Lock::acquire(
            &default.home.join(".oauth_refresh.lock"),
            CREDENTIAL_STALE,
            LOCK_TIMEOUT,
        )?,
        Lock::acquire(
            &lock_path(&default, ".lock"),
            CREDENTIAL_STALE,
            LOCK_TIMEOUT,
        )?,
        Lock::acquire(
            &lock_path(&default, ".json.lock"),
            CONFIG_STALE,
            LOCK_TIMEOUT,
        )?,
    ];

    // Read the active state only under the locks, so a concurrent switch or
    // refresh cannot pair one account's identity with another's token.
    let active = default.account();
    let active_id = active.as_ref().and_then(claude::identity);
    // A switch that failed and could not roll back leaves this marker. The
    // default token may then belong to another account than ~/.claude.json
    // names, so it is neither saved back nor trusted as "already on".
    let marker = ctx.ccs_home.join(".switching");
    let unsure = marker.exists();
    if unsure {
        eprintln!("repairing an unfinished switch");
    } else if active_id.is_some() && active_id == claude::identity(&account) {
        return Ok(format!("already on {name} ({})", claude::email(&account)));
    }
    let target_credential = ctx
        .store
        .read(&target.service)?
        .ok_or(format!("{name:?} has no saved token, run `ccs add {name}`"))?;
    let current = ctx.store.read(&default.service)?;

    // The active session rotates its refresh token, which voids the copy we
    // saved. Store the rotated token back before we overwrite it. An item with
    // only MCP logins holds no Claude login to lose.
    if let Some(current) = current
        .as_deref()
        .filter(|c| !unsure && claude::has_oauth(c))
    {
        let owners: Vec<_> = accounts(ctx)
            .into_iter()
            .filter(|(_, p)| {
                active_id.is_some() && p.account().as_ref().and_then(claude::identity) == active_id
            })
            .collect();
        if owners.is_empty() {
            let email = active.as_ref().map_or("an unknown account", claude::email);
            return Err(format!(
                "the active login ({email}) is not saved in ccs, \
                 switching would log it out; add it first with `ccs add <name>`"
            ));
        }
        for (other, p) in owners {
            let saved = ctx.store.read(&p.service)?;
            if saved.as_deref().map_or(0, claude::expires_at) < claude::expires_at(current) {
                ctx.store.write(
                    &p.service,
                    &claude::splice_oauth(saved.as_deref(), current)?,
                )?;
                eprintln!("saved the latest token of {other}");
            }
        }
    }

    let body = std::fs::read_to_string(&default.config)
        .map_err(|e| format!("read {}: {e}", default.config.display()))?;
    let new_body = claude::splice_account(&body, &account)?;
    let new_credential = claude::splice_oauth(current.as_deref(), &target_credential)?;

    std::fs::write(&marker, format!("to {name}\n"))
        .map_err(|e| format!("write {}: {e}", marker.display()))?;

    // Token and identity must name the same account. If either write fails,
    // put the old token back; the config is replaced last and atomically.
    let applied = ctx
        .store
        .write(&default.service, &new_credential)
        .and_then(|()| claude::write_atomic(&default.config, &new_body));
    if let Err(e) = applied {
        let restored = match current.as_deref() {
            Some(c) => ctx.store.write(&default.service, c),
            None => ctx.store.delete(&default.service),
        };
        return Err(match restored {
            Ok(()) => {
                // Back to the state before this switch, which was sure unless
                // the marker was already there.
                if !unsure {
                    let _ = std::fs::remove_file(&marker);
                }
                format!("{e}; the previous login is restored")
            }
            Err(r) => format!(
                "{e}; restoring the previous login also failed: {r}; \
                 run `ccs <name>` again to repair"
            ),
        });
    }
    std::fs::remove_file(&marker).map_err(|e| {
        format!(
            "switched to {name}, but could not remove {}: {e}",
            marker.display()
        )
    })?;

    Ok(format!(
        "switched to {name} ({})\nrestart running Claude Code sessions to use it",
        claude::email(&account)
    ))
}

fn remove(ctx: &Ctx, name: &str) -> Result<String, String> {
    let profile = find(ctx, name)?;
    // Delete the directory first: a directory without a token would still
    // list as an account, a leftover Keychain item is invisible.
    std::fs::remove_dir_all(&profile.home)
        .map_err(|e| format!("remove {}: {e}", profile.home.display()))?;
    ctx.patient.delete(&profile.service)?;
    Ok(format!("removed {name}"))
}

#[cfg(test)]
mod tests;
