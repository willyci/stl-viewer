#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The Python app's sample models: `$STLV_MODELS_DIR`, else `<repo>/models` next to `rust/`.
pub fn models_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("STLV_MODELS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../models"));
    if dir.join("ascii_cube.stl").is_file() {
        Some(dir)
    } else {
        eprintln!(
            "sample models not found at {} (set STLV_MODELS_DIR); skipping",
            dir.display()
        );
        None
    }
}

/// A fresh, self-deleting temp directory.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("stlv-core-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }

    pub fn write(&self, name: &str, data: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, data).expect("write fixture");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Zip archive with the given `(name, contents)` entries (deflated).
pub fn zip(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, contents) in entries {
        writer.start_file(*name, options).expect("start zip entry");
        writer
            .write_all(contents.as_bytes())
            .expect("write zip entry");
    }
    writer.finish().expect("finish zip").into_inner()
}

/// `<model>` document with the given unit attribute (if any) and body.
pub fn model_xml(unit: Option<&str>, body: &str) -> String {
    let unit = unit.map(|u| format!(" unit=\"{u}\"")).unwrap_or_default();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<model xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\" \
         xmlns:p=\"http://schemas.microsoft.com/3dmanufacturing/production/2015/06\"{unit}>\n{body}\n</model>"
    )
}

/// `<object>` holding an axis-aligned box mesh from `lo` to `hi` (12 outward triangles).
pub fn box_object(id: &str, lo: [f64; 3], hi: [f64; 3]) -> String {
    let mut verts = String::new();
    for i in 0..8 {
        let p = [
            if i & 1 == 0 { lo[0] } else { hi[0] },
            if i & 2 == 0 { lo[1] } else { hi[1] },
            if i & 4 == 0 { lo[2] } else { hi[2] },
        ];
        verts += &format!("<vertex x=\"{}\" y=\"{}\" z=\"{}\"/>", p[0], p[1], p[2]);
    }
    let tris = [
        [0, 2, 3],
        [0, 3, 1],
        [4, 5, 7],
        [4, 7, 6],
        [0, 1, 5],
        [0, 5, 4],
        [2, 6, 7],
        [2, 7, 3],
        [0, 4, 6],
        [0, 6, 2],
        [1, 3, 7],
        [1, 7, 5],
    ];
    let tris: String = tris
        .iter()
        .map(|t| {
            format!(
                "<triangle v1=\"{}\" v2=\"{}\" v3=\"{}\"/>",
                t[0], t[1], t[2]
            )
        })
        .collect();
    format!(
        "<object id=\"{id}\" type=\"model\"><mesh><vertices>{verts}</vertices><triangles>{tris}</triangles></mesh></object>"
    )
}

pub const CONTENT_TYPES: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>";

/// Deterministic xorshift64* generator for fuzz-style tests.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    pub fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }
}

pub fn assert_close(actual: f64, expected: f64, tol: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= tol,
        "{what}: {actual} vs {expected}"
    );
}
