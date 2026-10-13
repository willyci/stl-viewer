//! The main window (app.py `MainWindow`): layout, flows and background-job plumbing.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{
    Color32, Event, Id, Margin, Rect, Sense, Stroke, TextEdit, Ui, UiBuilder, Vec2,
    ViewportCommand, pos2, vec2,
};
use stlv_core::FileKind;
use stlv_core::format::format_file_size;
use stlv_media::{ImageViewer, VideoPlayer};
use stlv_render::{ShadingMode, ThumbnailRenderer, ViewPreset, Viewport3D};
use stlv_thumbs::{ThumbEvent, ThumbnailCache, ThumbnailService};

use crate::cli::Args;
use crate::dialogs::{self, ColorDialogResult};
use crate::grid::{SortOrder, ThumbGrid};
use crate::loader::{LoadOutcome, ModelLoader};
use crate::metadata::Metadata;
use crate::script;
use crate::settings::{Persist, Settings};
use crate::tree::{FileTree, TreeAction};
use crate::widgets::{self, ButtonStyle};
use crate::{emoji, fonts, icons, overlay, text, theme};

mod harness;

use harness::HarnessState;

const STATUS_H: f32 = 21.0;
const DECK_H: f32 = 64.0;
const SPLITTER_W: f32 = 4.0;
/// `central_splitter.setSizes([320, 960])`.
const DEFAULT_SPLIT: [f32; 2] = [320.0, 960.0];
/// The sidebar's minimum width without long metadata values (Qt's `minimumSizeHint`).
const MIN_SIDEBAR_W: f32 = 195.0;
const MIN_WINDOW_H: f32 = 420.0;
const SIDEBAR_MARGIN: f32 = 15.0;
const HEADER_H: f32 = 27.0;
/// The ⚡ label's width (Segoe UI Emoji advance at 20 px) and the header row spacing.
const HEADER_BOLT_W: f32 = 28.0;
const DECK_MARGIN: f32 = 20.0;
const DECK_SPACING: f32 = 20.0;
/// Gap between a deck label and its dropdown.
const LABEL_GAP: f32 = 8.0;
const PATH_EDIT_ID: &str = "path_edit";
const SECTION_SPACING: f32 = 15.0;
const ROW_SPACING: f32 = 6.0;
/// How long the thumbnail pipeline must be quiet before a screenshot counts as settled.
const QUIET_PERIOD: Duration = Duration::from_millis(300);
/// Screenshots (and `idle` script steps) wait at most this long for the UI to settle.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Viewport,
    Image,
    Video,
    Grid,
}

/// Folder-grid bookkeeping for the status bar; `generation` changes whenever the folder job
/// is replaced or stopped so late messages are ignored.
#[derive(Default)]
struct FolderJob {
    generation: u64,
    file_count: usize,
    loaded_status_pending: bool,
}

/// CLI debug options applied after the first successful model load.
#[derive(Default)]
struct AfterLoad {
    shading: Option<ShadingMode>,
    preset: Option<ViewPreset>,
    auto_rotate: bool,
}

pub struct StlViewerApp {
    viewport: Viewport3D,
    image_viewer: ImageViewer,
    video: VideoPlayer,
    service: ThumbnailService,
    loader: ModelLoader,
    tree: FileTree,
    grid: ThumbGrid,
    meta: Metadata,
    page: Page,
    status: String,
    path_text: String,
    path_dirty: bool,
    show_media: bool,
    loading: Option<String>,
    error: Option<String>,
    color_dialog: Option<(Color32, Color32)>,
    /// QSplitter "sizer" widths of the sidebar and the right panel: the space is shared in
    /// this ratio within each side's minimum (initialised on the first frame).
    split: Option<[f32; 2]>,
    /// Minimum window width last sent to the OS (Qt grows the window to fit its layout).
    min_window_w: f32,
    /// Move the path edit's cursor to the end after its text was set programmatically.
    path_cursor_to_end: bool,
    /// Input time the path edit's caret blink restarts from (typing keeps it visible).
    caret_epoch: f64,
    folder: FolderJob,
    /// Folder shown in the grid (the last one opened).
    grid_folder: Option<PathBuf>,
    /// Folder to return to with "Back" after a thumbnail was opened.
    back_folder: Option<PathBuf>,
    last_thumb_event: Instant,
    after_load: AfterLoad,
    /// Camera view shown in the "View:" dropdown (the last one applied).
    view: ViewPreset,
    /// Cross-section handles over the 3D view.
    zslider: crate::zslider::ZSlider,
    harness: HarnessState,
    started: Instant,
    persist: Persist,
    ctx: egui::Context,
}

impl StlViewerApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        args: Args,
        saved: Settings,
        settings_path: Option<PathBuf>,
    ) -> Result<Self, String> {
        let ctx = cc.egui_ctx.clone();
        fonts::install(&ctx);
        theme::apply(&ctx);
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .ok_or("The wgpu renderer is not available on this system.")?;
        // wgpu's default handler panics on any error. A surface reconfigure can time out
        // waiting for the GPU while thumbnails render (window resized mid-render); egui
        // reconfigures again on the next frame, so log instead of crashing.
        render_state
            .device
            .on_uncaptured_error(Arc::new(|e| log::error!("wgpu: {e}")));

        let viewport = Viewport3D::new(render_state);
        let renderer = Arc::new(ThumbnailRenderer::new(
            render_state.device.clone(),
            render_state.queue.clone(),
        ));
        let cache = Arc::new(open_cache(args.cache_db.as_deref())?);
        let wake_ctx = ctx.clone();
        let saver = crate::loader::ThumbSaver {
            renderer: Arc::clone(&renderer),
            cache: Arc::clone(&cache),
        };
        let service = ThumbnailService::new(Arc::clone(&cache), Some(renderer), move || {
            wake_ctx.request_repaint()
        });
        let reload_ctx = ctx.clone();
        let reload = ThumbnailService::new(cache, None, move || reload_ctx.request_repaint());
        let loader_ctx = ctx.clone();
        let loader = ModelLoader::new(move || loader_ctx.request_repaint()).with_thumbnails(saver);

        let (mut root, initial_file) = startup_target(args.path.as_deref());
        // Without a [PATH] argument, start where the user left off.
        let resume = args.path.is_none() && args.select.is_none();
        if resume && let Some(saved_root) = Settings::existing_dir(&saved.tree_root) {
            root = saved_root;
        }
        let show_media = args.show_media
            || saved.show_media
            || initial_file
                .as_deref()
                .is_some_and(|f| FileKind::from_path(f).is_media());
        let mut script = VecDeque::new();
        if let Some(s) = &args.script {
            script = script::parse(s)?.into();
        }

        let mut app = Self {
            viewport,
            image_viewer: ImageViewer::new(),
            video: VideoPlayer::new(),
            service,
            loader,
            path_text: display_path(&root),
            tree: FileTree::new(root.clone(), show_media),
            grid: ThumbGrid::new(&ctx, reload),
            meta: Metadata::default(),
            page: Page::Viewport,
            status: text::READY.into(),
            path_dirty: false,
            show_media,
            loading: None,
            error: None,
            color_dialog: None,
            split: None,
            min_window_w: 0.0,
            path_cursor_to_end: true,
            caret_epoch: 0.0,
            folder: FolderJob::default(),
            grid_folder: None,
            back_folder: None,
            last_thumb_event: Instant::now(),
            view: ViewPreset::Isometric,
            zslider: Default::default(),
            after_load: AfterLoad {
                shading: args.shading,
                preset: args.preset,
                auto_rotate: args.auto_rotate,
            },
            harness: HarnessState {
                screenshot: args.screenshot.clone(),
                delay: Duration::from_millis(args.screenshot_delay_ms),
                script_active: !script.is_empty(),
                script,
                ..Default::default()
            },
            started: Instant::now(),
            persist: Persist::new(settings_path, saved.clone()),
            ctx,
        };
        if let Some([r, g, b]) = saved.model_color {
            app.viewport
                .set_color([r, g, b].map(|v| f32::from(v) / 255.0));
        }
        app.grid.set_large(saved.large_icons);
        if let Some(order) = saved.sort.as_deref().and_then(SortOrder::from_key) {
            app.grid.set_sort(order);
        }
        app.split = saved.split;
        let watch_ctx = app.ctx.clone();
        app.tree.watch(crate::watch::POLL_INTERVAL, move || {
            watch_ctx.request_repaint()
        });
        app.service.precache_dir(&root);
        // Like Qt, the first widget in the focus chain (the path edit) starts focused.
        app.ctx
            .memory_mut(|m| m.request_focus(Id::new(PATH_EDIT_ID)));
        if let Some(file) = initial_file {
            app.select_path(&file);
        }
        if let Some(sel) = &args.select {
            app.select_path(&absolute(sel));
        } else if resume && let Some(folder) = Settings::existing_dir(&saved.last_folder) {
            app.select_path(&folder);
        }
        if args.color_picker {
            app.open_color_dialog();
        }
        Ok(app)
    }

    // -----------------------------------------------------------------------------------------
    // Flows

    /// Tree input: a changed selection (the tree has already selected the row) or a
    /// folder expanded / collapsed.
    fn on_tree_action(&mut self, action: TreeAction) {
        match action {
            TreeAction::Select(path) => self.on_selected(path),
            TreeAction::Toggle(path) => {
                if self.tree.is_expanded(&path) {
                    self.tree.collapse(&path);
                } else {
                    self.tree.expand(&path);
                    self.open_folder(&path);
                }
            }
        }
    }

    /// Select `path` in the tree as if clicked (revealing it), then act on it.
    fn select_path(&mut self, path: &Path) {
        self.tree.select_and_reveal(path);
        let resolved = self
            .tree
            .selected()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.to_path_buf());
        self.on_selected(resolved);
    }

    fn on_selected(&mut self, path: PathBuf) {
        if path.is_dir() {
            self.open_folder(&path);
        } else {
            self.open_file(path);
        }
    }

    fn stop_folder_job(&mut self) {
        self.service.cancel_folder();
        self.grid.cancel_reload();
        self.grid.progress = None;
        self.grid.generating = false;
        self.folder.generation += 1;
        self.folder.loaded_status_pending = false;
    }

    fn cleanup_media(&mut self) {
        self.image_viewer.clear();
        self.video.stop();
    }

    fn cancel_model_load(&mut self) {
        self.loader.cancel();
        self.loading = None;
    }

    /// Python `_open_folder`.
    fn open_folder(&mut self, folder: &Path) {
        self.cancel_model_load();
        self.cleanup_media();
        self.service.cancel_precache();
        self.page = Page::Grid;
        self.grid_folder = Some(folder.to_path_buf());
        self.back_folder = None;
        let listing = match crate::grid::list_folder(folder, self.show_media) {
            Ok(l) => l,
            Err(e) => {
                self.status = text::dir_error(&e);
                return;
            }
        };
        let total: u64 = listing.iter().map(|f| f.size).sum();
        self.stop_folder_job();
        let files = self.grid.set_files(listing);
        let count = files.len();
        self.service.start_folder(files);
        self.grid.generating = true;
        self.meta.set_folder(folder, count, total);
        self.set_path_text(display_path(folder));
        self.status = text::folder(&text::basename(folder), count, total);
        self.folder.file_count = count;
        self.folder.loaded_status_pending = true;
    }

    /// Python `_file_selected` for files.
    fn open_file(&mut self, path: PathBuf) {
        let kind = FileKind::from_path(&path);
        if !kind.is_mesh() && !kind.is_media() {
            return;
        }
        self.stop_folder_job();
        self.loader.cancel();
        self.cleanup_media();
        if kind.is_media() {
            self.loading = None;
            self.load_media(&path, kind);
            return;
        }
        self.page = Page::Viewport;
        let name = text::basename(&path);
        let size = std::fs::metadata(&path).ok().map(|m| m.len());
        self.status = text::reading_model(&name, size);
        self.meta.set_loading();
        self.loading = Some(text::parsing(&name));
        self.loader.load(path);
    }

    fn on_model_loaded(&mut self, out: LoadOutcome) {
        self.loading = None;
        let prepared = match out.result {
            Ok(p) => p,
            Err(e) => {
                self.status = text::MODEL_LOAD_FAILED.into();
                self.error = Some(text::load_error(&e));
                return;
            }
        };
        let model = Arc::clone(&prepared.model);
        self.viewport.set_model(prepared);
        self.view = ViewPreset::Isometric;
        self.zslider.reset();
        self.viewport.set_shading(ShadingMode::Smooth);
        self.viewport.set_auto_rotate(false);
        let size = std::fs::metadata(&out.path).ok().map(|m| m.len());
        self.meta.set_model(&model, &out.path, size);
        self.status = text::loaded(&model);

        let after = std::mem::take(&mut self.after_load);
        if let Some(s) = after.shading {
            self.viewport.set_shading(s);
        }
        if let Some(p) = after.preset {
            self.set_view(p);
        }
        if after.auto_rotate {
            self.viewport.set_auto_rotate(true);
        }
    }

    /// Python `_load_media_file`.
    fn load_media(&mut self, path: &Path, kind: FileKind) {
        let name = text::basename(path);
        self.status = text::reading_media(&name);
        self.meta.set_loading();
        let size = std::fs::metadata(path)
            .map(|m| format_file_size(m.len()))
            .unwrap_or_else(|_| "-".into());
        self.meta.set_media_file(path, &size);
        match kind {
            FileKind::Image => {
                self.page = Page::Image;
                match self.image_viewer.load(path) {
                    Ok(info) => {
                        self.meta.set_image(path, info.width, info.height, &size);
                        let ext = text::ext_upper(path);
                        self.status = text::image(&name, &ext, info.width, info.height, &size);
                    }
                    Err(e) => {
                        log::warn!("image {}: {e}", path.display());
                        self.status = text::IMAGE_LOAD_FAILED.into();
                    }
                }
            }
            FileKind::Gif => {
                self.page = Page::Image;
                let dims = match self.image_viewer.load(path) {
                    Ok(info) => Some((info.width, info.height)),
                    Err(e) => {
                        log::warn!("gif {}: {e}", path.display());
                        None
                    }
                };
                self.meta.set_gif(path, dims, &size);
                self.status = text::gif(&name, dims, &size);
            }
            FileKind::Svg => {
                self.page = Page::Image;
                let (w, h) = match self.image_viewer.load(path) {
                    Ok(info) => (info.width, info.height),
                    Err(e) => {
                        log::warn!("svg {}: {e}", path.display());
                        (0, 0)
                    }
                };
                self.meta.set_svg(path, w, h, &size);
                self.status = text::svg(&name, w, h, &size);
            }
            FileKind::Webm => {
                self.page = Page::Video;
                self.meta.set_webm(path, &size);
                self.status = match self.video.open(path) {
                    Ok(()) => text::video(&name, &size),
                    Err(e) => text::video_failed(&e),
                };
            }
            _ => {}
        }
    }

    fn on_thumbnail_clicked(&mut self, path: PathBuf) {
        self.stop_folder_job();
        let folder = self.grid_folder.clone();
        self.select_path(&path);
        // Only once the file is open: opening it may not leave the grid (unsupported file).
        if self.page != Page::Grid {
            self.back_folder = folder;
        }
    }

    /// "Back": return from a file opened from the grid to that folder's thumbnails.
    fn back_to_folder(&mut self) {
        if self.page == Page::Grid {
            return;
        }
        if let Some(folder) = self.back_folder.take() {
            self.select_path(&folder);
        }
    }

    fn can_go_back(&self) -> bool {
        self.page != Page::Grid && self.back_folder.is_some()
    }

    fn set_show_media(&mut self, on: bool) {
        self.show_media = on;
        self.tree.set_show_media(on);
        if on {
            self.status = text::MEDIA_ENABLED.into();
        } else {
            self.status = text::MEDIA_FILTERED.into();
            let media_selected = self
                .tree
                .selected()
                .is_some_and(|s| FileKind::from_path(s).is_media() && !s.is_dir());
            if media_selected {
                self.page = Page::Viewport;
                self.cleanup_media();
                self.meta.set_loading();
                self.tree.clear_selection();
                self.status = text::MEDIA_CLEARED.into();
            }
        }
        if let Some(sel) = self
            .tree
            .selected()
            .filter(|s| s.is_dir())
            .map(Path::to_path_buf)
        {
            self.open_folder(&sel);
        }
    }

    fn set_root(&mut self, root: PathBuf, status: String) {
        self.tree.set_root(root.clone());
        self.set_path_text(display_path(&root));
        self.status = status;
        self.service.precache_dir(&root);
    }

    fn set_path_text(&mut self, text: String) {
        self.path_text = text;
        self.path_dirty = false;
        self.path_cursor_to_end = true;
    }

    /// Path edit: Enter, or focus loss after an edit. Returns whether the path was a folder.
    fn navigate_typed(&mut self) -> bool {
        let norm = text::normpath(self.path_text.trim());
        // A bare drive ("c:") is its root, as in Qt; `absolute` would resolve it to the
        // drive's current directory instead. Python shows the text as typed.
        if let Some(root) = drive_root(&norm).filter(|r| r.is_dir()) {
            self.set_root(root, text::navigated(&norm));
            self.set_path_text(norm);
            return true;
        }
        let path = PathBuf::from(&norm);
        if path.is_dir() {
            let root = absolute(&path);
            let shown = display_path(&root);
            self.set_root(root, text::navigated(&shown));
            true
        } else {
            self.status = text::invalid_path(&norm);
            self.set_path_text(display_path(self.tree.root()));
            false
        }
    }

    fn navigate_up(&mut self) {
        let current = PathBuf::from(display_path(self.tree.root()));
        if let Some(parent) = current
            .parent()
            .filter(|p| !p.as_os_str().is_empty() && *p != current)
        {
            let parent = parent.to_path_buf();
            let shown = display_path(&parent);
            self.set_root(parent, text::navigated(&shown));
        }
    }

    fn browse(&mut self, frame: &eframe::Frame) {
        let dialog = rfd::FileDialog::new()
            .set_title(text::BROWSE_DIALOG_TITLE)
            .set_directory(self.tree.root())
            .set_parent(frame);
        if let Some(dir) = dialog.pick_folder() {
            let shown = display_path(&dir);
            self.set_root(dir, text::browsing(&shown));
        }
    }

    fn step_file(&mut self, step: isize) {
        let Some(current) = self.tree.selected().map(Path::to_path_buf) else {
            return;
        };
        if current.is_dir() {
            return;
        }
        let Some(dir) = current.parent() else { return };
        let files = crate::tree::visible_files(dir, self.show_media);
        if let Some(target) = crate::tree::step_file(&files, &current, step) {
            self.select_path(&target);
        }
    }

    fn set_view(&mut self, view: ViewPreset) {
        self.view = view;
        self.viewport.set_view_preset(view);
    }

    fn open_color_dialog(&mut self) {
        let [r, g, b] = self
            .viewport
            .color()
            .map(|c| (c.clamp(0.0, 1.0) * 255.0) as u8);
        let c = Color32::from_rgb(r, g, b);
        self.color_dialog = Some((c, c));
    }

    fn apply_color(&mut self, c: Color32) {
        let [r, g, b, _] = c.to_array();
        self.viewport.set_color([r, g, b].map(|v| v as f32 / 255.0));
    }

    fn copy_path(&mut self, path: String) {
        self.ctx.copy_text(path.clone());
        self.status = text::path_copied(&path);
    }

    // -----------------------------------------------------------------------------------------
    // Background results

    fn pump(&mut self) {
        if let Some(out) = self.loader.poll() {
            self.on_model_loaded(out);
        }
        if std::mem::take(&mut self.folder.loaded_status_pending) {
            self.status = text::folder_loaded(self.folder.file_count);
        }
        let ppp = self.ctx.pixels_per_point();
        self.grid.pump(ppp);
        for event in self.service.poll() {
            self.last_thumb_event = Instant::now();
            match event {
                ThumbEvent::Queued { total } => {
                    self.grid.generating = total > 0;
                    if total > 0 {
                        self.status = text::generating(total);
                        self.grid.progress = Some((0, total));
                    }
                }
                ThumbEvent::Ready { path, image } => self.grid.on_ready(path, image, ppp),
                ThumbEvent::Progress {
                    done,
                    total,
                    current,
                } => {
                    self.grid.progress = Some((done, total));
                    self.status = text::rendering(done + 1, total, &current);
                }
                ThumbEvent::Finished { total } => {
                    self.grid.progress = None;
                    self.grid.generating = false;
                    if total > 0 {
                        self.status = text::all_ready(total, self.grid.len());
                    }
                }
            }
        }
    }

    fn settled(&self) -> bool {
        !self.loader.is_loading()
            && !self.service.is_busy()
            && self.grid.progress.is_none()
            && !self.grid.is_busy()
            && !self.folder.loaded_status_pending
            && self.last_thumb_event.elapsed() >= QUIET_PERIOD
    }

    // -----------------------------------------------------------------------------------------
    // Layout

    /// The sidebar's minimum width: Qt's `minimumSizeHint`, which grows with metadata values
    /// that cannot wrap (e.g. the elided path).
    fn sidebar_min_width(&self, ui: &Ui) -> f32 {
        let chrome = 2.0 * (SIDEBAR_MARGIN + widgets::GROUP_INSET_SIDE);
        (self.meta.min_width(ui) + chrome).ceil().max(MIN_SIDEBAR_W)
    }

    /// Qt never lets the window be narrower than its layout's minimum and grows it when the
    /// minimum increases (e.g. a long unbreakable metadata value); `window` is the current
    /// inner size. On the first frame the window grows to `preferred` instead when that is
    /// wider, so it does not open at exactly the minimum, where the splitter has no room
    /// to move.
    fn enforce_min_window_width(&mut self, width: f32, preferred: f32, window: Vec2) {
        let width = width.ceil();
        if width == self.min_window_w {
            return;
        }
        let first = self.min_window_w == 0.0;
        self.min_window_w = width;
        self.ctx
            .send_viewport_cmd(ViewportCommand::MinInnerSize(vec2(width, MIN_WINDOW_H)));
        // The OS only applies the minimum to user resizes, so grow the window explicitly.
        let target = if first {
            width.max(preferred.ceil())
        } else {
            width
        };
        if window.x + 0.5 < target {
            self.ctx
                .send_viewport_cmd(ViewportCommand::InnerSize(vec2(target, window.y)));
        }
    }

    fn sidebar_ui(&mut self, ui: &mut Ui, rect: Rect, frame: &eframe::Frame) {
        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, theme::SIDEBAR_BG);
        painter.vline(
            rect.right() - 0.5,
            rect.y_range(),
            Stroke::new(1.0, theme::BORDER),
        );
        // Margins 15/20/15/15.
        let inner = Rect::from_min_max(
            rect.min + vec2(SIDEBAR_MARGIN, 20.0),
            rect.max - vec2(SIDEBAR_MARGIN, 15.0),
        );

        // Header: ⚡ + title (row spacing 15, inherited from the sidebar layout).
        let title =
            painter.layout_no_wrap("STL 3D View".into(), fonts::bold(16.0), theme::TEXT_STRONG);
        let cy = inner.top() + HEADER_H / 2.0;
        emoji::paint(
            ui,
            icons::BOLT,
            20.0,
            pos2(inner.left() + HEADER_BOLT_W / 2.0, cy),
            theme::ACCENT,
        );
        let title_pos = pos2(
            inner.left() + HEADER_BOLT_W + SECTION_SPACING,
            cy - title.size().y / 2.0,
        );
        painter.galley(title_pos.round(), title, theme::TEXT_STRONG);

        // Details group pinned to the bottom, browser group fills the rest.
        let meta_layout = self
            .meta
            .layout(ui, inner.width() - 2.0 * widgets::GROUP_INSET_SIDE);
        let meta_h = meta_layout.height + widgets::group_chrome_height();
        let browser_top = inner.top() + HEADER_H + SECTION_SPACING;
        let meta_top = (inner.bottom() - meta_h).max(browser_top + 160.0 + SECTION_SPACING);
        let meta_rect = Rect::from_min_max(
            pos2(inner.left(), meta_top),
            pos2(inner.right(), meta_top + meta_h),
        );
        let browser_rect = Rect::from_min_max(
            pos2(inner.left(), browser_top),
            pos2(inner.right(), meta_top - SECTION_SPACING),
        );

        let clip = ui.clip_rect().intersect(rect);
        let mut side = ui.new_child(UiBuilder::new().max_rect(rect).id_salt("sidebar"));
        side.set_clip_rect(clip);
        self.browser_group(&mut side, browser_rect, frame);
        let content = widgets::group_box(&side, meta_rect, &self.meta.title);
        if let Some(path) = self.meta.paint(&mut side, content, meta_layout) {
            self.copy_path(path);
        }
    }

    fn browser_group(&mut self, ui: &mut Ui, rect: Rect, frame: &eframe::Frame) {
        let c = widgets::group_box(ui, rect, "FileSystem Browser");

        // Row 1: path edit | ↑ | Browse...
        let browse_style = ButtonStyle::compact(11.0, vec2(8.0, 4.0));
        let up_style = ButtonStyle::compact(14.0, vec2(4.0, 4.0)).width(28.0);
        let browse = widgets::button_size(ui, "Browse...", &browse_style);
        let up = widgets::button_size(ui, "\u{2191}", &up_style);
        let edit_h: f32 = 25.0;
        let row_h = edit_h.max(browse.y).max(up.y);
        let cy = c.top() + row_h / 2.0;
        let browse_rect = Rect::from_center_size(pos2(c.right() - browse.x / 2.0, cy), browse);
        let up_rect =
            Rect::from_center_size(pos2(browse_rect.left() - ROW_SPACING - up.x / 2.0, cy), up);
        let edit_rect = Rect::from_min_max(
            pos2(c.left(), cy - edit_h / 2.0),
            pos2(up_rect.left() - ROW_SPACING, cy + edit_h / 2.0),
        );
        self.path_edit(ui, edit_rect);
        let up_resp = widgets::button_at(ui, up_rect, Id::new("up_button"), "\u{2191}", &up_style);
        if up_resp.on_hover_text(text::UP_TOOLTIP).clicked() {
            self.navigate_up();
        }
        if widgets::button_at(
            ui,
            browse_rect,
            Id::new("browse_button"),
            "Browse...",
            &browse_style,
        )
        .clicked()
        {
            self.browse(frame);
        }

        // Row 2: Show Images & Media.
        let font = fonts::regular(12.0);
        let label = "Show Images & Media";
        let size = widgets::checkbox_size(ui, label, &font);
        let cb_rect = Rect::from_min_size(pos2(c.left(), c.top() + row_h + ROW_SPACING), size);
        let mut on = self.show_media;
        if widgets::checkbox_at(ui, cb_rect, Id::new("show_media"), &mut on, label, &font) {
            self.set_show_media(on);
        }

        // Tree.
        let tree_rect = Rect::from_min_max(pos2(c.left(), cb_rect.bottom() + ROW_SPACING), c.max);
        if tree_rect.height() > 20.0 {
            for action in self.tree.show(ui, tree_rect) {
                self.on_tree_action(action);
            }
        }
    }

    fn path_edit(&mut self, ui: &mut Ui, rect: Rect) {
        let id = Id::new(PATH_EDIT_ID);
        // A modal dialog deactivates the main window, so Qt draws the edit unfocused.
        let modal_open = self.error.is_some() || self.color_dialog.is_some();
        let focused = !modal_open && ui.memory(|m| m.has_focus(id));
        if std::mem::take(&mut self.path_cursor_to_end) {
            // QLineEdit::setText leaves the cursor at the end, scrolled into view.
            let mut state = egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
            let end = egui::text::CCursor::new(self.path_text.chars().count());
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(end)));
            state.store(ui.ctx(), id);
        }
        let border = if focused {
            theme::ACCENT
        } else {
            theme::BORDER_SUBTLE
        };
        ui.painter().rect(
            rect,
            egui::CornerRadius::same(4),
            theme::PANEL_BG,
            Stroke::new(1.0, border),
            egui::StrokeKind::Inside,
        );
        let font = fonts::bold(11.0);
        // Text area inside the 1 px border and `padding: 3px 6px`; the editor is clipped to it.
        let inner = rect.shrink2(vec2(7.0, 4.0));
        // Unfocused, Qt shows the end of a long path; egui would show its start, so the
        // idle text is painted by hand on top of a transparent editor.
        let color = if focused {
            theme::PATH_EDIT_FOCUS_TEXT
        } else {
            Color32::TRANSPARENT
        };
        let edit = TextEdit::singleline(&mut self.path_text)
            .id(id)
            .font(font.clone())
            .text_color(color)
            .frame(egui::Frame::NONE)
            .margin(Margin::ZERO)
            .desired_width(inner.width())
            .vertical_align(egui::Align::Center)
            .hint_text(
                egui::RichText::new(text::PATH_PLACEHOLDER)
                    .color(theme::TEXT_MUTED)
                    .font(font.clone()),
            );
        let mut edit_ui = ui.new_child(UiBuilder::new().max_rect(inner).id_salt("path_edit_area"));
        edit_ui.set_clip_rect(inner.intersect(ui.clip_rect()));
        caret_blink(&mut edit_ui, focused, self.caret_epoch);
        let resp = edit_ui.put(inner, edit);
        if resp.changed() {
            self.path_dirty = true;
        }
        let interacted = ui.input(|i| {
            i.events.iter().any(|e| {
                matches!(
                    e,
                    Event::Key { pressed: true, .. } | Event::PointerButton { pressed: true, .. }
                )
            })
        });
        if resp.gained_focus() || resp.changed() || (focused && interacted) {
            self.caret_epoch = ui.input(|i| i.time);
        }
        if !focused && !self.path_text.is_empty() {
            let g = ui
                .painter()
                .layout_no_wrap(self.path_text.clone(), font, theme::TEXT_MUTED);
            let x = if g.size().x > inner.width() {
                inner.right() - g.size().x
            } else {
                inner.left()
            };
            let pos = pos2(x, inner.center().y - g.size().y / 2.0);
            ui.painter()
                .with_clip_rect(inner.intersect(ui.clip_rect()))
                .galley(pos.round(), g, theme::TEXT_MUTED);
        }
        if resp.lost_focus() {
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            if enter || self.path_dirty {
                let valid = self.navigate_typed();
                // egui drops focus on Enter; Python clears it only after a valid path.
                if enter && !valid {
                    ui.memory_mut(|m| m.request_focus(id));
                }
            }
        }
    }

    fn right_ui(&mut self, ui: &mut Ui, rect: Rect) {
        let deck = Rect::from_min_max(pos2(rect.left(), rect.bottom() - DECK_H), rect.max);
        let page = Rect::from_min_max(rect.min, pos2(rect.right(), deck.top()));
        let mut page_ui = ui.new_child(UiBuilder::new().max_rect(page).id_salt("page"));
        page_ui.set_clip_rect(ui.clip_rect().intersect(page));
        match self.page {
            Page::Viewport => {
                let view = self.viewport.show(&mut page_ui).rect;
                if self.loading.is_none()
                    && let Some(z) = self.viewport.model().map(|m| m.bounds[2])
                    && self.zslider.show(&mut page_ui, view, z)
                {
                    self.viewport.set_clip_z(self.zslider.range(z));
                }
                if let Some(msg) = &self.loading {
                    overlay::show(&mut page_ui, page, msg);
                }
            }
            Page::Image => {
                page_ui.painter().rect_filled(page, 0.0, theme::VIEWER_BG);
                self.image_viewer.show(&mut page_ui);
            }
            Page::Video => {
                page_ui.painter().rect_filled(page, 0.0, theme::VIEWER_BG);
                self.video.show(&mut page_ui);
            }
            Page::Grid => {
                if let Some(path) = self.grid.show(&mut page_ui, page) {
                    self.on_thumbnail_clicked(path);
                }
                // Re-sorted while thumbnails are still being made: make them in the new
                // order (cached ones come straight back).
                if let Some(paths) = self.grid.take_resorted()
                    && self.service.is_busy()
                {
                    self.service.start_folder(paths);
                    self.grid.generating = true;
                }
            }
        }
        self.deck_ui(ui, deck);
    }

    fn deck_ui(&mut self, ui: &mut Ui, rect: Rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, theme::SIDEBAR_BG);
        painter.vline(
            rect.left() + 0.5,
            rect.y_range(),
            Stroke::new(1.0, theme::BORDER),
        );
        let mut deck = ui.new_child(UiBuilder::new().max_rect(rect).id_salt("deck"));
        deck.set_clip_rect(ui.clip_rect().intersect(rect));
        let ui = &mut deck;
        let style = ButtonStyle::normal();
        let layout = DeckLayout::new(ui, rect.left() + DECK_MARGIN, rect.center().y);

        let (label_rect, label) = layout.view_label;
        ui.painter().galley(label_rect.min, label, theme::TEXT);
        let views = view_items();
        let current = VIEWS.iter().position(|&(_, v)| v == self.view).unwrap_or(0);
        // Picking the current view again re-applies it (resets an orbited camera).
        if let Some(i) = widgets::combo_pick_at(
            ui,
            layout.view_combo,
            Id::new("view_combo"),
            current,
            &views,
        ) {
            self.set_view(VIEWS[i].1);
        }
        for r in layout.separators {
            widgets::vseparator(ui, r);
        }
        let (label_rect, label) = layout.shading_label;
        ui.painter().galley(label_rect.min, label, theme::TEXT);
        let items = shading_items();
        let current = ShadingMode::ALL
            .iter()
            .position(|&m| m == self.viewport.shading())
            .unwrap_or(0);
        if let Some(i) =
            widgets::combo_at(ui, layout.combo, Id::new("shading_combo"), current, &items)
        {
            self.viewport.set_shading(ShadingMode::ALL[i]);
        }
        let accent = ButtonStyle::accent();
        if widgets::button_at(
            ui,
            layout.color,
            Id::new("model_color"),
            MODEL_COLOR,
            &accent,
        )
        .clicked()
        {
            self.open_color_dialog();
        }
        let mut rotate = self.viewport.auto_rotate();
        let font = fonts::regular(12.0);
        if widgets::checkbox_at(
            ui,
            layout.rotate,
            Id::new("auto_rotate"),
            &mut rotate,
            AUTO_ROTATE,
            &font,
        ) {
            self.viewport.set_auto_rotate(rotate);
        }
        for (r, (label, step)) in layout.steps.into_iter().zip(STEPS) {
            if widgets::button_at(ui, r, Id::new(("step", step)), label, &style).clicked() {
                self.step_file(step);
            }
        }
        let large = self.grid.is_large();
        let size_label = if large { SMALL_ICONS } else { LARGE_ICONS };
        if widgets::button_at(
            ui,
            layout.icon_size,
            Id::new("icon_size"),
            size_label,
            &style,
        )
        .on_hover_text("Toggle thumbnail preview size")
        .clicked()
        {
            self.grid.set_large(!large);
        }
        let back = style.clone().disabled(!self.can_go_back());
        if widgets::button_at(ui, layout.back, Id::new("back_to_folder"), BACK, &back)
            .on_hover_text("Back to the folder's thumbnails")
            .clicked()
        {
            self.back_to_folder();
        }
    }

    fn status_ui(&self, ui: &Ui, rect: Rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, theme::VIEWER_BG);
        painter.hline(
            rect.x_range(),
            rect.top() + 0.5,
            Stroke::new(1.0, theme::SPLITTER),
        );
        let body = Rect::from_min_max(
            pos2(rect.left() + 6.0, rect.top() + 1.0),
            pos2(rect.right() - 6.0, rect.bottom()),
        );
        let g = widgets::elided_right(
            ui,
            &self.status,
            fonts::regular(12.0),
            theme::TEXT,
            body.width(),
        );
        painter.galley(
            pos2(body.left(), body.center().y - g.size().y / 2.0).round(),
            g,
            theme::TEXT,
        );
    }

    fn splitter(&mut self, ui: &mut Ui, handle: Rect, main: Rect, left_min: f32, right_min: f32) {
        ui.painter().rect_filled(handle, 0.0, theme::SPLITTER);
        let resp = ui.interact(
            handle.expand2(vec2(2.0, 0.0)),
            Id::new("sidebar_splitter"),
            Sense::drag(),
        );
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if resp.dragged()
            && let Some(p) = resp.interact_pointer_pos()
        {
            let avail = main.width() - SPLITTER_W;
            let left = (p.x - main.left() - SPLITTER_W / 2.0)
                .min(avail - right_min)
                .max(left_min);
            self.split = Some([left, avail - left]);
        }
    }

    fn dialogs(&mut self) {
        let ctx = self.ctx.clone();
        if let Some(msg) = &self.error
            && !dialogs::error_dialog(&ctx, msg)
        {
            self.error = None;
        }
        if let Some((mut working, initial)) = self.color_dialog.take() {
            match dialogs::color_dialog(&ctx, &mut working, initial) {
                ColorDialogResult::Open => self.color_dialog = Some((working, initial)),
                ColorDialogResult::Accepted(c) => self.apply_color(c),
                ColorDialogResult::Cancelled => {}
            }
        }
    }

    /// The settings as they are now (see [`Settings`]).
    fn current_settings(&self) -> Settings {
        let [r, g, b] = self
            .viewport
            .color()
            .map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
        let window_size = self
            .ctx
            .input(|i| i.viewport().inner_rect)
            .map(|r| [r.width().round(), r.height().round()]);
        Settings {
            model_color: Some([r, g, b]),
            tree_root: Some(self.tree.root().to_path_buf()),
            last_folder: self.grid_folder.clone(),
            show_media: self.show_media,
            large_icons: self.grid.is_large(),
            sort: Some(self.grid.sort_order().key().to_string()),
            window_size,
            split: self.split,
        }
    }

    fn save_settings(&mut self) {
        let current = self.current_settings();
        self.persist.update(current);
        if self.persist.is_pending() {
            self.ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    fn shutdown(&mut self) {
        self.persist.update(self.current_settings());
        self.persist.flush();
        self.service.cancel_folder();
        self.service.cancel_precache();
        self.loader.cancel();
        self.video.stop();
    }
}

impl eframe::App for StlViewerApp {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        self.record_frame(frame);
        self.pump();
        self.run_script();

        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, theme::WINDOW_BG);
        let status_rect = Rect::from_min_max(pos2(full.left(), full.bottom() - STATUS_H), full.max);
        let main = Rect::from_min_max(full.min, pos2(full.right(), status_rect.top()));
        let (left_min, right_min) = (self.sidebar_min_width(ui), DeckLayout::min_width(ui));
        // Default sidebar width next to the narrowest right panel.
        let preferred = DEFAULT_SPLIT[0].max(left_min) + SPLITTER_W + right_min;
        self.enforce_min_window_width(left_min + SPLITTER_W + right_min, preferred, full.size());
        let sizers = *self
            .split
            .get_or_insert([DEFAULT_SPLIT[0], DEFAULT_SPLIT[1].max(right_min)]);
        let avail = main.width() - SPLITTER_W;
        let sidebar_w = split_width(avail, sizers, left_min, right_min);
        let sidebar = Rect::from_min_size(main.min, vec2(sidebar_w, main.height()));
        let handle = Rect::from_min_size(
            pos2(sidebar.right(), main.top()),
            vec2(SPLITTER_W, main.height()),
        );
        let right = Rect::from_min_max(pos2(handle.right(), main.top()), main.max);

        self.sidebar_ui(ui, sidebar, frame);
        self.right_ui(ui, right);
        self.splitter(ui, handle, main, left_min, right_min);
        self.status_ui(ui, status_rect);
        self.dialogs();
        self.save_settings();
        self.handle_screenshots(ui);
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let Some(batch) = self.harness.input.pop_front() {
            raw_input.events.extend(batch);
            self.harness.input_injected = true;
            ctx.request_repaint();
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::WINDOW_BG.to_normalized_gamma_f32()
    }

    fn on_exit(&mut self) {
        self.shutdown();
    }
}

/// Camera views in the "View:" dropdown.
const VIEWS: [(&str, ViewPreset); 7] = [
    ("Isometric", ViewPreset::Isometric),
    ("Top", ViewPreset::Top),
    ("Front", ViewPreset::Front),
    ("Right", ViewPreset::Right),
    ("Back", ViewPreset::Back),
    ("Left", ViewPreset::Left),
    ("Bottom", ViewPreset::Bottom),
];
const STEPS: [(&str, isize); 2] = [("\u{25C0} Prev", -1), ("Next \u{25B6}", 1)];
const MODEL_COLOR: &str = "Model Color";
const LARGE_ICONS: &str = "Large Icons";
const SMALL_ICONS: &str = "Small Icons";
const BACK: &str = "\u{25C0} Back";
const AUTO_ROTATE: &str = "Auto-Rotate";

fn view_items() -> Vec<&'static str> {
    VIEWS.iter().map(|&(label, _)| label).collect()
}

fn shading_items() -> Vec<&'static str> {
    ShadingMode::ALL.iter().map(|m| m.label()).collect()
}

/// Geometry of the control deck's row (a left-aligned `QHBoxLayout`, spacing 20).
struct DeckLayout {
    view_label: (Rect, Arc<egui::Galley>),
    view_combo: Rect,
    separators: [Rect; 3],
    shading_label: (Rect, Arc<egui::Galley>),
    combo: Rect,
    color: Rect,
    rotate: Rect,
    steps: [Rect; 2],
    icon_size: Rect,
    back: Rect,
    /// Right edge of the last item.
    right: f32,
}

impl DeckLayout {
    fn new(ui: &Ui, left: f32, cy: f32) -> Self {
        let style = ButtonStyle::normal();
        let bh = widgets::button_size(ui, "ISO", &style).y;
        let mut x = left;
        let mut place = |size: Vec2, gap: f32| {
            let r = Rect::from_min_size(pos2(x, (cy - size.y / 2.0).round()), size);
            x += size.x + gap;
            r
        };
        let font = fonts::regular(13.0);
        let view_text = ui
            .painter()
            .layout_no_wrap("View:".into(), font.clone(), theme::TEXT);
        let view_w = widgets::qt_text_width(ui, "View:", &font);
        let view_label = place(vec2(view_w, view_text.size().y), LABEL_GAP);
        let view_combo = place(widgets::combo_size(ui, &view_items()), DECK_SPACING);
        let sep = vec2(widgets::SEPARATOR_W, bh);
        let sep0 = place(sep, DECK_SPACING);
        let label = ui
            .painter()
            .layout_no_wrap("Shading:".into(), font.clone(), theme::TEXT);
        let label_w = widgets::qt_text_width(ui, "Shading:", &font);
        let label_rect = place(vec2(label_w, label.size().y), DECK_SPACING);
        let combo = place(widgets::combo_size(ui, &shading_items()), DECK_SPACING);
        let sep1 = place(sep, DECK_SPACING);
        let color = place(
            widgets::button_size(ui, MODEL_COLOR, &ButtonStyle::accent()),
            DECK_SPACING,
        );
        let rotate = place(
            widgets::checkbox_size(ui, AUTO_ROTATE, &fonts::regular(12.0)),
            DECK_SPACING,
        );
        let sep2 = place(sep, DECK_SPACING);
        let steps = STEPS.map(|(label, _)| {
            place(
                vec2(widgets::button_size(ui, label, &style).x, bh),
                DECK_SPACING,
            )
        });
        // Fixed width so the deck does not shift when the label flips.
        let size_w = [LARGE_ICONS, SMALL_ICONS]
            .map(|l| widgets::button_size(ui, l, &style).x)
            .into_iter()
            .fold(0.0, f32::max);
        let icon_size = place(vec2(size_w, bh), DECK_SPACING);
        let back = place(
            vec2(widgets::button_size(ui, BACK, &style).x, bh),
            DECK_SPACING,
        );
        Self {
            view_label: (view_label, view_text),
            view_combo,
            separators: [sep0, sep1, sep2],
            shading_label: (label_rect, label),
            combo,
            color,
            rotate,
            steps,
            icon_size,
            right: back.right(),
            back,
        }
    }

    /// The deck's minimum width: border, margins and items (the right panel's minimum).
    fn min_width(ui: &Ui) -> f32 {
        Self::new(ui, 0.0, 0.0).right + 1.0 + 2.0 * DECK_MARGIN
    }
}

/// Sidebar width for `avail` (window width minus the handle): QSplitter shares the space in
/// proportion to its sizer widths, honouring the right panel's minimum first and the
/// sidebar's last.
fn split_width(avail: f32, sizers: [f32; 2], left_min: f32, right_min: f32) -> f32 {
    let total = (sizers[0] + sizers[1]).max(1.0);
    let left = avail * sizers[0] / total;
    left.min(avail - right_min).max(left_min).round()
}

/// Root folder and optional file to open from the `[PATH]` argument.
fn startup_target(path: Option<&Path>) -> (PathBuf, Option<PathBuf>) {
    let default_root = PathBuf::from("C:\\");
    let Some(path) = path else {
        return (default_root, None);
    };
    let path = absolute(path);
    if path.is_dir() {
        (path, None)
    } else if path.is_file() {
        let parent = path.parent().map(Path::to_path_buf).unwrap_or(default_root);
        (parent, Some(path))
    } else {
        log::warn!("{} does not exist; starting at C:\\", path.display());
        (default_root, None)
    }
}

/// Blink the path edit's caret like a QLineEdit with one repaint per toggle. egui's own
/// blink timer tends to fire slightly early and then repaints the whole window several
/// times per toggle (about 13 frames/s while the edit is focused).
fn caret_blink(ui: &mut Ui, focused: bool, epoch: f64) {
    const HALF_PERIOD: f64 = 0.5;
    const LATE: f64 = 0.02;
    ui.visuals_mut().text_cursor.blink = false;
    if !focused || !ui.input(|i| i.focused) {
        return;
    }
    let phase = (ui.input(|i| i.time) - epoch).rem_euclid(2.0 * HALF_PERIOD);
    if phase >= HALF_PERIOD {
        ui.visuals_mut().text_cursor.stroke.color = Color32::TRANSPARENT;
    }
    let next_toggle = HALF_PERIOD - phase % HALF_PERIOD;
    ui.ctx()
        .request_repaint_after_secs((next_toggle + LATE) as f32);
}

/// `X:\` for a bare drive specifier `X:`.
fn drive_root(text: &str) -> Option<PathBuf> {
    match text.as_bytes() {
        [letter, b':'] if letter.is_ascii_alphabetic() => Some(PathBuf::from(format!("{text}\\"))),
        _ => None,
    }
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

fn display_path(path: &Path) -> String {
    text::normpath(&path.to_string_lossy())
}

/// The requested (or default) cache; falls back to an in-memory database (`:memory:` is
/// understood by SQLite) so a locked or unwritable cache never stops the app.
fn open_cache(path: Option<&Path>) -> Result<ThumbnailCache, String> {
    let primary = match path {
        Some(p) => ThumbnailCache::open(p),
        None => ThumbnailCache::open_default(),
    };
    primary.or_else(|e| {
        log::error!("thumbnail cache unavailable ({e}); using an in-memory one");
        ThumbnailCache::open(Path::new(":memory:"))
            .map_err(|e| format!("Cannot open the thumbnail cache: {e}"))
    })
}
