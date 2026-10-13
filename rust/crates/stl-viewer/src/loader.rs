//! Background model loading (app.py `ModelLoaderWorker`): `load_model` + `PreparedModel::new`
//! on a worker thread. Python terminated superseded threads; here every request gets a
//! generation number and results of older generations are dropped (a superseded worker also
//! skips the costly preparation step).
//!
//! An opened model without a fresh cached thumbnail gets one rendered and saved afterwards
//! on the same worker, so its folder card shows a preview from then on (even for files
//! too large for the grid to render).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use stlv_core::ModelData;
use stlv_render::{PreparedModel, ThumbnailRenderer};
use stlv_thumbs::ThumbnailCache;

pub struct LoadOutcome {
    pub path: PathBuf,
    /// The user-facing error text on failure (`LoadError`'s Display).
    pub result: Result<PreparedModel, String>,
}

type Wake = Arc<dyn Fn() + Send + Sync>;

/// Renders and caches the thumbnail of an opened model.
#[derive(Clone)]
pub struct ThumbSaver {
    pub renderer: Arc<ThumbnailRenderer>,
    pub cache: Arc<ThumbnailCache>,
}

impl ThumbSaver {
    /// Save a thumbnail for `path` unless the cache already has a fresh one.
    fn save(&self, path: &std::path::Path, model: &ModelData) {
        if self.cache.is_valid(path) {
            return;
        }
        let png = self.renderer.render(model).and_then(|img| {
            let mut png = Vec::new();
            img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .map_err(|e| e.to_string())?;
            Ok(png)
        });
        match png {
            Ok(png) => {
                if let Err(e) = self.cache.put(path, &png) {
                    log::warn!("cannot cache thumbnail of {}: {e}", path.display());
                }
            }
            Err(e) => log::warn!("cannot render thumbnail of {}: {e}", path.display()),
        }
    }
}

pub struct ModelLoader {
    current: Arc<AtomicU64>,
    tx: Sender<(u64, LoadOutcome)>,
    rx: Receiver<(u64, LoadOutcome)>,
    wake: Wake,
    in_flight: Option<PathBuf>,
    thumbs: Option<ThumbSaver>,
}

impl ModelLoader {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (tx, rx) = channel();
        Self {
            current: Arc::new(AtomicU64::new(0)),
            tx,
            rx,
            wake: Arc::new(wake),
            in_flight: None,
            thumbs: None,
        }
    }

    /// Also cache a thumbnail for every model opened without one.
    pub fn with_thumbnails(mut self, saver: ThumbSaver) -> Self {
        self.thumbs = Some(saver);
        self
    }

    /// Start loading `path`, superseding any load in progress.
    pub fn load(&mut self, path: PathBuf) {
        let generation = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.in_flight = Some(path.clone());
        let current = Arc::clone(&self.current);
        let tx = self.tx.clone();
        let wake = Arc::clone(&self.wake);
        let job_path = path.clone();
        let thumbs = self.thumbs.clone();
        let spawned = std::thread::Builder::new()
            .name("model-loader".into())
            .spawn(move || {
                let stale = || current.load(Ordering::SeqCst) != generation;
                let mut model_for_thumb = None;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let model = stlv_core::load_model(&job_path).map_err(|e| e.to_string())?;
                    let model = Arc::new(model);
                    model_for_thumb = Some(Arc::clone(&model));
                    if stale() {
                        return Err(String::new());
                    }
                    Ok(PreparedModel::new(model))
                }))
                .unwrap_or_else(|_| Err("Internal error while reading the file.".into()));
                if !stale()
                    && tx
                        .send((
                            generation,
                            LoadOutcome {
                                path: job_path.clone(),
                                result,
                            },
                        ))
                        .is_ok()
                {
                    wake();
                }
                // After the model is on screen, so it costs no load time. A superseded load
                // still saves its thumbnail: the file was parsed anyway.
                if let (Some(saver), Some(model)) = (thumbs, model_for_thumb) {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        saver.save(&job_path, &model)
                    }));
                }
            });
        if let Err(e) = spawned {
            let outcome = LoadOutcome {
                path,
                result: Err(format!("Cannot start loader thread: {e}")),
            };
            let _ = self.tx.send((generation, outcome));
            (self.wake)();
        }
    }

    /// Drop the current request (its result will be ignored).
    pub fn cancel(&mut self) {
        self.current.fetch_add(1, Ordering::SeqCst);
        self.in_flight = None;
    }

    pub fn is_loading(&self) -> bool {
        self.in_flight.is_some()
    }

    /// The finished result of the current request, if any.
    pub fn poll(&mut self) -> Option<LoadOutcome> {
        let current = self.current.load(Ordering::SeqCst);
        while let Ok((generation, outcome)) = self.rx.try_recv() {
            if generation == current && self.in_flight.is_some() {
                self.in_flight = None;
                return Some(outcome);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;
    use std::time::{Duration, Instant};

    fn wait(loader: &mut ModelLoader) -> Option<LoadOutcome> {
        // Generous: a parallel GPU test can hog the machine while shaders compile.
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if let Some(o) = loader.poll() {
                return Some(o);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    #[test]
    fn errors_are_reported_and_stale_results_dropped() {
        let mut loader = ModelLoader::new(|| {});
        let tmp = TempDir::new("loader");
        let dir = tmp.path();
        let bad = dir.join("bad.xyz");
        std::fs::write(&bad, b"nope").unwrap();

        loader.load(bad.clone());
        assert!(loader.is_loading());
        let out = wait(&mut loader).expect("result");
        assert_eq!(out.path, bad);
        assert!(out.result.is_err());
        assert!(!loader.is_loading());

        // A superseded request never surfaces; only the newest one does.
        let missing = dir.join("missing.stl");
        loader.load(bad.clone());
        loader.load(missing.clone());
        let out = wait(&mut loader).expect("result");
        assert_eq!(out.path, missing);

        // After a cancel, the next request's result is delivered, never the cancelled one.
        loader.load(bad);
        loader.cancel();
        assert!(!loader.is_loading());
        loader.load(missing.clone());
        let out = wait(&mut loader).expect("result");
        assert_eq!(out.path, missing);
        assert!(loader.poll().is_none());
    }

    #[test]
    fn opened_models_get_a_cached_thumbnail() {
        let Ok(renderer) = ThumbnailRenderer::new_headless() else {
            eprintln!("skipping: no GPU adapter");
            return;
        };
        let cache = Arc::new(ThumbnailCache::open(std::path::Path::new(":memory:")).unwrap());
        let saver = ThumbSaver {
            renderer: Arc::new(renderer),
            cache: Arc::clone(&cache),
        };
        let mut loader = ModelLoader::new(|| {}).with_thumbnails(saver);
        let tmp = TempDir::new("loader-thumb");
        let obj = tmp.path().join("t.obj");
        std::fs::write(
            &obj,
            "v 0 0 0
v 1 0 0
v 0 1 0
v 0 0 1
f 1 2 3
f 1 2 4
f 1 3 4
f 2 3 4
",
        )
        .unwrap();
        loader.load(obj.clone());
        assert!(wait(&mut loader).expect("result").result.is_ok());
        // Saved on the worker after the result was delivered.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !cache.is_valid(&obj) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let png = cache.get_png(&obj).expect("thumbnail cached");
        assert_eq!(image::load_from_memory(&png).unwrap().width(), 228);
    }
}
