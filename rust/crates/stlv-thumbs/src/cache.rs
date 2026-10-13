use std::fmt::Write as _;
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use md5::{Digest, Md5};

use crate::sqlite::{Connection, Value};

#[derive(Debug)]
pub struct CacheError(pub String);

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CacheError {}

/// Verbatim from app.py, so `sqlite_master` reads the same whichever app created the file.
const CREATE_TABLE: &str = "
            CREATE TABLE IF NOT EXISTS thumbnails (
                path_hash  TEXT PRIMARY KEY,
                filepath   TEXT NOT NULL,
                mtime      REAL NOT NULL,
                filesize   INTEGER NOT NULL,
                image_data BLOB NOT NULL,
                created_at REAL NOT NULL
            )
        ";

/// Lock wait when the Python app holds the file (its `sqlite3.connect` default is 5 s).
const BUSY_TIMEOUT_MS: i32 = 5000;

/// SQLite-backed PNG cache keyed on `MD5(absolute path)` and validated against the
/// file's mtime and size -- same database file and schema as the Python app, so both
/// can share one cache. `Send + Sync`.
///
/// Database errors are logged and reported as cache misses; nothing here panics.
pub struct ThumbnailCache {
    db_path: PathBuf,
    conn: Mutex<Option<Connection>>,
}

/// What a cached thumbnail is validated against: Python's `st_mtime` and `st_size`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FileStamp {
    pub mtime: f64,
    pub size: u64,
}

impl FileStamp {
    pub(crate) fn of(meta: &Metadata) -> Self {
        Self {
            mtime: python_mtime(meta),
            size: meta.len(),
        }
    }

    fn read(path: &Path) -> std::io::Result<Self> {
        fs::metadata(path).map(|m| Self::of(&m))
    }
}

impl ThumbnailCache {
    /// Open or create the database at `db_path` (creating its directory).
    pub fn open(db_path: &Path) -> Result<Self, CacheError> {
        let path_str = db_path.to_str().ok_or_else(|| {
            CacheError(format!(
                "database path is not valid Unicode: {}",
                db_path.display()
            ))
        })?;
        if let Some(dir) = db_path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir)
                .map_err(|e| CacheError(format!("cannot create {}: {e}", dir.display())))?;
        }
        let conn = Connection::open(path_str).map_err(CacheError)?;
        conn.busy_timeout(BUSY_TIMEOUT_MS).map_err(CacheError)?;
        conn.execute(CREATE_TABLE, &[]).map_err(CacheError)?;
        Ok(Self {
            db_path: db_path.to_path_buf(),
            conn: Mutex::new(Some(conn)),
        })
    }

    /// Open [`default_cache_path`], creating its directory.
    pub fn open_default() -> Result<Self, CacheError> {
        Self::open(&default_cache_path())
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// True if a thumbnail exists and the file's mtime and size still match.
    pub fn is_valid(&self, path: &Path) -> bool {
        FileStamp::read(path).is_ok_and(|stamp| self.is_valid_for(path, stamp))
    }

    /// Cached PNG bytes if fresh.
    pub fn get_png(&self, path: &Path) -> Option<Vec<u8>> {
        let stamp = FileStamp::read(path).ok()?;
        self.lookup(path, stamp, true)
    }

    /// Store `png` for `path`, stamped with the file's current mtime and size.
    pub fn put(&self, path: &Path, png: &[u8]) -> Result<(), CacheError> {
        let stamp = FileStamp::read(path)
            .map_err(|e| CacheError(format!("cannot stat {}: {e}", path.display())))?;
        self.put_stamped(path, stamp, png)
    }

    /// Close the database (app shutdown). Later lookups miss and `put` fails.
    pub fn close(&self) {
        self.lock().take();
    }

    pub(crate) fn is_valid_for(&self, path: &Path, stamp: FileStamp) -> bool {
        self.lookup(path, stamp, false).is_some()
    }

    /// Store `png` with a stamp taken earlier: if the file changed since (e.g. while it was
    /// being parsed), the entry is stale on the next lookup instead of wrongly fresh.
    pub(crate) fn put_stamped(
        &self,
        path: &Path,
        stamp: FileStamp,
        png: &[u8],
    ) -> Result<(), CacheError> {
        let (abs, size) = row_key(path, stamp)?;
        let guard = self.lock();
        insert(connected(&guard)?, &abs, stamp.mtime, size, png).map_err(|e| {
            log::warn!("ThumbnailCache.put error for {}: {e}", path.display());
            CacheError(e)
        })
    }

    /// [`put_stamped`](Self::put_stamped) for several thumbnails in one transaction, so the
    /// database syncs to disk once instead of once per row. Rows that cannot be stored
    /// (non-Unicode paths) are skipped; a database error drops the whole batch.
    pub(crate) fn put_many(
        &self,
        rows: &[(PathBuf, FileStamp, Vec<u8>)],
    ) -> Result<(), CacheError> {
        if rows.is_empty() {
            return Ok(());
        }
        let guard = self.lock();
        let conn = connected(&guard)?;
        let fail = |e: String| {
            log::warn!(
                "ThumbnailCache: cannot store {} thumbnails: {e}",
                rows.len()
            );
            CacheError(e)
        };
        // IMMEDIATE takes the write lock up front, waiting up to the busy timeout if the
        // Python app holds it.
        conn.execute("BEGIN IMMEDIATE", &[]).map_err(fail)?;
        let written = rows
            .iter()
            .try_for_each(|(path, stamp, png)| match row_key(path, *stamp) {
                Ok((abs, size)) => insert(conn, &abs, stamp.mtime, size, png),
                Err(e) => {
                    log::debug!("not caching: {e}");
                    Ok(())
                }
            });
        written
            .and_then(|()| conn.execute("COMMIT", &[]))
            .map_err(|e| {
                // Also needed after a failed COMMIT (e.g. busy), which leaves the
                // transaction open.
                let _ = conn.execute("ROLLBACK", &[]);
                fail(e)
            })
    }

    /// `Some` (the PNG when `with_png`, else empty) iff a row matches `stamp` exactly.
    fn lookup(&self, path: &Path, stamp: FileStamp, with_png: bool) -> Option<Vec<u8>> {
        // Python could not encode such paths either, so they are never cached.
        let hash = md5_hex(&python_abspath(path)?);
        let sql = if with_png {
            "SELECT mtime, filesize, image_data FROM thumbnails WHERE path_hash=?"
        } else {
            "SELECT mtime, filesize FROM thumbnails WHERE path_hash=?"
        };
        let guard = self.lock();
        let conn = guard.as_ref()?;
        let query = || -> Result<Option<Vec<u8>>, String> {
            let mut stmt = conn.prepare(sql)?;
            stmt.bind_all(&[Value::Text(&hash)])?;
            if !stmt.step()? {
                return Ok(None);
            }
            // Exact float equality, as in Python (`row[0] == st.st_mtime`).
            let fresh = !stmt.column_is_null(0)
                && stmt.column_f64(0) == stamp.mtime
                && u64::try_from(stmt.column_i64(1)) == Ok(stamp.size);
            Ok(fresh.then(|| {
                if with_png {
                    stmt.column_blob(2).to_vec()
                } else {
                    Vec::new()
                }
            }))
        };
        query().unwrap_or_else(|e| {
            log::warn!("thumbnail cache lookup failed for {}: {e}", path.display());
            None
        })
    }

    fn lock(&self) -> MutexGuard<'_, Option<Connection>> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn connected(guard: &Option<Connection>) -> Result<&Connection, CacheError> {
    guard
        .as_ref()
        .ok_or_else(|| CacheError("thumbnail cache is closed".into()))
}

/// The absolute path a row is keyed on and the size it is validated against; `Err` for
/// paths Python could not store either.
fn row_key(path: &Path, stamp: FileStamp) -> Result<(String, i64), CacheError> {
    let abs = python_abspath(path)
        .ok_or_else(|| CacheError(format!("path is not valid Unicode: {}", path.display())))?;
    let size = i64::try_from(stamp.size)
        .map_err(|_| CacheError(format!("file too large: {}", path.display())))?;
    Ok((abs, size))
}

fn insert(conn: &Connection, abs: &str, mtime: f64, size: i64, png: &[u8]) -> Result<(), String> {
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    let params = [
        Value::Text(&md5_hex(abs)),
        Value::Text(abs),
        Value::Real(mtime),
        Value::Int(size),
        Value::Blob(png),
        Value::Real(created_at),
    ];
    conn.execute(
        "INSERT OR REPLACE INTO thumbnails \
         (path_hash, filepath, mtime, filesize, image_data, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
        &params,
    )
}

/// `<exe dir>\cache\thumbnails.db` when `portable.txt` sits next to the executable,
/// otherwise `%USERPROFILE%\.gemini\antigravity-ide\thumbnails.db`.
pub fn default_cache_path() -> PathBuf {
    let exe = std::env::current_exe().ok();
    let exe_dir = exe.as_deref().and_then(Path::parent);
    cache_path_for(exe_dir, home_dir().as_deref())
}

/// The path choice behind [`default_cache_path`] for an explicit executable directory and
/// home directory (`None` home falls back to the current directory).
pub fn cache_path_for(exe_dir: Option<&Path>, home_dir: Option<&Path>) -> PathBuf {
    if let Some(exe_dir) = exe_dir.filter(|d| d.join("portable.txt").exists()) {
        return exe_dir.join("cache").join("thumbnails.db");
    }
    home_dir
        .unwrap_or(Path::new("."))
        .join(".gemini")
        .join("antigravity-ide")
        .join("thumbnails.db")
}

/// Python's `os.path.expanduser("~")` on Windows: `USERPROFILE`, else `HOMEDRIVE` + `HOMEPATH`.
fn home_dir() -> Option<PathBuf> {
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    let home_path = std::env::var_os("HOMEPATH")?;
    let mut home = PathBuf::from(std::env::var_os("HOMEDRIVE").unwrap_or_default());
    home.push(home_path);
    Some(home)
}

/// Lower-case hex MD5 of the absolute path's UTF-8 bytes (Python `os.path.abspath`).
pub fn path_hash(path: &Path) -> String {
    match python_abspath(path) {
        Some(abs) => md5_hex(&abs),
        None => {
            let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
            md5_hex(&abs.to_string_lossy())
        }
    }
}

/// `os.stat(path).st_mtime` bit-for-bit as CPython computes it on Windows: the last-write
/// FILETIME split into whole seconds and nanoseconds, combined as `sec + nsec * 1e-9`.
pub fn python_mtime(meta: &Metadata) -> f64 {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const TICKS_PER_SEC: u64 = 10_000_000;
        const SECS_1601_TO_1970: i64 = 11_644_473_600;
        let ticks = meta.last_write_time();
        let secs = (ticks / TICKS_PER_SEC) as i64 - SECS_1601_TO_1970;
        let nsec = (ticks % TICKS_PER_SEC) * 100;
        secs as f64 + nsec as f64 * 1e-9
    }
    #[cfg(not(windows))]
    {
        let since = |t: SystemTime| match t.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_secs() as f64 + f64::from(d.subsec_nanos()) * 1e-9,
            Err(e) => -e.duration().as_secs_f64(),
        };
        meta.modified().map_or(0.0, since)
    }
}

/// `os.path.abspath` (`GetFullPathNameW`: backslashes, `.`/`..` resolved, drive letter
/// case kept); `None` if that fails or the result is not valid Unicode.
fn python_abspath(path: &Path) -> Option<String> {
    std::path::absolute(path)
        .ok()?
        .into_os_string()
        .into_string()
        .ok()
}

fn md5_hex(text: &str) -> String {
    Md5::digest(text.as_bytes())
        .iter()
        .fold(String::with_capacity(32), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        })
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::test_util::TempDir;

    #[test]
    fn hashes_match_python_abspath_md5() {
        // Reference values from `hashlib.md5(os.path.abspath(p).encode()).hexdigest()`.
        let cases = [
            (r"C:\models\a.stl", "8078fabbe8ed4ad7de435df5da6bf8b4"),
            (
                r"C:\dev\stl_viewer\models\ascii_cube.stl",
                "5daffd90c04307aee5a52f0849334312",
            ),
            (
                "C:\\Users\\x\\caf\u{e9} \u{4e2d}.stl",
                "2ca3e51808b89fd39717e4efef3860c1",
            ),
            ("C:/a/./b/../c.stl", "d966c9c0c10fecd8f645875b2ba6f76d"),
            ("C:\\a\\\\b\\c. ", "2f45b6c70c21a7f65d9d61d84dda602f"),
            (r"c:\X\y.stl", "6c892cc5fdbde4a65bb7d8c31a3f5f15"),
            (r"\\server\share\m.stl", "540a8474eca3162dd0dfef71ee0f775b"),
        ];
        for (path, want) in cases {
            assert_eq!(path_hash(Path::new(path)), want, "{path}");
        }
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            path_hash(Path::new("rel.stl")),
            path_hash(&cwd.join("rel.stl"))
        );
    }

    #[test]
    fn mtime_matches_cpython_formula() {
        let dir = TempDir::new("mtime");
        let file = dir.path().join("m.stl");
        fs::write(&file, b"x").unwrap();
        // FILETIME has 100 ns resolution, so the trailing 89 ns are dropped.
        let when = UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_789);
        File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(when)
            .unwrap();
        let got = python_mtime(&fs::metadata(&file).unwrap());
        assert_eq!(
            got.to_bits(),
            (1_700_000_000f64 + 123_456_700f64 * 1e-9).to_bits()
        );
    }

    #[test]
    fn default_path_choice() {
        let dir = TempDir::new("portable");
        let home = Path::new(r"C:\Users\someone");
        let normal = cache_path_for(Some(dir.path()), Some(home));
        assert_eq!(normal, home.join(r".gemini\antigravity-ide\thumbnails.db"));
        fs::write(dir.path().join("portable.txt"), b"").unwrap();
        let portable = cache_path_for(Some(dir.path()), Some(home));
        assert_eq!(portable, dir.path().join(r"cache\thumbnails.db"));
        assert_eq!(
            cache_path_for(None, None),
            PathBuf::from(r".\.gemini\antigravity-ide\thumbnails.db")
        );
    }

    #[test]
    fn put_get_and_invalidation() {
        let dir = TempDir::new("cache");
        let cache = ThumbnailCache::open(&dir.path().join(r"nested\thumbs.db")).unwrap();
        let model = dir.path().join("cube.stl");
        fs::write(&model, b"solid cube").unwrap();
        assert!(!cache.is_valid(&model));
        assert_eq!(cache.get_png(&model), None);

        cache.put(&model, b"\x89PNG fake").unwrap();
        assert!(cache.is_valid(&model));
        assert_eq!(cache.get_png(&model).as_deref(), Some(&b"\x89PNG fake"[..]));
        // Same file through a different spelling of its path.
        let spelled = dir.path().join(r".\sub\..\cube.stl");
        assert!(cache.is_valid(&spelled));

        // Replacing keeps one row per path.
        cache.put(&model, b"second").unwrap();
        assert_eq!(cache.get_png(&model).as_deref(), Some(&b"second"[..]));

        // A new mtime invalidates.
        let f = File::options().write(true).open(&model).unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(5))
            .unwrap();
        drop(f);
        assert!(!cache.is_valid(&model));
        cache.put(&model, b"third").unwrap();
        assert!(cache.is_valid(&model));
        // A new size with the same mtime invalidates too.
        let mtime = fs::metadata(&model).unwrap().modified().unwrap();
        fs::write(&model, b"solid cube, longer").unwrap();
        File::options()
            .write(true)
            .open(&model)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        assert!(!cache.is_valid(&model));

        // Missing files and directories are never valid; `put` reports instead of panicking.
        assert!(!cache.is_valid(&dir.path().join("missing.stl")));
        assert!(cache.put(&dir.path().join("missing.stl"), b"x").is_err());
        assert!(!cache.is_valid(dir.path()));
    }

    #[test]
    fn stale_stamp_is_not_fresh() {
        let dir = TempDir::new("stamp");
        let cache = ThumbnailCache::open(&dir.path().join("t.db")).unwrap();
        let model = dir.path().join("a.obj");
        fs::write(&model, b"v 0 0 0").unwrap();
        let mut stamp = FileStamp::of(&fs::metadata(&model).unwrap());
        stamp.size += 1;
        cache.put_stamped(&model, stamp, b"png").unwrap();
        assert!(!cache.is_valid(&model));
        assert!(cache.is_valid_for(&model, stamp));
    }

    #[test]
    fn schema_and_reopen() {
        let dir = TempDir::new("schema");
        let db = dir.path().join("t.db");
        let model = dir.path().join("a.stl");
        fs::write(&model, b"abc").unwrap();
        ThumbnailCache::open(&db)
            .unwrap()
            .put(&model, b"png")
            .unwrap();
        // Reopening keeps the data (CREATE TABLE IF NOT EXISTS).
        let cache = ThumbnailCache::open(&db).unwrap();
        assert!(cache.is_valid(&model));
        let conn = Connection::open(db.to_str().unwrap()).unwrap();
        let mut stmt = conn
            .prepare("SELECT filepath, typeof(mtime), typeof(filesize), typeof(image_data), typeof(created_at) FROM thumbnails")
            .unwrap();
        assert!(stmt.step().unwrap());
        let text = |i| String::from_utf8(stmt.column_blob(i).to_vec()).unwrap();
        assert_eq!(
            text(0),
            std::path::absolute(&model).unwrap().to_str().unwrap()
        );
        assert_eq!(
            [text(1), text(2), text(3), text(4)],
            ["real", "integer", "blob", "real"]
        );
    }

    #[test]
    fn put_many_stores_a_batch() {
        use std::os::windows::ffi::OsStringExt;
        let dir = TempDir::new("batch");
        let cache = ThumbnailCache::open(&dir.path().join("t.db")).unwrap();
        let mut rows = Vec::new();
        for i in 0..5 {
            let model = dir.path().join(format!("m{i}.stl"));
            fs::write(&model, format!("model {i}")).unwrap();
            let stamp = FileStamp::read(&model).unwrap();
            rows.push((model, stamp, format!("png {i}").into_bytes()));
        }
        // A path Python cannot encode (unpaired surrogate) is skipped, not fatal.
        let bad = dir
            .path()
            .join(std::ffi::OsString::from_wide(&[0xD800, u16::from(b'x')]));
        rows.insert(2, (bad, rows[0].1, b"png".to_vec()));
        cache.put_many(&rows).unwrap();
        for (path, _, png) in rows.iter().filter(|r| r.0.to_str().is_some()) {
            assert_eq!(cache.get_png(path).as_ref(), Some(png));
        }
        assert!(cache.put_many(&[]).is_ok());
        cache.close();
        assert!(cache.put_many(&rows).is_err());
    }

    #[test]
    fn in_memory_database() {
        // The app falls back to `:memory:` when the cache file cannot be opened.
        let dir = TempDir::new("memory");
        let cache = ThumbnailCache::open(Path::new(":memory:")).unwrap();
        let model = dir.path().join("a.stl");
        fs::write(&model, b"abc").unwrap();
        cache.put(&model, b"png").unwrap();
        assert_eq!(cache.get_png(&model).as_deref(), Some(&b"png"[..]));
    }

    #[test]
    fn closed_cache_misses() {
        let dir = TempDir::new("closed");
        let cache = ThumbnailCache::open(&dir.path().join("t.db")).unwrap();
        let model = dir.path().join("a.stl");
        fs::write(&model, b"abc").unwrap();
        cache.put(&model, b"png").unwrap();
        cache.close();
        cache.close();
        assert!(!cache.is_valid(&model));
        assert!(cache.get_png(&model).is_none());
        assert!(cache.put(&model, b"png").is_err());
    }

    #[test]
    fn open_errors_do_not_panic() {
        let dir = TempDir::new("openerr");
        // A directory where the database file should be.
        let as_dir = dir.path().join("db");
        fs::create_dir(&as_dir).unwrap();
        assert!(ThumbnailCache::open(&as_dir).is_err());
        // Not an SQLite file.
        let junk = dir.path().join("junk.db");
        fs::write(&junk, vec![0x42; 4096]).unwrap();
        assert!(ThumbnailCache::open(&junk).is_err());
    }

    #[test]
    fn concurrent_use() {
        let dir = TempDir::new("threads");
        let cache = Arc::new(ThumbnailCache::open(&dir.path().join("t.db")).unwrap());
        let files: Vec<PathBuf> = (0..32)
            .map(|i| {
                let p = dir.path().join(format!("m{i}.stl"));
                fs::write(&p, format!("model {i}")).unwrap();
                p
            })
            .collect();
        std::thread::scope(|s| {
            for chunk in files.chunks(8) {
                let cache = &cache;
                s.spawn(move || {
                    for p in chunk {
                        cache.put(p, p.to_str().unwrap().as_bytes()).unwrap();
                        assert!(cache.is_valid(p));
                    }
                });
            }
        });
        for p in &files {
            assert_eq!(cache.get_png(p).unwrap(), p.to_str().unwrap().as_bytes());
        }
    }
}
