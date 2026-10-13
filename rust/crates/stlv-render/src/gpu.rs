//! wgpu pipelines and resources shared by [`crate::Viewport3D`] and [`crate::ThumbnailRenderer`].

use glam::{DMat4, DVec3};
use wgpu::util::DeviceExt;

use crate::ShadingMode;
use crate::camera::Camera;
use crate::prepared::{Geometry, Vertex};

/// Final (resolved) color format; also what egui-wgpu expects for native textures.
pub(crate) const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// VTK's default (`vtkRenderWindow` multisamples = 8), resolved linearly by the hardware like
/// VTK's framebuffer blit. Needs `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES`; see
/// [`crate::wgpu_setup`].
const PREFERRED_SAMPLE_COUNT: u32 = 8;
/// The only multisample count WebGPU guarantees for these formats.
const FALLBACK_SAMPLE_COUNT: u32 = 4;

/// Highest supported MSAA sample count (8 when the device allows it, else 4).
fn probe_sample_count(device: &wgpu::Device) -> u32 {
    if !device
        .features()
        .contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES)
    {
        return FALLBACK_SAMPLE_COUNT;
    }
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    for format in [COLOR_FORMAT, DEPTH_FORMAT] {
        let _ = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("stlv msaa probe"),
            size: wgpu::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: PREFERRED_SAMPLE_COUNT,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
    }
    match pollster::block_on(scope.pop()) {
        None => PREFERRED_SAMPLE_COUNT,
        Some(_) => FALLBACK_SAMPLE_COUNT,
    }
}

// renderer.py lighting rig and material.
const HEADLIGHT_INTENSITY: f32 = 0.85;
const SCENE_LIGHT_POSITION: DVec3 = DVec3::new(-1.0, 1.0, 1.0);
const SCENE_LIGHT_COLOR: [f32; 3] = [0.2, 0.25, 0.35];
const SCENE_LIGHT_INTENSITY: f32 = 0.3;
const AMBIENT: f32 = 0.18;
const DIFFUSE: f32 = 0.82;
const SPECULAR: f32 = 0.45;
const SPECULAR_POWER: f32 = 35.0;
const MESH_LINE_WIDTH: f32 = 1.0;
const TOOLPATH_LINE_WIDTH: f32 = 1.5;
const POINT_SIZE: f32 = 3.0;
const GRID_LINE_WIDTH: f32 = 1.0;
const GRID_DIVISIONS: usize = 24;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FrameUniform {
    view: [[f32; 4]; 4],
    proj: [[f32; 4]; 4],
    viewport: [f32; 4],
    light1_dir: [f32; 4],
    light0_color: [f32; 4],
    light1_color: [f32; 4],
    bg_bottom: [f32; 4],
    bg_top: [f32; 4],
}

/// Per-object uniform: model-view transform (objects are stored relative to an origin to
/// keep f32 precision for far-from-origin models) and VTK material.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ObjectUniform {
    model_view: [[f32; 4]; 4],
    ambient: [f32; 4],
    diffuse: [f32; 4],
    specular: [f32; 4],
    color: [f32; 4],
    size: [f32; 4],
    /// Z cross-section in object coordinates: lowest and highest z kept, 1 when active.
    clip: [f32; 4],
}

fn vec4(rgb: [f32; 3], w: f32) -> [f32; 4] {
    [rgb[0], rgb[1], rgb[2], w]
}

const MESH_ATTRIBUTES: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];
const TRIANGLE_ATTRIBUTES: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![0 => Uint32x3];
const SEGMENT_ATTRIBUTES: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];
const POINT_ATTRIBUTES: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![0 => Float32x3];

fn layout(
    stride: usize,
    step_mode: wgpu::VertexStepMode,
    attributes: &[wgpu::VertexAttribute],
) -> wgpu::VertexBufferLayout<'_> {
    wgpu::VertexBufferLayout {
        array_stride: stride as u64,
        step_mode,
        attributes,
    }
}

fn uniform_layout(device: &wgpu::Device, label: &str) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

/// Fixed-function state for one scene pipeline.
struct PipelineSpec<'a> {
    label: &'a str,
    vs: &'a str,
    fs: &'a str,
    buffers: &'a [Option<wgpu::VertexBufferLayout<'a>>],
    topology: wgpu::PrimitiveTopology,
    depth_write: bool,
    depth_compare: wgpu::CompareFunction,
    blend: Option<wgpu::BlendState>,
}

/// Every render pipeline the scene needs; created once per device.
pub(crate) struct Pipelines {
    frame_layout: wgpu::BindGroupLayout,
    object_layout: wgpu::BindGroupLayout,
    storage_layout: Option<wgpu::BindGroupLayout>,
    sample_count: u32,
    background: wgpu::RenderPipeline,
    mesh_smooth: wgpu::RenderPipeline,
    mesh_flat: wgpu::RenderPipeline,
    mesh_points: wgpu::RenderPipeline,
    /// Screen-space quads pulled from a storage buffer (needs vertex-stage storage buffers).
    mesh_wire: Option<wgpu::RenderPipeline>,
    /// Fallback: native 1 px line list.
    mesh_wire_native: wgpu::RenderPipeline,
    lines: wgpu::RenderPipeline,
    grid: wgpu::RenderPipeline,
    points: wgpu::RenderPipeline,
}

impl Pipelines {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        Self::with_wide_wireframe(device, true)
    }

    /// `wide_wireframe: false` forces the native-line wireframe fallback (for tests).
    pub(crate) fn with_wide_wireframe(device: &wgpu::Device, wide_wireframe: bool) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("stlv scene"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/scene.wgsl").into()),
        });
        let frame_layout = uniform_layout(device, "stlv frame");
        let object_layout = uniform_layout(device, "stlv object");
        let sample_count = probe_sample_count(device);
        let scene_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("stlv scene"),
            bind_group_layouts: &[Some(&frame_layout), Some(&object_layout)],
            immediate_size: 0,
        });

        let create = |layout: &wgpu::PipelineLayout, spec: PipelineSpec<'_>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(spec.label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some(spec.vs),
                    compilation_options: Default::default(),
                    buffers: spec.buffers,
                },
                primitive: wgpu::PrimitiveState {
                    topology: spec.topology,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(spec.depth_write),
                    depth_compare: Some(spec.depth_compare),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: sample_count,
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(spec.fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: COLOR_FORMAT,
                        blend: spec.blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let opaque = |label, vs, fs, buffers, topology| PipelineSpec {
            label,
            vs,
            fs,
            buffers,
            topology,
            depth_write: true,
            depth_compare: wgpu::CompareFunction::LessEqual,
            blend: None,
        };
        use wgpu::PrimitiveTopology::{LineList, TriangleList};
        use wgpu::VertexStepMode::{Instance, Vertex as PerVertex};
        let stride = size_of::<Vertex>();

        let background = create(
            &scene_layout,
            PipelineSpec {
                depth_write: false,
                depth_compare: wgpu::CompareFunction::Always,
                ..opaque(
                    "stlv background",
                    "vs_background",
                    "fs_background",
                    &[],
                    TriangleList,
                )
            },
        );
        let mesh_buffers = [Some(layout(stride, PerVertex, &MESH_ATTRIBUTES))];
        let mesh_smooth = create(
            &scene_layout,
            opaque(
                "stlv smooth",
                "vs_mesh",
                "fs_smooth",
                &mesh_buffers,
                TriangleList,
            ),
        );
        let mesh_flat = create(
            &scene_layout,
            opaque(
                "stlv flat",
                "vs_mesh",
                "fs_flat",
                &mesh_buffers,
                TriangleList,
            ),
        );
        let mesh_wire_native = create(
            &scene_layout,
            opaque(
                "stlv wire (native)",
                "vs_mesh",
                "fs_lit_primitive",
                &mesh_buffers,
                LineList,
            ),
        );
        let instanced_vertices = [Some(layout(stride, Instance, &MESH_ATTRIBUTES))];
        let mesh_points = create(
            &scene_layout,
            opaque(
                "stlv mesh points",
                "vs_mesh_points",
                "fs_lit_primitive",
                &instanced_vertices,
                TriangleList,
            ),
        );
        let segments = [Some(layout(24, Instance, &SEGMENT_ATTRIBUTES))];
        let lines = create(
            &scene_layout,
            opaque(
                "stlv lines",
                "vs_lines",
                "fs_unlit",
                &segments,
                TriangleList,
            ),
        );
        // VTK blends translucent actors with (SRC_ALPHA, ONE_MINUS_SRC_ALPHA, ONE, ONE_MINUS_SRC_ALPHA).
        let grid = create(
            &scene_layout,
            PipelineSpec {
                depth_write: false,
                blend: Some(wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::SrcAlpha,
                        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                        operation: wgpu::BlendOperation::Add,
                    },
                }),
                ..opaque("stlv grid", "vs_lines", "fs_unlit", &segments, TriangleList)
            },
        );
        let point_buffers = [Some(layout(12, Instance, &POINT_ATTRIBUTES))];
        let points = create(
            &scene_layout,
            opaque(
                "stlv points",
                "vs_points",
                "fs_unlit",
                &point_buffers,
                TriangleList,
            ),
        );

        // Vertex-stage storage buffers are missing on some downlevel (GL) devices: probe.
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let storage_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("stlv mesh storage"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let wire_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("stlv wire"),
            bind_group_layouts: &[
                Some(&frame_layout),
                Some(&object_layout),
                Some(&storage_layout),
            ],
            immediate_size: 0,
        });
        let triangles = [Some(layout(12, Instance, &TRIANGLE_ATTRIBUTES))];
        let mesh_wire = create(
            &wire_layout,
            opaque(
                "stlv wire",
                "vs_mesh_wire",
                "fs_lit_primitive",
                &triangles,
                TriangleList,
            ),
        );
        let (mesh_wire, storage_layout) = match pollster::block_on(scope.pop()) {
            None if wide_wireframe => (Some(mesh_wire), Some(storage_layout)),
            None => (None, None),
            Some(err) => {
                log::warn!("wide wireframe unavailable, using native lines: {err}");
                (None, None)
            }
        };

        Self {
            frame_layout,
            object_layout,
            storage_layout,
            sample_count,
            background,
            mesh_smooth,
            mesh_flat,
            mesh_points,
            mesh_wire,
            mesh_wire_native,
            lines,
            grid,
            points,
        }
    }
}

/// Multisampled color + depth attachments and the single-sample resolve target.
pub(crate) struct Targets {
    pub size: (u32, u32),
    msaa_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    pub resolved: wgpu::Texture,
    pub resolved_view: wgpu::TextureView,
}

impl Targets {
    pub(crate) fn new(device: &wgpu::Device, pipelines: &Pipelines, size: (u32, u32)) -> Self {
        let extent = wgpu::Extent3d {
            width: size.0.max(1),
            height: size.1.max(1),
            depth_or_array_layers: 1,
        };
        let texture = |label, format, sample_count, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent,
                mip_level_count: 1,
                sample_count,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        use wgpu::TextureUsages as U;
        let samples = pipelines.sample_count;
        let msaa = texture(
            "stlv msaa color",
            COLOR_FORMAT,
            samples,
            U::RENDER_ATTACHMENT,
        );
        let depth = texture("stlv depth", DEPTH_FORMAT, samples, U::RENDER_ATTACHMENT);
        let resolved = texture(
            "stlv color",
            COLOR_FORMAT,
            1,
            U::RENDER_ATTACHMENT | U::TEXTURE_BINDING | U::COPY_SRC,
        );
        Self {
            size: (extent.width, extent.height),
            msaa_view: msaa.create_view(&Default::default()),
            depth_view: depth.create_view(&Default::default()),
            resolved_view: resolved.create_view(&Default::default()),
            resolved,
        }
    }
}

struct GpuMeshChunk {
    vertices: wgpu::Buffer,
    vertex_count: u32,
    indices: wgpu::Buffer,
    triangle_count: u32,
    storage: Option<wgpu::BindGroup>,
    /// Edge index list for the native-line wireframe fallback.
    native_edges: Option<(wgpu::Buffer, u32)>,
}

struct GpuToolpathChunk {
    segments: Option<wgpu::Buffer>,
    segment_count: u32,
    points: Option<wgpu::Buffer>,
    point_count: u32,
}

enum GpuGeometryKind {
    Mesh(Vec<GpuMeshChunk>),
    Toolpath(Vec<GpuToolpathChunk>),
}

/// A model's buffers on the GPU, positioned relative to `origin`.
pub(crate) struct GpuGeometry {
    kind: GpuGeometryKind,
    origin: DVec3,
}

fn buffer(
    device: &wgpu::Device,
    label: &str,
    contents: &[u8],
    usage: wgpu::BufferUsages,
) -> Option<wgpu::Buffer> {
    (!contents.is_empty()).then(|| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents,
            usage,
        })
    })
}

impl GpuGeometry {
    /// Uploads `geometry`; fails (instead of panicking) when the device runs out of memory.
    pub(crate) fn upload(
        device: &wgpu::Device,
        pipelines: &Pipelines,
        geometry: &Geometry,
    ) -> Result<Self, String> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        use wgpu::BufferUsages as U;
        let kind = match geometry {
            Geometry::Mesh { chunks, .. } => GpuGeometryKind::Mesh(
                chunks
                    .iter()
                    .filter(|c| !c.indices.is_empty())
                    .filter_map(|c| {
                        let vertices = buffer(
                            device,
                            "stlv mesh vertices",
                            bytemuck::cast_slice(&c.vertices),
                            U::VERTEX | U::STORAGE,
                        )?;
                        let indices = buffer(
                            device,
                            "stlv mesh indices",
                            bytemuck::cast_slice(&c.indices),
                            U::INDEX | U::VERTEX,
                        )?;
                        let storage = pipelines.storage_layout.as_ref().map(|layout| {
                            device.create_bind_group(&wgpu::BindGroupDescriptor {
                                label: Some("stlv mesh storage"),
                                layout,
                                entries: &[wgpu::BindGroupEntry {
                                    binding: 0,
                                    resource: vertices.as_entire_binding(),
                                }],
                            })
                        });
                        let native_edges = pipelines.mesh_wire.is_none().then(|| {
                            let edges: Vec<u32> = c
                                .indices
                                .chunks_exact(3)
                                .flat_map(|t| [t[0], t[1], t[1], t[2], t[2], t[0]])
                                .collect();
                            let buf = buffer(
                                device,
                                "stlv mesh edges",
                                bytemuck::cast_slice(&edges),
                                U::INDEX,
                            );
                            buf.map(|b| (b, edges.len() as u32))
                        });
                        Some(GpuMeshChunk {
                            vertices,
                            vertex_count: c.vertices.len() as u32,
                            indices,
                            triangle_count: (c.indices.len() / 3) as u32,
                            storage,
                            native_edges: native_edges.flatten(),
                        })
                    })
                    .collect(),
            ),
            Geometry::Toolpath { chunks, .. } => GpuGeometryKind::Toolpath(
                chunks
                    .iter()
                    .map(|c| GpuToolpathChunk {
                        segments: buffer(
                            device,
                            "stlv toolpath segments",
                            bytemuck::cast_slice(&c.segments),
                            U::VERTEX,
                        ),
                        segment_count: c.segments.len() as u32,
                        points: buffer(
                            device,
                            "stlv toolpath points",
                            bytemuck::cast_slice(&c.points),
                            U::VERTEX,
                        ),
                        point_count: c.points.len() as u32,
                    })
                    .collect(),
            ),
        };
        let validation_error = pollster::block_on(validation.pop());
        if let Some(err) = pollster::block_on(scope.pop()).or(validation_error) {
            return Err(format!("GPU upload failed: {err}"));
        }
        Ok(Self {
            kind,
            origin: geometry.origin(),
        })
    }

    pub(crate) fn is_toolpath(&self) -> bool {
        matches!(self.kind, GpuGeometryKind::Toolpath(_))
    }
}

/// The ground grid's line segments (renderer.py `update_grid`), relative to the world origin.
pub(crate) struct GpuGrid {
    segments: wgpu::Buffer,
    count: u32,
    pub bounds: [[f64; 2]; 3],
}

impl GpuGrid {
    pub(crate) fn new(device: &wgpu::Device, model_bounds: Option<&[[f64; 2]; 3]>) -> Self {
        let (segments, bounds) = grid_segments(model_bounds);
        let segments_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("stlv grid"),
            contents: bytemuck::cast_slice(&segments),
            usage: wgpu::BufferUsages::VERTEX,
        });
        Self {
            segments: segments_buf,
            count: segments.len() as u32,
            bounds,
        }
    }
}

/// Grid lines (24 divisions, centred at the world origin) and the grid's bounds.
pub(crate) fn grid_segments(
    model_bounds: Option<&[[f64; 2]; 3]>,
) -> (Vec<[[f32; 3]; 2]>, [[f64; 2]; 3]) {
    let finite_or = |v: f64, d: f64| if v.is_finite() { v } else { d };
    let (size, z, cx, cy) = match model_bounds {
        None => (150.0, 0.0, 0.0, 0.0),
        Some(b) => {
            let size = ((b[0][1] - b[0][0]).max(b[1][1] - b[1][0]) * 2.5).max(10.0);
            let z = b[2][0] - (b[2][1] - b[2][0]) * 0.01;
            // `max` above swallows NaN; keep the grid finite like Python's isnan/isinf guard.
            // Deviation: the grid is centred under the model, not at the world origin
            // (Python), so models placed away from the origin still sit on it.
            (
                finite_or(size, 150.0),
                finite_or(z, 0.0),
                finite_or((b[0][0] + b[0][1]) / 2.0, 0.0),
                finite_or((b[1][0] + b[1][1]) / 2.0, 0.0),
            )
        }
    };
    let half = size / 2.0;
    let step = size / GRID_DIVISIONS as f64;
    let mut segments = Vec::with_capacity(2 * (GRID_DIVISIONS + 1));
    let (x0, x1, y0, y1) = (cx - half, cx + half, cy - half, cy + half);
    let z32 = z as f32;
    for i in 0..=GRID_DIVISIONS {
        let o = -half + i as f64 * step;
        let (x, y) = ((cx + o) as f32, (cy + o) as f32);
        segments.push([[x0 as f32, y, z32], [x1 as f32, y, z32]]);
        segments.push([[x, y0 as f32, z32], [x, y1 as f32, z32]]);
    }
    (segments, [[x0, x1], [y0, y1], [z, z]])
}

/// Union of two `[[min, max]; 3]` boxes.
pub(crate) fn union_bounds(a: &[[f64; 2]; 3], b: &[[f64; 2]; 3]) -> [[f64; 2]; 3] {
    std::array::from_fn(|i| [a[i][0].min(b[i][0]), a[i][1].max(b[i][1])])
}

/// Per-renderer uniform buffers.
pub(crate) struct SceneUniforms {
    frame: wgpu::Buffer,
    frame_bind_group: wgpu::BindGroup,
    model: wgpu::Buffer,
    model_bind_group: wgpu::BindGroup,
    grid: wgpu::Buffer,
    grid_bind_group: wgpu::BindGroup,
}

impl SceneUniforms {
    pub(crate) fn new(device: &wgpu::Device, pipelines: &Pipelines) -> Self {
        let make = |label, size: usize, layout| {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: size as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            });
            (buffer, bind_group)
        };
        let (frame, frame_bind_group) = make(
            "stlv frame",
            size_of::<FrameUniform>(),
            &pipelines.frame_layout,
        );
        let (model, model_bind_group) = make(
            "stlv model",
            size_of::<ObjectUniform>(),
            &pipelines.object_layout,
        );
        let (grid, grid_bind_group) = make(
            "stlv grid",
            size_of::<ObjectUniform>(),
            &pipelines.object_layout,
        );
        Self {
            frame,
            frame_bind_group,
            model,
            model_bind_group,
            grid,
            grid_bind_group,
        }
    }
}

/// What to draw in one frame.
pub(crate) struct DrawRequest<'a> {
    pub camera: &'a Camera,
    pub geometry: Option<(&'a GpuGeometry, [[f64; 2]; 3])>,
    pub grid: Option<&'a GpuGrid>,
    pub shading: ShadingMode,
    pub color: [f32; 3],
    /// World z range to keep (cross-section); `None` shows everything.
    pub clip_z: Option<[f64; 2]>,
}

fn model_view(view: &DMat4, origin: DVec3) -> [[f32; 4]; 4] {
    (*view * DMat4::from_translation(origin))
        .as_mat4()
        .to_cols_array_2d()
}

/// Records the scene into `targets` (MSAA render + linear resolve, as VTK's blit).
pub(crate) fn encode_scene(
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
    pipelines: &Pipelines,
    targets: &Targets,
    uniforms: &SceneUniforms,
    req: &DrawRequest<'_>,
) {
    let (w, h) = (targets.size.0 as f64, targets.size.1 as f64);
    let bounds = match (req.geometry.map(|g| g.1), req.grid.map(|g| g.bounds)) {
        (Some(a), Some(b)) => Some(union_bounds(&a, &b)),
        (a, b) => a.or(b),
    };
    let (near, far) = bounds.map_or((0.01, 1000.01), |b| req.camera.clipping_range(&b));
    let view = req.camera.view_matrix();
    let proj = req.camera.projection_matrix(w / h, near, far);
    let light_dir = view.transform_vector3(-SCENE_LIGHT_POSITION).normalize();
    let frame = FrameUniform {
        view: view.as_mat4().to_cols_array_2d(),
        proj: proj.as_mat4().to_cols_array_2d(),
        viewport: [w as f32, h as f32, near as f32, far as f32],
        light1_dir: vec4(light_dir.as_vec3().to_array(), 0.0),
        light0_color: vec4([HEADLIGHT_INTENSITY; 3], 0.0),
        light1_color: vec4(SCENE_LIGHT_COLOR.map(|c| c * SCENE_LIGHT_INTENSITY), 0.0),
        bg_bottom: vec4(stlv_core::palette::VIEWPORT_BG_BOTTOM, 1.0),
        bg_top: vec4(stlv_core::palette::VIEWPORT_BG_TOP, 1.0),
    };
    queue.write_buffer(&uniforms.frame, 0, bytemuck::bytes_of(&frame));

    let c = req.color;
    let toolpath = req.geometry.is_some_and(|(g, _)| g.is_toolpath());
    let origin = req.geometry.map_or(DVec3::ZERO, |(g, _)| g.origin);
    let clip = req.clip_z.map_or([0.0; 4], |[lo, hi]| {
        [(lo - origin.z) as f32, (hi - origin.z) as f32, 1.0, 0.0]
    });
    let model = ObjectUniform {
        model_view: model_view(&view, origin),
        ambient: vec4(c.map(|c| c * AMBIENT), 0.0),
        diffuse: vec4(c.map(|c| c * DIFFUSE), 0.0),
        // vtkProperty::SetColor sets the specular color too.
        specular: vec4(c.map(|c| c * SPECULAR), SPECULAR_POWER),
        color: vec4(c, 1.0),
        size: [
            if toolpath {
                TOOLPATH_LINE_WIDTH
            } else {
                MESH_LINE_WIDTH
            },
            POINT_SIZE,
            0.0,
            0.0,
        ],
        clip,
    };
    queue.write_buffer(&uniforms.model, 0, bytemuck::bytes_of(&model));
    let grid = ObjectUniform {
        model_view: model_view(&view, DVec3::ZERO),
        ambient: [0.0; 4],
        diffuse: [0.0; 4],
        specular: [0.0; 4],
        color: vec4(
            stlv_core::palette::GRID_COLOR,
            stlv_core::palette::GRID_OPACITY,
        ),
        size: [GRID_LINE_WIDTH, 1.0, 0.0, 0.0],
        clip: [0.0; 4],
    };
    queue.write_buffer(&uniforms.grid, 0, bytemuck::bytes_of(&grid));

    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("stlv scene"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &targets.msaa_view,
            depth_slice: None,
            resolve_target: Some(&targets.resolved_view),
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Discard,
            },
        })],
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: &targets.depth_view,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Clear(1.0),
                store: wgpu::StoreOp::Discard,
            }),
            stencil_ops: None,
        }),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_bind_group(0, &uniforms.frame_bind_group, &[]);
    pass.set_bind_group(1, &uniforms.model_bind_group, &[]);
    pass.set_pipeline(&pipelines.background);
    pass.draw(0..3, 0..1);

    if let Some((geometry, _)) = req.geometry {
        draw_geometry(&mut pass, pipelines, geometry, req.shading);
    }
    if let Some(grid) = req.grid {
        pass.set_bind_group(1, &uniforms.grid_bind_group, &[]);
        pass.set_pipeline(&pipelines.grid);
        pass.set_vertex_buffer(0, grid.segments.slice(..));
        pass.draw(0..6, 0..grid.count);
    }
}

fn draw_geometry(
    pass: &mut wgpu::RenderPass<'_>,
    pipelines: &Pipelines,
    geometry: &GpuGeometry,
    shading: ShadingMode,
) {
    match &geometry.kind {
        GpuGeometryKind::Mesh(chunks) => {
            for c in chunks {
                match shading {
                    ShadingMode::Smooth | ShadingMode::Flat => {
                        let pipeline = if shading == ShadingMode::Flat {
                            &pipelines.mesh_flat
                        } else {
                            &pipelines.mesh_smooth
                        };
                        pass.set_pipeline(pipeline);
                        pass.set_vertex_buffer(0, c.vertices.slice(..));
                        pass.set_index_buffer(c.indices.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(0..c.triangle_count * 3, 0, 0..1);
                    }
                    ShadingMode::Wireframe => {
                        match (&pipelines.mesh_wire, &c.storage, &c.native_edges) {
                            (Some(pipeline), Some(storage), _) => {
                                pass.set_pipeline(pipeline);
                                pass.set_bind_group(2, storage, &[]);
                                pass.set_vertex_buffer(0, c.indices.slice(..));
                                pass.draw(0..18, 0..c.triangle_count);
                            }
                            (_, _, Some((edges, count))) => {
                                pass.set_pipeline(&pipelines.mesh_wire_native);
                                pass.set_vertex_buffer(0, c.vertices.slice(..));
                                pass.set_index_buffer(edges.slice(..), wgpu::IndexFormat::Uint32);
                                pass.draw_indexed(0..*count, 0, 0..1);
                            }
                            _ => {}
                        }
                    }
                    ShadingMode::Points => {
                        pass.set_pipeline(&pipelines.mesh_points);
                        pass.set_vertex_buffer(0, c.vertices.slice(..));
                        pass.draw(0..6, 0..c.vertex_count);
                    }
                }
            }
        }
        GpuGeometryKind::Toolpath(chunks) => {
            for c in chunks {
                if shading == ShadingMode::Points {
                    if let Some(points) = &c.points {
                        pass.set_pipeline(&pipelines.points);
                        pass.set_vertex_buffer(0, points.slice(..));
                        pass.draw(0..6, 0..c.point_count);
                    }
                } else if let Some(segments) = &c.segments {
                    pass.set_pipeline(&pipelines.lines);
                    pass.set_vertex_buffer(0, segments.slice(..));
                    pass.draw(0..6, 0..c.segment_count);
                }
            }
        }
    }
}

#[cfg(test)]
impl Pipelines {
    pub(crate) fn mesh_wire_unavailable(&self) -> bool {
        self.mesh_wire.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_matches_update_grid() {
        let (segs, b) = grid_segments(None);
        assert_eq!(segs.len(), 50);
        assert_eq!(b, [[-75.0, 75.0], [-75.0, 75.0], [0.0, 0.0]]);
        let (segs, b) = grid_segments(Some(&[[0.0, 10.0], [-2.0, 30.0], [5.0, 105.0]]));
        // max(10, 32) * 2.5 = 80, floor 1% of dz below zmin.
        // Centred under the model: x centre 5, y centre 14.
        assert_eq!(b, [[-35.0, 45.0], [-26.0, 54.0], [4.0, 4.0]]);
        assert_eq!(segs[0], [[-35.0, -26.0, 4.0], [45.0, -26.0, 4.0]]);
        assert_eq!(segs[49], [[45.0, -26.0, 4.0], [45.0, 54.0, 4.0]]);
        let (_, b) = grid_segments(Some(&[[0.0, 1.0], [0.0, 1.0], [0.0, 0.0]]));
        assert_eq!(b[0], [-4.5, 5.5], "minimum grid size is 10");
        // Far from the origin: still under the model.
        let (_, b) = grid_segments(Some(&[[200.0, 220.0], [-90.0, -70.0], [0.0, 5.0]]));
        assert_eq!((b[0], b[1]), ([185.0, 235.0], [-105.0, -55.0]));
        let (_, b) = grid_segments(Some(&[[0.0, 1.0], [0.0, 1.0], [f64::NEG_INFINITY, 0.0]]));
        assert_eq!(b[2], [0.0, 0.0]);
    }

    #[test]
    fn uniform_sizes_are_16_byte_multiples() {
        assert_eq!(size_of::<FrameUniform>() % 16, 0);
        assert_eq!(size_of::<ObjectUniform>() % 16, 0);
    }
}
