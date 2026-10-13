//! Headless egui driver: runs widgets frame by frame and records texture uploads.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::epaint::ImageData;
use egui::{ColorImage, Event, RawInput, Rect, pos2, vec2};

pub struct Harness {
    pub ctx: egui::Context,
    pub screen: Rect,
    start: Instant,
    /// Every full-texture upload so far, in order.
    pub uploads: Vec<Arc<ColorImage>>,
}

impl Harness {
    pub fn new(w: f32, h: f32) -> Self {
        Self {
            ctx: egui::Context::default(),
            screen: Rect::from_min_size(pos2(0.0, 0.0), vec2(w, h)),
            start: Instant::now(),
            uploads: Vec::new(),
        }
    }

    /// Run one frame with `events`, drawing `ui_fn` over the whole screen.
    pub fn frame(&mut self, events: Vec<Event>, mut ui_fn: impl FnMut(&mut egui::Ui)) {
        let input = RawInput {
            screen_rect: Some(self.screen),
            time: Some(self.start.elapsed().as_secs_f64()),
            events,
            ..Default::default()
        };
        let out = self.ctx.run_ui(input, |ui| ui_fn(ui));
        for deltas in out.textures_delta.set.values() {
            for d in deltas {
                let ImageData::Color(img) = &d.image;
                if d.pos.is_none() {
                    self.uploads.push(img.clone());
                }
            }
        }
        out.drop_without_applying_deltas();
    }

    /// Run frames (~60 Hz) showing `widget` until `done(widget)` or `timeout`; returns
    /// whether `done` became true.
    pub fn run_until<T>(
        &mut self,
        widget: &mut T,
        timeout: Duration,
        mut show: impl FnMut(&mut T, &mut egui::Ui),
        mut done: impl FnMut(&T) -> bool,
    ) -> bool {
        let end = Instant::now() + timeout;
        loop {
            self.frame(Vec::new(), |ui| show(widget, ui));
            if done(widget) {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(16));
        }
    }

    /// Uploads (excluding egui's font atlas) of exactly `size`.
    pub fn uploads_of_size(&self, size: [usize; 2]) -> Vec<Arc<ColorImage>> {
        self.uploads
            .iter()
            .filter(|i| i.size == size)
            .cloned()
            .collect()
    }
}

/// Fraction of pixels brighter than `threshold` (max channel).
pub fn bright_fraction(img: &ColorImage, threshold: u8) -> f64 {
    let n = img
        .pixels
        .iter()
        .filter(|p| p.r().max(p.g()).max(p.b()) > threshold)
        .count();
    n as f64 / img.pixels.len().max(1) as f64
}

/// A fresh, empty directory under the system temp dir, removed when dropped (also when
/// the test fails).
pub struct TempDir(PathBuf);

impl std::ops::Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn temp_dir(name: &str) -> TempDir {
    let dir = std::env::temp_dir().join(format!("stlv-media-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    TempDir(dir)
}
