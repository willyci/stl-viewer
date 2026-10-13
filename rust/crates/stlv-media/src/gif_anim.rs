//! Animated GIF playback. A worker thread decodes frames one at a time into a bounded
//! channel and loops by re-reading the file, so memory stays bounded for huge GIFs. Small
//! animations (all frames within [`CACHE_BUDGET`]) are kept as textures after the first
//! pass and the worker stops.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use egui::{Color32, ColorImage, Painter, Rect, TextureHandle, TextureOptions, pos2};
use image::AnimationDecoder;

use crate::{SAFE_TEXTURE_SIDE, Waker, texture_side_limit};

/// Bytes of frame textures kept for looping without re-decoding.
const CACHE_BUDGET: usize = 64 * 1024 * 1024;
/// Frames larger than this (or than the GPU's texture limit) are downscaled.
const MAX_FRAME_SIDE: usize = 4096;
/// Refuse GIFs whose canvas alone would need more memory than this.
const MAX_CANVAS_BYTES: u64 = 512 * 1024 * 1024;

/// Logical screen size and whether there is more than one frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GifHeader {
    pub width: u32,
    pub height: u32,
    pub animated: bool,
}

pub(crate) fn probe(path: &Path) -> Result<GifHeader, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    scan(BufReader::new(file))
}

/// Walk the GIF block structure (without LZW decoding) until a second image is found.
pub(crate) fn scan(mut r: impl Read) -> Result<GifHeader, String> {
    let mut hdr = [0u8; 13];
    r.read_exact(&mut hdr)
        .map_err(|_| "Not a valid GIF file.".to_string())?;
    if &hdr[..3] != b"GIF" {
        return Err("Not a valid GIF file.".into());
    }
    let width = u32::from(u16::from_le_bytes([hdr[6], hdr[7]]));
    let height = u32::from(u16::from_le_bytes([hdr[8], hdr[9]]));
    if width == 0 || height == 0 {
        return Err("GIF has zero size.".into());
    }
    // Past the header, damage only stops the walk: the decoder reports real errors.
    let animated = count_images(&mut r, hdr[10]).unwrap_or_else(|n| n) >= 2;
    Ok(GifHeader {
        width,
        height,
        animated,
    })
}

/// Number of image descriptors (stops at 2); `Err(count so far)` on truncation.
fn count_images(r: &mut impl Read, screen_flags: u8) -> Result<usize, usize> {
    if screen_flags & 0x80 != 0 {
        skip(r, 3 << ((screen_flags & 7) + 1)).map_err(|_| 0usize)?;
    }
    let mut images = 0;
    while images < 2 {
        let mut b = [0u8; 1];
        r.read_exact(&mut b).map_err(|_| images)?;
        match b[0] {
            0x21 => {
                skip(r, 1).map_err(|_| images)?;
                skip_sub_blocks(r).map_err(|_| images)?;
            }
            0x2C => {
                let mut desc = [0u8; 9];
                r.read_exact(&mut desc).map_err(|_| images)?;
                if desc[8] & 0x80 != 0 {
                    skip(r, 3 << ((desc[8] & 7) + 1)).map_err(|_| images)?;
                }
                images += 1;
                skip(r, 1).map_err(|_| images)?; // LZW minimum code size
                skip_sub_blocks(r).map_err(|_| images)?;
            }
            _ => break, // trailer (0x3B) or garbage
        }
    }
    Ok(images)
}

fn skip(r: &mut impl Read, n: u64) -> Result<(), String> {
    let copied = std::io::copy(&mut r.take(n), &mut std::io::sink()).map_err(|e| e.to_string())?;
    if copied == n {
        Ok(())
    } else {
        Err("Truncated GIF file.".into())
    }
}

fn skip_sub_blocks(r: &mut impl Read) -> Result<(), String> {
    loop {
        let mut len = [0u8; 1];
        r.read_exact(&mut len)
            .map_err(|_| "Truncated GIF file.".to_string())?;
        if len[0] == 0 {
            return Ok(());
        }
        skip(r, u64::from(len[0]))?;
    }
}

/// Browsers (and most viewers) play delays of 0-10 ms as 100 ms.
pub(crate) fn frame_delay(numer_ms: u32, denom: u32) -> Duration {
    let ms = if denom == 0 {
        0.0
    } else {
        f64::from(numer_ms) / f64::from(denom)
    };
    if ms <= 10.0 {
        Duration::from_millis(100)
    } else {
        Duration::from_secs_f64(ms / 1000.0)
    }
}

enum Msg {
    Frame { image: ColorImage, delay: Duration },
    EndOfPass,
    Error(String),
}

fn to_color_image(buf: image::RgbaImage, max_side: usize) -> ColorImage {
    let max_side = u32::try_from(max_side).unwrap_or(u32::MAX).max(1);
    let buf = if buf.width() > max_side || buf.height() > max_side {
        let s = f64::from(max_side) / f64::from(buf.width().max(buf.height()));
        let w = ((f64::from(buf.width()) * s).round() as u32).max(1);
        let h = ((f64::from(buf.height()) * s).round() as u32).max(1);
        image::imageops::resize(&buf, w, h, image::imageops::FilterType::Triangle)
    } else {
        buf
    };
    let size = [buf.width() as usize, buf.height() as usize];
    let px = buf
        .pixels()
        .map(|p| Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
        .collect();
    ColorImage::new(size, px)
}

struct Shared {
    stop: AtomicBool,
    /// Largest frame side the UI can upload.
    max_side: AtomicUsize,
}

fn worker(path: PathBuf, header: GifHeader, tx: Sender<Msg>, shared: Arc<Shared>, waker: Waker) {
    let fail = |e: String| {
        let _ = tx.send(Msg::Error(e));
        waker.wake();
    };
    let mut pass = 0u32;
    loop {
        let decoder = File::open(&path)
            .map_err(|e| e.to_string())
            .and_then(|f| {
                image::codecs::gif::GifDecoder::new(BufReader::new(f)).map_err(|e| e.to_string())
            })
            .and_then(|mut d| {
                let mut limits = image::Limits::default();
                limits.max_alloc = Some(MAX_CANVAS_BYTES * 2);
                image::ImageDecoder::set_limits(&mut d, limits).map_err(|e| e.to_string())?;
                Ok(d)
            });
        let decoder = match decoder {
            Ok(d) => d,
            Err(e) => return fail(e),
        };
        let mut count = 0usize;
        for frame in decoder.into_frames() {
            if shared.stop.load(Ordering::Relaxed) {
                return;
            }
            match frame {
                Ok(f) => {
                    let (n, d) = f.delay().numer_denom_ms();
                    let msg = Msg::Frame {
                        image: to_color_image(
                            f.into_buffer(),
                            shared.max_side.load(Ordering::Relaxed),
                        ),
                        delay: frame_delay(n, d),
                    };
                    if tx.send(msg).is_err() {
                        return;
                    }
                    waker.wake();
                    count += 1;
                }
                // A corrupt tail ends the pass; the frames before it keep playing.
                Err(e) if count == 0 && pass == 0 => return fail(e.to_string()),
                Err(e) => {
                    log::warn!("GIF decode error in {}: {e}", path.display());
                    break;
                }
            }
        }
        if count == 0 {
            return fail("GIF contains no frames.".into());
        }
        if tx.send(Msg::EndOfPass).is_err() {
            return;
        }
        waker.wake();
        if count == 1 || !header.animated {
            return;
        }
        pass += 1;
    }
}

/// UI side of a GIF: frame timing, texture cache, streaming fallback.
pub(crate) struct GifView {
    rx: Option<Receiver<Msg>>,
    shared: Arc<Shared>,
    /// Frames of the first pass while they fit the budget.
    cache: Vec<(TextureHandle, Duration)>,
    cache_bytes: usize,
    caching: bool,
    /// All frames are cached: loop over `cache` without the worker.
    looping_cache: bool,
    cache_index: usize,
    stream_tex: Option<TextureHandle>,
    current: Option<egui::TextureId>,
    next_due: Option<Instant>,
    frames_shown: u64,
    error: Option<String>,
}

impl GifView {
    pub fn spawn(path: &Path, header: GifHeader, waker: Waker) -> Self {
        let (tx, rx) = crossbeam_channel::bounded(2);
        // Until the first `show` reports the GPU limit, assume the universal minimum.
        let max_side = waker.ctx().map_or(SAFE_TEXTURE_SIDE, |ctx| {
            texture_side_limit(ctx, MAX_FRAME_SIDE)
        });
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            max_side: AtomicUsize::new(max_side),
        });
        let (p, s) = (path.to_path_buf(), shared.clone());
        let spawned = std::thread::Builder::new()
            .name("stlv-gif-decode".into())
            .spawn(move || worker(p, header, tx, s, waker));
        Self {
            rx: Some(rx),
            shared,
            cache: Vec::new(),
            cache_bytes: 0,
            caching: true,
            looping_cache: false,
            cache_index: 0,
            stream_tex: None,
            current: None,
            next_due: None,
            frames_shown: 0,
            error: spawned
                .err()
                .map(|e| format!("Failed to start GIF decoder: {e}")),
        }
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn has_content(&self) -> bool {
        self.current.is_some()
    }

    /// Number of frame switches so far (for tests and screenshot checks).
    pub fn frames_shown(&self) -> u64 {
        self.frames_shown
    }

    /// Advance to the next frame when its time has come.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let limit = texture_side_limit(ctx, MAX_FRAME_SIDE);
        self.shared.max_side.store(limit, Ordering::Relaxed);
        let now = Instant::now();
        if let Some(due) = self.next_due
            && now < due
        {
            ctx.request_repaint_after(due - now);
            return;
        }
        if self.looping_cache {
            if self.cache.len() > 1 {
                self.cache_index = (self.cache_index + 1) % self.cache.len();
                let (id, delay) = (
                    self.cache[self.cache_index].0.id(),
                    self.cache[self.cache_index].1,
                );
                self.show_frame(ctx, id, delay, now);
            }
            return;
        }
        let Some(rx) = &self.rx else { return };
        loop {
            match rx.try_recv() {
                Ok(Msg::Frame { image, delay }) => {
                    let id = self.accept_frame(ctx, image, delay);
                    self.show_frame(ctx, id, delay, now);
                    return;
                }
                Ok(Msg::EndOfPass) => {
                    if self.caching && !self.cache.is_empty() {
                        self.caching = false;
                        self.looping_cache = true;
                        self.shared.stop.store(true, Ordering::Relaxed);
                        self.rx = None;
                        self.stream_tex = None;
                        if self.cache.len() > 1 {
                            self.cache_index = 0;
                            let (id, delay) = (self.cache[0].0.id(), self.cache[0].1);
                            self.show_frame(ctx, id, delay, now);
                        } else {
                            self.next_due = None;
                        }
                        return;
                    }
                    self.caching = false;
                }
                Ok(Msg::Error(e)) => {
                    if self.current.is_none() {
                        self.error = Some(e);
                    }
                    self.rx = None;
                    return;
                }
                Err(TryRecvError::Empty) => {
                    // The worker wakes us when the next frame is ready.
                    return;
                }
                Err(TryRecvError::Disconnected) => {
                    self.rx = None;
                    self.next_due = None;
                    return;
                }
            }
        }
    }

    fn accept_frame(
        &mut self,
        ctx: &egui::Context,
        image: ColorImage,
        delay: Duration,
    ) -> egui::TextureId {
        let bytes = image.width() * image.height() * 4;
        if self.caching {
            if self.cache_bytes + bytes <= CACHE_BUDGET {
                self.cache_bytes += bytes;
                let name = format!("stlv-gif-{}", self.cache.len());
                let tex = ctx.load_texture(name, image, TextureOptions::LINEAR);
                let id = tex.id();
                self.cache.push((tex, delay));
                return id;
            }
            // Too big to keep every frame: stream from now on.
            self.caching = false;
            self.cache.clear();
            self.cache_bytes = 0;
        }
        match &mut self.stream_tex {
            Some(tex) => tex.set(image, TextureOptions::LINEAR),
            None => {
                self.stream_tex =
                    Some(ctx.load_texture("stlv-gif-stream", image, TextureOptions::LINEAR))
            }
        }
        self.stream_tex
            .as_ref()
            .map_or(egui::TextureId::default(), TextureHandle::id)
    }

    fn show_frame(
        &mut self,
        ctx: &egui::Context,
        id: egui::TextureId,
        delay: Duration,
        now: Instant,
    ) {
        self.current = Some(id);
        self.frames_shown += 1;
        // Keep the schedule unless we fell behind (e.g. the page was hidden).
        let due = match self.next_due {
            Some(prev) if prev + delay > now => prev + delay,
            _ => now + delay,
        };
        self.next_due = Some(due);
        ctx.request_repaint_after(due.saturating_duration_since(now));
    }

    pub fn paint(&self, painter: &Painter, image_rect: Rect) {
        if let Some(id) = self.current {
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            painter.image(id, image_rect, uv, Color32::WHITE);
        }
    }

    /// First frame shown (later frames are timing-driven; never "settles").
    pub fn is_ready(&self) -> bool {
        self.current.is_some() || self.error.is_some()
    }
}

impl Drop for GifView {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal GIF: 2x1 canvas, global palette of 2, `frames` 1x1 images.
    fn tiny_gif(frames: usize) -> Vec<u8> {
        let mut g = b"GIF89a".to_vec();
        g.extend_from_slice(&[2, 0, 1, 0, 0x80, 0, 0]);
        g.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
        for _ in 0..frames {
            g.extend_from_slice(&[0x21, 0xF9, 4, 0, 10, 0, 0, 0]); // GCE, 100 ms
            g.extend_from_slice(&[0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0]);
            g.extend_from_slice(&[2, 2, 0x44, 0x01, 0]);
        }
        g.push(0x3B);
        g
    }

    #[test]
    fn scan_counts_frames() {
        let one = scan(&tiny_gif(1)[..]).unwrap();
        assert_eq!(
            one,
            GifHeader {
                width: 2,
                height: 1,
                animated: false
            }
        );
        let three = scan(&tiny_gif(3)[..]).unwrap();
        assert!(three.animated);
        // Truncated after the header still reports the size.
        let cut = &tiny_gif(3)[..20];
        assert_eq!(
            scan(cut).unwrap(),
            GifHeader {
                width: 2,
                height: 1,
                animated: false
            }
        );
        let gif = tiny_gif(3);
        let cut = &gif[..gif.len() - 20];
        assert!(scan(cut).unwrap().animated);
        assert!(scan(&b"GIF8"[..]).is_err());
        assert!(scan(&b"PNG89a1234567890"[..]).is_err());
    }

    #[test]
    fn tiny_gif_decodes_with_image_crate() {
        let data = tiny_gif(3);
        let dec = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(data)).unwrap();
        let frames: Vec<_> = dec.into_frames().collect::<Result<_, _>>().unwrap();
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].buffer().dimensions(), (2, 1));
        let (n, d) = frames[0].delay().numer_denom_ms();
        assert_eq!(frame_delay(n, d), Duration::from_millis(100));
    }

    #[test]
    fn delays() {
        assert_eq!(frame_delay(0, 1), Duration::from_millis(100));
        assert_eq!(frame_delay(10, 1), Duration::from_millis(100));
        assert_eq!(frame_delay(20, 1), Duration::from_millis(20));
        assert_eq!(frame_delay(250, 1), Duration::from_millis(250));
        assert_eq!(frame_delay(5, 0), Duration::from_millis(100));
    }
}
