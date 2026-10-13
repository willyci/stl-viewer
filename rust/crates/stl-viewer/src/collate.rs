//! File-name order of the Qt tree: `QFileSystemModel` sorts names with a `QCollator` in
//! numeric, case-insensitive mode, which on Windows is `CompareStringEx` with
//! `NORM_IGNORECASE | SORT_DIGITSASNUMBERS`. The equivalent `LCMapStringEx` sort key is
//! used here, so sorting compares bytes and is always a total order.

/// Natural, case-insensitive sort key of a file name: digit runs compare by value
/// (`file2` < `file10`), punctuation before digits before letters.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SortKey {
    /// The system collation's key (Windows).
    System(Vec<u8>),
    /// A plain natural order where the system collation is unavailable.
    Fallback(Vec<Token>),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Token {
    /// A digit run without leading zeros, ordered by length and then digits (= by value).
    Number(usize, String),
    Char(char),
}

pub fn sort_key(name: &str) -> SortKey {
    #[cfg(windows)]
    if let Some(key) = system_key(name) {
        return SortKey::System(key);
    }
    SortKey::Fallback(natural_tokens(name))
}

fn natural_tokens(name: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut chars = name.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            let mut digits = String::from(c);
            while let Some(d) = chars.next_if(char::is_ascii_digit) {
                digits.push(d);
            }
            let value = digits.trim_start_matches('0');
            out.push(Token::Number(value.len(), value.to_string()));
        } else {
            out.extend(c.to_lowercase().map(Token::Char));
        }
    }
    out
}

#[cfg(windows)]
fn system_key(name: &str) -> Option<Vec<u8>> {
    use std::ffi::c_void;
    use std::ptr::{null, null_mut};

    const LCMAP_SORTKEY: u32 = 0x400;
    const NORM_IGNORECASE: u32 = 0x1;
    const SORT_DIGITSASNUMBERS: u32 = 0x8;

    #[link(name = "kernel32", kind = "raw-dylib")]
    unsafe extern "system" {
        fn LCMapStringEx(
            locale: *const u16,
            flags: u32,
            src: *const u16,
            src_len: i32,
            dest: *mut u8,
            dest_len: i32,
            version: *mut c_void,
            reserved: *mut c_void,
            sort_handle: isize,
        ) -> i32;
    }

    let wide: Vec<u16> = name.encode_utf16().collect();
    if wide.is_empty() {
        return Some(Vec::new());
    }
    let len = i32::try_from(wide.len()).ok()?;
    let flags = LCMAP_SORTKEY | NORM_IGNORECASE | SORT_DIGITSASNUMBERS;
    // SAFETY: `wide` holds `len` UTF-16 units and a null locale means the user default;
    // without a destination the call only returns the key's size in bytes.
    let size = unsafe {
        LCMapStringEx(
            null(),
            flags,
            wide.as_ptr(),
            len,
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            0,
        )
    };
    let mut key = vec![0u8; usize::try_from(size).ok().filter(|&n| n > 0)?];
    // SAFETY: as above; with LCMAP_SORTKEY the destination is a byte buffer of `size` bytes.
    let written = unsafe {
        LCMapStringEx(
            null(),
            flags,
            wide.as_ptr(),
            len,
            key.as_mut_ptr(),
            size,
            null_mut(),
            null_mut(),
            0,
        )
    };
    key.truncate(usize::try_from(written).ok().filter(|&n| n > 0)?);
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(names: &[&str], key: impl Fn(&str) -> SortKey) -> Vec<String> {
        let mut v: Vec<String> = names.iter().map(|s| (*s).to_string()).collect();
        v.sort_by_cached_key(|n| key(n));
        v
    }

    const FILES: [&str; 7] = [
        "Z.obj",
        "3110_egg.stl",
        "b.stl",
        "36-54mm.stl",
        "3DBenchy.stl",
        "3_Tier.stl",
        "3.stl",
    ];

    /// The order the running Python app showed (QFileSystemModel on Windows).
    #[cfg(windows)]
    #[test]
    fn matches_qt_on_windows() {
        let files = sorted(&FILES, sort_key);
        assert_eq!(
            files,
            [
                "3.stl",
                "3_Tier.stl",
                "3DBenchy.stl",
                "36-54mm.stl",
                "3110_egg.stl",
                "b.stl",
                "Z.obj"
            ]
        );
        let dirs = sorted(
            &[
                "Y folder",
                "a_folder",
                "30 mm base",
                "3d printed",
                "3D DELOREAN",
            ],
            sort_key,
        );
        assert_eq!(
            dirs,
            [
                "3D DELOREAN",
                "3d printed",
                "30 mm base",
                "a_folder",
                "Y folder"
            ]
        );
        let root = sorted(
            &["rust", "models", "__pycache__", "dist", "build"],
            sort_key,
        );
        assert_eq!(root, ["__pycache__", "build", "dist", "models", "rust"]);
        assert!(sort_key("file2") < sort_key("File10"));
        assert_eq!(sort_key(""), sort_key(""));
    }

    #[test]
    fn fallback_is_natural() {
        let fallback = |n: &str| SortKey::Fallback(natural_tokens(n));
        assert_eq!(
            sorted(
                &["file10", "File2", "file02b", "file1", "a", "_x"],
                fallback
            ),
            ["_x", "a", "file1", "File2", "file02b", "file10"]
        );
        assert_eq!(
            sorted(&FILES, fallback),
            [
                "3.stl",
                "3_Tier.stl",
                "3DBenchy.stl",
                "36-54mm.stl",
                "3110_egg.stl",
                "b.stl",
                "Z.obj"
            ]
        );
    }
}
