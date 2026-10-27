//! macOS Keychain access through `/usr/bin/security`, the same binary Claude
//! Code uses. Going through it keeps the item ACLs unchanged, so neither tool
//! triggers a "wants to use your keychain" prompt.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const SECURITY: &str = "/usr/bin/security";
const NOT_FOUND: i32 = 44;
// `security -i` reads stdin with a 4096-byte line buffer.
const STDIN_LINE_LIMIT: usize = 4096 - 64;
// A locked Keychain can block `security` on an unlock dialog; never wait on it
// while we hold Claude Code's locks.
const TIMEOUT: Duration = Duration::from_secs(5);

struct Output {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str], input: Option<&str>) -> Result<Output, String> {
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
        if start.elapsed() > TIMEOUT {
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

/// Account attribute Claude Code keys its item with: `$USER` when it is a
/// plain name, else `claude-code-user`.
pub fn user() -> String {
    match std::env::var("USER") {
        Ok(u)
            if !u.is_empty()
                && u.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) =>
        {
            u
        }
        _ => "claude-code-user".to_string(),
    }
}

pub fn read(service: &str) -> Result<Option<String>, String> {
    let out = run(
        &["find-generic-password", "-a", &user(), "-s", service, "-w"],
        None,
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

pub fn write(service: &str, value: &str) -> Result<(), String> {
    let hex: String = value.bytes().map(|b| format!("{b:02x}")).collect();
    let user = user();
    let line = format!("add-generic-password -U -a {user:?} -s {service:?} -X {hex}\n");
    // Prefer stdin so the secret stays out of argv. A longer value cannot fit
    // the line buffer, so it goes through argv, as Claude Code does.
    let out = if line.len() <= STDIN_LINE_LIMIT {
        run(&["-i"], Some(&line))?
    } else {
        run(
            &[
                "add-generic-password",
                "-U",
                "-a",
                &user,
                "-s",
                service,
                "-X",
                &hex,
            ],
            None,
        )?
    };
    // `security -i` exits 0 even when a command fails, so read the value back.
    if out.code != Some(0) || read(service)?.as_deref() != Some(value) {
        return Err(format!(
            "security add-generic-password failed for {service}: {}",
            out.stderr.trim()
        ));
    }
    Ok(())
}

pub fn delete(service: &str) -> Result<(), String> {
    let out = run(
        &["delete-generic-password", "-a", &user(), "-s", service],
        None,
    )?;
    match out.code {
        Some(0) | Some(NOT_FOUND) => Ok(()),
        _ => Err(format!(
            "security delete-generic-password failed: {}",
            out.stderr.trim()
        )),
    }
}
