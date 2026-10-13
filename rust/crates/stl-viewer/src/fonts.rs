//! Segoe UI (regular + bold) with Segoe UI Symbol / Segoe UI Emoji fallbacks for the icon
//! glyphs (⚡ ↑ ◀ ▶ 📐 🧱 🏺 🧵 🖼 🎬 🎥 📄 📁). Emoji render as monochrome outlines that take the
//! text color. Missing fonts fall back to egui's defaults.

use std::path::PathBuf;
use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily, FontId};

use stlv_media::BOLD_FAMILY;

pub fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BOLD_FAMILY.into()))
}

/// Qt `QFontMetrics::averageCharWidth` (rounded OS/2 `xAvgCharWidth`) of Segoe UI.
pub fn avg_char_width(font: &FontId) -> f32 {
    let units = if font.family == FontFamily::Proportional {
        1103.0
    } else {
        1208.0
    };
    (font.size * units / 2048.0).round()
}

pub fn fonts_dir() -> PathBuf {
    let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
    PathBuf::from(windir).join("Fonts")
}

fn load(name: &str) -> Option<Arc<FontData>> {
    let path = fonts_dir().join(name);
    match std::fs::read(&path) {
        Ok(bytes) => Some(Arc::new(FontData::from_owned(bytes))),
        Err(e) => {
            log::warn!("font {} unavailable: {e}", path.display());
            None
        }
    }
}

pub fn install(ctx: &egui::Context) {
    let mut defs = FontDefinitions::default();
    let egui_defaults = defs
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();

    let mut add = |key: &str, file: &str| -> Option<String> {
        let data = load(file)?;
        defs.font_data.insert(key.to_string(), data);
        Some(key.to_string())
    };
    let regular = add("segoe-ui", "segoeui.ttf");
    let bold = add("segoe-ui-bold", "segoeuib.ttf");
    let fallbacks: Vec<String> = [
        add("segoe-ui-symbol", "seguisym.ttf"),
        add("segoe-ui-emoji", "seguiemj.ttf"),
    ]
    .into_iter()
    .flatten()
    .collect();

    let family = |primary: Option<String>| -> Vec<String> {
        primary
            .into_iter()
            .chain(fallbacks.iter().cloned())
            .chain(egui_defaults.iter().cloned())
            .collect()
    };
    let bold_primary = bold.or_else(|| regular.clone());
    defs.families
        .insert(FontFamily::Proportional, family(regular));
    defs.families
        .insert(FontFamily::Name(BOLD_FAMILY.into()), family(bold_primary));
    ctx.set_fonts(defs);
}
