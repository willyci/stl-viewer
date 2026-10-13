//! SVG: parsed (see `svg_load`) and rasterized with resvg on a worker thread. A coarse "base"
//! raster of the whole drawing backs a "detail" raster of only the visible region at the
//! exact screen scale, re-rendered after zoom/pan. The vector stays sharp at any zoom while
//! texture sizes stay bounded by the view size.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use egui::{Color32, ColorImage, Painter, Rect, TextureHandle, TextureOptions, Vec2, pos2, vec2};
use resvg::{tiny_skia, usvg};

use crate::svg_load::{STACK_SIZE, SvgDoc, svg_text};
use crate::view::subtract_rect;
use crate::{Waker, texture_side_limit};

/// Longest side of the whole-drawing fallback raster (`SAFE_TEXTURE_SIDE`).
const BASE_MAX_SIDE: f32 = crate::SAFE_TEXTURE_SIDE as f32;
/// Longest side of the visible-region raster (or the GPU's texture limit if smaller).
const DETAIL_MAX_SIDE: usize = 4096;

/// Rectangle in pixels of the drawing rendered at some scale (origin = drawing top-left).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PxRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl PxRect {
    fn width(&self) -> i32 {
        self.x1 - self.x0
    }

    fn height(&self) -> i32 {
        self.y1 - self.y0
    }

    fn is_empty(&self) -> bool {
        self.width() <= 0 || self.height() <= 0
    }

    fn contains(&self, o: &PxRect) -> bool {
        self.x0 <= o.x0 && self.y0 <= o.y0 && self.x1 >= o.x1 && self.y1 >= o.y1
    }
}

/// Grow the visible region `vis` by `margin`, within `[0, full)`, at most `max_side` wide.
pub(crate) fn plan_detail(vis: PxRect, full: [i32; 2], margin: [i32; 2], max_side: i32) -> PxRect {
    let axis = |lo: i32, hi: i32, full: i32, margin: i32| {
        let (lo, hi) = (lo.clamp(0, full), hi.clamp(0, full));
        let len = hi - lo;
        if len >= max_side {
            return (lo, lo + max_side);
        }
        let m = margin.min((max_side - len) / 2).max(0);
        (lo.saturating_sub(m).max(0), hi.saturating_add(m).min(full))
    };
    let (x0, x1) = axis(vis.x0, vis.x1, full[0], margin[0]);
    let (y0, y1) = axis(vis.y0, vis.y1, full[1], margin[1]);
    PxRect { x0, y0, x1, y1 }
}

fn same_scale(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-4 * b.abs().max(1e-6)
}

/// Render `rect` of the drawing scaled by `scale`; `None` for degenerate sizes.
fn render(tree: &usvg::Tree, scale: f32, rect: PxRect) -> Option<ColorImage> {
    if rect.is_empty() {
        return None;
    }
    let (w, h) = (
        u32::try_from(rect.width()).ok()?,
        u32::try_from(rect.height()).ok()?,
    );
    let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
    let t =
        tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, -rect.x0 as f32, -rect.y0 as f32);
    resvg::render(tree, t, &mut pixmap.as_mut());
    Some(ColorImage::from_rgba_premultiplied(
        [w as usize, h as usize],
        pixmap.data(),
    ))
}

fn base_scale(size: Vec2) -> f32 {
    (BASE_MAX_SIDE / size.max_elem().max(1e-3)).min(4.0)
}

fn full_px(size: Vec2, scale: f32) -> [i32; 2] {
    let f = |v: f32| ((v * scale).ceil().clamp(1.0, i32::MAX as f32 / 2.0)) as i32;
    [f(size.x), f(size.y)]
}

struct Request {
    scale: f32,
    rect: PxRect,
}

#[derive(Default)]
struct Shared {
    slot: Mutex<Option<Request>>,
    wake: Condvar,
    quit: AtomicBool,
}

enum Msg {
    Base {
        image: ColorImage,
        uv_max: Vec2,
    },
    Detail {
        image: ColorImage,
        scale: f32,
        rect: PxRect,
    },
    Error(String),
}

/// Parse `data`, sending the intrinsic size through `probe` as soon as it is known (the
/// system font scan for text can be slow).
fn parse(
    dir: Option<&Path>,
    data: &[u8],
    probe: &mut Option<Sender<Result<Vec2, String>>>,
) -> Result<(usvg::Tree, Vec2), String> {
    let text = svg_text(data)?;
    let doc = SvgDoc::parse(&text, dir)?;
    let tree = doc.tree(false)?;
    let size = vec2(tree.size().width(), tree.size().height());
    if let Some(p) = probe.take() {
        let _ = p.send(Ok(size));
    }
    let tree = if doc.has_text() {
        doc.tree(true)?
    } else {
        tree
    };
    Ok((tree, size))
}

/// Runs on a `STACK_SIZE` thread: the tree is parsed, rendered and dropped here.
fn worker(
    dir: Option<PathBuf>,
    data: Vec<u8>,
    probe: Sender<Result<Vec2, String>>,
    shared: Arc<Shared>,
    tx: Sender<Msg>,
    waker: Waker,
) {
    let mut probe = Some(probe);
    let (tree, size) = match parse(dir.as_deref(), &data, &mut probe) {
        Ok(t) => t,
        Err(e) => {
            // Before the size is known `SvgView::open` fails; afterwards the view shows it.
            match probe {
                Some(p) => drop(p.send(Err(e))),
                None => {
                    let _ = tx.send(Msg::Error(e));
                    waker.wake();
                }
            }
            return;
        }
    };
    drop(data);
    if shared.quit.load(Ordering::Relaxed) {
        return;
    }
    let bs = base_scale(size);
    let [bw, bh] = full_px(size, bs);
    if let Some(image) = render(
        &tree,
        bs,
        PxRect {
            x0: 0,
            y0: 0,
            x1: bw,
            y1: bh,
        },
    ) {
        let uv_max = vec2(size.x * bs / bw as f32, size.y * bs / bh as f32);
        if tx.send(Msg::Base { image, uv_max }).is_err() {
            return;
        }
        waker.wake();
    }
    loop {
        let req = {
            let mut slot = shared.slot.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if shared.quit.load(Ordering::Relaxed) {
                    return;
                }
                if let Some(r) = slot.take() {
                    break r;
                }
                slot = shared
                    .wake
                    .wait(slot)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        if let Some(image) = render(&tree, req.scale, req.rect) {
            if tx
                .send(Msg::Detail {
                    image,
                    scale: req.scale,
                    rect: req.rect,
                })
                .is_err()
            {
                return;
            }
            waker.wake();
        }
    }
}

struct Detail {
    tex: TextureHandle,
    scale: f32,
    rect: PxRect,
}

/// UI side of an SVG.
pub(crate) struct SvgView {
    size: Vec2,
    shared: Arc<Shared>,
    rx: Option<Receiver<Msg>>,
    base: Option<(TextureHandle, Vec2)>,
    detail: Option<Detail>,
    requested: Option<(f32, PxRect)>,
    error: Option<String>,
}

impl SvgView {
    /// Parse and render `data` (the SVG at `path`) on a worker thread, waiting only until
    /// the intrinsic size is known.
    pub fn open(path: &Path, data: Vec<u8>, waker: Waker) -> Result<Self, String> {
        let shared = Arc::new(Shared::default());
        let (tx, rx) = crossbeam_channel::unbounded();
        let (probe_tx, probe_rx) = crossbeam_channel::bounded(1);
        let s = shared.clone();
        let dir = path.parent().map(Path::to_path_buf);
        std::thread::Builder::new()
            .name("stlv-svg-render".into())
            .stack_size(STACK_SIZE)
            .spawn(move || worker(dir, data, probe_tx, s, tx, waker))
            .map_err(|e| format!("Failed to start SVG renderer: {e}"))?;
        let size = probe_rx
            .recv()
            .unwrap_or_else(|_| Err("SVG parser crashed".into()))?;
        Ok(Self {
            size,
            shared,
            rx: Some(rx),
            base: None,
            detail: None,
            requested: None,
            error: None,
        })
    }

    /// Intrinsic size in user units.
    pub fn size(&self) -> Vec2 {
        self.size
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn has_content(&self) -> bool {
        self.base.is_some() || self.detail.is_some()
    }

    /// Base shown and the detail raster matches the current view.
    pub fn is_ready(&self) -> bool {
        self.error.is_some()
            || (self.base.is_some()
                && match (&self.detail, self.requested) {
                    (Some(d), Some((s, r))) => same_scale(d.scale, s) && d.rect == r,
                    (_, None) => true,
                    (None, Some(_)) => false,
                })
    }

    /// Receive rasters and request a new detail raster if the view changed.
    pub fn poll(&mut self, ctx: &egui::Context, clip: Rect, image_rect: Rect, ppp: f32) {
        if let Some(rx) = &self.rx {
            loop {
                match rx.try_recv() {
                    Ok(Msg::Base { image, uv_max }) => {
                        let tex = ctx.load_texture("stlv-svg-base", image, TextureOptions::LINEAR);
                        self.base = Some((tex, uv_max));
                    }
                    Ok(Msg::Detail { image, scale, rect }) => match &mut self.detail {
                        Some(d) => {
                            d.tex.set(image, TextureOptions::LINEAR);
                            d.scale = scale;
                            d.rect = rect;
                        }
                        None => {
                            let tex =
                                ctx.load_texture("stlv-svg-detail", image, TextureOptions::LINEAR);
                            self.detail = Some(Detail { tex, scale, rect });
                        }
                    },
                    Ok(Msg::Error(e)) => {
                        self.error = Some(e);
                        self.rx = None;
                        return;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.rx = None;
                        return;
                    }
                }
            }
        }
        if self.size.x <= 0.0 || self.size.y <= 0.0 {
            return;
        }
        let scale = image_rect.width() * ppp / self.size.x;
        if !scale.is_finite() || scale <= 0.0 {
            return;
        }
        let full = full_px(self.size, scale);
        let vis = clip.intersect(image_rect);
        if !vis.is_positive() {
            return;
        }
        let to_px = |v: f32| v.clamp(-1e9, 1e9) as i32;
        let vis_px = PxRect {
            x0: to_px(((vis.min.x - image_rect.min.x) * ppp).floor()).clamp(0, full[0]),
            y0: to_px(((vis.min.y - image_rect.min.y) * ppp).floor()).clamp(0, full[1]),
            x1: to_px(((vis.max.x - image_rect.min.x) * ppp).ceil()).clamp(0, full[0]),
            y1: to_px(((vis.max.y - image_rect.min.y) * ppp).ceil()).clamp(0, full[1]),
        };
        if vis_px.is_empty() {
            return;
        }
        let covers = |s: f32, r: &PxRect| same_scale(s, scale) && r.contains(&vis_px);
        if self
            .detail
            .as_ref()
            .is_some_and(|d| covers(d.scale, &d.rect))
            || self.requested.is_some_and(|(s, r)| covers(s, &r))
        {
            return;
        }
        let margin = [
            (clip.width() * ppp * 0.25) as i32,
            (clip.height() * ppp * 0.25) as i32,
        ];
        let max_side = texture_side_limit(ctx, DETAIL_MAX_SIDE) as i32;
        let rect = plan_detail(vis_px, full, margin, max_side);
        self.requested = Some((scale, rect));
        *self
            .shared
            .slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Request { scale, rect });
        self.shared.wake.notify_one();
        ctx.request_repaint();
    }

    pub fn paint(&self, painter: &Painter, image_rect: Rect) {
        let zoom = image_rect.width() / self.size.x.max(1e-6);
        let detail_rect = self.detail.as_ref().map(|d| {
            let k = zoom / d.scale;
            Rect::from_min_max(
                image_rect.min + vec2(d.rect.x0 as f32, d.rect.y0 as f32) * k,
                image_rect.min + vec2(d.rect.x1 as f32, d.rect.y1 as f32) * k,
            )
        });
        if let Some((tex, uv_max)) = &self.base {
            let pieces = match detail_rect {
                Some(d) => subtract_rect(image_rect, d),
                None => vec![image_rect],
            };
            for piece in pieces {
                let to_uv = |p: egui::Pos2| {
                    let t = (p - image_rect.min) / image_rect.size();
                    pos2(t.x * uv_max.x, t.y * uv_max.y)
                };
                let uv = Rect::from_min_max(to_uv(piece.min), to_uv(piece.max));
                painter.image(tex.id(), piece, uv, Color32::WHITE);
            }
        }
        if let (Some(d), Some(r)) = (&self.detail, detail_rect) {
            painter.image(
                d.tex.id(),
                r,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
    }
}

impl Drop for SvgView {
    fn drop(&mut self) {
        self.shared.quit.store(true, Ordering::Relaxed);
        // Take the lock so the worker is either waiting (and gets notified) or will see `quit`.
        drop(
            self.shared
                .slot
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        self.shared.wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOGO: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 100" width="100%" height="100%">
        <rect x="10" y="10" width="80" height="80" fill="#ff0000"/></svg>"##;

    fn probe(data: &[u8]) -> Result<(usvg::Tree, Vec2), String> {
        parse(None, data, &mut None)
    }

    #[test]
    fn intrinsic_size_from_viewbox() {
        let (_, size) = probe(LOGO.as_bytes()).unwrap();
        assert_eq!(size, vec2(200.0, 100.0));
        let (_, size) =
            probe(br#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="32"/>"#).unwrap();
        assert_eq!(size, vec2(64.0, 32.0));
        assert!(probe(b"not svg").is_err());
        assert!(probe(b"").is_err());
        assert!(probe(b"\x1f\x8b broken gzip").is_err());
    }

    #[test]
    fn renders_region_at_scale() {
        let (tree, _) = probe(LOGO.as_bytes()).unwrap();
        // 4x scale, region covering drawing x 5..15, y 5..15 -> 40x40 px.
        let img = render(
            &tree,
            4.0,
            PxRect {
                x0: 20,
                y0: 20,
                x1: 60,
                y1: 60,
            },
        )
        .unwrap();
        assert_eq!(img.size, [40, 40]);
        assert_eq!(img.pixels[0], Color32::TRANSPARENT); // drawing (5, 5)
        assert_eq!(img.pixels[39 * 40 + 39], Color32::from_rgb(255, 0, 0)); // (14.9, 14.9)
        assert!(
            render(
                &tree,
                4.0,
                PxRect {
                    x0: 5,
                    y0: 5,
                    x1: 5,
                    y1: 9
                }
            )
            .is_none()
        );
    }

    #[test]
    fn detail_plan_is_bounded() {
        let vis = PxRect {
            x0: 100,
            y0: 100,
            x1: 900,
            y1: 700,
        };
        let r = plan_detail(vis, [100_000, 800], [200, 150], 4096);
        assert_eq!(
            r,
            PxRect {
                x0: 0,
                y0: 0,
                x1: 1100,
                y1: 800
            }
        );
        assert!(r.contains(&vis));
        let wide = PxRect {
            x0: 10,
            y0: 0,
            x1: 9000,
            y1: 10,
        };
        let r = plan_detail(wide, [10_000, 10], [500, 500], 4096);
        assert_eq!((r.x0, r.x1), (10, 4106));
        let r = plan_detail(
            PxRect {
                x0: 0,
                y0: 0,
                x1: 4000,
                y1: 10,
            },
            [10_000, 10],
            [500, 0],
            4096,
        );
        assert_eq!((r.x0, r.x1), (0, 4048));
    }
}
