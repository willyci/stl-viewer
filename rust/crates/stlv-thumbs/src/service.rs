use std::fs;
use std::num::NonZeroUsize;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, SendTimeoutError, Sender, bounded};
use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, RgbaImage};
use stlv_core::FileKind;
use stlv_render::{PreparedModel, ThumbnailRenderer};

use crate::ThumbnailCache;
use crate::cache::FileStamp;

/// Meshes larger than this are not rendered for the folder grid (app.py used 10 MB).
pub const MAX_RENDER_FILE_SIZE: u64 = 100 * 1024 * 1024;
/// Meshes larger than this render after all the smaller ones (largest last), so a few big
/// files do not hold up the rest of the folder's previews.
pub const LARGE_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Logical size of a card's preview image; `Ready` images fit within 2x this for HiDPI.
pub const THUMB_DISPLAY_SIZE: (u32, u32) = (110, 96);

const READY_FIT: (u32, u32) = (2 * THUMB_DISPLAY_SIZE.0, 2 * THUMB_DISPLAY_SIZE.1);
/// SVG cards: 2x the Python card's fixed 90x80 `QSvgWidget`.
const SVG_FIT: (u32, u32) = (180, 160);
const MAX_WORKERS: usize = 8;
/// Parsed meshes waiting for the render thread.
const RENDER_QUEUE_LEN: usize = 2;
/// Cap on the summed file size of meshes being parsed or waiting to render, across all jobs.
/// Pre-caching has no size limit, so this keeps a folder of huge files from being parsed all
/// at once, and parses still running for cancelled jobs hold back the next job's.
const IN_FLIGHT_BYTES: u64 = 256 * 1024 * 1024;
/// Cap on the summed decoded size of images being decoded and shrunk, across all jobs: a
/// folder of camera photos must not be decoded at full resolution eight at a time.
const DECODE_BYTES: u64 = 384 * 1024 * 1024;
/// Undelivered events; workers wait (cancellably) while the UI is not polling.
const EVENT_QUEUE_LEN: usize = 512;
/// How often blocked threads re-check their job's cancel flag.
const CANCEL_POLL: Duration = Duration::from_millis(20);
/// How long dropping the service waits for cancelled jobs before leaving them to finish
/// (or be ended by process exit): a parse in progress cannot be interrupted.
const DROP_GRACE: Duration = Duration::from_millis(500);
/// Pre-cached thumbnails rarely show on screen, so the UI is woken for them at most this often
/// rather than once per thumbnail.
const BACKGROUND_WAKE: Duration = Duration::from_millis(250);
/// Rendered thumbnails are written to the cache in one transaction once this many are waiting
/// or the oldest has waited [`WRITE_DELAY`].
const WRITE_BATCH: usize = 32;
const WRITE_DELAY: Duration = Duration::from_millis(250);

#[derive(Debug)]
pub enum ThumbEvent {
    /// The folder job has classified its files: `total` uncached meshes will be rendered
    /// (the `total` of the `Progress` / `Finished` events that follow). Sent first, and
    /// also when `total` is 0.
    Queued { total: usize },
    /// A card image is ready (aspect preserved, fits within 2 x [`THUMB_DISPLAY_SIZE`]).
    Ready {
        path: PathBuf,
        image: image::RgbaImage,
    },
    /// Folder-job mesh rendering progress, sent just before each queued mesh is rendered:
    /// `done` of `total` are finished (so this is render `done + 1`), `current` is the file
    /// name about to be rendered. Files that fail to parse count as done.
    Progress {
        done: usize,
        total: usize,
        current: String,
    },
    /// The folder job finished rendering all `total` uncached meshes. Not sent when the
    /// folder needed no renders.
    Finished { total: usize },
}

/// Parses a mesh and does the CPU side of rendering it (worker threads).
type LoadFn = dyn Fn(&Path) -> Result<PreparedModel, String> + Send + Sync;
/// Draws a prepared mesh (the render thread).
type RenderFn = dyn Fn(&PreparedModel) -> Result<RgbaImage, String> + Send + Sync;
type WakeFn = dyn Fn() + Send + Sync;

/// What every job thread needs.
struct Shared {
    cache: Arc<ThumbnailCache>,
    load: Box<LoadFn>,
    render: Option<Box<RenderFn>>,
    wake: Box<WakeFn>,
    events: Sender<(u64, ThumbEvent)>,
    /// File sizes of meshes being parsed or waiting to render.
    parse_budget: Budget,
    /// Decoded sizes of images being decoded.
    decode_budget: Budget,
    max_render_bytes: u64,
    large_file_bytes: u64,
}

/// Memory caps shared by all jobs (tests shrink them).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub parse_bytes: u64,
    pub decode_bytes: u64,
    pub max_render_bytes: u64,
    pub large_file_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            parse_bytes: IN_FLIGHT_BYTES,
            decode_bytes: DECODE_BYTES,
            max_render_bytes: MAX_RENDER_FILE_SIZE,
            large_file_bytes: LARGE_FILE_SIZE,
        }
    }
}

/// Background thumbnail pipeline (app.py `ThumbnailGeneratorWorker` + render queue + precache).
///
/// Each job runs on its own coordinator thread: it classifies the files, then a pool of up
/// to `min(cpus, 8, n)` workers decodes cached PNGs and media and parses and prepares
/// ([`PreparedModel`]) uncached meshes, feeding a bounded queue drained by one GPU render
/// thread (which also writes the cache).
/// Events carry their job id and [`poll`](Self::poll) drops those of cancelled jobs.
pub struct ThumbnailService {
    shared: Arc<Shared>,
    events: Receiver<(u64, ThumbEvent)>,
    last_job_id: u64,
    folder: Option<Job>,
    precache: Option<Job>,
    /// Coordinators of cancelled jobs that may still be finishing a file.
    retired: Vec<JoinHandle<()>>,
}

struct Job {
    id: u64,
    cancel: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl ThumbnailService {
    /// `wake` is called from worker threads whenever new events are queued
    /// (the app passes `move || ctx.request_repaint()`).
    pub fn new(
        cache: Arc<ThumbnailCache>,
        renderer: Option<Arc<ThumbnailRenderer>>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let render = renderer
            .map(|r| Box::new(move |m: &PreparedModel| r.render_prepared(m)) as Box<RenderFn>);
        Self::with_backend(
            cache,
            Box::new(load_and_prepare),
            render,
            Box::new(wake),
            Limits::default(),
        )
    }

    /// [`new`](Self::new) with the mesh loader, renderer and limits swapped out (tests).
    pub(crate) fn with_backend(
        cache: Arc<ThumbnailCache>,
        load: Box<LoadFn>,
        render: Option<Box<RenderFn>>,
        wake: Box<WakeFn>,
        limits: Limits,
    ) -> Self {
        let (tx, rx) = bounded(EVENT_QUEUE_LEN);
        Self {
            shared: Arc::new(Shared {
                cache,
                load,
                render,
                wake,
                events: tx,
                parse_budget: Budget::new(limits.parse_bytes),
                decode_budget: Budget::new(limits.decode_bytes),
                max_render_bytes: limits.max_render_bytes,
                large_file_bytes: limits.large_file_bytes,
            }),
            events: rx,
            last_job_id: 0,
            folder: None,
            precache: None,
            retired: Vec::new(),
        }
    }

    /// Start the folder-grid job for `files`, cancelling any previous folder job.
    pub fn start_folder(&mut self, files: Vec<PathBuf>) {
        self.cancel_folder();
        self.folder = self.spawn_job("stlv-thumbs-folder", move |ctx| run_folder(&ctx, files));
    }

    /// Stop the folder job (app.py `_stop_workers`); its pending events are dropped.
    pub fn cancel_folder(&mut self) {
        if let Some(job) = self.folder.take() {
            self.retire(job);
        }
    }

    /// Pre-render uncached meshes in `dir` in the background, replacing any previous precache.
    pub fn precache_dir(&mut self, dir: &Path) {
        self.cancel_precache();
        if self.shared.render.is_none() {
            return;
        }
        let dir = dir.to_path_buf();
        self.precache = self.spawn_job("stlv-thumbs-precache", move |ctx| run_precache(&ctx, &dir));
    }

    pub fn cancel_precache(&mut self) {
        if let Some(job) = self.precache.take() {
            self.retire(job);
        }
    }

    /// Drain queued events without blocking; call once per UI frame, also while the grid
    /// is hidden (workers pause once 512 events are waiting).
    pub fn poll(&mut self) -> Vec<ThumbEvent> {
        self.retired.retain(|t| !t.is_finished());
        let live = [&self.folder, &self.precache].map(|job| job.as_ref().map(|j| j.id));
        self.events
            .try_iter()
            .filter(|(id, _)| live.contains(&Some(*id)))
            .map(|(_, event)| event)
            .collect()
    }

    /// True while a folder or precache job is still working or has undelivered events
    /// (lets the app's `--screenshot` mode wait for the grid to settle).
    pub fn is_busy(&self) -> bool {
        let running = |job: &Option<Job>| job.as_ref().is_some_and(|j| !j.thread.is_finished());
        running(&self.folder) || running(&self.precache) || !self.events.is_empty()
    }

    fn spawn_job(&mut self, name: &str, body: impl FnOnce(JobCtx) + Send + 'static) -> Option<Job> {
        self.last_job_id += 1;
        let ctx = JobCtx {
            id: self.last_job_id,
            cancel: Arc::default(),
            shared: Arc::clone(&self.shared),
        };
        let (id, cancel) = (ctx.id, Arc::clone(&ctx.cancel));
        match thread::Builder::new()
            .name(name.into())
            .spawn(move || body(ctx))
        {
            Ok(thread) => Some(Job { id, cancel, thread }),
            Err(e) => {
                log::error!("cannot start thumbnail job: {e}");
                None
            }
        }
    }

    fn retire(&mut self, job: Job) {
        job.cancel.store(true, Ordering::Release);
        self.retired.push(job.thread);
    }
}

impl Drop for ThumbnailService {
    /// Cancels both jobs and gives them [`DROP_GRACE`] to stop; a thread still stuck in a
    /// parse after that is detached, so closing the app never waits for a slow or hostile
    /// file. Detached threads deliver nothing (the event receiver is gone).
    fn drop(&mut self) {
        self.cancel_folder();
        self.cancel_precache();
        let deadline = Instant::now() + DROP_GRACE;
        while self.retired.iter().any(|t| !t.is_finished()) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        for thread in self.retired.drain(..) {
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
    }
}

/// Parse and prepare on the worker pool so the render thread only does GPU work (the
/// split-normal mesh dominates a thumbnail's cost).
fn load_and_prepare(path: &Path) -> Result<PreparedModel, String> {
    let model = stlv_core::load_model(path).map_err(|e| e.to_string())?;
    Ok(PreparedModel::new(Arc::new(model)))
}

/// A job's view of the service, shared by its threads.
#[derive(Clone)]
struct JobCtx {
    id: u64,
    cancel: Arc<AtomicBool>,
    shared: Arc<Shared>,
}

impl JobCtx {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }

    /// Queue `event` and wake the UI.
    fn emit(&self, event: ThumbEvent) {
        if self.queue(event) {
            (self.shared.wake)();
        }
    }

    /// Queue `event` unless the job is cancelled, waiting while the queue is full; `true` if
    /// it was queued.
    fn queue(&self, mut event: ThumbEvent) -> bool {
        while !self.cancelled() {
            match self
                .shared
                .events
                .send_timeout((self.id, event), CANCEL_POLL)
            {
                Ok(()) => return true,
                Err(SendTimeoutError::Timeout((_, unsent))) => event = unsent,
                Err(SendTimeoutError::Disconnected(_)) => return false,
            }
        }
        false
    }
}

/// Queues a job's events and wakes the UI for them at most once per `every`; events queued
/// in between get a trailing wake-up from [`flush_if_due`](Self::flush_if_due) or
/// [`flush`](Self::flush).
struct Notifier<'a> {
    ctx: &'a JobCtx,
    every: Duration,
    last: Option<Instant>,
    pending: bool,
}

impl<'a> Notifier<'a> {
    fn new(ctx: &'a JobCtx, every: Duration) -> Self {
        Self {
            ctx,
            every,
            last: None,
            pending: false,
        }
    }

    fn emit(&mut self, event: ThumbEvent) {
        if self.ctx.queue(event) {
            self.pending = true;
            self.flush_if_due();
        }
    }

    fn flush_if_due(&mut self) {
        if self.last.is_none_or(|t| t.elapsed() >= self.every) {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if std::mem::take(&mut self.pending) {
            (self.ctx.shared.wake)();
            self.last = Some(Instant::now());
        }
    }
}

enum Task {
    /// Mesh with a fresh cached PNG.
    Cached(PathBuf),
    /// Static image or GIF (first frame).
    Raster(PathBuf),
    Svg(PathBuf),
    /// Uncached mesh: parsed by a worker, rendered on the render thread.
    Mesh(PathBuf, FileStamp),
}

struct Parsed<'a> {
    path: PathBuf,
    stamp: FileStamp,
    model: Result<PreparedModel, String>,
    /// Held until the mesh is rendered or dropped.
    _permit: Permit<'a>,
}

fn run_folder(ctx: &JobCtx, files: Vec<PathBuf>) {
    let can_render = ctx.shared.render.is_some();
    let mut tasks = Vec::with_capacity(files.len());
    for path in files {
        if ctx.cancelled() {
            return;
        }
        let task = match FileKind::from_path(&path) {
            FileKind::Image | FileKind::Gif => Task::Raster(path),
            FileKind::Svg => Task::Svg(path),
            kind if kind.is_mesh() => {
                let Ok(meta) = fs::metadata(&path) else {
                    continue;
                };
                let stamp = FileStamp::of(&meta);
                if ctx.shared.cache.is_valid_for(&path, stamp) {
                    Task::Cached(path)
                } else if can_render && meta.is_file() && stamp.size <= ctx.shared.max_render_bytes
                {
                    Task::Mesh(path, stamp)
                } else {
                    continue;
                }
            }
            _ => continue,
        };
        tasks.push(task);
    }
    // Meshes queue for the single render thread, so put every other card first (stable
    // sort) rather than behind workers waiting to hand over a parsed mesh; large meshes go
    // last, smallest first.
    let large = ctx.shared.large_file_bytes;
    tasks.sort_by_key(|task| match task {
        Task::Mesh(_, stamp) if stamp.size > large => (2, stamp.size),
        Task::Mesh(..) => (1, 0),
        _ => (0, 0),
    });
    let total = tasks.iter().filter(|t| matches!(t, Task::Mesh(..))).count();
    ctx.emit(ThumbEvent::Queued { total });
    run_tasks(ctx, &tasks, true);
}

/// app.py `_precache_directory_thumbnails`: every uncached mesh directly in `dir`, any size.
fn run_precache(ctx: &JobCtx, dir: &Path) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            log::warn!("Error scanning {} for pre-caching: {e}", dir.display());
            return;
        }
    };
    let mut meshes = Vec::new();
    for entry in entries.flatten() {
        if ctx.cancelled() {
            return;
        }
        let path = entry.path();
        if !FileKind::from_path(&path).is_mesh() {
            continue;
        }
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        let stamp = FileStamp::of(&meta);
        if meta.is_file() && !ctx.shared.cache.is_valid_for(&path, stamp) {
            meshes.push((path, stamp));
        }
    }
    meshes.sort_by_cached_key(|(path, _)| stlv_core::files::sort_key(path));
    let tasks: Vec<Task> = meshes
        .into_iter()
        .map(|(path, stamp)| Task::Mesh(path, stamp))
        .collect();
    run_tasks(ctx, &tasks, false);
}

/// Run `tasks` on the worker pool (+ render thread when meshes need rendering); returns
/// when all are done or the job is cancelled.
fn run_tasks(ctx: &JobCtx, tasks: &[Task], foreground: bool) {
    let total = tasks.iter().filter(|t| matches!(t, Task::Mesh(..))).count();
    let cpus = thread::available_parallelism().map_or(4, NonZeroUsize::get);
    let workers = cpus.min(MAX_WORKERS).min(tasks.len());
    if workers == 0 {
        return;
    }
    let next = &AtomicUsize::new(0);
    thread::scope(|s| {
        // Meshes are skipped (`None`) if the render thread cannot be started.
        let mut mesh_tx = None;
        if total > 0 {
            let (tx, rx) = bounded::<Parsed>(RENDER_QUEUE_LEN);
            let render = move || render_loop(ctx, rx, total, foreground);
            match thread::Builder::new()
                .name("stlv-thumbs-render".into())
                .spawn_scoped(s, render)
            {
                Ok(_) => mesh_tx = Some(tx),
                Err(e) => log::error!("cannot start thumbnail render thread: {e}"),
            }
        }
        let mut started = 0;
        for i in 0..workers {
            let tx = mesh_tx.clone();
            let work = move || worker_loop(ctx, tasks, next, tx.as_ref());
            match thread::Builder::new()
                .name(format!("stlv-thumbs-worker-{i}"))
                .spawn_scoped(s, work)
            {
                Ok(_) => started += 1,
                Err(e) => log::warn!("cannot start thumbnail worker: {e}"),
            }
        }
        if started == 0 {
            worker_loop(ctx, tasks, next, mesh_tx.as_ref());
        }
    });
}

fn worker_loop<'a>(
    ctx: &'a JobCtx,
    tasks: &[Task],
    next: &AtomicUsize,
    meshes: Option<&Sender<Parsed<'a>>>,
) {
    while !ctx.cancelled() {
        let Some(task) = tasks.get(next.fetch_add(1, Ordering::Relaxed)) else {
            return;
        };
        let (path, image) = match task {
            Task::Cached(path) => (path, guarded(|| decode_cached(&ctx.shared.cache, path))),
            Task::Raster(path) => (
                path,
                guarded(|| decode_raster(path, &ctx.shared.decode_budget, &ctx.cancel)),
            ),
            Task::Svg(path) => (path, guarded(|| rasterize_svg(path))),
            Task::Mesh(path, stamp) => {
                let Some(meshes) = meshes else { continue };
                let Some(permit) = ctx.shared.parse_budget.acquire(stamp.size, &ctx.cancel) else {
                    return;
                };
                let model = guarded(|| (ctx.shared.load)(path));
                let parsed = Parsed {
                    path: path.clone(),
                    stamp: *stamp,
                    model,
                    _permit: permit,
                };
                // Sending fails only once the render thread has quit (job cancelled).
                if ctx.cancelled() || meshes.send(parsed).is_err() {
                    return;
                }
                continue;
            }
        };
        match image {
            Ok(image) => ctx.emit(ThumbEvent::Ready {
                path: path.clone(),
                image,
            }),
            Err(e) => log::debug!("no thumbnail for {}: {e}", path.display()),
        }
    }
}

/// Render parsed meshes as they arrive. The `foreground` (folder grid) job reports progress
/// and wakes the UI for every event; the background pre-cache wakes it only now and then.
fn render_loop(ctx: &JobCtx, meshes: Receiver<Parsed<'_>>, total: usize, foreground: bool) {
    let Some(render) = ctx.shared.render.as_deref() else {
        return;
    };
    let wake_every = if foreground {
        Duration::ZERO
    } else {
        BACKGROUND_WAKE
    };
    let mut events = Notifier::new(ctx, wake_every);
    let mut writes = WriteBatch::new(&ctx.shared.cache);
    let mut done = 0;
    while done < total && !ctx.cancelled() {
        events.flush_if_due();
        writes.flush_if_due();
        let parsed = match meshes.recv_timeout(CANCEL_POLL) {
            Ok(parsed) => parsed,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if ctx.cancelled() {
            break;
        }
        if foreground {
            let current = file_name(&parsed.path);
            events.emit(ThumbEvent::Progress {
                done,
                total,
                current,
            });
        }
        match &parsed.model {
            Ok(model) => {
                if let Some(image) =
                    render_one(render, &parsed.path, parsed.stamp, model, &mut writes)
                {
                    let path = parsed.path.clone();
                    events.emit(ThumbEvent::Ready { path, image });
                }
            }
            Err(e) => log::warn!(
                "Error loading geometry for thumbnail {}: {e}",
                parsed.path.display()
            ),
        }
        done += 1;
    }
    if ctx.cancelled() {
        // Give back the parse budget of meshes nobody will render.
        meshes.try_iter().for_each(drop);
        return;
    }
    if foreground {
        events.emit(ThumbEvent::Finished { total });
    }
    events.flush();
}

/// Render `model`, queue its PNG for the cache and return the card image.
fn render_one(
    render: &RenderFn,
    path: &Path,
    stamp: FileStamp,
    model: &PreparedModel,
    writes: &mut WriteBatch<'_>,
) -> Option<RgbaImage> {
    // Python's generate_vtk_thumbnail returned nothing for empty models.
    if model.model.vertices.is_empty() {
        return None;
    }
    let image = match guarded(|| render(model)) {
        Ok(image) => image,
        Err(e) => {
            log::warn!("Error rendering thumbnail for {}: {e}", path.display());
            return None;
        }
    };
    match encode_png(&image) {
        Ok(png) => writes.push(path.to_path_buf(), stamp, png),
        Err(e) => log::warn!("cannot encode thumbnail for {}: {e}", path.display()),
    }
    Some(fit_within(DynamicImage::ImageRgba8(image), READY_FIT))
}

/// Rendered thumbnails waiting to be stored in one transaction: one disk sync per batch
/// instead of one per thumbnail. Written when full, once the oldest has waited
/// [`WRITE_DELAY`], and on drop (whichever way the render thread ends).
struct WriteBatch<'a> {
    cache: &'a ThumbnailCache,
    rows: Vec<(PathBuf, FileStamp, Vec<u8>)>,
    oldest: Option<Instant>,
}

impl<'a> WriteBatch<'a> {
    fn new(cache: &'a ThumbnailCache) -> Self {
        Self {
            cache,
            rows: Vec::new(),
            oldest: None,
        }
    }

    fn push(&mut self, path: PathBuf, stamp: FileStamp, png: Vec<u8>) {
        self.oldest.get_or_insert_with(Instant::now);
        self.rows.push((path, stamp, png));
        if self.rows.len() >= WRITE_BATCH {
            self.flush();
        }
    }

    fn flush_if_due(&mut self) {
        if self.oldest.is_some_and(|t| t.elapsed() >= WRITE_DELAY) {
            self.flush();
        }
    }

    fn flush(&mut self) {
        // `put_many` logs its own errors; the cards already have their images.
        let _ = self.cache.put_many(&self.rows);
        self.rows.clear();
        self.oldest = None;
    }
}

impl Drop for WriteBatch<'_> {
    fn drop(&mut self) {
        self.flush();
    }
}

/// Weighted semaphore shared by all jobs. Work still running for a cancelled job keeps its
/// share until it ends, so restarting jobs cannot pile up parses.
struct Budget {
    limit: u64,
    used: Mutex<u64>,
    freed: Condvar,
}

impl Budget {
    fn new(limit: u64) -> Self {
        Self {
            limit,
            used: Mutex::new(0),
            freed: Condvar::new(),
        }
    }

    /// Wait until `cost` fits (anything fits while nothing else holds a share); `None` if
    /// `cancel` is set meanwhile.
    fn acquire(&self, cost: u64, cancel: &AtomicBool) -> Option<Permit<'_>> {
        let mut used = self.lock();
        loop {
            if cancel.load(Ordering::Acquire) {
                return None;
            }
            if *used == 0 || used.saturating_add(cost) <= self.limit {
                *used = used.saturating_add(cost);
                return Some(Permit { budget: self, cost });
            }
            used = self
                .freed
                .wait_timeout(used, CANCEL_POLL)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn lock(&self) -> MutexGuard<'_, u64> {
        self.used.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A share of a [`Budget`], given back on drop.
struct Permit<'a> {
    budget: &'a Budget,
    cost: u64,
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut used = self.budget.lock();
        *used = used.saturating_sub(self.cost);
        self.budget.freed.notify_all();
    }
}

/// Run parser/renderer/decoder code, turning a panic into an error so one bad file
/// cannot take down the pipeline.
fn guarded<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|panic| {
        let msg = panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".into());
        Err(format!("panicked: {msg}"))
    })
}

fn decode_cached(cache: &ThumbnailCache, path: &Path) -> Result<RgbaImage, String> {
    let png = cache.get_png(path).ok_or("thumbnail no longer cached")?;
    let image = image::load_from_memory(&png).map_err(|e| e.to_string())?;
    Ok(fit_within(image, READY_FIT))
}

/// Static image or the first frame of a GIF, format sniffed from the content. Its decoded
/// size (from the header) is taken from `budget` until the image has been shrunk.
fn decode_raster(path: &Path, budget: &Budget, cancel: &AtomicBool) -> Result<RgbaImage, String> {
    let decoder = image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| e.to_string())?
        .into_decoder()
        .map_err(|e| e.to_string())?;
    let bytes = decoder.total_bytes();
    // What `ImageReader::decode` checks before allocating.
    image::Limits::default()
        .reserve(bytes)
        .map_err(|e| e.to_string())?;
    let _permit = budget.acquire(bytes, cancel).ok_or("cancelled")?;
    let image = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    Ok(fit_within(image, READY_FIT))
}

fn rasterize_svg(path: &Path) -> Result<RgbaImage, String> {
    stlv_media::rasterize_svg_fit(path, SVG_FIT.0, SVG_FIT.1)
}

/// Scale down (never up) to fit within `max`, keeping the aspect ratio.
fn fit_within(image: DynamicImage, (max_w, max_h): (u32, u32)) -> RgbaImage {
    let (w, h) = (image.width(), image.height());
    if w <= max_w && h <= max_h {
        return image.into_rgba8();
    }
    let scale = f64::min(
        f64::from(max_w) / f64::from(w),
        f64::from(max_h) / f64::from(h),
    );
    let nw = ((f64::from(w) * scale).round() as u32).clamp(1, max_w);
    let nh = ((f64::from(h) * scale).round() as u32).clamp(1, max_h);
    // Box-average big images most of the way first: far cheaper than a wide filter kernel.
    let image = if w > nw * 4 && h > nh * 4 {
        image.thumbnail_exact(nw * 2, nh * 2)
    } else {
        image
    };
    image
        .resize_exact(nw, nh, FilterType::Triangle)
        .into_rgba8()
}

fn encode_png(image: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(png)
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests;
