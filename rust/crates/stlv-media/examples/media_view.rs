//! Standalone viewer for one image / GIF / SVG / WebM file (verification harness).
//!
//! ```text
//! media_view <file> [--screenshot <png>] [--screenshot-delay-ms N] [--zoom F]
//!            [--window-size WxH] [--muted] [--at <ms>:<action>]...
//! ```
//! `--zoom F` sets the absolute zoom (points per image pixel) after the initial fit.
//! Scripted actions (run once, `ms` after start): `play`, `pause`, `toggle`, `seek=<ms>`,
//! `mute`, `unmute`, `stop`, `reopen`, `zoom=<f>`, `fit`, `shot=<png>`, `status`.
//! With `--screenshot` the window saves a PNG once the view is ready and the delay has
//! passed, prints a status line and exits.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use stlv_core::FileKind;
use stlv_media::{BOLD_FAMILY, ImageViewer, VideoPlayer};

struct Args {
    file: PathBuf,
    screenshot: Option<PathBuf>,
    delay: Duration,
    zoom: Option<f32>,
    window: [f32; 2],
    muted: bool,
    script: Vec<(Duration, String)>,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut args = Args {
        file: PathBuf::new(),
        screenshot: None,
        delay: Duration::from_millis(1500),
        zoom: None,
        window: [960.0, 700.0],
        muted: false,
        script: Vec::new(),
    };
    let mut file = None;
    while let Some(a) = it.next() {
        let mut value = || it.next().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--screenshot" => args.screenshot = Some(PathBuf::from(value()?)),
            "--screenshot-delay-ms" => {
                args.delay =
                    Duration::from_millis(value()?.parse().map_err(|e| format!("{a}: {e}"))?)
            }
            "--zoom" => args.zoom = Some(value()?.parse().map_err(|e| format!("{a}: {e}"))?),
            "--window-size" => {
                let v = value()?;
                let (w, h) = v.split_once('x').ok_or("--window-size WxH")?;
                args.window = [
                    w.parse().map_err(|_| "bad width")?,
                    h.parse().map_err(|_| "bad height")?,
                ];
            }
            "--muted" => args.muted = true,
            "--at" => {
                let v = value()?;
                let (ms, action) = v.split_once(':').ok_or("--at <ms>:<action>")?;
                let ms: u64 = ms.parse().map_err(|_| "--at: bad time")?;
                args.script
                    .push((Duration::from_millis(ms), action.to_string()));
            }
            _ if file.is_none() && !a.starts_with("--") => file = Some(PathBuf::from(a)),
            _ => return Err(format!("unexpected argument {a}")),
        }
    }
    args.file = file.ok_or("usage: media_view <file> [--screenshot out.png] ...")?;
    args.script.sort_by_key(|(t, _)| *t);
    Ok(args)
}

enum Viewer {
    Image(ImageViewer),
    Video(VideoPlayer),
}

struct App {
    args: Args,
    viewer: Viewer,
    start: Instant,
    next_action: usize,
    shot_requested: bool,
    /// Longest gap between frames while the view was still loading (UI stall detector).
    last_frame: Option<Instant>,
    max_loading_frame_ms: u128,
    ready_at: Option<Duration>,
}

impl App {
    fn new(args: Args) -> Self {
        let viewer = if FileKind::from_path(&args.file) == FileKind::Webm {
            let mut v = VideoPlayer::new();
            v.set_muted(args.muted);
            let t = Instant::now();
            match v.open(&args.file) {
                Ok(()) => println!("open: ok in {} ms", t.elapsed().as_millis()),
                Err(e) => println!("open: error: {e}"),
            }
            Viewer::Video(v)
        } else {
            let mut v = ImageViewer::new();
            let t = Instant::now();
            match v.load(&args.file) {
                Ok(info) => println!("load: {info:?} in {} ms", t.elapsed().as_millis()),
                Err(e) => println!("load: error: {e}"),
            }
            if let Some(z) = args.zoom {
                v.set_zoom(z);
            }
            Viewer::Image(v)
        };
        Self {
            args,
            viewer,
            start: Instant::now(),
            next_action: 0,
            shot_requested: false,
            last_frame: None,
            max_loading_frame_ms: 0,
            ready_at: None,
        }
    }

    fn status(&self) -> String {
        let t = self.start.elapsed().as_millis();
        let timing = format!(
            "ready_at={:?} max_loading_frame={} ms",
            self.ready_at, self.max_loading_frame_ms
        );
        let state = match &self.viewer {
            Viewer::Image(v) => format!(
                "[{t} ms] image: info={:?} zoom={:.4} ready={} frames_shown={} error={:?}",
                v.info(),
                v.zoom(),
                v.is_ready(),
                v.frames_shown(),
                v.error()
            ),
            Viewer::Video(v) => format!(
                "[{t} ms] video: open={} playing={} ended={} pos={} ms dur={:?} ms muted={} size={:?} frames_shown={} error={:?}",
                v.is_open(),
                v.is_playing(),
                v.is_ended(),
                v.position_ms(),
                v.duration_ms(),
                v.is_muted(),
                v.video_size(),
                v.frames_shown(),
                v.error()
            ),
        };
        format!("{state} {timing}")
    }

    fn run_action(&mut self, ctx: &egui::Context, action: &str) {
        let (name, arg) = action.split_once('=').unwrap_or((action, ""));
        match (&mut self.viewer, name) {
            (Viewer::Video(v), "play") => v.play(),
            (Viewer::Video(v), "pause") => v.pause(),
            (Viewer::Video(v), "toggle") => v.toggle_playback(),
            (Viewer::Video(v), "seek") => v.seek_ms(arg.parse().unwrap_or(0)),
            (Viewer::Video(v), "mute") => v.set_muted(true),
            (Viewer::Video(v), "unmute") => v.set_muted(false),
            (Viewer::Video(v), "stop") => v.stop(),
            (Viewer::Video(v), "reopen") => {
                let t = Instant::now();
                let r = v.open(&self.args.file);
                println!("reopen: {r:?} in {} ms", t.elapsed().as_millis());
            }
            (Viewer::Image(v), "zoom") => v.set_zoom(arg.parse().unwrap_or(1.0)),
            (Viewer::Image(v), "fit") => v.fit(),
            (Viewer::Image(v), "reopen") => println!("reload: {:?}", v.load(&self.args.file)),
            (_, "shot") => {
                let path = PathBuf::from(arg);
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(path)));
            }
            (_, "status") => {}
            _ => println!("unknown action {action}"),
        }
        println!("action {action}: {}", self.status());
    }

    fn ready(&self) -> bool {
        match &self.viewer {
            Viewer::Image(v) => v.is_ready(),
            Viewer::Video(v) => v.frames_shown() > 0 || v.error().is_some() || !v.is_open(),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = Instant::now();
        if self.ready_at.is_none() {
            if let Some(last) = self.last_frame {
                self.max_loading_frame_ms = self.max_loading_frame_ms.max((now - last).as_millis());
            }
            if self.ready() {
                self.ready_at = Some(self.start.elapsed());
            }
        }
        self.last_frame = Some(now);
        // Screenshots arrive as events.
        let shots: Vec<(PathBuf, std::sync::Arc<egui::ColorImage>)> = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Screenshot {
                        user_data, image, ..
                    } => user_data
                        .data
                        .as_ref()
                        .and_then(|d| d.downcast_ref::<PathBuf>())
                        .map(|p| (p.clone(), image.clone())),
                    _ => None,
                })
                .collect()
        });
        for (path, image) in shots {
            let [w, h] = image.size;
            match image::save_buffer(
                &path,
                image.as_raw(),
                w as u32,
                h as u32,
                image::ExtendedColorType::Rgba8,
            ) {
                Ok(()) => println!("saved {} ({w}x{h}): {}", path.display(), self.status()),
                Err(e) => println!("failed to save {}: {e}", path.display()),
            }
            if Some(&path) == self.args.screenshot.as_ref() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }

        let elapsed = self.start.elapsed();
        while let Some((at, action)) = self.args.script.get(self.next_action).cloned() {
            if elapsed < at {
                ctx.request_repaint_after(at - elapsed);
                break;
            }
            self.next_action += 1;
            self.run_action(&ctx, &action);
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| match &mut self.viewer {
                Viewer::Image(v) => v.show(ui),
                Viewer::Video(v) => v.show(ui),
            });

        if let Some(path) = self.args.screenshot.clone()
            && !self.shot_requested
        {
            let script_done = self.next_action >= self.args.script.len();
            let due = elapsed >= self.args.delay && script_done;
            let overdue = elapsed >= self.args.delay + Duration::from_secs(15);
            if (due && self.ready()) || overdue {
                self.shot_requested = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(path)));
            } else {
                ctx.request_repaint_after(Duration::from_millis(20));
            }
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let t = Instant::now();
        self.viewer = Viewer::Image(ImageViewer::new());
        println!(
            "[{} ms] viewer released in {} ms",
            self.start.elapsed().as_millis(),
            t.elapsed().as_millis()
        );
    }
}

fn main() -> eframe::Result {
    env_logger::init();
    let t0 = Instant::now();
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let title = format!("media_view - {}", args.file.display());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size(args.window),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    let result = eframe::run_native(
        "media_view",
        options,
        Box::new(|cc| {
            install_fonts(&cc.egui_ctx);
            Ok(Box::new(App::new(args)))
        }),
    );
    println!("[{} ms] window closed", t0.elapsed().as_millis());
    result
}

/// Segoe UI plus the "bold" family the app registers, so screenshots match the app.
fn install_fonts(ctx: &egui::Context) {
    use egui::{FontData, FontDefinitions, FontFamily};
    let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
    let load = |name: &str| std::fs::read(PathBuf::from(&windir).join("Fonts").join(name)).ok();
    let (Some(regular), Some(bold)) = (load("segoeui.ttf"), load("segoeuib.ttf")) else {
        return;
    };
    let mut defs = FontDefinitions::default();
    let defaults = defs
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    defs.font_data.insert(
        "segoe-ui".into(),
        std::sync::Arc::new(FontData::from_owned(regular)),
    );
    defs.font_data.insert(
        "segoe-ui-bold".into(),
        std::sync::Arc::new(FontData::from_owned(bold)),
    );
    let family = |first: &str| {
        std::iter::once(first.to_string())
            .chain(defaults.iter().cloned())
            .collect()
    };
    defs.families
        .insert(FontFamily::Proportional, family("segoe-ui"));
    defs.families.insert(
        FontFamily::Name(BOLD_FAMILY.into()),
        family("segoe-ui-bold"),
    );
    ctx.set_fonts(defs);
}
