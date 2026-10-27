//! Claude Code's advisory locks (npm `proper-lockfile`): a lock is a directory,
//! and `mkdir` is the mutex. Holding them while we swap credentials keeps a
//! running Claude Code from saving a refreshed token over ours.

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime};

pub struct Lock(PathBuf);

impl Lock {
    /// Wait up to `timeout` for the lock. A lock older than `stale` belongs to a
    /// dead holder and is taken over.
    pub fn acquire(path: &Path, stale: Duration, timeout: Duration) -> Result<Lock, String> {
        let start = Instant::now();
        loop {
            match std::fs::create_dir(path) {
                Ok(()) => return Ok(Lock(path.to_path_buf())),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let age = std::fs::metadata(path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| SystemTime::now().duration_since(t).ok());
                    if age.is_some_and(|a| a > stale) {
                        let _ = std::fs::remove_dir(path);
                        continue;
                    }
                }
                Err(e) => return Err(format!("lock {}: {e}", path.display())),
            }
            if start.elapsed() > timeout {
                return Err(format!(
                    "{} is held; Claude Code may be refreshing a token, try again",
                    path.display()
                ));
            }
            sleep(Duration::from_millis(200));
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}
