//! Corner XYZ orientation marker (vtkOrientationMarkerWidget + vtkAxesActor) as an egui overlay.

use egui::{Color32, Pos2, Rect, Shape, Stroke, pos2};
use glam::DVec3;

use crate::camera::Camera;

/// Fraction of the viewport (width and height) the marker occupies, lower-left corner.
const VIEWPORT_FRACTION: f32 = 0.18;
/// vtkAxesActor: shafts are lines up to 0.8, cones (radius 0.4 x tip length 0.2) to 1.0.
const SHAFT_LENGTH: f64 = 0.8;
const CONE_RADIUS: f64 = 0.08;
/// vtkCaptionActor2D box height as a fraction of the marker viewport, its padding and the
/// text actor's minimum size, in pixels.
const LABEL_BOX_FRACTION: f32 = 0.1;
const LABEL_PADDING_PX: f32 = 3.0;
const LABEL_MIN_BOX_PX: f32 = 10.0;
/// Measured from VTK 9.5 renders: cap height of the fitted bold italic Arial caption relative
/// to the text box height, and where the glyph ink starts relative to the box corner.
const LABEL_CAP_PER_BOX: f32 = 0.82;
const LABEL_INK_OFFSET_PX: egui::Vec2 = egui::vec2(1.0, 2.0);

const SHAFT_COLORS: [[u8; 3]; 3] = [[230, 0, 0], [0, 205, 0], [0, 0, 230]];
const CONE_COLORS: [[u8; 3]; 3] = [[235, 12, 12], [8, 175, 8], [10, 10, 200]];

fn color(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

fn label_color(rgb: [f32; 3]) -> Color32 {
    let c = rgb.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
    Color32::from_rgb(c[0], c[1], c[2])
}

/// Perspective projection of the marker camera: same orientation as the scene camera, reset
/// (VTK `ResetCamera`) to frame the axes actor's symmetric [-1, 1]^3 bounds.
struct MarkerProjection {
    right: DVec3,
    up: DVec3,
    back: DVec3,
    eye: DVec3,
    tan_half_fov: f64,
    aspect: f64,
    region: Rect,
}

impl MarkerProjection {
    fn new(camera: &Camera, region: Rect) -> Self {
        let aspect = f64::from(region.width() / region.height().max(1e-3));
        let view_angle = camera.view_angle_deg.to_radians();
        let mut fit_angle = view_angle;
        if aspect < 1.0 {
            fit_angle = 2.0 * ((fit_angle * 0.5).tan() * aspect).atan();
        }
        let radius = 3f64.sqrt();
        let distance = radius / (fit_angle * 0.5).sin();
        let (right, up, back) = camera.basis();
        Self {
            right,
            up,
            back,
            eye: back * distance,
            tan_half_fov: (view_angle * 0.5).tan(),
            aspect,
            region,
        }
    }

    /// Screen position and view depth of a marker-space point.
    fn project(&self, p: DVec3) -> (Pos2, f64) {
        let v = p - self.eye;
        let depth = -v.dot(self.back);
        let x = v.dot(self.right) / (depth * self.tan_half_fov * self.aspect);
        let y = v.dot(self.up) / (depth * self.tan_half_fov);
        let c = self.region.center();
        let pos = pos2(
            c.x + x as f32 * self.region.width() / 2.0,
            c.y - y as f32 * self.region.height() / 2.0,
        );
        (pos, depth)
    }
}

/// Convex hull (monotone chain) of a small point set, counter-clockwise on screen.
fn convex_hull(mut pts: Vec<Pos2>) -> Vec<Pos2> {
    pts.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let cross = |o: Pos2, a: Pos2, b: Pos2| (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x);
    let mut hull: Vec<Pos2> = Vec::with_capacity(pts.len() * 2);
    for pass in 0..2 {
        let start = hull.len();
        let iter: Box<dyn Iterator<Item = &Pos2>> = if pass == 0 {
            Box::new(pts.iter())
        } else {
            Box::new(pts.iter().rev())
        };
        for &p in iter {
            while hull.len() >= start + 2
                && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0
            {
                hull.pop();
            }
            hull.push(p);
        }
        hull.pop();
    }
    hull
}

/// Draw the marker into the lower-left 18% x 18% of `rect` (in points).
pub(crate) fn paint(painter: &egui::Painter, rect: Rect, camera: &Camera, pixels_per_point: f32) {
    let region = Rect::from_min_max(
        pos2(
            rect.left(),
            rect.bottom() - rect.height() * VIEWPORT_FRACTION,
        ),
        pos2(
            rect.left() + rect.width() * VIEWPORT_FRACTION,
            rect.bottom(),
        ),
    );
    if region.width() < 4.0 || region.height() < 4.0 {
        return;
    }
    let proj = MarkerProjection::new(camera, region);
    let px = 1.0 / pixels_per_point.max(1e-3);
    let painter = painter.with_clip_rect(rect.intersect(region));

    // Farthest axis first so nearer ones overlap it.
    let mut order: Vec<usize> = (0..3).collect();
    let depth_of = |i: usize| proj.project(DVec3::AXES[i]).1;
    order.sort_by(|&a, &b| depth_of(b).total_cmp(&depth_of(a)));
    let (origin, _) = proj.project(DVec3::ZERO);
    for &i in &order {
        let axis = DVec3::AXES[i];
        let (base, _) = proj.project(axis * SHAFT_LENGTH);
        painter.line_segment(
            [origin, base],
            Stroke::new(1.2 * px, color(SHAFT_COLORS[i])),
        );

        let (u, v) = axis.any_orthonormal_pair();
        let mut pts: Vec<Pos2> = (0..16)
            .map(|k| {
                let a = f64::from(k) * std::f64::consts::TAU / 16.0;
                proj.project(axis * SHAFT_LENGTH + (u * a.cos() + v * a.sin()) * CONE_RADIUS)
                    .0
            })
            .collect();
        pts.push(proj.project(axis).0);
        let hull = convex_hull(pts);
        if hull.len() >= 3 {
            painter.add(Shape::convex_polygon(
                hull,
                color(CONE_COLORS[i]),
                Stroke::NONE,
            ));
        }
    }

    // Captions are 2D overlay actors: drawn after (on top of) every axis.
    let labels = [
        ("X", stlv_core::palette::AXIS_LABEL_X),
        ("Y", stlv_core::palette::AXIS_LABEL_Y),
        ("Z", stlv_core::palette::AXIS_LABEL_Z),
    ];
    let box_px = (region.height() * pixels_per_point * LABEL_BOX_FRACTION - 2.0 * LABEL_PADDING_PX)
        .max(LABEL_MIN_BOX_PX);
    let cap_height = box_px * LABEL_CAP_PER_BOX * px;
    for i in order {
        let (name, rgb) = labels[i];
        let (base, _) = proj.project(DVec3::AXES[i] * SHAFT_LENGTH);
        let ink_corner = base
            + (egui::vec2(LABEL_PADDING_PX, -LABEL_PADDING_PX)
                + LABEL_INK_OFFSET_PX * egui::vec2(1.0, -1.0))
                * px;
        paint_caption(&painter, name, ink_corner, cap_height, label_color(rgb), px);
    }
}

/// A [`LayoutJob`](egui::text::LayoutJob) for `text` in the proportional font, italic.
fn caption_galley(
    painter: &egui::Painter,
    text: &str,
    size: f32,
    color: Color32,
) -> std::sync::Arc<egui::Galley> {
    let format = egui::TextFormat {
        font_id: egui::FontId::proportional(size),
        color,
        italics: true,
        ..Default::default()
    };
    painter.layout_job(egui::text::LayoutJob::single_section(
        text.to_owned(),
        format,
    ))
}

/// Ink rectangle of the first glyph, relative to the galley origin.
fn ink_rect(galley: &egui::Galley) -> Option<Rect> {
    let row = galley.rows.first()?;
    let glyph = row.row.glyphs.first()?;
    let min = row.pos + glyph.pos.to_vec2() + glyph.uv_rect.offset;
    Some(Rect::from_min_size(min, glyph.uv_rect.size))
}

/// VTK caption text (bold italic, drop shadow) whose glyph ink has its lower-left corner at
/// `ink_corner` and is `cap_height` tall (points), whatever proportional font is installed.
fn paint_caption(
    painter: &egui::Painter,
    text: &str,
    ink_corner: Pos2,
    cap_height: f32,
    color: Color32,
    px: f32,
) {
    // Scale from the font's real cap height (the egui font size is a line height).
    const PROBE_SIZE: f32 = 32.0;
    let Some(probe) = ink_rect(&caption_galley(painter, "X", PROBE_SIZE, color)) else {
        return;
    };
    if probe.height() <= 0.0 {
        return;
    }
    let size = (cap_height * PROBE_SIZE / probe.height()).clamp(4.0, 200.0);
    let shadow = caption_galley(painter, text, size, Color32::BLACK);
    let galley = caption_galley(painter, text, size, color);
    let Some(ink) = ink_rect(&galley) else { return };
    let origin = ink_corner - ink.left_bottom().to_vec2();
    // vtkTextProperty shadow offset (1, -1) px; bold approximated by a 1 px double strike.
    let strokes = [
        (egui::vec2(px, px), &shadow),
        (egui::vec2(2.0 * px, px), &shadow),
    ];
    for (offset, g) in strokes {
        painter.galley(origin + offset, g.clone(), Color32::BLACK);
    }
    for dx in [0.0, px] {
        painter.galley(origin + egui::vec2(dx, 0.0), galley.clone(), color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hull_of_square_with_inner_point() {
        let hull = convex_hull(vec![
            pos2(0.0, 0.0),
            pos2(1.0, 0.0),
            pos2(1.0, 1.0),
            pos2(0.0, 1.0),
            pos2(0.5, 0.5),
        ]);
        assert_eq!(hull.len(), 4);
        assert!(!hull.contains(&pos2(0.5, 0.5)));
    }

    #[test]
    fn projection_matches_vtk_marker() {
        // vtkAxesActor in a 288x216 marker viewport, isometric camera (see scratch notes):
        // world (0.8,0,0) -> display (180, 86) and (0,0,1) -> (144, 161), y up from bottom.
        let region = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(288.0, 216.0));
        let cam = Camera {
            position: DVec3::new(0.707, -0.707, 0.707),
            focal_point: DVec3::ZERO,
            view_up: DVec3::Z,
            view_angle_deg: 30.0,
        };
        let proj = MarkerProjection::new(&cam, region);
        let (p, _) = proj.project(DVec3::new(0.8, 0.0, 0.0));
        assert!(
            (p.x - 180.0).abs() < 1.0 && (216.0 - p.y - 86.0).abs() < 1.0,
            "{p:?}"
        );
        let (p, _) = proj.project(DVec3::new(0.0, 0.0, 1.0));
        assert!(
            (p.x - 144.0).abs() < 1.0 && (216.0 - p.y - 161.0).abs() < 1.0,
            "{p:?}"
        );
    }
}
