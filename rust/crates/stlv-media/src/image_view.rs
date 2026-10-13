use std::path::Path;

use egui::{Align2, CursorIcon, FontId, PointerButton, Sense, Vec2, vec2};
use stlv_core::FileKind;
use stlv_core::palette::{TEXT_MUTED, VIEWER_BG};

use crate::gif_anim::{self, GifView};
use crate::raster::{self, RasterView};
use crate::svg::SvgView;
use crate::view::{ViewState, WHEEL_STEP, wheel_notches};
use crate::{Waker, color};

/// Native size of the loaded image (from the header / SVG intrinsic size).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    pub animated: bool,
}

enum Content {
    Raster(RasterView),
    Gif(GifView),
    Svg(SvgView),
}

impl Content {
    fn error(&self) -> Option<&str> {
        match self {
            Self::Raster(r) => r.error(),
            Self::Gif(g) => g.error(),
            Self::Svg(s) => s.error(),
        }
    }

    fn has_content(&self) -> bool {
        match self {
            Self::Raster(r) => r.has_content(),
            Self::Gif(g) => g.has_content(),
            Self::Svg(s) => s.has_content(),
        }
    }

    fn is_ready(&self) -> bool {
        match self {
            Self::Raster(r) => r.is_ready(),
            Self::Gif(g) => g.is_ready(),
            Self::Svg(s) => s.is_ready(),
        }
    }
}

/// Static image, animated GIF or SVG with fit-to-view, wheel zoom and drag pan.
///
/// `load` only reads the header (or parses the SVG); pixels are decoded on worker threads
/// and a spinner is shown until the first raster is ready.
#[derive(Default)]
pub struct ImageViewer {
    loaded: bool,
    info: Option<ImageInfo>,
    /// Intrinsic size in image pixels / SVG user units.
    size: Vec2,
    content: Option<Content>,
    view: ViewState,
    needs_fit: bool,
    /// Zoom requested before the first `show` (applied after fitting).
    pending_zoom: Option<f32>,
    /// View size at the last `show`.
    last_view: Vec2,
    waker: Waker,
}

impl ImageViewer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a jpg/png/bmp/tiff, gif or svg; zoom fits it to the view on the next `show`.
    pub fn load(&mut self, path: &Path) -> Result<ImageInfo, String> {
        self.clear();
        let waker = self.waker.clone();
        let (info, size, content) = match FileKind::from_path(path) {
            FileKind::Gif => {
                let h = gif_anim::probe(path)?;
                let info = ImageInfo {
                    width: h.width,
                    height: h.height,
                    animated: h.animated,
                };
                (
                    info,
                    info_size(info),
                    Content::Gif(GifView::spawn(path, h, waker)),
                )
            }
            FileKind::Svg => {
                let data = std::fs::read(path).map_err(|e| e.to_string())?;
                let view = SvgView::open(path, data, waker)?;
                let size = view.size();
                if !(size.x > 0.0 && size.y > 0.0) {
                    return Err("SVG has no size.".into());
                }
                let info = ImageInfo {
                    width: (size.x.round() as u32).max(1),
                    height: (size.y.round() as u32).max(1),
                    animated: false,
                };
                (info, size, Content::Svg(view))
            }
            FileKind::Image => {
                let (width, height) = raster::probe(path)?;
                let info = ImageInfo {
                    width,
                    height,
                    animated: false,
                };
                (
                    info,
                    info_size(info),
                    Content::Raster(RasterView::spawn(path, waker)),
                )
            }
            _ => {
                let ext = stlv_core::files::ext_lower(path);
                return Err(format!("Unsupported image format: .{ext}"));
            }
        };
        self.loaded = true;
        self.info = Some(info);
        self.size = size;
        self.content = Some(content);
        self.needs_fit = true;
        self.waker.wake();
        Ok(info)
    }

    pub fn clear(&mut self) {
        self.loaded = false;
        self.info = None;
        self.content = None;
        self.needs_fit = false;
        self.pending_zoom = None;
        self.view = ViewState::default();
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// Info of the loaded image.
    pub fn info(&self) -> Option<ImageInfo> {
        self.info
    }

    /// Current zoom (screen points per image pixel).
    pub fn zoom(&self) -> f32 {
        self.pending_zoom.unwrap_or(self.view.zoom)
    }

    /// Set the zoom (clamped to [0.05, 20]) keeping the view center fixed. Before the
    /// first `show` it is applied right after the initial fit.
    pub fn set_zoom(&mut self, zoom: f32) {
        if self.needs_fit {
            self.pending_zoom = Some(zoom);
        } else if self.view.zoom > 0.0 {
            self.pending_zoom = None;
            let factor = zoom / self.view.zoom;
            self.view
                .zoom_at(factor, self.last_view * 0.5, self.last_view, self.size);
        }
    }

    /// Re-fit the image to the view on the next `show`.
    pub fn fit(&mut self) {
        if self.loaded {
            self.needs_fit = true;
        }
    }

    /// Asynchronous decode error (the image could not be displayed after `load` succeeded).
    pub fn error(&self) -> Option<&str> {
        self.content.as_ref().and_then(Content::error)
    }

    /// Everything decoded/uploaded for the current view (GIFs: first frame shown).
    pub fn is_ready(&self) -> bool {
        self.content.as_ref().is_none_or(Content::is_ready)
    }

    /// GIF frame switches so far (0 for other images).
    pub fn frames_shown(&self) -> u64 {
        match &self.content {
            Some(Content::Gif(g)) => g.frames_shown(),
            _ => 0,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.waker.set(&ctx);
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, color(VIEWER_BG));
        let Some(content) = &mut self.content else {
            return;
        };
        let view = rect.size();
        self.last_view = view;
        if view.x <= 0.0 || view.y <= 0.0 {
            return;
        }
        if self.needs_fit {
            self.needs_fit = false;
            self.view = ViewState::fitted(view, self.size);
            if let Some(z) = self.pending_zoom.take() {
                self.view
                    .zoom_at(z / self.view.zoom, view * 0.5, view, self.size);
            }
        }

        // Wheel zoom anchored at the cursor; left-drag pans with a closed hand.
        if response.hovered() {
            let (notches, pinch) = ui.input(|i| {
                let mut notches = 0.0;
                let mut pinch = 1.0;
                for e in &i.events {
                    match e {
                        egui::Event::MouseWheel { unit, delta, .. } => {
                            notches += wheel_notches(*unit, *delta)
                        }
                        egui::Event::Zoom(f) => pinch *= f,
                        _ => {}
                    }
                }
                (notches, pinch)
            });
            let factor = WHEEL_STEP.powf(notches) * pinch;
            if factor != 1.0
                && let Some(p) = response.hover_pos()
            {
                self.view.zoom_at(factor, p - rect.min, view, self.size);
            }
        }
        if response.dragged_by(PointerButton::Primary) {
            self.view.pan(response.drag_delta(), view, self.size);
        }
        if response.is_pointer_button_down_on() && ui.input(|i| i.pointer.primary_down()) {
            ctx.set_cursor_icon(CursorIcon::Grabbing);
        }
        self.view.constrain(view, self.size);

        let ppp = ctx.pixels_per_point();
        let image_rect = self.view.image_rect(rect, self.size, ppp);
        match content {
            Content::Raster(r) => {
                r.poll(&ctx);
                r.paint(&painter, image_rect, image_rect.width() * ppp / self.size.x);
            }
            Content::Gif(g) => {
                g.poll(&ctx);
                g.paint(&painter, image_rect);
            }
            Content::Svg(s) => {
                s.poll(&ctx, rect, image_rect, ppp);
                s.paint(&painter, image_rect);
            }
        }
        if let Some(err) = content.error() {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                format!("Failed to load image file.\n{err}"),
                FontId::proportional(13.0),
                color(TEXT_MUTED),
            );
        } else if !content.has_content() {
            let size = 28.0;
            let spinner_rect = egui::Rect::from_center_size(rect.center(), vec2(size, size));
            ui.put(
                spinner_rect,
                egui::Spinner::new().size(size).color(color(TEXT_MUTED)),
            );
        }
    }
}

fn info_size(info: ImageInfo) -> Vec2 {
    vec2(info.width as f32, info.height as f32)
}
