//! Slicer-style vertical range slider over the 3D view: the upper handle cuts the model
//! from the top, the lower one from the bottom (a z cross-section). Values are shown as
//! heights above the model's base, so the bottom always reads 0.

use egui::{CornerRadius, CursorIcon, Id, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use crate::{fonts, theme};

/// Distance from the view's right edge to the track's centre line.
const RIGHT_INSET: f32 = 34.0;
/// Room above and below the track for the handles' value labels.
const END_INSET: f32 = 46.0;
const TRACK_W: f32 = 4.0;
const HANDLE: egui::Vec2 = vec2(22.0, 12.0);
/// How far (px) from a handle's centre a press still grabs it.
const GRAB_REACH: f32 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Handle {
    Lo,
    Hi,
}

/// The handle being dragged; `Either` while the handles overlap and the drag has not moved
/// yet (the first movement decides: up takes the top handle, down the bottom one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Grab {
    One(Handle),
    Either,
}

/// Handle positions as fractions of the model's z extent (0 = bottom, 1 = top).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZSlider {
    lo: f32,
    hi: f32,
    grab: Option<Grab>,
}

impl Default for ZSlider {
    fn default() -> Self {
        Self {
            lo: 0.0,
            hi: 1.0,
            grab: None,
        }
    }
}

impl ZSlider {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The z range to keep for a model spanning `z`, or `None` when nothing is cut.
    pub fn range(&self, z: [f64; 2]) -> Option<[f64; 2]> {
        if self.lo <= 0.0 && self.hi >= 1.0 {
            return None;
        }
        let at = |f: f32| z[0] + (z[1] - z[0]) * f64::from(f);
        Some([at(self.lo), at(self.hi)])
    }

    /// Which handle a press at `y` grabs, given the handles' screen positions.
    fn grab_at(y: f32, y_lo: f32, y_hi: f32) -> Option<Grab> {
        let (d_lo, d_hi) = ((y - y_lo).abs(), (y - y_hi).abs());
        if d_lo > GRAB_REACH && d_hi > GRAB_REACH {
            return None;
        }
        if (y_lo - y_hi).abs() < HANDLE.y {
            return Some(Grab::Either);
        }
        Some(Grab::One(if d_hi < d_lo { Handle::Hi } else { Handle::Lo }))
    }

    fn set(&mut self, handle: Handle, f: f32) {
        match handle {
            Handle::Hi => self.hi = f.max(self.lo),
            Handle::Lo => self.lo = f.min(self.hi),
        }
    }

    /// Draw over `view` (the 3D viewport rect) for a model spanning `z`; returns true when
    /// a handle moved.
    pub fn show(&mut self, ui: &mut Ui, view: Rect, z: [f64; 2]) -> bool {
        let x = view.right() - RIGHT_INSET;
        let top = view.top() + END_INSET;
        let bottom = view.bottom() - END_INSET;
        if bottom - top < 40.0 {
            return false;
        }
        let y_of = |f: f32| bottom - (bottom - top) * f;
        let f_of = |y: f32| ((bottom - y) / (bottom - top)).clamp(0.0, 1.0);
        let (lo0, hi0) = (self.lo, self.hi);

        // One interaction for the whole slider, so overlapping handles stay reachable:
        // press on a handle and drag it, click the track to move the nearest handle there,
        // double-click to reset.
        let area = Rect::from_x_y_ranges(
            x - HANDLE.x / 2.0 - 4.0..=x + HANDLE.x / 2.0 + 4.0,
            top - HANDLE.y..=bottom + HANDLE.y,
        );
        let resp = ui.interact(area, Id::new("zslider"), Sense::click_and_drag());
        let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
        // egui reports the drag start only after the pointer moved past a threshold, so grab
        // whatever was under the press itself.
        let press = ui.input(|i| i.pointer.press_origin()).or(pointer);
        if resp.drag_started()
            && let Some(p) = press
        {
            self.grab = Self::grab_at(p.y, y_of(self.lo), y_of(self.hi)).or_else(|| {
                // Pressed on the bare track: take the nearest handle.
                let f = f_of(p.y);
                let (d_hi, d_lo) = ((f - self.hi).abs(), (f - self.lo).abs());
                Some(if (d_hi - d_lo).abs() < f32::EPSILON {
                    Grab::Either
                } else if d_hi < d_lo {
                    Grab::One(Handle::Hi)
                } else {
                    Grab::One(Handle::Lo)
                })
            });
        }
        if resp.dragged()
            && let Some(p) = pointer
        {
            let dy = resp.drag_delta().y;
            if self.grab == Some(Grab::Either) && dy != 0.0 {
                self.grab = Some(Grab::One(if dy < 0.0 { Handle::Hi } else { Handle::Lo }));
            }
            if let Some(Grab::One(h)) = self.grab {
                self.set(h, f_of(p.y));
            }
        }
        if resp.drag_stopped() {
            self.grab = None;
        }
        if resp.double_clicked() {
            self.reset();
        } else if resp.clicked()
            && let Some(p) = pointer
        {
            let f = f_of(p.y);
            let take_hi = if (self.hi - self.lo).abs() < f32::EPSILON {
                f > self.hi
            } else {
                (f - self.hi).abs() < (f - self.lo).abs()
            };
            self.set(if take_hi { Handle::Hi } else { Handle::Lo }, f);
        }
        let hover_handle = pointer
            .filter(|_| resp.hovered() || resp.dragged())
            .and_then(|p| Self::grab_at(p.y, y_of(self.lo), y_of(self.hi)));
        if resp.dragged() || hover_handle.is_some() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
        }

        let painter = ui.painter();
        painter.rect_filled(
            Rect::from_x_y_ranges(x - TRACK_W / 2.0..=x + TRACK_W / 2.0, top..=bottom),
            CornerRadius::same(2),
            theme::SCROLL_HANDLE,
        );
        painter.rect_filled(
            Rect::from_x_y_ranges(
                x - TRACK_W / 2.0..=x + TRACK_W / 2.0,
                y_of(self.hi)..=y_of(self.lo),
            ),
            CornerRadius::same(2),
            theme::ACCENT_BUTTON,
        );
        let height = z[1] - z[0];
        for handle in [Handle::Lo, Handle::Hi] {
            let f = if handle == Handle::Hi {
                self.hi
            } else {
                self.lo
            };
            let r = Rect::from_center_size(pos2(x, y_of(f)), HANDLE);
            let active = self.grab == Some(Grab::One(handle))
                || matches!(hover_handle, Some(Grab::One(h)) if h == handle)
                || hover_handle == Some(Grab::Either);
            painter.rect(
                r,
                CornerRadius::same(4),
                if active {
                    theme::ACCENT
                } else {
                    theme::BUTTON_BG
                },
                Stroke::new(1.5, theme::ACCENT),
                StrokeKind::Inside,
            );
            // Height above the model's base, left of the handle (below the bottom one,
            // above the top one); rounded so it never shows "-0.00".
            let value = (height * f64::from(f) * 100.0).round() / 100.0 + 0.0;
            let galley = painter.layout_no_wrap(
                format!("{value:.2}"),
                fonts::bold(11.0),
                theme::TEXT_STRONG,
            );
            let pad = vec2(6.0, 3.0);
            let size = galley.size() + 2.0 * pad;
            let dy = if handle == Handle::Hi { -16.0 } else { 16.0 };
            let label =
                Rect::from_center_size(pos2(x - size.x / 2.0 + 8.0, r.center().y + dy), size);
            painter.rect_filled(label, CornerRadius::same(4), theme::PANEL_BG);
            painter.galley((label.min + pad).round(), galley, theme::TEXT_STRONG);
        }
        if resp.hovered() && !resp.dragged() {
            resp.on_hover_text(
                "Drag the handles to cut the model from the top or bottom (height above the \
                 model's base, mm). Double-click to reset.",
            );
        }
        (self.lo, self.hi) != (lo0, hi0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_maps_fractions_to_z() {
        let mut s = ZSlider::default();
        assert_eq!(s.range([10.0, 30.0]), None);
        s.hi = 0.5;
        assert_eq!(s.range([10.0, 30.0]), Some([10.0, 20.0]));
        s.lo = 0.25;
        assert_eq!(s.range([10.0, 30.0]), Some([15.0, 20.0]));
        s.reset();
        assert_eq!(s.range([10.0, 30.0]), None);
    }

    #[test]
    fn overlapping_handles_are_both_reachable() {
        // Top handle dragged onto the bottom one: a press there grabs "either"...
        assert_eq!(ZSlider::grab_at(500.0, 500.0, 500.0), Some(Grab::Either));
        assert_eq!(ZSlider::grab_at(503.0, 500.0, 499.0), Some(Grab::Either));
        // ...apart, the nearest one; away from both, none.
        assert_eq!(
            ZSlider::grab_at(100.0, 500.0, 98.0),
            Some(Grab::One(Handle::Hi))
        );
        assert_eq!(
            ZSlider::grab_at(497.0, 500.0, 98.0),
            Some(Grab::One(Handle::Lo))
        );
        assert_eq!(ZSlider::grab_at(300.0, 500.0, 98.0), None);
        // Handles never cross.
        let mut s = ZSlider::default();
        s.set(Handle::Hi, 0.0);
        assert_eq!((s.lo, s.hi), (0.0, 0.0));
        s.set(Handle::Lo, 0.5);
        assert_eq!(s.lo, 0.0);
        s.set(Handle::Hi, 0.7);
        assert_eq!((s.lo, s.hi), (0.0, 0.7));
    }
}
