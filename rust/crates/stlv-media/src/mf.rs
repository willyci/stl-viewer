//! Windows Media Foundation playback: an `IMFMediaEngine` in frame-server mode, owned by a
//! dedicated MTA thread. Frames are blitted into a WIC bitmap (scaled by the engine to the
//! displayed size) and handed to the UI as premultiplied pixels; audio is rendered by the
//! engine itself. The UI talks to the thread through commands and a shared status.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use egui::Color32;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICBitmap, IWICImagingFactory,
    WICBitmapCacheOnLoad, WICBitmapLockRead, WICRect,
};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_DISABLE_OLE1DDE, COINIT_MULTITHREADED, CoCreateInstance,
    CoInitializeEx, CoUninitialize,
};
use windows_core::{BSTR, HRESULT, HSTRING, Interface};

use crate::Waker;

/// How long `open` (on the UI thread) waits for the metadata or an error; slower sources
/// keep loading in the background and report errors through the status.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);
/// How long `stop` waits for the engine thread to shut down.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// Engine thread poll period (frame ticks, position updates, commands).
const TICK: Duration = Duration::from_millis(4);

pub(crate) enum Cmd {
    Play,
    Pause,
    Seek(f64),
    SetMuted(bool),
    /// Pixel size the UI displays the video at (frames are scaled to this, up to native).
    OutputSize([u32; 2]),
    Shutdown,
}

enum Input {
    /// A command and its sequence number (see [`Status::applied`]).
    Cmd(u64, Cmd),
    Event {
        event: i32,
        param1: usize,
        param2: u32,
    },
}

/// A decoded frame ready for upload.
pub(crate) struct Frame {
    pub size: [usize; 2],
    pub pixels: Vec<Color32>,
}

/// State shared between the engine thread and the UI.
#[derive(Default)]
pub(crate) struct Status {
    pub error: Option<String>,
    /// Seconds; `None` until known (or for live/unbounded media).
    pub duration: Option<f64>,
    pub position: f64,
    pub paused: bool,
    pub ended: bool,
    pub muted: bool,
    /// Native (display-aspect corrected) video size.
    pub video_size: Option<[u32; 2]>,
    pub has_audio: bool,
    /// Newest frame not yet taken by the UI.
    pub frame: Option<Frame>,
    pub frames_transferred: u64,
    /// Sequence number of the last command whose effect is reflected above.
    pub applied: u64,
}

pub(crate) struct Engine {
    tx: Sender<Input>,
    sent: u64,
    status: Arc<Mutex<Status>>,
    thread: Option<JoinHandle<()>>,
    done_rx: Receiver<()>,
}

impl Engine {
    /// Start playback of `path` and wait until its metadata is loaded (or it failed).
    pub fn open(path: &Path, muted: bool, waker: Waker) -> Result<Self, String> {
        check_container(path)?;
        let (tx, rx) = crossbeam_channel::unbounded();
        let (ready_tx, ready_rx) = crossbeam_channel::bounded(1);
        let (done_tx, done_rx) = crossbeam_channel::bounded(1);
        let status = Arc::new(Mutex::new(Status {
            muted,
            ..Status::default()
        }));
        let ctx = ThreadCtx {
            path: path.to_path_buf(),
            muted,
            tx: tx.clone(),
            rx,
            status: status.clone(),
            ready_tx,
            waker,
        };
        let thread = std::thread::Builder::new()
            .name("stlv-media-engine".into())
            .spawn(move || {
                run(ctx);
                let _ = done_tx.send(());
            })
            .map_err(|e| format!("Failed to start the video thread: {e}"))?;
        let mut engine = Self {
            tx,
            sent: 0,
            status,
            thread: Some(thread),
            done_rx,
        };
        match ready_rx.recv_timeout(OPEN_TIMEOUT) {
            Ok(Ok(())) => Ok(engine),
            Ok(Err(e)) => {
                engine.shutdown();
                Err(e)
            }
            Err(RecvTimeoutError::Timeout) => {
                log::warn!(
                    "video metadata not loaded after {OPEN_TIMEOUT:?}; continuing asynchronously"
                );
                Ok(engine)
            }
            Err(RecvTimeoutError::Disconnected) => {
                engine.shutdown();
                Err("The video thread exited unexpectedly.".into())
            }
        }
    }

    /// Queue a command; returns its sequence number.
    pub fn send(&mut self, cmd: Cmd) -> u64 {
        self.sent += 1;
        let _ = self.tx.send(Input::Cmd(self.sent, cmd));
        self.sent
    }

    pub fn status(&self) -> MutexGuard<'_, Status> {
        lock(&self.status)
    }

    /// Shut the engine down and join its thread (bounded wait).
    pub fn shutdown(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        let _ = self.tx.send(Input::Cmd(u64::MAX, Cmd::Shutdown));
        match self.done_rx.recv_timeout(SHUTDOWN_TIMEOUT) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                let _ = thread.join();
            }
            Err(RecvTimeoutError::Timeout) => {
                log::error!(
                    "video engine did not shut down within {SHUTDOWN_TIMEOUT:?}; detaching its thread"
                );
            }
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn lock(status: &Mutex<Status>) -> MutexGuard<'_, Status> {
    status.lock().unwrap_or_else(PoisonError::into_inner)
}

#[windows_core::implement(IMFMediaEngineNotify)]
struct Notify {
    tx: Sender<Input>,
}

impl IMFMediaEngineNotify_Impl for Notify_Impl {
    fn EventNotify(&self, event: u32, param1: usize, param2: u32) -> windows_core::Result<()> {
        // Called on a Media Foundation work queue thread: just forward.
        let _ = self.tx.send(Input::Event {
            event: event as i32,
            param1,
            param2,
        });
        Ok(())
    }
}

struct ThreadCtx {
    path: PathBuf,
    muted: bool,
    tx: Sender<Input>,
    rx: Receiver<Input>,
    status: Arc<Mutex<Status>>,
    ready_tx: Sender<Result<(), String>>,
    waker: Waker,
}

/// Balances `CoInitializeEx` / `MFStartup` on the engine thread.
struct ComGuard {
    mf_started: bool,
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        // SAFETY: paired with the successful MFStartup / CoInitializeEx on this thread; every
        // COM object created here has been released before the guard drops.
        unsafe {
            if self.mf_started {
                let _ = MFShutdown();
            }
            CoUninitialize();
        }
    }
}

fn run(ctx: ThreadCtx) {
    // SAFETY: initializes COM for this (new) thread; balanced by `ComGuard`.
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED | COINIT_DISABLE_OLE1DDE) };
    if hr.is_err() {
        fail_open(
            &ctx,
            format!("COM initialization failed: {}", describe_hr(hr)),
        );
        return;
    }
    let mut guard = ComGuard { mf_started: false };
    // SAFETY: plain API call; balanced by MFShutdown in `ComGuard`.
    if let Err(e) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) } {
        fail_open(
            &ctx,
            format!(
                "Media Foundation is not available: {}",
                describe_hr(e.code())
            ),
        );
        return;
    }
    guard.mf_started = true;
    // Scope every COM object so it is released before MFShutdown / CoUninitialize.
    match Player::create(&ctx) {
        Ok(mut player) => {
            player.run_loop(&ctx);
            player.shutdown();
        }
        Err(e) => fail_open(&ctx, e),
    }
    drop(guard);
}

fn fail_open(ctx: &ThreadCtx, msg: String) {
    log::error!("video: {msg}");
    lock(&ctx.status).error = Some(msg.clone());
    let _ = ctx.ready_tx.try_send(Err(msg));
    ctx.waker.wake();
}

struct Player {
    engine: IMFMediaEngine,
    wic: IWICImagingFactory,
    bitmap: Option<(IWICBitmap, [u32; 2])>,
    /// Display-aspect native size once metadata is loaded.
    native: Option<[u32; 2]>,
    requested: Option<[u32; 2]>,
    force_transfer: bool,
    pending_seek: Option<f64>,
    ready_sent: bool,
    last_status: Instant,
    applied: u64,
}

impl Player {
    fn create(ctx: &ThreadCtx) -> Result<Self, String> {
        let err = |what: &str, e: windows_core::Error| format!("{what}: {}", describe_hr(e.code()));
        // SAFETY: COM calls on an initialized MTA thread with valid arguments; all returned
        // interfaces are reference counted by the bindings.
        unsafe {
            let factory: IMFMediaEngineClassFactory =
                CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)
                    .map_err(|e| err("Media Engine is not available", e))?;
            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 2).map_err(|e| err("MFCreateAttributes failed", e))?;
            let attrs = attrs.ok_or("MFCreateAttributes returned nothing")?;
            let notify: IMFMediaEngineNotify = Notify { tx: ctx.tx.clone() }.into();
            attrs
                .SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify)
                .map_err(|e| err("Media Engine setup failed", e))?;
            attrs
                .SetUINT32(
                    &MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT,
                    DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32,
                )
                .map_err(|e| err("Media Engine setup failed", e))?;
            let engine = factory
                .CreateInstance(0, &attrs)
                .map_err(|e| err("Cannot create the Media Engine", e))?;
            let wic: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                    .map_err(|e| err("WIC is not available", e))?;

            let mut player = Self {
                engine,
                wic,
                bitmap: None,
                native: None,
                requested: None,
                force_transfer: false,
                pending_seek: None,
                ready_sent: false,
                last_status: Instant::now(),
                applied: 0,
            };
            if let Err(e) = player.set_source(&ctx.path) {
                player.shutdown();
                return Err(e);
            }
            let _ = player.engine.SetMuted(ctx.muted);
            if let Err(e) = player.engine.Play() {
                player.shutdown();
                return Err(err("Cannot start playback", e));
            }
            Ok(player)
        }
    }

    /// Open the file as a byte stream (no URL escaping issues); the path doubles as the
    /// URL hint that selects the WebM source by extension.
    fn set_source(&self, path: &Path) -> Result<(), String> {
        let wide = HSTRING::from(path.as_os_str());
        let file = HSTRING::from(long_path(path).as_os_str());
        // SAFETY: valid NUL-terminated paths; engine is alive.
        unsafe {
            let stream = MFCreateFile(
                MF_ACCESSMODE_READ,
                MF_OPENMODE_FAIL_IF_NOT_EXIST,
                MF_FILEFLAGS_NONE,
                &file,
            )
            .map_err(|e| format!("Cannot open {}: {}", path.display(), describe_hr(e.code())))?;
            let ex: IMFMediaEngineEx = self
                .engine
                .cast()
                .map_err(|e| format!("Media Engine too old: {}", describe_hr(e.code())))?;
            let url = BSTR::from_wide(&wide);
            ex.SetSourceFromByteStream(&stream, &url)
                .map_err(|e| format!("Cannot play this file: {}", describe_hr(e.code())))
        }
    }

    fn run_loop(&mut self, ctx: &ThreadCtx) {
        loop {
            // Drain everything that arrived, then do periodic work.
            let mut first = match ctx.rx.recv_timeout(TICK) {
                Ok(i) => Some(i),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            };
            while let Some(input) = first.take().or_else(|| ctx.rx.try_recv().ok()) {
                match input {
                    Input::Cmd(_, Cmd::Shutdown) => return,
                    Input::Cmd(seq, cmd) => {
                        self.applied = seq;
                        self.command(ctx, cmd);
                    }
                    Input::Event {
                        event,
                        param1,
                        param2,
                    } => self.event(ctx, event, param1, param2),
                }
            }
            self.tick(ctx);
        }
    }

    fn command(&mut self, ctx: &ThreadCtx, cmd: Cmd) {
        // SAFETY: engine method calls on the owning thread.
        unsafe {
            match cmd {
                Cmd::Play => {
                    let _ = self.engine.Play();
                }
                Cmd::Pause => {
                    let _ = self.engine.Pause();
                }
                Cmd::Seek(t) => self.pending_seek = Some(t.max(0.0)),
                Cmd::SetMuted(m) => {
                    let _ = self.engine.SetMuted(m);
                    lock(&ctx.status).muted = m;
                }
                Cmd::OutputSize(size) => {
                    if self.requested != Some(size) {
                        self.requested = Some(size);
                        self.force_transfer = true;
                    }
                }
                Cmd::Shutdown => {}
            }
        }
        self.publish_status(ctx);
    }

    fn event(&mut self, ctx: &ThreadCtx, event: i32, param1: usize, param2: u32) {
        let ev = MF_MEDIA_ENGINE_EVENT(event);
        if ev == MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA || ev == MF_MEDIA_ENGINE_EVENT_FORMATCHANGE {
            self.native = self.query_video_size();
            self.force_transfer = true;
            if !self.ready_sent {
                self.ready_sent = true;
                let _ = ctx.ready_tx.try_send(Ok(()));
            }
        } else if ev == MF_MEDIA_ENGINE_EVENT_ERROR {
            let msg = self.describe_engine_error(param1, param2);
            log::error!("video: {msg}");
            lock(&ctx.status).error = Some(msg.clone());
            if !self.ready_sent {
                self.ready_sent = true;
                let _ = ctx.ready_tx.try_send(Err(msg));
            }
        } else if ev == MF_MEDIA_ENGINE_EVENT_SEEKED {
            self.force_transfer = true;
        }
        self.publish_status(ctx);
    }

    fn query_video_size(&self) -> Option<[u32; 2]> {
        // SAFETY: out-pointers to locals; engine alive.
        unsafe {
            if !self.engine.HasVideo().as_bool() {
                return None;
            }
            let (mut w, mut h) = (0u32, 0u32);
            self.engine
                .GetNativeVideoSize(Some(&mut w), Some(&mut h))
                .ok()?;
            if w == 0 || h == 0 {
                return None;
            }
            // Non-square pixels: stretch the width to the picture aspect ratio.
            let (mut ax, mut ay) = (0u32, 0u32);
            if self
                .engine
                .GetVideoAspectRatio(Some(&mut ax), Some(&mut ay))
                .is_ok()
                && ax > 0
                && ay > 0
            {
                let aw = (f64::from(h) * f64::from(ax) / f64::from(ay)).round() as u32;
                if aw > 0 && aw.abs_diff(w) > 1 {
                    return Some([aw, h]);
                }
            }
            Some([w, h])
        }
    }

    fn describe_engine_error(&self, param1: usize, param2: u32) -> String {
        let code = param1 as i32;
        let reason = match MF_MEDIA_ENGINE_ERR(code) {
            MF_MEDIA_ENGINE_ERR_ABORTED => "playback was aborted",
            MF_MEDIA_ENGINE_ERR_NETWORK => "the file could not be read",
            MF_MEDIA_ENGINE_ERR_DECODE => "the video could not be decoded",
            MF_MEDIA_ENGINE_ERR_SRC_NOT_SUPPORTED => {
                "the file is damaged, or its format or codec is not supported (is a WebM / VP8 / VP9 decoder installed?)"
            }
            MF_MEDIA_ENGINE_ERR_ENCRYPTED => "the media is encrypted",
            _ => "unknown error",
        };
        format!(
            "Media Foundation cannot play this video: {reason} ({})",
            describe_hr(HRESULT(param2 as i32))
        )
    }

    fn tick(&mut self, ctx: &ThreadCtx) {
        // SAFETY: engine method calls on the owning thread. The raw vtable call passes the
        // live interface pointer and a valid out-pointer, exactly like the generated wrapper.
        unsafe {
            if let Some(t) = self.pending_seek
                && !self.engine.IsSeeking().as_bool()
            {
                self.pending_seek = None;
                let _ = self.engine.SetCurrentTime(t);
            }
            if let Some(native) = self.native {
                let mut pts = 0i64;
                // The binding maps S_FALSE ("no new frame") to Ok, so call the vtable
                // directly to tell it apart from S_OK.
                let hr = (Interface::vtable(&self.engine).OnVideoStreamTick)(
                    self.engine.as_raw(),
                    &mut pts,
                );
                if (hr == windows_core::HRESULT(0) || (self.force_transfer && hr.is_ok()))
                    && let Some(frame) = self.transfer(native)
                {
                    self.force_transfer = false;
                    let mut s = lock(&ctx.status);
                    s.frame = Some(frame);
                    s.frames_transferred += 1;
                    drop(s);
                    ctx.waker.wake();
                }
            }
        }
        if self.last_status.elapsed() >= Duration::from_millis(100) {
            self.publish_status(ctx);
        }
    }

    /// Copy the current frame, scaled to the requested size (aspect kept, at most native).
    fn transfer(&mut self, native: [u32; 2]) -> Option<Frame> {
        let size = output_size(native, self.requested);
        // SAFETY: COM calls with valid rects; the WIC lock is released (dropped) before the
        // bitmap is reused, and the copied range is within the locked buffer
        // (`stride * (h - 1) + w * 4 <= len`, checked below).
        unsafe {
            if self.bitmap.as_ref().is_none_or(|(_, s)| *s != size) {
                let bmp = self
                    .wic
                    .CreateBitmap(
                        size[0],
                        size[1],
                        &GUID_WICPixelFormat32bppBGRA,
                        WICBitmapCacheOnLoad,
                    )
                    .map_err(|e| log::error!("video: CreateBitmap failed: {e}"))
                    .ok()?;
                self.bitmap = Some((bmp, size));
            }
            let (bmp, _) = self.bitmap.as_ref()?;
            let dst = RECT {
                left: 0,
                top: 0,
                right: size[0] as i32,
                bottom: size[1] as i32,
            };
            let border = MFARGB {
                rgbBlue: 0,
                rgbGreen: 0,
                rgbRed: 0,
                rgbAlpha: 255,
            };
            if let Err(e) = self
                .engine
                .TransferVideoFrame(bmp, None, &dst, Some(&border))
            {
                log::debug!("video: TransferVideoFrame failed: {e}");
                return None;
            }
            let rect = WICRect {
                X: 0,
                Y: 0,
                Width: size[0] as i32,
                Height: size[1] as i32,
            };
            let lock = bmp.Lock(&rect, WICBitmapLockRead.0 as u32).ok()?;
            let stride = lock.GetStride().ok()? as usize;
            let (mut len, mut data) = (0u32, std::ptr::null_mut::<u8>());
            lock.GetDataPointer(&mut len, &mut data).ok()?;
            let (w, h) = (size[0] as usize, size[1] as usize);
            if data.is_null() || stride < w * 4 || (len as usize) < stride * (h - 1) + w * 4 {
                return None;
            }
            let buf = std::slice::from_raw_parts(data, len as usize);
            let mut pixels = Vec::with_capacity(w * h);
            for y in 0..h {
                let row = &buf[y * stride..y * stride + w * 4];
                pixels.extend(
                    row.chunks_exact(4)
                        .map(|p| Color32::from_rgb(p[2], p[1], p[0])),
                );
            }
            Some(Frame {
                size: [w, h],
                pixels,
            })
        }
    }

    fn publish_status(&mut self, ctx: &ThreadCtx) {
        self.last_status = Instant::now();
        // SAFETY: engine getters on the owning thread.
        let (position, duration, paused, ended, has_audio) = unsafe {
            (
                self.engine.GetCurrentTime(),
                self.engine.GetDuration(),
                self.engine.IsPaused().as_bool(),
                self.engine.IsEnded().as_bool(),
                self.engine.HasAudio().as_bool(),
            )
        };
        let position = self.pending_seek.unwrap_or(position);
        let mut s = lock(&ctx.status);
        let changed = (s.position - position).abs() >= 0.05
            || s.paused != paused
            || s.ended != ended
            || s.video_size != self.native
            || s.applied != self.applied;
        s.position = if position.is_finite() {
            position.max(0.0)
        } else {
            0.0
        };
        s.duration = (duration.is_finite() && duration > 0.0).then_some(duration);
        s.paused = paused;
        s.ended = ended;
        s.has_audio = has_audio;
        s.video_size = self.native;
        s.applied = self.applied;
        drop(s);
        if changed {
            ctx.waker.wake();
        }
    }

    fn shutdown(&mut self) {
        // SAFETY: final call on the engine; it is released right after.
        unsafe {
            let _ = self.engine.Shutdown();
        }
        self.bitmap = None;
    }
}

/// EBML magic every WebM / Matroska file starts with.
const EBML_MAGIC: [u8; 4] = [0x1A, 0x45, 0xDF, 0xA3];

/// Reject unreadable files, and Matroska-family files without an EBML header, before
/// involving Media Foundation: its failed source resolution keeps a few handles alive for
/// the life of the process, and this gives a clearer message. Other extensions are left
/// to Media Foundation.
pub(crate) fn check_container(path: &Path) -> Result<(), String> {
    let mut file = File::open(path).map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
    let ext = stlv_core::files::ext_lower(path);
    if !matches!(ext.as_str(), "webm" | "mkv" | "mka" | "weba") {
        return Ok(());
    }
    let mut magic = [0u8; 4];
    match file.read_exact(&mut magic) {
        Ok(()) if magic == EBML_MAGIC => Ok(()),
        _ => Err(format!(
            "{} is not a valid WebM video (no EBML header).",
            path.display()
        )),
    }
}

/// Frame size for display: the requested box (aspect of `native` kept), never above native.
pub(crate) fn output_size(native: [u32; 2], requested: Option<[u32; 2]>) -> [u32; 2] {
    let [nw, nh] = native;
    let Some([rw, rh]) = requested else {
        return native;
    };
    if rw == 0 || rh == 0 || (rw >= nw && rh >= nh) {
        return native;
    }
    let s = (f64::from(rw) / f64::from(nw))
        .min(f64::from(rh) / f64::from(nh))
        .min(1.0);
    let w = ((f64::from(nw) * s).round() as u32).clamp(1, nw);
    let h = ((f64::from(nh) * s).round() as u32).clamp(1, nh);
    [w, h]
}

/// "0xC00D36C4: <system message>".
pub(crate) fn describe_hr(hr: HRESULT) -> String {
    let msg = hr.message();
    let msg = msg.trim();
    if msg.is_empty() {
        format!("HRESULT 0x{:08X}", hr.0 as u32)
    } else {
        format!("0x{:08X}: {msg}", hr.0 as u32)
    }
}

/// `path` in a form Win32 `CreateFileW` opens past MAX_PATH: MFCreateFile has no long-path
/// handling of its own (std::fs adds the verbatim prefix itself), so long paths are made
/// absolute and verbatim. Short and already-verbatim/device paths are returned unchanged.
fn long_path(path: &Path) -> PathBuf {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    const MAX_PATH: usize = 260;
    let Ok(abs) = std::path::absolute(path) else {
        return path.to_path_buf();
    };
    let wide: Vec<u16> = abs.as_os_str().encode_wide().collect();
    let bs = u16::from(b'\\');
    let prefixed = wide.len() >= 4
        && wide[0] == bs
        && wide[1] == bs
        && (wide[2] == u16::from(b'?') || wide[2] == u16::from(b'.'))
        && wide[3] == bs;
    if wide.len() < MAX_PATH || prefixed {
        return path.to_path_buf();
    }
    let (prefix, rest) = if wide.starts_with(&[bs, bs]) {
        (r"\\?\UNC\", &wide[2..])
    } else {
        (r"\\?\", &wide[..])
    };
    let mut out: Vec<u16> = prefix.encode_utf16().collect();
    out.extend_from_slice(rest);
    PathBuf::from(OsString::from_wide(&out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_paths_become_verbatim() {
        let short = Path::new(r"C:\videos\clip.webm");
        assert_eq!(long_path(short), short);
        let seg = "d".repeat(100);
        let long = format!(r"C:\{seg}\{seg}\{seg}\clip.webm");
        assert_eq!(
            long_path(Path::new(&long)),
            Path::new(&format!(r"\\?\{long}"))
        );
        // Forward slashes are normalized before the prefix (verbatim paths take them literally).
        let mixed = long.replace('\\', "/");
        assert_eq!(
            long_path(Path::new(&mixed)),
            Path::new(&format!(r"\\?\{long}"))
        );
        let unc = format!(r"\\server\share\{seg}\{seg}\{seg}\clip.webm");
        assert_eq!(
            long_path(Path::new(&unc)),
            Path::new(&format!(r"\\?\UNC\{}", &unc[2..]))
        );
        let verbatim = format!(r"\\?\{long}");
        assert_eq!(long_path(Path::new(&verbatim)), Path::new(&verbatim));
    }

    #[test]
    fn output_size_keeps_aspect_and_caps_at_native() {
        assert_eq!(output_size([1920, 1080], None), [1920, 1080]);
        assert_eq!(output_size([1920, 1080], Some([4000, 3000])), [1920, 1080]);
        assert_eq!(output_size([1920, 1080], Some([960, 1080])), [960, 540]);
        assert_eq!(output_size([1920, 1080], Some([0, 10])), [1920, 1080]);
        // Never collapses to zero.
        assert_eq!(output_size([100, 50], Some([1, 1])), [1, 1]);
    }

    #[test]
    fn container_check() {
        let dir = std::env::temp_dir().join(format!("stlv-media-mf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, data: &[u8]| {
            let p = dir.join(name);
            std::fs::write(&p, data).unwrap();
            p
        };
        assert!(check_container(&write("ok.webm", &[0x1A, 0x45, 0xDF, 0xA3, 0x9F])).is_ok());
        assert!(
            check_container(&write("text.webm", b"hello world"))
                .unwrap_err()
                .contains("EBML")
        );
        assert!(check_container(&write("short.WEBM", &[0x1A])).is_err());
        assert!(check_container(&write("empty.mkv", b"")).is_err());
        // Other containers are Media Foundation's call.
        assert!(check_container(&write("clip.mp4", b"....ftyp")).is_ok());
        assert!(
            check_container(&dir.join("missing.webm"))
                .unwrap_err()
                .starts_with("Cannot open")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hresult_text() {
        let s = describe_hr(HRESULT(0x80004005u32 as i32));
        assert!(s.starts_with("0x80004005"), "{s}");
    }
}
