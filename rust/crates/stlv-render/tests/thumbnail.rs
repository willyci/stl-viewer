//! Headless ThumbnailRenderer tests. They skip (pass) when no GPU adapter is available.
//! Set `STLV_THUMB_OUT=<dir>` to also write the rendered PNGs there.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use stlv_core::ModelData;
use stlv_render::{
    Camera, PreparedModel, ShadingMode, SnapshotOptions, THUMBNAIL_SIZE, ThumbnailRenderer,
    ViewPreset,
};

fn renderer() -> Option<&'static ThumbnailRenderer> {
    static RENDERER: OnceLock<Option<ThumbnailRenderer>> = OnceLock::new();
    RENDERER
        .get_or_init(|| match ThumbnailRenderer::new_headless() {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!("skipping GPU tests: {e}");
                None
            }
        })
        .as_ref()
}

fn save(img: &image::RgbaImage, name: &str) {
    if let Some(dir) = std::env::var_os("STLV_THUMB_OUT") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).expect("create output dir");
        img.save(dir.join(name)).expect("write png");
    }
}

#[rustfmt::skip]
fn cube(size: f32) -> ModelData {
    let s = size;
    let v = vec![
        [0.0, 0.0, 0.0], [s, 0.0, 0.0], [s, s, 0.0], [0.0, s, 0.0],
        [0.0, 0.0, s], [s, 0.0, s], [s, s, s], [0.0, s, s],
    ];
    let t = vec![
        [0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7], [0, 1, 5], [0, 5, 4],
        [2, 3, 7], [2, 7, 6], [0, 4, 7], [0, 7, 3], [1, 2, 6], [1, 6, 5],
    ];
    ModelData::new(v, t, vec![], "STL (Binary)", "cube.stl")
}

fn helix() -> ModelData {
    let n = 400;
    let vertices: Vec<[f32; 3]> = (0..n)
        .map(|i| {
            let a = i as f32 * 0.1;
            [20.0 * a.cos(), 20.0 * a.sin(), i as f32 * 0.05]
        })
        .collect();
    let lines = (0..n as u32 - 1).map(|i| [i, i + 1]).collect();
    ModelData::new(vertices, vec![], lines, "G-Code Path", "helix.gcode")
}

/// Expected background color at the image's top/bottom rows (8-bit, VTK gradient).
fn bg(row_from_top: u32, height: u32) -> [f32; 3] {
    let t = 1.0 - (row_from_top as f32 + 0.5) / height as f32;
    let (b, top) = (
        stlv_core::palette::VIEWPORT_BG_BOTTOM,
        stlv_core::palette::VIEWPORT_BG_TOP,
    );
    std::array::from_fn(|i| (b[i] + (top[i] - b[i]) * t) * 255.0)
}

fn assert_px_near(img: &image::RgbaImage, x: u32, y: u32, expected: [f32; 3], tol: f32) {
    let p = img.get_pixel(x, y).0;
    for i in 0..3 {
        assert!(
            (p[i] as f32 - expected[i]).abs() <= tol,
            "pixel ({x},{y}) = {p:?}, expected ~{expected:?}"
        );
    }
    assert_eq!(p[3], 255);
}

/// Fraction of pixels that are clearly cyan (model color, any shade).
fn cyan_fraction(img: &image::RgbaImage) -> f64 {
    let n = img
        .pixels()
        .filter(|p| p[2] > 60 && p[1] > 50 && p[0] < 40 && p[2] > p[0] + 40)
        .count();
    n as f64 / (img.width() * img.height()) as f64
}

#[test]
fn renders_cube_thumbnail() {
    let Some(r) = renderer() else { return };
    let img = r.render(&cube(10.0)).expect("render");
    save(&img, "cube.png");
    assert_eq!(img.dimensions(), THUMBNAIL_SIZE);
    let (w, h) = img.dimensions();
    assert_px_near(&img, 0, 0, bg(0, h), 2.0);
    assert_px_near(&img, w - 1, h - 1, bg(h - 1, h), 2.0);
    // The model sits in the middle, lit cyan (1.85 x extent framing is tight for a cube).
    let center = img.get_pixel(w / 2, h / 2).0;
    assert!(
        center[1] > 100 && center[2] > 100 && center[0] < 60,
        "center {center:?}"
    );
    let f = cyan_fraction(&img);
    assert!((0.3..0.95).contains(&f), "cyan fraction {f}");
}

#[test]
fn renders_toolpath_thumbnail() {
    let Some(r) = renderer() else { return };
    let img = r.render(&helix()).expect("render");
    save(&img, "helix.png");
    let f = cyan_fraction(&img);
    // Thin lines: some cyan, mostly background.
    assert!(f > 0.01 && f < 0.3, "cyan fraction {f}");
}

#[test]
fn empty_model_is_an_error() {
    let Some(r) = renderer() else { return };
    let m = ModelData::new(vec![], vec![], vec![], "STL (ASCII)", "empty.stl");
    assert!(r.render(&m).is_err());
}

#[test]
fn degenerate_and_nan_models_do_not_panic() {
    let Some(r) = renderer() else { return };
    // Every vertex identical: zero extent.
    let flat = ModelData::new(
        vec![[1.0, 2.0, 3.0]; 3],
        vec![[0, 1, 2]],
        vec![],
        "STL (ASCII)",
        "dot.stl",
    );
    r.render(&flat).expect("render degenerate");
    let nan = ModelData::new(
        vec![[f32::NAN, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        vec![[0, 1, 2]],
        vec![],
        "STL (ASCII)",
        "nan.stl",
    );
    let _ = r.render(&nan);
    let bad_index = ModelData::new(
        vec![[0.0; 3], [1.0, 0.0, 0.0]],
        vec![[0, 1, 7]],
        vec![],
        "OBJ",
        "bad.obj",
    );
    let _ = r.render(&bad_index);
}

#[test]
fn concurrent_renders_from_threads() {
    let Some(r) = renderer() else { return };
    let model = Arc::new(cube(5.0));
    let reference = r.render(&model).expect("render");
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let model = model.clone();
                s.spawn(move || r.render(&model).expect("render"))
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().expect("thread"), reference);
        }
    });
}

#[test]
fn snapshot_options_cover_all_shading_modes() {
    let Some(r) = renderer() else { return };
    let prepared = PreparedModel::new(Arc::new(cube(10.0)));
    for shading in ShadingMode::ALL {
        let opts = SnapshotOptions {
            size: (320, 240),
            shading,
            grid: true,
            ..Default::default()
        };
        let img = r.render_snapshot(&prepared, &opts).expect("snapshot");
        assert_eq!(img.dimensions(), (320, 240));
        save(&img, &format!("cube_{shading:?}.png").to_lowercase());
        // Points mode: only the cube's 8 corners (3 px squares).
        assert!(
            cyan_fraction(&img) * 320.0 * 240.0 >= 10.0,
            "{shading:?} drew nothing"
        );
    }
    let top = SnapshotOptions {
        camera: Some(Camera::for_preset(&prepared.model.bounds, ViewPreset::Top)),
        ..Default::default()
    };
    let img = r.render_snapshot(&prepared, &top).expect("snapshot");
    // Looking straight down on a cube: a centred square face, uniformly lit.
    let (w, h) = img.dimensions();
    assert_eq!(
        img.get_pixel(w / 2, h / 2),
        img.get_pixel(w / 2 + 5, h / 2 + 5)
    );
}

#[test]
fn renders_sample_models() {
    let Some(r) = renderer() else { return };
    let dir = Path::new(r"C:\dev\stl_viewer\models");
    for name in [
        "ascii_cube.stl",
        "binary_cylinder.stl",
        "cube.obj",
        "pyramid_mesh.3mf",
        "spiral_path.gcode",
    ] {
        let path = dir.join(name);
        let model = match stlv_core::load_model(&path) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("skipping {name}: {e}");
                continue;
            }
        };
        let img = r.render(&model).expect("render");
        save(&img, &format!("{name}.png"));
        assert!(cyan_fraction(&img) > 0.005, "{name} drew nothing");
    }
}

#[test]
fn thread_safety_contract() {
    fn send<T: Send>() {}
    fn send_sync<T: Send + Sync>() {}
    send::<PreparedModel>();
    send_sync::<ThumbnailRenderer>();
}
