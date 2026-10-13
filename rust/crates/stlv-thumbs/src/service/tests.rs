use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::time::Instant;

use image::{GenericImageView, Rgba, RgbaImage};
use stlv_core::ModelData;

use super::*;
use crate::test_util::TempDir;

const CYAN: Rgba<u8> = Rgba([0, 240, 255, 255]);
const MAGENTA: Rgba<u8> = Rgba([255, 0, 255, 255]);

// ---------------------------------------------------------------------------------------
// Fixtures

fn cube_triangles(size: f32) -> Vec<[[f32; 3]; 3]> {
    let p = |x: u8, y: u8, z: u8| {
        [
            f32::from(x) * size,
            f32::from(y) * size,
            f32::from(z) * size,
        ]
    };
    let quads = [
        [p(0, 0, 0), p(0, 1, 0), p(1, 1, 0), p(1, 0, 0)],
        [p(0, 0, 1), p(1, 0, 1), p(1, 1, 1), p(0, 1, 1)],
        [p(0, 0, 0), p(1, 0, 0), p(1, 0, 1), p(0, 0, 1)],
        [p(0, 1, 0), p(0, 1, 1), p(1, 1, 1), p(1, 1, 0)],
        [p(0, 0, 0), p(0, 0, 1), p(0, 1, 1), p(0, 1, 0)],
        [p(1, 0, 0), p(1, 1, 0), p(1, 1, 1), p(1, 0, 1)],
    ];
    quads
        .iter()
        .flat_map(|[a, b, c, d]| [[*a, *b, *c], [*a, *c, *d]])
        .collect()
}

fn write_binary_stl(path: &Path, triangles: &[[[f32; 3]; 3]]) {
    let mut out = Vec::with_capacity(84 + 50 * triangles.len());
    out.extend_from_slice(&[b' '; 80]);
    out.extend_from_slice(&(triangles.len() as u32).to_le_bytes());
    for tri in triangles {
        out.extend_from_slice(&[0; 12]);
        for v in tri {
            for c in v {
                out.extend_from_slice(&c.to_le_bytes());
            }
        }
        out.extend_from_slice(&[0; 2]);
    }
    fs::write(path, out).unwrap();
}

/// A wavy height field with `n` triangles: a valid, renderable mesh of any size.
fn write_terrain_stl(path: &Path, min_triangles: usize) {
    let side = ((min_triangles as f64 / 2.0).sqrt().ceil() as usize).max(1);
    let z = |i: usize, j: usize| ((i as f32 * 0.07).sin() + (j as f32 * 0.05).cos()) * 4.0;
    let v = |i: usize, j: usize| [i as f32, j as f32, z(i, j)];
    let mut tris = Vec::with_capacity(side * side * 2);
    for i in 0..side {
        for j in 0..side {
            tris.push([v(i, j), v(i + 1, j), v(i + 1, j + 1)]);
            tris.push([v(i, j), v(i + 1, j + 1), v(i, j + 1)]);
        }
    }
    write_binary_stl(path, &tris);
}

fn write_gif(path: &Path, frames: &[Rgba<u8>], (w, h): (u32, u32)) {
    let file = fs::File::create(path).unwrap();
    let mut encoder = image::codecs::gif::GifEncoder::new(file);
    encoder
        .set_repeat(image::codecs::gif::Repeat::Infinite)
        .unwrap();
    let frames = frames.iter().map(|&c| {
        let delay = image::Delay::from_numer_denom_ms(100, 1);
        image::Frame::from_parts(RgbaImage::from_pixel(w, h, c), 0, 0, delay)
    });
    encoder.encode_frames(frames).unwrap();
}

const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100">
  <rect width="200" height="100" fill="#ff8800"/>
  <circle cx="100" cy="50" r="40" fill="#0066ff"/>
  <text x="10" y="95" font-family="Segoe UI" font-size="12" fill="#ffffff">Logo</text>
</svg>"##;

fn png_bytes(w: u32, h: u32, c: Rgba<u8>) -> Vec<u8> {
    encode_png(&RgbaImage::from_pixel(w, h, c)).unwrap()
}

/// One of everything the folder grid can meet.
struct Folder {
    dir: TempDir,
    cache: Arc<ThumbnailCache>,
}

impl Folder {
    fn new(tag: &str) -> Self {
        let dir = TempDir::new(tag);
        let cache =
            Arc::new(ThumbnailCache::open(&dir.path().join("db").join("thumbs.db")).unwrap());
        Self { dir, cache }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn populate(&self) {
        write_binary_stl(&self.path("a_cube.stl"), &cube_triangles(10.0));
        fs::write(
            self.path("b_tri.obj"),
            "v 0 0 0\nv 10 0 0\nv 0 10 0\nv 0 0 10\nf 1 2 3\nf 1 2 4\nf 1 3 4\nf 2 3 4\n",
        )
        .unwrap();
        fs::write(
            self.path("c_path.gcode"),
            "G90\nG1 X0 Y0 Z0.2 E1\nG1 X20 Y0 E2\nG1 X20 Y20 E3\nG1 X0 Y20 E4\nG1 X0 Y0 Z0.4 E5\n",
        )
        .unwrap();
        fs::write(self.path("corrupt.stl"), b"not an stl").unwrap();
        write_terrain_stl(&self.path("huge.stl"), 220_000);
        write_binary_stl(&self.path("cached.3mf"), &cube_triangles(1.0));
        self.cache
            .put(&self.path("cached.3mf"), &png_bytes(228, 200, MAGENTA))
            .unwrap();
        write_terrain_stl(&self.path("cached_huge.stl"), 220_000);
        self.cache
            .put(&self.path("cached_huge.stl"), &png_bytes(228, 200, MAGENTA))
            .unwrap();

        let gradient = RgbaImage::from_fn(300, 200, |x, y| Rgba([x as u8, y as u8, 128, 255]));
        gradient.save(self.path("pic.png")).unwrap();
        let photo = image::RgbImage::from_fn(640, 480, |x, _| image::Rgb([(x / 3) as u8, 90, 30]));
        photo.save(self.path("photo.JPG")).unwrap();
        write_gif(
            &self.path("anim.gif"),
            &[Rgba([255, 0, 0, 255]), Rgba([0, 0, 255, 255])],
            (64, 48),
        );
        fs::write(self.path("logo.svg"), SVG).unwrap();
        fs::write(self.path("clip.webm"), [0x1a, 0x45, 0xdf, 0xa3, 0, 0, 0]).unwrap();
        fs::write(self.path("broken.png"), b"\x89PNG\r\n\x1a\n garbage").unwrap();
        fs::write(self.path("notes.txt"), b"hello").unwrap();
    }

    /// The folder listing as the app produces it (media shown).
    fn files(&self) -> Vec<PathBuf> {
        stlv_core::files::list_supported_files(self.dir.path(), true).unwrap()
    }
}

#[test]
fn fixture_sizes() {
    let f = Folder::new("sizes");
    f.populate();
    assert!(fs::metadata(f.path("huge.stl")).unwrap().len() > test_limits().max_render_bytes);
    assert!(fs::metadata(f.path("a_cube.stl")).unwrap().len() < test_limits().max_render_bytes);
    assert_eq!(f.files().len(), 13, "{:?}", f.files());
}

// ---------------------------------------------------------------------------------------
// Fake backends: deterministic and independent of the GPU and the real parsers.

#[derive(Default)]
struct Counters {
    loads: AtomicUsize,
    renders: AtomicUsize,
    wakes: AtomicUsize,
    active: AtomicUsize,
    max_active: AtomicUsize,
}

impl Counters {
    fn enter(&self) {
        let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(now, Ordering::SeqCst);
    }

    fn leave(&self) {
        self.loads.fetch_add(1, Ordering::SeqCst);
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

fn fake_load(counters: &Arc<Counters>, delay: Duration) -> Box<LoadFn> {
    let counters = Arc::clone(counters);
    Box::new(move |path: &Path| {
        counters.enter();
        thread::sleep(delay);
        counters.leave();
        let name = file_name(path);
        if name.starts_with("parse-panic") {
            panic!("parser bug on {name}");
        }
        if name.contains("corrupt") {
            return Err("Corrupt Binary STL file: Cannot read triangle count.".into());
        }
        Ok(fake_model(name))
    })
}

fn fake_model(name: String) -> PreparedModel {
    let vertices = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let model = ModelData::new(vertices, vec![[0, 1, 2]], vec![], "fake", name);
    PreparedModel::new(Arc::new(model))
}

/// A loader that hangs on files named `stuck*` (like a parser on a hostile file) until the
/// returned sender is dropped.
fn gated_load(counters: &Arc<Counters>) -> (Box<LoadFn>, Sender<()>) {
    let counters = Arc::clone(counters);
    let (release, gate) = bounded::<()>(0);
    let load = Box::new(move |path: &Path| {
        counters.enter();
        let name = file_name(path);
        if name.starts_with("stuck") {
            let _ = gate.recv();
        }
        counters.leave();
        Ok(fake_model(name))
    });
    (load, release)
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(2));
    }
}

fn fake_render(counters: &Arc<Counters>, delay: Duration) -> Box<RenderFn> {
    let counters = Arc::clone(counters);
    Box::new(move |prepared: &PreparedModel| {
        thread::sleep(delay);
        counters.renders.fetch_add(1, Ordering::SeqCst);
        if prepared.model.filename.contains("render-panic") {
            panic!("renderer bug");
        }
        Ok(RgbaImage::from_pixel(228, 200, CYAN))
    })
}

fn fake_service(
    cache: &Arc<ThumbnailCache>,
    counters: &Arc<Counters>,
    load_delay: Duration,
    render_delay: Option<Duration>,
) -> ThumbnailService {
    let wake_counters = Arc::clone(counters);
    ThumbnailService::with_backend(
        Arc::clone(cache),
        fake_load(counters, load_delay),
        render_delay.map(|d| fake_render(counters, d)),
        Box::new(move || {
            wake_counters.wakes.fetch_add(1, Ordering::SeqCst);
        }),
        test_limits(),
    )
}

/// The fixture's 11 MB `huge.stl` is over this render limit (the app uses 100 MB).
fn test_limits() -> Limits {
    Limits {
        max_render_bytes: 10 * 1024 * 1024,
        ..Limits::default()
    }
}

/// Poll like the UI does until the service is idle (or `timeout`).
fn drain(svc: &mut ThumbnailService, timeout: Duration) -> Vec<ThumbEvent> {
    let deadline = Instant::now() + timeout;
    let mut events = Vec::new();
    loop {
        events.extend(svc.poll());
        if !svc.is_busy() {
            events.extend(svc.poll());
            return events;
        }
        assert!(
            Instant::now() < deadline,
            "service did not settle; got {events:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn ready_map(events: &[ThumbEvent]) -> HashMap<String, &RgbaImage> {
    let mut map = HashMap::new();
    for event in events {
        if let ThumbEvent::Ready { path, image } = event {
            let previous = map.insert(file_name(path), image);
            assert!(previous.is_none(), "duplicate Ready for {}", path.display());
        }
    }
    map
}

fn center(image: &RgbaImage) -> Rgba<u8> {
    *image.get_pixel(image.width() / 2, image.height() / 2)
}

// ---------------------------------------------------------------------------------------
// Pipeline behaviour

#[test]
fn folder_job_emits_ready_progress_and_finished() {
    let f = Folder::new("folder");
    f.populate();
    let counters = Arc::default();
    let mut svc = fake_service(&f.cache, &counters, Duration::ZERO, Some(Duration::ZERO));
    svc.start_folder(f.files());
    let events = drain(&mut svc, Duration::from_secs(60));
    // The mesh count comes before any card.
    assert!(
        matches!(events[0], ThumbEvent::Queued { total: 4 }),
        "{events:?}"
    );
    let queued = events
        .iter()
        .filter(|e| matches!(e, ThumbEvent::Queued { .. }));
    assert_eq!(queued.count(), 1);

    let ready = ready_map(&events);
    let mut names: Vec<&str> = ready.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "a_cube.stl",
            "anim.gif",
            "b_tri.obj",
            "c_path.gcode",
            "cached.3mf",
            "cached_huge.stl",
            "logo.svg",
            "photo.JPG",
            "pic.png"
        ]
    );
    for (name, image) in &ready {
        assert!(
            image.width() <= 220 && image.height() <= 192,
            "{name}: {:?}",
            image.dimensions()
        );
    }
    // Renders and cache hits come out at 2x the card's 110x96 preview.
    assert_eq!(ready["a_cube.stl"].dimensions(), (219, 192));
    assert_eq!(center(ready["a_cube.stl"]), CYAN);
    assert_eq!(ready["cached.3mf"].dimensions(), (219, 192));
    assert_eq!(center(ready["cached.3mf"]), MAGENTA);
    assert_eq!(center(ready["cached_huge.stl"]), MAGENTA);
    // Media keep their aspect ratio and are never upscaled; GIFs show the first frame.
    assert_eq!(ready["pic.png"].dimensions(), (220, 147));
    assert_eq!(ready["photo.JPG"].dimensions(), (220, 165));
    assert_eq!(ready["anim.gif"].dimensions(), (64, 48));
    assert_eq!(center(ready["anim.gif"]), Rgba([255, 0, 0, 255]));
    assert_eq!(ready["logo.svg"].dimensions(), (180, 90));
    assert_eq!(center(ready["logo.svg"]), Rgba([0, 0x66, 0xff, 255]));

    // Progress: uncached meshes <= 10 MB, incl. the corrupt one, in order, then Finished.
    let progress: Vec<(usize, usize, &str)> = events
        .iter()
        .filter_map(|e| match e {
            ThumbEvent::Progress {
                done,
                total,
                current,
            } => Some((*done, *total, current.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        progress.iter().map(|p| p.0).collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    assert!(progress.iter().all(|p| p.1 == 4));
    let mut current: Vec<&str> = progress.iter().map(|p| p.2).collect();
    current.sort_unstable();
    assert_eq!(
        current,
        ["a_cube.stl", "b_tri.obj", "c_path.gcode", "corrupt.stl"]
    );
    let finished: Vec<usize> = events
        .iter()
        .filter_map(|e| match e {
            ThumbEvent::Finished { total } => Some(*total),
            _ => None,
        })
        .collect();
    assert_eq!(finished, [4]);
    let last_progress = events
        .iter()
        .rposition(|e| matches!(e, ThumbEvent::Progress { .. }));
    let finished_at = events
        .iter()
        .position(|e| matches!(e, ThumbEvent::Finished { .. }));
    assert!(last_progress < finished_at);

    // Rendered meshes are cached as 228x200 PNGs; skipped and failed ones are not.
    for name in ["a_cube.stl", "b_tri.obj", "c_path.gcode"] {
        let png = f
            .cache
            .get_png(&f.path(name))
            .unwrap_or_else(|| panic!("{name} not cached"));
        assert_eq!(
            image::load_from_memory(&png).unwrap().dimensions(),
            (228, 200)
        );
    }
    for name in ["huge.stl", "corrupt.stl", "pic.png", "logo.svg"] {
        assert!(!f.cache.is_valid(&f.path(name)), "{name}");
    }
    assert_eq!(counters.loads.load(Ordering::SeqCst), 4);
    assert_eq!(counters.renders.load(Ordering::SeqCst), 3);
    assert!(counters.wakes.load(Ordering::SeqCst) >= events.len());

    // Second visit: everything comes from the cache; only the corrupt file is retried.
    svc.start_folder(f.files());
    let events = drain(&mut svc, Duration::from_secs(60));
    assert_eq!(ready_map(&events).len(), 9);
    let others: Vec<_> = events
        .iter()
        .filter(|e| !matches!(e, ThumbEvent::Ready { .. }))
        .collect();
    assert!(
        matches!(others[..], [
            ThumbEvent::Queued { total: 1 },
            ThumbEvent::Progress { done: 0, total: 1, current },
            ThumbEvent::Finished { total: 1 },
        ] if current == "corrupt.stl"),
        "{others:?}"
    );
    assert_eq!(counters.renders.load(Ordering::SeqCst), 3);
}

#[test]
fn media_cards_do_not_wait_behind_mesh_renders() {
    let f = Folder::new("order");
    for i in 0..12 {
        write_binary_stl(&f.path(&format!("a{i:02}.stl")), &cube_triangles(1.0));
    }
    RgbaImage::from_pixel(64, 64, MAGENTA)
        .save(f.path("z_pic.png"))
        .unwrap();
    let counters = Arc::default();
    let mut svc = fake_service(
        &f.cache,
        &counters,
        Duration::ZERO,
        Some(Duration::from_millis(50)),
    );
    svc.start_folder(f.files());
    let events = drain(&mut svc, Duration::from_secs(60));
    let pic = events
        .iter()
        .position(|e| matches!(e, ThumbEvent::Ready { path, .. } if path.ends_with("z_pic.png")))
        .unwrap();
    let third_render = events
        .iter()
        .position(|e| matches!(e, ThumbEvent::Progress { done: 2, .. }))
        .unwrap();
    assert!(pic < third_render, "{events:?}");
}

#[test]
fn precache_renders_all_sizes_without_progress() {
    let f = Folder::new("precache");
    f.populate();
    let counters = Arc::default();
    let mut svc = fake_service(&f.cache, &counters, Duration::ZERO, Some(Duration::ZERO));
    svc.precache_dir(f.dir.path());
    let events = drain(&mut svc, Duration::from_secs(60));
    let ready = ready_map(&events);
    let mut names: Vec<&str> = ready.keys().map(String::as_str).collect();
    names.sort_unstable();
    // Only uncached meshes, including the one over the folder-grid size limit.
    assert_eq!(
        names,
        ["a_cube.stl", "b_tri.obj", "c_path.gcode", "huge.stl"]
    );
    assert!(events.iter().all(|e| matches!(e, ThumbEvent::Ready { .. })));
    assert!(f.cache.is_valid(&f.path("huge.stl")));

    // Nothing left to do the second time.
    svc.precache_dir(f.dir.path());
    assert!(drain(&mut svc, Duration::from_secs(10)).is_empty());
    // Missing directories are ignored.
    svc.precache_dir(&f.path("no such dir"));
    assert!(drain(&mut svc, Duration::from_secs(10)).is_empty());
}

#[test]
fn without_renderer_only_cached_and_media_are_ready() {
    let f = Folder::new("norender");
    f.populate();
    let counters = Arc::default();
    let mut svc = fake_service(&f.cache, &counters, Duration::ZERO, None);
    svc.start_folder(f.files());
    let events = drain(&mut svc, Duration::from_secs(60));
    assert_eq!(ready_map(&events).len(), 6);
    assert!(matches!(events[0], ThumbEvent::Queued { total: 0 }));
    assert!(
        events[1..]
            .iter()
            .all(|e| matches!(e, ThumbEvent::Ready { .. }))
    );
    assert_eq!(counters.loads.load(Ordering::SeqCst), 0);
    svc.precache_dir(f.dir.path());
    assert!(!svc.is_busy());
}

#[test]
fn cancelled_folder_job_stops_promptly_and_stays_silent() {
    let f = Folder::new("cancel");
    for i in 0..40 {
        write_binary_stl(&f.path(&format!("m{i:02}.stl")), &cube_triangles(1.0));
    }
    let counters: Arc<Counters> = Arc::default();
    let mut svc = fake_service(
        &f.cache,
        &counters,
        Duration::ZERO,
        Some(Duration::from_millis(30)),
    );
    svc.start_folder(f.files());
    let deadline = Instant::now() + Duration::from_secs(20);
    while !svc
        .poll()
        .iter()
        .any(|e| matches!(e, ThumbEvent::Progress { .. }))
    {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    svc.cancel_folder();
    let renders_at_cancel = counters.renders.load(Ordering::SeqCst);
    thread::sleep(Duration::from_millis(300));
    assert!(
        svc.poll().is_empty(),
        "events from a cancelled job were delivered"
    );
    assert!(!svc.is_busy());
    // At most the render in flight finishes after cancelling.
    assert!(counters.renders.load(Ordering::SeqCst) <= renders_at_cancel + 1);
    assert!(
        svc.retired.iter().all(|t| t.is_finished()),
        "cancelled job threads still running"
    );
}

#[test]
fn cancelled_job_still_stores_what_it_rendered() {
    let f = Folder::new("cancel-store");
    for i in 0..40 {
        write_binary_stl(&f.path(&format!("m{i:02}.stl")), &cube_triangles(1.0));
    }
    let counters: Arc<Counters> = Arc::default();
    let mut svc = fake_service(
        &f.cache,
        &counters,
        Duration::ZERO,
        Some(Duration::from_millis(5)),
    );
    svc.start_folder(f.files());
    let mut shown = Vec::new();
    wait_until("a few thumbnails", || {
        for event in svc.poll() {
            if let ThumbEvent::Ready { path, .. } = event {
                shown.push(path);
            }
        }
        shown.len() >= 5
    });
    svc.cancel_folder();
    wait_until("the job to stop", || {
        svc.retired.iter().all(|t| t.is_finished())
    });
    // Every card the grid showed has its thumbnail written, though no batch was full.
    for path in &shown {
        assert!(f.cache.is_valid(path), "{} not cached", path.display());
    }
}

#[test]
fn precache_wakes_the_ui_sparingly() {
    let f = Folder::new("wakes");
    for i in 0..40 {
        write_binary_stl(&f.path(&format!("m{i:02}.stl")), &cube_triangles(1.0));
    }
    let counters: Arc<Counters> = Arc::default();
    let mut svc = fake_service(
        &f.cache,
        &counters,
        Duration::ZERO,
        Some(Duration::from_millis(5)),
    );
    svc.precache_dir(f.dir.path());
    // Poll only when woken, like an idle UI: the last thumbnails must still get a wake-up.
    let (mut seen_wakes, mut ready) = (0, 0);
    wait_until("all pre-cached thumbnails", || {
        let wakes = counters.wakes.load(Ordering::SeqCst);
        if wakes != seen_wakes {
            seen_wakes = wakes;
            ready += svc.poll().len();
        }
        ready == 40
    });
    assert!(
        seen_wakes < 10,
        "{seen_wakes} wake-ups for 40 background thumbnails"
    );
}

#[test]
fn restarted_folder_job_supersedes_the_old_one() {
    let a = Folder::new("restart-a");
    let b = Folder::new("restart-b");
    for i in 0..20 {
        write_binary_stl(&a.path(&format!("a{i:02}.stl")), &cube_triangles(1.0));
    }
    for i in 0..3 {
        write_binary_stl(&b.path(&format!("b{i}.stl")), &cube_triangles(1.0));
    }
    let counters = Arc::default();
    let mut svc = fake_service(
        &a.cache,
        &counters,
        Duration::from_millis(5),
        Some(Duration::from_millis(5)),
    );
    svc.start_folder(a.files());
    thread::sleep(Duration::from_millis(40));
    svc.start_folder(b.files());
    let events = drain(&mut svc, Duration::from_secs(30));
    for event in &events {
        match event {
            ThumbEvent::Ready { path, .. } => assert!(file_name(path).starts_with('b'), "{path:?}"),
            ThumbEvent::Queued { total }
            | ThumbEvent::Progress { total, .. }
            | ThumbEvent::Finished { total } => {
                assert_eq!(*total, 3)
            }
        }
    }
    assert_eq!(ready_map(&events).len(), 3);
}

#[test]
fn precache_survives_folder_jobs_and_cancels_separately() {
    let pre = Folder::new("pre-indep");
    let grid = Folder::new("grid-indep");
    for i in 0..6 {
        write_binary_stl(&pre.path(&format!("p{i}.stl")), &cube_triangles(1.0));
    }
    write_binary_stl(&grid.path("g.stl"), &cube_triangles(1.0));
    let counters = Arc::default();
    let mut svc = fake_service(
        &pre.cache,
        &counters,
        Duration::ZERO,
        Some(Duration::from_millis(10)),
    );
    svc.precache_dir(pre.dir.path());
    svc.start_folder(grid.files());
    svc.cancel_folder();
    let events = drain(&mut svc, Duration::from_secs(30));
    let ready = ready_map(&events);
    assert_eq!(ready.len(), 6, "{:?}", ready.keys());
    assert!(ready.keys().all(|k| k.starts_with('p')));

    for i in 0..6 {
        write_binary_stl(&pre.path(&format!("q{i}.stl")), &cube_triangles(2.0));
    }
    svc.precache_dir(pre.dir.path());
    svc.cancel_precache();
    thread::sleep(Duration::from_millis(100));
    assert!(svc.poll().is_empty());
}

#[test]
fn parser_and_renderer_panics_are_contained() {
    let f = Folder::new("panics");
    write_binary_stl(&f.path("parse-panic.stl"), &cube_triangles(1.0));
    write_binary_stl(&f.path("render-panic.stl"), &cube_triangles(1.0));
    write_binary_stl(&f.path("fine.stl"), &cube_triangles(1.0));
    let counters = Arc::default();
    let mut svc = fake_service(&f.cache, &counters, Duration::ZERO, Some(Duration::ZERO));
    svc.start_folder(f.files());
    let events = drain(&mut svc, Duration::from_secs(30));
    let ready = ready_map(&events);
    assert_eq!(ready.keys().collect::<Vec<_>>(), ["fine.stl"]);
    assert!(matches!(
        events.last(),
        Some(ThumbEvent::Finished { total: 3 })
    ));
}

#[test]
fn drop_joins_every_thread_even_when_nobody_polls() {
    let f = Folder::new("drop");
    // More events than the queue holds, so workers end up waiting on a full queue.
    let tiny = png_bytes(4, 4, CYAN);
    for i in 0..EVENT_QUEUE_LEN + 100 {
        fs::write(f.path(&format!("img{i:04}.png")), &tiny).unwrap();
    }
    for i in 0..10 {
        write_binary_stl(&f.path(&format!("slow{i}.stl")), &cube_triangles(1.0));
    }
    let counters: Arc<Counters> = Arc::default();
    let mut svc = fake_service(
        &f.cache,
        &counters,
        Duration::from_millis(50),
        Some(Duration::ZERO),
    );
    svc.start_folder(f.files());
    svc.precache_dir(f.dir.path());
    let deadline = Instant::now() + Duration::from_secs(30);
    while svc.events.len() < EVENT_QUEUE_LEN {
        assert!(Instant::now() < deadline, "event queue never filled");
        thread::sleep(Duration::from_millis(5));
    }
    let started = Instant::now();
    drop(svc);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "drop took {:?}",
        started.elapsed()
    );
    assert_eq!(counters.active.load(Ordering::SeqCst), 0);
    // Every job thread held the cache through `Shared`; all are gone now.
    assert_eq!(Arc::strong_count(&f.cache), 1);
}

#[test]
fn drop_does_not_wait_for_a_stuck_parse() {
    let f = Folder::new("stuck");
    write_binary_stl(&f.path("stuck.stl"), &cube_triangles(1.0));
    let counters: Arc<Counters> = Arc::default();
    let (load, release) = gated_load(&counters);
    let mut svc = ThumbnailService::with_backend(
        Arc::clone(&f.cache),
        load,
        Some(fake_render(&counters, Duration::ZERO)),
        Box::new(|| {}),
        Limits::default(),
    );
    svc.precache_dir(f.dir.path());
    wait_until("the parse to start", || {
        counters.active.load(Ordering::SeqCst) == 1
    });
    let started = Instant::now();
    drop(svc);
    let took = started.elapsed();
    assert!(
        took < DROP_GRACE + Duration::from_secs(1),
        "drop took {took:?}"
    );
    // The detached parse ends later without rendering or caching anything.
    drop(release);
    wait_until("the detached job to end", || {
        Arc::strong_count(&f.cache) == 1
    });
    assert_eq!(counters.loads.load(Ordering::SeqCst), 1);
    assert_eq!(counters.renders.load(Ordering::SeqCst), 0);
    assert!(!f.cache.is_valid(&f.path("stuck.stl")));
}

#[test]
fn cancelled_parses_hold_back_the_next_job() {
    let a = Folder::new("budget-a");
    let b = Folder::new("budget-b");
    write_binary_stl(&a.path("stuck.stl"), &cube_triangles(1.0));
    write_binary_stl(&b.path("next.stl"), &cube_triangles(1.0));
    let size = fs::metadata(a.path("stuck.stl")).unwrap().len();
    let counters: Arc<Counters> = Arc::default();
    let (load, release) = gated_load(&counters);
    // Room for one file at a time.
    let limits = Limits {
        parse_bytes: size * 3 / 2,
        ..Limits::default()
    };
    let mut svc = ThumbnailService::with_backend(
        Arc::clone(&a.cache),
        load,
        Some(fake_render(&counters, Duration::ZERO)),
        Box::new(|| {}),
        limits,
    );
    svc.precache_dir(a.dir.path());
    wait_until("the parse to start", || {
        counters.active.load(Ordering::SeqCst) == 1
    });
    // Like navigating away mid-parse: the old job is cancelled, its parse keeps running.
    svc.precache_dir(b.dir.path());
    thread::sleep(Duration::from_millis(200));
    assert_eq!(counters.loads.load(Ordering::SeqCst), 0);
    assert_eq!(counters.active.load(Ordering::SeqCst), 1);

    drop(release);
    let events = drain(&mut svc, Duration::from_secs(30));
    assert_eq!(ready_map(&events).keys().collect::<Vec<_>>(), ["next.stl"]);
    assert_eq!(counters.loads.load(Ordering::SeqCst), 2);
    assert_eq!(counters.max_active.load(Ordering::SeqCst), 1);
    // The cancelled job's result was dropped, not rendered.
    assert_eq!(counters.renders.load(Ordering::SeqCst), 1);
    assert!(!a.cache.is_valid(&a.path("stuck.stl")));
    assert!(a.cache.is_valid(&b.path("next.stl")));
}

#[test]
fn fit_within_scales_down_only() {
    let fit = |w, h| fit_within(DynamicImage::new_rgba8(w, h), READY_FIT).dimensions();
    assert_eq!(fit(228, 200), (219, 192));
    assert_eq!(fit(220, 192), (220, 192));
    assert_eq!(fit(50, 40), (50, 40));
    assert_eq!(fit(10_000, 10), (220, 1));
    assert_eq!(fit(10, 10_000), (1, 192));
    assert_eq!(fit(4000, 3000), (220, 165));
    let big = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        3000,
        2000,
        image::Rgb([9, 99, 199]),
    ));
    let small = fit_within(big, READY_FIT);
    assert_eq!(small.dimensions(), (220, 147));
    assert_eq!(center(&small), Rgba([9, 99, 199, 255]));
}

#[test]
fn hostile_svgs_are_refused_on_worker_threads() {
    let f = Folder::new("svgs");
    const SVG_OPEN: &str = "<svg xmlns='http://www.w3.org/2000/svg' width='100' height='100'>";
    let deep = format!(
        "{SVG_OPEN}{}<rect width='9' height='9'/>{}</svg>",
        "<g>".repeat(5000),
        "</g>".repeat(5000)
    );
    fs::write(f.path("deep.svg"), deep).unwrap();
    RgbaImage::from_pixel(8, 8, Rgba([255, 0, 0, 255]))
        .save(f.path("red.png"))
        .unwrap();
    let linked =
        |href: &str| format!("{SVG_OPEN}<image href='{href}' width='100' height='100'/></svg>");
    fs::write(f.path("linked.svg"), linked("red.png")).unwrap();
    fs::write(f.path("unc.svg"), linked(r"\127.0.0.1\c$\red.png")).unwrap();
    let text = "<svg width='100' height='40' xmlns='http://www.w3.org/2000/svg'>\
                <text x='5' y='30' font-size='30'>ABC</text></svg>";
    fs::write(f.path("plain.svg"), text).unwrap();
    let prefixed = text
        .replace('<', "<svg:")
        .replace("<svg:/", "</svg:")
        .replace("xmlns=", "xmlns:svg=");
    fs::write(f.path("prefixed.svg"), prefixed).unwrap();

    let counters = Arc::default();
    let mut svc = fake_service(&f.cache, &counters, Duration::ZERO, None);
    svc.start_folder(f.files());
    let events = drain(&mut svc, Duration::from_secs(60));
    let ready = ready_map(&events);
    assert!(!ready.contains_key("deep.svg"));
    // Relative hrefs resolve against the SVG's folder; UNC paths are never opened.
    assert_eq!(center(ready["linked.svg"]), Rgba([255, 0, 0, 255]));
    assert_eq!(center(ready["unc.svg"])[3], 0);
    // Namespace-prefixed text is drawn like plain text.
    assert!(ready["plain.svg"].pixels().any(|p| p[3] > 0));
    assert_eq!(ready["prefixed.svg"], ready["plain.svg"]);
}

#[test]
fn bad_media_yield_errors_not_panics() {
    let dir = TempDir::new("badmedia");
    let svg = dir.path().join("bad.svg");
    fs::write(&svg, "<svg width='0' height='0'").unwrap();
    assert!(rasterize_svg(&svg).is_err());
    let empty = dir.path().join("empty.svg");
    fs::write(
        &empty,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="0" height="10"/>"#,
    )
    .unwrap();
    assert!(rasterize_svg(&empty).is_err());
    let budget = Budget::new(DECODE_BYTES);
    let go = AtomicBool::new(false);
    let decode = |path: &Path| decode_raster(path, &budget, &go);
    let gif = dir.path().join("trunc.gif");
    fs::write(&gif, b"GIF89a\x10\x00").unwrap();
    assert!(decode(&gif).is_err());
    assert!(decode(&dir.path().join("missing.jpg")).is_err());
    // An image with a lying extension is sniffed by content.
    let lying = dir.path().join("really_png.jpg");
    fs::write(&lying, png_bytes(8, 8, MAGENTA)).unwrap();
    assert_eq!(center(&decode(&lying).unwrap()), MAGENTA);
}

/// Only the headers of a 24-bit BMP claiming `w` x `h` pixels.
fn bmp_header(w: i32, h: i32) -> Vec<u8> {
    let mut bmp = b"BM".to_vec();
    bmp.extend_from_slice(&54u32.to_le_bytes()); // file size (a lie)
    bmp.extend_from_slice(&[0; 4]);
    bmp.extend_from_slice(&54u32.to_le_bytes()); // pixel data offset
    bmp.extend_from_slice(&40u32.to_le_bytes()); // BITMAPINFOHEADER
    bmp.extend_from_slice(&w.to_le_bytes());
    bmp.extend_from_slice(&h.to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes()); // planes
    bmp.extend_from_slice(&24u16.to_le_bytes()); // bits per pixel
    bmp.extend_from_slice(&[0; 24]); // uncompressed, no palette
    bmp
}

#[test]
fn raster_decodes_wait_for_their_memory_budget() {
    let dir = TempDir::new("decode-budget");
    let pic = dir.path().join("pic.png");
    RgbaImage::from_pixel(100, 100, MAGENTA).save(&pic).unwrap();
    let budget = Budget::new(50_000);
    let (go, stop) = (AtomicBool::new(false), AtomicBool::new(true));
    // Its 40,000 decoded bytes do not fit next to 20,000 held by another decode.
    let held = budget.acquire(20_000, &go).unwrap();
    thread::scope(|s| {
        let decode = s.spawn(|| decode_raster(&pic, &budget, &go));
        thread::sleep(Duration::from_millis(100));
        assert!(!decode.is_finished(), "decoded beyond the budget");
        drop(held);
        assert_eq!(center(&decode.join().unwrap().unwrap()), MAGENTA);
    });
    let _held = budget.acquire(20_000, &go).unwrap();
    assert!(
        decode_raster(&pic, &budget, &stop).is_err(),
        "a cancelled job kept waiting"
    );
    // A header claiming a 2.7 GB bitmap is refused before anything is allocated.
    let bomb = dir.path().join("bomb.bmp");
    fs::write(&bomb, bmp_header(30_000, 30_000)).unwrap();
    let err = decode_raster(&bomb, &budget, &go).unwrap_err();
    assert!(err.contains("limit"), "{err}");
}

#[test]
fn real_parsers_feed_the_render_thread() {
    let f = Folder::new("parsers");
    f.populate();
    // (file, vertices, triangles, lines) as the render thread sees them.
    type Seen = Vec<(String, usize, usize, usize)>;
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let record = Arc::clone(&seen);
    let render = move |p: &PreparedModel| {
        let m = &p.model;
        let row = (
            m.filename.clone(),
            m.num_vertices(),
            m.num_triangles(),
            m.num_lines(),
        );
        record.lock().unwrap().push(row);
        Ok(RgbaImage::from_pixel(228, 200, CYAN))
    };
    let mut svc = ThumbnailService::with_backend(
        Arc::clone(&f.cache),
        Box::new(load_and_prepare),
        Some(Box::new(render)),
        Box::new(|| {}),
        test_limits(),
    );
    svc.start_folder(f.files());
    let events = drain(&mut svc, Duration::from_secs(60));
    let mut seen = seen.lock().unwrap().clone();
    seen.sort();
    assert_eq!(
        seen,
        [
            ("a_cube.stl".to_string(), 8, 12, 0),
            ("b_tri.obj".to_string(), 4, 4, 0),
            ("c_path.gcode".to_string(), 10, 0, 5),
        ]
    );
    assert_eq!(ready_map(&events).len(), 9);
    let finished = events
        .iter()
        .filter(|e| matches!(e, ThumbEvent::Finished { total: 4 }));
    assert_eq!(finished.count(), 1);
    assert!(!f.cache.is_valid(&f.path("corrupt.stl")));
}

// ---------------------------------------------------------------------------------------
// Real parsers + GPU renderer

#[test]
fn real_backend_renders_meshes() {
    let renderer = match ThumbnailRenderer::new_headless() {
        Ok(r) => Arc::new(r),
        Err(e) => {
            eprintln!("skipping: no headless GPU renderer ({e})");
            return;
        }
    };
    let f = Folder::new("real");
    write_binary_stl(&f.path("cube.stl"), &cube_triangles(10.0));
    fs::write(
        f.path("tetra.obj"),
        "v 0 0 0\nv 10 0 0\nv 0 10 0\nv 0 0 10\nf 1 3 2\nf 1 2 4\nf 1 4 3\nf 2 3 4\n",
    )
    .unwrap();
    fs::write(
        f.path("square.gcode"),
        "G1 X0 Y0 Z0.2 E1\nG1 X20 Y0 E2\nG1 X20 Y20 E3\nG1 X0 Y20 E4\nG1 X0 Y0 E5\n",
    )
    .unwrap();
    fs::write(f.path("corrupt.stl"), b"short").unwrap();
    let mut svc = ThumbnailService::new(Arc::clone(&f.cache), Some(renderer), || {});
    svc.start_folder(f.files());
    let events = drain(&mut svc, Duration::from_secs(120));
    let ready = ready_map(&events);
    let mut names: Vec<&str> = ready.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(names, ["cube.stl", "square.gcode", "tetra.obj"]);
    assert!(matches!(
        events.last(),
        Some(ThumbEvent::Finished { total: 4 })
    ));
    for (name, image) in &ready {
        // Not a flat fill: the model is drawn over the gradient background.
        let distinct: std::collections::HashSet<_> = image.pixels().map(|p| p.0).collect();
        assert!(distinct.len() > 20, "{name} looks blank");
        assert_eq!(image.dimensions(), (219, 192), "{name}");
        // Background in the corners: dark blue-grey, brighter at the top.
        let [tr, tg, tb, _] = image.get_pixel(1, 1).0;
        let [br, bg, bb, _] = image.get_pixel(1, image.height() - 2).0;
        assert!(
            tb > tr && tb > bb && bb > br,
            "{name}: corners {tr},{tg},{tb} / {br},{bg},{bb}"
        );
        let png = f.cache.get_png(&f.path(name)).unwrap();
        assert_eq!(
            image::load_from_memory(&png).unwrap().dimensions(),
            (228, 200)
        );
    }
    // The cube fills the middle of its thumbnail in the cyan model color.
    let [r, g, b, _] = center(ready["cube.stl"]).0;
    assert!(r < 40 && g > 100 && b > 100, "cube center {r},{g},{b}");
}

#[test]
fn large_meshes_render_after_small_ones() {
    let f = Folder::new("large-last");
    f.populate();
    let counters = Arc::new(Counters::default());
    let limits = Limits {
        max_render_bytes: 100 * 1024 * 1024,
        large_file_bytes: 1024 * 1024,
        ..Limits::default()
    };
    let mut svc = ThumbnailService::with_backend(
        Arc::clone(&f.cache),
        fake_load(&counters, Duration::from_millis(5)),
        Some(fake_render(&counters, Duration::from_millis(5))),
        Box::new(|| {}),
        limits,
    );
    svc.start_folder(f.files());
    let events = drain(&mut svc, Duration::from_secs(60));
    let order: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            ThumbEvent::Progress { current, .. } => Some(current.as_str()),
            _ => None,
        })
        .collect();
    // huge.stl (11 MB) sorts between the small meshes by name but renders last.
    assert_eq!(order.last(), Some(&"huge.stl"), "{order:?}");
    assert!(order.len() > 1, "{order:?}");
}
