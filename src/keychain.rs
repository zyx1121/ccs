//! macOS Keychain access through `/usr/bin/security`, the same binary Claude
//! Code uses. Going through it keeps the item ACLs unchanged, so neither tool
//! triggers a "wants to use your keychain" prompt.

use std::io::Write;
use std::process::{Command, Stdio};

const SECURITY: &str = "/usr/bin/security";
const NOT_FOUND: i32 = 44;
// `security -i` reads stdin with a 4096-byte line buffer.
const STDIN_LINE_LIMIT: usize = 4096 - 64;

/// Account attribute Claude Code keys its item with: `$USER`, then the OS user.
pub fn user() -> String {
    std::env::var("USER")
        .ok()
        .filter(|u| !u.is_empty())
        .or_else(|| {
            let out = Command::new("/usr/bin/id").arg("-un").output().ok()?;
            Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

pub fn read(service: &str) -> Result<Option<String>, String> {
    let out = Command::new(SECURITY)
        .args(["find-generic-password", "-a", &user(), "-s", service, "-w"])
        .output()
        .map_err(|e| format!("run security: {e}"))?;
    match out.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim_end().to_string(),
        )),
        Some(NOT_FOUND) => Ok(None),
        _ => Err(format!(
            "security find-generic-password failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

pub fn write(service: &str, value: &str) -> Result<(), String> {
    let hex: String = value.bytes().map(|b| format!("{b:02x}")).collect();
    let user = user();
    let line = format!("add-generic-password -U -a {user:?} -s {service:?} -X {hex}\n");
    // Prefer stdin so the secret stays out of argv.
    let status = if line.len() <= STDIN_LINE_LIMIT {
        let mut child = Command::new(SECURITY)
            .arg("-i")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("run security: {e}"))?;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(line.as_bytes())
            .map_err(|e| format!("write to security: {e}"))?;
        child.wait_with_output().map_err(|e| e.to_string())?
    } else {
        Command::new(SECURITY)
            .args([
                "add-generic-password",
                "-U",
                "-a",
                &user,
                "-s",
                service,
                "-X",
                &hex,
            ])
            .output()
            .map_err(|e| format!("run security: {e}"))?
    };
    // `security -i` exits 0 even when a command fails, so read the value back.
    if !status.status.success() || read(service)?.as_deref() != Some(value) {
        return Err(format!(
            "security add-generic-password failed for {service}: {}",
            String::from_utf8_lossy(&status.stderr).trim()
        ));
    }
    Ok(())
}

pub fn delete(service: &str) -> Result<(), String> {
    let out = Command::new(SECURITY)
        .args(["delete-generic-password", "-a", &user(), "-s", service])
        .output()
        .map_err(|e| format!("run security: {e}"))?;
    match out.status.code() {
        Some(0) | Some(NOT_FOUND) => Ok(()),
        _ => Err(format!(
            "security delete-generic-password failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}
