//! The `--script` / `--screenshot` verification harness: runs script steps through the real
//! UI flows and widgets, records frame times and saves screenshots.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Color32, Event, Id, Ui, ViewportCommand, pos2, vec2};

use super::{PATH_EDIT_ID, SETTLE_TIMEOUT, StlViewerApp, absolute};
use crate::script::Step;
use crate::tree::TreeAction;

#[derive(Default)]
pub(super) struct HarnessState {
    pub(super) screenshot: Option<PathBuf>,
    pub(super) delay: Duration,
    pub(super) script: VecDeque<Step>,
    pub(super) script_active: bool,
    pub(super) wait_until: Option<Instant>,
    /// Deadline of a running `idle` step.
    pub(super) wait_idle: Option<Instant>,
    pub(super) shot_in_flight: Option<(PathBuf, bool)>,
    pub(super) exiting: bool,
    /// Synthetic input from raw-input script steps, one batch per frame.
    pub(super) input: VecDeque<Vec<Event>>,
    /// A batch was injected this frame: let the widgets react before the next step.
    pub(super) input_injected: bool,
    /// `(interval since the previous frame, CPU time of the previous frame)` in ms, recorded
    /// while a script runs (reported and cleared by the `frames` step).
    pub(super) frame_log: Vec<(f32, f32)>,
    pub(super) last_frame: Option<Instant>,
}

impl StlViewerApp {
    pub(super) fn run_script(&mut self) {
        if !self.harness.script_active {
            return;
        }
        loop {
            if self.harness.shot_in_flight.is_some() {
                return;
            }
            if !self.harness.input.is_empty() || std::mem::take(&mut self.harness.input_injected) {
                self.ctx.request_repaint();
                return;
            }
            if let Some(until) = self.harness.wait_until {
                let now = Instant::now();
                if now < until {
                    self.ctx.request_repaint_after(until - now);
                    return;
                }
                self.harness.wait_until = None;
            }
            if let Some(deadline) = self.harness.wait_idle {
                if !self.settled() && Instant::now() < deadline {
                    self.ctx.request_repaint_after(Duration::from_millis(30));
                    return;
                }
                if !self.settled() {
                    println!("[script] idle: gave up waiting after {SETTLE_TIMEOUT:?}");
                }
                self.harness.wait_idle = None;
            }
            let Some(step) = self.harness.script.pop_front() else {
                self.harness.script_active = false;
                println!("[script] done");
                return;
            };
            self.exec(step);
        }
    }

    fn exec(&mut self, step: Step) {
        let label = format!("{step:?}");
        match step {
            Step::Select(p) => {
                // A click on the row, which also focuses the tree.
                self.select_path(&absolute(&p));
                self.tree.request_focus();
            }
            Step::Expand(p) => {
                let p = absolute(&p);
                let p = self.tree.reveal(&p).unwrap_or(p);
                self.on_tree_action(TreeAction::Toggle(p));
            }
            Step::Thumb(name) => match self.grid.find(&name) {
                Some(p) => self.on_thumbnail_clicked(p),
                None => println!("[script] no card named {name}"),
            },
            Step::Next => self.step_file(1),
            Step::Prev => self.step_file(-1),
            Step::Media(on) => {
                if on != self.show_media {
                    self.set_show_media(on);
                }
            }
            Step::Path { text, enter } => {
                // Typing marks the edit dirty, so Enter and focus loss both navigate; the
                // edit ends up focused only after Enter on an invalid path.
                self.path_text = text;
                let valid = self.navigate_typed();
                let id = Id::new(PATH_EDIT_ID);
                self.ctx.memory_mut(|m| {
                    if enter && !valid {
                        m.request_focus(id);
                    } else {
                        m.surrender_focus(id);
                    }
                });
            }
            Step::Up => self.navigate_up(),
            Step::Shading(s) => self.viewport.set_shading(s),
            Step::Preset(p) => self.set_view(p),
            Step::Rotate(on) => self.viewport.set_auto_rotate(on),
            Step::Color => self.open_color_dialog(),
            Step::ColorOk([r, g, b]) => {
                self.color_dialog = None;
                self.apply_color(Color32::from_rgb(r, g, b));
            }
            Step::CopyPath => {
                let p = self.meta.full_path().to_string();
                if !p.is_empty() {
                    self.copy_path(p);
                }
            }
            Step::Dismiss => self.error = None,
            Step::Wait(ms) => {
                self.harness.wait_until = Some(Instant::now() + Duration::from_millis(ms))
            }
            Step::Idle => self.harness.wait_idle = Some(Instant::now() + SETTLE_TIMEOUT),
            Step::Shot(path) => self.request_screenshot(path, false),
            Step::Status => {}
            Step::Meta => println!("[script] meta {}", self.meta.dump()),
            Step::Tree => self.print_tree(),
            Step::Grid => println!(
                "[script] grid cards={} progress={:?}",
                self.grid.len(),
                self.grid.progress
            ),
            Step::Page => println!(
                "[script] page={:?} loading={:?} error={:?}",
                self.page, self.loading, self.error
            ),
            Step::Frames => {
                let log = std::mem::take(&mut self.harness.frame_log);
                println!("[script] frames {}", frame_summary(&log));
            }
            Step::Click { .. }
            | Step::Move(_)
            | Step::Wheel { .. }
            | Step::Drag { .. }
            | Step::Key { .. }
            | Step::Type(_) => {
                for event in input_events(step) {
                    self.queue_input(event);
                }
            }
            Step::Quit => {
                self.harness.script.clear();
                self.harness.exiting = true;
                self.shutdown();
                self.ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
        println!("[script] {label} => status: {}", self.status);
    }

    /// The `tree` step: the visible rows (indented by depth) and the selection.
    fn print_tree(&mut self) {
        let rows: Vec<String> = self
            .tree
            .rows()
            .iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth), r.entry.name))
            .collect();
        println!(
            "[script] tree root={} rows={}\n{}",
            self.tree.root().display(),
            rows.len(),
            rows.join("\n")
        );
        println!("[script] selected={:?}", self.tree.selected());
    }

    pub(super) fn record_frame(&mut self, frame: &eframe::Frame) {
        const MAX_LOG: usize = 100_000;
        let now = Instant::now();
        let prev = self.harness.last_frame.replace(now);
        if let Some(prev) = prev
            && self.harness.script_active
            && self.harness.frame_log.len() < MAX_LOG
        {
            let cpu = frame.info().cpu_usage.unwrap_or(0.0) * 1000.0;
            let interval = (now - prev).as_secs_f32() * 1000.0;
            self.harness.frame_log.push((interval, cpu));
        }
    }

    /// Deliver `event` to the widgets in a later frame of its own (see `raw_input_hook`).
    fn queue_input(&mut self, event: Event) {
        self.harness.input.push_back(vec![event]);
        self.ctx.request_repaint();
    }

    fn request_screenshot(&mut self, path: PathBuf, final_shot: bool) {
        self.ctx
            .send_viewport_cmd(ViewportCommand::Screenshot(Default::default()));
        self.harness.shot_in_flight = Some((path, final_shot));
        self.ctx.request_repaint();
    }

    pub(super) fn handle_screenshots(&mut self, ui: &Ui) {
        let image = ui.input(|i| {
            i.events.iter().find_map(|e| match e {
                Event::Screenshot { image, .. } => Some(Arc::clone(image)),
                _ => None,
            })
        });
        if let Some(image) = image
            && let Some((path, final_shot)) = self.harness.shot_in_flight.take()
        {
            match save_png(&image, &path) {
                Ok(()) => println!("[screenshot] saved {}", path.display()),
                Err(e) => eprintln!("[screenshot] {}: {e}", path.display()),
            }
            if final_shot {
                self.harness.exiting = true;
                self.shutdown();
                self.ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
        if self.harness.shot_in_flight.is_some() {
            self.ctx.request_repaint();
            return;
        }
        let Some(path) = self.harness.screenshot.clone() else {
            return;
        };
        if self.harness.script_active || self.harness.exiting {
            return;
        }
        let elapsed = self.started.elapsed();
        if elapsed >= self.harness.delay
            && (self.settled() || elapsed >= self.harness.delay + SETTLE_TIMEOUT)
        {
            self.harness.screenshot = None;
            self.request_screenshot(path, true);
        } else {
            self.ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
}

/// The synthetic input events of a raw-input step (none for other steps); each is delivered
/// in a frame of its own.
fn input_events(step: Step) -> Vec<Event> {
    let mut ev = Vec::new();
    match step {
        Step::Click { pos, double } => {
            let pos = pos2(pos[0], pos[1]);
            ev.push(Event::PointerMoved(pos));
            for _ in 0..if double { 2 } else { 1 } {
                ev.push(button_event(pos, true));
                ev.push(button_event(pos, false));
            }
        }
        Step::Move([x, y]) => ev.push(Event::PointerMoved(pos2(x, y))),
        Step::Wheel { pos, lines } => {
            ev.push(Event::PointerMoved(pos2(pos[0], pos[1])));
            // egui (like winit) reports scrolling up/away from the user as positive.
            ev.push(Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: vec2(0.0, -lines),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            });
        }
        Step::Drag { from, to } => {
            const DRAG_FRAMES: u8 = 12;
            let (from, to) = (pos2(from[0], from[1]), pos2(to[0], to[1]));
            ev.push(Event::PointerMoved(from));
            ev.push(button_event(from, true));
            for i in 1..=DRAG_FRAMES {
                let t = f32::from(i) / f32::from(DRAG_FRAMES);
                ev.push(Event::PointerMoved(from.lerp(to, t)));
            }
            ev.push(button_event(to, false));
        }
        Step::Key { key, modifiers } => {
            for pressed in [true, false] {
                ev.push(Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed,
                    repeat: false,
                    modifiers,
                });
            }
        }
        Step::Type(text) => ev.push(Event::Text(text)),
        _ => {}
    }
    ev
}

/// `n=.. interval mean/p95/max .. cpu mean/p95/max ..` (ms) for the `frames` script step.
fn frame_summary(log: &[(f32, f32)]) -> String {
    fn stats(mut v: Vec<f32>) -> String {
        if v.is_empty() {
            return "-".into();
        }
        v.sort_by(f32::total_cmp);
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        let p95 = v[(v.len() * 95 / 100).min(v.len() - 1)];
        let max = v[v.len() - 1];
        format!("mean {mean:.1} p95 {p95:.1} max {max:.1}")
    }
    format!(
        "n={} interval[{}] cpu[{}]",
        log.len(),
        stats(log.iter().map(|f| f.0).collect()),
        stats(log.iter().map(|f| f.1).collect())
    )
}

fn button_event(pos: egui::Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

fn save_png(image: &egui::ColorImage, path: &Path) -> Result<(), String> {
    let [w, h] = image.size;
    let mut bytes = Vec::with_capacity(w * h * 4);
    for c in &image.pixels {
        bytes.extend_from_slice(&c.to_srgba_unmultiplied());
    }
    let img = image::RgbaImage::from_raw(w as u32, h as u32, bytes).ok_or("bad screenshot size")?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    img.save(path).map_err(|e| e.to_string())
}
