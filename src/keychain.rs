//! macOS Keychain access through `/usr/bin/security`, the binary Claude Code
//! uses. Its items trust only that binary, so any other caller, including
//! Security.framework from this process, can trigger an access dialog.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Where account credentials live. The real one is [`Keychain`]; tests use an
/// in-memory store.
pub trait Store {
    fn read(&self, service: &str) -> Result<Option<String>, String>;
    fn write(&self, service: &str, value: &str) -> Result<(), String>;
    fn delete(&self, service: &str) -> Result<(), String>;
}

pub struct Keychain {
    /// Limit for one `security` call. Short while we hold Claude Code's locks,
    /// long when a Keychain unlock dialog may be waiting for the user.
    pub timeout: Duration,
}

pub const SHORT: Duration = Duration::from_secs(5);
pub const LONG: Duration = Duration::from_secs(120);

const SECURITY: &str = "/usr/bin/security";
const NOT_FOUND: i32 = 44;
// `security -i` reads stdin with a 4096-byte line buffer; Claude Code uses 4032.
const STDIN_LINE_LIMIT: usize = 4096 - 64;

struct Output {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str], input: Option<&str>, timeout: Duration) -> Result<Output, String> {
    let mut child = Command::new(SECURITY)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("run security: {e}"))?;
    if let Some(input) = input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .map_err(|e| format!("write to security: {e}"))?;
    }
    let drain = |mut r: Box<dyn Read + Send>| {
        thread::spawn(move || {
            let mut s = String::new();
            let _ = r.read_to_string(&mut s);
            s
        })
    };
    let stdout = drain(Box::new(child.stdout.take().unwrap()));
    let stderr = drain(Box::new(child.stderr.take().unwrap()));
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "security {} timed out; is the login Keychain locked?",
                args[0]
            ));
        }
        thread::sleep(Duration::from_millis(1));
    };
    Ok(Output {
        code: status.code(),
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// Account attribute Claude Code keys its item with: `$USER`, else the OS
/// user name, and `claude-code-user` when that is not a plain name.
pub fn user() -> String {
    let name = std::env::var("USER")
        .ok()
        .filter(|u| !u.is_empty())
        .or_else(|| {
            let out = Command::new("/usr/bin/id").arg("-un").output().ok()?;
            Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
        })
        .unwrap_or_default();
    let plain = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    if plain {
        name
    } else {
        "claude-code-user".to_string()
    }
}

impl Store for Keychain {
    fn read(&self, service: &str) -> Result<Option<String>, String> {
        let out = run(
            &["find-generic-password", "-a", &user(), "-s", service, "-w"],
            None,
            self.timeout,
        )?;
        match out.code {
            Some(0) => Ok(Some(out.stdout.trim_end().to_string())),
            Some(NOT_FOUND) => Ok(None),
            _ => Err(format!(
                "security find-generic-password failed: {}",
                out.stderr.trim()
            )),
        }
    }

    fn write(&self, service: &str, value: &str) -> Result<(), String> {
        let hex: String = value.bytes().map(|b| format!("{b:02x}")).collect();
        let user = user();
        let line = format!("add-generic-password -U -a {user:?} -s {service:?} -X {hex}\n");
        // Prefer stdin so the secret stays out of argv. A longer value cannot
        // fit the line buffer and goes through argv, as Claude Code does (#2).
        let out = if line.len() <= STDIN_LINE_LIMIT {
            run(&["-i"], Some(&line), self.timeout)?
        } else {
            let args = [
                "add-generic-password",
                "-U",
                "-a",
                &user,
                "-s",
                service,
                "-X",
                &hex,
            ];
            run(&args, None, self.timeout)?
        };
        // `security -i` exits 0 even when a command fails, so read the value back.
        if out.code != Some(0) || self.read(service)?.as_deref() != Some(value) {
            return Err(format!(
                "security add-generic-password failed for {service}: {}",
                out.stderr.trim()
            ));
        }
        Ok(())
    }

    fn delete(&self, service: &str) -> Result<(), String> {
        let out = run(
            &["delete-generic-password", "-a", &user(), "-s", service],
            None,
            self.timeout,
        )?;
        match out.code {
            Some(0) | Some(NOT_FOUND) => Ok(()),
            _ => Err(format!(
                "security delete-generic-password failed: {}",
                out.stderr.trim()
            )),
        }
    }
}
