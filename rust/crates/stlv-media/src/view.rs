//! Zoom / pan geometry of the image viewer (pure math, unit-tested).

use egui::{Rect, Vec2, pos2, vec2};

pub const MIN_ZOOM: f32 = 0.05;
pub const MAX_ZOOM: f32 = 20.0;
/// Zoom factor per wheel notch (app.py `_zoom_image(1.15)`).
pub const WHEEL_STEP: f32 = 1.15;

/// Python: `min((w_area - 4) / w, (h_area - 4) / h)`, then clamped by `_zoom_image(1.0)`.
pub fn fit_zoom(view: Vec2, image: Vec2) -> f32 {
    if image.x <= 0.0 || image.y <= 0.0 {
        return 1.0;
    }
    let z = ((view.x - 4.0) / image.x).min((view.y - 4.0) / image.y);
    if z.is_finite() {
        z.clamp(MIN_ZOOM, MAX_ZOOM)
    } else {
        1.0
    }
}

/// Zoom (points per image pixel) and pan of an image inside a view rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewState {
    pub zoom: f32,
    /// Image top-left relative to the view's top-left, in points.
    pub offset: Vec2,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            offset: Vec2::ZERO,
        }
    }
}

impl ViewState {
    /// Fit `image` into `view` and center it.
    pub fn fitted(view: Vec2, image: Vec2) -> Self {
        let mut v = Self {
            zoom: fit_zoom(view, image),
            offset: Vec2::ZERO,
        };
        v.offset = centered(view, image * v.zoom);
        v
    }

    /// Like a `QScrollArea` with centered alignment: an axis that fits is centered,
    /// one that does not is clamped so no empty band shows at either edge.
    pub fn constrain(&mut self, view: Vec2, image: Vec2) {
        let shown = image * self.zoom;
        let c = centered(view, shown);
        self.offset.x = if shown.x <= view.x {
            c.x
        } else {
            self.offset.x.clamp(view.x - shown.x, 0.0)
        };
        self.offset.y = if shown.y <= view.y {
            c.y
        } else {
            self.offset.y.clamp(view.y - shown.y, 0.0)
        };
    }

    /// Multiply the zoom by `factor` (clamped) keeping the image point under `anchor`
    /// (view-relative, points) fixed.
    pub fn zoom_at(&mut self, factor: f32, anchor: Vec2, view: Vec2, image: Vec2) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let new_zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let image_pt = (anchor - self.offset) / self.zoom;
        self.offset = anchor - image_pt * new_zoom;
        self.zoom = new_zoom;
        self.constrain(view, image);
    }

    pub fn pan(&mut self, delta: Vec2, view: Vec2, image: Vec2) {
        self.offset += delta;
        self.constrain(view, image);
    }

    /// Screen rectangle of the whole image for a view at `view_rect`, with the origin
    /// snapped to physical pixels so 1:1 zoom is crisp.
    pub fn image_rect(&self, view_rect: Rect, image: Vec2, pixels_per_point: f32) -> Rect {
        let ppp = pixels_per_point.max(1e-3);
        let o = view_rect.min + self.offset;
        let o = pos2((o.x * ppp).round() / ppp, (o.y * ppp).round() / ppp);
        Rect::from_min_size(o, image * self.zoom)
    }
}

fn centered(view: Vec2, shown: Vec2) -> Vec2 {
    vec2((view.x - shown.x) * 0.5, (view.y - shown.y) * 0.5)
}

/// Wheel notches in a scroll event: one per line (1/120 wheel delta), 40 points or a
/// quarter page for pixel / page deltas. Positive = scroll up = zoom in.
pub fn wheel_notches(unit: egui::MouseWheelUnit, delta: Vec2) -> f32 {
    match unit {
        egui::MouseWheelUnit::Line => delta.y,
        egui::MouseWheelUnit::Point => delta.y / 40.0,
        egui::MouseWheelUnit::Page => delta.y * 4.0,
    }
}

/// `outer` minus `inner` as up to four non-overlapping rectangles.
pub fn subtract_rect(outer: Rect, inner: Rect) -> Vec<Rect> {
    let inner = outer.intersect(inner);
    if !inner.is_positive() {
        return vec![outer];
    }
    let mut out = Vec::with_capacity(4);
    let mut push = |r: Rect| {
        if r.is_positive() {
            out.push(r);
        }
    };
    push(Rect::from_min_max(
        outer.min,
        pos2(outer.max.x, inner.min.y),
    ));
    push(Rect::from_min_max(
        pos2(outer.min.x, inner.max.y),
        outer.max,
    ));
    push(Rect::from_min_max(
        pos2(outer.min.x, inner.min.y),
        pos2(inner.min.x, inner.max.y),
    ));
    push(Rect::from_min_max(
        pos2(inner.max.x, inner.min.y),
        pos2(outer.max.x, inner.max.y),
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_matches_python() {
        // qt_ref.py on a 960x700 area: test_pattern.bmp (256x256) -> 2.71875.
        assert!((fit_zoom(vec2(960.0, 700.0), vec2(256.0, 256.0)) - 2.71875).abs() < 1e-6);
        assert!((fit_zoom(vec2(960.0, 700.0), vec2(1200.0, 800.0)) - 0.796_666_7).abs() < 1e-6);
        // Clamped to [0.05, 20].
        assert_eq!(fit_zoom(vec2(960.0, 700.0), vec2(1.0, 1.0)), MAX_ZOOM);
        assert_eq!(
            fit_zoom(vec2(960.0, 700.0), vec2(100_000.0, 10.0)),
            MIN_ZOOM
        );
        assert_eq!(fit_zoom(vec2(2.0, 2.0), vec2(10.0, 10.0)), MIN_ZOOM);
        assert_eq!(fit_zoom(vec2(100.0, 100.0), vec2(0.0, 10.0)), 1.0);
    }

    #[test]
    fn small_images_are_centered() {
        let view = vec2(800.0, 600.0);
        let img = vec2(100.0, 50.0);
        let mut v = ViewState {
            zoom: 1.0,
            offset: vec2(-500.0, 900.0),
        };
        v.constrain(view, img);
        assert_eq!(v.offset, vec2(350.0, 275.0));
    }

    #[test]
    fn large_images_are_clamped() {
        let view = vec2(100.0, 100.0);
        let img = vec2(400.0, 50.0);
        let mut v = ViewState {
            zoom: 1.0,
            offset: vec2(30.0, 0.0),
        };
        v.constrain(view, img);
        assert_eq!(v.offset, vec2(0.0, 25.0));
        v.pan(vec2(-1000.0, 0.0), view, img);
        assert_eq!(v.offset.x, -300.0);
    }

    #[test]
    fn zoom_keeps_anchor_fixed() {
        let view = vec2(400.0, 400.0);
        let img = vec2(1000.0, 1000.0);
        let mut v = ViewState {
            zoom: 1.0,
            offset: vec2(-200.0, -300.0),
        };
        let anchor = vec2(150.0, 250.0);
        let before = (anchor - v.offset) / v.zoom;
        v.zoom_at(WHEEL_STEP, anchor, view, img);
        let after = (anchor - v.offset) / v.zoom;
        assert!((before - after).length() < 1e-3);
        assert!((v.zoom - 1.15).abs() < 1e-6);
    }

    #[test]
    fn zoom_is_clamped() {
        let view = vec2(400.0, 400.0);
        let img = vec2(100.0, 100.0);
        let mut v = ViewState::fitted(view, img);
        for _ in 0..100 {
            v.zoom_at(WHEEL_STEP, vec2(10.0, 10.0), view, img);
        }
        assert_eq!(v.zoom, MAX_ZOOM);
        for _ in 0..200 {
            v.zoom_at(1.0 / WHEEL_STEP, vec2(10.0, 10.0), view, img);
        }
        assert_eq!(v.zoom, MIN_ZOOM);
        v.zoom_at(f32::NAN, Vec2::ZERO, view, img);
        assert_eq!(v.zoom, MIN_ZOOM);
    }

    #[test]
    fn subtract_rect_covers_remainder() {
        let outer = Rect::from_min_max(pos2(0.0, 0.0), pos2(10.0, 10.0));
        let inner = Rect::from_min_max(pos2(2.0, 3.0), pos2(6.0, 7.0));
        let parts = subtract_rect(outer, inner);
        assert_eq!(parts.len(), 4);
        let area: f32 = parts.iter().map(|r| r.area()).sum();
        assert!((area - (100.0 - 16.0)).abs() < 1e-4);
        assert_eq!(subtract_rect(outer, outer).len(), 0);
        let disjoint = Rect::from_min_max(pos2(20.0, 20.0), pos2(30.0, 30.0));
        assert_eq!(subtract_rect(outer, disjoint), vec![outer]);
    }
}
