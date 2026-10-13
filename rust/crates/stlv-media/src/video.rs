use std::path::Path;

use egui::{
    Align, Align2, Button, Color32, CornerRadius, FontFamily, FontId, Layout, Rect, RichText,
    Sense, Slider, Stroke, TextureHandle, TextureOptions, UiBuilder, pos2, vec2,
};
use stlv_core::format::format_mmss;
use stlv_core::palette::{
    ACCENT, ACCENT_BUTTON, BORDER, BUTTON_BG, BUTTON_BORDER, BUTTON_HOVER_BG, BUTTON_PRESSED_BG,
    SIDEBAR_BG, TEXT_BRIGHT, TEXT_MUTED, TEXT_STRONG,
};

use crate::mf::{Cmd, Engine};
use crate::{BOLD_FAMILY, Waker, color, texture_side_limit};

const BAR_HEIGHT: f32 = 50.0;

/// Expected effect of commands the engine thread has not applied yet, so the API and the
/// buttons reflect a click immediately.
#[derive(Clone, Copy, Debug, Default)]
struct Pending {
    seq: u64,
    paused: Option<bool>,
    position: Option<f64>,
}

/// Playback state as the UI should see it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Snapshot {
    position: f64,
    duration: Option<f64>,
    paused: bool,
    ended: bool,
}

impl Snapshot {
    /// Nothing open: the button offers "Play", the time reads 00:00 / 00:00.
    const IDLE: Self = Self {
        position: 0.0,
        duration: None,
        paused: true,
        ended: false,
    };

    fn playing(&self) -> bool {
        !self.paused && !self.ended
    }

    /// Overlay `pending` unless the engine already applied its last command.
    fn with_pending(mut self, pending: &Pending, applied: u64) -> Self {
        if applied < pending.seq {
            if let Some(p) = pending.paused {
                self.paused = p;
                if !p {
                    self.ended = false;
                }
            }
            if let Some(t) = pending.position {
                self.position = t;
                self.ended = false;
            }
        }
        self
    }
}

/// WebM playback with Play/Pause, seek slider, time label and Mute (app.py page 2).
///
/// Decoding and audio run in Windows Media Foundation on a dedicated thread; frames are
/// copied into an egui texture. The mute state persists across `open` calls (Python kept
/// one `QAudioOutput`).
#[derive(Default)]
pub struct VideoPlayer {
    engine: Option<Engine>,
    texture: Option<TextureHandle>,
    frames_shown: u64,
    muted: bool,
    /// Slider value (ms) while the user drags it.
    drag_ms: Option<f64>,
    last_output: Option<[u32; 2]>,
    pending: Pending,
    waker: Waker,
}

impl VideoPlayer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open and immediately start playing `path`.
    pub fn open(&mut self, path: &Path) -> Result<(), String> {
        self.stop();
        let engine = Engine::open(path, self.muted, self.waker.clone())?;
        self.engine = Some(engine);
        Ok(())
    }

    /// Stop playback and release the media.
    pub fn stop(&mut self) {
        if let Some(mut engine) = self.engine.take() {
            engine.shutdown();
        }
        self.texture = None;
        self.frames_shown = 0;
        self.drag_ms = None;
        self.last_output = None;
        self.pending = Pending::default();
    }

    pub fn is_open(&self) -> bool {
        self.engine.is_some()
    }

    /// Resume (or restart from the beginning once ended).
    pub fn play(&mut self) {
        if let Some(seq) = self.send(Cmd::Play) {
            self.pending = Pending {
                seq,
                paused: Some(false),
                ..self.pending
            };
        }
    }

    pub fn pause(&mut self) {
        if let Some(seq) = self.send(Cmd::Pause) {
            self.pending = Pending {
                seq,
                paused: Some(true),
                ..self.pending
            };
        }
    }

    /// The Play/Pause button: pause when playing, otherwise play (from the start once ended).
    pub fn toggle_playback(&mut self) {
        if self.is_playing() {
            self.pause()
        } else {
            self.play()
        }
    }

    /// Playing (not paused and not at the end).
    pub fn is_playing(&self) -> bool {
        self.snapshot().is_some_and(|s| s.playing())
    }

    /// Reached the end of the media (the button then reads "Play").
    pub fn is_ended(&self) -> bool {
        self.snapshot().is_some_and(|s| s.ended)
    }

    /// Seek to `ms` (clamped by the engine to the media length).
    pub fn seek_ms(&mut self, ms: u64) {
        let t = ms as f64 / 1000.0;
        if let Some(seq) = self.send(Cmd::Seek(t)) {
            self.pending = Pending {
                seq,
                position: Some(t),
                ..self.pending
            };
        }
    }

    pub fn position_ms(&self) -> u64 {
        self.snapshot().map_or(0, |s| secs_to_ms(s.position))
    }

    /// Media length; `None` while unknown.
    pub fn duration_ms(&self) -> Option<u64> {
        self.snapshot().and_then(|s| s.duration.map(secs_to_ms))
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        self.send(Cmd::SetMuted(muted));
    }

    pub fn is_muted(&self) -> bool {
        self.muted
    }

    /// Native video size (display aspect), once known.
    pub fn video_size(&self) -> Option<[u32; 2]> {
        self.engine.as_ref().and_then(|e| e.status().video_size)
    }

    /// Playback error reported after `open` returned.
    pub fn error(&self) -> Option<String> {
        self.engine.as_ref().and_then(|e| e.status().error.clone())
    }

    /// Frames uploaded to the screen since `open`.
    pub fn frames_shown(&self) -> u64 {
        self.frames_shown
    }

    fn send(&mut self, cmd: Cmd) -> Option<u64> {
        self.engine.as_mut().map(|e| e.send(cmd))
    }

    fn snapshot(&self) -> Option<Snapshot> {
        let s = self.engine.as_ref()?.status();
        let snap = Snapshot {
            position: s.position,
            duration: s.duration,
            paused: s.paused,
            ended: s.ended,
        };
        Some(snap.with_pending(&self.pending, s.applied))
    }

    /// Video surface (black, aspect-fit) above a 50 px control bar.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.waker.set(&ctx);
        let rect = ui.available_rect_before_wrap();
        ui.allocate_rect(rect, Sense::hover());
        let bar_h = BAR_HEIGHT.min(rect.height());
        let video_rect = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y - bar_h));
        let bar_rect = Rect::from_min_max(pos2(rect.min.x, rect.max.y - bar_h), rect.max);

        // Take the newest frame without holding the status lock during the upload.
        let (frame, video_size, error) = match &self.engine {
            Some(e) => {
                let mut s = e.status();
                (s.frame.take(), s.video_size, s.error.clone())
            }
            None => (None, None, None),
        };
        let snap = self.snapshot().unwrap_or(Snapshot::IDLE);
        // Frames come at the requested display size; one transferred at native size before
        // the first request may exceed the GPU's texture limit and is skipped.
        let limit = texture_side_limit(&ctx, usize::MAX);
        if let Some(f) = frame.filter(|f| f.size[0] <= limit && f.size[1] <= limit) {
            let image = egui::ColorImage::new(f.size, f.pixels);
            match &mut self.texture {
                Some(t) => t.set(image, TextureOptions::LINEAR),
                None => {
                    self.texture =
                        Some(ctx.load_texture("stlv-video", image, TextureOptions::LINEAR))
                }
            }
            self.frames_shown += 1;
        }

        let painter = ui.painter_at(rect);
        painter.rect_filled(video_rect, 0.0, Color32::BLACK);
        if let Some([vw, vh]) = video_size {
            let fit = aspect_fit(video_rect, vw as f32 / vh as f32);
            let ppp = ctx.pixels_per_point();
            let side = |v: f32| ((v * ppp).round().max(1.0) as usize).min(limit) as u32;
            let out = [side(fit.width()), side(fit.height())];
            if self.last_output != Some(out) {
                self.last_output = Some(out);
                self.send(Cmd::OutputSize(out));
            }
            if let Some(t) = &self.texture {
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                painter.image(t.id(), fit, uv, Color32::WHITE);
            }
        }
        if let Some(err) = &error {
            painter.text(
                video_rect.center(),
                Align2::CENTER_CENTER,
                format!("Cannot play video.\n{err}"),
                FontId::proportional(13.0),
                color(TEXT_MUTED),
            );
        }

        self.control_bar(ui, bar_rect, snap);
    }

    fn control_bar(&mut self, ui: &mut egui::Ui, bar: Rect, snap: Snapshot) {
        let painter = ui.painter_at(bar);
        painter.rect_filled(bar, 0.0, color(SIDEBAR_BG));
        painter.hline(
            bar.x_range(),
            bar.top() + 0.5,
            Stroke::new(1.0, color(BORDER)),
        );
        let inner = Rect::from_min_max(bar.min + vec2(15.0, 5.0), bar.max - vec2(15.0, 5.0));
        if !inner.is_positive() {
            return;
        }
        let mut ui = ui.new_child(
            UiBuilder::new()
                .max_rect(inner)
                .layout(Layout::left_to_right(Align::Center)),
        );
        style_controls(ui.style_mut());
        ui.spacing_mut().item_spacing.x = 15.0;
        let enabled = self.engine.is_some();
        let button_h = 30.0f32.min(inner.height());
        let font = button_font(ui.ctx());

        let play_text = if snap.playing() { "Pause" } else { "Play" };
        let play_button =
            Button::new(RichText::new(play_text).font(font.clone())).min_size(vec2(80.0, button_h));
        if ui.add_enabled(enabled, play_button).clicked() {
            self.toggle_playback();
        }

        let dur_ms = snap.duration.map_or(0.0, |d| (d * 1000.0).floor());
        let pos_ms = (snap.position * 1000.0).clamp(0.0, dur_ms);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let mute_text = if self.muted { "Unmute" } else { "Mute" };
            let mute_button =
                Button::new(RichText::new(mute_text).font(font)).min_size(vec2(70.0, button_h));
            if ui.add_enabled(enabled, mute_button).clicked() {
                self.set_muted(!self.muted);
            }
            let shown_pos = self.drag_ms.unwrap_or(pos_ms);
            let time = format!(
                "{} / {}",
                format_mmss(shown_pos as u64),
                format_mmss(dur_ms as u64)
            );
            ui.label(RichText::new(time).size(11.0).color(color(TEXT_MUTED)));

            ui.spacing_mut().slider_width = ui.available_width().max(20.0);
            let mut value = shown_pos;
            // An empty range would park the handle mid-rail; Qt shows it at 0.
            let slider = Slider::new(&mut value, 0.0..=dur_ms.max(1.0))
                .show_value(false)
                .trailing_fill(true);
            let resp = ui.add_enabled(enabled && dur_ms > 0.0, slider);
            if resp.dragged() || resp.drag_started() {
                if self.drag_ms != Some(value) {
                    self.drag_ms = Some(value);
                    self.seek_ms(value as u64);
                }
            } else {
                if resp.changed() {
                    self.seek_ms(value as u64); // click on the rail / keyboard
                }
                self.drag_ms = None;
            }
        });
    }
}

impl Drop for VideoPlayer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn secs_to_ms(s: f64) -> u64 {
    if s.is_finite() && s > 0.0 {
        (s * 1000.0) as u64
    } else {
        0
    }
}

/// Largest rect of the given aspect (w / h) centered in `area`.
fn aspect_fit(area: Rect, aspect: f32) -> Rect {
    if !(aspect.is_finite() && aspect > 0.0 && area.is_positive()) {
        return area;
    }
    let (w, h) = if area.width() / area.height() > aspect {
        (area.height() * aspect, area.height())
    } else {
        (area.width(), area.width() / aspect)
    };
    Rect::from_center_size(area.center(), vec2(w, h))
}

/// styles.py buttons: bold 12 px (the app's bold family when registered).
fn button_font(ctx: &egui::Context) -> FontId {
    let bold = FontFamily::Name(BOLD_FAMILY.into());
    let has_bold = ctx.fonts(|f| f.definitions().families.contains_key(&bold));
    FontId::new(
        12.0,
        if has_bold {
            bold
        } else {
            FontFamily::Proportional
        },
    )
}

/// styles.py `QPushButton` look for the control bar, plus an accent slider.
fn style_controls(style: &mut egui::Style) {
    let v = &mut style.visuals;
    let radius = CornerRadius::same(6);
    let w = &mut v.widgets;
    w.inactive.weak_bg_fill = color(BUTTON_BG);
    w.inactive.bg_fill = color(BORDER);
    w.inactive.bg_stroke = Stroke::new(1.0, color(BUTTON_BORDER));
    w.inactive.fg_stroke = Stroke::new(1.0, color(TEXT_BRIGHT));
    w.hovered.weak_bg_fill = color(BUTTON_HOVER_BG);
    w.hovered.bg_fill = color(BUTTON_HOVER_BG);
    w.hovered.bg_stroke = Stroke::new(1.0, color(ACCENT));
    w.hovered.fg_stroke = Stroke::new(1.0, color(TEXT_STRONG));
    w.active.weak_bg_fill = color(BUTTON_PRESSED_BG);
    w.active.bg_fill = color(BUTTON_PRESSED_BG);
    w.active.bg_stroke = Stroke::new(1.0, color(ACCENT_BUTTON));
    w.active.fg_stroke = Stroke::new(1.0, color(TEXT_STRONG));
    w.noninteractive.fg_stroke = Stroke::new(1.0, color(TEXT_MUTED));
    for s in [&mut w.inactive, &mut w.hovered, &mut w.active] {
        s.corner_radius = radius;
        s.expansion = 0.0;
    }
    v.selection.bg_fill = color(ACCENT_BUTTON);
    v.selection.stroke = Stroke::new(1.0, color(ACCENT));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aspect_fit_letterboxes() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 500.0));
        let r = aspect_fit(area, 16.0 / 9.0);
        assert!((r.width() - 888.888_9).abs() < 1e-3 && r.height() == 500.0);
        assert_eq!(r.center(), area.center());
        let r = aspect_fit(area, 4.0);
        assert_eq!((r.width(), r.height()), (1000.0, 250.0));
        assert_eq!(aspect_fit(area, f32::NAN), area);
    }

    #[test]
    fn pending_commands_override_until_applied() {
        let engine = Snapshot {
            position: 24.6,
            duration: Some(24.6),
            paused: true,
            ended: true,
        };
        let play = Pending {
            seq: 3,
            paused: Some(false),
            position: None,
        };
        let s = engine.with_pending(&play, 2);
        assert!(s.playing() && s.position == 24.6);
        // Applied: the engine's own state wins again.
        assert_eq!(engine.with_pending(&play, 3), engine);
        let seek = Pending {
            seq: 4,
            paused: Some(true),
            position: Some(3.0),
        };
        let s = engine.with_pending(&seek, 3);
        assert!(s.paused && !s.ended && s.position == 3.0);
    }

    #[test]
    fn idle_player_is_inert() {
        let mut p = VideoPlayer::new();
        assert!(!p.is_open() && !p.is_playing());
        p.play();
        p.seek_ms(1000);
        p.toggle_playback();
        p.set_muted(true);
        assert!(p.is_muted());
        assert_eq!(p.position_ms(), 0);
        assert_eq!(p.duration_ms(), None);
        p.stop();
        assert!(p.open(Path::new("Z:/definitely/missing.webm")).is_err());
        assert!(!p.is_open());
    }
}
