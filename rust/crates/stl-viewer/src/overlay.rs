//! Loading overlay over the 3D page (app.py `LoadingOverlay`): a 72 % veil and a centered
//! card with ⚡, an indeterminate progress bar and "Parsing {name} ...".

use egui::{CornerRadius, Id, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use crate::{emoji, fonts, icons, theme, widgets};

/// Card chrome on each side: 1.5 px border, 25 px padding and the 9 px layout margin.
const INSET: f32 = 1.5 + 25.0 + 9.0;
/// `max-width: 280px` (layout margins included) plus padding and border: the card always
/// takes its maximum width in the Qt app.
const CARD_W: f32 = 280.0 + 2.0 * (25.0 + 1.5);
const SPACING: f32 = 12.0;
const BOLT_SIZE: f32 = 36.0;
/// `margin-bottom: 5px` under the bolt.
const BOLT_MARGIN: f32 = 5.0;
const BAR: egui::Vec2 = vec2(160.0, 4.0);

pub fn show(ui: &mut Ui, rect: Rect, message: &str) {
    // Swallow pointer input so the viewport underneath can't be dragged while loading.
    ui.interact(rect, Id::new("loading_overlay"), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::OVERLAY_VEIL);

    let card_w = CARD_W.min(rect.width() - 8.0).max(2.0 * INSET);
    let label = widgets::elided_right(
        ui,
        message,
        fonts::bold(13.0),
        theme::TEXT_CARD,
        card_w - 2.0 * INSET,
    );
    let bolt_h = emoji::box_size(BOLT_SIZE).y.round();
    let label_h = label.size().y.round();
    let content_h = bolt_h + BOLT_MARGIN + SPACING + BAR.y + SPACING + label_h;
    let card = Rect::from_center_size(rect.center(), vec2(card_w, content_h + 2.0 * INSET));
    let card = Rect::from_min_max(card.min.round(), card.max.round());
    painter.rect(
        card,
        CornerRadius::same(12),
        theme::LOADING_CARD_BG,
        Stroke::new(1.5, theme::ACCENT),
        StrokeKind::Inside,
    );

    let cx = card.center().x;
    let mut y = card.top() + INSET;
    emoji::paint(
        ui,
        icons::BOLT,
        BOLT_SIZE,
        pos2(cx, y + bolt_h / 2.0),
        theme::ACCENT,
    );
    y += bolt_h + BOLT_MARGIN + SPACING;
    let bar = Rect::from_min_size(pos2((cx - BAR.x / 2.0).round(), y), BAR);
    widgets::busy_bar(ui, bar, ui.input(|i| i.time));
    y += BAR.y + SPACING;
    let pos = pos2(cx - label.size().x / 2.0, y);
    ui.painter()
        .with_clip_rect(card)
        .galley(pos.round(), label, theme::TEXT_CARD);
    ui.ctx().request_repaint();
}
