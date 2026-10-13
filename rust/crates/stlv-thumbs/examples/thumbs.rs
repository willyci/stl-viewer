//! Developer tool for the thumbnail cache and service (Python interop checks, contact sheets).
//!
//! ```text
//! thumbs info <path>...                     abspath, cache key, Python st_mtime bits, size
//! thumbs check <db> <path>...               freshness and cached PNG per path
//! thumbs put <db> <png> <path>...           store the PNG file for each path
//! thumbs sheet <dir> <out.png> --cache-db <db> [--no-render]
//!                                           run the folder pipeline, save a contact sheet
//! ```
//! Always pass a scratch `<db>`; never the app's real cache.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use image::{Rgba, RgbaImage};
use md5::{Digest, Md5};
use resvg::{tiny_skia, usvg};
use stlv_render::ThumbnailRenderer;
use stlv_thumbs::{ThumbEvent, ThumbnailCache, ThumbnailService, path_hash, python_mtime};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("info") => info(&args[1..]),
        Some("check") if args.len() >= 2 => check(&args[1], &args[2..]),
        Some("put") if args.len() >= 3 => put(&args[1], &args[2], &args[3..]),
        Some("sheet") if args.len() >= 3 => sheet(&args[1..]),
        _ => Err("usage: thumbs info|check|put|sheet ... (see the source header)".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn info(paths: &[String]) -> Result<(), String> {
    for p in paths {
        let path = Path::new(p);
        let abs = std::path::absolute(path).map_err(|e| e.to_string())?;
        let meta = std::fs::metadata(path).map_err(|e| format!("{p}: {e}"))?;
        let mtime = python_mtime(&meta);
        println!(
            "{p}\t{}\t{}\t{:016x}\t{mtime}\t{}",
            abs.display(),
            path_hash(path),
            mtime.to_bits(),
            meta.len()
        );
    }
    Ok(())
}

fn check(db: &str, paths: &[String]) -> Result<(), String> {
    let cache = ThumbnailCache::open(Path::new(db)).map_err(|e| e.to_string())?;
    for p in paths {
        let path = Path::new(p);
        let valid = cache.is_valid(path);
        let (len, digest, dims) = match cache.get_png(path) {
            Some(png) => {
                let dims = image::load_from_memory(&png)
                    .map(|i| format!("{}x{}", i.width(), i.height()))
                    .unwrap_or_else(|e| format!("undecodable({e})"));
                (png.len(), hex(&Md5::digest(&png)), dims)
            }
            None => (0, "-".into(), "-".into()),
        };
        println!("{p}\t{valid}\t{len}\t{digest}\t{dims}");
    }
    Ok(())
}

fn put(db: &str, png: &str, paths: &[String]) -> Result<(), String> {
    let cache = ThumbnailCache::open(Path::new(db)).map_err(|e| e.to_string())?;
    let png = std::fs::read(png).map_err(|e| format!("{png}: {e}"))?;
    for p in paths {
        cache.put(Path::new(p), &png).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn sheet(args: &[String]) -> Result<(), String> {
    let (dir, out) = (PathBuf::from(&args[0]), PathBuf::from(&args[1]));
    let db = args
        .iter()
        .position(|a| a == "--cache-db")
        .and_then(|i| args.get(i + 1))
        .ok_or("--cache-db <scratch db> is required")?;
    let no_render = args.iter().any(|a| a == "--no-render");
    let cache = Arc::new(ThumbnailCache::open(Path::new(db)).map_err(|e| e.to_string())?);
    let renderer = if no_render {
        None
    } else {
        match ThumbnailRenderer::new_headless() {
            Ok(r) => Some(Arc::new(r)),
            Err(e) => {
                eprintln!("no renderer ({e}); meshes only from the cache");
                None
            }
        }
    };
    let files = stlv_core::files::list_supported_files(&dir, true).map_err(|e| e.to_string())?;
    let started = Instant::now();
    let mut svc = ThumbnailService::new(cache, renderer, || {});
    svc.start_folder(files.clone());
    let mut ready = HashMap::new();
    loop {
        for event in svc.poll() {
            match event {
                ThumbEvent::Ready { path, image } => {
                    println!(
                        "{:>7.1} ms  Ready {} {}x{}",
                        started.elapsed().as_secs_f64() * 1e3,
                        path.display(),
                        image.width(),
                        image.height()
                    );
                    ready.insert(path, image);
                }
                other => println!(
                    "{:>7.1} ms  {other:?}",
                    started.elapsed().as_secs_f64() * 1e3
                ),
            }
        }
        if !svc.is_busy() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    println!(
        "{} files, {} images in {:?}",
        files.len(),
        ready.len(),
        started.elapsed()
    );
    contact_sheet(&files, &ready)
        .save(&out)
        .map_err(|e| e.to_string())?;
    println!("wrote {}", out.display());
    Ok(())
}

/// Cards in the Python grid's colors, each image centred in a 228x200 well, names below.
fn contact_sheet(files: &[PathBuf], ready: &HashMap<PathBuf, RgbaImage>) -> RgbaImage {
    const COLS: u32 = 5;
    const CARD: (u32, u32) = (244, 240);
    const WELL: (u32, u32) = (228, 200);
    const GAP: u32 = 12;
    let rows = (files.len() as u32).div_ceil(COLS).max(1);
    let size = (GAP + COLS * (CARD.0 + GAP), GAP + rows * (CARD.1 + GAP));
    let mut sheet = RgbaImage::from_pixel(size.0, size.1, Rgba([0x0a, 0x0c, 0x10, 255]));
    let mut labels = String::new();
    for (i, path) in files.iter().enumerate() {
        let (col, row) = (i as u32 % COLS, i as u32 / COLS);
        let (x, y) = (GAP + col * (CARD.0 + GAP), GAP + row * (CARD.1 + GAP));
        fill(&mut sheet, x, y, CARD, Rgba([0x1a, 0x1d, 0x26, 255]));
        let (wx, wy) = (x + (CARD.0 - WELL.0) / 2, y + 8);
        fill(&mut sheet, wx, wy, WELL, Rgba([0x0d, 0x0f, 0x14, 255]));
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let caption = match ready.get(path) {
            Some(image) => {
                let ix = wx + (WELL.0 - image.width()) / 2;
                let iy = wy + (WELL.1 - image.height()) / 2;
                image::imageops::overlay(&mut sheet, image, i64::from(ix), i64::from(iy));
                format!("{name}  ({}x{})", image.width(), image.height())
            }
            None => format!("{name}  (placeholder)"),
        };
        labels.push_str(&format!(
            r##"<text x="{}" y="{}" font-family="Segoe UI" font-size="12" fill="#E2E8F0" text-anchor="middle">{}</text>"##,
            x + CARD.0 / 2,
            y + CARD.1 - 12,
            xml_escape(&caption)
        ));
    }
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{}" height="{}">{labels}</svg>"#,
        size.0, size.1
    );
    if let Some(text) = rasterize(&svg, size) {
        image::imageops::overlay(&mut sheet, &text, 0, 0);
    }
    sheet
}

fn fill(image: &mut RgbaImage, x: u32, y: u32, (w, h): (u32, u32), color: Rgba<u8>) {
    for py in y..y + h {
        for px in x..x + w {
            image.put_pixel(px, py, color);
        }
    }
}

fn rasterize(svg: &str, (w, h): (u32, u32)) -> Option<RgbaImage> {
    let mut options = usvg::Options::default();
    options.fontdb_mut().load_system_fonts();
    let tree = usvg::Tree::from_str(svg, &options).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    let rgba = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    RgbaImage::from_raw(w, h, rgba)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
