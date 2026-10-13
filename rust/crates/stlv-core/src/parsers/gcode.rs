use std::path::Path;

use super::{LoadError, file_name, text};
use crate::model::ModelData;

/// G-code G0/G1 moves as line segments (toolpath preview).
pub fn parse_gcode(path: &Path) -> Result<ModelData, LoadError> {
    let data = std::fs::read(path)?;
    parse_gcode_bytes(&data, file_name(path))
}

/// [`parse_gcode`] on an in-memory file.
pub fn parse_gcode_bytes(data: &[u8], filename: String) -> Result<ModelData, LoadError> {
    let mut vertices: Vec<[f32; 3]> = Vec::new();
    let mut lines: Vec<[u32; 2]> = Vec::new();
    let mut cur = [0.0f64; 3];
    let mut absolute = true;

    for line in text::lines(data) {
        let code = line.split(|&b| b == b';').next().unwrap_or_default();
        let mut toks = text::tokens(code);
        let Some(cmd) = toks.next() else { continue };
        let cmd = cmd.to_ascii_uppercase();
        let is_g1 = match cmd.as_slice() {
            b"G90" => {
                absolute = true;
                continue;
            }
            b"G91" => {
                absolute = false;
                continue;
            }
            // Deviation: the zero-padded G00 / G01 spellings are moves too.
            b"G0" | b"G00" => false,
            b"G1" | b"G01" => true,
            _ => continue,
        };

        // X, Y, Z, E; later duplicates win, unparsable / non-finite values are ignored.
        let mut params = [None::<f64>; 4];
        for tok in toks {
            if tok.len() < 2 {
                continue;
            }
            let slot = match tok[0].to_ascii_uppercase() {
                b'X' => 0,
                b'Y' => 1,
                b'Z' => 2,
                b'E' => 3,
                _ => continue,
            };
            if let Some(v) = text::parse_float(&tok[1..]).filter(|v| v.is_finite()) {
                params[slot] = Some(v);
            }
        }

        let mut new = cur;
        for axis in 0..3 {
            if let Some(v) = params[axis] {
                new[axis] = if absolute { v } else { cur[axis] + v };
            }
        }
        let extruding = params[3].is_some_and(|e| e > 0.0) || is_g1;
        if new != cur {
            if extruding {
                let first = vertices.len() as u32;
                vertices.push(cur.map(|c| c as f32));
                vertices.push(new.map(|c| c as f32));
                lines.push([first, first + 1]);
            }
            cur = new;
        }
    }

    if vertices.is_empty() {
        return Err(LoadError::invalid(
            "No valid print movement paths found in GCODE file.",
        ));
    }
    Ok(ModelData::new(
        vertices,
        vec![],
        lines,
        "G-Code Path",
        filename,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<ModelData, LoadError> {
        parse_gcode_bytes(text.as_bytes(), "t.gcode".into())
    }

    #[test]
    fn g1_moves_become_segments_g0_travels_do_not() {
        let m =
            parse("G28 ; home\nG0 X10 Y10\nG1 X20 E1.5\ng1 y20 ; lower case\nG1 X20 Y20\nG0 Z5\n")
                .unwrap();
        assert_eq!(m.format, "G-Code Path");
        assert!(m.is_toolpath());
        assert_eq!(
            (m.num_lines(), m.num_vertices(), m.num_triangles()),
            (2, 4, 0)
        );
        assert_eq!(
            m.vertices,
            [
                [10.0, 10.0, 0.0],
                [20.0, 10.0, 0.0],
                [20.0, 10.0, 0.0],
                [20.0, 20.0, 0.0]
            ]
        );
        assert_eq!(m.lines, [[0, 1], [2, 3]]);
        assert_eq!(m.volume, 0.0);
    }

    #[test]
    fn g0_with_positive_extrusion_draws() {
        let m = parse("G0 X1 E0.2\nG0 X2 E0\nG0 X3 E-1\n").unwrap();
        assert_eq!(m.num_lines(), 1);
    }

    #[test]
    fn zero_padded_commands() {
        let m = parse("G00 X5\nG01 X6 Y1\nG01 Z0.2\n").unwrap();
        assert_eq!(m.num_lines(), 2);
        assert_eq!(m.vertices[0], [5.0, 0.0, 0.0]);
    }

    #[test]
    fn relative_mode_and_bad_params() {
        let m =
            parse("G91\nG1 X1 Y1\nG1 X1 Xbad Y1 X2\nG1 Xnan Yinf Z0.5\nG90\nG1 X0 Y0 Z0\nG1 X\n")
                .unwrap();
        // One segment per pair of vertices.
        let expected: Vec<[f32; 3]> = [
            [[0.0, 0.0, 0.0], [1.0, 1.0, 0.0]],
            [[1.0, 1.0, 0.0], [3.0, 2.0, 0.0]],
            [[3.0, 2.0, 0.0], [3.0, 2.0, 0.5]],
            [[3.0, 2.0, 0.5], [0.0, 0.0, 0.0]],
        ]
        .concat();
        assert_eq!(m.vertices, expected);
        assert!(m.bounds.iter().flatten().all(|b| b.is_finite()));
    }

    #[test]
    fn no_moves_errors() {
        for text in [
            "",
            "G28\nM104 S200\n",
            "G1 X0 Y0\n",
            "; G1 X10\n",
            "G0 X10 Y10\n",
        ] {
            let err = parse(text).unwrap_err();
            assert_eq!(
                err.to_string(),
                "No valid print movement paths found in GCODE file."
            );
        }
    }
}
