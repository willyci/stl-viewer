/// Standardized geometry container produced by every parser.
///
/// Meshes fill `triangles`; G-code toolpaths fill `lines` (two vertices per segment,
/// never deduplicated) and leave `triangles` empty.
#[derive(Clone, Debug, Default)]
pub struct ModelData {
    pub vertices: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
    pub lines: Vec<[u32; 2]>,
    /// Human-readable format label, e.g. "STL (Binary)", "3MF Model", "Wavefront OBJ", "G-Code Path".
    pub format: String,
    /// File basename.
    pub filename: String,
    /// `[[xmin, xmax], [ymin, ymax], [zmin, zmax]]`; all zero for an empty model.
    pub bounds: [[f64; 2]; 3],
    /// Absolute value of the signed (divergence theorem) volume; 0 for toolpaths.
    pub volume: f64,
    /// Wall-clock parse time, filled in by `load_model`.
    pub load_time_ms: f64,
}

impl ModelData {
    pub fn new(
        vertices: Vec<[f32; 3]>,
        triangles: Vec<[u32; 3]>,
        lines: Vec<[u32; 2]>,
        format: impl Into<String>,
        filename: impl Into<String>,
    ) -> Self {
        let bounds = compute_bounds(&vertices);
        let volume = compute_volume(&vertices, &triangles);
        Self {
            vertices,
            triangles,
            lines,
            format: format.into(),
            filename: filename.into(),
            bounds,
            volume,
            load_time_ms: 0.0,
        }
    }

    pub fn num_vertices(&self) -> usize {
        self.vertices.len()
    }

    pub fn num_triangles(&self) -> usize {
        self.triangles.len()
    }

    pub fn num_lines(&self) -> usize {
        self.lines.len()
    }

    /// True for G-code toolpaths (line segments instead of triangles).
    pub fn is_toolpath(&self) -> bool {
        !self.lines.is_empty()
    }

    /// Extent along X, Y and Z.
    pub fn bounding_box_size(&self) -> [f64; 3] {
        self.bounds.map(|[lo, hi]| hi - lo)
    }

    pub fn center(&self) -> [f64; 3] {
        self.bounds.map(|[lo, hi]| (lo + hi) / 2.0)
    }
}

fn compute_bounds(vertices: &[[f32; 3]]) -> [[f64; 2]; 3] {
    if vertices.is_empty() {
        return [[0.0; 2]; 3];
    }
    let mut bounds = [[f64::INFINITY, f64::NEG_INFINITY]; 3];
    for v in vertices {
        for (axis, b) in bounds.iter_mut().enumerate() {
            let c = v[axis] as f64;
            b[0] = b[0].min(c);
            b[1] = b[1].max(c);
        }
    }
    bounds
}

/// Sum of signed tetrahedron volumes `A · (B × C) / 6` over all triangles.
fn compute_volume(vertices: &[[f32; 3]], triangles: &[[u32; 3]]) -> f64 {
    let vertex = |i: u32| vertices.get(i as usize).map(|v| v.map(f64::from));
    let mut sum = 0.0;
    for t in triangles {
        let (Some(a), Some(b), Some(c)) = (vertex(t[0]), vertex(t[1]), vertex(t[2])) else {
            continue;
        };
        let cross = [
            b[1] * c[2] - b[2] * c[1],
            b[2] * c[0] - b[0] * c[2],
            b[0] * c[1] - b[1] * c[0],
        ];
        sum += a[0] * cross[0] + a[1] * cross[1] + a[2] * cross[2];
    }
    (sum / 6.0).abs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[rustfmt::skip]
    fn unit_cube_volume_and_bounds() {
        let v = vec![
            [0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0], [1.0, 0.0, 1.0], [1.0, 1.0, 1.0], [0.0, 1.0, 1.0],
        ];
        let t = vec![
            [0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7], [0, 1, 5], [0, 5, 4],
            [2, 3, 7], [2, 7, 6], [0, 4, 7], [0, 7, 3], [1, 2, 6], [1, 6, 5],
        ];
        let m = ModelData::new(v, t, vec![], "test", "cube");
        assert!((m.volume - 1.0).abs() < 1e-9);
        assert_eq!(m.bounding_box_size(), [1.0, 1.0, 1.0]);
        assert_eq!(m.center(), [0.5, 0.5, 0.5]);
    }

    #[test]
    fn empty_model() {
        let m = ModelData::new(vec![], vec![], vec![], "test", "empty");
        assert_eq!(m.bounds, [[0.0; 2]; 3]);
        assert_eq!(m.volume, 0.0);
    }
}
