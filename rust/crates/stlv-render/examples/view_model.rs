//! Interactive viewer for one model: Viewport3D in an eframe (wgpu) window.
//!
//! ```text
//! cargo run -p stlv-render --example view_model -- [MODEL] [--shading smooth|flat|wireframe|points]
//!     [--preset iso|top|front|right|...] [--size WxH] [--auto-rotate] [--no-vsync]
//!     [--zoom F] [--bare] [--screenshot OUT.png [--screenshot-delay-ms N]] [--bench-secs S]
//! ```
//! `--screenshot` saves the window once the model is shown and exits. `--bench-secs` turns
//! auto-rotate on, records frame times for that long, prints statistics and exits (after
//! the screenshot, if requested). `--bare` hides the controls (viewport only).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use eframe::egui;
use stlv_render::{PreparedModel, ShadingMode, ViewPreset, Viewport3D};

#[derive(Default)]
struct Args {
    model: Option<PathBuf>,
    shading: ShadingMode,
    preset: Option<ViewPreset>,
    size: Option<(f32, f32)>,
    auto_rotate: bool,
    no_vsync: bool,
    zoom: Option<f32>,
    bare: bool,
    screenshot: Option<PathBuf>,
    screenshot_delay: Duration,
    bench: Option<Duration>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        screenshot_delay: Duration::from_millis(300),
        ..Default::default()
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--shading" => {
                args.shading =
                    ShadingMode::from_name(&value("--shading")?).ok_or("bad --shading")?
            }
            "--preset" => {
                args.preset =
                    Some(ViewPreset::from_name(&value("--preset")?).ok_or("bad --preset")?);
            }
            "--size" => {
                let v = value("--size")?;
                let (w, h) = v.split_once(['x', 'X']).ok_or("bad --size")?;
                args.size = Some((
                    w.parse().map_err(|_| "bad --size")?,
                    h.parse().map_err(|_| "bad --size")?,
                ));
            }
            "--auto-rotate" => args.auto_rotate = true,
            "--no-vsync" => args.no_vsync = true,
            "--bare" => args.bare = true,
            "--zoom" => args.zoom = Some(value("--zoom")?.parse().map_err(|_| "bad --zoom")?),
            "--screenshot" => args.screenshot = Some(value("--screenshot")?.into()),
            "--screenshot-delay-ms" => {
                let ms = value("--screenshot-delay-ms")?
                    .parse()
                    .map_err(|_| "bad delay")?;
                args.screenshot_delay = Duration::from_millis(ms);
            }
            "--bench-secs" => {
                let s: f64 = value("--bench-secs")?
                    .parse()
                    .map_err(|_| "bad --bench-secs")?;
                args.bench = Some(Duration::from_secs_f64(s.max(0.1)));
            }
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            _ => args.model = Some(arg.into()),
        }
    }
    Ok(args)
}

/// Rolling frame statistics.
#[derive(Default)]
struct FrameStats {
    last: Option<Instant>,
    /// Wall time between consecutive frames, ms.
    intervals: Vec<f64>,
    /// CPU time spent inside `ui` (including the viewport's command encoding), ms.
    cpu: Vec<f64>,
}

impl FrameStats {
    fn record(&mut self, frame_start: Instant, cpu_ms: f64) {
        if let Some(last) = self.last {
            self.intervals
                .push((frame_start - last).as_secs_f64() * 1e3);
        }
        self.last = Some(frame_start);
        self.cpu.push(cpu_ms);
    }

    fn recent(values: &[f64], n: usize) -> f64 {
        let tail = &values[values.len().saturating_sub(n)..];
        if tail.is_empty() {
            0.0
        } else {
            tail.iter().sum::<f64>() / tail.len() as f64
        }
    }

    fn summary(values: &[f64]) -> String {
        if values.is_empty() {
            return "n/a".into();
        }
        let mut v = values.to_vec();
        v.sort_by(f64::total_cmp);
        let pct = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        format!(
            "n={} mean={mean:.2} p50={:.2} p95={:.2} p99={:.2} max={:.2} ms",
            v.len(),
            pct(0.5),
            pct(0.95),
            pct(0.99),
            pct(1.0)
        )
    }
}

enum Load {
    Pending(Receiver<Result<(PreparedModel, f64, f64), String>>),
    Done,
    Failed(String),
}

struct ViewerApp {
    viewport: Viewport3D,
    load: Load,
    args: Args,
    shown_at: Option<Instant>,
    screenshot_requested: bool,
    screenshot_done: bool,
    bench_start: Option<Instant>,
    bench_done: bool,
    stats: FrameStats,
    status: String,
}

impl ViewerApp {
    fn new(cc: &eframe::CreationContext<'_>, args: Args) -> Result<Self, String> {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .ok_or("the wgpu renderer is required")?;
        if let Some(zoom) = args.zoom {
            cc.egui_ctx.set_zoom_factor(zoom);
        }
        let mut viewport = Viewport3D::new(rs);
        viewport.set_shading(args.shading);
        let load = match args.model.clone() {
            Some(path) => {
                let (tx, rx) = mpsc::channel();
                let ctx = cc.egui_ctx.clone();
                std::thread::spawn(move || {
                    let t0 = Instant::now();
                    let result = stlv_core::load_model(&path)
                        .map_err(|e| e.to_string())
                        .map(|m| {
                            let t1 = Instant::now();
                            let prepared = PreparedModel::new(Arc::new(m));
                            let prep_ms = t1.elapsed().as_secs_f64() * 1e3;
                            (prepared, (t1 - t0).as_secs_f64() * 1e3, prep_ms)
                        });
                    let _ = tx.send(result);
                    ctx.request_repaint();
                });
                Load::Pending(rx)
            }
            None => Load::Done,
        };
        let status = if args.model.is_some() {
            "Loading...".into()
        } else {
            "No model".into()
        };
        Ok(Self {
            viewport,
            load,
            args,
            shown_at: None,
            screenshot_requested: false,
            screenshot_done: false,
            bench_start: None,
            bench_done: false,
            stats: FrameStats::default(),
            status,
        })
    }

    fn poll_load(&mut self) {
        let Load::Pending(rx) = &self.load else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok((prepared, parse_ms, prep_ms))) => {
                let m = prepared.model.clone();
                let t = Instant::now();
                self.viewport.set_model(prepared);
                let upload_ms = t.elapsed().as_secs_f64() * 1e3;
                self.viewport.set_shading(self.args.shading);
                if let Some(p) = self.args.preset {
                    self.viewport.set_view_preset(p);
                }
                self.viewport
                    .set_auto_rotate(self.args.auto_rotate || self.args.bench.is_some());
                self.status = format!(
                    "{}  —  {}  —  {} vertices, {} triangles, {} lines  —  parse {parse_ms:.0} ms, \
                     prepare {prep_ms:.0} ms, upload {upload_ms:.0} ms",
                    m.filename,
                    m.format,
                    m.num_vertices(),
                    m.num_triangles(),
                    m.num_lines()
                );
                println!("{}", self.status);
                self.load = Load::Done;
            }
            Ok(Err(e)) => {
                eprintln!("load failed: {e}");
                self.status = format!("Error: {e}");
                self.load = Load::Failed(e);
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => self.load = Load::Failed("loader died".into()),
        }
    }

    /// Screenshot / benchmark automation; returns true when the app should close.
    fn automate(&mut self, ctx: &egui::Context) -> bool {
        if matches!(self.load, Load::Pending(_)) {
            return false;
        }
        let shown_at = *self.shown_at.get_or_insert_with(Instant::now);
        if let Some(len) = self.args.bench
            && !self.bench_done
        {
            let start = *self.bench_start.get_or_insert_with(|| {
                self.stats = FrameStats::default();
                Instant::now()
            });
            if start.elapsed() < len {
                ctx.request_repaint();
                return false;
            }
            self.bench_done = true;
            println!(
                "frame interval: {}",
                FrameStats::summary(&self.stats.intervals)
            );
            println!("ui cpu time:    {}", FrameStats::summary(&self.stats.cpu));
            self.viewport.set_auto_rotate(false);
        }
        if let Some(path) = self.args.screenshot.clone() {
            if !self.screenshot_requested {
                if shown_at.elapsed() >= self.args.screenshot_delay {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
                    self.screenshot_requested = true;
                }
                ctx.request_repaint();
                return false;
            }
            let image = ctx.input(|i| {
                i.raw.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            if let Some(image) = image {
                let [w, h] = image.size;
                let bytes: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
                match image::RgbaImage::from_raw(w as u32, h as u32, bytes)
                    .map(|img| img.save(&path))
                {
                    Some(Ok(())) => println!("saved {}", path.display()),
                    Some(Err(e)) => eprintln!("cannot save {}: {e}", path.display()),
                    None => eprintln!("bad screenshot buffer"),
                }
                self.screenshot_done = true;
            }
            ctx.request_repaint();
            return self.screenshot_done;
        }
        self.args.bench.is_some() && self.bench_done
    }
}

impl eframe::App for ViewerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let frame_start = Instant::now();
        self.poll_load();
        let ctx = ui.ctx().clone();

        if !self.args.bare {
            egui::Panel::bottom("controls").show(ui, |ui| {
                ui.horizontal(|ui| {
                    for preset in [
                        ViewPreset::Isometric,
                        ViewPreset::Top,
                        ViewPreset::Front,
                        ViewPreset::Right,
                    ] {
                        if ui.button(preset.label()).clicked() {
                            self.viewport.set_view_preset(preset);
                        }
                    }
                    ui.separator();
                    ui.label("Shading:");
                    let mut shading = self.viewport.shading();
                    egui::ComboBox::from_id_salt("shading")
                        .selected_text(shading.label())
                        .show_ui(ui, |ui| {
                            for mode in ShadingMode::ALL {
                                ui.selectable_value(&mut shading, mode, mode.label());
                            }
                        });
                    self.viewport.set_shading(shading);
                    ui.separator();
                    let mut auto = self.viewport.auto_rotate();
                    if ui.checkbox(&mut auto, "Auto-Rotate").changed() {
                        self.viewport.set_auto_rotate(auto);
                    }
                    let mut grid = self.viewport.grid_visible();
                    if ui.checkbox(&mut grid, "Grid").changed() {
                        self.viewport.set_grid_visible(grid);
                    }
                    ui.separator();
                    let interval = FrameStats::recent(&self.stats.intervals, 60);
                    let cpu = FrameStats::recent(&self.stats.cpu, 60);
                    ui.monospace(format!("frame {interval:5.1} ms  ui {cpu:4.1} ms"));
                });
                ui.label(&self.status);
                if let Load::Failed(e) = &self.load {
                    ui.colored_label(egui::Color32::LIGHT_RED, e);
                }
            });
        }
        egui::CentralPanel::no_frame().show(ui, |ui| {
            self.viewport.show(ui);
        });

        if self.automate(&ctx) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.stats
            .record(frame_start, frame_start.elapsed().as_secs_f64() * 1e3);
    }
}

fn main() -> eframe::Result {
    env_logger::init();
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let (w, h) = args.size.unwrap_or((1100.0, 760.0));
    let mut wgpu_options = stlv_render::wgpu_configuration();
    if args.no_vsync {
        wgpu_options.surface.present_mode = wgpu::PresentMode::AutoNoVsync;
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([w, h])
            .with_title("stlv-render viewer"),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options,
        ..Default::default()
    };
    eframe::run_native(
        "stlv-render viewer",
        options,
        Box::new(|cc| Ok(Box::new(ViewerApp::new(cc, args)?) as Box<dyn eframe::App>)),
    )
}
