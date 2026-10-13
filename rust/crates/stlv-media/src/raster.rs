//! Static raster images (jpg/png/bmp/tiff). Decoding runs on a worker thread that builds a
//! premultiplied mip pyramid cut into <= 2048 px tiles: egui-wgpu textures have no mipmaps
//! (minifying a big image would alias) and a size limit (8000 px images must still work).
//! Tiles are sent coarsest level first and uploaded a few per frame, so a huge image shows
//! a preview almost immediately and the UI never stalls on one giant upload.
//!
//! Only one decode runs at a time, and a superseded one (the view was dropped) stops at its
//! next read or stage boundary, so stepping quickly through big photos never stacks up
//! full-size buffers.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, TryRecvError};
use egui::{Color32, ColorImage, Painter, Rect, TextureHandle, TextureOptions, pos2, vec2};

use crate::Waker;

/// Texture side including the 1 px borders shared with neighbouring tiles.
const TILE_SIDE: usize = 2048;
const TILE_CONTENT: usize = TILE_SIDE - 2;
/// Stop building the pyramid once a level is this small.
const MIN_LEVEL_SIDE: usize = 16;
/// Pixels uploaded per frame (at least one tile is always uploaded).
const UPLOAD_BUDGET_PX: usize = 4 * 1024 * 1024;

/// Premultiplied RGBA pixels.
pub(crate) struct Pixels {
    pub w: usize,
    pub h: usize,
    pub px: Vec<Color32>,
}

/// One tile: the `w`x`h` content at (`x`, `y`) of its level, stored in a texture that also
/// holds a 1 px border (where neighbours exist) so bilinear filtering is seamless.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TileRect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub tex_x: usize,
    pub tex_y: usize,
    pub tex_w: usize,
    pub tex_h: usize,
}

impl TileRect {
    fn uv(&self) -> Rect {
        let u0 = (self.x - self.tex_x) as f32 / self.tex_w as f32;
        let v0 = (self.y - self.tex_y) as f32 / self.tex_h as f32;
        let u1 = (self.x + self.w - self.tex_x) as f32 / self.tex_w as f32;
        let v1 = (self.y + self.h - self.tex_y) as f32 / self.tex_h as f32;
        Rect::from_min_max(pos2(u0, v0), pos2(u1, v1))
    }
}

/// Split one axis of `len` pixels into (start, len, tex_start, tex_len) spans.
fn spans(len: usize) -> Vec<(usize, usize, usize, usize)> {
    if len <= TILE_SIDE {
        return vec![(0, len, 0, len)];
    }
    (0..len)
        .step_by(TILE_CONTENT)
        .map(|start| {
            let n = TILE_CONTENT.min(len - start);
            let t0 = start.saturating_sub(1);
            let t1 = (start + n + 1).min(len);
            (start, n, t0, t1 - t0)
        })
        .collect()
}

pub(crate) fn tile_layout(w: usize, h: usize) -> Vec<TileRect> {
    let xs = spans(w);
    let ys = spans(h);
    let mut out = Vec::with_capacity(xs.len() * ys.len());
    for &(y, h, tex_y, tex_h) in &ys {
        for &(x, w, tex_x, tex_w) in &xs {
            out.push(TileRect {
                x,
                y,
                w,
                h,
                tex_x,
                tex_y,
                tex_w,
                tex_h,
            });
        }
    }
    out
}

fn extract_tile(src: &Pixels, t: &TileRect) -> ColorImage {
    let mut px = Vec::with_capacity(t.tex_w * t.tex_h);
    for row in t.tex_y..t.tex_y + t.tex_h {
        let start = row * src.w + t.tex_x;
        px.extend_from_slice(&src.px[start..start + t.tex_w]);
    }
    ColorImage::new([t.tex_w, t.tex_h], px)
}

/// 2x2 box filter on premultiplied pixels (odd edges repeat the last row/column).
pub(crate) fn halve(src: &Pixels) -> Pixels {
    let w = src.w.div_ceil(2);
    let h = src.h.div_ceil(2);
    let mut px = Vec::with_capacity(w * h);
    for y in 0..h {
        let r0 = &src.px[(2 * y) * src.w..][..src.w];
        let r1 = &src.px[(2 * y + 1).min(src.h - 1) * src.w..][..src.w];
        for x in 0..w {
            let x0 = 2 * x;
            let x1 = (2 * x + 1).min(src.w - 1);
            let quad = [r0[x0], r0[x1], r1[x0], r1[x1]];
            let avg = |c: usize| {
                let sum: u16 = quad.iter().map(|p| u16::from(p.to_array()[c])).sum();
                ((sum + 2) / 4) as u8
            };
            px.push(Color32::from_rgba_premultiplied(
                avg(0),
                avg(1),
                avg(2),
                avg(3),
            ));
        }
    }
    Pixels { w, h, px }
}

pub(crate) fn premultiply(img: image::DynamicImage) -> Pixels {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let px = match img {
        image::DynamicImage::ImageRgb8(buf) => buf
            .pixels()
            .map(|p| Color32::from_rgb(p[0], p[1], p[2]))
            .collect(),
        other => other
            .to_rgba8()
            .pixels()
            .map(|p| Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
            .collect(),
    };
    Pixels { w, h, px }
}

/// Image size from the file header without decoding pixels.
pub(crate) fn probe(path: &Path) -> Result<(u32, u32), String> {
    let reader = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let (w, h) = reader.into_dimensions().map_err(|e| e.to_string())?;
    if w == 0 || h == 0 {
        return Err("Image has zero size.".into());
    }
    Ok((w, h))
}

/// Held by the worker that is decoding / building a pyramid.
static DECODE_SLOT: Mutex<()> = Mutex::new(());

/// File reader that fails every read once `cancel` is set, aborting streaming decoders.
struct CancellableRead<R> {
    inner: R,
    cancel: Arc<AtomicBool>,
}

impl<R: Read> Read for CancellableRead<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            // Not `Interrupted`: read loops retry that.
            return Err(io::Error::other("cancelled"));
        }
        self.inner.read(buf)
    }
}

impl<R: Seek> Seek for CancellableRead<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// Decode `path`; `None` when `cancel` was set before it finished.
pub(crate) fn decode(path: &Path, cancel: &Arc<AtomicBool>) -> Option<Result<Pixels, String>> {
    let decoded = (|| {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let mut reader = image::ImageReader::new(BufReader::new(CancellableRead {
            inner: file,
            cancel: cancel.clone(),
        }));
        if let Ok(format) = image::ImageFormat::from_path(path) {
            reader.set_format(format);
        }
        let reader = reader.with_guessed_format().map_err(|e| e.to_string())?;
        reader.decode().map_err(|e| e.to_string())
    })();
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let img = match decoded {
        Ok(img) => img,
        Err(e) => return Some(Err(e)),
    };
    if img.width() == 0 || img.height() == 0 {
        return Some(Err("Image has zero size.".into()));
    }
    Some(Ok(premultiply(img)))
}

/// Mip levels (index 0 = full size) built by repeated halving; `None` once cancelled.
pub(crate) fn build_pyramid(base: Pixels, cancel: &AtomicBool) -> Option<Vec<Pixels>> {
    let mut levels = vec![base];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let last = &levels[levels.len() - 1];
        if last.w.max(last.h) <= MIN_LEVEL_SIDE || (last.w == 1 && last.h == 1) {
            break;
        }
        let next = halve(last);
        levels.push(next);
    }
    Some(levels)
}

enum Msg {
    Levels(Vec<[usize; 2]>),
    Tile {
        level: usize,
        rect: TileRect,
        image: ColorImage,
    },
    Error(String),
}

fn worker(
    path: PathBuf,
    tx: crossbeam_channel::Sender<Msg>,
    cancel: Arc<AtomicBool>,
    waker: Waker,
) {
    let mut levels = {
        let _slot = DECODE_SLOT.lock().unwrap_or_else(|e| e.into_inner());
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let base = match decode(&path, &cancel) {
            None => return,
            Some(Ok(p)) => p,
            Some(Err(e)) => {
                let _ = tx.send(Msg::Error(e));
                waker.wake();
                return;
            }
        };
        match build_pyramid(base, &cancel) {
            Some(levels) => levels,
            None => return,
        }
    };
    if tx
        .send(Msg::Levels(levels.iter().map(|l| [l.w, l.h]).collect()))
        .is_err()
    {
        return;
    }
    // Coarsest first; free each level as soon as its tiles are queued.
    while let Some(level) = levels.pop() {
        let index = levels.len();
        for rect in tile_layout(level.w, level.h) {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let image = extract_tile(&level, &rect);
            if tx
                .send(Msg::Tile {
                    level: index,
                    rect,
                    image,
                })
                .is_err()
            {
                return;
            }
            waker.wake();
        }
    }
}

struct Level {
    size: [usize; 2],
    expected: usize,
    tiles: Vec<(TileRect, TextureHandle)>,
}

impl Level {
    fn complete(&self) -> bool {
        self.tiles.len() == self.expected
    }
}

/// UI side of a static image: receives tiles and paints the best uploaded level.
pub(crate) struct RasterView {
    rx: Option<Receiver<Msg>>,
    cancel: Arc<AtomicBool>,
    levels: Vec<Level>,
    pending: Vec<(usize, TileRect, ColorImage)>,
    error: Option<String>,
}

impl RasterView {
    pub fn spawn(path: &Path, waker: Waker) -> Self {
        let (tx, rx) = crossbeam_channel::bounded(2);
        let cancel = Arc::new(AtomicBool::new(false));
        let path = path.to_path_buf();
        let c = cancel.clone();
        let spawned = std::thread::Builder::new()
            .name("stlv-image-decode".into())
            .spawn(move || worker(path, tx, c, waker));
        let error = spawned
            .err()
            .map(|e| format!("Failed to start image decoder: {e}"));
        Self {
            rx: Some(rx),
            cancel,
            levels: Vec::new(),
            pending: Vec::new(),
            error,
        }
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Every level fully uploaded (or failed).
    pub fn is_ready(&self) -> bool {
        self.error.is_some()
            || (self.rx.is_none()
                && self.pending.is_empty()
                && !self.levels.is_empty()
                && self.levels.iter().all(Level::complete))
    }

    /// Receive finished tiles and upload up to the per-frame budget.
    pub fn poll(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.rx {
            loop {
                match rx.try_recv() {
                    Ok(Msg::Levels(sizes)) => {
                        self.levels = sizes
                            .into_iter()
                            .map(|size| Level {
                                size,
                                expected: tile_layout(size[0], size[1]).len(),
                                tiles: Vec::new(),
                            })
                            .collect();
                    }
                    Ok(Msg::Tile { level, rect, image }) => {
                        self.pending.push((level, rect, image));
                        if self.pending.len() >= 2 {
                            break;
                        }
                    }
                    Ok(Msg::Error(e)) => {
                        self.error = Some(e);
                        self.rx = None;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.rx = None;
                        break;
                    }
                }
            }
        }
        let mut uploaded = 0;
        while uploaded < UPLOAD_BUDGET_PX && !self.pending.is_empty() {
            let (level, rect, image) = self.pending.remove(0);
            uploaded += image.width() * image.height();
            let name = format!("stlv-image-l{level}-{}x{}", rect.x, rect.y);
            let tex = ctx.load_texture(name, image, TextureOptions::LINEAR);
            if let Some(l) = self.levels.get_mut(level) {
                l.tiles.push((rect, tex));
            }
        }
        if !self.is_ready()
            && self.error.is_none()
            && (!self.pending.is_empty() || self.rx.is_some())
        {
            ctx.request_repaint();
        }
    }

    pub fn has_content(&self) -> bool {
        self.levels.iter().any(|l| !l.tiles.is_empty())
    }

    /// Paint into `image_rect` (screen rect of the whole image); `scale` = physical pixels
    /// per full-resolution image pixel.
    pub fn paint(&self, painter: &Painter, image_rect: Rect, scale: f32) {
        let Some(level) = self.pick_level(scale) else {
            return;
        };
        let l = &self.levels[level];
        let (sx, sy) = (
            image_rect.width() / l.size[0] as f32,
            image_rect.height() / l.size[1] as f32,
        );
        let clip = painter.clip_rect();
        for (t, tex) in &l.tiles {
            let rect = Rect::from_min_size(
                image_rect.min + vec2(t.x as f32 * sx, t.y as f32 * sy),
                vec2(t.w as f32 * sx, t.h as f32 * sy),
            );
            if rect.intersects(clip) {
                painter.image(tex.id(), rect, t.uv(), Color32::WHITE);
            }
        }
    }

    /// The smallest level still at least as detailed as the screen, else the nearest
    /// uploaded one (finer preferred).
    fn pick_level(&self, scale: f32) -> Option<usize> {
        let full_w = self.levels.first()?.size[0] as f32;
        let ideal = self
            .levels
            .iter()
            .rposition(|l| l.size[0] as f32 / full_w >= scale)
            .unwrap_or(0);
        let complete = |i: usize| self.levels.get(i).is_some_and(Level::complete);
        if complete(ideal) {
            return Some(ideal);
        }
        if let Some(i) = (0..ideal).rev().find(|&i| complete(i)) {
            return Some(i);
        }
        if let Some(i) = (ideal + 1..self.levels.len()).find(|&i| complete(i)) {
            return Some(i);
        }
        // Nothing complete yet: draw whatever has been uploaded.
        self.levels.iter().position(|l| !l.tiles.is_empty())
    }
}

impl Drop for RasterView {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: usize, h: usize, c: Color32) -> Pixels {
        Pixels {
            w,
            h,
            px: vec![c; w * h],
        }
    }

    #[test]
    fn small_images_are_one_borderless_tile() {
        let t = tile_layout(300, 200);
        assert_eq!(
            t,
            vec![TileRect {
                x: 0,
                y: 0,
                w: 300,
                h: 200,
                tex_x: 0,
                tex_y: 0,
                tex_w: 300,
                tex_h: 200
            }]
        );
        assert_eq!(
            t[0].uv(),
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))
        );
    }

    #[test]
    fn large_images_tile_with_shared_borders() {
        let tiles = tile_layout(8000, 6000);
        assert_eq!(tiles.len(), 4 * 3);
        let mut covered = 0usize;
        for t in &tiles {
            assert!(t.tex_w <= TILE_SIDE && t.tex_h <= TILE_SIDE);
            assert!(t.tex_x <= t.x && t.x + t.w <= t.tex_x + t.tex_w);
            assert!(t.tex_x + t.tex_w <= 8000 && t.tex_y + t.tex_h <= 6000);
            // Interior edges carry a 1 px border.
            if t.x > 0 {
                assert_eq!(t.tex_x, t.x - 1);
            }
            if t.x + t.w < 8000 {
                assert_eq!(t.tex_x + t.tex_w, t.x + t.w + 1);
            }
            covered += t.w * t.h;
        }
        assert_eq!(covered, 8000 * 6000);
        // Exactly one tile boundary case: 2049 px needs two tiles.
        assert_eq!(spans(2049).len(), 2);
        assert_eq!(spans(2048).len(), 1);
    }

    #[test]
    fn halving_averages_premultiplied() {
        let mut p = solid(3, 3, Color32::from_rgba_premultiplied(100, 0, 0, 255));
        p.px[0] = Color32::TRANSPARENT;
        let h = halve(&p);
        assert_eq!((h.w, h.h), (2, 2));
        // Top-left quad: 1 transparent + 3 red.
        assert_eq!(h.px[0].to_array(), [75, 0, 0, 191]);
        // Odd edge repeats the last column/row: all red.
        assert_eq!(h.px[3].to_array(), [100, 0, 0, 255]);
    }

    #[test]
    fn pyramid_ends_small() {
        let never = AtomicBool::new(false);
        let levels = build_pyramid(solid(1000, 10, Color32::WHITE), &never).unwrap();
        let last = levels.last().unwrap();
        assert!(last.w <= MIN_LEVEL_SIDE);
        assert_eq!(levels[1].w, 500);
        assert_eq!(levels[1].h, 5);
        let one = build_pyramid(solid(1, 1, Color32::WHITE), &never).unwrap();
        assert_eq!(one.len(), 1);
    }

    #[test]
    fn cancelled_decode_stops_reading() {
        let path =
            std::env::temp_dir().join(format!("stlv-raster-cancel-{}.png", std::process::id()));
        image::RgbImage::from_pixel(64, 48, image::Rgb([200, 10, 10]))
            .save(&path)
            .unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let px = decode(&path, &cancel).unwrap().unwrap();
        assert_eq!((px.w, px.h), (64, 48));
        assert_eq!(px.px[0], Color32::from_rgb(200, 10, 10));

        // A set flag fails the very first read, so nothing is decoded or allocated.
        let mut r = CancellableRead {
            inner: File::open(&path).unwrap(),
            cancel: cancel.clone(),
        };
        cancel.store(true, Ordering::Relaxed);
        let mut buf = [0u8; 8];
        assert_eq!(r.read(&mut buf).unwrap_err().kind(), io::ErrorKind::Other);
        assert!(decode(&path, &cancel).is_none());
        assert!(build_pyramid(solid(64, 64, Color32::WHITE), &cancel).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn extract_tile_copies_border() {
        let mut p = solid(5000, 3, Color32::BLACK);
        p.px[TILE_CONTENT - 1] = Color32::RED; // last content column of tile 0
        p.px[TILE_CONTENT] = Color32::GREEN; // first column of tile 1
        let tiles = tile_layout(5000, 3);
        let t0 = extract_tile(&p, &tiles[0]);
        assert_eq!(t0.size, [TILE_CONTENT + 1, 3]);
        assert_eq!(t0.pixels[TILE_CONTENT - 1], Color32::RED);
        assert_eq!(t0.pixels[TILE_CONTENT], Color32::GREEN);
        let t1 = extract_tile(&p, &tiles[1]);
        assert_eq!(t1.pixels[0], Color32::RED);
        assert_eq!(t1.pixels[1], Color32::GREEN);
    }
}
