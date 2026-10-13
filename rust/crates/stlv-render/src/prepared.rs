use std::sync::Arc;

use glam::DVec3;

use stlv_core::ModelData;
use stlv_core::mesh::{DEFAULT_FEATURE_ANGLE_DEG, RenderMesh, compute_render_mesh};

/// Largest vertex count per GPU chunk: 24-byte vertices stay well below wgpu's default
/// 128 MiB storage-binding / 256 MiB buffer limits.
const MAX_CHUNK_VERTICES: usize = 4 << 20;
/// Largest triangle count per GPU chunk (12-byte index triples).
const MAX_CHUNK_TRIANGLES: usize = 8 << 20;
/// Largest toolpath segment count per GPU chunk (24-byte endpoint pairs).
const MAX_CHUNK_SEGMENTS: usize = 4 << 20;

/// Interleaved mesh vertex as uploaded to the GPU.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
}

/// A piece of a split-normal mesh small enough for one set of GPU buffers.
#[derive(Debug, Default)]
pub(crate) struct MeshChunk {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

/// Toolpath data: one `[p0, p1]` record per segment plus the raw vertices for point mode.
#[derive(Debug, Default)]
pub(crate) struct ToolpathChunk {
    pub segments: Vec<[[f32; 3]; 2]>,
    pub points: Vec<[f32; 3]>,
}

/// Render geometry with positions relative to `origin` (the bounds center) so that f32
/// precision is not wasted on large world offsets.
#[derive(Debug)]
pub(crate) enum Geometry {
    Mesh {
        chunks: Vec<MeshChunk>,
        origin: DVec3,
    },
    Toolpath {
        chunks: Vec<ToolpathChunk>,
        origin: DVec3,
    },
}

impl Geometry {
    pub(crate) fn build(model: &ModelData) -> Self {
        let center = DVec3::from(model.center());
        let origin = if center.is_finite() {
            center
        } else {
            DVec3::ZERO
        };
        if model.is_toolpath() {
            Self::Toolpath {
                chunks: toolpath_chunks(model, origin),
                origin,
            }
        } else {
            let mut mesh = compute_render_mesh(model, DEFAULT_FEATURE_ANGLE_DEG);
            for p in &mut mesh.positions {
                *p = relative(*p, origin);
            }
            Self::Mesh {
                chunks: mesh_chunks(mesh),
                origin,
            }
        }
    }

    pub(crate) fn origin(&self) -> DVec3 {
        match self {
            Self::Mesh { origin, .. } | Self::Toolpath { origin, .. } => *origin,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        match self {
            Self::Mesh { chunks, .. } => chunks.iter().all(|c| c.indices.is_empty()),
            Self::Toolpath { chunks, .. } => chunks
                .iter()
                .all(|c| c.segments.is_empty() && c.points.is_empty()),
        }
    }
}

fn relative(p: [f32; 3], origin: DVec3) -> [f32; 3] {
    (DVec3::from(p.map(f64::from)) - origin)
        .as_vec3()
        .to_array()
}

/// A model plus its CPU-side render data (split-normal mesh chunks or toolpath segments).
///
/// Building one is CPU-heavy for large meshes, so do it on a worker thread and hand
/// the result to [`crate::Viewport3D::set_model`] on the UI thread (which only uploads).
pub struct PreparedModel {
    pub model: Arc<ModelData>,
    pub(crate) geometry: Geometry,
}

impl PreparedModel {
    pub fn new(model: Arc<ModelData>) -> Self {
        let geometry = Geometry::build(&model);
        Self { model, geometry }
    }

    /// Vertices after crease splitting (what the GPU draws); toolpath vertex count otherwise.
    pub fn render_vertex_count(&self) -> usize {
        match &self.geometry {
            Geometry::Mesh { chunks, .. } => chunks.iter().map(|c| c.vertices.len()).sum(),
            Geometry::Toolpath { chunks, .. } => chunks.iter().map(|c| c.points.len()).sum(),
        }
    }
}

fn mesh_chunks(mesh: RenderMesh) -> Vec<MeshChunk> {
    mesh_chunks_limited(mesh, MAX_CHUNK_VERTICES, MAX_CHUNK_TRIANGLES)
}

fn mesh_chunks_limited(
    mesh: RenderMesh,
    max_vertices: usize,
    max_triangles: usize,
) -> Vec<MeshChunk> {
    let RenderMesh {
        positions,
        normals,
        indices,
    } = mesh;
    let vertex = |i: usize| Vertex {
        position: positions[i],
        normal: normals.get(i).copied().unwrap_or_default(),
    };
    let n = positions.len();
    // Drop triangles with out-of-range indices instead of letting the GPU read garbage.
    let valid = |t: &[u32]| t.iter().all(|&i| (i as usize) < n);
    if n <= max_vertices && indices.len() / 3 <= max_triangles {
        let vertices = (0..n).map(vertex).collect();
        let indices = if indices.chunks_exact(3).all(valid) && indices.len() % 3 == 0 {
            indices
        } else {
            indices
                .chunks_exact(3)
                .filter(|t| valid(t))
                .flatten()
                .copied()
                .collect()
        };
        return vec![MeshChunk { vertices, indices }];
    }
    // Split by triangle runs, re-indexing the vertices each run references.
    let mut chunks = Vec::new();
    let mut local = vec![u32::MAX; n];
    let mut current = MeshChunk::default();
    let mut used: Vec<usize> = Vec::new();
    for t in indices.chunks_exact(3).filter(|t| valid(t)) {
        if current.vertices.len() + 3 > max_vertices || current.indices.len() / 3 >= max_triangles {
            for &g in &used {
                local[g] = u32::MAX;
            }
            used.clear();
            chunks.push(std::mem::take(&mut current));
        }
        for &g in t {
            let g = g as usize;
            if local[g] == u32::MAX {
                local[g] = current.vertices.len() as u32;
                current.vertices.push(vertex(g));
                used.push(g);
            }
            current.indices.push(local[g]);
        }
    }
    if !current.indices.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn toolpath_chunks(model: &ModelData, origin: DVec3) -> Vec<ToolpathChunk> {
    let v: Vec<[f32; 3]> = model
        .vertices
        .iter()
        .map(|&p| relative(p, origin))
        .collect();
    let segments: Vec<[[f32; 3]; 2]> = model
        .lines
        .iter()
        .filter_map(|&[a, b]| Some([*v.get(a as usize)?, *v.get(b as usize)?]))
        .collect();
    let seg_chunks = segments.chunks(MAX_CHUNK_SEGMENTS).map(<[_]>::to_vec);
    let point_chunks = v.chunks(MAX_CHUNK_SEGMENTS * 2).map(<[_]>::to_vec);
    let mut out: Vec<ToolpathChunk> = seg_chunks
        .map(|segments| ToolpathChunk {
            segments,
            points: Vec::new(),
        })
        .collect();
    for (i, points) in point_chunks.enumerate() {
        if i >= out.len() {
            out.push(ToolpathChunk::default());
        }
        out[i].points = points;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolpath_skips_bad_indices() {
        let m = ModelData::new(
            vec![[0.0; 3], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0]],
            vec![],
            vec![[0, 1], [1, 2], [2, 7]],
            "G-Code Path",
            "p.gcode",
        );
        let Geometry::Toolpath { chunks, origin } = Geometry::build(&m) else {
            panic!("expected toolpath")
        };
        assert_eq!(origin, DVec3::new(0.5, 0.5, 0.0));
        assert_eq!(chunks[0].segments[0], [[-0.5, -0.5, 0.0], [0.5, -0.5, 0.0]]);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].segments.len(), 2);
        assert_eq!(chunks[0].points.len(), 3);
    }

    #[test]
    fn mesh_chunks_reindex_large_meshes() {
        // A triangle strip longer than the (shrunk) chunk limit forces a split.
        let (max_v, max_t) = (100, 1000);
        let n = 250;
        let positions: Vec<[f32; 3]> = (0..n).map(|i| [i as f32, (i % 2) as f32, 0.0]).collect();
        let normals = vec![[0.0, 0.0, 1.0]; n];
        let indices: Vec<u32> = (0..n as u32 - 2).flat_map(|i| [i, i + 1, i + 2]).collect();
        let tri_count = indices.len() / 3;
        let chunks = mesh_chunks_limited(
            RenderMesh {
                positions: positions.clone(),
                normals,
                indices,
            },
            max_v,
            max_t,
        );
        assert!(chunks.len() >= 3);
        assert_eq!(
            chunks.iter().map(|c| c.indices.len() / 3).sum::<usize>(),
            tri_count
        );
        for c in &chunks {
            assert!(c.vertices.len() <= max_v);
            assert!(c.indices.iter().all(|&i| (i as usize) < c.vertices.len()));
        }
        // First triangle of the second chunk still refers to the right positions.
        let first = chunks[0].indices.len() / 3;
        let c = &chunks[1];
        let p = c.vertices[c.indices[0] as usize].position;
        assert_eq!(p, positions[first]);
    }

    #[test]
    fn mesh_drops_out_of_range_triangles() {
        let chunks = mesh_chunks(RenderMesh {
            positions: vec![[0.0; 3]; 3],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            indices: vec![0, 1, 2, 0, 1, 9],
        });
        assert_eq!(chunks[0].indices, vec![0, 1, 2]);
    }
}
