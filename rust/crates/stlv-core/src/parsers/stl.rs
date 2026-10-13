use std::path::Path;

use super::{LoadError, file_name, text};
use crate::mesh::{dedup_by, dedup_vertices};
use crate::model::ModelData;

const HEADER_LEN: usize = 80;
const RECORD_LEN: usize = 50;

/// ASCII or binary STL, auto-detected; vertices deduplicated.
pub fn parse_stl(path: &Path) -> Result<ModelData, LoadError> {
    let data = std::fs::read(path)?;
    parse_stl_bytes(&data, file_name(path))
}

/// [`parse_stl`] on an in-memory file.
pub fn parse_stl_bytes(data: &[u8], filename: String) -> Result<ModelData, LoadError> {
    if is_ascii(data) {
        parse_ascii(data, filename)
    } else {
        parse_binary(data, filename)
    }
}

/// Python's sniffing: starts with `solid` and the next 1024 bytes mention `facet` or `outer`.
/// Deviation: a file whose size is exactly `84 + 50 * count` is binary even when its header
/// happens to start with `solid ... facet` text.
fn is_ascii(data: &[u8]) -> bool {
    if !data.starts_with(b"solid") {
        return false;
    }
    if let Some(count) = triangle_count(data)
        && count > 0
        && data.len() as u64 == (HEADER_LEN + 4) as u64 + RECORD_LEN as u64 * u64::from(count)
    {
        return false;
    }
    let sample = &data[5..data.len().min(5 + 1024)];
    contains(sample, b"facet") || contains(sample, b"outer")
}

fn triangle_count(data: &[u8]) -> Option<u32> {
    let bytes = data.get(HEADER_LEN..HEADER_LEN + 4)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn parse_ascii(data: &[u8], filename: String) -> Result<ModelData, LoadError> {
    let mut corners: Vec<[f32; 3]> = Vec::new();
    for line in text::lines(data) {
        let line = text::trim(line);
        if line.len() < 6 || !line[..6].eq_ignore_ascii_case(b"vertex") {
            continue;
        }
        let mut toks = text::tokens(line).skip(1);
        let (Some(x), Some(y), Some(z)) = (toks.next(), toks.next(), toks.next()) else {
            continue;
        };
        if let (Some(x), Some(y), Some(z)) =
            (text::parse_f32(x), text::parse_f32(y), text::parse_f32(z))
        {
            corners.push([x, y, z]);
        }
    }
    if corners.is_empty() {
        return Err(LoadError::invalid("No vertices found in ASCII STL file."));
    }
    // A trailing partial triangle is dropped (Python would fail to reshape).
    let (vertices, inverse) = dedup_vertices(&corners[..corners.len() / 3 * 3]);
    Ok(ModelData::new(
        vertices,
        to_triangles(&inverse),
        vec![],
        "STL (ASCII)",
        filename,
    ))
}

fn parse_binary(data: &[u8], filename: String) -> Result<ModelData, LoadError> {
    let count = triangle_count(data).ok_or_else(|| {
        LoadError::invalid("Corrupt Binary STL file: Cannot read triangle count.")
    })?;
    let body = &data[HEADER_LEN + 4..];
    // A truncated file (or an inflated count) uses whatever whole records are present.
    let count = (count as usize).min(body.len() / RECORD_LEN);
    // Record: normal (ignored), three vertices, attribute byte count.
    let f = |at: usize| f32::from_le_bytes([body[at], body[at + 1], body[at + 2], body[at + 3]]);
    let corner = |i: usize| {
        let at = (i / 3) * RECORD_LEN + 12 + 12 * (i % 3);
        [f(at), f(at + 4), f(at + 8)]
    };
    // Closed meshes have about half as many vertices as triangles.
    let (vertices, inverse) = dedup_by(3 * count, count / 2 + 16, corner);
    Ok(ModelData::new(
        vertices,
        to_triangles(&inverse),
        vec![],
        "STL (Binary)",
        filename,
    ))
}

fn to_triangles(corners: &[u32]) -> Vec<[u32; 3]> {
    corners
        .chunks_exact(3)
        .map(|t| [t[0], t[1], t[2]])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binary(tris: &[[[f32; 3]; 3]], declared: u32) -> Vec<u8> {
        let mut data = vec![0u8; HEADER_LEN];
        data.extend_from_slice(&declared.to_le_bytes());
        for t in tris {
            data.extend_from_slice(&[0u8; 12]);
            for v in t {
                for c in v {
                    data.extend_from_slice(&c.to_le_bytes());
                }
            }
            data.extend_from_slice(&[0u8; 2]);
        }
        data
    }

    const QUAD: [[[f32; 3]; 3]; 2] = [
        [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0]],
        [[0.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]],
    ];

    #[test]
    fn binary_dedups_and_ignores_extra_bytes() {
        let mut data = binary(&QUAD, 2);
        data.extend_from_slice(b"trailing junk");
        let m = parse_stl_bytes(&data, "q.stl".into()).unwrap();
        assert_eq!(m.format, "STL (Binary)");
        assert_eq!((m.num_vertices(), m.num_triangles()), (4, 2));
        assert_eq!(m.filename, "q.stl");
    }

    #[test]
    fn binary_count_larger_than_data_is_clamped() {
        let data = binary(&QUAD, u32::MAX);
        let m = parse_stl_bytes(&data, String::new()).unwrap();
        assert_eq!(m.num_triangles(), 2);
        let mut truncated = binary(&QUAD, 2);
        truncated.truncate(truncated.len() - 1);
        assert_eq!(
            parse_stl_bytes(&truncated, String::new())
                .unwrap()
                .num_triangles(),
            1
        );
    }

    #[test]
    fn binary_zero_triangles_is_an_empty_model() {
        let m = parse_stl_bytes(&binary(&[], 0), String::new()).unwrap();
        assert_eq!((m.num_vertices(), m.num_triangles()), (0, 0));
    }

    #[test]
    fn short_binary_reports_python_message() {
        for len in [0, 5, 83] {
            let err = parse_stl_bytes(&vec![7u8; len], String::new()).unwrap_err();
            assert_eq!(
                err.to_string(),
                "Corrupt Binary STL file: Cannot read triangle count."
            );
        }
    }

    #[test]
    fn solid_header_with_matching_size_is_binary() {
        let mut data = binary(&QUAD, 2);
        let header = b"solid exported by X facet normal outer loop";
        data[..header.len()].copy_from_slice(header);
        assert_eq!(
            parse_stl_bytes(&data, String::new()).unwrap().format,
            "STL (Binary)"
        );
        // Without the size match Python's sniffing decides (ASCII here, which finds no vertices).
        data.push(0);
        let err = parse_stl_bytes(&data, String::new()).unwrap_err();
        assert_eq!(err.to_string(), "No vertices found in ASCII STL file.");
    }

    #[test]
    fn solid_without_keywords_is_binary() {
        let mut data = binary(&QUAD, 2);
        data[..5].copy_from_slice(b"solid");
        data.push(1); // break the size match: the sniffing alone must choose binary
        assert_eq!(
            parse_stl_bytes(&data, String::new()).unwrap().format,
            "STL (Binary)"
        );
    }

    #[test]
    fn ascii_parsing_matches_python_rules() {
        let text = "solid t\r\n facet normal 0 0 1\n  outer loop\n   VERTEX 0 0 0\n\tvertex 1 0 0\r\
                    vertex 1 1 0\n   vertex 1 x 0\n   vertexfoo 0 0 0\n vertex 1 1 0\n vertex 0 1 0\n\
                    vertex 1 2\n vertex 9 9 9\n endsolid";
        let m = parse_stl_bytes(text.as_bytes(), String::new()).unwrap();
        assert_eq!(m.format, "STL (ASCII)");
        // `vertexfoo` counts (Python only checks the prefix); the bad-number and short lines
        // are skipped and the trailing single vertex is dropped: two whole triangles.
        assert_eq!((m.num_triangles(), m.num_vertices()), (2, 4));
        assert_eq!(m.bounds, [[0.0, 1.0], [0.0, 1.0], [0.0, 0.0]]);
    }

    #[test]
    fn ascii_without_vertices_errors() {
        let err =
            parse_stl_bytes(b"solid x\nfacet normal 0 0 0\nendsolid\n", String::new()).unwrap_err();
        assert_eq!(err.to_string(), "No vertices found in ASCII STL file.");
    }
}
