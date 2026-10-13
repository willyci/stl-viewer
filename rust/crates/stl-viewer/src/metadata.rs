//! The "Mesh Details" group: state for every mode (mesh, toolpath, folder, media, loading)
//! and its painting (2-column grid of row labels and word-wrapped values).

use std::path::Path;
use std::sync::Arc;

use egui::text::LayoutJob;
use egui::{CursorIcon, FontId, Galley, Rect, Sense, Stroke, TextFormat, Ui, pos2};
use stlv_core::ModelData;
use stlv_core::format::{format_file_size, thousands};

use crate::{fonts, text, theme, widgets};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    FileName,
    FileSize,
    Path,
    Format,
    Vertices,
    Triangles,
    Bounds,
    Volume,
}

impl Field {
    pub const ALL: [Self; 8] = [
        Self::FileName,
        Self::FileSize,
        Self::Path,
        Self::Format,
        Self::Vertices,
        Self::Triangles,
        Self::Bounds,
        Self::Volume,
    ];

    fn default_label(self) -> &'static str {
        match self {
            Self::FileName => "Filename:",
            Self::FileSize => "File Size:",
            Self::Path => "Path:",
            Self::Format => "Format:",
            Self::Vertices => "Vertices:",
            Self::Triangles => "Triangles:",
            Self::Bounds => "Dimensions:",
            Self::Volume => "Est. Volume:",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Metadata {
    pub title: String,
    labels: [&'static str; 8],
    values: [String; 8],
    /// Full path behind the link-styled Path value; empty means "nothing to copy".
    full_path: String,
}

impl Default for Metadata {
    fn default() -> Self {
        Self {
            title: "Mesh Details".into(),
            labels: Field::ALL.map(Field::default_label),
            values: Field::ALL.map(|_| "-".to_string()),
            full_path: String::new(),
        }
    }
}

const LOADING: &str = "Loading...";
const NA: &str = "N/A";

impl Metadata {
    pub fn label(&self, field: Field) -> &str {
        self.labels[field as usize]
    }

    pub fn value(&self, field: Field) -> &str {
        &self.values[field as usize]
    }

    pub fn full_path(&self) -> &str {
        &self.full_path
    }

    fn set(&mut self, field: Field, value: impl Into<String>) {
        self.values[field as usize] = value.into();
    }

    fn set_label(&mut self, field: Field, label: &'static str) {
        self.labels[field as usize] = label;
    }

    fn set_path(&mut self, path: &Path) {
        let norm = text::normpath(&path.to_string_lossy());
        self.set(Field::Path, norm.clone());
        self.full_path = norm;
    }

    /// Python `_reset_metadata_labels` (the title is left alone).
    pub fn set_loading(&mut self) {
        self.set_label(Field::Triangles, "Triangles:");
        self.set_label(Field::Volume, "Est. Volume:");
        for v in &mut self.values {
            *v = LOADING.into();
        }
        self.full_path.clear();
    }

    /// Python `_update_metadata_ui`.
    pub fn set_model(&mut self, model: &ModelData, path: &Path, size: Option<u64>) {
        self.set(Field::FileName, model.filename.clone());
        self.set(
            Field::FileSize,
            size.map(format_file_size).unwrap_or_else(|| "-".into()),
        );
        self.set_path(path);
        self.set(Field::Format, model.format.clone());
        self.set(Field::Vertices, thousands(model.num_vertices() as u64));
        self.set_label(Field::Volume, "Est. Volume:");
        self.set(
            Field::Bounds,
            text::dimensions_mm(model.bounding_box_size()),
        );
        if model.is_toolpath() {
            self.title = "Toolpath Details".into();
            self.set_label(Field::Triangles, "Lines:");
            self.set(Field::Triangles, thousands(model.num_lines() as u64));
            self.set(Field::Volume, "N/A (Toolpath)");
        } else {
            self.title = "Mesh Details".into();
            self.set_label(Field::Triangles, "Triangles:");
            self.set(Field::Triangles, thousands(model.num_triangles() as u64));
            self.set(Field::Volume, text::volume(model.volume));
        }
    }

    pub fn set_folder(&mut self, folder: &Path, files: usize, total_bytes: u64) {
        self.title = "Folder Details".into();
        self.set_label(Field::Triangles, "Total Files:");
        self.set_label(Field::Volume, "Total Size:");
        self.set(Field::FileName, text::basename(folder));
        self.set(Field::FileSize, NA);
        self.set_path(folder);
        self.set(Field::Format, "Directory");
        self.set(Field::Vertices, NA);
        self.set(Field::Triangles, files.to_string());
        self.set(Field::Bounds, NA);
        self.set(Field::Volume, format_file_size(total_bytes));
    }

    /// Shared first step of every media load: size and path (after `set_loading`).
    pub fn set_media_file(&mut self, path: &Path, size: &str) {
        self.set(Field::FileSize, size);
        self.set_path(path);
    }

    fn media_common(&mut self, title: &str, path: &Path, format: impl Into<String>, size: &str) {
        self.title = title.into();
        self.set_label(Field::Volume, "File Size:");
        self.set(Field::FileName, text::basename(path));
        self.set(Field::Format, format);
        self.set(Field::Vertices, NA);
        self.set(Field::Triangles, NA);
        self.set(Field::Volume, size);
    }

    pub fn set_image(&mut self, path: &Path, w: u32, h: u32, size: &str) {
        self.media_common("Image Details", path, text::ext_upper(path), size);
        self.set(Field::Bounds, format!("{w} x {h} px"));
    }

    pub fn set_gif(&mut self, path: &Path, dims: Option<(u32, u32)>, size: &str) {
        self.media_common("Image Details (GIF)", path, "GIF (Animated)", size);
        let (w, h) = text::dims_or_unknown(dims);
        self.set(Field::Bounds, format!("{w} x {h} px"));
    }

    pub fn set_svg(&mut self, path: &Path, w: u32, h: u32, size: &str) {
        self.media_common("Vector Details", path, "SVG (Vector)", size);
        self.set(Field::Bounds, format!("{w} x {h} px"));
    }

    pub fn set_webm(&mut self, path: &Path, size: &str) {
        self.media_common("Video Details", path, "WebM (Video)", size);
        self.set(Field::Triangles, "WebM Video Stream");
        self.set(Field::Bounds, NA);
    }

    /// One line per row, for script logging and tests.
    pub fn dump(&self) -> String {
        let mut out = format!("[{}]", self.title);
        for f in Field::ALL {
            out.push_str(&format!("\n  {} {}", self.label(f), self.value(f)));
        }
        out
    }
}

// ---------------------------------------------------------------------------------------------
// Painting

const ROW_SPACING: f32 = 10.0;
const COLUMN_SPACING: f32 = 10.0;
const LABEL_SIZE: f32 = 13.0;
const LINK_SIZE: f32 = 10.0;
/// Qt elided the Path value to this many pixels.
const PATH_ELIDE_WIDTH: f32 = 160.0;

/// The rows laid out for one content width, the way the Qt `QGridLayout` of word-wrapped
/// `QLabel`s sizes them (see [`text::qt_two_columns`]).
pub struct MetaLayout {
    label_col: f32,
    rows: Vec<MetaRow>,
    pub height: f32,
}

struct MetaRow {
    label: Arc<Galley>,
    lines: Vec<Arc<Galley>>,
    height: f32,
}

impl Metadata {
    /// Value texts as displayed (the path elided) with their fonts.
    fn display_values(&self, ui: &Ui) -> Vec<(String, FontId)> {
        let link_font = fonts::bold(LINK_SIZE);
        Field::ALL
            .iter()
            .map(|&f| match f {
                Field::Path => (self.path_display(ui, &link_font), link_font.clone()),
                _ => (self.value(f).to_string(), fonts::bold(LABEL_SIZE)),
            })
            .collect()
    }

    /// Size hints of the two columns: label width, value hint and value minimum.
    fn column_hints(&self, ui: &Ui, values: &[(String, FontId)]) -> (f32, f32, f32) {
        let label_font = fonts::regular(LABEL_SIZE);
        let label_hint = Field::ALL
            .iter()
            .map(|&f| widgets::qt_text_width(ui, self.label(f), &label_font))
            .fold(0.0, f32::max);
        let (mut value_hint, mut value_min) = (0.0f32, 0.0f32);
        for (v, font) in values {
            let mut measure = |s: &str| widgets::qt_text_width(ui, s, font);
            value_hint = value_hint.max(text::qt_wrap_hint(
                v,
                fonts::avg_char_width(font),
                &mut measure,
            ));
            value_min = value_min.max(text::qt_wrap_min(v, &mut measure));
        }
        (label_hint, value_hint, value_min)
    }

    /// Narrowest content width without overflow: labels plus the widest unbreakable value word.
    pub fn min_width(&self, ui: &Ui) -> f32 {
        let (label_hint, _, value_min) = self.column_hints(ui, &self.display_values(ui));
        label_hint + COLUMN_SPACING + value_min
    }

    pub fn layout(&self, ui: &Ui, width: f32) -> MetaLayout {
        let painter = ui.painter();
        let label_font = fonts::regular(LABEL_SIZE);
        let labels: Vec<_> = Field::ALL
            .iter()
            .map(|&f| {
                painter.layout_no_wrap(self.label(f).to_string(), label_font.clone(), theme::TEXT)
            })
            .collect();
        let values = self.display_values(ui);
        let (label_hint, value_hint, _) = self.column_hints(ui, &values);
        let (label_col, value_col) =
            text::qt_two_columns(width - COLUMN_SPACING, label_hint, value_hint);

        let mut rows = Vec::with_capacity(values.len());
        let mut height = 0.0;
        for (label, (f, (v, font))) in labels.into_iter().zip(Field::ALL.iter().zip(&values)) {
            let link = *f == Field::Path;
            let mut measure = |s: &str| widgets::qt_text_width(ui, s, font);
            let lines: Vec<Arc<Galley>> = text::wrap_lines(v, value_col, &mut measure)
                .into_iter()
                .map(|line| {
                    let format = TextFormat {
                        font_id: font.clone(),
                        color: if link {
                            theme::LINK
                        } else {
                            theme::TEXT_STRONG
                        },
                        underline: if link {
                            Stroke::new(1.0, theme::LINK)
                        } else {
                            Stroke::NONE
                        },
                        ..Default::default()
                    };
                    painter.layout_job(LayoutJob::single_section(line, format))
                })
                .collect();
            let value_h: f32 = lines.iter().map(|g| g.size().y).sum();
            let row_h = label.size().y.max(value_h).round();
            if !rows.is_empty() {
                height += ROW_SPACING;
            }
            height += row_h;
            rows.push(MetaRow {
                label,
                lines,
                height: row_h,
            });
        }
        MetaLayout {
            label_col,
            rows,
            height,
        }
    }

    fn path_display(&self, ui: &Ui, font: &FontId) -> String {
        if self.full_path.is_empty() {
            return self.value(Field::Path).to_string();
        }
        widgets::elide_left(ui, &self.full_path, PATH_ELIDE_WIDTH, font)
    }

    /// Paint the rows into `rect` (as laid out by [`Self::layout`]); returns the full path
    /// when the Path link was clicked.
    pub fn paint(&self, ui: &mut Ui, rect: Rect, layout: MetaLayout) -> Option<String> {
        let mut y = rect.top();
        let value_x = rect.left() + layout.label_col + COLUMN_SPACING;
        let mut clicked = None;
        for (row, f) in layout.rows.into_iter().zip(Field::ALL) {
            let painter = ui.painter();
            let label_pos = pos2(rect.left(), y + (row.height - row.label.size().y) / 2.0);
            painter.galley(label_pos.round(), row.label, theme::TEXT);
            let value_h: f32 = row.lines.iter().map(|g| g.size().y).sum();
            let mut line_y = y + (row.height - value_h) / 2.0;
            let mut value_rect = Rect::NOTHING;
            for line in row.lines {
                let pos = pos2(value_x, line_y).round();
                value_rect = value_rect.union(Rect::from_min_size(pos, line.size()));
                line_y += line.size().y;
                painter.galley(pos, line, theme::TEXT_STRONG);
            }
            if f == Field::Path {
                let resp = ui
                    .interact(value_rect, ui.id().with("meta_path_link"), Sense::click())
                    .on_hover_cursor(CursorIcon::PointingHand);
                let resp = if self.full_path.is_empty() {
                    resp
                } else {
                    resp.on_hover_text(&self.full_path)
                };
                if resp.clicked() && !self.full_path.is_empty() {
                    clicked = Some(self.full_path.clone());
                }
            }
            y += row.height + ROW_SPACING;
        }
        clicked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_and_loading() {
        let mut m = Metadata::default();
        assert_eq!(m.title, "Mesh Details");
        assert!(Field::ALL.iter().all(|&f| m.value(f) == "-"));
        assert_eq!(m.label(Field::Bounds), "Dimensions:");
        m.set_folder(Path::new(r"C:\dev\models"), 3, 4096);
        m.set_loading();
        assert_eq!(m.title, "Folder Details");
        assert_eq!(m.label(Field::Triangles), "Triangles:");
        assert_eq!(m.label(Field::Volume), "Est. Volume:");
        assert!(Field::ALL.iter().all(|&f| m.value(f) == "Loading..."));
        assert_eq!(m.full_path(), "");
    }

    #[test]
    fn mesh_and_toolpath() {
        let v = vec![
            [0.0, 0.0, 0.0],
            [10.0, 0.0, 0.0],
            [0.0, 10.0, 0.0],
            [0.0, 0.0, 10.0],
        ];
        let t = vec![[0, 2, 1], [0, 1, 3], [0, 3, 2], [1, 2, 3]];
        let mesh = ModelData::new(v.clone(), t, vec![], "STL (Binary)", "tet.stl");
        let mut m = Metadata::default();
        m.set_model(&mesh, Path::new("C:/dev/tet.stl"), Some(684));
        assert_eq!(m.title, "Mesh Details");
        assert_eq!(m.value(Field::FileName), "tet.stl");
        assert_eq!(m.value(Field::FileSize), "684 Bytes");
        assert_eq!(m.value(Field::Path), r"C:\dev\tet.stl");
        assert_eq!(m.full_path(), r"C:\dev\tet.stl");
        assert_eq!(m.value(Field::Vertices), "4");
        assert_eq!(m.value(Field::Triangles), "4");
        assert_eq!(m.value(Field::Bounds), "10.0 x 10.0 x 10.0 mm");
        assert_eq!(m.value(Field::Volume), "0.17 cm\u{b3} (166.7 mm\u{b3})");

        let path = ModelData::new(v, vec![], vec![[0, 1], [2, 3]], "G-Code Path", "p.gcode");
        m.set_model(&path, Path::new(r"C:\p.gcode"), None);
        assert_eq!(m.title, "Toolpath Details");
        assert_eq!(m.label(Field::Triangles), "Lines:");
        assert_eq!(m.value(Field::Triangles), "2");
        assert_eq!(m.value(Field::FileSize), "-");
        assert_eq!(m.value(Field::Volume), "N/A (Toolpath)");
    }

    #[test]
    fn folder_and_media() {
        let mut m = Metadata::default();
        m.set_folder(Path::new(r"C:\"), 0, 0);
        assert_eq!(m.value(Field::FileName), r"C:\");
        assert_eq!(m.label(Field::Triangles), "Total Files:");
        assert_eq!(m.label(Field::Volume), "Total Size:");
        assert_eq!(m.value(Field::Volume), "0 Bytes");
        assert_eq!(m.value(Field::Format), "Directory");

        m.set_loading();
        m.set_media_file(Path::new(r"C:\m\t.bmp"), "192.1 KB");
        m.set_image(Path::new(r"C:\m\t.bmp"), 256, 256, "192.1 KB");
        assert_eq!(m.title, "Image Details");
        assert_eq!(m.label(Field::Volume), "File Size:");
        assert_eq!(m.value(Field::Format), "BMP");
        assert_eq!(m.value(Field::Bounds), "256 x 256 px");
        assert_eq!(m.value(Field::Vertices), "N/A");

        m.set_gif(Path::new("a.gif"), None, "1 Bytes");
        assert_eq!(m.title, "Image Details (GIF)");
        assert_eq!(m.value(Field::Format), "GIF (Animated)");
        assert_eq!(m.value(Field::Bounds), "Unknown x Unknown px");

        m.set_svg(Path::new("l.svg"), 200, 100, "766 Bytes");
        assert_eq!(m.title, "Vector Details");
        assert_eq!(m.value(Field::Format), "SVG (Vector)");

        m.set_webm(Path::new("v.webm"), "2.00 MB");
        assert_eq!(m.title, "Video Details");
        assert_eq!(m.value(Field::Triangles), "WebM Video Stream");
        assert_eq!(m.value(Field::Bounds), "N/A");
        assert_eq!(m.value(Field::Volume), "2.00 MB");
    }
}
