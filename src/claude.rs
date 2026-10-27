//! Where Claude Code keeps an account: a Keychain item for the OAuth token and
//! the `oauthAccount` key of `.claude.json` for the identity.

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

pub const DEFAULT_SERVICE: &str = "Claude Code-credentials";

/// One Claude Code profile: the default (`~/.claude`) or a `CLAUDE_CONFIG_DIR`.
pub struct Profile {
    /// Config home, used for the lock paths.
    pub home: PathBuf,
    /// The `.claude.json` that holds `oauthAccount`.
    pub config: PathBuf,
    /// Keychain service of the OAuth credential.
    pub service: String,
}

impl Profile {
    pub fn default_profile() -> Profile {
        let home = home_dir();
        Profile {
            home: home.join(".claude"),
            config: home.join(".claude.json"),
            service: DEFAULT_SERVICE.to_string(),
        }
    }

    /// The profile Claude Code uses when `CLAUDE_CONFIG_DIR=dir`.
    pub fn config_dir(dir: &Path) -> Profile {
        Profile {
            home: dir.to_path_buf(),
            config: dir.join(".claude.json"),
            service: service_for(&dir.to_string_lossy()),
        }
    }

    pub fn account(&self) -> Option<Map<String, Value>> {
        read_json(&self.config)?
            .get("oauthAccount")?
            .as_object()
            .cloned()
    }
}

/// Claude Code names the item after the first 8 hex digits of SHA-256 of the
/// raw `CLAUDE_CONFIG_DIR` value, NFC-normalized and not resolved.
pub fn service_for(config_dir: &str) -> String {
    let normalized: String = config_dir.nfc().collect();
    let digest = Sha256::digest(normalized.as_bytes());
    let hex: String = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
    format!("{DEFAULT_SERVICE}-{hex}")
}

pub fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME is not set"))
}

pub fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Replace `oauthAccount` in a `.claude.json` body and keep every other key.
pub fn splice_account(body: &str, account: &Map<String, Value>) -> Result<String, String> {
    let mut config: Value =
        serde_json::from_str(body).map_err(|e| format!("parse .claude.json: {e}"))?;
    let obj = config
        .as_object_mut()
        .ok_or(".claude.json is not an object")?;
    obj.insert("oauthAccount".into(), Value::Object(account.clone()));
    serde_json::to_string_pretty(&config).map_err(|e| e.to_string())
}

/// Write through a temp file and rename, so Claude Code never reads half a file.
pub fn write_atomic(path: &Path, body: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension(format!("json.ccs-{}", std::process::id()));
    let result = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut f| {
            f.write_all(body.as_bytes())?;
            f.sync_all()
        })
        .map_err(|e| format!("write {}: {e}", tmp.display()))
        .and_then(|()| {
            std::fs::rename(&tmp, path).map_err(|e| format!("replace {}: {e}", path.display()))
        });
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Claude Code treats a login as the same only when both the account and the
/// organization match: one person can sit in a personal and a team org.
pub fn identity(account: &Map<String, Value>) -> Option<(&str, &str)> {
    let org = account
        .get("organizationUuid")
        .and_then(Value::as_str)
        .unwrap_or("");
    Some((account.get("accountUuid")?.as_str()?, org))
}

pub fn email(account: &Map<String, Value>) -> &str {
    account
        .get("emailAddress")
        .and_then(Value::as_str)
        .unwrap_or("?")
}

/// Copy `claudeAiOauth` from `from` into `into`. The item also holds MCP
/// server logins (`mcpOAuth`), which belong to the profile, not the account.
pub fn splice_oauth(into: Option<&str>, from: &str) -> Result<String, String> {
    let from: Value = serde_json::from_str(from).map_err(|e| format!("parse credential: {e}"))?;
    let oauth = from
        .get("claudeAiOauth")
        .ok_or("credential has no claudeAiOauth")?
        .clone();
    let mut into: Value = match into {
        Some(s) => serde_json::from_str(s).map_err(|e| format!("parse credential: {e}"))?,
        None => Value::Object(Map::new()),
    };
    into.as_object_mut()
        .ok_or("credential is not an object")?
        .insert("claudeAiOauth".into(), oauth);
    serde_json::to_string(&into).map_err(|e| e.to_string())
}

/// `claudeAiOauth.expiresAt` of a stored credential; a later one is fresher.
pub fn expires_at(credential: &str) -> i64 {
    serde_json::from_str::<Value>(credential)
        .ok()
        .and_then(|v| v.get("claudeAiOauth")?.get("expiresAt")?.as_i64())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn service_hashes_raw_path() {
        assert_eq!(
            service_for("/Users/loki/.claude"),
            "Claude Code-credentials-956e1fb2"
        );
        // A trailing slash is a different string, so a different item.
        assert_eq!(
            service_for("/Users/loki/.claude/"),
            "Claude Code-credentials-3652a864"
        );
    }

    #[test]
    fn splice_keeps_other_keys_and_order() {
        let body = r#"{"a":1,"oauthAccount":{"emailAddress":"old"},"z":2}"#;
        let account = json!({"emailAddress": "new"}).as_object().unwrap().clone();
        let out: Value = serde_json::from_str(&splice_account(body, &account).unwrap()).unwrap();
        let keys: Vec<_> = out.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["a", "oauthAccount", "z"]);
        assert_eq!(out["oauthAccount"]["emailAddress"], "new");
    }

    #[test]
    fn splice_oauth_keeps_mcp_logins() {
        let into = r#"{"mcpOAuth":{"a":1},"claudeAiOauth":{"expiresAt":1}}"#;
        let from = r#"{"mcpOAuth":{"b":2},"claudeAiOauth":{"expiresAt":9}}"#;
        let out: Value = serde_json::from_str(&splice_oauth(Some(into), from).unwrap()).unwrap();
        assert_eq!(out["mcpOAuth"], json!({"a": 1}));
        assert_eq!(out["claudeAiOauth"]["expiresAt"], 9);
        let fresh: Value = serde_json::from_str(&splice_oauth(None, from).unwrap()).unwrap();
        assert_eq!(fresh, json!({"claudeAiOauth": {"expiresAt": 9}}));
    }

    #[test]
    fn identity_includes_the_org() {
        let personal = json!({"accountUuid": "a", "organizationUuid": "p"});
        let team = json!({"accountUuid": "a", "organizationUuid": "t"});
        let id = |v: &Value| {
            identity(v.as_object().unwrap()).map(|(a, o)| (a.to_string(), o.to_string()))
        };
        assert_ne!(id(&personal), id(&team));
        assert_eq!(
            id(&json!({"accountUuid": "a"})),
            Some(("a".into(), "".into()))
        );
        assert_eq!(id(&json!({})), None);
    }

    #[test]
    fn expires_at_reads_or_defaults() {
        assert_eq!(expires_at(r#"{"claudeAiOauth":{"expiresAt":42}}"#), 42);
        assert_eq!(expires_at("not json"), 0);
    }
}
