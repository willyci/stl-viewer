//! Keeps the browser tree in sync with the disk, like the `QFileSystemWatcher` behind Qt's
//! `QFileSystemModel`: a background thread polls the modification time of each listed
//! folder (creating, deleting or renaming an entry updates it) and reports the folders that
//! changed. An idle app costs a few `stat` calls per poll and wakes the UI only when a
//! listing is stale.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, Thread};
use std::time::{Duration, SystemTime};

pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// A folder's modification time; `None` when it cannot be read (e.g. it was deleted).
pub fn dir_mtime(dir: &Path) -> Option<SystemTime> {
    std::fs::metadata(dir).and_then(|m| m.modified()).ok()
}

type Stamps = HashMap<PathBuf, Option<SystemTime>>;

#[derive(Default)]
struct Shared {
    dirs: Mutex<Stamps>,
    stop: AtomicBool,
}

impl Shared {
    fn dirs(&self) -> std::sync::MutexGuard<'_, Stamps> {
        self.dirs.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub struct DirWatcher {
    shared: Arc<Shared>,
    changed: Receiver<PathBuf>,
    thread: Option<Thread>,
}

impl DirWatcher {
    /// Poll every `interval`; `wake` runs on the watcher thread after a change was found.
    pub fn new(interval: Duration, wake: impl Fn() + Send + 'static) -> Self {
        let shared = Arc::new(Shared::default());
        let (tx, changed) = channel();
        let poller = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("tree-watch".into())
            .spawn(move || poll_loop(&poller, interval, &tx, &wake))
            .map(|t| t.thread().clone())
            .map_err(|e| log::warn!("cannot start the folder watcher: {e}"))
            .ok();
        Self {
            shared,
            changed,
            thread,
        }
    }

    /// Watch exactly these folders, each with its modification time when it was listed.
    pub fn set_dirs(&self, dirs: Stamps) {
        *self.shared.dirs() = dirs;
    }

    /// Folders whose contents changed since they were listed; each change is reported once.
    pub fn changed(&self) -> Vec<PathBuf> {
        self.changed.try_iter().collect()
    }
}

impl Drop for DirWatcher {
    fn drop(&mut self) {
        // Not joined: a stat of an unreachable network folder may take a while.
        self.shared.stop.store(true, Ordering::Release);
        if let Some(t) = &self.thread {
            t.unpark();
        }
    }
}

fn poll_loop(shared: &Shared, interval: Duration, tx: &Sender<PathBuf>, wake: &dyn Fn()) {
    loop {
        thread::park_timeout(interval);
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        // Stat without holding the lock, so a slow folder never blocks the UI thread.
        let snapshot: Vec<(PathBuf, Option<SystemTime>)> = shared
            .dirs()
            .iter()
            .map(|(dir, stamp)| (dir.clone(), *stamp))
            .collect();
        let changed: Vec<(PathBuf, Option<SystemTime>)> = snapshot
            .into_iter()
            .filter_map(|(dir, stamp)| {
                let now = dir_mtime(&dir);
                (now != stamp).then_some((dir, now))
            })
            .collect();
        if changed.is_empty() {
            continue;
        }
        let mut reported = false;
        {
            let mut dirs = shared.dirs();
            for (dir, now) in changed {
                // Skip folders that stopped being watched meanwhile.
                if let Some(stamp) = dirs.get_mut(&dir) {
                    *stamp = now;
                    reported |= tx.send(dir).is_ok();
                }
            }
        }
        if reported {
            wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::test_util::TempDir;

    #[test]
    fn reports_changed_folders_once() {
        let d = TempDir::new("watch");
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let watcher = DirWatcher::new(Duration::from_millis(20), || {});
        watcher.set_dirs(
            [&a, &b]
                .into_iter()
                .map(|p| (p.clone(), dir_mtime(p)))
                .collect(),
        );
        std::thread::sleep(Duration::from_millis(100));
        assert!(watcher.changed().is_empty(), "nothing changed yet");

        std::fs::write(b.join("new.stl"), b"x").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut seen = Vec::new();
        while seen.is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            seen = watcher.changed();
        }
        assert_eq!(seen, vec![b.clone()]);
        std::thread::sleep(Duration::from_millis(100));
        assert!(watcher.changed().is_empty(), "a change is reported once");

        // A deleted folder is a change too.
        std::fs::remove_dir(&a).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        seen.clear();
        while seen.is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            seen = watcher.changed();
        }
        assert_eq!(seen, vec![a]);
    }
}
