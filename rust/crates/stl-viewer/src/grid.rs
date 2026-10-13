//! Folder thumbnail grid (app.py `ThumbnailGridWidget` / `ThumbnailCard`), virtualized so
//! folders with thousands of files stay smooth.
//!
//! `Ready` images from the thumbnail service are shrunk to the on-screen card size on a
//! helper thread; GPU textures are created only for visible cards and evicted again when
//! many cards have scrolled away. Decoded images are kept within a memory budget: past it,
//! only cards near the viewport keep theirs, and the others are fetched again (cached PNG
//! or media decode, never a render) by a second, render-less service when they scroll back.

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant, SystemTime};

use egui::{
    Align2, ColorImage, CornerRadius, CursorIcon, Id, Rect, ScrollArea, Sense, Stroke, StrokeKind,
    TextureHandle, TextureOptions, Ui, Vec2, pos2, vec2,
};
use stlv_core::FileKind;
use stlv_thumbs::{ThumbEvent, ThumbnailService};

use crate::card_info::{self, InfoCache};
use crate::{emoji, fonts, icons, text, theme, widgets};

const CARD: Vec2 = vec2(130.0, 150.0);
const SPACING: f32 = 15.0;
const MARGIN: f32 = 15.0;
const CARD_PAD: f32 = 8.0;
const WELL: Vec2 = vec2(114.0, 100.0);
/// The preview image is fit into this box (aspect kept).
pub const IMAGE_BOX: Vec2 = vec2(110.0, 96.0);
const NAME_WIDTH: f32 = 110.0;
pub const PROGRESS_H: f32 = 18.0;
/// The "Sort by" bar above the cards.
const SORT_BAR_H: f32 = 36.0;
const SORT_LABEL: &str = "Sort by:";
/// Keep at most this many card textures alive; invisible ones beyond it are released.
const MAX_TEXTURES: usize = 400;
/// Decoded card images kept in memory (about 1,500 cards at 100 % scaling, 400 at 200 %).
const IMAGE_BUDGET: usize = 64 << 20;
/// Service images waiting to be shrunk; beyond this, far-away cards are fetched later.
const PREP_BUDGET: usize = 16 << 20;
/// Cards this many screens above and below the visible ones count as near the viewport.
const NEAR_SCREENS: usize = 1;
/// Assumed visible card count before the first frame.
const DEFAULT_VISIBLE: usize = 60;
/// Minimum time between restarts of the reload job while scrolling.
const RELOAD_THROTTLE: Duration = Duration::from_millis(150);

/// [`stlv_core::files::list_supported_files`] with each file's size. The sizes come from the
/// directory listing itself, which on Windows costs nothing (a `metadata` call per file took
/// ~250 ms for 5,000 files).
pub fn list_folder(dir: &Path, show_media: bool) -> io::Result<Vec<FileEntry>> {
    let mut files: Vec<FileEntry> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter_map(|e| {
            let path = e.path();
            if !stlv_core::files::is_supported(&path, show_media) {
                return None;
            }
            // Not following symlinks, like the core listing's `file_type().is_file()`.
            let meta = e.metadata().ok()?;
            meta.is_file().then(|| FileEntry {
                path,
                size: meta.len(),
                modified: meta.modified().ok(),
            })
        })
        .collect();
    files.sort_by_cached_key(|f| stlv_core::files::sort_key(&f.path));
    Ok(files)
}

/// A file listed in the grid with what the sort orders need.
#[derive(Clone, Debug)]
pub struct FileEntry {
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

/// Order of the grid's cards (the "Sort by" dropdown).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortOrder {
    #[default]
    NameAsc,
    NameDesc,
    SizeAsc,
    SizeDesc,
    DateNewest,
    DateOldest,
}

impl SortOrder {
    pub const ALL: [Self; 6] = [
        Self::NameAsc,
        Self::NameDesc,
        Self::SizeAsc,
        Self::SizeDesc,
        Self::DateNewest,
        Self::DateOldest,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::NameAsc => "Name (A \u{2192} Z)",
            Self::NameDesc => "Name (Z \u{2192} A)",
            Self::SizeAsc => "Size (smallest first)",
            Self::SizeDesc => "Size (largest first)",
            Self::DateNewest => "Date (newest first)",
            Self::DateOldest => "Date (oldest first)",
        }
    }

    /// Stable name for the settings file.
    pub fn key(self) -> &'static str {
        match self {
            Self::NameAsc => "name_asc",
            Self::NameDesc => "name_desc",
            Self::SizeAsc => "size_asc",
            Self::SizeDesc => "size_desc",
            Self::DateNewest => "date_newest",
            Self::DateOldest => "date_oldest",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|o| o.key() == key)
    }

    /// Ordering of two files; ties fall back to the name (A to Z).
    fn compare(self, a: &SortKey, b: &SortKey) -> std::cmp::Ordering {
        let primary = match self {
            Self::NameAsc => a.name.cmp(&b.name),
            Self::NameDesc => b.name.cmp(&a.name),
            Self::SizeAsc => a.size.cmp(&b.size),
            Self::SizeDesc => b.size.cmp(&a.size),
            Self::DateNewest => b.modified.cmp(&a.modified),
            Self::DateOldest => a.modified.cmp(&b.modified),
        };
        primary.then_with(|| a.name.cmp(&b.name))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SortKey {
    /// [`stlv_core::files::sort_key`] (case-insensitive name).
    name: String,
    size: u64,
    modified: Option<SystemTime>,
}

impl SortKey {
    fn of(entry: &FileEntry) -> Self {
        Self {
            name: stlv_core::files::sort_key(&entry.path),
            size: entry.size,
            modified: entry.modified,
        }
    }
}

/// Card geometry; "large" doubles the preview (the card grows by the extra preview size).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    card: Vec2,
    well: Vec2,
    image: Vec2,
    name_width: f32,
    icon: f32,
}

impl Metrics {
    pub const SMALL: Self = Self::scaled(1.0);
    pub const LARGE: Self = Self::scaled(2.0);

    const fn scaled(s: f32) -> Self {
        let grow = vec2(WELL.x * (s - 1.0), WELL.y * (s - 1.0));
        Self {
            card: vec2(CARD.x + grow.x, CARD.y + grow.y),
            well: vec2(WELL.x * s, WELL.y * s),
            image: vec2(IMAGE_BOX.x * s, IMAGE_BOX.y * s),
            name_width: NAME_WIDTH + grow.x,
            icon: PLACEHOLDER_ICON * s,
        }
    }

    /// Number of columns for a grid of the given inner width (Python: `max(1, width // 145)`).
    pub fn columns(&self, width: f32) -> usize {
        ((width / (self.card.x + SPACING)).floor() as usize).max(1)
    }

    /// Top-left of card `index` relative to the grid's content origin.
    pub fn card_offset(&self, index: usize, cols: usize) -> Vec2 {
        let (row, col) = (index / cols, index % cols);
        vec2(
            MARGIN + col as f32 * (self.card.x + SPACING),
            MARGIN + row as f32 * (self.card.y + SPACING),
        )
    }

    pub fn content_height(&self, n: usize, cols: usize) -> f32 {
        if n == 0 {
            return 0.0;
        }
        let rows = n.div_ceil(cols);
        2.0 * MARGIN + rows as f32 * (self.card.y + SPACING) - SPACING
    }
}

/// Fit `size` into `bounds` keeping the aspect ratio (may scale up, like `QPixmap::scaled`).
pub fn fit(size: Vec2, bounds: Vec2) -> Vec2 {
    if size.x <= 0.0 || size.y <= 0.0 {
        return Vec2::ZERO;
    }
    let s = (bounds.x / size.x).min(bounds.y / size.y);
    size * s
}

/// A card's preview image.
#[derive(Clone, Default)]
enum Thumb {
    /// No image (yet): the placeholder is shown.
    #[default]
    None,
    /// The service image is being shrunk on the prep thread.
    Preparing,
    Ready(Arc<ColorImage>),
    /// Dropped to stay within the memory budget; fetched again once near the viewport.
    Evicted,
    /// Requested again from the reload service.
    Reloading,
}

struct Card {
    path: PathBuf,
    name: String,
    kind: FileKind,
    elided: Option<String>,
    thumb: Thumb,
    texture: Option<TextureHandle>,
    last_seen: u64,
    key: SortKey,
}

struct PrepJob {
    generation: u64,
    path: PathBuf,
    image: image::RgbaImage,
    max_px: [u32; 2],
}

struct PrepDone {
    generation: u64,
    path: PathBuf,
    image: ColorImage,
    /// Size of the job's input image (counted in [`ThumbGrid::prep_bytes`]).
    input_bytes: usize,
}

pub struct ThumbGrid {
    cards: Vec<Card>,
    index: HashMap<PathBuf, usize>,
    /// `(done, total)` while the folder job renders meshes; the bar is hidden otherwise.
    pub progress: Option<(usize, usize)>,
    generation: u64,
    prep_tx: Sender<PrepJob>,
    prep_rx: Receiver<PrepDone>,
    in_prep: usize,
    prep_bytes: usize,
    textures: usize,
    /// Cards holding a `Ready` image and the bytes of those images.
    loaded: BTreeSet<usize>,
    image_bytes: usize,
    /// Cards painted in the last frame.
    visible: Range<usize>,
    /// Fetches evicted cards again; has no renderer, so it never renders a mesh.
    reload: ThumbnailService,
    reload_pending: bool,
    reload_started: Option<Instant>,
    frame: u64,
    reset_scroll: bool,
    metrics: Metrics,
    sort: SortOrder,
    /// The folder job may still deliver previews (set by the app).
    pub generating: bool,
    info: InfoCache,
    /// Set when the dropdown re-sorted the cards; see [`Self::take_resorted`].
    resorted: Option<Vec<PathBuf>>,
    ctx: egui::Context,
}

impl ThumbGrid {
    /// `reload` is a service without a renderer used to fetch evicted card images again.
    pub fn new(ctx: &egui::Context, reload: ThumbnailService) -> Self {
        let (prep_tx, job_rx) = channel::<PrepJob>();
        let (done_tx, prep_rx) = channel::<PrepDone>();
        let prep_ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("thumb-prep".into())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    let input_bytes = job.image.as_raw().len();
                    let image = shrink(job.image, job.max_px);
                    if done_tx
                        .send(PrepDone {
                            generation: job.generation,
                            path: job.path,
                            image,
                            input_bytes,
                        })
                        .is_err()
                    {
                        break;
                    }
                    prep_ctx.request_repaint();
                }
            });
        if let Err(e) = spawned {
            log::error!("cannot start thumbnail prep thread: {e}");
        }
        Self {
            cards: Vec::new(),
            index: HashMap::new(),
            progress: None,
            generation: 0,
            prep_tx,
            prep_rx,
            in_prep: 0,
            prep_bytes: 0,
            textures: 0,
            loaded: BTreeSet::new(),
            image_bytes: 0,
            visible: 0..0,
            reload,
            reload_pending: false,
            reload_started: None,
            frame: 0,
            reset_scroll: false,
            metrics: Metrics::SMALL,
            sort: SortOrder::default(),
            generating: false,
            info: InfoCache::new(ctx),
            resorted: None,
            ctx: ctx.clone(),
        }
    }

    pub fn is_large(&self) -> bool {
        self.metrics == Metrics::LARGE
    }

    /// Switch between normal and double-size previews. Card images were shrunk for the old
    /// size, so they are dropped and fetched again (from the cache) as they come into view.
    pub fn set_large(&mut self, large: bool) {
        let metrics = if large {
            Metrics::LARGE
        } else {
            Metrics::SMALL
        };
        if metrics == self.metrics {
            return;
        }
        self.metrics = metrics;
        self.cancel_reload();
        // In-flight shrinks are for the old size.
        self.generation += 1;
        for i in 0..self.cards.len() {
            let had_image = !matches!(self.cards[i].thumb, Thumb::None);
            self.drop_image(i);
            let card = &mut self.cards[i];
            card.elided = None;
            if had_image {
                card.thumb = Thumb::Evicted;
            }
        }
    }

    /// Show `files` in the current sort order; returns their paths in display order (the
    /// order thumbnails should be generated in).
    pub fn set_files(&mut self, files: Vec<FileEntry>) -> Vec<PathBuf> {
        self.cancel_reload();
        let order = self.sort;
        let mut files: Vec<(SortKey, FileEntry)> =
            files.into_iter().map(|f| (SortKey::of(&f), f)).collect();
        files.sort_by(|a, b| order.compare(&a.0, &b.0));
        let paths: Vec<PathBuf> = files.iter().map(|(_, f)| f.path.clone()).collect();
        // Same folder again (e.g. "Back"): keep the cards, their images and the scroll
        // position; the folder job re-delivers whatever changed.
        if !paths.is_empty() && self.cards.iter().map(|c| &c.path).eq(paths.iter()) {
            self.progress = None;
            return paths;
        }
        self.generation += 1;
        self.progress = None;
        self.textures = 0;
        self.loaded.clear();
        self.image_bytes = 0;
        self.visible = 0..0;
        self.info.clear();
        self.cards = files
            .into_iter()
            .map(|(key, f)| Card {
                name: text::basename(&f.path),
                kind: FileKind::from_path(&f.path),
                elided: None,
                thumb: Thumb::None,
                texture: None,
                last_seen: 0,
                key,
                path: f.path,
            })
            .collect();
        self.reindex();
        self.reset_scroll = true;
        paths
    }

    pub fn sort_order(&self) -> SortOrder {
        self.sort
    }

    /// Re-order the cards in place (images and textures move with them); returns the paths
    /// in the new display order.
    pub fn set_sort(&mut self, order: SortOrder) -> Vec<PathBuf> {
        self.sort = order;
        self.cards.sort_by(|a, b| order.compare(&a.key, &b.key));
        self.reindex();
        self.visible = 0..0;
        self.reset_scroll = true;
        self.cards.iter().map(|c| c.path.clone()).collect()
    }

    fn reindex(&mut self) {
        self.index = self
            .cards
            .iter()
            .enumerate()
            .map(|(i, c)| (c.path.clone(), i))
            .collect();
        self.loaded = self
            .cards
            .iter()
            .enumerate()
            .filter(|(_, c)| matches!(c.thumb, Thumb::Ready(_)))
            .map(|(i, _)| i)
            .collect();
    }

    pub fn len(&self) -> usize {
        self.cards.len()
    }

    /// Card path by file name (case-insensitive) or full path, for scripted clicks.
    pub fn find(&self, name: &str) -> Option<PathBuf> {
        let lower = name.to_lowercase();
        self.cards
            .iter()
            .find(|c| {
                c.name.to_lowercase() == lower || c.path.to_string_lossy().to_lowercase() == lower
            })
            .map(|c| c.path.clone())
    }

    /// True while Ready images are still being shrunk or evicted ones fetched again.
    pub fn is_busy(&self) -> bool {
        self.in_prep > 0 || self.reload_pending
    }

    /// Queue a service `Ready` image for a card of this grid (others are ignored). Past the
    /// memory budget a card far from the viewport only remembers that it has an image.
    pub fn on_ready(&mut self, path: PathBuf, image: image::RgbaImage, pixels_per_point: f32) {
        let Some(&i) = self.index.get(&path) else {
            return;
        };
        let max_px = [
            (self.metrics.image.x * pixels_per_point).ceil() as u32,
            (self.metrics.image.y * pixels_per_point).ceil() as u32,
        ];
        let input_bytes = image.as_raw().len();
        let shrunk_bytes = 4 * max_px[0] as usize * max_px[1] as usize;
        let near = self.near().contains(&i);
        // Images in prep shrink to at most their input size.
        let fits = self.image_bytes + self.prep_bytes + shrunk_bytes <= IMAGE_BUDGET
            && self.prep_bytes + input_bytes <= PREP_BUDGET;
        let card = &mut self.cards[i];
        if !near && !fits {
            if !matches!(card.thumb, Thumb::Ready(_)) {
                card.thumb = Thumb::Evicted;
            }
            return;
        }
        let job = PrepJob {
            generation: self.generation,
            path,
            image,
            max_px,
        };
        if self.prep_tx.send(job).is_ok() {
            self.in_prep += 1;
            self.prep_bytes += input_bytes;
            if !matches!(card.thumb, Thumb::Ready(_)) {
                card.thumb = Thumb::Preparing;
            }
        }
    }

    /// Deliver the reload service's images; call once per frame, also while the grid is
    /// hidden.
    pub fn pump(&mut self, pixels_per_point: f32) {
        for event in self.reload.poll() {
            if let ThumbEvent::Ready { path, image } = event {
                self.on_ready(path, image, pixels_per_point);
            }
        }
        if self.reload_pending {
            if self.reload.is_busy() {
                // The service only wakes the UI for events; notice the job's end too.
                self.ctx.request_repaint_after(Duration::from_millis(30));
            } else {
                self.reload_pending = false;
                // Whatever the job could not produce (e.g. a deleted file) shows a placeholder.
                for card in &mut self.cards {
                    if matches!(card.thumb, Thumb::Reloading) {
                        card.thumb = Thumb::None;
                    }
                }
            }
        }
    }

    /// Stop fetching evicted images (the folder job was stopped or replaced).
    pub fn cancel_reload(&mut self) {
        self.reload.cancel_folder();
        self.reload_pending = false;
        for card in &mut self.cards {
            if matches!(card.thumb, Thumb::Reloading) {
                card.thumb = Thumb::Evicted;
            }
        }
    }

    /// Cards within [`NEAR_SCREENS`] of the visible ones.
    fn near(&self) -> Range<usize> {
        let span = NEAR_SCREENS
            * if self.visible.is_empty() {
                DEFAULT_VISIBLE
            } else {
                self.visible.len()
            };
        let end = (self.visible.end + span).min(self.cards.len());
        self.visible.start.saturating_sub(span)..end
    }

    fn drain_prepared(&mut self) {
        while let Ok(done) = self.prep_rx.try_recv() {
            self.in_prep = self.in_prep.saturating_sub(1);
            self.prep_bytes = self.prep_bytes.saturating_sub(done.input_bytes);
            if done.generation != self.generation {
                continue;
            }
            if let Some(&i) = self.index.get(&done.path) {
                self.drop_image(i);
                self.image_bytes += image_bytes(&done.image);
                self.loaded.insert(i);
                self.cards[i].thumb = Thumb::Ready(Arc::new(done.image));
            }
        }
    }

    /// Release card `i`'s image and texture (the caller sets its new state).
    fn drop_image(&mut self, i: usize) {
        let card = &mut self.cards[i];
        if card.texture.take().is_some() {
            self.textures -= 1;
        }
        if let Thumb::Ready(img) = &card.thumb {
            self.image_bytes -= image_bytes(img);
            self.loaded.remove(&i);
        }
    }

    /// Over the budget, evict the least recently seen images of cards away from the
    /// viewport, down to 3/4 of the budget so this does not run every frame.
    fn evict_images(&mut self) {
        if self.image_bytes <= IMAGE_BUDGET {
            return;
        }
        let near = self.near();
        let mut far: Vec<usize> = self
            .loaded
            .iter()
            .copied()
            .filter(|i| !near.contains(i))
            .collect();
        far.sort_by_key(|&i| self.cards[i].last_seen);
        for i in far {
            if self.image_bytes <= IMAGE_BUDGET / 4 * 3 {
                break;
            }
            self.drop_image(i);
            self.cards[i].thumb = Thumb::Evicted;
        }
    }

    /// Fetch evicted images near the viewport again, visible cards first. A running job
    /// is replaced (at most every [`RELOAD_THROTTLE`]) only while visible cards wait.
    fn request_reloads(&mut self) {
        let near = self.near();
        let evicted = |c: &Card| matches!(c.thumb, Thumb::Evicted);
        if !self.cards[near.clone()].iter().any(evicted) {
            return;
        }
        if self.reload_pending {
            let visible_waiting = self.cards[self.visible.clone()].iter().any(evicted);
            let throttled = self
                .reload_started
                .is_some_and(|t| t.elapsed() < RELOAD_THROTTLE);
            if !visible_waiting || throttled {
                return;
            }
        }
        let order = self
            .visible
            .clone()
            .chain(near.start..self.visible.start)
            .chain(self.visible.end..near.end);
        let mut batch = Vec::new();
        for i in order {
            let card = &mut self.cards[i];
            if matches!(card.thumb, Thumb::Evicted | Thumb::Reloading) {
                card.thumb = Thumb::Reloading;
                batch.push(card.path.clone());
            }
        }
        // Cards of a replaced job that are no longer near are fetched when they come back.
        for (i, card) in self.cards.iter_mut().enumerate() {
            if !near.contains(&i) && matches!(card.thumb, Thumb::Reloading) {
                card.thumb = Thumb::Evicted;
            }
        }
        self.reload.start_folder(batch);
        self.reload_pending = true;
        self.reload_started = Some(Instant::now());
    }

    /// Paint the grid page; returns the clicked card's path.
    pub fn show(&mut self, ui: &mut Ui, rect: Rect) -> Option<PathBuf> {
        self.drain_prepared();
        self.frame += 1;
        ui.painter().rect_filled(rect, 0.0, theme::VIEWER_BG);
        let mut area_rect = rect;
        let bar = Rect::from_min_size(rect.min, vec2(rect.width(), SORT_BAR_H));
        self.sort_bar(ui, bar);
        area_rect.min.y += SORT_BAR_H;
        if let Some((done, total)) = self.progress {
            let bar = Rect::from_min_size(area_rect.min, vec2(rect.width(), PROGRESS_H));
            widgets::progress_bar(ui, bar, done, total, &text::progress(done, total));
            area_rect.min.y += PROGRESS_H;
        }

        let mut clicked = None;
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(area_rect)
                .id_salt("thumb_grid"),
        );
        let mut area = ScrollArea::vertical()
            .id_salt("thumb_grid_scroll")
            .auto_shrink([false, false]);
        if std::mem::take(&mut self.reset_scroll) {
            area = area.vertical_scroll_offset(0.0);
        }
        let n = self.cards.len();
        let mut visible = 0..0;
        let m = self.metrics;
        area.show_viewport(&mut child, |ui, viewport| {
            let cols = m.columns(ui.available_width());
            ui.set_height(m.content_height(n, cols));
            ui.set_width(ui.available_width());
            if n == 0 {
                return;
            }
            let pitch = m.card.y + SPACING;
            let first_row = ((viewport.min.y - MARGIN) / pitch).floor().max(0.0) as usize;
            let last_row = (((viewport.max.y - MARGIN) / pitch).ceil().max(0.0) as usize)
                .min(n.div_ceil(cols));
            let origin = ui.max_rect().min;
            visible = (first_row * cols).min(n)..(last_row * cols).min(n);
            for idx in visible.clone() {
                let r = Rect::from_min_size(origin + m.card_offset(idx, cols), m.card);
                if self.paint_card(ui, idx, r) {
                    clicked = Some(self.cards[idx].path.clone());
                }
            }
        });
        self.visible = visible;
        self.evict_textures();
        self.evict_images();
        self.request_reloads();
        clicked
    }

    /// Paths in their new order after the dropdown changed the sort (once).
    pub fn take_resorted(&mut self) -> Option<Vec<PathBuf>> {
        self.resorted.take()
    }

    /// "Sort by:" label and dropdown above the cards.
    fn sort_bar(&mut self, ui: &mut Ui, bar: Rect) {
        ui.painter().hline(
            bar.x_range(),
            bar.bottom() - 0.5,
            Stroke::new(1.0, theme::BORDER_SUBTLE),
        );
        let label =
            ui.painter()
                .layout_no_wrap(SORT_LABEL.into(), fonts::regular(12.0), theme::TEXT);
        let x = bar.left() + MARGIN;
        ui.painter().galley(
            pos2(x, bar.center().y - label.size().y / 2.0).round(),
            label.clone(),
            theme::TEXT,
        );
        let items: Vec<&str> = SortOrder::ALL.iter().map(|o| o.label()).collect();
        let size = widgets::combo_size(ui, &items);
        let combo = Rect::from_min_size(
            pos2(
                x + label.size().x + 8.0,
                (bar.center().y - size.y / 2.0).round(),
            ),
            size,
        );
        let current = SortOrder::ALL
            .iter()
            .position(|&o| o == self.sort)
            .unwrap_or(0);
        if let Some(i) = widgets::combo_at(ui, combo, Id::new("grid_sort"), current, &items)
            && SortOrder::ALL[i] != self.sort
        {
            self.resorted = Some(self.set_sort(SortOrder::ALL[i]));
        }
    }

    fn paint_card(&mut self, ui: &mut Ui, idx: usize, r: Rect) -> bool {
        let resp = ui.interact(
            r,
            Id::new(("thumb_card", idx, self.generation)),
            Sense::click(),
        );
        let hovered = resp.hovered();
        let frame = self.frame;
        let m = self.metrics;
        let card = &mut self.cards[idx];
        card.last_seen = frame;

        let (bg, border) = if hovered {
            (theme::CARD_HOVER_BG, theme::ACCENT)
        } else {
            (theme::CARD_BG, theme::BORDER)
        };
        let painter = ui.painter();
        painter.rect(
            r,
            CornerRadius::same(8),
            bg,
            Stroke::new(1.5, border),
            StrokeKind::Inside,
        );
        let well = Rect::from_min_size(r.min + Vec2::splat(CARD_PAD), m.well);
        painter.rect_filled(well, CornerRadius::same(6), theme::THUMB_WELL_BG);

        if card.texture.is_none()
            && let Thumb::Ready(img) = &card.thumb
        {
            let name = format!("thumb:{}", card.path.display());
            card.texture = Some(ui.ctx().load_texture(
                name,
                Arc::clone(img),
                TextureOptions::LINEAR,
            ));
            self.textures += 1;
        }
        let card = &mut self.cards[idx];
        if let Some(tex) = &card.texture {
            let size = fit(tex.size_vec2(), m.image);
            let img_rect = Rect::from_center_size(well.center(), size);
            ui.painter().image(
                tex.id(),
                img_rect,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        } else {
            let (glyph, tint, caption) = icons::placeholder(card.kind);
            paint_placeholder(ui, well, m.icon, glyph, tint, caption);
        }

        let font = fonts::bold(10.0);
        let name = card
            .elided
            .get_or_insert_with(|| widgets::elide_middle(ui, &card.name, m.name_width, &font));
        let name_center = pos2(
            r.center().x,
            well.bottom() + (r.bottom() - CARD_PAD - well.bottom()) / 2.0 + 3.0,
        );
        ui.painter().text(
            name_center,
            Align2::CENTER_CENTER,
            name.as_str(),
            font,
            theme::TEXT_CARD,
        );

        if hovered {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        let clicked = resp.clicked();
        if resp.hovered() {
            self.card_tooltip(&resp, idx);
        }
        clicked
    }

    /// Name, type, size, date, dimensions and, without a preview, the reason why.
    fn card_tooltip(&mut self, resp: &egui::Response, idx: usize) {
        let card = &self.cards[idx];
        let (path, kind, size) = (card.path.clone(), card.kind, card.key.size);
        let has_preview = !matches!(card.thumb, Thumb::None);
        let mut rows: Vec<(&str, String)> = vec![
            ("Type", card_info::kind_label(kind).into()),
            ("Size", card_info::size_label(size)),
        ];
        if let Some(t) = card.key.modified.and_then(card_info::local_time) {
            rows.push(("Modified", t));
        }
        let name = card.name.clone();
        let details = self.info.get(&path, kind, size).cloned();
        let reading =
            details.is_none() && (kind.is_mesh() || kind.is_media()) && kind != FileKind::Webm;
        if let Some(d) = &details {
            rows.extend(d.rows.iter().cloned());
        }
        let reason = (!has_preview).then(|| {
            card_info::missing_preview_reason(
                kind,
                size,
                details.as_ref().and_then(|d| d.error.as_deref()),
                self.generating,
            )
        });
        resp.clone().on_hover_ui(|ui| {
            ui.set_max_width(360.0);
            ui.label(
                egui::RichText::new(&name)
                    .font(fonts::bold(13.0))
                    .color(theme::TEXT_STRONG),
            );
            egui::Grid::new(("card_tip", idx))
                .num_columns(2)
                .spacing(vec2(12.0, 3.0))
                .show(ui, |ui| {
                    for (label, value) in &rows {
                        ui.label(egui::RichText::new(format!("{label}:")).color(theme::TEXT));
                        ui.label(egui::RichText::new(value).color(theme::TEXT_STRONG));
                        ui.end_row();
                    }
                    if reading && kind.is_mesh() && size <= stlv_thumbs::MAX_RENDER_FILE_SIZE {
                        ui.label(egui::RichText::new("Dimensions:").color(theme::TEXT));
                        ui.label(egui::RichText::new("Reading\u{2026}").color(theme::TEXT_MUTED));
                        ui.end_row();
                    }
                });
            if let Some(reason) = &reason {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(reason).color(theme::ACCENT));
            }
        });
    }

    fn evict_textures(&mut self) {
        if self.textures <= MAX_TEXTURES {
            return;
        }
        let frame = self.frame;
        for card in &mut self.cards {
            if self.textures <= MAX_TEXTURES / 2 {
                break;
            }
            if card.last_seen + 2 < frame && card.texture.take().is_some() {
                self.textures -= 1;
            }
        }
    }
}

/// Font size of the placeholder emoji (`font-size: 28px`).
const PLACEHOLDER_ICON: f32 = 28.0;

/// The color emoji with its caption below (spacing 2), centered in the well.
fn paint_placeholder(
    ui: &Ui,
    well: Rect,
    icon: f32,
    glyph: &str,
    tint: egui::Color32,
    caption: &str,
) {
    let icon_h = emoji::box_size(icon).y.round();
    if caption.is_empty() {
        emoji::paint(ui, glyph, icon, well.center(), tint);
        return;
    }
    let job = egui::text::LayoutJob::single_section(
        caption.to_string(),
        egui::TextFormat {
            font_id: fonts::bold(9.0),
            color: tint,
            extra_letter_spacing: 0.5,
            ..Default::default()
        },
    );
    let cap = ui.painter().layout_job(job);
    let total_h = icon_h + 2.0 + cap.size().y;
    let top = well.center().y - total_h / 2.0;
    emoji::paint(
        ui,
        glyph,
        icon,
        pos2(well.center().x, top + icon_h / 2.0),
        tint,
    );
    let cap_pos = pos2(well.center().x - cap.size().x / 2.0, top + icon_h + 2.0);
    ui.painter().galley(cap_pos.round(), cap, tint);
}

fn image_bytes(image: &ColorImage) -> usize {
    image.pixels.len() * 4
}

/// Shrink to fit `max_px` (never enlarges) and convert for egui.
fn shrink(img: image::RgbaImage, max_px: [u32; 2]) -> ColorImage {
    let (w, h) = img.dimensions();
    let target = fit(
        vec2(w as f32, h as f32),
        vec2(max_px[0] as f32, max_px[1] as f32),
    );
    let img = if w > 0 && h > 0 && (target.x < w as f32 || target.y < h as f32) {
        let tw = (target.x.round() as u32).max(1);
        let th = (target.y.round() as u32).max(1);
        image::imageops::resize(&img, tw, th, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let size = [img.width() as usize, img.height() as usize];
    ColorImage::from_rgba_unmultiplied(size, img.as_raw())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;

    #[test]
    fn folder_listing_matches_core() {
        let d = TempDir::new("grid");
        let p = d.path();
        for (name, len) in [("b.STL", 3), ("a.png", 5), ("C.gcode", 1), ("x.txt", 2)] {
            std::fs::write(p.join(name), vec![0u8; len]).unwrap();
        }
        std::fs::create_dir(p.join("dir.stl")).unwrap();
        for show_media in [false, true] {
            let listed = list_folder(p, show_media).unwrap();
            let core = stlv_core::files::list_supported_files(p, show_media).unwrap();
            let paths: Vec<PathBuf> = listed.iter().map(|f| f.path.clone()).collect();
            assert_eq!(paths, core);
            for f in &listed {
                let meta = std::fs::metadata(&f.path).unwrap();
                assert_eq!(f.size, meta.len());
                assert_eq!(f.modified, meta.modified().ok());
            }
        }
        assert_eq!(list_folder(p, true).unwrap().len(), 3);
        assert!(list_folder(&p.join("missing"), true).is_err());
    }

    fn entries(files: &[PathBuf]) -> Vec<FileEntry> {
        files
            .iter()
            .map(|p| FileEntry {
                path: p.clone(),
                size: 0,
                modified: None,
            })
            .collect()
    }

    fn entry(name: &str, size: u64, secs: u64) -> FileEntry {
        FileEntry {
            path: PathBuf::from(name),
            size,
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs)),
        }
    }

    #[test]
    fn sort_orders() {
        let mut grid = ThumbGrid::new(
            &egui::Context::default(),
            ThumbnailService::new(
                Arc::new(stlv_thumbs::ThumbnailCache::open(Path::new(":memory:")).unwrap()),
                None,
                || {},
            ),
        );
        let files = vec![
            entry("b.stl", 30, 100),
            entry("A.stl", 10, 300),
            entry("c.stl", 20, 200),
            entry("d.stl", 20, 300),
        ];
        let names = |paths: Vec<PathBuf>| -> Vec<String> {
            paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(
            names(grid.set_files(files)),
            ["A.stl", "b.stl", "c.stl", "d.stl"]
        );
        let expect = [
            (SortOrder::NameDesc, ["d.stl", "c.stl", "b.stl", "A.stl"]),
            (SortOrder::SizeAsc, ["A.stl", "c.stl", "d.stl", "b.stl"]),
            (SortOrder::SizeDesc, ["b.stl", "c.stl", "d.stl", "A.stl"]),
            (SortOrder::DateNewest, ["A.stl", "d.stl", "c.stl", "b.stl"]),
            (SortOrder::DateOldest, ["b.stl", "c.stl", "A.stl", "d.stl"]),
            (SortOrder::NameAsc, ["A.stl", "b.stl", "c.stl", "d.stl"]),
        ];
        for (order, want) in expect {
            assert_eq!(names(grid.set_sort(order)), want, "{order:?}");
            // The path index follows the cards.
            for (i, name) in want.iter().enumerate() {
                assert_eq!(grid.index[&PathBuf::from(name)], i);
            }
        }
        // A newly opened folder keeps the chosen order.
        grid.set_sort(SortOrder::SizeDesc);
        let again = vec![entry("x.stl", 1, 0), entry("y.stl", 2, 0)];
        assert_eq!(names(grid.set_files(again)), ["y.stl", "x.stl"]);
    }

    fn wait_prepared(grid: &mut ThumbGrid) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while grid.in_prep > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
            grid.drain_prepared();
        }
        assert_eq!(grid.in_prep, 0, "prep thread stalled");
    }

    fn state(grid: &ThumbGrid, i: usize) -> &'static str {
        match grid.cards[i].thumb {
            Thumb::None => "none",
            Thumb::Preparing => "preparing",
            Thumb::Ready(_) => "ready",
            Thumb::Evicted => "evicted",
            Thumb::Reloading => "reloading",
        }
    }

    #[test]
    fn card_images_stay_within_budget() {
        let d = TempDir::new("grid-budget");
        let files: Vec<PathBuf> = (0..2000)
            .map(|i| {
                let p = d.path().join(format!("{i:04}.png"));
                image::RgbaImage::new(8, 8).save(&p).unwrap();
                p
            })
            .collect();
        let cache = Arc::new(stlv_thumbs::ThumbnailCache::open(Path::new(":memory:")).unwrap());
        let mut grid = ThumbGrid::new(
            &egui::Context::default(),
            ThumbnailService::new(cache, None, || {}),
        );
        grid.set_files(entries(&files));
        // Every card gets a full-size image at 200 % scaling (169 KB each, 338 MB in all),
        // delivered in per-frame batches.
        let big = || image::RgbaImage::new(220, 192);
        for chunk in files.chunks(50) {
            for f in chunk {
                grid.on_ready(f.clone(), big(), 2.0);
            }
            wait_prepared(&mut grid);
        }
        assert!(grid.image_bytes <= IMAGE_BUDGET, "{}", grid.image_bytes);
        assert_eq!(grid.image_bytes, grid.loaded.len() * 220 * 192 * 4);
        assert!(grid.loaded.len() > 300, "{}", grid.loaded.len());
        assert_eq!(state(&grid, 0), "ready");
        assert_eq!(state(&grid, 1999), "evicted");

        // Scrolled to the end: the near cards are fetched again (the PNG files decode as
        // 8 x 8 images); near cards are kept even past the budget, far ones evicted.
        grid.visible = 1900..1930;
        grid.request_reloads();
        assert!(grid.is_busy());
        assert_eq!(state(&grid, 1900), "reloading");
        let deadline = Instant::now() + Duration::from_secs(20);
        while grid.is_busy() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
            grid.pump(2.0);
            grid.drain_prepared();
        }
        assert!(!grid.is_busy());
        for i in grid.near() {
            assert_eq!(state(&grid, i), "ready", "card {i}");
        }
        for f in &files[1870..1960] {
            grid.on_ready(f.clone(), big(), 2.0);
        }
        wait_prepared(&mut grid);
        grid.evict_images();
        assert!(grid.image_bytes <= IMAGE_BUDGET, "{}", grid.image_bytes);
        assert_eq!(state(&grid, 1900), "ready");
        assert_eq!(state(&grid, 0), "evicted");

        // A new folder starts from scratch.
        grid.set_files(entries(&files[..3]));
        assert_eq!((grid.image_bytes, grid.loaded.len()), (0, 0));
    }

    #[test]
    fn large_icons_refetch_and_same_folder_keeps_cards() {
        let d = TempDir::new("grid-large");
        let files: Vec<PathBuf> = (0..3).map(|i| d.path().join(format!("{i}.png"))).collect();
        let cache = Arc::new(stlv_thumbs::ThumbnailCache::open(Path::new(":memory:")).unwrap());
        let mut grid = ThumbGrid::new(
            &egui::Context::default(),
            ThumbnailService::new(cache, None, || {}),
        );
        grid.set_files(entries(&files));
        grid.on_ready(files[0].clone(), image::RgbaImage::new(220, 192), 1.0);
        wait_prepared(&mut grid);
        assert_eq!(grid.image_bytes, 110 * 96 * 4);

        // Re-opening the same folder (Back) keeps the image.
        grid.set_files(entries(&files));
        assert_eq!(state(&grid, 0), "ready");

        // Large: images shrunk for the small card are dropped and re-delivered at 2x.
        grid.set_large(true);
        assert!(grid.is_large());
        assert_eq!((state(&grid, 0), state(&grid, 1)), ("evicted", "none"));
        assert_eq!((grid.image_bytes, grid.loaded.len()), (0, 0));
        grid.on_ready(files[0].clone(), image::RgbaImage::new(220, 192), 1.0);
        wait_prepared(&mut grid);
        assert_eq!(grid.image_bytes, 220 * 192 * 4);
    }

    #[test]
    fn grid_geometry() {
        let m = Metrics::SMALL;
        assert_eq!(m.columns(0.0), 1);
        assert_eq!(m.columns(144.0), 1);
        assert_eq!(m.columns(290.0), 2);
        assert_eq!(m.columns(1000.0), 6);
        assert_eq!(m.card_offset(0, 3), vec2(15.0, 15.0));
        assert_eq!(m.card_offset(4, 3), vec2(160.0, 180.0));
        assert_eq!(m.content_height(0, 3), 0.0);
        assert_eq!(m.content_height(3, 3), 180.0);
        assert_eq!(m.content_height(4, 3), 345.0);
        // Large doubles the preview well (114x100 -> 228x200); the card grows by as much.
        let l = Metrics::LARGE;
        assert_eq!(
            (l.card, l.well, l.image),
            (vec2(244.0, 250.0), vec2(228.0, 200.0), vec2(220.0, 192.0))
        );
        assert_eq!(l.columns(1000.0), 3);
        assert_eq!(l.card_offset(4, 3), vec2(274.0, 280.0));
    }

    #[test]
    fn fitting() {
        let f = fit(vec2(228.0, 200.0), IMAGE_BOX);
        assert!((f.x - 109.44).abs() < 1e-3 && f.y == 96.0, "{f:?}");
        assert_eq!(fit(vec2(55.0, 48.0), IMAGE_BOX), vec2(110.0, 96.0));
        assert_eq!(fit(vec2(0.0, 5.0), IMAGE_BOX), Vec2::ZERO);
        let img = shrink(image::RgbaImage::new(228, 200), [110, 96]);
        assert_eq!(img.size, [109, 96]);
        let small = shrink(image::RgbaImage::new(20, 10), [110, 96]);
        assert_eq!(small.size, [20, 10]);
    }
}
