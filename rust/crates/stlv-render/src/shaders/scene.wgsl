// Scene shaders replicating VTK 9's OpenGL2 polydata pipeline as configured by renderer.py.
// All colors are written untouched into an Rgba8Unorm target, like VTK's framebuffer.

struct Frame {
    view: mat4x4<f32>,
    proj: mat4x4<f32>,
    // x, y: target size in pixels; z: near clipping distance.
    viewport: vec4<f32>,
    // VTK `lightDirectionVC1`: direction the scene light travels, in view space.
    light1_dir: vec4<f32>,
    // Light colors (diffuse color x intensity).
    light0_color: vec4<f32>,
    light1_color: vec4<f32>,
    bg_bottom: vec4<f32>,
    bg_top: vec4<f32>,
};

// Per-object transform and VTK material. Objects are stored relative to an origin, so
// `model_view` = view x translate(origin) (computed in f64 on the CPU).
struct Object {
    model_view: mat4x4<f32>,
    ambient: vec4<f32>,  // ambient x ambient color
    diffuse: vec4<f32>,  // diffuse x diffuse color
    specular: vec4<f32>, // rgb: specular x specular color, a: specular power
    color: vec4<f32>,    // unlit color, a: opacity
    size: vec4<f32>,     // x: line width px, y: point size px
    // Z cross-section in object coordinates: x = lowest, y = highest z kept; z > 0.5 when
    // active. Fragments outside are discarded and back faces (the exposed inside) darkened.
    clip: vec4<f32>,
};

@group(0) @binding(0) var<uniform> frame: Frame;
@group(1) @binding(0) var<uniform> material: Object;
// Interleaved mesh vertices (position, normal), read by the wireframe vertex shader.
@group(2) @binding(0) var<storage, read> mesh_vertices: array<f32>;

struct LitVarying {
    @builtin(position) clip: vec4<f32>,
    @location(0) p_vc: vec3<f32>,
    @location(1) n_vc: vec3<f32>,
    // Object-space z, for the cross-section.
    @location(2) z: f32,
};

struct UnlitVarying {
    @builtin(position) clip: vec4<f32>,
    @location(0) z: f32,
};

fn clipped(z: f32) -> bool {
    return material.clip.z > 0.5 && (z < material.clip.x || z > material.clip.y);
}

// While a cross-section is active, back faces are the model's inside seen through the cut.
fn shade_back(c: vec4<f32>, front: bool) -> vec4<f32> {
    if !front && material.clip.z > 0.5 {
        return vec4<f32>(c.rgb * 0.45, c.a);
    }
    return c;
}

fn safe_normalize(v: vec3<f32>) -> vec3<f32> {
    let len2 = dot(v, v);
    if len2 > 1e-30 {
        return v * inverseSqrt(len2);
    }
    return vec3<f32>(0.0, 0.0, 1.0);
}

// VTK "lighting complexity 2": headlight (light 0, travelling along -Z in view space) plus
// one directional scene light; ambient has no light dependence.
fn vtk_lighting(n: vec3<f32>, p_vc: vec3<f32>) -> vec4<f32> {
    let view_dir = safe_normalize(-p_vc);
    let power = material.specular.a;
    let l0 = vec3<f32>(0.0, 0.0, -1.0);
    let l1 = frame.light1_dir.xyz;
    var df = max(0.0, dot(n, -l0));
    var diffuse = df * frame.light0_color.rgb;
    var sf = sign(df) * pow(max(1e-5, dot(reflect(l0, n), view_dir)), power);
    var specular = sf * frame.light0_color.rgb;
    df = max(0.0, dot(n, -l1));
    diffuse += df * frame.light1_color.rgb;
    sf = sign(df) * pow(max(1e-5, dot(reflect(l1, n), view_dir)), power);
    specular += sf * frame.light1_color.rgb;
    let rgb = material.ambient.rgb + diffuse * material.diffuse.rgb + specular * material.specular.rgb;
    return vec4<f32>(rgb, material.color.a);
}

fn to_view(p: vec3<f32>) -> vec3<f32> {
    return (material.model_view * vec4<f32>(p, 1.0)).xyz;
}

fn normal_to_view(n: vec3<f32>) -> vec3<f32> {
    return (material.model_view * vec4<f32>(n, 0.0)).xyz;
}

// Two triangles per screen-space quad: corner -> (end 0/1, side -1/+1).
fn corner_end(c: u32) -> u32 {
    return select(0u, 1u, c == 1u || c == 2u || c == 4u);
}

fn corner_side(c: u32) -> f32 {
    return select(-1.0, 1.0, c == 2u || c == 4u || c == 5u);
}

// Where the segment a..b (view space) enters/leaves the visible side of the near plane,
// as parameters (t0, t1); t0 > t1 when the whole segment is behind the near plane.
fn near_clip(za: f32, zb: f32) -> vec2<f32> {
    let zn = -frame.viewport.z;
    var t = vec2<f32>(0.0, 1.0);
    if za > zn && zb > zn {
        return vec2<f32>(1.0, 0.0);
    }
    if za > zn {
        t.x = (zn - za) / (zb - za);
    }
    if zb > zn {
        t.y = (zn - za) / (zb - za);
    }
    return t;
}

// Clip-space position of one corner of a `width_px` wide quad along the view-space segment
// a..b. Like OpenGL wide lines (what VTK draws), the width is measured along the minor axis:
// an x-major line covers `width_px` in every pixel column. Segments entirely behind the
// near plane collapse outside the clip volume.
fn line_corner(a: vec3<f32>, b: vec3<f32>, t: vec2<f32>, corner: u32, width_px: f32) -> vec4<f32> {
    if t.x > t.y {
        return vec4<f32>(2.0, 2.0, 2.0, 1.0);
    }
    let c0 = frame.proj * vec4<f32>(mix(a, b, t.x), 1.0);
    let c1 = frame.proj * vec4<f32>(mix(a, b, t.y), 1.0);
    let half_px = frame.viewport.xy * 0.5;
    let d = c1.xy / c1.w - c0.xy / c0.w;
    let x_major = abs(d.x * half_px.x) >= abs(d.y * half_px.y);
    let axis = select(vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0), x_major);
    let c = select(c0, c1, corner_end(corner) == 1u);
    let offset = axis * (corner_side(corner) * width_px * 0.5) / half_px;
    return vec4<f32>(c.xy + offset * c.w, c.z, c.w);
}

fn quad_offset(corner: u32, size_px: f32) -> vec2<f32> {
    let x = select(-0.5, 0.5, corner_end(corner) == 1u);
    let y = corner_side(corner) * 0.5;
    return vec2<f32>(x, y) * size_px / (frame.viewport.xy * 0.5);
}

// ---------------------------------------------------------------- background

@vertex
fn vs_background(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 1.0, 1.0);
}

@fragment
fn fs_background(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let t = 1.0 - pos.y / frame.viewport.y;
    return vec4<f32>(mix(frame.bg_bottom.rgb, frame.bg_top.rgb, t), 1.0);
}

// ---------------------------------------------------------------- triangles

@vertex
fn vs_mesh(@location(0) position: vec3<f32>, @location(1) normal: vec3<f32>) -> LitVarying {
    var out: LitVarying;
    let p = to_view(position);
    out.clip = frame.proj * vec4<f32>(p, 1.0);
    out.p_vc = p;
    out.n_vc = normal_to_view(normal);
    out.z = position.z;
    return out;
}

// Phong interpolation; back faces are lit with the flipped normal (two-sided lighting).
@fragment
fn fs_smooth(in: LitVarying, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    if clipped(in.z) {
        discard;
    }
    var n = safe_normalize(in.n_vc);
    if !front {
        n = -n;
    }
    return shade_back(vtk_lighting(n, in.p_vc), front);
}

// Flat interpolation: VTK derives the facet normal from screen-space derivatives and
// turns it towards the camera.
@fragment
fn fs_flat(in: LitVarying, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    // Derivatives first: they need uniform control flow.
    var n = safe_normalize(cross(dpdx(in.p_vc), dpdy(in.p_vc)));
    if clipped(in.z) {
        discard;
    }
    if dot(n, in.p_vc) > 0.0 {
        n = -n;
    }
    return shade_back(vtk_lighting(n, in.p_vc), front);
}

// Lines and points are never back-facing in OpenGL, so their normals are used as is.
@fragment
fn fs_lit_primitive(in: LitVarying) -> @location(0) vec4<f32> {
    if clipped(in.z) {
        discard;
    }
    return vtk_lighting(safe_normalize(in.n_vc), in.p_vc);
}

// ---------------------------------------------------------------- mesh wireframe / points

fn mesh_vertex_position(i: u32) -> vec3<f32> {
    let b = i * 6u;
    return vec3<f32>(mesh_vertices[b], mesh_vertices[b + 1u], mesh_vertices[b + 2u]);
}

fn mesh_vertex_normal(i: u32) -> vec3<f32> {
    let b = i * 6u;
    return vec3<f32>(mesh_vertices[b + 3u], mesh_vertices[b + 4u], mesh_vertices[b + 5u]);
}

// One instance per triangle, 3 edge quads (18 vertices) each, like VTK's wireframe which
// draws every triangle's edges.
@vertex
fn vs_mesh_wire(@builtin(vertex_index) vi: u32, @location(0) tri: vec3<u32>) -> LitVarying {
    let edge = vi / 6u;
    let corner = vi % 6u;
    let ia = tri[edge];
    let ib = tri[(edge + 1u) % 3u];
    let pa = mesh_vertex_position(ia);
    let pb = mesh_vertex_position(ib);
    let a = to_view(pa);
    let b = to_view(pb);
    let t = near_clip(a.z, b.z);
    let s = select(t.x, t.y, corner_end(corner) == 1u);
    var out: LitVarying;
    out.clip = line_corner(a, b, t, corner, material.size.x);
    out.z = mix(pa.z, pb.z, s);
    out.p_vc = mix(a, b, s);
    out.n_vc = mix(normal_to_view(mesh_vertex_normal(ia)), normal_to_view(mesh_vertex_normal(ib)), s);
    return out;
}

@vertex
fn vs_mesh_points(
    @builtin(vertex_index) vi: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
) -> LitVarying {
    var out: LitVarying;
    let p = to_view(position);
    let c = frame.proj * vec4<f32>(p, 1.0);
    out.clip = vec4<f32>(c.xy + quad_offset(vi, material.size.y) * c.w, c.z, c.w);
    out.p_vc = p;
    out.n_vc = normal_to_view(normal);
    out.z = position.z;
    return out;
}

// ---------------------------------------------------------------- unlit lines / points

@vertex
fn vs_lines(@builtin(vertex_index) vi: u32, @location(0) p0: vec3<f32>, @location(1) p1: vec3<f32>) -> UnlitVarying {
    let a = to_view(p0);
    let b = to_view(p1);
    let t = near_clip(a.z, b.z);
    var out: UnlitVarying;
    out.clip = line_corner(a, b, t, vi, material.size.x);
    out.z = mix(p0.z, p1.z, select(t.x, t.y, corner_end(vi) == 1u));
    return out;
}

@vertex
fn vs_points(@builtin(vertex_index) vi: u32, @location(0) position: vec3<f32>) -> UnlitVarying {
    let c = frame.proj * vec4<f32>(to_view(position), 1.0);
    var out: UnlitVarying;
    out.clip = vec4<f32>(c.xy + quad_offset(vi, material.size.y) * c.w, c.z, c.w);
    out.z = position.z;
    return out;
}

// VTK with lighting off: ambient + diffuse colors, i.e. the plain actor color.
@fragment
fn fs_unlit(in: UnlitVarying) -> @location(0) vec4<f32> {
    if clipped(in.z) {
        discard;
    }
    return material.color;
}
