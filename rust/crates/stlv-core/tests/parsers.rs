//! Port of the Python app's `test_parsers.py` plus error-message and deviation coverage.

mod common;

use std::path::Path;

use common::*;
use stlv_core::parsers::{parse_3mf_bytes, parse_gcode_bytes, parse_stl_bytes};
use stlv_core::{LoadError, ModelData, load_model};

fn load_bytes_3mf(entries: &[(&str, &str)]) -> Result<ModelData, LoadError> {
    parse_3mf_bytes(&zip(entries), "t.3mf".into())
}

fn simple_3mf(model: &str) -> Vec<(&'static str, String)> {
    vec![
        ("[Content_Types].xml", CONTENT_TYPES.to_owned()),
        ("3D/3dmodel.model", model.to_owned()),
    ]
}

fn as_refs<'a>(entries: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    entries.iter().map(|(n, c)| (*n, c.as_str())).collect()
}

// ---------------------------------------------------------------------------------------------
// test_parsers.py

#[test]
fn python_test_parsers_meshes() {
    let Some(dir) = models_dir() else { return };
    let cases = [
        ("ascii_cube.stl", "STL (ASCII)", 12, 64000.0),
        ("binary_cylinder.stl", "STL (Binary)", 128, 35118.0),
        ("pyramid_mesh.3mf", "3MF Model", 6, 33333.3),
        ("hierarchical_assembly.3mf", "3MF Model", 48, 13500.0),
        ("cube.obj", "Wavefront OBJ", 12, 8000.0),
    ];
    for (name, format, triangles, volume) in cases {
        let m = load_model(&dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(m.format, format, "{name}");
        assert_eq!(m.filename, name);
        assert_eq!(m.num_triangles(), triangles, "{name}");
        let err_pct = (m.volume - volume).abs() / volume * 100.0;
        assert!(
            err_pct < 5.0,
            "{name}: volume {} vs {volume} ({err_pct:.2}%)",
            m.volume
        );
        assert!(m.load_time_ms >= 0.0 && m.load_time_ms.is_finite());
        assert!(!m.is_toolpath());
    }
}

#[test]
fn python_test_parsers_gcode() {
    let Some(dir) = models_dir() else { return };
    let m = load_model(&dir.join("spiral_path.gcode")).unwrap();
    assert_eq!(m.format, "G-Code Path");
    assert_eq!(m.num_triangles(), 0);
    assert_eq!(m.num_lines(), 41);
    assert_eq!(m.num_vertices(), 82);
    assert!(m.is_toolpath());
    assert_eq!(m.volume, 0.0);
}

#[test]
fn sample_models_details() {
    let Some(dir) = models_dir() else { return };
    let cube = load_model(&dir.join("ascii_cube.stl")).unwrap();
    assert_eq!(cube.num_vertices(), 8);
    assert_eq!(cube.bounds, [[-20.0, 20.0], [-20.0, 20.0], [-20.0, 20.0]]);
    let obj = load_model(&dir.join("cube.obj")).unwrap();
    assert_eq!(
        (obj.num_vertices(), obj.bounding_box_size()),
        (8, [20.0, 20.0, 20.0])
    );
    // Uppercase extensions dispatch too.
    let tmp = TempDir::new("ext");
    let copy = tmp.write("CUBE.OBJ", &std::fs::read(dir.join("cube.obj")).unwrap());
    assert_eq!(load_model(&copy).unwrap().num_triangles(), 12);
}

// ---------------------------------------------------------------------------------------------
// User-visible error messages (verbatim from parsers.py)

#[test]
fn error_messages_through_load_model() {
    let tmp = TempDir::new("errors");
    let empty_zip = zip(&[("readme.txt", "hi")]);
    let no_build = zip(&as_refs(&simple_3mf(&model_xml(
        None,
        "<resources/><build/>",
    ))));
    let bad_xml = zip(&[("3D/3dmodel.model", "<model><resources></model>")]);
    let cases: [(&str, &[u8], &str); 9] = [
        (
            "a.stl",
            b"solid a\n facet normal 0 0 1\n outer loop\n endloop\nendsolid\n",
            "No vertices found in ASCII STL file.",
        ),
        (
            "b.stl",
            b"tiny",
            "Corrupt Binary STL file: Cannot read triangle count.",
        ),
        (
            "c.3mf",
            b"definitely not a zip",
            "File is not a valid 3MF zip archive.",
        ),
        (
            "d.3mf",
            &empty_zip,
            "Could not locate 3D/3dmodel.model in 3MF archive.",
        ),
        (
            "f.3mf",
            &no_build,
            "No displayable meshes or build items found in 3MF file.",
        ),
        (
            "g.obj",
            b"# no vertices\nvn 0 0 1\nf 1 2 3\n",
            "No valid geometry found in Wavefront OBJ file.",
        ),
        (
            "h.gcode",
            b"G28\nG0 X10\nM107\n",
            "No valid print movement paths found in GCODE file.",
        ),
        (
            "i.gco",
            b"",
            "No valid print movement paths found in GCODE file.",
        ),
        ("j.xyz", b"", "Unsupported file format: .xyz"),
    ];
    for (name, data, message) in cases {
        let err = load_model(&tmp.write(name, data)).unwrap_err();
        assert_eq!(err.to_string(), message, "{name}");
        assert!(!matches!(err, LoadError::Io(_)), "{name}");
    }
    let err = load_model(&tmp.write("e.3mf", &bad_xml)).unwrap_err();
    assert!(
        err.to_string()
            .starts_with("Failed to read root XML inside 3MF model: "),
        "{err}"
    );

    // Missing files are I/O errors with the OS message.
    let err = load_model(&tmp.0.join("missing.stl")).unwrap_err();
    assert!(matches!(err, LoadError::Io(_)), "{err:?}");
    assert!(matches!(
        load_model(Path::new("Z:/nope/x.3mf")),
        Err(LoadError::Io(_))
    ));
}

// ---------------------------------------------------------------------------------------------
// 3MF

#[test]
fn threemf_rotated_component_uses_row_vectors() {
    // A 10 x 2 x 1 box rotated 90 degrees about Z by a component, then moved by the build item.
    // Row-vector convention: x axis -> y axis, so the result spans 2 in X and 10 in Y.
    let body = format!(
        "<resources>{}<object id=\"2\"><components><component objectid=\"1\" transform=\"0 1 0 -1 0 0 0 0 1 0 0 0\"/>\
         </components></object></resources><build><item objectid=\"2\" transform=\"1 0 0 0 1 0 0 0 1 100 0 0\"/></build>",
        box_object("1", [0.0; 3], [10.0, 2.0, 1.0])
    );
    let m = load_bytes_3mf(&as_refs(&simple_3mf(&model_xml(None, &body)))).unwrap();
    assert_eq!((m.num_vertices(), m.num_triangles()), (8, 12));
    assert_eq!(m.bounds, [[98.0, 100.0], [0.0, 10.0], [0.0, 1.0]]);
    assert_close(m.volume, 20.0, 1e-9, "volume");
}

#[test]
fn threemf_nested_transforms_apply_child_first() {
    // Item: rotate 90 deg about Z; component: translate +5 in X. The component transform is
    // applied first, then the item's.
    let body = format!(
        "<resources>{}<object id=\"2\"><components><component objectid=\"1\" transform=\"1 0 0 0 1 0 0 0 1 5 0 0\"/>\
         </components></object></resources><build><item objectid=\"2\" transform=\"0 1 0 -1 0 0 0 0 1 0 0 0\"/></build>",
        box_object("1", [0.0; 3], [1.0; 3])
    );
    let m = load_bytes_3mf(&as_refs(&simple_3mf(&model_xml(None, &body)))).unwrap();
    // (x, y) -> (-y, x) after the shift: x in [5, 6] -> y in [5, 6]; y in [0, 1] -> x in [-1, 0].
    assert_eq!(m.bounds, [[-1.0, 0.0], [5.0, 6.0], [0.0, 1.0]]);
}

#[test]
fn threemf_units_scale_to_millimetres() {
    for (unit, scale) in [
        ("micron", 0.001),
        ("millimeter", 1.0),
        ("centimeter", 10.0),
        ("inch", 25.4),
        ("foot", 304.8),
        ("meter", 1000.0),
    ] {
        let body = format!(
            "<resources>{}</resources><build><item objectid=\"1\"/></build>",
            box_object("1", [0.0; 3], [1.0, 2.0, 4.0])
        );
        let m = load_bytes_3mf(&as_refs(&simple_3mf(&model_xml(Some(unit), &body)))).unwrap();
        let size = m.bounding_box_size();
        for (axis, base) in [1.0, 2.0, 4.0].into_iter().enumerate() {
            let expected = f64::from((base * scale) as f32);
            assert_close(size[axis], expected, expected * 1e-6, unit);
        }
    }
}

#[test]
fn threemf_root_from_rels_and_external_parts() {
    // Root part lives at a non-standard path named by _rels/.rels; it pulls geometry from an
    // external part (production extension) referenced with a backslash, odd-case path.
    let rels = "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Target=\"/Auxiliaries/thumb.png\" Id=\"r0\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail\"/>\
        <Relationship Target=\"/Models/Main.model\" Id=\"r1\" Type=\"http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel\"/>\
        </Relationships>";
    let root = model_xml(
        None,
        "<resources><object id=\"7\"><components><component p:path=\"\\models\\PARTS\\box.model\" objectid=\"3\"/>\
         </components></object></resources><build><item objectid=\"7\"/></build>",
    );
    let part = model_xml(
        None,
        &format!(
            "<resources>{}</resources><build/>",
            box_object("3", [0.0; 3], [3.0; 3])
        ),
    );
    // A decoy at the conventional path must be ignored.
    let decoy = model_xml(
        None,
        &format!(
            "<resources>{}</resources><build><item objectid=\"1\"/></build>",
            box_object("1", [0.0; 3], [50.0; 3])
        ),
    );
    let m = load_bytes_3mf(&[
        ("_rels/.rels", rels),
        ("Models/Main.model", &root),
        ("Models/parts/Box.model", &part),
        ("3D/3dmodel.model", &decoy),
    ])
    .unwrap();
    assert_eq!(m.bounding_box_size(), [3.0, 3.0, 3.0]);

    // Without (or with a useless) _rels/.rels the conventional path is used, case-insensitively.
    let m = load_bytes_3mf(&[("_rels/.rels", "<garbage"), ("3d/3DModel.MODEL", &decoy)]).unwrap();
    assert_eq!(m.bounding_box_size(), [50.0, 50.0, 50.0]);
}

#[test]
fn threemf_entries_stored_with_backslashes() {
    // Windows tools may store `3D\3dmodel.model`; Python's zipfile reads it as `3D/...`.
    let root = model_xml(
        None,
        "<resources><object id=\"2\"><components><component p:path=\"/3D/parts/box.model\" objectid=\"1\"/>\
         </components></object></resources><build><item objectid=\"2\"/></build>",
    );
    let part = model_xml(
        None,
        &format!(
            "<resources>{}</resources><build/>",
            box_object("1", [0.0; 3], [4.0; 3])
        ),
    );
    let m = load_bytes_3mf(&[
        ("3D\\3dmodel.model", &root),
        ("3D\\parts\\box.model", &part),
    ])
    .unwrap();
    assert_eq!(m.bounding_box_size(), [4.0, 4.0, 4.0]);
}

#[test]
fn threemf_build_fallbacks() {
    // Empty build: top-level objects (not used as components) are shown, once.
    let body = format!(
        "<resources>{}<object id=\"2\"><components><component objectid=\"1\" transform=\"1 0 0 0 1 0 0 0 1 10 0 0\"/></components></object>\
         </resources><build/>",
        box_object("1", [0.0; 3], [1.0; 3])
    );
    let m = load_bytes_3mf(&as_refs(&simple_3mf(&model_xml(None, &body)))).unwrap();
    assert_eq!(m.num_triangles(), 12);
    assert_eq!(m.bounds[0], [10.0, 11.0]);

    // Every object is a component of another (a cycle): everything with geometry is shown.
    let body = format!(
        "<resources>{}<object id=\"2\"><components><component objectid=\"1\"/><component objectid=\"3\"/></components></object>\
         <object id=\"3\"><components><component objectid=\"2\"/></components></object></resources>",
        box_object("1", [0.0; 3], [1.0; 3]).replace(
            "</mesh>",
            "</mesh><components><component objectid=\"2\"/></components>"
        )
    );
    let m = load_bytes_3mf(&as_refs(&simple_3mf(&model_xml(None, &body)))).unwrap();
    assert_eq!(m.num_vertices(), 8);
    assert_eq!(m.num_triangles(), 36);

    // Build items pointing at unknown objects fall back as well.
    let body = format!(
        "<resources>{}</resources><build><item objectid=\"9\"/><item/></build>",
        box_object("1", [0.0; 3], [2.0; 3])
    );
    let m = load_bytes_3mf(&as_refs(&simple_3mf(&model_xml(None, &body)))).unwrap();
    assert_eq!(m.num_triangles(), 12);
}

#[test]
fn threemf_instancing_and_cycles() {
    // The same part instanced twice side by side, and a self-referencing component ignored.
    let body = format!(
        "<resources>{}<object id=\"2\"><components>\
         <component objectid=\"1\"/><component objectid=\"1\" transform=\"1 0 0 0 1 0 0 0 1 1 0 0\"/>\
         <component objectid=\"2\"/><component objectid=\"404\"/><component/></components></object></resources>\
         <build><item objectid=\"2\"/></build>",
        box_object("1", [0.0; 3], [1.0; 3])
    );
    let m = load_bytes_3mf(&as_refs(&simple_3mf(&model_xml(None, &body)))).unwrap();
    // Two unit cubes sharing a face: 12 unique corners, 24 triangles.
    assert_eq!((m.num_vertices(), m.num_triangles()), (12, 24));
    assert_close(m.volume, 2.0, 1e-9, "volume");
}

#[test]
fn threemf_malformed_numbers_and_indices() {
    let body = "<resources><object id=\"1\"><mesh><vertices>\
        <vertex x=\"0\" y=\"0\" z=\"0\"/><vertex x=\"1\" y=\"oops\" z=\"0\"/><vertex X=\"0\" Y=\"1\"/><vertex x=\"1\" y=\"1\" z=\"1\"/>\
        </vertices><triangles>\
        <triangle v1=\"0\" v2=\"1\" v3=\"2\"/><triangle v1=\"0\" v2=\"1\" v3=\"4\"/><triangle v1=\"-1\" v2=\"1\" v3=\"2\"/>\
        <triangle v1=\"0\" v2=\"2.5\" v3=\"3\"/><triangle v2=\"2\" v3=\"3\"/>\
        </triangles></mesh></object></resources><build><item objectid=\"1\" transform=\"garbage\"/></build>";
    let m = load_bytes_3mf(&as_refs(&simple_3mf(&model_xml(None, body)))).unwrap();
    assert_eq!(m.num_vertices(), 4);
    // Kept: (0,1,2) and the one with a missing v1 (reads as 0).
    assert_eq!(m.num_triangles(), 2);
    assert_eq!(m.bounds, [[0.0, 1.0], [0.0, 1.0], [0.0, 1.0]]);
}

// ---------------------------------------------------------------------------------------------
// STL / OBJ / G-code deviations

#[test]
fn stl_size_based_binary_detection() {
    let mut data = vec![b' '; 80];
    data[..32].copy_from_slice(b"solid facet outer exported ascii");
    data.extend_from_slice(&1u32.to_le_bytes());
    data.extend_from_slice(&[0u8; 12]);
    for v in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
        data.extend(v.iter().flat_map(|c| c.to_le_bytes()));
    }
    data.extend_from_slice(&[0, 0]);
    let m = parse_stl_bytes(&data, String::new()).unwrap();
    assert_eq!((m.format.as_str(), m.num_triangles()), ("STL (Binary)", 1));
}

#[test]
fn obj_out_of_range_faces_are_dropped() {
    let tmp = TempDir::new("obj");
    let path = tmp.write(
        "q.obj",
        b"v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1 2 3 4\nf 0 1 2\nf 1 2 5\nf -9 1 2\n",
    );
    let m = load_model(&path).unwrap();
    assert_eq!((m.num_vertices(), m.num_triangles()), (4, 2));
}

#[test]
fn gcode_zero_padded_moves() {
    let m = parse_gcode_bytes(b"G00 X10 Y0\nG01 X20 E1\nG1 X30\nG0 X40\n", String::new()).unwrap();
    assert_eq!(m.num_lines(), 2);
    assert_eq!(m.bounds[0], [10.0, 30.0]);
}
