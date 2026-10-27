//! Claude Code's advisory locks (npm `proper-lockfile`): a lock is a directory,
//! and `mkdir` is the mutex. Holding them while we swap credentials keeps a
//! running Claude Code from saving a refreshed token over ours.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle, sleep};
use std::time::{Duration, Instant, SystemTime};

// proper-lockfile treats a lock as live while its mtime moves; Claude Code
// touches every 5 s, we touch a little faster.
const TOUCH_INTERVAL: Duration = Duration::from_secs(3);

pub struct Lock {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    toucher: Option<JoinHandle<()>>,
}

impl Lock {
    /// Wait up to `timeout` for the lock. A lock older than `stale` belongs to a
    /// dead holder and is taken over.
    pub fn acquire(path: &Path, stale: Duration, timeout: Duration) -> Result<Lock, String> {
        let start = Instant::now();
        loop {
            match std::fs::create_dir(path) {
                Ok(()) => return Ok(Lock::hold(path)),
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

    fn hold(path: &Path) -> Lock {
        let stop = Arc::new(AtomicBool::new(false));
        let (p, s) = (path.to_path_buf(), stop.clone());
        let toucher = thread::spawn(move || {
            loop {
                thread::park_timeout(TOUCH_INTERVAL);
                if s.load(Ordering::Relaxed) {
                    break;
                }
                if let Ok(dir) = std::fs::File::open(&p) {
                    let _ = dir.set_modified(SystemTime::now());
                }
            }
        });
        Lock {
            path: path.to_path_buf(),
            stop,
            toucher: Some(toucher),
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.toucher.take() {
            t.thread().unpark();
            let _ = t.join();
        }
        let _ = std::fs::remove_dir(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_lock_stays_fresh_and_is_released() {
        let dir = std::env::temp_dir().join(format!("ccs-lock-test-{}", std::process::id()));
        let lock = Lock::acquire(&dir, Duration::from_secs(1), Duration::from_secs(1)).unwrap();
        sleep(TOUCH_INTERVAL + Duration::from_millis(500));
        let age = SystemTime::now()
            .duration_since(std::fs::metadata(&dir).unwrap().modified().unwrap())
            .unwrap();
        assert!(age < Duration::from_secs(2), "lock went stale: {age:?}");
        // A second holder must wait, not take it over.
        assert!(Lock::acquire(&dir, Duration::from_secs(10), Duration::from_millis(300)).is_err());
        drop(lock);
        assert!(!dir.exists());
    }
}
