//! Lazy file-system tree (Python: `QFileSystemModel` + `QTreeView` with the root hidden).
//!
//! Shows the children of the root directory; folders expand lazily. Entries are all
//! non-hidden folders plus files that pass the filter (meshes always, media only when
//! "Show Images & Media" is on), folders first, each group in Qt's natural,
//! case-insensitive order ([`collate`](crate::collate)).

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use egui::{CornerRadius, EventFilter, Id, Key, Rect, ScrollArea, Sense, Ui, pos2, vec2};
use stlv_core::FileKind;
use stlv_core::files::{MEDIA_EXTS, MESH_EXTS};

use crate::watch::{self, DirWatcher};
use crate::{collate, emoji, fonts, icons, theme, widgets};

#[derive(Debug)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub kind: FileKind,
}

impl Entry {
    fn visible(&self, show_media: bool) -> bool {
        self.is_dir || self.kind.is_mesh() || (show_media && self.kind.is_media())
    }
}

/// Case-insensitive path equality (Windows file systems).
pub fn same_path(a: &Path, b: &Path) -> bool {
    a.as_os_str().len() == b.as_os_str().len() && eq_ci(a.as_os_str(), b.as_os_str())
        || norm_key(a) == norm_key(b)
}

fn eq_ci(a: &OsStr, b: &OsStr) -> bool {
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

fn norm_key(p: &Path) -> String {
    let s = crate::text::normpath(&p.to_string_lossy()).to_lowercase();
    s.trim_end_matches('\\').to_string()
}

/// Qt (no `QDir::Hidden` filter) leaves out items with the Hidden attribute only; the
/// System attribute alone does not hide anything.
#[cfg(windows)]
fn is_hidden(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
}

#[cfg(not(windows))]
fn is_hidden(_meta: &std::fs::Metadata) -> bool {
    false
}

fn is_listable_file(name: &str) -> bool {
    let ext = stlv_core::files::ext_lower(Path::new(name));
    MESH_EXTS.contains(&ext.as_str()) || MEDIA_EXTS.contains(&ext.as_str())
}

/// Directories and supported files (incl. media; the media filter is applied when
/// flattening) directly inside `dir`, hidden items excluded, in tree order.
pub fn read_listing(dir: &Path) -> io::Result<Vec<Arc<Entry>>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        if is_hidden(&meta) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        // Junctions / symlinks to folders count as folders.
        let is_dir = meta.is_dir() || (meta.file_type().is_symlink() && path.is_dir());
        if !is_dir && !is_listable_file(&name) {
            continue;
        }
        let kind = if is_dir {
            FileKind::Other
        } else {
            FileKind::from_path(&path)
        };
        out.push(Arc::new(Entry {
            name,
            path,
            is_dir,
            kind,
        }));
    }
    out.sort_by_cached_key(|e| (!e.is_dir, collate::sort_key(&e.name), e.name.clone()));
    Ok(out)
}

/// Files of `dir` that the tree shows (Prev / Next navigate among these), in
/// `stlv_core::files::sort_key` order.
pub fn visible_files(dir: &Path, show_media: bool) -> Vec<PathBuf> {
    let Ok(listing) = read_listing(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = listing
        .iter()
        .filter(|e| !e.is_dir && e.visible(show_media))
        .map(|e| e.path.clone())
        .collect();
    files.sort_by_cached_key(|p| stlv_core::files::sort_key(p));
    files
}

/// The neighbour of `current` in `files` (`step` = -1 or +1), without wrap-around.
pub fn step_file(files: &[PathBuf], current: &Path, step: isize) -> Option<PathBuf> {
    let idx = files.iter().position(|p| same_path(p, current))?;
    let target = idx.checked_add_signed(step)?;
    files.get(target).cloned()
}

#[derive(Clone)]
pub struct Row {
    pub entry: Arc<Entry>,
    pub depth: usize,
    pub expanded: bool,
}

#[derive(Debug, PartialEq)]
pub enum TreeAction {
    /// The selection changed to this row (click or keyboard): folder → grid, file → load.
    Select(PathBuf),
    /// Expand or collapse a folder: branch cell click, double-click, Right / Left keys.
    Toggle(PathBuf),
}

/// Result of a navigation key (`QTreeView::moveCursor`).
#[derive(Debug, PartialEq)]
enum Nav {
    Move(usize),
    Toggle(usize),
}

/// A folder's entries and its modification time when they were read.
struct Listing {
    entries: Vec<Arc<Entry>>,
    mtime: Option<SystemTime>,
}

pub struct FileTree {
    root: PathBuf,
    listings: HashMap<PathBuf, Listing>,
    expanded: HashSet<PathBuf>,
    selected: Option<PathBuf>,
    show_media: bool,
    rows: Vec<Row>,
    rows_dirty: bool,
    scroll_to_selected: bool,
    scroll_offset: f32,
    view_height: f32,
    focus_requested: bool,
    /// Type-ahead text and the input time of its last keystroke.
    search: String,
    search_time: f64,
    watcher: Option<DirWatcher>,
    /// The set of watched folders (root + expanded listings) changed.
    watch_dirty: bool,
}

pub const ROW_H: f32 = 33.0;
const INDENT: f32 = 30.0;
const TREE_PAD: f32 = 5.0;
/// Offset of the 16 px icon inside an item.
const ICON_X: f32 = 9.0;
/// Emoji font size whose folder glyph is about 16 px wide, like the shell's folder icon.
const FOLDER_EMOJI_SIZE: f32 = 15.0;
/// `QApplication::keyboardInputInterval`: keystrokes closer than this extend a type-ahead.
const TYPE_AHEAD_INTERVAL: f64 = 0.4;
const FOCUS_ID: &str = "file_tree";

impl FileTree {
    pub fn new(root: PathBuf, show_media: bool) -> Self {
        let mut t = Self {
            root: PathBuf::new(),
            listings: HashMap::new(),
            expanded: HashSet::new(),
            selected: None,
            show_media,
            rows: Vec::new(),
            rows_dirty: true,
            scroll_to_selected: false,
            scroll_offset: 0.0,
            view_height: 0.0,
            focus_requested: false,
            search: String::new(),
            search_time: f64::NEG_INFINITY,
            watcher: None,
            watch_dirty: false,
        };
        t.set_root(root);
        t
    }

    /// Keep the shown listings in sync with the disk, polling every `interval`; `wake` asks
    /// for a repaint when a watched folder changed.
    pub fn watch(&mut self, interval: Duration, wake: impl Fn() + Send + 'static) {
        self.watcher = Some(DirWatcher::new(interval, wake));
        self.watch_dirty = true;
    }

    /// Give the tree keyboard focus on the next frame (as a click on a row does).
    pub fn request_focus(&mut self) {
        self.focus_requested = true;
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn selected(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    pub fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.contains(path)
    }

    /// Change the root; expansion state and selection are kept (like `setRootIndex`).
    pub fn set_root(&mut self, root: PathBuf) {
        self.root = root;
        self.listings.clear();
        self.load_listing(&self.root.clone());
        self.scroll_offset = 0.0;
        self.rows_dirty = true;
    }

    pub fn set_show_media(&mut self, show: bool) {
        if self.show_media != show {
            self.show_media = show;
            self.rows_dirty = true;
        }
    }

    pub fn clear_selection(&mut self) {
        self.selected = None;
    }

    pub fn expand(&mut self, path: &Path) {
        // Re-read on every expansion so the listing reflects the disk.
        self.load_listing(path);
        self.expanded.insert(path.to_path_buf());
        self.rows_dirty = true;
    }

    pub fn collapse(&mut self, path: &Path) {
        self.expanded.remove(path);
        self.rows_dirty = true;
        self.watch_dirty = true;
    }

    fn load_listing(&mut self, dir: &Path) {
        // Stamped before reading, so a change during the read is still noticed.
        let mtime = watch::dir_mtime(dir);
        let entries = read_listing(dir).unwrap_or_else(|e| {
            log::warn!("cannot list {}: {e}", dir.display());
            Vec::new()
        });
        self.listings
            .insert(dir.to_path_buf(), Listing { entries, mtime });
        self.watch_dirty = true;
    }

    /// Re-read the listings that changed on disk (Qt inserts and removes rows live) and
    /// drop a selection that no longer exists; then tell the watcher what is shown.
    fn sync_with_disk(&mut self) {
        let Some(watcher) = &self.watcher else {
            return;
        };
        let changed = watcher.changed();
        if !changed.is_empty() {
            for dir in changed {
                if self.listings.contains_key(&dir) {
                    self.load_listing(&dir);
                }
            }
            self.rows_dirty = true;
            if self.selected.as_deref().is_some_and(|s| !s.exists()) {
                self.selected = None;
            }
        }
        if std::mem::take(&mut self.watch_dirty)
            && let Some(watcher) = &self.watcher
        {
            let watched = self
                .listings
                .iter()
                .filter(|(dir, _)| **dir == self.root || self.expanded.contains(*dir))
                .map(|(dir, l)| (dir.clone(), l.mtime))
                .collect();
            watcher.set_dirs(watched);
        }
    }

    /// Select `path` and make it visible: expand its ancestors below the root (without
    /// opening their folder grids) and scroll it into view. Path components are matched
    /// case-insensitively against the listings so the stored paths keep their on-disk case.
    pub fn select_and_reveal(&mut self, path: &Path) {
        let resolved = self.reveal(path).unwrap_or_else(|| path.to_path_buf());
        self.selected = Some(resolved);
        self.scroll_to_selected = true;
    }

    /// Expand the ancestors of `path` (no selection change); returns its on-disk spelling.
    pub fn reveal(&mut self, path: &Path) -> Option<PathBuf> {
        let rel = strip_prefix_ci(path, &self.root)?;
        let comps: Vec<&OsStr> = rel.iter().collect();
        let mut dir = self.root.clone();
        for (i, comp) in comps.iter().enumerate() {
            if !self.listings.contains_key(&dir) {
                self.load_listing(&dir);
            }
            let entry = self
                .listings
                .get(&dir)?
                .entries
                .iter()
                .find(|e| eq_ci(OsStr::new(&e.name), comp))?
                .clone();
            if i + 1 == comps.len() {
                return Some(entry.path.clone());
            }
            if !entry.is_dir {
                return None;
            }
            if !self.expanded.contains(&entry.path) {
                self.expand(&entry.path);
            }
            dir = entry.path.clone();
        }
        None
    }

    pub fn rows(&mut self) -> &[Row] {
        if self.rows_dirty {
            self.rebuild_rows();
        }
        &self.rows
    }

    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        // Iterative DFS keeping order: push children in reverse.
        let mut pending: Vec<(Arc<Entry>, usize)> = Vec::new();
        self.push_children(&self.root, 0, &mut pending);
        while let Some((entry, depth)) = pending.pop() {
            let expanded = entry.is_dir && self.expanded.contains(&entry.path);
            rows.push(Row {
                entry: entry.clone(),
                depth,
                expanded,
            });
            if expanded {
                if !self.listings.contains_key(&entry.path) {
                    self.load_listing(&entry.path);
                }
                self.push_children(&entry.path, depth + 1, &mut pending);
            }
        }
        self.rows = rows;
        self.rows_dirty = false;
    }

    fn push_children(&self, dir: &Path, depth: usize, pending: &mut Vec<(Arc<Entry>, usize)>) {
        if let Some(listing) = self.listings.get(dir) {
            for e in listing
                .entries
                .iter()
                .rev()
                .filter(|e| e.visible(self.show_media))
            {
                pending.push((e.clone(), depth));
            }
        }
    }

    /// Paint the tree and handle its mouse and keyboard input; returns the resulting actions
    /// in order (a keyboard frame can both move the selection and expand the new row).
    pub fn show(&mut self, ui: &mut Ui, rect: Rect) -> Vec<TreeAction> {
        let painter = ui.painter();
        painter.rect(
            rect,
            CornerRadius::same(6),
            theme::PANEL_BG,
            egui::Stroke::new(1.0, theme::BORDER_SUBTLE),
            egui::StrokeKind::Inside,
        );
        let inner = rect.shrink(1.0 + TREE_PAD);
        self.sync_with_disk();
        let view_h = if self.view_height > 0.0 {
            self.view_height
        } else {
            inner.height()
        };

        // The tree as a whole takes keyboard focus (QTreeView's StrongFocus); rows on top
        // of it handle the clicks, empty space focuses it.
        let focus = ui.interact(rect, Id::new(FOCUS_ID), Sense::click());
        if focus.clicked() || std::mem::take(&mut self.focus_requested) {
            focus.request_focus();
        }
        self.rows();
        let mut actions = Vec::new();
        if focus.has_focus() {
            let filter = EventFilter {
                horizontal_arrows: true,
                vertical_arrows: true,
                ..Default::default()
            };
            ui.memory_mut(|m| m.set_focus_lock_filter(focus.id, filter));
            let page = ((view_h / ROW_H).floor() as usize).max(1);
            actions = self.handle_keys(ui, page);
        }

        let rows_len = self.rows.len();
        let content_h = rows_len as f32 * ROW_H;
        let mut offset = None;
        if std::mem::take(&mut self.scroll_to_selected)
            && let Some(idx) = self.current_row()
        {
            let top = idx as f32 * ROW_H;
            if top < self.scroll_offset {
                offset = Some(top);
            } else if top + ROW_H > self.scroll_offset + view_h {
                offset = Some(top + ROW_H - view_h);
            }
        }

        let mut clicked = None;
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).id_salt("file_tree"));
        let mut area = ScrollArea::vertical()
            .id_salt("tree_scroll")
            .auto_shrink([false, false]);
        if let Some(o) = offset {
            area = area.vertical_scroll_offset(o);
        }
        let out = area.show_viewport(&mut child, |ui, viewport| {
            ui.set_height(content_h);
            ui.set_width(ui.available_width());
            let first = (viewport.min.y / ROW_H).floor().max(0.0) as usize;
            let last = ((viewport.max.y / ROW_H).ceil() as usize).min(rows_len);
            let origin = ui.max_rect().min;
            let width = ui.max_rect().width();
            for i in first..last {
                let r = Rect::from_min_size(
                    pos2(origin.x, origin.y + i as f32 * ROW_H),
                    vec2(width, ROW_H),
                );
                if let Some(a) = self.paint_row(ui, r, &self.rows[i]) {
                    clicked = Some(a);
                }
            }
        });
        self.scroll_offset = out.state.offset.y;
        self.view_height = out.inner_rect.height();

        if let Some(action) = clicked {
            focus.request_focus();
            // Qt only reports a click that changes the selection.
            if let TreeAction::Select(path) = &action {
                if self.selected.as_deref().is_some_and(|s| same_path(s, path)) {
                    return actions;
                }
                self.selected = Some(path.clone());
            }
            actions.push(action);
        }
        if actions.iter().any(|a| matches!(a, TreeAction::Toggle(_))) {
            // The rows change once the app has applied the toggle.
            ui.ctx().request_repaint();
        }
        actions
    }

    /// Index of the selected row, or of its closest visible ancestor.
    fn current_row(&self) -> Option<usize> {
        let sel = self.selected.as_deref()?;
        sel.ancestors()
            .take_while(|p| !same_path(p, &self.root))
            .find_map(|p| self.rows.iter().position(|r| same_path(&r.entry.path, p)))
    }

    /// Navigation keys and type-ahead while the tree has focus (`QTreeView::keyPressEvent`):
    /// the selection follows the current row, so moving it loads the file or opens the
    /// folder grid; Right / Left expand, descend, collapse or go to the parent.
    fn handle_keys(&mut self, ui: &Ui, page: usize) -> Vec<TreeAction> {
        let (events, now) = ui.input(|i| (i.events.clone(), i.time));
        let keyboard = |e: &egui::Event| {
            matches!(
                e,
                egui::Event::Key { pressed: true, .. } | egui::Event::Text(_)
            )
        };
        if !events.iter().any(keyboard) {
            return Vec::new();
        }
        let start = self.current_row();
        let mut current = start;
        let mut toggle = None;
        for event in events {
            match event {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } if !(modifiers.ctrl || modifiers.alt || modifiers.command) => {
                    match self.navigate(key, current, page) {
                        Some(Nav::Move(i)) => current = Some(i),
                        Some(Nav::Toggle(i)) => {
                            // The rows change with the toggle; later keys wait a frame.
                            current = Some(i);
                            toggle = Some(i);
                            break;
                        }
                        None => {}
                    }
                }
                egui::Event::Text(text) => {
                    if let Some(i) = self.type_ahead(&text, now, current) {
                        current = Some(i);
                    }
                }
                _ => {}
            }
        }
        let mut actions = Vec::new();
        if let Some(i) = current.filter(|_| current != start) {
            let path = self.rows[i].entry.path.clone();
            self.selected = Some(path.clone());
            self.scroll_to_selected = true;
            actions.push(TreeAction::Select(path));
        }
        if let Some(i) = toggle {
            actions.push(TreeAction::Toggle(self.rows[i].entry.path.clone()));
        }
        actions
    }

    /// The row a navigation key moves to from `current` (`QTreeView::moveCursor` with the
    /// Windows style, whose Right / Left keys also step into children and up to parents).
    fn navigate(&self, key: Key, current: Option<usize>, page: usize) -> Option<Nav> {
        use Key::{ArrowDown, ArrowLeft, ArrowRight, ArrowUp, End, Home, PageDown, PageUp};
        let last = self.rows.len().checked_sub(1)?;
        let nav_key = matches!(
            key,
            ArrowUp | ArrowDown | ArrowLeft | ArrowRight | Home | End | PageUp | PageDown
        );
        let Some(i) = current else {
            // Without a current row every movement starts at the first one.
            return nav_key.then_some(Nav::Move(0));
        };
        let row = &self.rows[i];
        Some(match key {
            ArrowUp => Nav::Move(i.saturating_sub(1)),
            ArrowDown => Nav::Move((i + 1).min(last)),
            Home => Nav::Move(0),
            End => Nav::Move(last),
            PageUp => Nav::Move(i.saturating_sub(page)),
            PageDown => Nav::Move((i + page).min(last)),
            ArrowRight if row.entry.is_dir && !row.expanded => Nav::Toggle(i),
            ArrowRight if self.rows.get(i + 1).is_some_and(|r| r.depth > row.depth) => {
                Nav::Move(i + 1)
            }
            ArrowLeft if row.expanded => Nav::Toggle(i),
            ArrowLeft if row.depth > 0 => {
                Nav::Move(self.rows[..i].iter().rposition(|r| r.depth < row.depth)?)
            }
            _ => return None,
        })
    }

    /// `QTreeView::keyboardSearch`: keystrokes within [`TYPE_AHEAD_INTERVAL`] build a prefix
    /// that is matched case-insensitively from the current row on, wrapping around. A new
    /// search, or the same letter repeated, starts after the current row.
    fn type_ahead(&mut self, text: &str, now: f64, current: Option<usize>) -> Option<usize> {
        if self.rows.is_empty() || text.is_empty() {
            return None;
        }
        let text = text.to_lowercase();
        let mut skip = false;
        if now - self.search_time > TYPE_AHEAD_INTERVAL {
            self.search.clear();
            skip = current.is_some();
        }
        self.search_time = now;
        self.search.push_str(&text);
        let mut chars = self.search.chars();
        let first = chars.next();
        let same_key = self.search.chars().count() > 1 && chars.all(|c| Some(c) == first);
        let needle = if same_key {
            skip = true;
            text.as_str()
        } else {
            self.search.as_str()
        };
        let n = self.rows.len();
        let start = current.map_or(0, |i| i + usize::from(skip));
        (0..n)
            .map(|k| (start + k) % n)
            .find(|&i| self.rows[i].entry.name.to_lowercase().starts_with(needle))
    }

    fn paint_row(&self, ui: &mut Ui, r: Rect, row: &Row) -> Option<TreeAction> {
        let entry = &row.entry;
        let item_left = r.left() + (row.depth as f32 + 1.0) * INDENT;
        let id = Id::new(("tree_row", &entry.path));
        let resp = ui.interact(r, id, Sense::CLICK);
        let selected = self
            .selected
            .as_deref()
            .is_some_and(|s| same_path(s, &entry.path));
        let hovered = resp.hovered();

        let item_rect = Rect::from_min_max(pos2(item_left, r.top()), r.max);
        let painter = ui.painter();
        if selected {
            // The Windows 11 style paints the selected row's branch area too, with its small
            // accent-blue selection mark; the stylesheet adds the cyan bar on the item.
            let branch =
                Rect::from_min_max(pos2(r.left() + 1.5, r.top()), pos2(item_left, r.bottom()));
            if branch.width() > 0.0 {
                painter.rect_filled(
                    branch,
                    CornerRadius {
                        nw: 4,
                        sw: 4,
                        ne: 0,
                        se: 0,
                    },
                    theme::TREE_SELECTED_BG,
                );
                let mark =
                    Rect::from_center_size(pos2(r.left() + 4.0, r.center().y), vec2(2.0, 15.0));
                painter.rect_filled(mark, CornerRadius::same(1), theme::WIN_ACCENT);
            }
            painter.rect_filled(item_rect, CornerRadius::same(4), theme::TREE_SELECTED_BG);
            let bar = Rect::from_min_size(item_rect.min, vec2(3.0, item_rect.height()));
            painter.rect_filled(bar, CornerRadius::same(1), theme::ACCENT);
        } else if hovered {
            painter.rect_filled(item_rect, CornerRadius::same(4), theme::TREE_HOVER_BG);
        }

        // 16 px icon 9 px into the item (the selected item's border pushes it 3 px right).
        let shift = if selected { 3.0 } else { 0.0 };
        let icon_c = pos2(item_left + shift + ICON_X + 8.0, r.center().y);
        if entry.is_dir {
            emoji::paint(
                ui,
                icons::FOLDER,
                FOLDER_EMOJI_SIZE,
                icon_c,
                theme::FOLDER_ICON,
            );
        } else {
            let (glyph, tint) = icons::tree_icon(entry.kind);
            ui.painter().text(
                icon_c,
                egui::Align2::CENTER_CENTER,
                glyph,
                fonts::regular(14.0),
                tint,
            );
        }

        let (font, color) = if selected {
            (fonts::bold(13.0), theme::ACCENT)
        } else if hovered {
            (fonts::regular(13.0), theme::TEXT_STRONG)
        } else {
            (fonts::regular(13.0), theme::TEXT_BRIGHT)
        };
        let text_x = item_left + shift + 31.0;
        let g = widgets::elided_right(ui, &entry.name, font, color, r.right() - text_x - 6.0);
        let pos = pos2(text_x, r.center().y - g.size().y / 2.0);
        ui.painter().galley(pos.round(), g, color);

        if resp.clicked() || resp.double_clicked() {
            // The branch cell toggles a folder; anywhere else selects the row. The stylesheet
            // hides Qt's expand/collapse indicators, so the cell is drawn empty.
            let on_branch = resp
                .interact_pointer_pos()
                .is_some_and(|p| p.x < item_left && p.x >= item_left - INDENT);
            if entry.is_dir && (resp.double_clicked() || on_branch) {
                return Some(TreeAction::Toggle(entry.path.clone()));
            }
            if resp.clicked() {
                return Some(TreeAction::Select(entry.path.clone()));
            }
        }
        None
    }
}

/// `path` relative to `root`, comparing components case-insensitively.
fn strip_prefix_ci(path: &Path, root: &Path) -> Option<PathBuf> {
    let mut p = path.components();
    for rc in root.components() {
        let pc = p.next()?;
        if !eq_ci(rc.as_os_str(), pc.as_os_str()) {
            return None;
        }
    }
    let rest: PathBuf = p.collect();
    (!rest.as_os_str().is_empty()).then_some(rest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;

    fn fixture() -> TempDir {
        let d = TempDir::new("tree");
        let p = d.path();
        for f in ["b.STL", "a.png", "C.gcode", "notes.txt", "z.webm", "d.obj"] {
            std::fs::write(p.join(f), b"x").unwrap();
        }
        std::fs::create_dir(p.join("Sub")).unwrap();
        std::fs::create_dir(p.join("abc")).unwrap();
        std::fs::write(p.join("Sub").join("inner.3mf"), b"x").unwrap();
        d
    }

    #[test]
    fn listing_filters_and_orders() {
        let d = fixture();
        let names = |show_media: bool| -> Vec<String> {
            let mut t = FileTree::new(d.path().to_path_buf(), show_media);
            t.rows().iter().map(|r| r.entry.name.clone()).collect()
        };
        assert_eq!(
            names(false),
            vec!["abc", "Sub", "b.STL", "C.gcode", "d.obj"]
        );
        assert_eq!(
            names(true),
            vec!["abc", "Sub", "a.png", "b.STL", "C.gcode", "d.obj", "z.webm"]
        );
    }

    #[cfg(windows)]
    #[test]
    fn hides_hidden_but_not_system_items() {
        use std::os::windows::fs::OpenOptionsExt;
        const HIDDEN: u32 = 0x2;
        const SYSTEM: u32 = 0x4;
        let d = TempDir::new("attrs");
        for (name, attrs) in [
            ("plain.stl", 0),
            ("sys.stl", SYSTEM),
            ("hidden.stl", HIDDEN),
            ("both.stl", HIDDEN | SYSTEM),
        ] {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .attributes(attrs)
                .open(d.path().join(name))
                .unwrap();
        }
        let names: Vec<String> = read_listing(d.path())
            .unwrap()
            .iter()
            .map(|e| e.name.clone())
            .collect();
        assert_eq!(names, ["plain.stl", "sys.stl"]);
    }

    #[test]
    fn expand_reveal_and_selection() {
        let d = fixture();
        let mut t = FileTree::new(d.path().to_path_buf(), false);
        // Reveal with different case expands the parent and resolves the on-disk name.
        let target = d.path().join("SUB").join("INNER.3MF");
        t.select_and_reveal(&target);
        let sel = t.selected().unwrap().to_path_buf();
        assert_eq!(sel, d.path().join("Sub").join("inner.3mf"));
        assert!(t.is_expanded(&d.path().join("Sub")));
        let rows: Vec<(String, usize)> = t
            .rows()
            .iter()
            .map(|r| (r.entry.name.clone(), r.depth))
            .collect();
        assert!(rows.contains(&("inner.3mf".to_string(), 1)));
        t.collapse(&d.path().join("Sub"));
        assert_eq!(t.rows().len(), 5);
        // Paths outside the root are still selectable, just not revealed.
        t.select_and_reveal(Path::new(r"Z:\elsewhere\x.stl"));
        assert_eq!(t.selected(), Some(Path::new(r"Z:\elsewhere\x.stl")));
    }

    #[test]
    fn prev_next_resolution() {
        let d = fixture();
        let files = visible_files(d.path(), false);
        let names: Vec<_> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["b.STL", "C.gcode", "d.obj"]);
        let with_media = visible_files(d.path(), true);
        assert_eq!(with_media.len(), 5);

        let cur = d.path().join("C.gcode");
        assert_eq!(step_file(&files, &cur, 1), Some(d.path().join("d.obj")));
        assert_eq!(step_file(&files, &cur, -1), Some(d.path().join("b.STL")));
        assert_eq!(step_file(&files, &d.path().join("d.obj"), 1), None);
        assert_eq!(step_file(&files, &d.path().join("b.stl"), -1), None);
        assert_eq!(
            step_file(&files, &d.path().join("B.STL"), 1),
            Some(d.path().join("C.gcode"))
        );
        assert_eq!(step_file(&files, &d.path().join("missing.stl"), 1), None);
    }

    #[test]
    fn path_helpers() {
        assert!(same_path(
            Path::new(r"C:\Dev\a.stl"),
            Path::new(r"c:/dev/A.STL")
        ));
        assert!(!same_path(
            Path::new(r"C:\dev\a.stl"),
            Path::new(r"C:\dev\b.stl")
        ));
        assert_eq!(
            strip_prefix_ci(Path::new(r"c:\DEV\x\y.stl"), Path::new(r"C:\dev")),
            Some(PathBuf::from(r"x\y.stl"))
        );
        assert_eq!(
            strip_prefix_ci(Path::new(r"C:\dev"), Path::new(r"C:\dev")),
            None
        );
        assert_eq!(
            strip_prefix_ci(Path::new(r"D:\dev\x"), Path::new(r"C:\dev")),
            None
        );
    }

    #[test]
    fn follows_disk_changes() {
        let d = fixture();
        let mut t = FileTree::new(d.path().to_path_buf(), false);
        t.watch(Duration::from_millis(20), || {});
        t.expand(&d.path().join("Sub"));
        t.select_and_reveal(&d.path().join("d.obj"));
        t.sync_with_disk();
        std::fs::write(d.path().join("Sub").join("new.stl"), b"x").unwrap();
        std::fs::remove_file(d.path().join("d.obj")).unwrap();
        let expected = ["abc", "Sub", "inner.3mf", "new.stl", "b.STL", "C.gcode"];
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut names = Vec::new();
        while names != expected && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            t.sync_with_disk();
            names = t.rows().iter().map(|r| r.entry.name.clone()).collect();
        }
        assert_eq!(names, expected);
        assert_eq!(t.selected(), None, "the deleted file is no longer selected");
    }

    fn row_index(t: &mut FileTree, name: &str) -> usize {
        t.rows().iter().position(|r| r.entry.name == name).unwrap()
    }

    #[test]
    fn keyboard_navigation() {
        let d = fixture();
        let mut t = FileTree::new(d.path().to_path_buf(), false);
        let last = t.rows().len() - 1;
        let (b, sub) = (row_index(&mut t, "b.STL"), row_index(&mut t, "Sub"));
        // Without a current row every movement key goes to the first row.
        assert_eq!(t.navigate(Key::End, None, 3), Some(Nav::Move(0)));
        assert_eq!(t.navigate(Key::ArrowUp, None, 3), Some(Nav::Move(0)));
        assert_eq!(t.navigate(Key::A, None, 3), None);
        assert_eq!(t.navigate(Key::ArrowDown, Some(1), 3), Some(Nav::Move(2)));
        assert_eq!(
            t.navigate(Key::ArrowDown, Some(last), 3),
            Some(Nav::Move(last))
        );
        assert_eq!(t.navigate(Key::ArrowUp, Some(0), 3), Some(Nav::Move(0)));
        assert_eq!(t.navigate(Key::Home, Some(3), 3), Some(Nav::Move(0)));
        assert_eq!(t.navigate(Key::End, Some(1), 3), Some(Nav::Move(last)));
        assert_eq!(t.navigate(Key::PageDown, Some(0), 3), Some(Nav::Move(3)));
        assert_eq!(t.navigate(Key::PageDown, Some(3), 3), Some(Nav::Move(last)));
        assert_eq!(
            t.navigate(Key::PageUp, Some(last), 3),
            Some(Nav::Move(last - 3))
        );
        assert_eq!(t.navigate(Key::PageUp, Some(1), 3), Some(Nav::Move(0)));
        // Right expands a collapsed folder, does nothing on a file; Left on a top-level
        // row has no parent to go to.
        assert_eq!(
            t.navigate(Key::ArrowRight, Some(sub), 3),
            Some(Nav::Toggle(sub))
        );
        assert_eq!(t.navigate(Key::ArrowRight, Some(b), 3), None);
        assert_eq!(t.navigate(Key::ArrowLeft, Some(b), 3), None);
        // Expanded: Right steps into the first child, Left goes back to the parent and
        // then collapses it.
        t.expand(&d.path().join("Sub"));
        let inner = row_index(&mut t, "inner.3mf");
        assert_eq!(inner, sub + 1);
        assert_eq!(
            t.navigate(Key::ArrowRight, Some(sub), 3),
            Some(Nav::Move(inner))
        );
        assert_eq!(t.navigate(Key::ArrowRight, Some(inner), 3), None);
        assert_eq!(
            t.navigate(Key::ArrowLeft, Some(inner), 3),
            Some(Nav::Move(sub))
        );
        assert_eq!(
            t.navigate(Key::ArrowLeft, Some(sub), 3),
            Some(Nav::Toggle(sub))
        );
        // The current row of a selection hidden in a collapsed folder is that folder.
        t.select_and_reveal(&d.path().join("Sub").join("inner.3mf"));
        assert_eq!(t.current_row(), Some(inner));
        t.collapse(&d.path().join("Sub"));
        t.rows();
        assert_eq!(t.current_row(), Some(sub));
        t.clear_selection();
        assert_eq!(t.current_row(), None);
    }

    #[test]
    fn type_ahead_search() {
        let d = TempDir::new("typeahead");
        for f in ["alpha.stl", "Apex.obj", "ax.3mf", "beta.stl"] {
            std::fs::write(d.path().join(f), b"x").unwrap();
        }
        let mut t = FileTree::new(d.path().to_path_buf(), false);
        let names: Vec<String> = t.rows().iter().map(|r| r.entry.name.clone()).collect();
        assert_eq!(names, ["alpha.stl", "Apex.obj", "ax.3mf", "beta.stl"]);
        // A first search starts at the first row, case-insensitively.
        assert_eq!(t.type_ahead("A", 0.0, None), Some(0));
        // A new search starts after the current row; repeating the key cycles.
        assert_eq!(t.type_ahead("a", 1.0, Some(0)), Some(1));
        assert_eq!(t.type_ahead("a", 1.1, Some(1)), Some(2));
        assert_eq!(t.type_ahead("a", 1.2, Some(2)), Some(0));
        assert_eq!(t.type_ahead("b", 3.0, Some(0)), Some(3));
        // Quick keystrokes extend the prefix.
        assert_eq!(t.type_ahead("a", 5.0, Some(3)), Some(0));
        assert_eq!(t.type_ahead("p", 5.1, Some(0)), Some(1));
        assert_eq!(t.type_ahead("z", 7.0, Some(1)), None);
    }
}
