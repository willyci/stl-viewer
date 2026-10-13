use std::sync::Arc;
use std::time::Instant;

use egui::mutex::RwLock;
use stlv_core::ModelData;

use crate::camera::Camera;
use crate::gpu::{
    DrawRequest, GpuGeometry, GpuGrid, Pipelines, SceneUniforms, Targets, encode_scene,
};
use crate::{PreparedModel, ShadingMode, ViewPreset, marker};

/// renderer.py auto-rotate: 0.8 degrees per 16 ms timer tick.
const AUTO_ROTATE_DEG_PER_SEC: f64 = 0.8 / 0.016;
/// Longest frame gap the auto-rotation catches up on (avoids jumps after stalls).
const MAX_ROTATE_STEP_SECS: f64 = 0.1;
/// vtkInteractorStyle `MotionFactor`.
const MOTION_FACTOR: f64 = 10.0;
/// egui reports smooth (touchpad) scrolling in points; treat this many as one wheel notch.
const POINTS_PER_WHEEL_NOTCH: f32 = 50.0;

/// What the current mouse drag does (chosen at button press, like VTK).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DragMode {
    Rotate,
    Pan,
    Dolly,
    Spin,
}

/// Interactive 3D view (renderer.py `VTKRendererWidget`): gradient background, ground
/// grid, model, corner XYZ orientation marker, trackball camera, auto-rotate.
pub struct Viewport3D {
    device: wgpu::Device,
    queue: wgpu::Queue,
    egui_renderer: Arc<RwLock<egui_wgpu::Renderer>>,
    pipelines: Pipelines,
    uniforms: SceneUniforms,
    targets: Option<Targets>,
    texture_id: Option<egui::TextureId>,
    model: Option<Arc<ModelData>>,
    geometry: Option<GpuGeometry>,
    grid: GpuGrid,
    grid_visible: bool,
    /// Without a model this stays VTK's default camera, as in the Python app: its
    /// orientation marker creates the camera before the first render, so VTK never resets
    /// it and the empty view looks straight down at the grid centre from z = 1.
    camera: Camera,
    shading: ShadingMode,
    color: [f32; 3],
    auto_rotate: bool,
    last_rotate_tick: Option<Instant>,
    drag: Option<DragMode>,
    /// World z range kept by the cross-section slider; `None` shows the whole model.
    clip_z: Option<[f64; 2]>,
    dirty: bool,
}

impl Viewport3D {
    pub fn new(render_state: &egui_wgpu::RenderState) -> Self {
        let device = render_state.device.clone();
        let pipelines = Pipelines::new(&device);
        let uniforms = SceneUniforms::new(&device, &pipelines);
        let grid = GpuGrid::new(&device, None);
        Self {
            queue: render_state.queue.clone(),
            egui_renderer: render_state.renderer.clone(),
            pipelines,
            uniforms,
            targets: None,
            texture_id: None,
            model: None,
            geometry: None,
            grid,
            grid_visible: true,
            camera: Camera::default(),
            shading: ShadingMode::Smooth,
            color: stlv_core::palette::DEFAULT_MODEL_COLOR,
            auto_rotate: false,
            last_rotate_tick: None,
            drag: None,
            clip_z: None,
            dirty: true,
            device,
        }
    }

    /// Replace the model: stops auto-rotate, re-seats the grid, resets to the ISO view.
    pub fn set_model(&mut self, model: PreparedModel) {
        if model.model.vertices.is_empty() {
            return;
        }
        self.set_auto_rotate(false);
        let geometry = if model.geometry.is_empty() {
            None
        } else {
            match GpuGeometry::upload(&self.device, &self.pipelines, &model.geometry) {
                Ok(g) => Some(g),
                Err(e) => {
                    log::error!("cannot display {}: {e}", model.model.filename);
                    None
                }
            }
        };
        let bounds = model.model.bounds;
        self.geometry = geometry;
        self.model = Some(model.model);
        self.grid = GpuGrid::new(&self.device, Some(&bounds));
        self.camera = Camera::for_preset(&bounds, ViewPreset::Isometric);
        self.clip_z = None;
        self.dirty = true;
    }

    /// Back to the empty scene (grid only, default camera).
    pub fn clear_model(&mut self) {
        self.set_auto_rotate(false);
        self.model = None;
        self.geometry = None;
        self.grid = GpuGrid::new(&self.device, None);
        self.camera = Camera::default();
        self.clip_z = None;
        self.dirty = true;
    }

    /// Show only the part of the model with `lo <= z <= hi` (world units), like a slicer's
    /// layer range; `None` shows everything. A new model resets it.
    pub fn set_clip_z(&mut self, range: Option<[f64; 2]>) {
        self.dirty |= range != self.clip_z;
        self.clip_z = range;
    }

    pub fn clip_z(&self) -> Option<[f64; 2]> {
        self.clip_z
    }

    pub fn has_model(&self) -> bool {
        self.model.is_some()
    }

    /// The displayed model, if any.
    pub fn model(&self) -> Option<&Arc<ModelData>> {
        self.model.as_ref()
    }

    pub fn set_shading(&mut self, mode: ShadingMode) {
        self.dirty |= mode != self.shading;
        self.shading = mode;
    }

    pub fn shading(&self) -> ShadingMode {
        self.shading
    }

    /// Linear RGB in 0..1.
    pub fn set_color(&mut self, rgb: [f32; 3]) {
        self.dirty |= rgb != self.color;
        self.color = rgb;
    }

    pub fn color(&self) -> [f32; 3] {
        self.color
    }

    /// No-op without a model (like the Python app).
    pub fn set_view_preset(&mut self, preset: ViewPreset) {
        if let Some(model) = &self.model {
            self.camera = Camera::for_preset(&model.bounds, preset);
            self.dirty = true;
        }
    }

    pub fn set_auto_rotate(&mut self, enabled: bool) {
        self.auto_rotate = enabled;
        self.last_rotate_tick = None;
    }

    pub fn auto_rotate(&self) -> bool {
        self.auto_rotate
    }

    pub fn set_grid_visible(&mut self, visible: bool) {
        self.dirty |= visible != self.grid_visible;
        self.grid_visible = visible;
    }

    pub fn grid_visible(&self) -> bool {
        self.grid_visible
    }

    pub fn camera(&self) -> &Camera {
        &self.camera
    }

    /// Replace the camera (e.g. to restore a saved view).
    pub fn set_camera(&mut self, camera: Camera) {
        self.camera = camera;
        self.dirty = true;
    }

    /// Fill the available space with the viewport and handle mouse input.
    pub fn show(&mut self, ui: &mut egui::Ui) -> egui::Response {
        let (rect, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        let ppp = ui.ctx().pixels_per_point();
        // Snap to physical pixels so the image maps 1:1 onto the screen at any scale factor.
        let rect = egui::emath::GuiRounding::round_to_pixels(rect, ppp);
        let (w_px, h_px) = ((rect.width() * ppp).round(), (rect.height() * ppp).round());
        if !(w_px >= 1.0 && h_px >= 1.0) {
            return response;
        }
        // eframe's first (invisible) pass lays out a huge placeholder screen; allocating
        // multisampled targets for it would cost gigabytes, so wait for the real size.
        let max_dim = self.device.limits().max_texture_dimension_2d as f32;
        if w_px > max_dim || h_px > max_dim {
            ui.painter().rect_filled(rect, 0.0, background_color());
            ui.ctx().request_repaint();
            return response;
        }
        let size_px = (w_px as u32, h_px as u32);
        self.handle_input(ui, &response, rect);
        self.tick_auto_rotate(ui.ctx());
        self.ensure_targets(size_px);
        if self.dirty {
            self.render();
        }

        if let Some(id) = self.texture_id {
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            ui.painter().image(id, rect, uv, egui::Color32::WHITE);
        }
        marker::paint(ui.painter(), rect, &self.camera, ppp);
        response
    }

    fn ensure_targets(&mut self, size: (u32, u32)) {
        if self.targets.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        log::debug!("viewport render target {}x{} px", size.0, size.1);
        let targets = Targets::new(&self.device, &self.pipelines, size);
        let mut renderer = self.egui_renderer.write();
        // Rendered 1:1 with the screen, so nearest sampling keeps it crisp.
        let filter = wgpu::FilterMode::Nearest;
        match self.texture_id {
            Some(id) => renderer.update_egui_texture_from_wgpu_texture(
                &self.device,
                &targets.resolved_view,
                filter,
                id,
            ),
            None => {
                self.texture_id = Some(renderer.register_native_texture(
                    &self.device,
                    &targets.resolved_view,
                    filter,
                ))
            }
        }
        self.targets = Some(targets);
        self.dirty = true;
    }

    fn render(&mut self) {
        let Some(targets) = &self.targets else { return };
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("stlv viewport"),
            });
        let geometry = match (&self.geometry, &self.model) {
            (Some(g), Some(m)) => Some((g, m.bounds)),
            _ => None,
        };
        let request = DrawRequest {
            camera: &self.camera,
            geometry,
            grid: self.grid_visible.then_some(&self.grid),
            shading: self.shading,
            color: self.color,
            clip_z: self.clip_z,
        };
        encode_scene(
            &self.queue,
            &mut encoder,
            &self.pipelines,
            targets,
            &self.uniforms,
            &request,
        );
        self.queue.submit([encoder.finish()]);
        self.dirty = false;
    }

    fn tick_auto_rotate(&mut self, ctx: &egui::Context) {
        let Some(model) = self.model.as_ref().filter(|_| self.auto_rotate) else {
            self.last_rotate_tick = None;
            return;
        };
        let now = Instant::now();
        if let Some(last) = self.last_rotate_tick {
            let dt = now
                .duration_since(last)
                .as_secs_f64()
                .min(MAX_ROTATE_STEP_SECS);
            let [cx, cy, _] = model.center();
            self.camera.orbit_z(cx, cy, AUTO_ROTATE_DEG_PER_SEC * dt);
            self.dirty = true;
        }
        self.last_rotate_tick = Some(now);
        ctx.request_repaint();
    }

    /// vtkInteractorStyleTrackballCamera bindings.
    fn handle_input(&mut self, ui: &egui::Ui, response: &egui::Response, rect: egui::Rect) {
        if response.drag_started() {
            let modifiers = ui.input(|i| i.modifiers);
            self.drag = [
                egui::PointerButton::Primary,
                egui::PointerButton::Middle,
                egui::PointerButton::Secondary,
            ]
            .into_iter()
            .find(|&b| response.drag_started_by(b))
            .and_then(|b| drag_mode(b, modifiers));
        }
        if response.dragged()
            && let Some(mode) = self.drag
        {
            let delta = response.drag_delta();
            if let Some(pos) = response.interact_pointer_pos()
                && delta != egui::Vec2::ZERO
            {
                apply_drag(&mut self.camera, mode, delta, pos, rect);
                self.dirty = true;
            }
        }
        if response.drag_stopped() {
            self.drag = None;
        }

        if response.hovered() {
            let notches = ui.input(|i| wheel_notches(&i.events));
            if notches != 0.0 && notches.is_finite() {
                self.camera.dolly(wheel_dolly_factor(notches));
                self.dirty = true;
            }
        }
    }
}

/// vtkInteractorStyleTrackballCamera's button/modifier bindings.
fn drag_mode(button: egui::PointerButton, m: egui::Modifiers) -> Option<DragMode> {
    match button {
        egui::PointerButton::Primary => Some(match (m.shift, m.ctrl || m.command) {
            (true, true) => DragMode::Dolly,
            (true, false) => DragMode::Pan,
            (false, true) => DragMode::Spin,
            (false, false) => DragMode::Rotate,
        }),
        egui::PointerButton::Middle => Some(DragMode::Pan),
        egui::PointerButton::Secondary => Some(DragMode::Dolly),
        _ => None,
    }
}

/// One mouse-move step of a drag: the pointer moved by `delta` to `pos` (points) inside
/// `rect`. Mirrors vtkInteractorStyleTrackballCamera's Rotate/Pan/Dolly/Spin.
fn apply_drag(
    camera: &mut Camera,
    mode: DragMode,
    delta: egui::Vec2,
    pos: egui::Pos2,
    rect: egui::Rect,
) {
    let (w, h) = (f64::from(rect.width()), f64::from(rect.height()));
    if !(w > 0.0 && h > 0.0) {
        return;
    }
    // VTK's display y axis points up.
    let (dx, dy_up) = (f64::from(delta.x), -f64::from(delta.y));
    match mode {
        DragMode::Rotate => {
            camera.azimuth(dx * -20.0 / w * MOTION_FACTOR);
            camera.elevation(dy_up * -20.0 / h * MOTION_FACTOR);
            camera.orthogonalize_view_up();
        }
        DragMode::Pan => camera.pan_pixels(dx, dy_up, h),
        DragMode::Dolly => camera.dolly(1.1f64.powf(MOTION_FACTOR * dy_up / (h / 2.0))),
        DragMode::Spin => {
            let c = rect.center();
            let angle = |p: egui::Pos2| {
                f64::from(c.y - p.y)
                    .atan2(f64::from(p.x - c.x))
                    .to_degrees()
            };
            camera.roll(angle(pos) - angle(pos - delta));
            camera.orthogonalize_view_up();
        }
    }
}

/// Wheel notches in `events` (forward positive); smooth scrolling counts proportionally.
fn wheel_notches(events: &[egui::Event]) -> f32 {
    events
        .iter()
        .map(|e| match e {
            egui::Event::MouseWheel { unit, delta, .. } => match unit {
                egui::MouseWheelUnit::Line => delta.y,
                egui::MouseWheelUnit::Point => delta.y / POINTS_PER_WHEEL_NOTCH,
                egui::MouseWheelUnit::Page => delta.y * 10.0,
            },
            _ => 0.0,
        })
        .sum()
}

/// VTK wheel dolly: 1.1^(MotionFactor * 0.2) = 1.21 per notch, forward = closer.
fn wheel_dolly_factor(notches: f32) -> f64 {
    1.1f64.powf(MOTION_FACTOR * 0.2 * f64::from(notches))
}

/// The gradient's bottom color, for frames without a rendered image.
fn background_color() -> egui::Color32 {
    let [r, g, b] = stlv_core::palette::VIEWPORT_BG_BOTTOM.map(|c| (c * 255.0).round() as u8);
    egui::Color32::from_rgb(r, g, b)
}

impl Drop for Viewport3D {
    fn drop(&mut self) {
        if let Some(id) = self.texture_id.take() {
            self.egui_renderer.write().free_texture(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Modifiers, PointerButton, Pos2, Rect, Vec2, pos2, vec2};
    use glam::DVec3;

    fn cam() -> Camera {
        Camera::for_preset(
            &[[-10.0, 10.0], [-10.0, 10.0], [0.0, 20.0]],
            ViewPreset::Isometric,
        )
    }

    fn rect() -> Rect {
        Rect::from_min_size(pos2(100.0, 50.0), vec2(800.0, 600.0))
    }

    fn near(a: DVec3, b: DVec3) -> bool {
        (a - b).length() < 1e-9
    }

    #[test]
    fn bindings_match_vtk_trackball_camera() {
        let both = Modifiers {
            shift: true,
            ctrl: true,
            ..Default::default()
        };
        assert_eq!(
            drag_mode(PointerButton::Primary, Modifiers::NONE),
            Some(DragMode::Rotate)
        );
        assert_eq!(
            drag_mode(PointerButton::Primary, Modifiers::SHIFT),
            Some(DragMode::Pan)
        );
        assert_eq!(
            drag_mode(PointerButton::Primary, Modifiers::CTRL),
            Some(DragMode::Spin)
        );
        assert_eq!(
            drag_mode(PointerButton::Primary, both),
            Some(DragMode::Dolly)
        );
        assert_eq!(
            drag_mode(PointerButton::Middle, Modifiers::NONE),
            Some(DragMode::Pan)
        );
        assert_eq!(
            drag_mode(PointerButton::Secondary, Modifiers::NONE),
            Some(DragMode::Dolly)
        );
        assert_eq!(drag_mode(PointerButton::Extra1, Modifiers::NONE), None);
    }

    #[test]
    fn rotate_drag_degrees() {
        // 40 px right in an 800 px wide view: azimuth -20/800 * 40 * 10 = -10 degrees.
        let mut c = cam();
        apply_drag(
            &mut c,
            DragMode::Rotate,
            vec2(40.0, 0.0),
            pos2(500.0, 300.0),
            rect(),
        );
        let mut expected = cam();
        expected.azimuth(-10.0);
        expected.orthogonalize_view_up();
        assert!(near(c.position, expected.position) && near(c.view_up, expected.view_up));
        // 30 px down (VTK dy = -30) in a 600 px tall view: elevation +10 degrees.
        let mut c = cam();
        apply_drag(
            &mut c,
            DragMode::Rotate,
            vec2(0.0, 30.0),
            pos2(500.0, 300.0),
            rect(),
        );
        let mut expected = cam();
        expected.elevation(10.0);
        expected.orthogonalize_view_up();
        assert!(near(c.position, expected.position) && near(c.view_up, expected.view_up));
    }

    #[test]
    fn dolly_drag_and_wheel() {
        // Dragging up by a tenth of the half height dollies in by 1.1.
        let mut c = cam();
        let d0 = c.distance();
        apply_drag(
            &mut c,
            DragMode::Dolly,
            vec2(0.0, -30.0),
            pos2(500.0, 300.0),
            rect(),
        );
        assert!((d0 / c.distance() - 1.1).abs() < 1e-9);
        assert_eq!(c.focal_point, cam().focal_point);

        let wheel = |unit, y| egui::Event::MouseWheel {
            unit,
            delta: vec2(0.0, y),
            phase: egui::TouchPhase::Move,
            modifiers: Modifiers::NONE,
        };
        let events = [
            wheel(egui::MouseWheelUnit::Line, 1.0),
            wheel(egui::MouseWheelUnit::Point, -25.0),
        ];
        assert!((wheel_notches(&events) - 0.5).abs() < 1e-6);
        assert!((wheel_dolly_factor(1.0) - 1.21).abs() < 1e-12);
        assert!((wheel_dolly_factor(-1.0) - 1.0 / 1.21).abs() < 1e-12);
    }

    #[test]
    fn spin_follows_pointer_angle() {
        // From right of the center to straight above it: a quarter turn counter-clockwise.
        let r = rect();
        let c0 = r.center();
        let from = c0 + vec2(100.0, 0.0);
        let to: Pos2 = c0 + vec2(0.0, -100.0);
        let mut c = cam();
        apply_drag(&mut c, DragMode::Spin, to - from, to, r);
        let mut expected = cam();
        expected.roll(90.0);
        expected.orthogonalize_view_up();
        assert!(
            near(c.view_up, expected.view_up),
            "{:?} {:?}",
            c.view_up,
            expected.view_up
        );
        assert_eq!(c.position, cam().position);
    }

    #[test]
    fn pan_drag_moves_scene_with_pointer() {
        let mut c = cam();
        apply_drag(
            &mut c,
            DragMode::Pan,
            Vec2::new(10.0, -20.0),
            pos2(500.0, 300.0),
            rect(),
        );
        let mut expected = cam();
        expected.pan_pixels(10.0, 20.0, 600.0);
        assert!(near(c.focal_point, expected.focal_point) && near(c.position, expected.position));
    }

    #[test]
    fn degenerate_rect_is_ignored() {
        let mut c = cam();
        let empty = Rect::from_min_size(Pos2::ZERO, Vec2::ZERO);
        for mode in [
            DragMode::Rotate,
            DragMode::Pan,
            DragMode::Dolly,
            DragMode::Spin,
        ] {
            apply_drag(&mut c, mode, vec2(5.0, 5.0), pos2(1.0, 1.0), empty);
        }
        assert_eq!(c, cam());
    }
}
