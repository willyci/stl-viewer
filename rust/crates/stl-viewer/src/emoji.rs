//! Color emoji, drawn the way Qt draws the app's ⚡ icons and thumbnail placeholders: Qt
//! renders Segoe UI Emoji's COLR glyphs in color (ignoring the stylesheet text color),
//! while egui's font renderer only knows monochrome outlines. resvg rasterizes the color
//! glyph once per size into a texture; without the font the tinted glyph is used instead.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use egui::{
    Color32, ColorImage, Context, Id, Pos2, Rect, TextureHandle, TextureOptions, Ui, Vec2, pos2,
    vec2,
};
use resvg::{tiny_skia, usvg};

use crate::fonts;

/// Segoe UI Emoji metrics in font units: advance, line ascent (`usWinAscent`) and descent.
const UNITS_PER_EM: f32 = 2048.0;
const ADVANCE: f32 = 2812.0;
const ASCENT: f32 = 2210.0;
const DESCENT: f32 = 514.0;

/// The text box Qt lays an emoji out in at font size `size`: its advance by the line height.
pub fn box_size(size: f32) -> Vec2 {
    vec2(ADVANCE, ASCENT + DESCENT) * (size / UNITS_PER_EM)
}

#[derive(Default)]
struct Cache {
    /// The emoji font, loaded on first use (`None` when it is missing).
    db: OnceLock<Option<Arc<usvg::fontdb::Database>>>,
    textures: HashMap<(String, u32), Option<TextureHandle>>,
}

fn emoji_font_db() -> Option<Arc<usvg::fontdb::Database>> {
    let path = fonts::fonts_dir().join("seguiemj.ttf");
    let mut db = usvg::fontdb::Database::new();
    if let Err(e) = db.load_font_file(&path) {
        log::warn!("color emoji unavailable ({}): {e}", path.display());
        return None;
    }
    Some(Arc::new(db))
}

/// Rasterize `emoji` at `px` device pixels per em into its [`box_size`] box.
fn rasterize(db: &Arc<usvg::fontdb::Database>, emoji: &str, px: f32) -> Option<ColorImage> {
    let size = box_size(px);
    let (w, h) = (size.x.ceil() as u32, size.y.ceil() as u32);
    let baseline = ASCENT / UNITS_PER_EM * px;
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}"><text x="0" y="{baseline}" font-family="Segoe UI Emoji" font-size="{px}">{emoji}</text></svg>"#
    );
    let opts = usvg::Options {
        fontdb: Arc::clone(db),
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(&svg, &opts).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    // A font without a color glyph for this char would leave the canvas empty.
    pixmap
        .data()
        .chunks_exact(4)
        .any(|p| p[3] > 0)
        .then(|| ColorImage::from_rgba_premultiplied([w as usize, h as usize], pixmap.data()))
}

fn texture(ctx: &Context, emoji: &str, size: f32) -> Option<TextureHandle> {
    let cache = ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<Arc<Mutex<Cache>>>(Id::NULL)
            .clone()
    });
    let mut cache = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let px = size * ctx.pixels_per_point();
    let key = (emoji.to_string(), (px * 4.0).round() as u32);
    if let Some(t) = cache.textures.get(&key) {
        return t.clone();
    }
    let db = cache.db.get_or_init(emoji_font_db).clone();
    let tex = db
        .and_then(|db| rasterize(&db, emoji, px))
        .map(|img| ctx.load_texture(format!("emoji:{emoji}:{px}"), img, TextureOptions::LINEAR));
    cache.textures.insert(key, tex.clone());
    tex
}

/// Paint `emoji` at font size `size` with its text box centered on `center`; falls back to
/// the monochrome glyph in `tint`. Returns the box.
pub fn paint(ui: &Ui, emoji: &str, size: f32, center: Pos2, tint: Color32) -> Rect {
    let rect = Rect::from_center_size(center, box_size(size));
    let ppp = ui.ctx().pixels_per_point();
    // Snap to device pixels so the texture maps 1:1.
    let min = pos2(
        (rect.min.x * ppp).round() / ppp,
        (rect.min.y * ppp).round() / ppp,
    );
    match texture(ui.ctx(), emoji, size) {
        Some(tex) => {
            let r = Rect::from_min_size(min, tex.size_vec2() / ppp);
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            ui.painter().image(tex.id(), r, uv, Color32::WHITE);
        }
        None => {
            let g = ui
                .painter()
                .layout_no_wrap(emoji.to_string(), fonts::regular(size), tint);
            ui.painter()
                .galley((center - g.size() / 2.0).round(), g, tint);
        }
    }
    rect
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_color_glyphs() {
        let Some(db) = emoji_font_db() else { return };
        let img = rasterize(&db, "\u{26A1}", 40.0).expect("bolt renders");
        assert_eq!(img.size, [55, 54]);
        // The bolt is orange: some pixel with much more red than blue.
        let orange = img
            .pixels
            .iter()
            .any(|p| p.a() > 200 && p.r() > 200 && p.b() < 120);
        assert!(orange, "expected a colored glyph");
    }
}
