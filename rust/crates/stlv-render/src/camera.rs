use glam::{DMat4, DQuat, DVec3};

/// Camera presets (renderer.py `set_view_preset`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ViewPreset {
    Isometric,
    Top,
    Bottom,
    Front,
    Back,
    Left,
    Right,
}

impl ViewPreset {
    pub const ALL: [Self; 7] = [
        Self::Isometric,
        Self::Top,
        Self::Bottom,
        Self::Front,
        Self::Back,
        Self::Left,
        Self::Right,
    ];

    /// Button caption, e.g. "ISO".
    pub fn label(self) -> &'static str {
        match self {
            Self::Isometric => "ISO",
            Self::Top => "TOP",
            Self::Bottom => "BOTTOM",
            Self::Front => "FRONT",
            Self::Back => "BACK",
            Self::Left => "LEFT",
            Self::Right => "RIGHT",
        }
    }

    /// Parses `iso`/`isometric`, `top`, `bottom`, `front`, `back`, `left`, `right` (any case,
    /// surrounding whitespace ignored).
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name.trim().to_ascii_lowercase().as_str() {
            "iso" | "isometric" => Self::Isometric,
            "top" => Self::Top,
            "bottom" => Self::Bottom,
            "front" => Self::Front,
            "back" => Self::Back,
            "left" => Self::Left,
            "right" => Self::Right,
            _ => return None,
        })
    }
}

/// [`ViewPreset::from_name`] with an error message naming the accepted values.
impl std::str::FromStr for ViewPreset {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        Self::from_name(s).ok_or_else(|| {
            format!(
                "unknown view preset {s:?} (expected iso, top, bottom, front, back, left or right)"
            )
        })
    }
}

/// Perspective camera in VTK terms (position / focal point / view-up / view angle).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub position: DVec3,
    pub focal_point: DVec3,
    pub view_up: DVec3,
    /// Vertical field of view in degrees (VTK default 30).
    pub view_angle_deg: f64,
}

/// VTK `vtkRenderer::ClippingRangeExpansion` default.
const CLIPPING_RANGE_EXPANSION: f64 = 0.5;
/// VTK `NearClippingPlaneTolerance` for depth buffers deeper than 16 bits.
const NEAR_CLIPPING_PLANE_TOLERANCE: f64 = 0.001;

impl Default for Camera {
    /// VTK's default camera: at (0,0,1) looking at the origin, +Y up.
    fn default() -> Self {
        Self {
            position: DVec3::Z,
            focal_point: DVec3::ZERO,
            view_up: DVec3::Y,
            view_angle_deg: 30.0,
        }
    }
}

impl Camera {
    /// Camera looking at the center of `bounds` from `preset`, at 1.85 x the largest extent.
    pub fn for_preset(bounds: &[[f64; 2]; 3], preset: ViewPreset) -> Self {
        let c = DVec3::from(bounds.map(|[lo, hi]| (lo + hi) / 2.0));
        let max_dim = bounds
            .iter()
            .map(|[lo, hi]| hi - lo)
            .fold(f64::NEG_INFINITY, f64::max);
        let d = if max_dim > 0.0 { max_dim * 1.85 } else { 100.0 };
        let (offset, view_up) = match preset {
            ViewPreset::Isometric => (DVec3::new(0.707, -0.707, 0.707) * d, DVec3::Z),
            ViewPreset::Top => (DVec3::new(0.0, 0.0, d), DVec3::Y),
            ViewPreset::Bottom => (DVec3::new(0.0, 0.0, -d), DVec3::NEG_Y),
            ViewPreset::Front => (DVec3::new(0.0, -d, 0.0), DVec3::Z),
            ViewPreset::Back => (DVec3::new(0.0, d, 0.0), DVec3::Z),
            ViewPreset::Left => (DVec3::new(-d, 0.0, 0.0), DVec3::Z),
            ViewPreset::Right => (DVec3::new(d, 0.0, 0.0), DVec3::Z),
        };
        Self {
            position: c + offset,
            focal_point: c,
            view_up,
            view_angle_deg: 30.0,
        }
    }

    pub fn distance(&self) -> f64 {
        self.position.distance(self.focal_point)
    }

    /// Unit vector from the position towards the focal point.
    pub fn direction(&self) -> DVec3 {
        (self.focal_point - self.position).normalize_or(DVec3::NEG_Z)
    }

    /// Orthonormal camera basis `(right, up, back)` exactly as VTK's view transform rows.
    pub fn basis(&self) -> (DVec3, DVec3, DVec3) {
        let back = -self.direction();
        let mut right = self.view_up.cross(back);
        if right.length_squared() < 1e-24 {
            right = back.any_orthonormal_vector();
        }
        let right = right.normalize();
        (right, back.cross(right), back)
    }

    /// World -> view (camera looks down -Z), VTK `GetViewTransformMatrix`.
    pub fn view_matrix(&self) -> DMat4 {
        let (r, u, b) = self.basis();
        let p = self.position;
        DMat4::from_cols_array(&[
            r.x,
            u.x,
            b.x,
            0.0, //
            r.y,
            u.y,
            b.y,
            0.0, //
            r.z,
            u.z,
            b.z,
            0.0, //
            -r.dot(p),
            -u.dot(p),
            -b.dot(p),
            1.0,
        ])
    }

    /// VTK's symmetric perspective frustum, mapped to wgpu's 0..1 depth range.
    pub fn projection_matrix(&self, aspect: f64, near: f64, far: f64) -> DMat4 {
        let f = 1.0 / (self.view_angle_deg.to_radians() / 2.0).tan();
        let depth = far / (near - far);
        DMat4::from_cols_array(&[
            f / aspect.max(1e-9),
            0.0,
            0.0,
            0.0, //
            0.0,
            f,
            0.0,
            0.0, //
            0.0,
            0.0,
            depth,
            -1.0, //
            0.0,
            0.0,
            near * depth,
            0.0,
        ])
    }

    /// VTK `vtkRenderer::ResetCameraClippingRange(bounds)`: `(near, far)` enclosing `bounds`.
    pub fn clipping_range(&self, bounds: &[[f64; 2]; 3]) -> (f64, f64) {
        let vn = -self.direction();
        let d = vn.dot(self.position);
        let mut range = [f64::INFINITY, 1e-18];
        for z in bounds[2] {
            for y in bounds[1] {
                for x in bounds[0] {
                    let dist = d - vn.dot(DVec3::new(x, y, z));
                    range[0] = range[0].min(dist);
                    range[1] = range[1].max(dist);
                }
            }
        }
        let min_gap = 0.2 * (self.view_angle_deg.to_radians() / 2.0).tan() * range[1];
        if range[1] - range[0] < min_gap {
            let grow = min_gap - range[1] + range[0];
            range[1] += grow / 2.0;
            range[0] -= grow / 2.0;
        }
        range[0] = range[0].max(0.0);
        range[0] = 0.99 * range[0] - (range[1] - range[0]) * CLIPPING_RANGE_EXPANSION;
        range[1] = 1.01 * range[1] + (range[1] - range[0]) * CLIPPING_RANGE_EXPANSION;
        if range[0] >= range[1] {
            range[0] = 0.01 * range[1];
        }
        if range[0] < NEAR_CLIPPING_PLANE_TOLERANCE * range[1] {
            range[0] = NEAR_CLIPPING_PLANE_TOLERANCE * range[1];
        }
        if !(range[0].is_finite() && range[1].is_finite() && range[0] > 0.0 && range[1] > range[0])
        {
            // Degenerate bounds (NaN coordinates): any sane range keeps the matrices finite.
            let dist = self.distance().max(1e-6);
            return (dist * 1e-3, dist * 1e3);
        }
        (range[0], range[1])
    }

    /// VTK `vtkRenderer::ResetCamera(bounds)`: keep the view direction and view-up, aim at
    /// the bounds center and back off until the bounding sphere fits the view.
    pub fn reset_to_bounds(&mut self, bounds: &[[f64; 2]; 3], aspect: f64) {
        let center = DVec3::from(bounds.map(|[lo, hi]| (lo + hi) / 2.0));
        let r2: f64 = bounds.iter().map(|[lo, hi]| (hi - lo) * (hi - lo)).sum();
        let radius = if r2 > 0.0 && r2.is_finite() {
            r2.sqrt() * 0.5
        } else {
            0.5
        };
        let mut angle = self.view_angle_deg.to_radians();
        if aspect < 1.0 {
            angle = 2.0 * ((angle * 0.5).tan() * aspect).atan();
        }
        let distance = radius / (angle * 0.5).sin();
        let vn = -self.direction();
        if self.view_up.dot(vn).abs() > 0.999 {
            let u = self.view_up;
            self.view_up = DVec3::new(-u.z, u.x, u.y);
        }
        self.focal_point = center;
        self.position = center + distance * vn;
    }

    /// VTK `vtkCamera::Azimuth`: rotate the position about the view-up through the focal point.
    pub fn azimuth(&mut self, degrees: f64) {
        let q = DQuat::from_axis_angle(self.view_up.normalize_or(DVec3::Z), degrees.to_radians());
        self.position = self.focal_point + q * (self.position - self.focal_point);
    }

    /// VTK `vtkCamera::Elevation`: rotate about the camera's left axis through the focal point.
    /// The view-up is carried along so repeated calls can pass over the poles (VTK re-derives
    /// it via `OrthogonalizeViewUp` after every step; for small steps both are the same).
    pub fn elevation(&mut self, degrees: f64) {
        let (right, _, _) = self.basis();
        let q = DQuat::from_axis_angle(-right, degrees.to_radians());
        let up_before = self.view_up;
        self.position = self.focal_point + q * (self.position - self.focal_point);
        let rotated_up = q * up_before;
        // Keep VTK's result when it is well defined; only fall back to the rotated up when
        // the old up became (anti)parallel to the view direction.
        let dir = self.direction();
        if up_before.cross(dir).length_squared() < 1e-12 {
            self.view_up = rotated_up;
        }
    }

    /// VTK `vtkCamera::Roll`: rotate the view-up about the direction of projection.
    pub fn roll(&mut self, degrees: f64) {
        let q = DQuat::from_axis_angle(self.direction(), degrees.to_radians());
        self.view_up = q * self.view_up;
    }

    /// VTK `vtkCamera::OrthogonalizeViewUp`.
    pub fn orthogonalize_view_up(&mut self) {
        self.view_up = self.basis().1;
    }

    /// VTK `vtkCamera::Dolly`: move towards (`factor > 1`) or away from the focal point.
    pub fn dolly(&mut self, factor: f64) {
        if !(factor > 0.0 && factor.is_finite()) {
            return;
        }
        let d = self.distance() / factor;
        self.position = self.focal_point - d * self.direction();
    }

    /// Translate camera and focal point so that a point on the focal plane follows a mouse
    /// motion of `(dx, dy_up)` pixels in a viewport `height_px` tall (VTK trackball `Pan`).
    pub fn pan_pixels(&mut self, dx: f64, dy_up: f64, height_px: f64) {
        if height_px <= 0.0 {
            return;
        }
        let (right, up, _) = self.basis();
        let world_per_px =
            2.0 * self.distance() * (self.view_angle_deg.to_radians() / 2.0).tan() / height_px;
        let motion = (right * dx + up * dy_up) * world_per_px;
        self.position -= motion;
        self.focal_point -= motion;
    }

    /// Auto-rotate step (renderer.py `_auto_rotate_tick`): orbit the position about the
    /// world Z axis through `(cx, cy)`, keeping its height, the focal point and view-up.
    pub fn orbit_z(&mut self, cx: f64, cy: f64, degrees: f64) {
        let (rx, ry) = (self.position.x - cx, self.position.y - cy);
        let radius = rx.hypot(ry);
        let angle = ry.atan2(rx) + degrees.to_radians();
        self.position.x = cx + radius * angle.cos();
        self.position.y = cy + radius * angle.sin();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: DVec3, b: [f64; 3], tol: f64) -> bool {
        (a - DVec3::from(b)).abs().max_element() <= tol
    }

    #[test]
    fn preset_math() {
        let b = [[-15.0, 15.0], [-15.0, 15.0], [0.0, 50.0]];
        let iso = Camera::for_preset(&b, ViewPreset::Isometric);
        // Values printed by renderer.py's set_view_preset for binary_cylinder.stl.
        assert!(close(iso.position, [65.3975, -65.3975, 90.3975], 1e-9));
        assert!(close(iso.focal_point, [0.0, 0.0, 25.0], 0.0));
        assert_eq!(iso.view_up, DVec3::Z);
        let top = Camera::for_preset(&b, ViewPreset::Top);
        assert!(close(top.position, [0.0, 0.0, 25.0 + 92.5], 1e-12));
        assert_eq!(top.view_up, DVec3::Y);
        let bottom = Camera::for_preset(&b, ViewPreset::Bottom);
        assert!(close(bottom.position, [0.0, 0.0, 25.0 - 92.5], 1e-12));
        assert_eq!(bottom.view_up, DVec3::NEG_Y);
        let front = Camera::for_preset(&b, ViewPreset::Front);
        assert!(close(front.position, [0.0, -92.5, 25.0], 1e-12));
        let back = Camera::for_preset(&b, ViewPreset::Back);
        assert!(close(back.position, [0.0, 92.5, 25.0], 1e-12));
        let left = Camera::for_preset(&b, ViewPreset::Left);
        assert!(close(left.position, [-92.5, 0.0, 25.0], 1e-12));
        let right = Camera::for_preset(&b, ViewPreset::Right);
        assert!(close(right.position, [92.5, 0.0, 25.0], 1e-12));
        for p in ViewPreset::ALL {
            assert_eq!(ViewPreset::from_name(p.label()), Some(p));
            assert_eq!(p.label().parse::<ViewPreset>(), Ok(p));
        }
        assert_eq!(
            ViewPreset::from_name(
                " Isometric
"
            ),
            Some(ViewPreset::Isometric)
        );
        assert!(
            "diagonal"
                .parse::<ViewPreset>()
                .unwrap_err()
                .contains("\"diagonal\"")
        );
    }

    #[test]
    fn preset_degenerate_bounds_uses_distance_100() {
        let b = [[1.0, 1.0], [2.0, 2.0], [3.0, 3.0]];
        let c = Camera::for_preset(&b, ViewPreset::Right);
        assert!(close(c.position, [101.0, 2.0, 3.0], 1e-12));
    }

    #[test]
    fn trackball_ops_match_vtk() {
        // Reference values from vtkCamera (VTK 9.5) -- see scratch cam_fixtures.py.
        let mut c = Camera {
            position: DVec3::new(11.0, -8.0, 13.0),
            focal_point: DVec3::new(1.0, 2.0, 3.0),
            view_up: DVec3::Z,
            view_angle_deg: 30.0,
        };
        c.azimuth(30.0);
        assert!(close(c.position, [14.660254038, -1.660254038, 13.0], 1e-8));
        c.elevation(20.0);
        assert!(close(
            c.position,
            [10.532779022, -0.554300441, 17.233821461],
            1e-8
        ));
        c.orthogonalize_view_up();
        assert!(close(
            c.view_up,
            [-0.793788248, 0.21269492, 0.56979039],
            1e-8
        ));
        c.roll(15.0);
        assert!(close(
            c.view_up,
            [-0.699753271, 0.455447516, 0.550375253],
            1e-8
        ));
        c.orthogonalize_view_up();
        c.dolly(1.3);
        assert!(close(
            c.position,
            [8.33290694, 0.035153507, 13.949093431],
            1e-8
        ));
        assert!(close(c.focal_point, [1.0, 2.0, 3.0], 0.0));
    }

    #[test]
    fn elevation_passes_over_the_pole() {
        let mut c = Camera::for_preset(&[[-1.0, 1.0]; 3], ViewPreset::Front);
        for _ in 0..180 {
            c.elevation(1.0);
            c.orthogonalize_view_up();
            assert!(c.view_matrix().is_finite());
        }
        // Half a turn: now behind the model, upside down.
        assert!(close(c.position, [0.0, 3.7, 0.0], 1e-6), "{:?}", c.position);
        assert!(close(c.view_up, [0.0, 0.0, -1.0], 1e-6), "{:?}", c.view_up);
    }

    #[test]
    fn clipping_range_matches_vtk() {
        let cam = Camera {
            position: DVec3::new(65.3975, -65.3975, 90.3975),
            focal_point: DVec3::new(0.0, 0.0, 25.0),
            view_up: DVec3::Z,
            view_angle_deg: 30.0,
        };
        let (n, f) = cam.clipping_range(&[[-37.5, 37.5], [-37.5, 37.5], [-0.5, 50.0]]);
        assert!(
            (n - 0.26010518).abs() < 1e-7 && (f - 260.105180247).abs() < 1e-7,
            "{n} {f}"
        );
        let (n, f) = cam.clipping_range(&[[-15.0, 15.0], [-15.0, 15.0], [0.0, 50.0]]);
        assert!((n - 48.94808779830123).abs() < 1e-9 && (f - 194.5153029159061).abs() < 1e-9);

        let close_cam = Camera {
            position: DVec3::new(0.0, 0.0, 10.0),
            ..Camera::default()
        };
        let (n, f) = close_cam.clipping_range(&[[-1.0, 1.0], [-1.0, 1.0], [-1.0, 1.0]]);
        assert!(
            (n - 7.91).abs() < 1e-9 && (f - 12.655).abs() < 1e-9,
            "{n} {f}"
        );
        let (n, f) = close_cam.clipping_range(&[[-1.0, 1.0], [-1.0, 1.0], [-5.0, -5.0]]);
        assert!(
            (n - 14.050171661).abs() < 1e-8 && (f - 16.231819091).abs() < 1e-8,
            "{n} {f}"
        );
        // Everything behind the camera: VTK yields a tiny but valid range.
        let (n, f) = close_cam.clipping_range(&[[-1.0, 1.0], [-1.0, 1.0], [20.0, 30.0]]);
        assert!(n > 0.0 && f > n && f < 1e-12);
        // NaN bounds never produce NaN matrices.
        let (n, f) = close_cam.clipping_range(&[[f64::NAN, 1.0], [-1.0, 1.0], [0.0, 1.0]]);
        assert!(n > 0.0 && f > n && n.is_finite() && f.is_finite());
    }

    #[test]
    fn reset_to_bounds_matches_vtk() {
        let grid = [[-75.0, 75.0], [-75.0, 75.0], [0.0, 0.0]];
        let mut c = Camera::default();
        c.reset_to_bounds(&grid, 400.0 / 300.0);
        assert!(close(c.position, [0.0, 0.0, 409.807621135], 1e-6));
        let (n, f) = c.clipping_range(&grid);
        assert!((n - 383.857828318).abs() < 1e-6 && (f - 443.461544547).abs() < 1e-6);
        let mut c = Camera::default();
        c.reset_to_bounds(&grid, 300.0 / 400.0);
        assert!(close(c.position, [0.0, 0.0, 538.343816353], 1e-6));
    }

    #[test]
    fn pan_moves_focal_plane_point_with_mouse() {
        let mut c = Camera::for_preset(&[[-10.0, 10.0]; 3], ViewPreset::Front);
        let before = c;
        let h = 500.0;
        c.pan_pixels(50.0, 0.0, h);
        // Camera moves left so the scene follows the mouse to the right.
        let shift = c.focal_point - before.focal_point;
        let expected = 2.0 * before.distance() * 15f64.to_radians().tan() / h * 50.0;
        assert!(close(shift, [-expected, 0.0, 0.0], 1e-9));
        assert!((c.position - before.position - shift).length() < 1e-12);
    }

    #[test]
    fn orbit_keeps_height_radius_and_focus() {
        let b = [[0.0, 20.0], [0.0, 10.0], [0.0, 5.0]];
        let mut c = Camera::for_preset(&b, ViewPreset::Isometric);
        let before = c;
        c.orbit_z(10.0, 5.0, 0.8);
        assert_eq!(c.position.z, before.position.z);
        assert_eq!(c.focal_point, before.focal_point);
        assert_eq!(c.view_up, before.view_up);
        let r0 = (before.position.truncate() - glam::DVec2::new(10.0, 5.0)).length();
        let r1 = (c.position.truncate() - glam::DVec2::new(10.0, 5.0)).length();
        assert!((r0 - r1).abs() < 1e-9);
        let a0 = (before.position.y - 5.0).atan2(before.position.x - 10.0);
        let a1 = (c.position.y - 5.0).atan2(c.position.x - 10.0);
        assert!((a1 - a0 - 0.8f64.to_radians()).abs() < 1e-12);
    }

    #[test]
    fn view_and_projection_are_consistent() {
        let c = Camera::for_preset(&[[-1.0, 1.0]; 3], ViewPreset::Isometric);
        let v = c.view_matrix();
        // Focal point lies on the -Z axis at the camera distance.
        let fp = v.transform_point3(c.focal_point);
        assert!(close(fp, [0.0, 0.0, -c.distance()], 1e-9));
        let (n, f) = c.clipping_range(&[[-1.0, 1.0]; 3]);
        let p = c.projection_matrix(1.5, n, f);
        let clip = p * fp.extend(1.0);
        let ndc = clip.truncate() / clip.w;
        assert!(ndc.x.abs() < 1e-9 && ndc.y.abs() < 1e-9 && (0.0..=1.0).contains(&ndc.z));
    }
}
