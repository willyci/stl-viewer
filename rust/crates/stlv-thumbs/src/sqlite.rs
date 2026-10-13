//! Minimal safe wrapper over the SQLite C API exported by Windows' own `winsqlite3.dll`
//! (no C compiler or bundled SQLite needed). Only what the thumbnail cache uses.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::marker::PhantomData;
use std::ptr::{self, NonNull};

#[repr(C)]
struct RawDb {
    _opaque: [u8; 0],
}

#[repr(C)]
struct RawStmt {
    _opaque: [u8; 0],
}

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_NULL: c_int = 5;
const SQLITE_OPEN_READWRITE: c_int = 0x0000_0002;
const SQLITE_OPEN_CREATE: c_int = 0x0000_0004;
const SQLITE_OPEN_FULLMUTEX: c_int = 0x0001_0000;
/// `SQLITE_TRANSIENT`: SQLite copies bound text/blobs before the bind call returns.
const SQLITE_TRANSIENT: isize = -1;

// `extern "system"` is the plain C ABI on x64 and stdcall on x86, matching winsqlite3's
// `SQLITE_APICALL` on both. Destructor arguments are declared as `isize`: the C type is a
// function pointer, and the only value passed is the pointer-sized sentinel -1.
#[link(name = "winsqlite3", kind = "raw-dylib")]
unsafe extern "system" {
    fn sqlite3_open_v2(
        filename: *const c_char,
        db: *mut *mut RawDb,
        flags: c_int,
        vfs: *const c_char,
    ) -> c_int;
    fn sqlite3_close_v2(db: *mut RawDb) -> c_int;
    fn sqlite3_busy_timeout(db: *mut RawDb, ms: c_int) -> c_int;
    fn sqlite3_errmsg(db: *mut RawDb) -> *const c_char;
    fn sqlite3_errstr(code: c_int) -> *const c_char;
    fn sqlite3_prepare_v2(
        db: *mut RawDb,
        sql: *const c_char,
        n_bytes: c_int,
        stmt: *mut *mut RawStmt,
        tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_finalize(stmt: *mut RawStmt) -> c_int;
    fn sqlite3_step(stmt: *mut RawStmt) -> c_int;
    fn sqlite3_bind_text(
        stmt: *mut RawStmt,
        index: c_int,
        text: *const c_char,
        n_bytes: c_int,
        destructor: isize,
    ) -> c_int;
    fn sqlite3_bind_blob(
        stmt: *mut RawStmt,
        index: c_int,
        data: *const c_void,
        n_bytes: c_int,
        destructor: isize,
    ) -> c_int;
    fn sqlite3_bind_double(stmt: *mut RawStmt, index: c_int, value: f64) -> c_int;
    fn sqlite3_bind_int64(stmt: *mut RawStmt, index: c_int, value: i64) -> c_int;
    fn sqlite3_column_type(stmt: *mut RawStmt, col: c_int) -> c_int;
    fn sqlite3_column_double(stmt: *mut RawStmt, col: c_int) -> f64;
    fn sqlite3_column_int64(stmt: *mut RawStmt, col: c_int) -> i64;
    fn sqlite3_column_blob(stmt: *mut RawStmt, col: c_int) -> *const c_void;
    fn sqlite3_column_bytes(stmt: *mut RawStmt, col: c_int) -> c_int;
}

/// A value bound to a `?` parameter.
#[derive(Clone, Copy, Debug)]
pub enum Value<'a> {
    Text(&'a str),
    Blob(&'a [u8]),
    Real(f64),
    Int(i64),
}

/// An open database connection.
pub struct Connection {
    db: NonNull<RawDb>,
}

// SAFETY: the connection is opened with SQLITE_OPEN_FULLMUTEX and winsqlite3 is built
// thread-safe (`sqlite3_threadsafe() == 1`), so the handle may be used from any thread.
// `Connection` is deliberately not `Sync`; callers share it behind a `Mutex`.
unsafe impl Send for Connection {}

impl Connection {
    /// Open (creating if needed) the database file at the UTF-8 path `path`.
    pub fn open(path: &str) -> Result<Self, String> {
        let c_path = CString::new(path).map_err(|_| format!("invalid database path: {path:?}"))?;
        let mut db: *mut RawDb = ptr::null_mut();
        let flags = SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_FULLMUTEX;
        // SAFETY: `c_path` is NUL-terminated and outlives the call; `db` is a valid out-pointer.
        let rc = unsafe { sqlite3_open_v2(c_path.as_ptr(), &mut db, flags, ptr::null()) };
        let Some(db) = NonNull::new(db) else {
            return Err(format!("cannot open {path}: {}", errstr(rc)));
        };
        // Owning the handle first guarantees it is closed even when opening failed.
        let conn = Self { db };
        if rc != SQLITE_OK {
            return Err(format!("cannot open {path}: {}", conn.errmsg()));
        }
        Ok(conn)
    }

    /// Wait up to `ms` milliseconds for locks held by other connections (e.g. the Python app).
    pub fn busy_timeout(&self, ms: i32) -> Result<(), String> {
        // SAFETY: `self.db` is a live connection handle.
        let rc = unsafe { sqlite3_busy_timeout(self.db.as_ptr(), ms) };
        self.check(rc)
    }

    /// Run a statement that returns no rows.
    pub fn execute(&self, sql: &str, params: &[Value<'_>]) -> Result<(), String> {
        let mut stmt = self.prepare(sql)?;
        stmt.bind_all(params)?;
        while stmt.step()? {}
        Ok(())
    }

    pub fn prepare(&self, sql: &str) -> Result<Statement<'_>, String> {
        let len = c_int::try_from(sql.len()).map_err(|_| "SQL text too long".to_string())?;
        let mut stmt: *mut RawStmt = ptr::null_mut();
        // SAFETY: `sql` is valid for `len` bytes (no NUL terminator needed when the length
        // is given); `stmt` is a valid out-pointer; `tail` may be null.
        let rc = unsafe {
            sqlite3_prepare_v2(
                self.db.as_ptr(),
                sql.as_ptr().cast(),
                len,
                &mut stmt,
                ptr::null_mut(),
            )
        };
        self.check(rc)?;
        let stmt = NonNull::new(stmt).ok_or_else(|| "empty SQL statement".to_string())?;
        Ok(Statement {
            stmt,
            conn: self,
            _not_send: PhantomData,
        })
    }

    fn check(&self, rc: c_int) -> Result<(), String> {
        if rc == SQLITE_OK {
            Ok(())
        } else {
            Err(self.errmsg())
        }
    }

    fn errmsg(&self) -> String {
        // SAFETY: `self.db` is live; SQLite returns a NUL-terminated string owned by the
        // connection, which we copy before any further call can invalidate it.
        unsafe { cstr_to_string(sqlite3_errmsg(self.db.as_ptr())) }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // SAFETY: every `Statement` borrows the connection, so none are alive here;
        // `close_v2` releases the handle exactly once.
        unsafe { sqlite3_close_v2(self.db.as_ptr()) };
    }
}

/// A prepared statement; finalized on drop.
pub struct Statement<'c> {
    stmt: NonNull<RawStmt>,
    conn: &'c Connection,
    _not_send: PhantomData<*mut RawStmt>,
}

impl Statement<'_> {
    /// Bind `params` to parameters 1..=n.
    pub fn bind_all(&mut self, params: &[Value<'_>]) -> Result<(), String> {
        for (i, value) in params.iter().enumerate() {
            let index = c_int::try_from(i + 1).map_err(|_| "too many parameters".to_string())?;
            self.bind(index, *value)?;
        }
        Ok(())
    }

    pub fn bind(&mut self, index: c_int, value: Value<'_>) -> Result<(), String> {
        let s = self.stmt.as_ptr();
        // SAFETY: `s` is a live statement. Text/blob pointers are valid for the given
        // lengths and SQLITE_TRANSIENT makes SQLite copy them before returning. An empty
        // slice still has a non-null (dangling) pointer, so it binds an empty value, not NULL.
        let rc = unsafe {
            match value {
                Value::Text(t) => sqlite3_bind_text(
                    s,
                    index,
                    t.as_ptr().cast(),
                    len_c(t.len())?,
                    SQLITE_TRANSIENT,
                ),
                Value::Blob(b) => sqlite3_bind_blob(
                    s,
                    index,
                    b.as_ptr().cast(),
                    len_c(b.len())?,
                    SQLITE_TRANSIENT,
                ),
                Value::Real(v) => sqlite3_bind_double(s, index, v),
                Value::Int(v) => sqlite3_bind_int64(s, index, v),
            }
        };
        self.conn.check(rc)
    }

    /// Advance; `Ok(true)` when a row is available.
    pub fn step(&mut self) -> Result<bool, String> {
        // SAFETY: `self.stmt` is a live statement.
        match unsafe { sqlite3_step(self.stmt.as_ptr()) } {
            SQLITE_ROW => Ok(true),
            SQLITE_DONE => Ok(false),
            _ => Err(self.conn.errmsg()),
        }
    }

    pub fn column_is_null(&self, col: c_int) -> bool {
        // SAFETY: valid statement; out-of-range columns are defined to report NULL.
        unsafe { sqlite3_column_type(self.stmt.as_ptr(), col) == SQLITE_NULL }
    }

    pub fn column_f64(&self, col: c_int) -> f64 {
        // SAFETY: valid statement; out-of-range columns are defined to return 0.0.
        unsafe { sqlite3_column_double(self.stmt.as_ptr(), col) }
    }

    pub fn column_i64(&self, col: c_int) -> i64 {
        // SAFETY: valid statement; out-of-range columns are defined to return 0.
        unsafe { sqlite3_column_int64(self.stmt.as_ptr(), col) }
    }

    /// The column's bytes; valid until the next `step` (enforced by the borrow).
    pub fn column_blob(&self, col: c_int) -> &[u8] {
        let s = self.stmt.as_ptr();
        // SAFETY: valid statement. `column_blob` must be called before `column_bytes`
        // (SQLite's documented order); the pointer stays valid until the next step,
        // reset or finalize, all of which need `&mut self` or drop.
        unsafe {
            let data = sqlite3_column_blob(s, col);
            let len = usize::try_from(sqlite3_column_bytes(s, col)).unwrap_or(0);
            if data.is_null() || len == 0 {
                &[]
            } else {
                std::slice::from_raw_parts(data.cast::<u8>(), len)
            }
        }
    }
}

impl Drop for Statement<'_> {
    fn drop(&mut self) {
        // SAFETY: the statement is live and finalized exactly once.
        unsafe { sqlite3_finalize(self.stmt.as_ptr()) };
    }
}

fn len_c(len: usize) -> Result<c_int, String> {
    c_int::try_from(len).map_err(|_| "value too large for SQLite".to_string())
}

fn errstr(rc: c_int) -> String {
    // SAFETY: `sqlite3_errstr` returns a static NUL-terminated string for any code.
    unsafe { cstr_to_string(sqlite3_errstr(rc)) }
}

/// # Safety
/// `p` must be null or point to a NUL-terminated string.
unsafe fn cstr_to_string(p: *const c_char) -> String {
    if p.is_null() {
        return "unknown SQLite error".into();
    }
    // SAFETY: guaranteed by the caller.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_roundtrip() {
        let conn = Connection::open(":memory:").unwrap();
        conn.busy_timeout(100).unwrap();
        conn.execute("CREATE TABLE t (a TEXT, b BLOB, c REAL, d INTEGER)", &[])
            .unwrap();
        let values = [
            Value::Text("h\u{e9}llo"),
            Value::Blob(&[]),
            Value::Real(1.5e9 + 0.123_456_7),
            Value::Int(-7),
        ];
        conn.execute("INSERT INTO t VALUES (?, ?, ?, ?)", &values)
            .unwrap();
        let mut stmt = conn.prepare("SELECT a, b, c, d, b IS NULL FROM t").unwrap();
        assert!(stmt.step().unwrap());
        assert_eq!(stmt.column_blob(0), "h\u{e9}llo".as_bytes());
        assert_eq!(stmt.column_blob(1), b"");
        assert!(
            !stmt.column_is_null(1),
            "an empty blob must not bind as NULL"
        );
        assert_eq!(
            stmt.column_f64(2).to_bits(),
            (1.5e9 + 0.123_456_7f64).to_bits()
        );
        assert_eq!(stmt.column_i64(3), -7);
        assert_eq!(stmt.column_i64(4), 0);
        assert!(!stmt.step().unwrap());
    }

    #[test]
    fn errors_are_reported_not_panics() {
        let conn = Connection::open(":memory:").unwrap();
        let err = conn.execute("SELEC nonsense", &[]).err().unwrap();
        assert!(err.contains("syntax error"), "{err}");
        assert!(conn.prepare("   ").is_err());
        conn.execute("CREATE TABLE t (a TEXT NOT NULL)", &[])
            .unwrap();
        assert!(conn.execute("INSERT INTO t VALUES (NULL)", &[]).is_err());
        assert!(Connection::open("C:\\stlv-definitely-missing-dir\\sub\\x.db").is_err());
    }
}
