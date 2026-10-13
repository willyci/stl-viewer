//! Dark slate theme (styles.py): palette colors as `Color32` and the egui style used for the
//! few stock widgets we keep (text edit, scroll bars, tooltips, color picker).

use egui::style::ScrollStyle;
use egui::{Color32, CornerRadius, Shadow, Stroke, Style, Visuals};
use stlv_core::palette as p;

pub const fn hex(rgb: u32) -> Color32 {
    let [r, g, b] = p::rgb(rgb);
    Color32::from_rgb(r, g, b)
}

pub const ACCENT: Color32 = hex(p::ACCENT);
pub const ACCENT_BUTTON: Color32 = hex(p::ACCENT_BUTTON);
pub const ACCENT_PRESSED: Color32 = hex(p::ACCENT_PRESSED);
pub const LINK: Color32 = hex(p::LINK);

pub const WINDOW_BG: Color32 = hex(p::WINDOW_BG);
pub const SIDEBAR_BG: Color32 = hex(p::SIDEBAR_BG);
pub const PANEL_BG: Color32 = hex(p::PANEL_BG);
pub const VIEWER_BG: Color32 = hex(p::VIEWER_BG);
pub const CARD_BG: Color32 = hex(p::CARD_BG);
pub const CARD_HOVER_BG: Color32 = hex(p::CARD_HOVER_BG);
pub const THUMB_WELL_BG: Color32 = hex(p::THUMB_WELL_BG);
pub const BUTTON_BG: Color32 = hex(p::BUTTON_BG);
pub const BUTTON_HOVER_BG: Color32 = hex(p::BUTTON_HOVER_BG);
pub const BUTTON_PRESSED_BG: Color32 = hex(p::BUTTON_PRESSED_BG);
pub const TREE_HOVER_BG: Color32 = hex(p::TREE_HOVER_BG);
pub const TREE_SELECTED_BG: Color32 = hex(p::TREE_SELECTED_BG);
pub const LOADING_CARD_BG: Color32 = hex(p::LOADING_CARD_BG);
pub const LOADING_TRACK: Color32 = hex(p::LOADING_TRACK);

pub const BORDER: Color32 = hex(p::BORDER);
pub const BORDER_SUBTLE: Color32 = hex(p::BORDER_SUBTLE);
pub const BUTTON_BORDER: Color32 = hex(p::BUTTON_BORDER);
pub const SPLITTER: Color32 = hex(p::SPLITTER);
pub const SCROLL_TRACK: Color32 = hex(p::SCROLL_TRACK);
pub const SCROLL_HANDLE: Color32 = hex(p::SCROLL_HANDLE);

pub const TEXT: Color32 = hex(p::TEXT);
pub const TEXT_STRONG: Color32 = hex(p::TEXT_STRONG);
pub const TEXT_BRIGHT: Color32 = hex(p::TEXT_BRIGHT);
pub const TEXT_CARD: Color32 = hex(p::TEXT_CARD);
pub const TEXT_MUTED: Color32 = hex(p::TEXT_MUTED);

pub const OVERLAY_VEIL: Color32 = {
    let [r, g, b, a] = p::OVERLAY_RGBA;
    Color32::from_rgba_unmultiplied_const(r, g, b, a)
};

pub const ICON_STL: Color32 = hex(p::ICON_STL);
pub const ICON_3MF: Color32 = hex(p::ICON_3MF);
pub const ICON_OBJ: Color32 = hex(p::ICON_OBJ);
pub const ICON_GCODE: Color32 = hex(p::ICON_GCODE);
pub const ICON_IMAGE: Color32 = hex(p::ICON_IMAGE);
pub const ICON_GIF: Color32 = hex(p::ICON_GIF);
pub const ICON_VIDEO: Color32 = hex(p::ICON_VIDEO);

// Colors that only appear in styles.py / app.py stylesheets.
pub const COMBO_POPUP_BG: Color32 = hex(0x181B26);
pub const COMBO_POPUP_TEXT: Color32 = hex(0xD2D8E6);
pub const PATH_EDIT_FOCUS_TEXT: Color32 = hex(0xE2E8F0);
/// Qt's VLine in #232836, anti-aliased over two device pixels.
pub const DECK_SEPARATOR: Color32 = hex(0x1C202C);
/// Windows-style yellow folder.
pub const FOLDER_ICON: Color32 = hex(0xF4C55B);
/// Windows 11 accent blue: the native tree style's selection mark.
pub const WIN_ACCENT: Color32 = hex(0x0067C0);
pub const ERROR_RED: Color32 = hex(0xF43F5E);

pub fn apply(ctx: &egui::Context) {
    ctx.all_styles_mut(configure);
}

fn configure(style: &mut Style) {
    let mut v = Visuals::dark();
    v.panel_fill = WINDOW_BG;
    v.window_fill = COMBO_POPUP_BG;
    v.window_stroke = Stroke::new(1.0, BUTTON_BORDER);
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(6);
    v.window_shadow = Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(120),
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 10,
        spread: 0,
        color: Color32::from_black_alpha(90),
    };
    v.extreme_bg_color = SCROLL_TRACK; // scroll bar track
    v.text_edit_bg_color = Some(PANEL_BG);
    v.hyperlink_color = LINK;
    v.selection.bg_fill = ACCENT_PRESSED;
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.text_cursor.stroke = Stroke::new(1.5, ACCENT);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.noninteractive.bg_fill = PANEL_BG;
    // Stock-widget states double as the scroll bar handle colors (styles.py QScrollBar).
    for (w, fill) in [
        (&mut v.widgets.inactive, SCROLL_HANDLE),
        (&mut v.widgets.hovered, ACCENT),
        (&mut v.widgets.active, ACCENT),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = BUTTON_BG;
        w.corner_radius = CornerRadius::same(5);
        w.bg_stroke = Stroke::new(1.0, BUTTON_BORDER);
        w.fg_stroke = Stroke::new(1.0, TEXT_BRIGHT);
        w.expansion = 0.0;
    }
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT_BUTTON);
    style.visuals = v;

    style.spacing.scroll = ScrollStyle {
        bar_width: 10.0,
        handle_min_length: 24.0,
        bar_inner_margin: 0.0,
        bar_outer_margin: 0.0,
        ..ScrollStyle::solid()
    };
    style.spacing.item_spacing = egui::vec2(6.0, 6.0);
    style.spacing.tooltip_width = 600.0;
    style.interaction.selectable_labels = false;
    style
        .text_styles
        .insert(egui::TextStyle::Body, crate::fonts::regular(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, crate::fonts::bold(12.0));
    style
        .text_styles
        .insert(egui::TextStyle::Small, crate::fonts::regular(11.0));
}

/// Visuals for the stock egui widgets inside dialogs (the color picker's sliders etc.).
pub fn dialog_visuals(v: &mut Visuals) {
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
    ] {
        w.bg_fill = BUTTON_BG;
        w.weak_bg_fill = BUTTON_BG;
        w.corner_radius = CornerRadius::same(4);
    }
    v.widgets.hovered.bg_fill = BUTTON_HOVER_BG;
    v.widgets.hovered.weak_bg_fill = BUTTON_HOVER_BG;
    v.widgets.active.bg_fill = BUTTON_PRESSED_BG;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_BRIGHT);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT_STRONG);
    v.widgets.active.fg_stroke = Stroke::new(1.0, TEXT_STRONG);
}
