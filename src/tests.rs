//! The switch flow against a temp home and an in-memory store.

use super::*;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct MemStore {
    items: RefCell<HashMap<String, String>>,
    /// Upcoming writes to a service that fail, in order. `true` stores the
    /// value before failing, `false` fails without storing it.
    failures: RefCell<HashMap<String, Vec<bool>>>,
}

impl MemStore {
    fn fail(&self, service: &str, writes: &[bool]) {
        self.failures
            .borrow_mut()
            .insert(service.into(), writes.to_vec());
    }
}

impl Store for MemStore {
    fn read(&self, service: &str) -> Result<Option<String>, String> {
        Ok(self.items.borrow().get(service).cloned())
    }
    fn write(&self, service: &str, value: &str) -> Result<(), String> {
        let next = self
            .failures
            .borrow_mut()
            .get_mut(service)
            .filter(|q| !q.is_empty())
            .map(|q| q.remove(0));
        if next != Some(false) {
            self.items.borrow_mut().insert(service.into(), value.into());
        }
        match next {
            Some(_) => Err("injected write failure".into()),
            None => Ok(()),
        }
    }
    fn delete(&self, service: &str) -> Result<(), String> {
        self.items.borrow_mut().remove(service);
        Ok(())
    }
}

fn account(uuid: &str, org: &str, email: &str) -> Value {
    json!({"accountUuid": uuid, "organizationUuid": org, "emailAddress": email})
}

fn credential(token: &str, expires: i64) -> String {
    json!({"claudeAiOauth": {"refreshToken": token, "expiresAt": expires}}).to_string()
}

/// A home with accounts `a` and `b` saved, and `a` active with a rotated token.
struct Fixture {
    dir: PathBuf,
    store: MemStore,
}

impl Fixture {
    fn new() -> Fixture {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ccs-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("home/.claude")).unwrap();
        let f = Fixture {
            dir,
            store: MemStore::default(),
        };
        f.save("a", account("A", "org", "a@x"), &credential("a-old", 1));
        f.save("b", account("B", "org", "b@x"), &credential("b-1", 1));
        f.set_config(json!({"keep": 1, "oauthAccount": account("A", "org", "a@x")}));
        f.store
            .write(claude::DEFAULT_SERVICE, &f.default_with("a-new", 5))
            .unwrap();
        f
    }

    fn ctx(&self) -> Ctx<'_> {
        Ctx {
            home: self.dir.join("home"),
            ccs_home: self.dir.join("ccs"),
            store: &self.store,
            patient: &self.store,
        }
    }

    fn save(&self, name: &str, acct: Value, cred: &str) {
        let dir = self.dir.join("ccs").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".claude.json"),
            json!({"oauthAccount": acct}).to_string(),
        )
        .unwrap();
        self.store
            .write(&Profile::config_dir(&dir).service, cred)
            .unwrap();
    }

    fn default_with(&self, token: &str, expires: i64) -> String {
        let mut v: Value = serde_json::from_str(&credential(token, expires)).unwrap();
        v["mcpOAuth"] = json!({"server": "mcp-login"});
        v.to_string()
    }

    fn set_config(&self, v: Value) {
        std::fs::write(self.dir.join("home/.claude.json"), v.to_string()).unwrap();
    }

    fn config(&self) -> Value {
        claude::read_json(&self.dir.join("home/.claude.json")).unwrap()
    }

    fn item(&self, service: &str) -> Value {
        serde_json::from_str(&self.store.read(service).unwrap().unwrap()).unwrap()
    }

    fn profile_item(&self, name: &str) -> Value {
        self.item(&Profile::config_dir(&self.dir.join("ccs").join(name)).service)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn switch_saves_the_rotated_token_and_keeps_mcp_logins() {
    let f = Fixture::new();
    switch(&f.ctx(), "b").unwrap();

    let default = f.item(claude::DEFAULT_SERVICE);
    assert_eq!(default["claudeAiOauth"]["refreshToken"], "b-1");
    assert_eq!(default["mcpOAuth"]["server"], "mcp-login");
    assert_eq!(f.config()["oauthAccount"]["emailAddress"], "b@x");
    assert_eq!(f.config()["keep"], 1);
    // The token `a` rotated to while active is not lost.
    assert_eq!(
        f.profile_item("a")["claudeAiOauth"]["refreshToken"],
        "a-new"
    );
    assert!(f.profile_item("a").get("mcpOAuth").is_none());

    switch(&f.ctx(), "a").unwrap();
    let default = f.item(claude::DEFAULT_SERVICE);
    assert_eq!(default["claudeAiOauth"]["refreshToken"], "a-new");
    assert_eq!(default["mcpOAuth"]["server"], "mcp-login");
    // No lock or marker is left behind.
    assert!(!f.dir.join("home/.claude/.oauth_refresh.lock").exists());
    assert!(!f.dir.join("home/.claude.lock").exists());
    assert!(!f.dir.join("ccs/.switching").exists());
    assert!(!f.dir.join("home/.claude.json.lock").exists());
}

#[test]
fn switch_to_the_active_account_does_nothing() {
    let f = Fixture::new();
    let msg = switch(&f.ctx(), "a").unwrap();
    assert!(msg.starts_with("already on a"), "{msg}");
}

#[test]
fn switch_tells_orgs_of_one_account_apart() {
    let f = Fixture::new();
    f.save(
        "team",
        account("A", "team-org", "a@x"),
        &credential("team-1", 1),
    );
    switch(&f.ctx(), "team").unwrap();
    assert_eq!(f.config()["oauthAccount"]["organizationUuid"], "team-org");
    // The personal org's token went to `a` only, not to `team`.
    assert_eq!(
        f.profile_item("a")["claudeAiOauth"]["refreshToken"],
        "a-new"
    );
    assert_eq!(
        f.profile_item("team")["claudeAiOauth"]["refreshToken"],
        "team-1"
    );
}

#[test]
fn switch_refuses_to_drop_an_unsaved_login() {
    let f = Fixture::new();
    f.set_config(json!({"oauthAccount": account("C", "org", "c@x")}));
    let err = switch(&f.ctx(), "b").unwrap_err();
    assert!(err.contains("c@x"), "{err}");
    // Without an identity, an item that holds a login is still protected.
    f.set_config(json!({}));
    assert!(switch(&f.ctx(), "b").is_err());
    assert_eq!(
        f.item(claude::DEFAULT_SERVICE)["claudeAiOauth"]["refreshToken"],
        "a-new"
    );
}

#[test]
fn switch_proceeds_when_only_mcp_logins_remain() {
    let f = Fixture::new();
    f.set_config(json!({}));
    f.store
        .write(
            claude::DEFAULT_SERVICE,
            &json!({"mcpOAuth": {"server": "mcp-login"}}).to_string(),
        )
        .unwrap();
    switch(&f.ctx(), "b").unwrap();
    let default = f.item(claude::DEFAULT_SERVICE);
    assert_eq!(default["claudeAiOauth"]["refreshToken"], "b-1");
    assert_eq!(default["mcpOAuth"]["server"], "mcp-login");
}

#[test]
fn switch_restores_the_token_when_the_config_write_fails() {
    let f = Fixture::new();
    let before = f.store.read(claude::DEFAULT_SERVICE).unwrap();
    // A directory where the temp config goes makes the config write fail.
    let tmp = f
        .dir
        .join(format!("home/.claude.json.ccs-{}", std::process::id()));
    std::fs::create_dir(&tmp).unwrap();

    let err = switch(&f.ctx(), "b").unwrap_err();
    assert!(err.contains("previous login is restored"), "{err}");
    assert_eq!(f.store.read(claude::DEFAULT_SERVICE).unwrap(), before);
    assert_eq!(f.config()["oauthAccount"]["emailAddress"], "a@x");
}

#[test]
fn switch_restores_the_token_when_the_keychain_write_fails() {
    let f = Fixture::new();
    let before = f.store.read(claude::DEFAULT_SERVICE).unwrap();
    f.store.fail(claude::DEFAULT_SERVICE, &[true]);

    let err = switch(&f.ctx(), "b").unwrap_err();
    assert!(err.contains("previous login is restored"), "{err}");
    assert_eq!(f.store.read(claude::DEFAULT_SERVICE).unwrap(), before);
    assert_eq!(f.config()["oauthAccount"]["emailAddress"], "a@x");
}

#[test]
fn remove_deletes_the_profile_and_its_item() {
    let f = Fixture::new();
    let service = Profile::config_dir(&f.dir.join("ccs/b")).service;
    remove(&f.ctx(), "b").unwrap();
    assert!(!f.dir.join("ccs/b").exists());
    assert!(f.store.read(&service).unwrap().is_none());
    assert_eq!(list(&f.ctx()).unwrap(), "* a  a@x");
}

#[test]
fn switch_repairs_a_failed_rollback_without_polluting_a_saved_token() {
    let f = Fixture::new();
    // b's token expires later than a's, so a careless save-back would take it.
    f.save("b", account("B", "org", "b@x"), &credential("b-1", 9));
    // The write of b's token lands but reports failure, then the rollback fails.
    f.store.fail(claude::DEFAULT_SERVICE, &[true, false]);

    let err = switch(&f.ctx(), "b").unwrap_err();
    assert!(err.contains("also failed"), "{err}");
    assert!(f.dir.join("ccs/.switching").exists());
    // The mismatch the marker guards: b's token, a's identity.
    assert_eq!(
        f.item(claude::DEFAULT_SERVICE)["claudeAiOauth"]["refreshToken"],
        "b-1"
    );
    assert_eq!(f.config()["oauthAccount"]["emailAddress"], "a@x");

    // The retry does not trust the token: no "already on", no save-back.
    let msg = switch(&f.ctx(), "a").unwrap();
    assert!(msg.starts_with("switched to a"), "{msg}");
    assert_eq!(
        f.profile_item("a")["claudeAiOauth"]["refreshToken"],
        "a-new"
    );
    let default = f.item(claude::DEFAULT_SERVICE);
    assert_eq!(default["claudeAiOauth"]["refreshToken"], "a-new");
    assert_eq!(default["mcpOAuth"]["server"], "mcp-login");
    assert!(!f.dir.join("ccs/.switching").exists());
}

#[test]
fn a_restored_switch_clears_its_marker() {
    let f = Fixture::new();
    f.store.fail(claude::DEFAULT_SERVICE, &[true]);
    switch(&f.ctx(), "b").unwrap_err();
    assert!(!f.dir.join("ccs/.switching").exists());
}
