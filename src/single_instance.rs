//! Per-user locks: one background process (two would record twice and fight over the hotkey)
//! and one settings window.

use std::fs::{File, OpenOptions, TryLockError};
use std::time::Duration;

/// Held by the background process (tray, hotkey, recording).
pub const DAEMON: &str = "daemon.lock";
/// Held by the open settings window.
pub const WINDOW: &str = "window.lock";

/// Held for the life of the process; the OS releases the lock when it exits.
pub struct InstanceLock(#[allow(dead_code)] File);

/// `Ok(None)` means another process already holds the lock.
pub fn acquire(name: &str) -> Result<Option<InstanceLock>, String> {
    let dir = kairo::config::config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(name))
        .map_err(|e| e.to_string())?;
    match file.try_lock() {
        Ok(()) => Ok(Some(InstanceLock(file))),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e.to_string()),
    }
}

/// Like [`acquire`], but keeps trying for a few seconds, for a restart where the old process is
/// still shutting down.
pub fn acquire_patiently(name: &str) -> Result<Option<InstanceLock>, String> {
    acquire_within(name, Duration::from_secs(5))
}

/// Like [`acquire`], but keeps trying for up to `patience`.
pub fn acquire_within(name: &str, patience: Duration) -> Result<Option<InstanceLock>, String> {
    let start = std::time::Instant::now();
    loop {
        if let Some(lock) = acquire(name)? {
            return Ok(Some(lock));
        }
        if start.elapsed() >= patience {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// True if another process holds the lock.
pub fn is_held(name: &str) -> bool {
    matches!(acquire(name), Ok(None))
}
