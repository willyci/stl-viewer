use std::num::IntErrorKind;
use std::path::Path;

use super::{LoadError, file_name, text};
use crate::mesh::dedup_by;
use crate::model::ModelData;

/// Wavefront OBJ: `v` and `f` records, polygon faces fan-triangulated.
pub fn parse_obj(path: &Path) -> Result<ModelData, LoadError> {
    let data = std::fs::read(path)?;
    parse_obj_bytes(&data, file_name(path))
}

/// Face vertex reference after resolving relative indices; `None` when it cannot be valid.
type Ref = Option<usize>;

/// [`parse_obj`] on an in-memory file.
pub fn parse_obj_bytes(data: &[u8], filename: String) -> Result<ModelData, LoadError> {
    let mut vertices: Vec<[f32; 3]> = Vec::new();
    // Faces are validated at the end: positive indices may refer to later vertices.
    let mut face_refs: Vec<Ref> = Vec::new();
    let mut faces: Vec<(usize, usize)> = Vec::new();

    for line in text::lines(data) {
        let mut toks = text::tokens(line);
        match toks.next() {
            Some(b"v") => {
                let (Some(x), Some(y), Some(z)) = (toks.next(), toks.next(), toks.next()) else {
                    continue;
                };
                if let (Some(x), Some(y), Some(z)) =
                    (text::parse_f32(x), text::parse_f32(y), text::parse_f32(z))
                {
                    vertices.push([x, y, z]);
                }
            }
            Some(b"f") => {
                let refs: Vec<&[u8]> = toks.collect();
                if refs.len() < 3 {
                    continue;
                }
                let start = face_refs.len();
                // Unparsable references are skipped (Python), the rest still form the fan.
                face_refs.extend(refs.iter().filter_map(|r| resolve_ref(r, vertices.len())));
                faces.push((start, face_refs.len()));
            }
            _ => {}
        }
    }
    if vertices.is_empty() {
        return Err(LoadError::invalid(
            "No valid geometry found in Wavefront OBJ file.",
        ));
    }

    let (unique, remap) = dedup_by(vertices.len(), vertices.len(), |i| vertices[i]);
    let mut triangles = Vec::new();
    for &(start, end) in &faces {
        let face = &face_refs[start..end];
        // Deviation: a face with an out-of-range reference (incl. `0`) is dropped.
        let Some(ids) = face
            .iter()
            .map(|r| r.and_then(|i| remap.get(i).copied()))
            .collect::<Option<Vec<u32>>>()
        else {
            continue;
        };
        for i in 1..ids.len().saturating_sub(1) {
            triangles.push([ids[0], ids[i], ids[i + 1]]);
        }
    }
    Ok(ModelData::new(
        unique,
        triangles,
        vec![],
        "Wavefront OBJ",
        filename,
    ))
}

/// `i[/vt[/vn]]` → 0-based vertex index. Outer `None`: unparsable (skipped, like Python's
/// `ValueError`); inner `None`: an integer that can never be a valid index.
fn resolve_ref(token: &[u8], count_so_far: usize) -> Option<Ref> {
    let index = token.split(|&b| b == b'/').next().unwrap_or_default();
    let index = std::str::from_utf8(index).ok()?;
    match index.parse::<i64>() {
        Ok(i) if i < 0 => Some(
            (count_so_far as i64)
                .checked_add(i)
                .and_then(|r| usize::try_from(r).ok()),
        ),
        Ok(i) => Some(usize::try_from(i - 1).ok()),
        Err(e)
            if matches!(
                e.kind(),
                IntErrorKind::PosOverflow | IntErrorKind::NegOverflow
            ) =>
        {
            Some(None)
        }
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<ModelData, LoadError> {
        parse_obj_bytes(text.as_bytes(), "t.obj".into())
    }

    #[test]
    fn quads_are_fan_triangulated_and_vertices_deduped() {
        let m = parse(
            "# quad\nv 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nv 0 0 0\nvn 0 0 1\nvt 0 0\n\
             f 1/1/1 2/2/1 3/3/1 4/4/1\nf 5//1 2//1 3//1\n",
        )
        .unwrap();
        assert_eq!(m.format, "Wavefront OBJ");
        assert_eq!((m.num_vertices(), m.num_triangles()), (4, 3));
        assert_eq!(m.triangles[2], m.triangles[0]);
    }

    #[test]
    fn negative_and_forward_references() {
        let m = parse("v 0 0 0\nv 1 0 0\nv 0 1 0\nf -3 -2 -1\nf 1 2 4\nv 0 0 1\n").unwrap();
        assert_eq!(m.num_triangles(), 2);
        assert!(m.volume.abs() < 1e-12);
    }

    #[test]
    fn out_of_range_faces_are_dropped() {
        let m = parse(
            "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 0 1 2\nf 1 2 4\nf -4 1 2\nf 1 2 99999999999999999999\n\
             f 1 2 3 9\nf 1 2 3\n",
        )
        .unwrap();
        assert_eq!(m.num_triangles(), 1);
    }

    #[test]
    fn unparsable_refs_and_vertices_are_skipped() {
        let m = parse("v 0 0 0\nv 1 0 0\nv a 1 0\nv 0 1\nv 0 1 0\nf 1 x 2 3\nf 1 2 y\n").unwrap();
        assert_eq!(m.num_vertices(), 3);
        // `x` skipped -> triangle (1, 2, 3); second face keeps only two refs -> nothing.
        assert_eq!(m.num_triangles(), 1);
    }

    #[test]
    fn tabs_and_leading_whitespace_are_accepted() {
        let m = parse("v\t0 0 0\n  v 1 0 0\nv 0 1 0\n\tf 1 2 3\n").unwrap();
        assert_eq!((m.num_vertices(), m.num_triangles()), (3, 1));
    }

    #[test]
    fn no_vertices_errors() {
        for text in ["", "# nothing\nf 1 2 3\n", "vn 0 0 1\nvt 0 0\n"] {
            let err = parse(text).unwrap_err();
            assert_eq!(
                err.to_string(),
                "No valid geometry found in Wavefront OBJ file."
            );
        }
    }

    #[test]
    fn vertices_without_faces_are_kept() {
        let m = parse("v 0 0 0\nv 1 2 3\n").unwrap();
        assert_eq!((m.num_vertices(), m.num_triangles()), (2, 0));
        assert_eq!(m.bounding_box_size(), [1.0, 2.0, 3.0]);
    }
}
