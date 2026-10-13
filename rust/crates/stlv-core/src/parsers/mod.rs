//! STL / 3MF / OBJ / G-code parsers (port of parsers.py).

mod gcode;
mod obj;
mod stl;
mod threemf;

use std::path::Path;
use std::time::Instant;

use crate::model::ModelData;

pub use gcode::{parse_gcode, parse_gcode_bytes};
pub use obj::{parse_obj, parse_obj_bytes};
pub use stl::{parse_stl, parse_stl_bytes};
pub use threemf::{parse_3mf, parse_3mf_bytes};

#[derive(Debug)]
pub enum LoadError {
    Io(std::io::Error),
    /// Malformed or empty file; the message is shown to the user verbatim.
    Invalid(String),
    /// Extension not handled; holds the lower-cased extension including the dot.
    Unsupported(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Invalid(msg) => f.write_str(msg),
            Self::Unsupported(ext) => write!(f, "Unsupported file format: {ext}"),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<std::io::Error> for LoadError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl LoadError {
    fn invalid(msg: impl Into<String>) -> Self {
        Self::Invalid(msg.into())
    }
}

/// Load any supported 3D file, dispatching on its extension; fills `load_time_ms`.
pub fn load_model(path: &Path) -> Result<ModelData, LoadError> {
    let start = Instant::now();
    // Like Python's `os.path.splitext(path)[1].lower()`: "" without an extension, "." for "a.".
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    let mut model = match ext.as_str() {
        ".stl" => parse_stl(path)?,
        ".3mf" => parse_3mf(path)?,
        ".obj" => parse_obj(path)?,
        ".gcode" | ".gco" => parse_gcode(path)?,
        _ => return Err(LoadError::Unsupported(ext)),
    };
    model.load_time_ms = start.elapsed().as_secs_f64() * 1000.0;
    Ok(model)
}

/// File basename, as `os.path.basename`.
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Byte-level helpers that reproduce how the Python parsers read text files: `open(..., 'r',
/// errors='ignore')` with universal newlines, `str.strip()` / `str.split()`, `float()`, `int()`.
/// Files are handled as bytes (the Python app decodes them as cp1252, one char per byte).
pub(crate) mod text {
    /// Characters Python's `str.split()` / `str.strip()` treat as whitespace (cp1252 decoding).
    pub fn is_space(b: u8) -> bool {
        matches!(
            b,
            b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C | 0x1C..=0x1F | 0xA0
        )
    }

    /// Lines split at `\n`, `\r` or `\r\n` (universal newlines); empty lines may appear.
    pub fn lines(data: &[u8]) -> impl Iterator<Item = &[u8]> {
        data.split(|&b| b == b'\n' || b == b'\r')
    }

    pub fn trim(s: &[u8]) -> &[u8] {
        let start = s.iter().position(|&b| !is_space(b)).unwrap_or(s.len());
        let end = s
            .iter()
            .rposition(|&b| !is_space(b))
            .map_or(start, |i| i + 1);
        &s[start..end]
    }

    /// Whitespace-separated tokens (`str.split()`).
    pub fn tokens(s: &[u8]) -> impl Iterator<Item = &[u8]> {
        s.split(|&b| is_space(b)).filter(|t| !t.is_empty())
    }

    /// Python `float(token)`; `None` where Python raises `ValueError`.
    pub fn parse_float(token: &[u8]) -> Option<f64> {
        std::str::from_utf8(token).ok()?.parse().ok()
    }

    /// Python `float(x)` narrowed to the f32 numpy array the parsers build (double rounding,
    /// exactly like `np.array([...], dtype=np.float32)`).
    pub fn parse_f32(token: &[u8]) -> Option<f32> {
        parse_float(token).map(|x| x as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::text::*;
    use super::*;

    #[test]
    fn python_like_text_handling() {
        let data = b"a\r\nb\rc\n\nd";
        let lines: Vec<&[u8]> = lines(data).filter(|l| !l.is_empty()).collect();
        assert_eq!(lines, [b"a", b"b", b"c", b"d"]);
        assert_eq!(trim(b" \t\x0bx y\x0c "), b"x y");
        assert_eq!(trim(b"   "), b"");
        let toks: Vec<&[u8]> = tokens(b"  v 1\t2\xA03 ").collect();
        assert_eq!(toks, [&b"v"[..], b"1", b"2", b"3"]);
        assert_eq!(parse_float(b"1e3"), Some(1000.0));
        assert_eq!(parse_float(b"-.5"), Some(-0.5));
        assert_eq!(parse_float(b"+2."), Some(2.0));
        assert!(parse_float(b"inf").unwrap().is_infinite());
        assert!(parse_float(b"NaN").unwrap().is_nan());
        assert_eq!(parse_float(b"1,5"), None);
        assert_eq!(parse_float(b""), None);
        assert_eq!(parse_float(b"\xff"), None);
    }

    #[test]
    fn unsupported_extensions() {
        let msg = |p: &str| load_model(Path::new(p)).unwrap_err().to_string();
        assert_eq!(msg("model.PLY"), "Unsupported file format: .ply");
        assert_eq!(msg("noext"), "Unsupported file format: ");
        assert_eq!(msg(".stl"), "Unsupported file format: ");
        assert_eq!(msg("trailing."), "Unsupported file format: .");
    }
}
