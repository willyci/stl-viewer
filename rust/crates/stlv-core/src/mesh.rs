//! Mesh processing shared by the parsers and the renderer.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::hash::{BuildHasher, BuildHasherDefault, Hash, Hasher, RandomState};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering::Relaxed};

use crate::model::ModelData;
use crate::par;

/// VTK `vtkPolyDataNormals` default feature angle; edges sharper than this stay hard.
pub const DEFAULT_FEATURE_ANGLE_DEG: f32 = 30.0;

/// Merge bit-identical vertices (like `np.unique(axis=0, return_inverse=True)`).
/// Returns the unique vertices and, for every input vertex, its index into them.
///
/// Coordinates compare by value (`-0.0 == 0.0`) and a vertex with a NaN coordinate is never
/// merged. Unique vertices keep first-occurrence order (numpy sorts them; the counts match).
pub fn dedup_vertices(flat: &[[f32; 3]]) -> (Vec<[f32; 3]>, Vec<u32>) {
    dedup_by(flat.len(), flat.len() / 4, |i| flat[i])
}

/// [`dedup_vertices`] over the `n` vertices `vertex(0..n)`, expecting about `unique_hint`
/// distinct ones. Large inputs use several threads; the result is the same either way.
pub fn dedup_by<F>(n: usize, unique_hint: usize, vertex: F) -> (Vec<[f32; 3]>, Vec<u32>)
where
    F: Fn(usize) -> [f32; 3] + Sync,
{
    let threads = par::threads();
    if n < PARALLEL_DEDUP_MIN || threads < 2 || u32::try_from(n).is_err() {
        let mut dedup = VertexDedup::with_capacity(unique_hint.min(n));
        let inverse = (0..n).map(|i| dedup.insert(vertex(i))).collect();
        return (dedup.into_vertices(), inverse);
    }
    dedup_parallel(n, unique_hint, &vertex, threads)
}

/// Below this many vertices threads cost more than they save.
const PARALLEL_DEDUP_MIN: usize = 1 << 17;
/// Unique vertices per hash shard in the parallel dedup: small enough for the shard's map
/// to stay in a core's L2 cache, which is what makes the parallel path fast.
const SHARD_UNIQUE_TARGET: usize = 8192;
const MAX_SHARDS: usize = 4096;

/// First-occurrence dedup in four parallel passes: partition the vertex indices by hash
/// shard (ascending within each shard), map every vertex to the first one with the same key
/// shard by shard, number the first occurrences in index order, then look up every vertex's
/// number. Requires `n <= u32::MAX`.
fn dedup_parallel<F>(
    n: usize,
    unique_hint: usize,
    vertex: &F,
    threads: usize,
) -> (Vec<[f32; 3]>, Vec<u32>)
where
    F: Fn(usize) -> [f32; 3] + Sync,
{
    let shards = (unique_hint / SHARD_UNIQUE_TARGET)
        .max(4 * threads)
        .next_power_of_two()
        .min(MAX_SHARDS);
    // One extra list for vertices with a NaN coordinate, which are never merged.
    let nan_list = shards;
    let lists = shards + 1;
    // Bits 32.. of the hash: hashbrown uses the low bits for buckets and the top 7 as tags.
    let list_of = |i: usize| match vertex_key(vertex(i)) {
        Some(key) => (key.mix() >> 32) as usize & (shards - 1),
        None => nan_list,
    };
    let ranges = par::ranges(n, threads);
    let chunk = par::chunk_len(n, threads);

    // 1. Partition: count per chunk and list, then scatter list-major, chunk-minor.
    let counts = par::map(&ranges, |r| {
        let mut count = vec![0usize; lists];
        for i in r.clone() {
            count[list_of(i)] += 1;
        }
        count
    });
    let mut order = vec![0u32; n];
    let mut segments = Vec::with_capacity(lists);
    let mut outs: Vec<Vec<&mut [u32]>> = ranges.iter().map(|_| Vec::new()).collect();
    let mut rest = order.as_mut_slice();
    for list in 0..lists {
        let start = n - rest.len();
        for (out, count) in outs.iter_mut().zip(&counts) {
            let (head, tail) = std::mem::take(&mut rest).split_at_mut(count[list]);
            out.push(head);
            rest = tail;
        }
        segments.push(start..n - rest.len());
    }
    par::map(ranges.iter().zip(outs), |(r, mut out)| {
        let mut fill = vec![0usize; lists];
        for i in r.clone() {
            let list = list_of(i);
            out[list][fill[list]] = i as u32;
            fill[list] += 1;
        }
    });

    // 2. Per shard: every vertex's first occurrence (shards are handed out dynamically).
    let first: Vec<AtomicU32> = (0..n).map(|_| AtomicU32::new(0)).collect();
    let next_list = AtomicUsize::new(0);
    par::map(0..threads, |_| {
        loop {
            let list = next_list.fetch_add(1, Relaxed);
            let Some(segment) = segments.get(list) else {
                break;
            };
            let indices = &order[segment.clone()];
            let hint = (unique_hint as f64 * indices.len() as f64 / n as f64) as usize + 16;
            let mut map = HashMap::with_capacity_and_hasher(
                if list == nan_list { 0 } else { hint },
                BuildHasherDefault::<KeyHasher>::default(),
            );
            for &i in indices {
                let f = match vertex_key(vertex(i as usize)) {
                    Some(key) if list != nan_list => *map.entry(key).or_insert(i),
                    _ => i,
                };
                first[i as usize].store(f, Relaxed);
            }
        }
    });

    // 3. Number the first occurrences in index order and copy out the unique vertices.
    let is_first = |i: usize| first[i].load(Relaxed) as usize == i;
    let unique_counts = par::map(&ranges, |r| r.clone().filter(|&i| is_first(i)).count());
    let mut vertices = vec![[0.0f32; 3]; unique_counts.iter().sum()];
    let mut ids = order; // reused: `ids[i]` is the output index of first occurrence `i`
    let mut jobs = Vec::with_capacity(ranges.len());
    let (mut rest, mut base) = (vertices.as_mut_slice(), 0);
    for ((r, ids), &count) in ranges.iter().zip(ids.chunks_mut(chunk)).zip(&unique_counts) {
        let (head, tail) = std::mem::take(&mut rest).split_at_mut(count);
        jobs.push((r.clone(), ids, head, base));
        rest = tail;
        base += count;
    }
    par::map(jobs, |(r, ids, out, base)| {
        let mut k = 0;
        for i in r.clone() {
            if is_first(i) {
                ids[i - r.start] = (base + k) as u32;
                out[k] = vertex(i);
                k += 1;
            }
        }
    });

    // 4. Every vertex takes its first occurrence's number.
    let mut inverse = vec![0u32; n];
    par::map(ranges.iter().zip(inverse.chunks_mut(chunk)), |(r, out)| {
        for (o, i) in out.iter_mut().zip(r.clone()) {
            *o = ids[first[i].load(Relaxed) as usize];
        }
    });
    (vertices, inverse)
}

/// Incremental form of [`dedup_vertices`], used by the parsers to merge vertices while
/// reading instead of materialising the flat (per-triangle-corner) vertex list first.
#[derive(Debug, Default)]
pub struct VertexDedup {
    map: HashMap<VertexKey, u32, BuildHasherDefault<KeyHasher>>,
    vertices: Vec<[f32; 3]>,
}

impl VertexDedup {
    /// `unique_hint`: expected number of unique vertices.
    pub fn with_capacity(unique_hint: usize) -> Self {
        Self {
            map: HashMap::with_capacity_and_hasher(unique_hint, Default::default()),
            vertices: Vec::with_capacity(unique_hint),
        }
    }

    /// Index of `v` among the unique vertices, adding it if it is new.
    pub fn insert(&mut self, v: [f32; 3]) -> u32 {
        let next = self.vertices.len() as u32;
        let Some(key) = vertex_key(v) else {
            self.vertices.push(v);
            return next;
        };
        match self.map.entry(key) {
            Entry::Occupied(e) => *e.get(),
            Entry::Vacant(e) => {
                e.insert(next);
                self.vertices.push(v);
                next
            }
        }
    }

    pub fn len(&self) -> usize {
        self.vertices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vertices.is_empty()
    }

    pub fn into_vertices(self) -> Vec<[f32; 3]> {
        self.vertices
    }
}

/// Coordinate bits with `-0.0` folded into `+0.0`; `None` for a vertex that has a NaN.
fn vertex_key(v: [f32; 3]) -> Option<VertexKey> {
    if v.iter().any(|c| c.is_nan()) {
        return None;
    }
    Some(VertexKey(v.map(|c| (c + 0.0).to_bits())))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VertexKey([u32; 3]);

impl VertexKey {
    /// Keyed folded-multiply hash. The per-process random key keeps crafted inputs from
    /// colliding on purpose (which would make dedup quadratic); hashbrown needs good high bits.
    fn mix(&self) -> u64 {
        let [k0, k1, k2] = *hash_key();
        let [x, y, z] = self.0.map(u64::from);
        let h = folded_multiply((x | (y << 32)) ^ k0, z ^ k1);
        folded_multiply(h ^ k2, 0x9E37_79B9_7F4A_7C15)
    }
}

fn folded_multiply(a: u64, b: u64) -> u64 {
    let p = u128::from(a) * u128::from(b);
    (p as u64) ^ ((p >> 64) as u64)
}

/// Random per-process key for [`VertexKey::mix`], drawn from std's `RandomState`.
fn hash_key() -> &'static [u64; 3] {
    static KEY: OnceLock<[u64; 3]> = OnceLock::new();
    KEY.get_or_init(|| {
        let state = RandomState::new();
        [1u64, 2, 3].map(|i| state.hash_one(i))
    })
}

impl Hash for VertexKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.mix());
    }
}

/// Pass-through hasher for the pre-mixed [`VertexKey`] hash.
#[derive(Default)]
struct KeyHasher(u64);

impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(8) ^ u64::from(b)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
    }

    fn write_u64(&mut self, h: u64) {
        self.0 = h;
    }
}

/// GPU-ready triangle mesh with per-vertex normals.
#[derive(Clone, Debug, Default)]
pub struct RenderMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

impl RenderMesh {
    pub fn num_triangles(&self) -> usize {
        self.indices.len() / 3
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

/// Equivalent of `vtkPolyDataNormals` with VTK defaults (point normals on, consistency on,
/// splitting on at `feature_angle_deg`): vertices shared across a sharp edge are duplicated
/// so smooth shading keeps hard creases.
///
/// Mirrors VTK step by step: triangle windings are made consistent by a breadth-first wave
/// over edge neighbours (non-manifold edges included, VTK's default), unit face normals are
/// computed from the reoriented triangles, every vertex fan is split into regions separated by
/// sharp, boundary or non-manifold edges, and point normals are the normalised sum of the
/// adjacent unit face normals. Degenerate triangles get a zero face normal (they split their
/// vertices off, as in VTK); a vertex whose face normals sum to zero keeps a zero normal, so
/// shaders should guard their `normalize`. Triangles with out-of-range indices are ignored and
/// vertices not used by any triangle are dropped; the remaining vertices keep their relative
/// order, followed by the split-off duplicates.
pub fn compute_render_mesh(model: &ModelData, feature_angle_deg: f32) -> RenderMesh {
    let points = &model.vertices;
    let n = points.len();
    let old: Vec<[u32; 3]> = model
        .triangles
        .iter()
        .copied()
        .filter(|t| t.iter().all(|&i| (i as usize) < n))
        .collect();
    // Triangle ids must stay below the edge-table sentinels (only fails past 4G triangles).
    if old.is_empty() || old.len() >= NON_MANIFOLD as usize {
        return RenderMesh::default();
    }

    let topo = Topology::new(n, &old);
    let mut tris = old.clone();
    make_consistent(&topo, &mut tris);

    let face_normals: Vec<[f32; 3]> = tris.iter().map(|t| triangle_normal(points, t)).collect();

    let cos_angle = f64::from(feature_angle_deg).to_radians().cos();
    let map = split_sharp_edges(&topo, &mut tris, &face_normals, cos_angle);

    let mut normals = vec![[0.0f32; 3]; map.len()];
    for (t, fnorm) in tris.iter().zip(&face_normals) {
        for &p in t {
            let acc = &mut normals[p as usize];
            for k in 0..3 {
                acc[k] += fnorm[k];
            }
        }
    }

    // Drop unreferenced points; the rest keep their order.
    let mut new_index = vec![u32::MAX; map.len()];
    for t in &tris {
        for &p in t {
            new_index[p as usize] = 0;
        }
    }
    let mut positions = Vec::with_capacity(map.len());
    let mut out_normals = Vec::with_capacity(map.len());
    for (i, slot) in new_index.iter_mut().enumerate() {
        if *slot == u32::MAX {
            continue;
        }
        *slot = positions.len() as u32;
        positions.push(points[map[i] as usize]);
        out_normals.push(normalized(normals[i]));
    }
    let indices = tris
        .iter()
        .flatten()
        .map(|&p| new_index[p as usize])
        .collect();

    RenderMesh {
        positions,
        normals: out_normals,
        indices,
    }
}

/// For every point, the triangles using it in ascending order — once per occurrence, so a
/// degenerate triangle with a repeated point is listed twice (VTK `vtkCellLinks`).
struct PointCells {
    offsets: Vec<u32>,
    cells: Vec<u32>,
}

impl PointCells {
    fn build(num_points: usize, tris: &[[u32; 3]]) -> Self {
        let mut offsets = vec![0u32; num_points + 1];
        for t in tris {
            for &p in t {
                offsets[p as usize + 1] += 1;
            }
        }
        for i in 0..num_points {
            offsets[i + 1] += offsets[i];
        }
        let mut fill = offsets.clone();
        let mut cells = vec![0u32; tris.len() * 3];
        for (c, t) in tris.iter().enumerate() {
            for &p in t {
                let slot = &mut fill[p as usize];
                cells[*slot as usize] = c as u32;
                *slot += 1;
            }
        }
        Self { offsets, cells }
    }

    fn of(&self, p: u32) -> &[u32] {
        let p = p as usize;
        &self.cells[self.offsets[p] as usize..self.offsets[p + 1] as usize]
    }

    /// `vtkPolyData::GetCellEdgeNeighbors`: the triangles other than `cell` that use both `p1`
    /// and `p2`, in ascending order, repeated as often as they occur in `p1`'s list.
    fn edge_neighbors(&self, cell: u32, p1: u32, p2: u32, out: &mut Vec<u32>) {
        out.clear();
        let (a, b) = (self.of(p1), self.of(p2));
        if a.len() * b.len() <= 64 {
            out.extend(a.iter().filter(|&&c| c != cell && b.contains(&c)));
        } else if a.len() <= b.len() {
            out.extend(
                a.iter()
                    .filter(|&&c| c != cell && b.binary_search(&c).is_ok()),
            );
        } else {
            // `p1` is a high-valence point: walk the shorter list and count `p1` occurrences.
            let mut i = 0;
            while i < b.len() {
                let c = b[i];
                while i < b.len() && b[i] == c {
                    i += 1;
                }
                if c == cell {
                    continue;
                }
                let lo = a.partition_point(|&x| x < c);
                let count = a[lo..].iter().take_while(|&&x| x == c).count();
                out.extend(std::iter::repeat_n(c, count));
            }
        }
    }
}

/// Edge-table entry: no other triangle uses the edge.
const BOUNDARY: u32 = u32::MAX;
/// Edge-table entry: the edge touches a degenerate triangle, so only the general
/// [`PointCells::edge_neighbors`] query knows its neighbours.
const COMPLEX: u32 = u32::MAX - 1;
/// Edge-table entry: three or more triangles (none degenerate) share the edge.
const NON_MANIFOLD: u32 = u32::MAX - 2;

/// Connectivity of the input triangles (VTK's read-only `OldMesh`).
struct Topology<'a> {
    tris: &'a [[u32; 3]],
    links: PointCells,
    /// Per triangle and edge `(t[j], t[(j + 1) % 3])`: the one triangle across a clean
    /// manifold edge, [`BOUNDARY`], [`NON_MANIFOLD`] or [`COMPLEX`].
    across: Vec<[u32; 3]>,
}

impl<'a> Topology<'a> {
    fn new(num_points: usize, tris: &'a [[u32; 3]]) -> Self {
        let links = PointCells::build(num_points, tris);
        let degenerate = |c: u32| {
            let t = tris[c as usize];
            t[0] == t[1] || t[1] == t[2] || t[2] == t[0]
        };
        let mut across = vec![[COMPLEX; 3]; tris.len()];
        // Every edge is collected at its lower endpoint: (other end, triangle, edge slot).
        let mut edges: Vec<(u32, u32, u8)> = Vec::new();
        for p in 0..num_points as u32 {
            edges.clear();
            let mut prev = None;
            for &c in links.of(p) {
                if prev == Some(c) {
                    continue; // a degenerate triangle listed twice
                }
                prev = Some(c);
                let t = tris[c as usize];
                for j in 0..3u8 {
                    let (a, b) = (t[j as usize], t[(j as usize + 1) % 3]);
                    let other = if a == p {
                        b
                    } else if b == p {
                        a
                    } else {
                        continue;
                    };
                    if other > p {
                        edges.push((other, c, j));
                    }
                }
            }
            edges.sort_unstable();
            for run in edges.chunk_by(|x, y| x.0 == y.0) {
                match *run {
                    [(_, c, j)] if !degenerate(c) => across[c as usize][j as usize] = BOUNDARY,
                    [(_, c1, j1), (_, c2, j2)] if !degenerate(c1) && !degenerate(c2) => {
                        across[c1 as usize][j1 as usize] = c2;
                        across[c2 as usize][j2 as usize] = c1;
                    }
                    [_, _, _, ..] if run.iter().all(|&(_, c, _)| !degenerate(c)) => {
                        for &(_, c, j) in run {
                            across[c as usize][j as usize] = NON_MANIFOLD;
                        }
                    }
                    _ => {}
                }
            }
        }
        Self {
            tris,
            links,
            across,
        }
    }

    /// Edge-table entry of the edge `{p1, p2}` of `cell`.
    fn entry(&self, cell: u32, p1: u32, p2: u32) -> u32 {
        let t = self.tris[cell as usize];
        (0..3)
            .find(|&j| {
                let (a, b) = (t[j], t[(j + 1) % 3]);
                (a == p1 && b == p2) || (a == p2 && b == p1)
            })
            .map_or(COMPLEX, |j| self.across[cell as usize][j])
    }

    /// `vtkPolyData::GetCellEdgeNeighbors(cell, p1, p2)` (see [`PointCells::edge_neighbors`]),
    /// answered from the edge table where possible.
    fn edge_neighbors<'b>(&self, cell: u32, p1: u32, p2: u32, out: &'b mut Vec<u32>) -> &'b [u32] {
        out.clear();
        match self.entry(cell, p1, p2) {
            BOUNDARY => {}
            COMPLEX | NON_MANIFOLD => self.links.edge_neighbors(cell, p1, p2, out),
            nb => out.push(nb),
        }
        out
    }

    /// The edge neighbour of `cell` across `{p1, p2}` if there is exactly one.
    fn single_neighbor(&self, cell: u32, p1: u32, p2: u32, buf: &mut Vec<u32>) -> Option<u32> {
        match self.entry(cell, p1, p2) {
            BOUNDARY | NON_MANIFOLD => None,
            COMPLEX => match *self.edge_neighbors(cell, p1, p2, buf) {
                [nb] => Some(nb),
                _ => None,
            },
            nb => Some(nb),
        }
    }
}

/// VTK `TraverseAndOrder`: flip triangles so neighbours traverse shared edges in opposite
/// directions. Topology comes from the original triangles, windings from `tris`.
fn make_consistent(topo: &Topology, tris: &mut [[u32; 3]]) {
    let mut visited = vec![false; tris.len()];
    let mut wave = Vec::new();
    let mut next_wave = Vec::new();
    let mut neighbors = Vec::new();
    for seed in 0..tris.len() {
        if visited[seed] {
            continue;
        }
        visited[seed] = true;
        wave.clear();
        wave.push(seed as u32);
        while !wave.is_empty() {
            for &cell in &wave {
                let pts = tris[cell as usize];
                for j in 0..3 {
                    let (p1, p2) = (pts[j], pts[(j + 1) % 3]);
                    for &nb in topo.edge_neighbors(cell, p1, p2, &mut neighbors) {
                        let nb = nb as usize;
                        if visited[nb] {
                            continue;
                        }
                        let nt = &mut tris[nb];
                        let l = nt.iter().position(|&p| p == p2).unwrap_or(0);
                        if nt[(l + 1) % 3] != p1 {
                            nt.reverse();
                        }
                        visited[nb] = true;
                        next_wave.push(nb as u32);
                    }
                }
            }
            std::mem::swap(&mut wave, &mut next_wave);
            next_wave.clear();
        }
    }
}

/// VTK `MarkAndSplit` for every point: the triangles around a point are grouped into regions
/// reachable across smooth manifold edges; every region but the first gets its own copy of the
/// point. Returns the map from output point to input point (identity prefix + duplicates).
fn split_sharp_edges(
    topo: &Topology,
    tris: &mut [[u32; 3]],
    face_normals: &[[f32; 3]],
    cos_angle: f64,
) -> Vec<u32> {
    let old = topo.tris;
    let num_points = topo.links.offsets.len() - 1;
    let mut map: Vec<u32> = (0..num_points as u32).collect();
    let mut region = vec![-1i32; old.len()];
    let mut neighbors = Vec::new();
    let smooth = |a: usize, b: usize| f64::from(dot(face_normals[a], face_normals[b])) > cos_angle;

    for pt in 0..num_points as u32 {
        let cells = topo.links.of(pt);
        if cells.len() <= 1 {
            continue;
        }
        for &c in cells {
            region[c as usize] = -1;
        }
        let mut num_regions = 0i32;
        for &seed in cells {
            if region[seed as usize] >= 0 {
                continue;
            }
            region[seed as usize] = num_regions;
            for start_nei in fan_edges(old[seed as usize], pt) {
                let mut cell = seed;
                let mut nei = start_nei;
                while let Some(next) = topo.single_neighbor(cell, pt, nei, &mut neighbors) {
                    if region[next as usize] >= 0 || !smooth(cell as usize, next as usize) {
                        break;
                    }
                    region[next as usize] = num_regions;
                    cell = next;
                    let [a, b] = fan_edges(old[cell as usize], pt);
                    nei = if a != nei { a } else { b };
                }
            }
            num_regions += 1;
        }
        if num_regions <= 1 {
            continue;
        }
        let last_id = map.len();
        map.resize(last_id + num_regions as usize - 1, pt);
        for &c in cells {
            let r = region[c as usize];
            if r > 0 {
                let replacement = (last_id + r as usize - 1) as u32;
                if let Some(slot) = tris[c as usize].iter_mut().find(|p| **p == pt) {
                    *slot = replacement;
                }
            }
        }
    }
    map
}

/// The other endpoints of the two edges of triangle `t` that meet at `pt`, in VTK's order.
fn fan_edges(t: [u32; 3], pt: u32) -> [u32; 2] {
    match t.iter().position(|&p| p == pt) {
        Some(0) => [t[1], t[2]],
        Some(1) => [t[2], t[0]],
        _ => [t[1], t[0]],
    }
}

/// Unit normal of a triangle (VTK `vtkTriangle::ComputeNormal`, in f64), zero if degenerate.
fn triangle_normal(points: &[[f32; 3]], t: &[u32; 3]) -> [f32; 3] {
    let [v1, v2, v3] = t.map(|i| points[i as usize].map(f64::from));
    let a = [v3[0] - v2[0], v3[1] - v2[1], v3[2] - v2[2]];
    let b = [v1[0] - v2[0], v1[1] - v2[1], v1[2] - v2[2]];
    let n = [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len == 0.0 || !len.is_finite() {
        return [0.0; 3];
    }
    n.map(|c| (c / len) as f32)
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalized(v: [f32; 3]) -> [f32; 3] {
    let len = dot(v, v).sqrt();
    if len == 0.0 || !len.is_finite() {
        [0.0; 3]
    } else {
        v.map(|c| c / len)
    }
}

/// Unique undirected edges of an indexed triangle list (for wireframe rendering).
///
/// Each edge is returned once as `[lo, hi]` with `lo < hi`, sorted; zero-length edges of
/// degenerate triangles are skipped and a trailing partial triangle is ignored.
pub fn unique_edges(indices: &[u32]) -> Vec<[u32; 2]> {
    let tris = indices.chunks_exact(3);
    let edge_ends = |t: &[u32]| [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])];
    let num_points = indices.iter().copied().max().map_or(0, |m| m as usize + 1);

    // Bucket every edge's high end by its low end, then sort and dedup each small bucket.
    let mut offsets = vec![0u32; num_points + 1];
    for t in tris.clone() {
        for (a, b) in edge_ends(t) {
            if a != b {
                offsets[a.min(b) as usize + 1] += 1;
            }
        }
    }
    for i in 0..num_points {
        offsets[i + 1] += offsets[i];
    }
    let mut fill = offsets.clone();
    let mut highs = vec![0u32; offsets[num_points] as usize];
    for t in tris {
        for (a, b) in edge_ends(t) {
            if a != b {
                let slot = &mut fill[a.min(b) as usize];
                highs[*slot as usize] = a.max(b);
                *slot += 1;
            }
        }
    }

    let mut edges = Vec::with_capacity(highs.len() / 2 + 1);
    for lo in 0..num_points {
        let bucket = &mut highs[offsets[lo] as usize..offsets[lo + 1] as usize];
        bucket.sort_unstable();
        let mut prev = None;
        for &hi in bucket.iter() {
            if prev != Some(hi) {
                edges.push([lo as u32, hi]);
                prev = Some(hi);
            }
        }
    }
    edges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rustfmt::skip]
    fn cube() -> ModelData {
        let v = vec![
            [0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0], [1.0, 0.0, 1.0], [1.0, 1.0, 1.0], [0.0, 1.0, 1.0],
        ];
        let t = vec![
            [0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7], [0, 1, 5], [0, 5, 4],
            [2, 3, 7], [2, 7, 6], [0, 4, 7], [0, 7, 3], [1, 2, 6], [1, 6, 5],
        ];
        ModelData::new(v, t, vec![], "test", "cube")
    }

    #[test]
    fn dedup_merges_signed_zero_but_not_nan() {
        let flat = [
            [0.0, 1.0, 2.0],
            [-0.0, 1.0, 2.0],
            [f32::NAN, 0.0, 0.0],
            [f32::NAN, 0.0, 0.0],
            [0.0, 1.0, 2.0],
            [3.0, 1.0, 2.0],
        ];
        let (unique, inverse) = dedup_vertices(&flat);
        assert_eq!(unique.len(), 4);
        assert_eq!(inverse, vec![0, 0, 1, 2, 0, 3]);
        for (i, &u) in inverse.iter().enumerate() {
            let (a, b) = (flat[i], unique[u as usize]);
            assert!(
                a.iter()
                    .zip(&b)
                    .all(|(x, y)| x == y || (x.is_nan() && y.is_nan()))
            );
        }
    }

    #[test]
    fn parallel_dedup_matches_sequential() {
        // Pseudo-random vertices from a small pool, so most of them repeat.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let n = 200_000;
        let flat: Vec<[f32; 3]> = (0..n)
            .map(|_| match next() % 1000 {
                0 => [f32::NAN, 1.0, 2.0],
                1 => [-0.0, 0.0, -0.0],
                2 => [0.0, -0.0, 0.0],
                r => [(r % 37) as f32, (next() % 41) as f32 * 0.5, (r % 13) as f32],
            })
            .collect();
        let mut seq = VertexDedup::default();
        let expected: Vec<u32> = flat.iter().map(|&v| seq.insert(v)).collect();
        let expected_vertices = seq.into_vertices();
        for threads in [1, 2, 3, 8] {
            for hint in [0, 1000, n] {
                let (vertices, inverse) = dedup_parallel(n, hint, &|i| flat[i], threads);
                assert_eq!(inverse, expected, "{threads} threads, hint {hint}");
                assert_eq!(vertices.len(), expected_vertices.len());
                assert!(
                    vertices
                        .iter()
                        .zip(&expected_vertices)
                        .all(|(a, b)| a.map(f32::to_bits) == b.map(f32::to_bits))
                );
            }
        }
        assert_eq!(dedup_vertices(&flat).1, expected);
    }

    #[test]
    fn cube_splits_every_corner_into_three() {
        let mesh = compute_render_mesh(&cube(), DEFAULT_FEATURE_ANGLE_DEG);
        assert_eq!(mesh.positions.len(), 24);
        assert_eq!(mesh.indices.len(), 36);
        // Every corner normal is an axis direction pointing out of the cube.
        for (p, n) in mesh.positions.iter().zip(&mesh.normals) {
            let axis = n
                .iter()
                .position(|c| c.abs() > 0.999)
                .expect("axis-aligned normal");
            let outward = if p[axis] > 0.5 { 1.0 } else { -1.0 };
            assert_eq!(n[axis], outward);
        }
    }

    #[test]
    fn smooth_without_splitting_angle() {
        let mesh = compute_render_mesh(&cube(), 180.0);
        assert_eq!(mesh.positions.len(), 8);
        for (p, n) in mesh.positions.iter().zip(&mesh.normals) {
            assert!((dot(*n, *n) - 1.0).abs() < 1e-6);
            let outward = p.map(|c| c - 0.5);
            assert!(
                n.iter().zip(&outward).all(|(a, b)| a * b > 0.0),
                "{p:?} {n:?}"
            );
        }
        // Corner 0 touches two triangles of each of its three faces: unweighted mean.
        let corner = mesh.positions.iter().position(|p| *p == [0.0; 3]).unwrap();
        let expected = -1.0 / 3f32.sqrt();
        assert!(
            mesh.normals[corner]
                .iter()
                .all(|c| (c - expected).abs() < 1e-6)
        );
    }

    #[test]
    fn inconsistent_winding_is_repaired() {
        let mut model = cube();
        model.triangles[3] = [4, 7, 6]; // flipped top triangle
        let mesh = compute_render_mesh(&model, 180.0);
        let fixed = compute_render_mesh(&cube(), 180.0);
        assert_eq!(mesh.normals, fixed.normals);
    }

    #[test]
    fn degenerate_and_invalid_triangles_do_not_produce_nan() {
        let v = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let t = vec![[0, 1, 2], [0, 1, 3], [1, 1, 3], [0, 9, 1]];
        let model = ModelData::new(v, t, vec![], "test", "degenerate");
        let mesh = compute_render_mesh(&model, DEFAULT_FEATURE_ANGLE_DEG);
        assert_eq!(mesh.indices.len(), 9);
        assert!(mesh.normals.iter().flatten().all(|c| c.is_finite()));
        assert!(
            mesh.indices
                .iter()
                .all(|&i| (i as usize) < mesh.positions.len())
        );
    }

    #[test]
    fn empty_and_toolpath_models_give_empty_meshes() {
        let empty = ModelData::default();
        assert!(compute_render_mesh(&empty, 30.0).is_empty());
        let path = ModelData::new(
            vec![[0.0; 3], [1.0; 3]],
            vec![],
            vec![[0, 1]],
            "G-Code Path",
            "p",
        );
        assert!(compute_render_mesh(&path, 30.0).positions.is_empty());
    }

    #[test]
    fn edges_are_unique_and_sorted() {
        let edges = unique_edges(&[0, 1, 2, 2, 1, 3, 3, 3, 1, 9]);
        assert_eq!(edges, vec![[0, 1], [0, 2], [1, 2], [1, 3], [2, 3]]);
        assert!(unique_edges(&[]).is_empty());
        let cube_edges = unique_edges(&cube().triangles.concat());
        assert_eq!(cube_edges.len(), 18);
    }
}
