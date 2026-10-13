use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use stlv_core::ModelData;

use crate::camera::{Camera, ViewPreset};
use crate::gpu::{
    DrawRequest, GpuGeometry, GpuGrid, Pipelines, SceneUniforms, Targets, encode_scene,
};
use crate::prepared::Geometry;
use crate::{PreparedModel, ShadingMode};

/// Off-screen thumbnail size in pixels (app.py `generate_vtk_thumbnail`).
pub const THUMBNAIL_SIZE: (u32, u32) = (228, 200);

/// Longest wait for the GPU to finish one snapshot.
const READBACK_TIMEOUT: Duration = Duration::from_secs(20);

/// Options for [`ThumbnailRenderer::render_snapshot`]: an off-screen render with the full
/// viewport look. `Default` is the thumbnail look.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnapshotOptions {
    /// Image size in pixels.
    pub size: (u32, u32),
    /// `None`: the isometric preset for the model's bounds.
    pub camera: Option<Camera>,
    pub shading: ShadingMode,
    /// Linear RGB in 0..1.
    pub color: [f32; 3],
    /// Draw the ground grid under the model (the interactive viewport's default).
    pub grid: bool,
}

impl Default for SnapshotOptions {
    fn default() -> Self {
        Self {
            size: THUMBNAIL_SIZE,
            camera: None,
            shading: ShadingMode::Smooth,
            color: stlv_core::palette::DEFAULT_MODEL_COLOR,
            grid: false,
        }
    }
}

/// GPU state guarded by the renderer's mutex (one snapshot at a time).
struct Inner {
    pipelines: Pipelines,
    uniforms: SceneUniforms,
    targets: Option<Targets>,
}

/// Renders [`THUMBNAIL_SIZE`] snapshots: isometric camera, default cyan, gradient
/// background, no grid or axes. `Send + Sync` -- safe to drive from a worker thread.
pub struct ThumbnailRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    inner: Mutex<Inner>,
}

impl ThumbnailRenderer {
    /// Share the app's device (e.g. from `eframe::CreationContext::wgpu_render_state`).
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let pipelines = Pipelines::new(&device);
        Self::with_pipelines(device, queue, pipelines)
    }

    fn with_pipelines(device: wgpu::Device, queue: wgpu::Queue, pipelines: Pipelines) -> Self {
        let uniforms = SceneUniforms::new(&device, &pipelines);
        Self {
            device,
            queue,
            inner: Mutex::new(Inner {
                pipelines,
                uniforms,
                targets: None,
            }),
        }
    }

    /// Create a private device (tests, tools).
    pub fn new_headless() -> Result<Self, String> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::from_env().unwrap_or_default(),
            ..Default::default()
        }))
        .map_err(|e| format!("no GPU adapter available: {e}"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("stlv headless"),
            required_features: crate::optional_features(&adapter),
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            memory_hints: crate::MEMORY_HINTS,
            ..Default::default()
        }))
        .map_err(|e| format!("could not create a GPU device: {e}"))?;
        // Without a handler wgpu panics on errors; snapshots report them as `Err` instead.
        device.on_uncaptured_error(Arc::new(|e| log::error!("wgpu: {e}")));
        Ok(Self::new(device, queue))
    }

    /// Thumbnail of `model` (mesh: smooth shading; toolpath: 1.5 px unlit lines).
    /// Errors for empty models (Python produced no thumbnail) and GPU failures.
    pub fn render(&self, model: &ModelData) -> Result<image::RgbaImage, String> {
        if model.vertices.is_empty() {
            return Err("model has no vertices".into());
        }
        let geometry = Geometry::build(model);
        self.render_geometry(&geometry, &model.bounds, &SnapshotOptions::default())
    }

    /// Like [`Self::render`] for an already prepared model (skips the CPU preparation).
    pub fn render_prepared(&self, prepared: &PreparedModel) -> Result<image::RgbaImage, String> {
        self.render_snapshot(prepared, &SnapshotOptions::default())
    }

    /// Off-screen render of `prepared` with the given camera/shading/grid at any size.
    pub fn render_snapshot(
        &self,
        prepared: &PreparedModel,
        options: &SnapshotOptions,
    ) -> Result<image::RgbaImage, String> {
        if prepared.model.vertices.is_empty() {
            return Err("model has no vertices".into());
        }
        self.render_geometry(&prepared.geometry, &prepared.model.bounds, options)
    }

    fn render_geometry(
        &self,
        geometry: &Geometry,
        bounds: &[[f64; 2]; 3],
        options: &SnapshotOptions,
    ) -> Result<image::RgbaImage, String> {
        let (w, h) = options.size;
        let max_dim = self.device.limits().max_texture_dimension_2d;
        if w == 0 || h == 0 || w > max_dim || h > max_dim {
            return Err(format!("invalid snapshot size {w}x{h}"));
        }
        let camera = options
            .camera
            .unwrap_or_else(|| Camera::for_preset(bounds, ViewPreset::Isometric));
        // A panic while holding the lock cannot leave GPU state half-updated in a way that
        // matters (everything is rewritten per snapshot), so recover from poisoning.
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Inner {
            pipelines,
            uniforms,
            targets,
        } = &mut *inner;
        let gpu_geometry = if geometry.is_empty() {
            None
        } else {
            Some(GpuGeometry::upload(&self.device, pipelines, geometry)?)
        };
        let grid = options
            .grid
            .then(|| GpuGrid::new(&self.device, Some(bounds)));
        if targets.as_ref().map(|t| t.size) != Some((w, h)) {
            *targets = Some(Targets::new(&self.device, pipelines, (w, h)));
        }
        let Some(targets) = targets.as_ref() else {
            return Err("no render target".into());
        };

        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("stlv snapshot"),
            });
        let request = DrawRequest {
            camera: &camera,
            geometry: gpu_geometry.as_ref().map(|g| (g, *bounds)),
            grid: grid.as_ref(),
            shading: options.shading,
            color: options.color,
            clip_z: None,
        };
        encode_scene(
            &self.queue,
            &mut encoder,
            pipelines,
            targets,
            uniforms,
            &request,
        );
        let image = read_back(
            &self.device,
            &self.queue,
            encoder,
            &targets.resolved,
            (w, h),
        );
        if let Some(err) = pollster::block_on(scope.pop()) {
            return Err(format!("GPU error: {err}"));
        }
        image
    }
}

/// Copies `texture` (Rgba8Unorm) to the CPU after submitting `encoder`.
fn read_back(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    mut encoder: wgpu::CommandEncoder,
    texture: &wgpu::Texture,
    (w, h): (u32, u32),
) -> Result<image::RgbaImage, String> {
    let row_bytes = w * 4;
    let padded_row =
        row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("stlv readback"),
        size: u64::from(padded_row) * u64::from(h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    let submission = queue.submit([encoder.finish()]);
    let (tx, rx) = mpsc::channel();
    staging.map_async(wgpu::MapMode::Read, .., move |result| {
        let _ = tx.send(result);
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(READBACK_TIMEOUT),
        })
        .map_err(|e| format!("GPU wait failed: {e}"))?;
    match rx.recv_timeout(READBACK_TIMEOUT) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(format!("readback failed: {e}")),
        Err(_) => return Err("readback timed out".into()),
    }
    let mut pixels = Vec::with_capacity((row_bytes * h) as usize);
    {
        let data = staging
            .get_mapped_range(..)
            .map_err(|e| format!("readback failed: {e}"))?;
        for row in data.chunks_exact(padded_row as usize) {
            pixels.extend_from_slice(&row[..row_bytes as usize]);
        }
    }
    staging.unmap();
    image::RgbaImage::from_raw(w, h, pixels).ok_or_else(|| "readback size mismatch".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A faceted cylinder (radius 10, height 30) as a triangle soup.
    fn cylinder() -> ModelData {
        let n = 24;
        let mut v = Vec::new();
        let mut t = Vec::new();
        for i in 0..n {
            let a = i as f32 / n as f32 * std::f32::consts::TAU;
            v.push([10.0 * a.cos(), 10.0 * a.sin(), 0.0]);
            v.push([10.0 * a.cos(), 10.0 * a.sin(), 30.0]);
        }
        let (c0, c1) = (v.len() as u32, v.len() as u32 + 1);
        v.push([0.0, 0.0, 0.0]);
        v.push([0.0, 0.0, 30.0]);
        for i in 0..n as u32 {
            let j = (i + 1) % n as u32;
            t.extend([[2 * i, 2 * j, 2 * j + 1], [2 * i, 2 * j + 1, 2 * i + 1]]);
            t.extend([[c0, 2 * j, 2 * i], [c1, 2 * i + 1, 2 * j + 1]]);
        }
        ModelData::new(v, t, vec![], "STL (Binary)", "cylinder.stl")
    }

    fn lit_pixels(img: &image::RgbaImage) -> usize {
        img.pixels()
            .filter(|p| p[1] > 60 && p[2] > 60 && p[0] < 50)
            .count()
    }

    #[test]
    fn native_line_wireframe_fallback_matches_wide_lines() {
        let Ok(wide) = ThumbnailRenderer::new_headless() else {
            return;
        };
        let pipelines = Pipelines::with_wide_wireframe(&wide.device, false);
        assert!(pipelines.mesh_wire_unavailable());
        let native =
            ThumbnailRenderer::with_pipelines(wide.device.clone(), wide.queue.clone(), pipelines);
        let prepared = PreparedModel::new(Arc::new(cylinder()));
        let opts = SnapshotOptions {
            size: (320, 240),
            shading: ShadingMode::Wireframe,
            ..Default::default()
        };
        let a = lit_pixels(&wide.render_snapshot(&prepared, &opts).expect("wide"));
        let b = lit_pixels(&native.render_snapshot(&prepared, &opts).expect("native"));
        assert!(a > 500, "wide wireframe drew {a} px");
        let ratio = b as f64 / a as f64;
        assert!(
            (0.75..1.25).contains(&ratio),
            "native {b} px vs wide {a} px"
        );
    }
}
