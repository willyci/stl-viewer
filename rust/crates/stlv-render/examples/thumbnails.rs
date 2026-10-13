//! Writes ThumbnailRenderer PNGs for model files (headless, no window).
//!
//! ```text
//! cargo run -p stlv-render --example thumbnails -- OUT_DIR MODEL... [--size WxH]
//!     [--shading smooth|flat|wireframe|points] [--preset iso|top|...] [--grid] [--color R,G,B]
//!     [--suffix S]
//! ```
//! Without options each model gets the app's 228x200 thumbnail; any option switches to a
//! snapshot with the interactive viewport's look (minus the orientation marker).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use stlv_render::{
    Camera, PreparedModel, ShadingMode, SnapshotOptions, ThumbnailRenderer, ViewPreset,
};

struct Args {
    out_dir: PathBuf,
    models: Vec<PathBuf>,
    snapshot: Option<SnapshotOptions>,
    preset: ViewPreset,
    suffix: String,
}

fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once(['x', 'X'])?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut positional = Vec::new();
    let mut snapshot: Option<SnapshotOptions> = None;
    let mut preset = ViewPreset::Isometric;
    let mut suffix = String::new();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--size" => {
                let v = value("--size")?;
                snapshot.get_or_insert_with(Default::default).size =
                    parse_size(&v).ok_or("bad --size")?;
            }
            "--shading" => {
                let v = value("--shading")?;
                snapshot.get_or_insert_with(Default::default).shading =
                    ShadingMode::from_name(&v).ok_or("bad --shading")?;
            }
            "--preset" => {
                preset = ViewPreset::from_name(&value("--preset")?).ok_or("bad --preset")?;
                snapshot.get_or_insert_with(Default::default);
            }
            "--grid" => snapshot.get_or_insert_with(Default::default).grid = true,
            "--color" => {
                let v = value("--color")?;
                let c: Vec<f32> = v.split(',').filter_map(|c| c.trim().parse().ok()).collect();
                let [r, g, b] = c[..] else {
                    return Err("bad --color (r,g,b in 0..1)".into());
                };
                snapshot.get_or_insert_with(Default::default).color = [r, g, b];
            }
            "--suffix" => suffix = value("--suffix")?,
            _ => positional.push(PathBuf::from(arg)),
        }
    }
    if positional.len() < 2 {
        return Err(
            "usage: thumbnails OUT_DIR MODEL... [--size WxH] [--shading S] [--preset P] [--grid]"
                .into(),
        );
    }
    let out_dir = positional.remove(0);
    Ok(Args {
        out_dir,
        models: positional,
        snapshot,
        preset,
        suffix,
    })
}

fn render_one(renderer: &ThumbnailRenderer, args: &Args, path: &Path) -> Result<PathBuf, String> {
    let t0 = Instant::now();
    let model = Arc::new(stlv_core::load_model(path).map_err(|e| e.to_string())?);
    let t1 = Instant::now();
    let prepared = PreparedModel::new(model.clone());
    let t2 = Instant::now();
    let image = match &args.snapshot {
        None => renderer.render_prepared(&prepared)?,
        Some(opts) => {
            let opts = SnapshotOptions {
                camera: Some(Camera::for_preset(&model.bounds, args.preset)),
                ..*opts
            };
            renderer.render_snapshot(&prepared, &opts)?
        }
    };
    let t3 = Instant::now();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let out = args.out_dir.join(format!("{name}{}.png", args.suffix));
    image.save(&out).map_err(|e| e.to_string())?;
    println!(
        "{} -> {}  (parse {:.0} ms, prepare {:.0} ms, render {:.0} ms)",
        path.display(),
        out.display(),
        (t1 - t0).as_secs_f64() * 1e3,
        (t2 - t1).as_secs_f64() * 1e3,
        (t3 - t2).as_secs_f64() * 1e3,
    );
    Ok(out)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    if let Err(e) = std::fs::create_dir_all(&args.out_dir) {
        eprintln!("cannot create {}: {e}", args.out_dir.display());
        return ExitCode::FAILURE;
    }
    let renderer = match ThumbnailRenderer::new_headless() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let mut failed = false;
    for path in &args.models {
        if let Err(e) = render_one(&renderer, &args, path) {
            eprintln!("{}: {e}", path.display());
            failed = true;
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
