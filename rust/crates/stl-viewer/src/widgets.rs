//! Custom-painted widgets that reproduce the Qt stylesheet look (styles.py): buttons, the
//! accent button, checkboxes, group boxes with the title on the border, the shading combo,
//! separators and progress bars.

use std::sync::Arc;

use egui::text::LayoutJob;
use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Galley, Id, Popup, PopupCloseBehavior, Pos2,
    Rect, Response, Sense, Stroke, StrokeKind, TextFormat, Ui, Vec2, pos2, vec2,
};

use crate::{fonts, theme};

// ---------------------------------------------------------------------------------------------
// Buttons

#[derive(Clone, Debug)]
pub struct ButtonStyle {
    pub font: FontId,
    pub padding: Vec2,
    pub accent: bool,
    pub fixed_width: Option<f32>,
    /// Drawn muted and ignores clicks.
    pub disabled: bool,
}

impl ButtonStyle {
    /// `QPushButton`: bold 12 px, padding 8×16.
    pub fn normal() -> Self {
        Self {
            font: fonts::bold(12.0),
            padding: vec2(16.0, 8.0),
            accent: false,
            fixed_width: None,
            disabled: false,
        }
    }

    /// "Model Color" (`#AccentButton`).
    pub fn accent() -> Self {
        Self {
            accent: true,
            ..Self::normal()
        }
    }

    /// Compact button with its own font size and padding (`Browse...`, `↑`).
    pub fn compact(size: f32, padding: Vec2) -> Self {
        Self {
            font: fonts::bold(size),
            padding,
            ..Self::normal()
        }
    }

    pub fn width(mut self, w: f32) -> Self {
        self.fixed_width = Some(w);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

pub fn layout_text(ui: &Ui, text: &str, font: FontId, color: Color32) -> Arc<Galley> {
    ui.painter().layout_no_wrap(text.to_string(), font, color)
}

/// Text width as Qt measures it on Windows: hinted advances, i.e. each glyph's advance
/// rounded to whole device pixels. Sizing widgets with it reproduces the Qt geometry
/// (egui lays text out with fractional advances, a pixel or two wider per word).
pub fn qt_text_width(ui: &Ui, text: &str, font: &FontId) -> f32 {
    let ppp = ui.ctx().pixels_per_point();
    ui.fonts_mut(|f| {
        text.chars()
            .map(|c| (f.glyph_width(font, c) * ppp).round())
            .sum::<f32>()
            / ppp
    })
}

/// Qt `ElideLeft` of `text` to `max_w` in `font` (hinted advances).
pub fn elide_left(ui: &Ui, text: &str, max_w: f32, font: &FontId) -> String {
    let ppp = ui.ctx().pixels_per_point();
    ui.fonts_mut(|f| {
        crate::text::elide_left(text, max_w, |c| {
            (f.glyph_width(font, c) * ppp).round() / ppp
        })
    })
}

/// Qt `ElideMiddle` of `text` to `max_w` in `font` (hinted advances).
pub fn elide_middle(ui: &Ui, text: &str, max_w: f32, font: &FontId) -> String {
    let ppp = ui.ctx().pixels_per_point();
    ui.fonts_mut(|f| {
        crate::text::elide_middle(text, max_w, |c| {
            (f.glyph_width(font, c) * ppp).round() / ppp
        })
    })
}

#[derive(Clone, Copy, PartialEq)]
enum Arrow {
    Left,
    Right,
}

/// Advance of the small ◀ / ▶ the Qt app shows in "◀ Prev" / "Next ▶".
const ARROW_ADVANCE: f32 = 8.0;

/// Splits a leading ◀ or trailing ▶ off a button label. Qt draws them as tiny pointers that
/// no font available to egui reproduces, so they are painted as shapes.
fn split_arrow(text: &str) -> (Option<Arrow>, &str) {
    if let Some(rest) = text.strip_prefix('\u{25C0}') {
        (Some(Arrow::Left), rest)
    } else if let Some(rest) = text.strip_suffix('\u{25B6}') {
        (Some(Arrow::Right), rest)
    } else {
        (None, text)
    }
}

fn label_width(ui: &Ui, text: &str, font: &FontId) -> f32 {
    let (arrow, rest) = split_arrow(text);
    let arrow_w = if arrow.is_some() { ARROW_ADVANCE } else { 0.0 };
    qt_text_width(ui, rest, font) + arrow_w
}

/// Outer size including the 1 px border (the accent button has none).
pub fn button_size(ui: &Ui, text: &str, style: &ButtonStyle) -> Vec2 {
    let border = if style.accent { 0.0 } else { 2.0 };
    let text_h = layout_text(ui, "Ag", style.font.clone(), theme::TEXT_BRIGHT)
        .size()
        .y;
    let w = style
        .fixed_width
        .unwrap_or(label_width(ui, text, &style.font) + 2.0 * style.padding.x + border);
    vec2(w, text_h + 2.0 * style.padding.y + border)
}

/// A 4×6 px pointer centered on `c`.
fn paint_arrow(ui: &Ui, side: Arrow, c: Pos2, color: Color32) {
    let (hw, hh) = (2.0, 3.0);
    let pts = match side {
        Arrow::Left => vec![
            pos2(c.x - hw, c.y),
            pos2(c.x + hw, c.y - hh),
            pos2(c.x + hw, c.y + hh),
        ],
        Arrow::Right => vec![
            pos2(c.x + hw, c.y),
            pos2(c.x - hw, c.y + hh),
            pos2(c.x - hw, c.y - hh),
        ],
    };
    ui.painter()
        .add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}

pub fn button_at(ui: &mut Ui, rect: Rect, id: Id, text: &str, style: &ButtonStyle) -> Response {
    let sense = if style.disabled {
        Sense::hover()
    } else {
        Sense::click()
    };
    let resp = ui.interact(rect, id, sense);
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let hovered = resp.hovered();
    let pressed = resp.is_pointer_button_down_on();
    let (bg, border, fg) = if style.disabled {
        (theme::BUTTON_BG, theme::BUTTON_BORDER, theme::TEXT_MUTED)
    } else if style.accent {
        let bg = if pressed {
            theme::ACCENT_PRESSED
        } else if hovered {
            theme::ACCENT
        } else {
            theme::ACCENT_BUTTON
        };
        (bg, Color32::TRANSPARENT, theme::WINDOW_BG)
    } else if pressed {
        (
            theme::BUTTON_PRESSED_BG,
            theme::ACCENT_BUTTON,
            theme::TEXT_STRONG,
        )
    } else if hovered {
        (theme::BUTTON_HOVER_BG, theme::ACCENT, theme::TEXT_STRONG)
    } else {
        (theme::BUTTON_BG, theme::BUTTON_BORDER, theme::TEXT_BRIGHT)
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(6),
        bg,
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
    let (arrow, label) = split_arrow(text);
    let g = layout_text(ui, label, style.font.clone(), fg);
    let arrow_w = if arrow.is_some() { ARROW_ADVANCE } else { 0.0 };
    let left = rect.center().x - (g.size().x + arrow_w) / 2.0;
    let text_x = if arrow == Some(Arrow::Left) {
        left + arrow_w
    } else {
        left
    };
    if let Some(side) = arrow {
        let ax = if side == Arrow::Left {
            left
        } else {
            left + g.size().x
        };
        let c = pos2(ax + ARROW_ADVANCE / 2.0, rect.center().y + 0.5);
        paint_arrow(ui, side, c, fg);
    }
    let pos = pos2(text_x, rect.center().y - g.size().y / 2.0);
    ui.painter().galley(pos.round(), g, fg);
    resp
}

// ---------------------------------------------------------------------------------------------
// Checkbox

/// Indicator box incl. its 1 px border (Qt: 16 px + border).
const CHECK_BOX: f32 = 18.0;
const CHECK_SPACING: f32 = 8.0;

pub fn checkbox_size(ui: &Ui, text: &str, font: &FontId) -> Vec2 {
    let g = layout_text(ui, text, font.clone(), theme::TEXT);
    let w = CHECK_BOX + CHECK_SPACING + qt_text_width(ui, text, font);
    vec2(w, CHECK_BOX.max(g.size().y))
}

/// Returns true when toggled.
pub fn checkbox_at(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    checked: &mut bool,
    text: &str,
    font: &FontId,
) -> bool {
    let resp = ui.interact(rect, id, Sense::click());
    let boxr = Rect::from_min_size(
        pos2(rect.left(), rect.center().y - CHECK_BOX / 2.0),
        Vec2::splat(CHECK_BOX),
    );
    let (fill, border) = if *checked {
        (theme::ACCENT_BUTTON, theme::ACCENT)
    } else if resp.hovered() {
        (theme::BUTTON_BG, theme::ACCENT)
    } else {
        (theme::BUTTON_BG, theme::BUTTON_BORDER)
    };
    let painter = ui.painter();
    let boxr = Rect::from_min_max(boxr.min.round(), boxr.max.round());
    painter.rect(
        boxr,
        CornerRadius::same(4),
        fill,
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
    let g = layout_text(ui, text, font.clone(), theme::TEXT);
    let pos = pos2(
        boxr.right() + CHECK_SPACING,
        rect.center().y - g.size().y / 2.0,
    );
    ui.painter().galley(pos.round(), g, theme::TEXT);
    if resp.clicked() {
        *checked = !*checked;
        return true;
    }
    false
}

// ---------------------------------------------------------------------------------------------
// Group box

/// Height of the title strip above the frame (`QGroupBox { margin-top: 15px }`).
pub const GROUP_TITLE_H: f32 = 15.0;
/// Content inset from the frame's outer edge: top (padding-top 15 + layout margin), sides, bottom.
pub const GROUP_INSET_TOP: f32 = 25.0;
pub const GROUP_INSET_SIDE: f32 = 10.0;
pub const GROUP_INSET_BOTTOM: f32 = 10.0;

/// Total vertical chrome of a group box around content of the given height.
pub fn group_chrome_height() -> f32 {
    GROUP_TITLE_H + GROUP_INSET_TOP + GROUP_INSET_BOTTOM
}

/// Paint a `QGroupBox` filling `rect` and return the content rect: upper-case cyan title
/// (bold 12 px, 1 px letter spacing) in a #11131A box sitting on the frame's top edge.
pub fn group_box(ui: &Ui, rect: Rect, title: &str) -> Rect {
    let frame = Rect::from_min_max(pos2(rect.left(), rect.top() + GROUP_TITLE_H), rect.max);
    let painter = ui.painter();
    painter.rect(
        frame,
        CornerRadius::same(8),
        theme::PANEL_BG,
        Stroke::new(1.0, theme::BORDER),
        StrokeKind::Inside,
    );

    let mut job = LayoutJob::single_section(
        title.to_uppercase(),
        TextFormat {
            font_id: fonts::bold(12.0),
            color: theme::ACCENT,
            extra_letter_spacing: 1.0,
            ..Default::default()
        },
    );
    job.wrap.max_width = (rect.width() - 34.0).max(10.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let g = painter.layout_job(job);
    let title_rect = Rect::from_min_size(
        pos2(rect.left() + 12.0, rect.top()),
        vec2(g.size().x + 12.0, GROUP_TITLE_H),
    );
    painter.rect_filled(title_rect, 0.0, theme::PANEL_BG);
    let pos = pos2(
        title_rect.left() + 7.0,
        title_rect.center().y - g.size().y / 2.0,
    );
    painter.galley(pos.round(), g, theme::ACCENT);

    Rect::from_min_max(
        pos2(
            frame.left() + GROUP_INSET_SIDE,
            frame.top() + GROUP_INSET_TOP,
        ),
        pos2(
            frame.right() - GROUP_INSET_SIDE,
            frame.bottom() - GROUP_INSET_BOTTOM,
        ),
    )
}

// ---------------------------------------------------------------------------------------------
// Combo box

const COMBO_PAD_X: f32 = 12.0;
const COMBO_PAD_Y: f32 = 7.0;
const COMBO_ARROW_W: f32 = 20.0;
const COMBO_ITEM_H: f32 = 26.0;

pub fn combo_font() -> FontId {
    fonts::regular(12.0)
}

pub fn combo_size(ui: &Ui, items: &[&str]) -> Vec2 {
    let font = combo_font();
    let (mut w, mut h) = (0.0f32, 0.0f32);
    for it in items {
        let g = layout_text(ui, it, font.clone(), theme::TEXT_BRIGHT);
        w = w.max(qt_text_width(ui, it, &font));
        h = h.max(g.size().y);
    }
    // Border plus one pixel Qt's combo adds next to the drop-down area.
    vec2(
        w + 2.0 * COMBO_PAD_X + COMBO_ARROW_W + 3.0,
        h + 2.0 * COMBO_PAD_Y + 2.0,
    )
}

/// `QComboBox` look; returns the newly chosen index.
pub fn combo_at(ui: &mut Ui, rect: Rect, id: Id, selected: usize, items: &[&str]) -> Option<usize> {
    combo(ui, rect, id, selected, items).filter(|&i| i != selected)
}

/// [`combo_at`] that also reports picking the current item again (an action, not a state).
pub fn combo_pick_at(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    selected: usize,
    items: &[&str],
) -> Option<usize> {
    combo(ui, rect, id, selected, items)
}

fn combo(ui: &mut Ui, rect: Rect, id: Id, selected: usize, items: &[&str]) -> Option<usize> {
    let resp = ui.interact(rect, id, Sense::click());
    let open = Popup::is_id_open(ui.ctx(), id.with("popup"));
    let border = if resp.hovered() || open {
        theme::ACCENT
    } else {
        theme::BUTTON_BORDER
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(6),
        theme::BUTTON_BG,
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
    let label = items.get(selected).copied().unwrap_or_default();
    let g = layout_text(ui, label, combo_font(), theme::TEXT_BRIGHT);
    let pos = pos2(
        rect.left() + 1.0 + COMBO_PAD_X,
        rect.center().y - g.size().y / 2.0,
    );
    ui.painter()
        .with_clip_rect(rect.shrink(1.0))
        .galley(pos.round(), g, theme::TEXT_BRIGHT);

    let mut chosen = None;
    let frame = egui::Frame::new()
        .fill(theme::COMBO_POPUP_BG)
        .stroke(Stroke::new(1.0, theme::BUTTON_BORDER))
        .inner_margin(egui::Margin::same(1));
    Popup::from_toggle_button_response(&resp)
        .id(id.with("popup"))
        .frame(frame)
        .gap(2.0)
        .width(rect.width() - 2.0)
        .close_behavior(PopupCloseBehavior::CloseOnClick)
        .show(|ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            let any_hovered = ui.rect_contains_pointer(ui.max_rect());
            for (i, item) in items.iter().enumerate() {
                let (r, item_resp) = ui
                    .allocate_exact_size(vec2(ui.available_width(), COMBO_ITEM_H), Sense::click());
                let highlighted = if any_hovered {
                    item_resp.hovered()
                } else {
                    i == selected
                };
                let color = if highlighted {
                    theme::ACCENT
                } else {
                    theme::COMBO_POPUP_TEXT
                };
                if highlighted {
                    ui.painter().rect_filled(r, 0.0, theme::BUTTON_HOVER_BG);
                }
                let g = layout_text(ui, item, combo_font(), color);
                let pos = pos2(
                    r.left() + COMBO_PAD_X - 1.0,
                    r.center().y - g.size().y / 2.0,
                );
                ui.painter().galley(pos.round(), g, color);
                if item_resp.clicked() {
                    chosen = Some(i);
                }
            }
        });
    chosen
}

// ---------------------------------------------------------------------------------------------
// Misc

/// Width taken by a deck separator (`QFrame::VLine`).
pub const SEPARATOR_W: f32 = 3.0;

pub fn vseparator(ui: &Ui, rect: Rect) {
    let line = Rect::from_min_size(pos2(rect.left(), rect.top()), vec2(2.0, rect.height()));
    ui.painter().rect_filled(line, 0.0, theme::DECK_SEPARATOR);
}

/// The grid's determinate progress bar (18 px, text centered).
pub fn progress_bar(ui: &Ui, rect: Rect, done: usize, total: usize, label: &str) {
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, theme::PANEL_BG);
    let frac = if total == 0 {
        0.0
    } else {
        (done as f32 / total as f32).clamp(0.0, 1.0)
    };
    let inner = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - 1.0));
    if frac > 0.0 {
        let chunk = Rect::from_min_size(inner.min, vec2(inner.width() * frac, inner.height()));
        painter.rect_filled(chunk, 0.0, theme::ACCENT);
    }
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, theme::BORDER_SUBTLE),
    );
    painter.text(
        inner.center(),
        Align2::CENTER_CENTER,
        label,
        fonts::regular(10.0),
        theme::TEXT_MUTED,
    );
}

/// Indeterminate "busy" bar: a chunk sliding left to right, as Qt draws `setRange(0, 0)`.
pub fn busy_bar(ui: &Ui, rect: Rect, time: f64) {
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, CornerRadius::same(2), theme::LOADING_TRACK);
    let chunk_w = rect.width() * 0.35;
    let period = 1.6;
    let t = (time % period / period) as f32;
    let x = rect.left() - chunk_w + t * (rect.width() + chunk_w);
    let chunk = Rect::from_min_size(pos2(x, rect.top()), vec2(chunk_w, rect.height()));
    painter.rect_filled(chunk, CornerRadius::same(2), theme::ACCENT);
}

/// Single-line text truncated with `…` at the right edge (Qt `ElideRight`).
pub fn elided_right(ui: &Ui, text: &str, font: FontId, color: Color32, max_w: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap.max_width = max_w.max(1.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.halign = Align::LEFT;
    ui.painter().layout_job(job)
}
