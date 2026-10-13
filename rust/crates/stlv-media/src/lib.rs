//! Media viewers for the right-hand viewport: zoomable image / animated GIF / SVG viewer
//! and a WebM video player (Windows Media Foundation).

mod gif_anim;
mod image_view;
mod mf;
mod raster;
mod svg;
mod svg_load;
mod video;
mod view;

pub use image_view::{ImageInfo, ImageViewer};
pub use svg_load::{rasterize_svg_fit, svg_size};
pub use video::VideoPlayer;
pub use view::{MAX_ZOOM, MIN_ZOOM, WHEEL_STEP};

use std::sync::{Arc, OnceLock};

use egui::Color32;

/// Font family under which the app registers its bold face (Segoe UI Bold). The video
/// controls use it when it is registered and the proportional family otherwise.
pub const BOLD_FAMILY: &str = "bold";

/// Palette color (`0xRRGGBB`).
pub(crate) fn color(hex: u32) -> Color32 {
    let [r, g, b] = stlv_core::palette::rgb(hex);
    Color32::from_rgb(r, g, b)
}

/// Texture side every wgpu backend supports (the downlevel minimum); egui also assumes it
/// until the integration reports the GPU's real limit.
pub(crate) const SAFE_TEXTURE_SIDE: usize = 2048;

/// Largest texture side the renderer accepts (egui's `max_texture_side`), capped at `cap`.
pub(crate) fn texture_side_limit(ctx: &egui::Context, cap: usize) -> usize {
    ctx.input(|i| i.max_texture_side).min(cap)
}

/// Lets worker threads started before the first `show` request a repaint once the widget
/// knows its `egui::Context`.
#[derive(Clone, Default)]
pub(crate) struct Waker(Arc<OnceLock<egui::Context>>);

impl Waker {
    pub fn set(&self, ctx: &egui::Context) {
        let _ = self.0.set(ctx.clone());
    }

    pub fn wake(&self) {
        if let Some(ctx) = self.0.get() {
            ctx.request_repaint();
        }
    }

    pub fn ctx(&self) -> Option<&egui::Context> {
        self.0.get()
    }
}
