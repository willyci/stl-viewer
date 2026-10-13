//! User settings kept in `settings.json` next to the thumbnail cache database, so portable
//! mode (`portable.txt` beside the exe) keeps them in the exe's `cache` folder too.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "settings.json";
/// Changes are written at most this often (and once more on exit).
const SAVE_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub model_color: Option<[u8; 3]>,
    /// Root of the file browser tree.
    pub tree_root: Option<PathBuf>,
    /// Folder last shown in the thumbnail grid; reopened at startup.
    pub last_folder: Option<PathBuf>,
    pub show_media: bool,
    pub large_icons: bool,
    /// [`crate::grid::SortOrder::key`].
    pub sort: Option<String>,
    /// Inner window size in points.
    pub window_size: Option<[f32; 2]>,
    /// Sidebar / right panel sizer widths.
    pub split: Option<[f32; 2]>,
}

/// `settings.json` beside the cache database in use (`None` for an in-memory cache).
pub fn path_for(cache_db: Option<&Path>) -> Option<PathBuf> {
    let db = match cache_db {
        Some(p) if p.as_os_str() == ":memory:" => return None,
        Some(p) => p.to_path_buf(),
        None => stlv_thumbs::default_cache_path(),
    };
    Some(db.parent().unwrap_or(Path::new(".")).join(FILE_NAME))
}

impl Settings {
    /// Missing or unreadable files give the defaults.
    pub fn load(path: Option<&Path>) -> Self {
        let Some(path) = path else {
            return Self::default();
        };
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                log::warn!("ignoring unreadable {}: {e}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Write atomically (temp file + rename) so a crash never leaves a half-written file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }

    /// An existing directory from the settings, if still there.
    pub fn existing_dir(p: &Option<PathBuf>) -> Option<PathBuf> {
        p.as_ref().filter(|d| d.is_dir()).cloned()
    }
}

/// Writes [`Settings`] whenever they change, throttled.
pub struct Persist {
    path: Option<PathBuf>,
    saved: Settings,
    pending: Option<Settings>,
    last_write: Instant,
}

impl Persist {
    pub fn new(path: Option<PathBuf>, loaded: Settings) -> Self {
        Self {
            path,
            saved: loaded,
            pending: None,
            last_write: Instant::now() - SAVE_INTERVAL,
        }
    }

    /// Record the current settings; written when changed and not written in the last second.
    pub fn update(&mut self, current: Settings) {
        if current == self.saved {
            self.pending = None;
            return;
        }
        self.pending = Some(current);
        if self.last_write.elapsed() >= SAVE_INTERVAL {
            self.flush();
        }
    }

    /// True while a change waits for the throttle (the app keeps a repaint scheduled).
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn flush(&mut self) {
        let (Some(path), Some(current)) = (&self.path, self.pending.take()) else {
            return;
        };
        self.last_write = Instant::now();
        match current.save(path) {
            Ok(()) => self.saved = current,
            Err(e) => log::warn!("cannot save {}: {e}", path.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;

    #[test]
    fn round_trip_and_defaults() {
        let d = TempDir::new("settings");
        let path = d.path().join("cache").join(FILE_NAME);
        assert_eq!(Settings::load(Some(&path)), Settings::default());
        let s = Settings {
            model_color: Some([255, 128, 0]),
            tree_root: Some(d.path().to_path_buf()),
            last_folder: Some(d.path().join("models")),
            show_media: true,
            large_icons: true,
            sort: Some("size_desc".into()),
            window_size: Some([1500.0, 900.0]),
            split: Some([300.0, 1100.0]),
        };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(Some(&path)), s);
        // Unknown or missing fields are tolerated (older/newer versions).
        std::fs::write(&path, br#"{"show_media": true, "future_field": 1}"#).unwrap();
        let loaded = Settings::load(Some(&path));
        assert!(loaded.show_media && loaded.model_color.is_none());
        std::fs::write(&path, b"not json").unwrap();
        assert_eq!(Settings::load(Some(&path)), Settings::default());
    }

    #[test]
    fn stored_next_to_the_cache() {
        assert_eq!(
            path_for(Some(Path::new(r"C:\x\cache\thumbnails.db"))),
            Some(PathBuf::from(r"C:\x\cache\settings.json"))
        );
        assert_eq!(path_for(Some(Path::new(":memory:"))), None);
    }

    #[test]
    fn persist_writes_changes_only() {
        let d = TempDir::new("settings-persist");
        let path = d.path().join(FILE_NAME);
        let mut p = Persist::new(Some(path.clone()), Settings::default());
        p.update(Settings::default());
        assert!(!path.exists(), "unchanged settings are not written");
        let s = Settings {
            large_icons: true,
            ..Default::default()
        };
        p.update(s.clone());
        assert_eq!(Settings::load(Some(&path)), s);
        // Throttled: a second change right away waits for flush.
        let s2 = Settings {
            show_media: true,
            ..s
        };
        p.update(s2.clone());
        assert!(p.is_pending());
        p.flush();
        assert_eq!(Settings::load(Some(&path)), s2);
    }
}
