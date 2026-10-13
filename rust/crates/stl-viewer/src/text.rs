//! User-visible strings (status bar, overlay, dialogs) and pure text helpers.
//!
//! Every string here must match the Python app verbatim; `—` is U+2014 and the
//! separators around it are two spaces wide.

use std::path::Path;

use stlv_core::ModelData;
use stlv_core::format::{format_file_size, thousands, thousands_f};

pub const READY: &str = "Ready. Select a file in the browser tree to preview.";
pub const MODEL_LOAD_FAILED: &str = "Error loading model.";
pub const IMAGE_LOAD_FAILED: &str = "Error: Failed to load image file.";
pub const MEDIA_ENABLED: &str = "Image & Media files enabled in browser.";
pub const MEDIA_FILTERED: &str = "Image & Media files filtered out.";
pub const MEDIA_CLEARED: &str = "Active media cleared.";
pub const LOADING_ERROR_TITLE: &str = "Loading Error";
pub const COLOR_DIALOG_TITLE: &str = "Select Model Color";
pub const BROWSE_DIALOG_TITLE: &str = "Select Directory to Browse";
pub const PATH_PLACEHOLDER: &str = "Enter folder path...";
pub const UP_TOOLTIP: &str = "Go up one folder level";
pub const WINDOW_TITLE: &str = "STL/3MF Viewer (3MF, OBJ, STL, G-Code, images)";

const SEP: &str = "  \u{2014}  ";

/// File name of `path`, or the whole path when it has none (e.g. a drive root).
pub fn basename(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// `Reading: name  (1.5 KB)`, or without the size when the file could not be stat'ed.
pub fn reading_model(basename: &str, size: Option<u64>) -> String {
    match size {
        Some(s) => format!("Reading: {basename}  ({})", format_file_size(s)),
        None => format!("Reading: {basename}"),
    }
}

pub fn reading_media(basename: &str) -> String {
    format!("Reading: {basename} ...")
}

pub fn parsing(basename: &str) -> String {
    format!("Parsing {basename} ...")
}

pub fn loaded(model: &ModelData) -> String {
    let ms = format_ms(model.load_time_ms);
    if model.is_toolpath() {
        format!(
            "Loaded: {}{SEP}{}{SEP}{} lines{SEP}{ms} ms",
            model.filename,
            model.format,
            thousands(model.num_lines() as u64)
        )
    } else {
        format!(
            "Loaded: {}{SEP}{}{SEP}{} vertices, {} triangles{SEP}{ms} ms",
            model.filename,
            model.format,
            thousands(model.num_vertices() as u64),
            thousands(model.num_triangles() as u64)
        )
    }
}

/// Python `f"{ms:.0f}"`: round half to even, never "-0".
fn format_ms(ms: f64) -> String {
    if !ms.is_finite() {
        return "0".into();
    }
    let r = ms.round_ties_even();
    if r == 0.0 {
        "0".into()
    } else {
        format!("{r:.0}")
    }
}

pub fn load_error(err: &str) -> String {
    format!("Failed to load the 3D model:\n{err}")
}

pub fn folder(name: &str, files: usize, total_bytes: u64) -> String {
    format!(
        "Folder: {name}{SEP}{files} files{SEP}{}",
        format_file_size(total_bytes)
    )
}

pub fn folder_loaded(total: usize) -> String {
    format!("Folder loaded: {total} files")
}

pub fn dir_error(err: &std::io::Error) -> String {
    format!("Error reading directory: {err}")
}

pub fn generating(n: usize) -> String {
    format!("Generating {n} thumbnails (files > 100 MB skipped) ...")
}

pub fn rendering(index: usize, total: usize, name: &str) -> String {
    format!("Rendering thumbnail {index} / {total}{SEP}{name}")
}

pub fn all_ready(total: usize, files_in_folder: usize) -> String {
    format!("All {total} thumbnails ready{SEP}{files_in_folder} files in folder")
}

pub fn progress(done: usize, total: usize) -> String {
    format!("Generating thumbnails: {done} / {total}")
}

pub fn image(basename: &str, ext_upper: &str, w: u32, h: u32, size: &str) -> String {
    format!("Image: {basename}{SEP}{ext_upper}{SEP}{w} \u{d7} {h} px{SEP}{size}")
}

/// `dims` is `None` when the first frame could not be decoded (Python printed "Unknown").
pub fn gif(basename: &str, dims: Option<(u32, u32)>, size: &str) -> String {
    let (w, h) = dims_or_unknown(dims);
    format!("GIF: {basename}{SEP}Animated{SEP}{w} \u{d7} {h} px{SEP}{size}")
}

pub fn svg(basename: &str, w: u32, h: u32, size: &str) -> String {
    format!("SVG: {basename}{SEP}Vector{SEP}{w} \u{d7} {h} px{SEP}{size}")
}

pub fn video(basename: &str, size: &str) -> String {
    format!("Video: {basename}{SEP}WebM{SEP}{size}")
}

pub fn video_failed(err: &str) -> String {
    format!("Error: Failed to open video file: {err}")
}

pub fn navigated(path: &str) -> String {
    format!("Navigated to: {path}")
}

pub fn invalid_path(path: &str) -> String {
    format!("Invalid path: {path}")
}

pub fn browsing(path: &str) -> String {
    format!("Browsing directory: {path}")
}

pub fn path_copied(path: &str) -> String {
    format!("Path copied to clipboard: {path}")
}

/// Whole mebibytes as `100 MB` (for size limits).
pub fn megabytes(bytes: u64) -> String {
    format!("{} MB", bytes / (1024 * 1024))
}

pub fn dims_or_unknown(dims: Option<(u32, u32)>) -> (String, String) {
    match dims {
        Some((w, h)) => (w.to_string(), h.to_string()),
        None => ("Unknown".into(), "Unknown".into()),
    }
}

/// `{x:.1f} x {y:.1f} x {z:.1f} mm`
pub fn dimensions_mm(size: [f64; 3]) -> String {
    format!("{:.1} x {:.1} x {:.1} mm", size[0], size[1], size[2])
}

/// `{vol/1000:,.2f} cm³ ({vol:,.1f} mm³)`
pub fn volume(mm3: f64) -> String {
    format!(
        "{} cm\u{b3} ({} mm\u{b3})",
        thousands_f(mm3 / 1000.0, 2),
        thousands_f(mm3, 1)
    )
}

/// Upper-cased extension without the dot (`JPG`, `PNG`, `TIF`, ...).
pub fn ext_upper(path: &Path) -> String {
    stlv_core::files::ext_lower(path).to_uppercase()
}

/// Port of Python's `ntpath.normpath`: `/` → `\`, collapse `.`/`..`/empty components.
/// Never touches the file system and keeps the drive letter's case.
pub fn normpath(path: &str) -> String {
    let path = path.replace('/', "\\");
    let (prefix, rest) = split_root(&path);
    let has_root = prefix.ends_with('\\');
    let mut comps: Vec<&str> = Vec::new();
    for c in rest.split('\\') {
        match c {
            "" | "." => {}
            ".." => match comps.last() {
                Some(&last) if last != ".." => {
                    comps.pop();
                }
                None if has_root => {}
                _ => comps.push(".."),
            },
            _ => comps.push(c),
        }
    }
    if prefix.is_empty() && comps.is_empty() {
        return ".".into();
    }
    let mut out = prefix.to_string();
    out.push_str(&comps.join("\\"));
    out
}

/// Split into (drive + root, remainder): `C:\`, `C:`, `\\server\share\`, `\`.
fn split_root(path: &str) -> (&str, &str) {
    let b = path.as_bytes();
    if b.starts_with(b"\\\\") {
        // UNC: \\server\share\rest (also \\?\ and \\.\ device paths).
        let mut idx = 2;
        for _ in 0..2 {
            match path[idx..].find('\\') {
                Some(i) => idx += i + 1,
                None => return (path, ""),
            }
        }
        return (&path[..idx], &path[idx..]);
    }
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        let end = if b.get(2) == Some(&b'\\') { 3 } else { 2 };
        return (&path[..end], &path[end..]);
    }
    if b.first() == Some(&b'\\') {
        return (&path[..1], &path[1..]);
    }
    ("", path)
}

/// Qt `ElideLeft` (`QTextEngine::elidedText`): `…` followed by the longest suffix whose
/// width stays below `max_w` minus the ellipsis.
pub fn elide_left(text: &str, max_w: f32, mut char_w: impl FnMut(char) -> f32) -> String {
    let chars: Vec<char> = text.chars().collect();
    let widths: Vec<f32> = chars.iter().map(|&c| char_w(c)).collect();
    if chars.is_empty() || widths.iter().sum::<f32>() <= max_w {
        return text.to_string();
    }
    let avail = max_w - char_w('\u{2026}');
    let (mut next, mut used) = (chars.len(), 0.0);
    let pos = loop {
        let pos = next;
        next -= 1;
        used += widths[next];
        if !(next > 0 && used < avail) {
            break pos;
        }
    };
    if next == 0 {
        return text.to_string();
    }
    std::iter::once('\u{2026}')
        .chain(chars[pos..].iter().copied())
        .collect()
}

/// Qt `ElideMiddle`: as many characters from each end (the same number) as fit, with `…`
/// in between.
pub fn elide_middle(text: &str, max_w: f32, mut char_w: impl FnMut(char) -> f32) -> String {
    let chars: Vec<char> = text.chars().collect();
    let widths: Vec<f32> = chars.iter().map(|&c| char_w(c)).collect();
    if chars.is_empty() || widths.iter().sum::<f32>() <= max_w {
        return text.to_string();
    }
    let avail = max_w - char_w('\u{2026}');
    let n = chars.len();
    let (mut next_left, mut next_right) = (0, n);
    let (mut left_w, mut right_w) = (0.0, 0.0);
    let (left, right) = loop {
        let (left, right) = (next_left, next_right);
        next_left += 1;
        next_right -= 1;
        left_w += widths[next_left - 1];
        right_w += widths[next_right];
        if !(next_left < next_right && left_w + right_w < avail) {
            break (left, right);
        }
    };
    if next_left > next_right {
        return text.to_string();
    }
    let mut out: String = chars[..left].iter().collect();
    out.push('\u{2026}');
    out.extend(&chars[right..]);
    out
}

// ---------------------------------------------------------------------------------------------
// Word wrapping like a Qt `QLabel` with `setWordWrap(True)`

/// Byte offsets where wrappable segments end, a simplified UAX #14 as Qt applies it: after
/// a run of spaces, after a hyphen not followed by a digit, after an ellipsis and between a
/// colon and a backslash (`C:` | `\dev`, line break classes IS and PR). Other path
/// separators are not break points. The last offset is always `text.len()`.
pub fn break_points(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let next = chars.peek().map(|&(_, n)| n);
        let brk = match c {
            ' ' => next.is_some_and(|n| n != ' '),
            '-' => next.is_some_and(|n| !n.is_ascii_digit() && n != ' ' && n != '-'),
            '\u{2026}' => next.is_some_and(|n| n != '\u{2026}' && n != ' '),
            ':' => next == Some('\\'),
            _ => false,
        };
        if brk {
            out.push(i + c.len_utf8());
        }
    }
    out.push(text.len());
    out
}

/// Greedy word wrap into lines at most `width` wide; a word wider than the line overflows
/// on its own line (Qt `WordWrap`). Trailing spaces are dropped. `measure` returns the
/// advance width of a string.
pub fn wrap_lines(text: &str, width: f32, measure: &mut impl FnMut(&str) -> f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut fit_end: Option<usize> = None;
    for end in break_points(text) {
        if fit_end.is_none() || measure(text[start..end].trim_end()) <= width {
            fit_end = Some(end);
            continue;
        }
        let line_end = fit_end.unwrap_or(end);
        lines.push(text[start..line_end].trim_end().to_string());
        start = line_end;
        fit_end = Some(end);
    }
    lines.push(text[start..].trim_end().to_string());
    lines
}

/// Widest line and line count of `text` wrapped at `width`.
pub fn wrap_extent(text: &str, width: f32, measure: &mut impl FnMut(&str) -> f32) -> (f32, usize) {
    let lines = wrap_lines(text, width, measure);
    let widest = lines.iter().map(|l| measure(l)).fold(0.0, f32::max);
    (widest, lines.len())
}

/// The width a word-wrapped `QLabel` asks for (`QLabelPrivate::sizeForWidth(-1)`): lay the
/// text out 80 average characters wide, then at half and a quarter of that while it stays
/// short. This is why short values wrap in the Qt metadata panel even with room to spare.
pub fn qt_wrap_hint(text: &str, avg_char_w: f32, measure: &mut impl FnMut(&str) -> f32) -> f32 {
    let w = 80.0 * avg_char_w;
    let (mut widest, mut lines) = wrap_extent(text, w, measure);
    if lines < 4 && widest > w / 2.0 {
        (widest, lines) = wrap_extent(text, w / 2.0, measure);
    }
    if lines < 2 && widest > w / 4.0 {
        (widest, _) = wrap_extent(text, w / 4.0, measure);
    }
    widest
}

/// The narrowest a word-wrapped label can get: its widest unbreakable segment.
pub fn qt_wrap_min(text: &str, measure: &mut impl FnMut(&str) -> f32) -> f32 {
    let mut start = 0;
    let mut widest = 0.0f32;
    for end in break_points(text) {
        widest = widest.max(measure(text[start..end].trim_end()));
        start = end;
    }
    widest
}

/// Widths of a two-column `QGridLayout` (no stretch factors) sharing `avail` (spacing
/// excluded): fixed-size labels on the left, word-wrapped values on the right. Spare room is
/// split evenly unless one column's size hint needs more (`qGeomCalc`); when the hints do not
/// fit, the values shrink.
pub fn qt_two_columns(avail: f32, label_hint: f32, value_hint: f32) -> (f32, f32) {
    let avail = avail.max(0.0);
    if label_hint + value_hint > avail {
        return (label_hint, (avail - label_hint).max(0.0));
    }
    let half = avail / 2.0;
    if label_hint > half {
        (label_hint, avail - label_hint)
    } else if value_hint > half {
        (avail - value_hint, value_hint)
    } else {
        (half, avail - half)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn model(lines: bool) -> ModelData {
        let mut m = if lines {
            ModelData::new(
                vec![[0.0; 3], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [2.0, 1.0, 0.0]],
                vec![],
                vec![[0, 1], [2, 3]],
                "G-Code Path",
                "spiral_path.gcode",
            )
        } else {
            let v = (0..1234).map(|i| [i as f32, 0.0, 0.0]).collect();
            let t = (0..2000u32)
                .map(|i| [i % 1234, (i + 1) % 1234, (i + 2) % 1234])
                .collect();
            ModelData::new(v, t, vec![], "STL (ASCII)", "ascii_cube.stl")
        };
        m.load_time_ms = 12.5;
        m
    }

    #[test]
    fn status_strings() {
        assert_eq!(
            loaded(&model(false)),
            "Loaded: ascii_cube.stl  \u{2014}  STL (ASCII)  \u{2014}  1,234 vertices, 2,000 triangles  \u{2014}  12 ms"
        );
        assert_eq!(
            loaded(&model(true)),
            "Loaded: spiral_path.gcode  \u{2014}  G-Code Path  \u{2014}  2 lines  \u{2014}  12 ms"
        );
        assert_eq!(
            reading_model("a.stl", Some(2691)),
            "Reading: a.stl  (2.6 KB)"
        );
        assert_eq!(reading_model("a.stl", None), "Reading: a.stl");
        assert_eq!(reading_media("p.png"), "Reading: p.png ...");
        assert_eq!(
            folder("models", 9, 2_400_000),
            "Folder: models  \u{2014}  9 files  \u{2014}  2.29 MB"
        );
        assert_eq!(folder_loaded(9), "Folder loaded: 9 files");
        assert_eq!(
            generating(3),
            "Generating 3 thumbnails (files > 100 MB skipped) ..."
        );
        assert_eq!(
            rendering(1, 3, "a.stl"),
            "Rendering thumbnail 1 / 3  \u{2014}  a.stl"
        );
        assert_eq!(
            all_ready(3, 9),
            "All 3 thumbnails ready  \u{2014}  9 files in folder"
        );
        assert_eq!(progress(2, 5), "Generating thumbnails: 2 / 5");
        assert_eq!(
            image("t.bmp", "BMP", 256, 256, "192.1 KB"),
            "Image: t.bmp  \u{2014}  BMP  \u{2014}  256 \u{d7} 256 px  \u{2014}  192.1 KB"
        );
        assert_eq!(
            gif("a.gif", None, "1 Bytes"),
            "GIF: a.gif  \u{2014}  Animated  \u{2014}  Unknown \u{d7} Unknown px  \u{2014}  1 Bytes"
        );
        assert_eq!(
            svg("l.svg", 200, 100, "766 Bytes"),
            "SVG: l.svg  \u{2014}  Vector  \u{2014}  200 \u{d7} 100 px  \u{2014}  766 Bytes"
        );
        assert_eq!(
            video("v.webm", "1.00 MB"),
            "Video: v.webm  \u{2014}  WebM  \u{2014}  1.00 MB"
        );
        assert_eq!(load_error("boom"), "Failed to load the 3D model:\nboom");
        assert_eq!(parsing("a.stl"), "Parsing a.stl ...");
    }

    #[test]
    fn ms_rounding_matches_python() {
        assert_eq!(format_ms(0.4), "0");
        assert_eq!(format_ms(0.5), "0");
        assert_eq!(format_ms(1.5), "2");
        assert_eq!(format_ms(2.5), "2");
        assert_eq!(format_ms(520.49), "520");
        assert_eq!(format_ms(f64::NAN), "0");
    }

    #[test]
    fn metadata_numbers() {
        assert_eq!(dimensions_mm([60.0, 31.04, 48.0]), "60.0 x 31.0 x 48.0 mm");
        assert_eq!(volume(15550.5), "15.55 cm\u{b3} (15,550.5 mm\u{b3})");
        assert_eq!(volume(0.0), "0.00 cm\u{b3} (0.0 mm\u{b3})");
        assert_eq!(ext_upper(Path::new("a/b.Jpeg")), "JPEG");
        assert_eq!(basename(Path::new(r"C:\dev\x.stl")), "x.stl");
        assert_eq!(basename(Path::new(r"C:\")), r"C:\");
        assert_eq!(basename(&PathBuf::from(r"C:\dev\models")), "models");
    }

    #[test]
    fn normpath_like_python() {
        assert_eq!(
            normpath(r"C:/dev//stl_viewer/./models/../x"),
            r"C:\dev\stl_viewer\x"
        );
        assert_eq!(normpath(r"C:\"), r"C:\");
        assert_eq!(normpath(r"C:\.."), r"C:\");
        assert_eq!(normpath(r"c:\Dev\"), r"c:\Dev");
        assert_eq!(normpath("C:"), "C:");
        assert_eq!(normpath(""), ".");
        assert_eq!(normpath(r"..\a\..\.."), r"..\..");
        assert_eq!(normpath(r"\\server\share\a\..\b"), r"\\server\share\b");
        assert_eq!(normpath(r"\a\.\b"), r"\a\b");
    }

    #[test]
    fn elision() {
        let w = |_: char| 1.0;
        assert_eq!(elide_left("abcdef", 6.0, w), "abcdef");
        assert_eq!(elide_left("abcdef", 4.0, w), "\u{2026}ef");
        assert_eq!(elide_left("abcdef", 4.5, w), "\u{2026}def");
        assert_eq!(elide_left("abcdef", 0.5, w), "\u{2026}");
        assert_eq!(elide_middle("abcdefgh", 8.0, w), "abcdefgh");
        assert_eq!(elide_middle("abcdefgh", 6.0, w), "ab\u{2026}gh");
        assert_eq!(elide_middle("abcdefgh", 5.0, w), "a\u{2026}h");
        // Qt keeps the same number of characters on both sides.
        let qt = elide_middle("hierarchical_assembly.3mf", 110.0, |_| 6.0);
        assert_eq!(qt, "hierarch\u{2026}mbly.3mf");
        assert_eq!(elide_middle("abcdefgh", 1.0, w), "\u{2026}");
        assert_eq!(elide_middle("", 0.0, w), "");
    }

    fn chars(s: &str) -> f32 {
        s.chars().count() as f32
    }

    #[test]
    fn qt_like_wrapping() {
        let mut m = chars;
        assert_eq!(break_points(""), vec![0]);
        assert_eq!(break_points("ab cd-ef 1-2"), vec![3, 6, 9, 12]);
        assert_eq!(break_points("\u{2026}\\a\\b"), vec![3, 7]);
        assert_eq!(
            wrap_lines("64.00 cm (64,000.0 mm)", 19.0, &mut m),
            vec!["64.00 cm (64,000.0", "mm)"]
        );
        assert_eq!(wrap_lines("one two", 100.0, &mut m), vec!["one two"]);
        // A word wider than the line overflows instead of being split.
        assert_eq!(
            wrap_lines("a verylongword b", 4.0, &mut m),
            vec!["a", "verylongword", "b"]
        );
        assert_eq!(wrap_lines("", 4.0, &mut m), vec![""]);
        assert_eq!(wrap_extent("aa bbbb c", 5.0, &mut m), (4.0, 3));
        assert_eq!(wrap_extent("aa bbbb c", 6.0, &mut m), (6.0, 2));
        assert_eq!(qt_wrap_min("aa bbbb c", &mut m), 4.0);
        // Qt wraps a path after the drive colon, not at its separators.
        assert_eq!(break_points(r"C:\dev\a_b.obj"), vec![2, 14]);
        assert_eq!(break_points("c:/dev a:b"), vec![7, 10]);
        assert_eq!(
            wrap_lines(r"C:\dev\stl_viewer\models\cube.obj", 20.0, &mut m),
            vec!["C:", r"\dev\stl_viewer\models\cube.obj"]
        );
        assert_eq!(qt_wrap_min(r"C:\dev\models", &mut m), 11.0);
    }

    #[test]
    fn qlabel_hint_and_grid_columns() {
        let mut m = chars;
        // 80 average chars of width 1: short text keeps its width ...
        assert_eq!(qt_wrap_hint("short", 1.0, &mut m), 5.0);
        // ... a single line wider than a quarter is re-laid at 20 wide.
        let text = "aaaa bbbb cccc dddd eeee ffff";
        assert_eq!(qt_wrap_hint(text, 1.0, &mut m), 19.0);
        // Labels fit: spare room split evenly; a wide value takes what it asks for.
        assert_eq!(qt_two_columns(198.0, 70.0, 20.0), (99.0, 99.0));
        assert_eq!(qt_two_columns(198.0, 70.0, 115.0), (83.0, 115.0));
        assert_eq!(qt_two_columns(220.0, 70.0, 150.0), (70.0, 150.0));
        // Not enough room: labels keep their width, values shrink.
        assert_eq!(qt_two_columns(288.0, 70.0, 260.0), (70.0, 218.0));
    }
}
