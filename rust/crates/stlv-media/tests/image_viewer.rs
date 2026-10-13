//! ImageViewer end to end on generated fixtures, driven by a headless egui context.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{Harness, temp_dir};
use egui::{
    Color32, ColorImage, Event, Modifiers, MouseWheelUnit, PointerButton, TouchPhase, pos2, vec2,
};
use image::{Delay, Frame, Rgba, RgbaImage};
use stlv_media::{ImageInfo, ImageViewer, MAX_ZOOM, MIN_ZOOM, WHEEL_STEP};

const VIEW: [f32; 2] = [960.0, 700.0];
const RED: Rgba<u8> = Rgba([220, 30, 30, 255]);
const BLUE: Rgba<u8> = Rgba([30, 30, 220, 255]);

/// Left half red, right half blue.
fn halves(w: u32, h: u32) -> RgbaImage {
    RgbaImage::from_fn(w, h, |x, _| if x < w / 2 { RED } else { BLUE })
}

fn save(dir: &Path, name: &str, img: &RgbaImage) -> PathBuf {
    let path = dir.join(name);
    if name.ends_with(".jpg") || name.ends_with(".bmp") {
        image::DynamicImage::ImageRgba8(img.clone())
            .to_rgb8()
            .save(&path)
            .unwrap();
    } else {
        img.save(&path).unwrap();
    }
    path
}

fn wait_ready(h: &mut Harness, v: &mut ImageViewer) {
    assert!(
        h.run_until(
            v,
            Duration::from_secs(10),
            ImageViewer::show,
            ImageViewer::is_ready
        ),
        "never ready"
    );
}

fn close(a: Color32, b: Rgba<u8>, tol: u8) -> bool {
    let b = [b[0], b[1], b[2]];
    [a.r(), a.g(), a.b()]
        .iter()
        .zip(b)
        .all(|(x, y)| x.abs_diff(y) <= tol)
}

fn pixel(img: &ColorImage, x: usize, y: usize) -> Color32 {
    img.pixels[y * img.size[0] + x]
}

fn fit(w: f32, h: f32) -> f32 {
    ((VIEW[0] - 4.0) / w).min((VIEW[1] - 4.0) / h)
}

#[test]
fn static_formats_load_fit_and_upload() {
    let dir = temp_dir("static");
    let img = halves(300, 200);
    for name in ["a.png", "b.jpg", "c.bmp", "d.tiff", "e.tif"] {
        let path = save(&dir, name, &img);
        let mut h = Harness::new(VIEW[0], VIEW[1]);
        let mut v = ImageViewer::new();
        let t = Instant::now();
        let info = v.load(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(
            t.elapsed() < Duration::from_secs(1),
            "{name}: load took {:?}",
            t.elapsed()
        );
        assert_eq!(
            info,
            ImageInfo {
                width: 300,
                height: 200,
                animated: false
            },
            "{name}"
        );
        assert!(v.is_loaded());
        wait_ready(&mut h, &mut v);
        assert!(v.error().is_none(), "{name}: {:?}", v.error());
        assert!(
            (v.zoom() - fit(300.0, 200.0)).abs() < 1e-4,
            "{name}: zoom {}",
            v.zoom()
        );
        let full = h.uploads_of_size([300, 200]);
        assert_eq!(full.len(), 1, "{name}: full-size level uploaded once");
        let tol = if name.ends_with(".jpg") { 24 } else { 0 };
        assert!(
            close(pixel(&full[0], 60, 100), RED, tol),
            "{name}: {:?}",
            pixel(&full[0], 60, 100)
        );
        assert!(
            close(pixel(&full[0], 240, 100), BLUE, tol),
            "{name}: {:?}",
            pixel(&full[0], 240, 100)
        );
        // A mip level for smooth minification exists too.
        assert_eq!(h.uploads_of_size([150, 100]).len(), 1, "{name}");
    }
}

#[test]
fn large_images_are_tiled_within_texture_limits() {
    let dir = temp_dir("large");
    let path = save(&dir, "wide.png", &halves(5000, 300));
    let mut h = Harness::new(VIEW[0], VIEW[1]);
    let mut v = ImageViewer::new();
    assert_eq!(
        v.load(&path).unwrap(),
        ImageInfo {
            width: 5000,
            height: 300,
            animated: false
        }
    );
    wait_ready(&mut h, &mut v);
    assert!(
        h.uploads
            .iter()
            .all(|i| i.size[0] <= 2048 && i.size[1] <= 2048)
    );
    // Full-resolution level: three tiles covering 5000 px plus shared borders.
    let widths: usize = h
        .uploads
        .iter()
        .filter(|i| i.size[1] == 300)
        .map(|i| i.size[0])
        .sum();
    assert_eq!(widths, 5000 + 4);
}

#[test]
fn animated_gif_cycles_frames_with_delays() {
    let dir = temp_dir("gif");
    let path = dir.join("anim.gif");
    {
        let file = std::fs::File::create(&path).unwrap();
        let mut enc = image::codecs::gif::GifEncoder::new(file);
        enc.set_repeat(image::codecs::gif::Repeat::Infinite)
            .unwrap();
        let colors = [RED, Rgba([30, 220, 30, 255]), BLUE];
        let frames = colors.map(|c| {
            Frame::from_parts(
                RgbaImage::from_pixel(40, 30, c),
                0,
                0,
                Delay::from_numer_denom_ms(50, 1),
            )
        });
        enc.encode_frames(frames).unwrap();
    }
    let mut h = Harness::new(VIEW[0], VIEW[1]);
    let mut v = ImageViewer::new();
    assert_eq!(
        v.load(&path).unwrap(),
        ImageInfo {
            width: 40,
            height: 30,
            animated: true
        }
    );
    wait_ready(&mut h, &mut v);
    let start = v.frames_shown();
    let t = Instant::now();
    let _ = h.run_until(
        &mut v,
        Duration::from_millis(800),
        ImageViewer::show,
        |_| false,
    );
    let shown = v.frames_shown() - start;
    let expected = t.elapsed().as_millis() as u64 / 50;
    assert!(
        shown >= expected / 2 && shown <= expected + 2,
        "{shown} frame switches in {:?}",
        t.elapsed()
    );
    // Small animations are cached after the first pass: each frame uploaded exactly once.
    let uploads = h.uploads_of_size([40, 30]);
    assert_eq!(uploads.len(), 3);
    assert!(close(uploads[0].pixels[0], RED, 8) && close(uploads[2].pixels[0], BLUE, 8));

    // A single-frame GIF is a still image.
    let still = dir.join("still.gif");
    RgbaImage::from_pixel(8, 4, RED).save(&still).unwrap();
    assert_eq!(
        v.load(&still).unwrap(),
        ImageInfo {
            width: 8,
            height: 4,
            animated: false
        }
    );
    wait_ready(&mut h, &mut v);
}

#[test]
fn svg_rerasterizes_sharp_at_high_zoom() {
    let dir = temp_dir("svg");
    let path = dir.join("logo.svg");
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50" viewBox="0 0 100 50">
        <rect x="0" y="0" width="50" height="50" fill="#dc1e1e"/><circle cx="75" cy="25" r="20" fill="#1e1edc"/></svg>"##;
    std::fs::write(&path, svg).unwrap();
    let mut h = Harness::new(VIEW[0], VIEW[1]);
    let mut v = ImageViewer::new();
    assert_eq!(
        v.load(&path).unwrap(),
        ImageInfo {
            width: 100,
            height: 50,
            animated: false
        }
    );
    wait_ready(&mut h, &mut v);
    assert!((v.zoom() - fit(100.0, 50.0)).abs() < 1e-4);
    let before = h.uploads.len();
    v.set_zoom(MAX_ZOOM);
    wait_ready(&mut h, &mut v);
    assert_eq!(v.zoom(), MAX_ZOOM);
    let detail = h.uploads[before..]
        .last()
        .expect("a detail raster after zooming")
        .clone();
    // At 20x the drawing is 2000x1000 px: the raster covers at least the visible 960x700
    // at full screen resolution, and never the whole (unbounded) drawing.
    assert!(
        detail.size[0] >= 960 && detail.size[1] >= 700,
        "{:?}",
        detail.size
    );
    assert!(
        detail.size[0] <= 4096 && detail.size[1] <= 4096,
        "{:?}",
        detail.size
    );
    assert!(
        h.uploads
            .iter()
            .all(|i| i.size[0] <= 4096 && i.size[1] <= 4096)
    );
}

fn wheel(lines: f32) -> Event {
    Event::MouseWheel {
        unit: MouseWheelUnit::Line,
        delta: vec2(0.0, lines),
        phase: TouchPhase::Move,
        modifiers: Modifiers::NONE,
    }
}

#[test]
fn wheel_zoom_steps_and_clamps_and_drag_pans() {
    let dir = temp_dir("wheel");
    let path = save(&dir, "z.png", &halves(200, 100));
    let mut h = Harness::new(VIEW[0], VIEW[1]);
    let mut v = ImageViewer::new();
    v.load(&path).unwrap();
    wait_ready(&mut h, &mut v);
    let z0 = v.zoom();
    let at = pos2(300.0, 200.0);
    h.frame(vec![Event::PointerMoved(at)], |ui| v.show(ui));
    h.frame(vec![], |ui| v.show(ui));
    h.frame(vec![wheel(1.0)], |ui| v.show(ui));
    assert!(
        (v.zoom() / z0 - WHEEL_STEP).abs() < 1e-4,
        "{} -> {}",
        z0,
        v.zoom()
    );
    h.frame(vec![wheel(-2.0)], |ui| v.show(ui));
    assert!((v.zoom() * WHEEL_STEP / z0 - 1.0).abs() < 1e-4);
    h.frame(vec![wheel(100.0)], |ui| v.show(ui));
    assert_eq!(v.zoom(), MAX_ZOOM);
    h.frame(vec![wheel(-200.0)], |ui| v.show(ui));
    assert_eq!(v.zoom(), MIN_ZOOM);
    // Dragging pans without changing the zoom (and never panics at the limits).
    let press = |pressed| Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    h.frame(vec![wheel(30.0)], |ui| v.show(ui));
    let z = v.zoom();
    h.frame(vec![press(true)], |ui| v.show(ui));
    for i in 1..10 {
        h.frame(
            vec![Event::PointerMoved(
                at + vec2(i as f32 * 40.0, i as f32 * 25.0),
            )],
            |ui| v.show(ui),
        );
    }
    h.frame(vec![press(false)], |ui| v.show(ui));
    assert_eq!(v.zoom(), z);
    // Re-fit on request.
    v.fit();
    h.frame(vec![], |ui| v.show(ui));
    assert!((v.zoom() - z0).abs() < 1e-4);
}

#[test]
fn bad_files_are_errors_not_panics() {
    let dir = temp_dir("bad");
    let mut v = ImageViewer::new();
    assert!(v.load(&dir.join("missing.png")).is_err());
    assert!(!v.is_loaded());
    let write = |name: &str, data: &[u8]| {
        let p = dir.join(name);
        std::fs::write(&p, data).unwrap();
        p
    };
    for (name, data) in [
        ("tiny.png", &b"\x89PNG"[..]),
        ("text.jpg", b"hello, this is not a jpeg"),
        ("empty.bmp", b""),
        ("bad.gif", b"GIF89a"),
        ("bad.svg", b"<svg"),
        ("empty.svg", b""),
        ("model.stl", b"solid x"),
    ] {
        let p = write(name, data);
        assert!(v.load(&p).is_err(), "{name} should fail");
        assert!(!v.is_loaded(), "{name}");
    }

    // Valid header, broken pixel data: `load` succeeds from the header, decoding fails later.
    let mut png = Vec::new();
    halves(64, 64)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let p = write("truncated.png", &png[..50]);
    let mut h = Harness::new(VIEW[0], VIEW[1]);
    assert_eq!(
        v.load(&p).unwrap(),
        ImageInfo {
            width: 64,
            height: 64,
            animated: false
        }
    );
    wait_ready(&mut h, &mut v);
    assert!(v.error().is_some());
    v.clear();
    assert!(!v.is_loaded() && v.info().is_none());
    h.frame(vec![], |ui| v.show(ui));
}

#[test]
fn svg_detail_follows_pixels_per_point() {
    let dir = temp_dir("svg-hidpi");
    let path = dir.join("logo.svg");
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50">
        <rect width="100" height="50" fill="#1e1edc"/></svg>"##;
    std::fs::write(&path, svg).unwrap();
    // A 960x700 pt view at 2 px/pt (1920x1400 physical pixels), as on a 200% display.
    // RawInput rects are in points; let the zoom change settle before loading.
    let mut h = Harness::new(VIEW[0], VIEW[1]);
    h.ctx.set_zoom_factor(2.0);
    h.frame(vec![], |_| {});
    h.frame(vec![], |_| {});
    let mut v = ImageViewer::new();
    v.load(&path).unwrap();
    wait_ready(&mut h, &mut v);
    assert_eq!(h.ctx.pixels_per_point(), 2.0);
    assert!(
        (v.zoom() - fit(100.0, 50.0)).abs() < 1e-4,
        "fit uses points: {}",
        v.zoom()
    );
    let before = h.uploads.len();
    v.set_zoom(MAX_ZOOM);
    wait_ready(&mut h, &mut v);
    let detail = h.uploads[before..].last().expect("detail raster").clone();
    assert!(
        detail.size[0] >= 1920 && detail.size[1] >= 1000,
        "{:?}",
        detail.size
    );
    assert!(
        detail.size[0] <= 4096 && detail.size[1] <= 4096,
        "{:?}",
        detail.size
    );
}

#[test]
fn gif_frames_wider_than_the_texture_limit_are_downscaled() {
    let dir = temp_dir("gif-wide");
    let path = dir.join("wide.gif");
    {
        let file = std::fs::File::create(&path).unwrap();
        let mut enc = image::codecs::gif::GifEncoder::new(file);
        let frames = [RED, BLUE].map(|c| {
            let img = RgbaImage::from_pixel(3000, 20, c);
            Frame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(40, 1))
        });
        enc.encode_frames(frames).unwrap();
    }
    let mut h = Harness::new(VIEW[0], VIEW[1]);
    let limit = h.ctx.input(|i| i.max_texture_side);
    let mut v = ImageViewer::new();
    assert_eq!(
        v.load(&path).unwrap(),
        ImageInfo {
            width: 3000,
            height: 20,
            animated: true
        }
    );
    let _ = h.run_until(
        &mut v,
        Duration::from_millis(400),
        ImageViewer::show,
        |_| false,
    );
    assert!(v.frames_shown() >= 3);
    let frames: Vec<_> = h.uploads.iter().filter(|i| i.size[1] < 20).collect();
    assert!(!frames.is_empty());
    assert!(
        frames.iter().all(|i| i.size == [limit, 14]),
        "{:?}",
        frames[0].size
    );
}
