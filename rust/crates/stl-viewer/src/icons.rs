//! Emoji icons per file kind (app.py `ThumbnailCard._add_placeholder`) and their tints.

use egui::Color32;
use stlv_core::FileKind;

use crate::theme;

pub const BOLT: &str = "\u{26A1}";
pub const FOLDER: &str = "\u{1F4C1}";

/// Glyph, tint and caption of a thumbnail card placeholder.
pub fn placeholder(kind: FileKind) -> (&'static str, Color32, &'static str) {
    match kind {
        FileKind::Stl => ("\u{1F4D0}", theme::ICON_STL, "STL"),
        FileKind::ThreeMf => ("\u{1F9F1}", theme::ICON_3MF, "3MF"),
        FileKind::Obj => ("\u{1F3FA}", theme::ICON_OBJ, "OBJ"),
        FileKind::Gcode => ("\u{1F9F5}", theme::ICON_GCODE, "G-CODE"),
        FileKind::Image | FileKind::Svg => ("\u{1F5BC}", theme::ICON_IMAGE, ""),
        FileKind::Gif => ("\u{1F3AC}", theme::ICON_GIF, ""),
        FileKind::Webm => ("\u{1F3A5}", theme::ICON_VIDEO, ""),
        FileKind::Other => ("\u{1F4C4}", theme::ICON_IMAGE, ""),
    }
}

/// Glyph and tint of a file row in the tree (folders use the color [`FOLDER`] emoji).
pub fn tree_icon(kind: FileKind) -> (&'static str, Color32) {
    let (glyph, tint, _) = placeholder(kind);
    (glyph, tint)
}
