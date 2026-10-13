//! Thumbnail card tooltips: file facts from the listing plus details (dimensions, counts)
//! read on a background thread the first time a card is hovered.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::SystemTime;

use stlv_core::{FileKind, format};
use stlv_thumbs::MAX_RENDER_FILE_SIZE;

use crate::text;

/// Facts that need the file to be read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Details {
    pub rows: Vec<(&'static str, String)>,
    /// Why the file could not be read (parse or decode error).
    pub error: Option<String>,
}

enum State {
    Pending,
    Ready(Details),
}

struct Request {
    path: PathBuf,
    kind: FileKind,
    size: u64,
}

/// Reads [`Details`] on one background thread. Hovering moves fast, so only the newest
/// request is served; older queued ones are dropped and asked for again when re-hovered.
pub struct InfoCache {
    states: HashMap<PathBuf, State>,
    tx: Sender<Request>,
    rx: Receiver<(PathBuf, Option<Details>)>,
}

impl InfoCache {
    pub fn new(ctx: &egui::Context) -> Self {
        let (tx, job_rx) = channel::<Request>();
        let (done_tx, rx) = channel();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("card-info".into())
            .spawn(move || {
                while let Ok(mut req) = job_rx.recv() {
                    // Skip to the newest request; tell the grid about the skipped ones.
                    while let Ok(newer) = job_rx.try_recv() {
                        if done_tx.send((req.path, None)).is_err() {
                            return;
                        }
                        req = newer;
                    }
                    let details = read_details(&req.path, req.kind, req.size);
                    if done_tx.send((req.path, Some(details))).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            });
        if let Err(e) = spawned {
            log::error!("cannot start card info thread: {e}");
        }
        Self {
            states: HashMap::new(),
            tx,
            rx,
        }
    }

    /// Forget everything (another folder is shown).
    pub fn clear(&mut self) {
        self.states.clear();
    }

    /// Details for `path`, requesting them the first time; `None` while being read.
    pub fn get(&mut self, path: &Path, kind: FileKind, size: u64) -> Option<&Details> {
        while let Ok((p, details)) = self.rx.try_recv() {
            match details {
                Some(d) => {
                    self.states.insert(p, State::Ready(d));
                }
                None => {
                    self.states.remove(&p);
                }
            }
        }
        if !self.states.contains_key(path) {
            let req = Request {
                path: path.to_path_buf(),
                kind,
                size,
            };
            if self.tx.send(req).is_ok() {
                self.states.insert(path.to_path_buf(), State::Pending);
            }
        }
        match self.states.get(path) {
            Some(State::Ready(d)) => Some(d),
            _ => None,
        }
    }
}

/// Read dimensions and counts. Meshes over the preview limit are not parsed.
pub fn read_details(path: &Path, kind: FileKind, size: u64) -> Details {
    let mut d = Details::default();
    match kind {
        k if k.is_mesh() => {
            if size > MAX_RENDER_FILE_SIZE {
                return d;
            }
            match stlv_core::load_model(path) {
                Ok(m) => {
                    d.rows.push(("Format", m.format.clone()));
                    d.rows
                        .push(("Dimensions", text::dimensions_mm(m.bounding_box_size())));
                    if m.is_toolpath() {
                        d.rows
                            .push(("Lines", format::thousands(m.num_lines() as u64)));
                    } else {
                        d.rows
                            .push(("Triangles", format::thousands(m.num_triangles() as u64)));
                    }
                    d.rows
                        .push(("Vertices", format::thousands(m.num_vertices() as u64)));
                }
                Err(e) => d.error = Some(e.to_string()),
            }
        }
        FileKind::Image | FileKind::Gif => match image::image_dimensions(path) {
            Ok((w, h)) => d.rows.push(("Dimensions", format!("{w} \u{d7} {h} px"))),
            Err(e) => d.error = Some(e.to_string()),
        },
        FileKind::Svg => match stlv_media::svg_size(path) {
            Ok((w, h)) => d.rows.push((
                "Dimensions",
                format!("{} \u{d7} {} px", w.round(), h.round()),
            )),
            Err(e) => d.error = Some(e),
        },
        _ => {}
    }
    d
}

/// Human name of a file type for the tooltip.
pub fn kind_label(kind: FileKind) -> &'static str {
    match kind {
        FileKind::Stl => "STL mesh",
        FileKind::ThreeMf => "3MF model",
        FileKind::Obj => "Wavefront OBJ mesh",
        FileKind::Gcode => "G-code toolpath",
        FileKind::Image => "Image",
        FileKind::Gif => "GIF image",
        FileKind::Svg => "SVG vector image",
        FileKind::Webm => "WebM video",
        FileKind::Other => "File",
    }
}

/// `2.01 MB (2,109,605 bytes)`, or just `512 Bytes` below 1 KB.
pub fn size_label(bytes: u64) -> String {
    if bytes < 1024 {
        format::format_file_size(bytes)
    } else {
        format!(
            "{} ({} bytes)",
            format::format_file_size(bytes),
            format::thousands(bytes)
        )
    }
}

/// Why a card shows its placeholder icon instead of a preview.
pub fn missing_preview_reason(
    kind: FileKind,
    size: u64,
    error: Option<&str>,
    generating: bool,
) -> String {
    if kind == FileKind::Webm {
        return "No preview for video files".into();
    }
    if kind.is_mesh() && size > MAX_RENDER_FILE_SIZE {
        return format!(
            "No preview: larger than {}. Open the file once (or make this folder the tree root) to create one.",
            text::megabytes(MAX_RENDER_FILE_SIZE)
        );
    }
    if let Some(e) = error {
        return format!("No preview: {e}");
    }
    if generating {
        return "Preview is being generated\u{2026}".into();
    }
    "No preview available".into()
}

/// Local date and time as `2026-05-11 09:53`.
pub fn local_time(t: SystemTime) -> Option<String> {
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct SystemTimeW {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        millis: u16,
    }
    #[link(name = "kernel32", kind = "raw-dylib")]
    unsafe extern "system" {
        fn FileTimeToLocalFileTime(utc: *const FileTime, local: *mut FileTime) -> i32;
        fn FileTimeToSystemTime(ft: *const FileTime, st: *mut SystemTimeW) -> i32;
    }
    // FILETIME counts 100 ns intervals since 1601-01-01.
    const EPOCH_DIFF_SECS: u64 = 11_644_473_600;
    let since_unix = t.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    let ticks = (since_unix.as_secs() + EPOCH_DIFF_SECS) * 10_000_000
        + u64::from(since_unix.subsec_nanos() / 100);
    let utc = FileTime {
        low: ticks as u32,
        high: (ticks >> 32) as u32,
    };
    let mut local = FileTime::default();
    let mut st = SystemTimeW::default();
    // SAFETY: both calls only read/write the stack structs passed by pointer.
    let ok = unsafe {
        FileTimeToLocalFileTime(&utc, &mut local) != 0 && FileTimeToSystemTime(&local, &mut st) != 0
    };
    ok.then(|| {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            st.year, st.month, st.day, st.hour, st.minute
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;
    use std::time::Duration;

    #[test]
    fn reasons() {
        let big = MAX_RENDER_FILE_SIZE + 1;
        assert!(missing_preview_reason(FileKind::Stl, big, None, true).contains("larger than"));
        assert_eq!(
            missing_preview_reason(FileKind::Stl, 10, Some("bad"), true),
            "No preview: bad"
        );
        assert!(missing_preview_reason(FileKind::Obj, 10, None, true).contains("generated"));
        assert_eq!(
            missing_preview_reason(FileKind::Obj, 10, None, false),
            "No preview available"
        );
        assert!(missing_preview_reason(FileKind::Webm, 10, None, false).contains("video"));
    }

    #[test]
    fn labels() {
        assert_eq!(size_label(512), "512 Bytes");
        assert_eq!(size_label(2_109_605), "2.01 MB (2,109,605 bytes)");
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let s = local_time(t).unwrap();
        assert!(s.starts_with("2023-11-1"), "{s}");
        assert_eq!(s.len(), 16);
    }

    #[test]
    fn details_from_files() {
        let d = TempDir::new("card-info");
        let obj = d.path().join("t.obj");
        std::fs::write(
            &obj,
            "v 0 0 0\nv 2 0 0\nv 0 3 0\nv 0 0 4\nf 1 2 3\nf 1 2 4\n",
        )
        .unwrap();
        let det = read_details(&obj, FileKind::Obj, 40);
        assert_eq!(det.error, None);
        assert!(
            det.rows
                .contains(&("Dimensions", "2.0 x 3.0 x 4.0 mm".into()))
        );
        assert!(det.rows.contains(&("Triangles", "2".into())));

        let png = d.path().join("p.png");
        image::RgbaImage::new(7, 5).save(&png).unwrap();
        let det = read_details(&png, FileKind::Image, 1);
        assert_eq!(det.rows, [("Dimensions", "7 \u{d7} 5 px".to_string())]);

        let bad = d.path().join("bad.stl");
        std::fs::write(&bad, b"x").unwrap();
        assert!(read_details(&bad, FileKind::Stl, 1).error.is_some());
        // Over the preview limit: not parsed at all.
        assert_eq!(
            read_details(&bad, FileKind::Stl, u64::MAX),
            Details::default()
        );
    }
}
