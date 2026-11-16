//! Test-only scratch folder in the temporary folder, removed when it goes out of scope.
//! Rust tests in one binary run side by side in one process, so the name carries the
//! pid, the time and a counter: two tests never share a folder, not even when the
//! suite runs twice at once.

use std::ffi::OsString;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

pub(crate) struct TempDir(PathBuf);

impl TempDir {
    /// Creates an empty folder `$TMPDIR/{prefix}-…`.
    pub(crate) fn new(prefix: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("{prefix}-{pid}-{nanos}-{unique}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("test scratch folder");
        Self(dir)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Sets `HOME` for one test and puts it back on drop. The environment is one per process and
/// tests run side by side, so a lock keeps two overrides from interleaving; a test that panics
/// while holding it leaves the mutex poisoned, which the next one takes anyway.
pub(crate) struct Home {
    previous: Option<OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl Home {
    pub(crate) fn set(path: &Path) -> Self {
        static LOCK: Mutex<()> = Mutex::new(());
        let lock = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let previous = std::env::var_os("HOME");
        // SAFETY: the lock keeps every HOME override in this binary sequential.
        unsafe { std::env::set_var("HOME", path) };
        Self {
            previous,
            _lock: lock,
        }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        // SAFETY: the lock is still held.
        unsafe {
            match &self.previous {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}

/// Waits up to five seconds for a session log's lock to go after its `Session` is dropped.
/// Another test forking at that moment (the bash tool spawns through `pre_exec`) holds a copy
/// of every open fd until it execs, and with it the lock, so the release is awaited instead
/// of assumed instant; a leaked lock still fails.
pub(crate) fn await_unlocked(path: &Path) {
    for _ in 0..100 {
        if !crate::session::is_locked(path) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("the lock on {} outlived its session", path.display());
}
