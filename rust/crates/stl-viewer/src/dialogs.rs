//! Modal dialogs: the "Loading Error" message box and the "Select Model Color" picker.

use egui::color_picker::{Alpha, color_picker_color32};
use egui::{
    Align2, Color32, CornerRadius, Frame, Id, Key, Margin, Modal, Rect, Sense, Stroke, StrokeKind,
    Ui, Vec2, pos2, vec2,
};

use crate::widgets::{ButtonStyle, button_at, button_size};
use crate::{fonts, text, theme};

/// Qt dialogs appear at once: no egui fade-in.
fn modal(id: Id) -> Modal {
    Modal::new(id)
        .area(Modal::default_area(id).fade_in(false))
        .frame(dialog_frame())
        .backdrop_color(Color32::from_black_alpha(110))
}

fn dialog_frame() -> Frame {
    Frame::new()
        .fill(theme::SIDEBAR_BG)
        .stroke(Stroke::new(1.0, theme::BUTTON_BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(20))
        .shadow(egui::Shadow {
            offset: [0, 8],
            blur: 24,
            spread: 0,
            color: Color32::from_black_alpha(140),
        })
}

fn title(ui: &mut Ui, s: &str) {
    let g = ui
        .painter()
        .layout_no_wrap(s.to_string(), fonts::bold(15.0), theme::TEXT_STRONG);
    let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
    ui.painter().galley(r.min, g, theme::TEXT_STRONG);
}

/// Right-aligned row of buttons; returns the index of the clicked one.
fn button_row(ui: &mut Ui, id: Id, labels: &[&str]) -> Option<usize> {
    let style = ButtonStyle::normal();
    let sizes: Vec<Vec2> = labels
        .iter()
        .map(|l| button_size(ui, l, &style.clone().width(button_w(ui, l))))
        .collect();
    let h = sizes.iter().map(|s| s.y).fold(0.0, f32::max);
    let total: f32 =
        sizes.iter().map(|s| s.x).sum::<f32>() + 8.0 * (labels.len().saturating_sub(1)) as f32;
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width().max(total), h), Sense::hover());
    let mut x = row.right() - total;
    let mut clicked = None;
    for (i, (label, size)) in labels.iter().zip(&sizes).enumerate() {
        let r = Rect::from_min_size(pos2(x, row.top()), *size);
        if button_at(ui, r, id.with(i), label, &style).clicked() {
            clicked = Some(i);
        }
        x += size.x + 8.0;
    }
    clicked
}

fn button_w(ui: &Ui, label: &str) -> f32 {
    button_size(ui, label, &ButtonStyle::normal()).x.max(84.0)
}

/// Python `QMessageBox.critical(self, "Loading Error", "Failed to load the 3D model:\n{err}")`.
/// Returns false once the dialog was dismissed.
pub fn error_dialog(ctx: &egui::Context, message: &str) -> bool {
    let id = Id::new("loading_error_dialog");
    let mut keep_open = true;
    let resp = modal(id).show(ctx, |ui| {
        ui.set_max_width(460.0);
        ui.spacing_mut().item_spacing = vec2(12.0, 14.0);
        title(ui, text::LOADING_ERROR_TITLE);
        ui.horizontal_top(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::hover());
            ui.painter()
                .circle_filled(r.center(), 16.0, theme::ERROR_RED);
            ui.painter().text(
                r.center(),
                Align2::CENTER_CENTER,
                "\u{2715}",
                fonts::bold(16.0),
                Color32::WHITE,
            );
            let g = ui.painter().layout(
                message.to_string(),
                fonts::regular(13.0),
                theme::TEXT_BRIGHT,
                400.0,
            );
            let (tr, _) = ui.allocate_exact_size(g.size(), Sense::hover());
            ui.painter().galley(tr.min, g, theme::TEXT_BRIGHT);
        });
        if button_row(ui, id.with("buttons"), &["OK"]).is_some() {
            keep_open = false;
        }
    });
    if resp.should_close() || ctx.input(|i| i.key_pressed(Key::Enter)) {
        keep_open = false;
    }
    keep_open
}

pub enum ColorDialogResult {
    Open,
    Accepted(Color32),
    Cancelled,
}

/// QColorDialog replacement: `color` is the working value, initialised by the caller.
pub fn color_dialog(
    ctx: &egui::Context,
    color: &mut Color32,
    initial: Color32,
) -> ColorDialogResult {
    let id = Id::new("model_color_dialog");
    let mut result = ColorDialogResult::Open;
    let resp = modal(id).show(ctx, |ui| {
        theme::dialog_visuals(ui.visuals_mut());
        ui.spacing_mut().item_spacing = vec2(10.0, 12.0);
        ui.spacing_mut().slider_width = 260.0;
        title(ui, text::COLOR_DIALOG_TITLE);
        color_picker_color32(ui, color, Alpha::Opaque);
        ui.horizontal(|ui| {
            swatch(ui, initial, "Current");
            swatch(ui, *color, "New");
            let [r, g, b, _] = color.to_array();
            ui.label(
                egui::RichText::new(format!("#{r:02X}{g:02X}{b:02X}"))
                    .font(fonts::bold(12.0))
                    .color(theme::TEXT_BRIGHT),
            );
        });
        match button_row(ui, id.with("buttons"), &["OK", "Cancel"]) {
            Some(0) => result = ColorDialogResult::Accepted(*color),
            Some(_) => result = ColorDialogResult::Cancelled,
            None => {}
        }
    });
    if matches!(result, ColorDialogResult::Open) {
        if ctx.input(|i| i.key_pressed(Key::Enter)) {
            result = ColorDialogResult::Accepted(*color);
        } else if resp.should_close() {
            result = ColorDialogResult::Cancelled;
        }
    }
    result
}

fn swatch(ui: &mut Ui, color: Color32, label: &str) {
    let (r, _) = ui.allocate_exact_size(vec2(44.0, 24.0), Sense::hover());
    ui.painter().rect(
        r,
        CornerRadius::same(4),
        color,
        Stroke::new(1.0, theme::BUTTON_BORDER),
        StrokeKind::Inside,
    );
    ui.label(
        egui::RichText::new(label)
            .font(fonts::regular(12.0))
            .color(theme::TEXT),
    );
}
