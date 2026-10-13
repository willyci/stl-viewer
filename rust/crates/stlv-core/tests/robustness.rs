//! Bad input must never panic: truncated, empty, random and mutated files for every parser,
//! and the mesh processing on whatever they return.

mod common;

use common::*;
use stlv_core::mesh::{DEFAULT_FEATURE_ANGLE_DEG, compute_render_mesh, unique_edges};
use stlv_core::parsers::{parse_3mf_bytes, parse_gcode_bytes, parse_obj_bytes, parse_stl_bytes};
use stlv_core::{LoadError, ModelData, load_model};

type Parser = fn(&[u8], String) -> Result<ModelData, LoadError>;
const PARSERS: [Parser; 4] = [
    parse_stl_bytes,
    parse_3mf_bytes,
    parse_obj_bytes,
    parse_gcode_bytes,
];

/// Run every parser on `data`; check the invariants of whatever parses.
fn exercise(data: &[u8]) {
    for parse in PARSERS {
        if let Ok(model) = parse(data, "fuzz".into()) {
            check_model(&model);
        }
    }
}

fn check_model(m: &ModelData) {
    let n = m.vertices.len();
    assert!(
        m.triangles.iter().flatten().all(|&i| (i as usize) < n),
        "triangle index out of range"
    );
    assert!(
        m.lines.iter().flatten().all(|&i| (i as usize) < n),
        "line index out of range"
    );
    assert!(m.volume >= 0.0 || m.volume.is_nan());
    let mesh = compute_render_mesh(m, DEFAULT_FEATURE_ANGLE_DEG);
    assert_eq!(mesh.positions.len(), mesh.normals.len());
    assert_eq!(mesh.indices.len(), m.triangles.len() * 3);
    assert!(
        mesh.indices
            .iter()
            .all(|&i| (i as usize) < mesh.positions.len())
    );
    assert!(
        mesh.normals.iter().flatten().all(|c| c.is_finite()),
        "non-finite normal"
    );
    let edges = unique_edges(&mesh.indices);
    assert!(
        edges
            .iter()
            .all(|&[a, b]| a < b && (b as usize) < mesh.positions.len())
    );
}

fn fixtures() -> Vec<Vec<u8>> {
    let mut files = Vec::new();
    if let Some(dir) = models_dir() {
        for name in [
            "ascii_cube.stl",
            "binary_cylinder.stl",
            "cube.obj",
            "pyramid_mesh.3mf",
            "hierarchical_assembly.3mf",
            "spiral_path.gcode",
        ] {
            files.push(std::fs::read(dir.join(name)).unwrap());
        }
    }
    let assembly = model_xml(
        Some("inch"),
        &format!(
            "<resources>{}<object id=\"2\"><components><component objectid=\"1\" transform=\"0 1 0 -1 0 0 0 0 1 5 0 0\"/>\
             </components></object></resources><build><item objectid=\"2\"/></build>",
            box_object("1", [0.0; 3], [1.0; 3])
        ),
    );
    files.push(zip(&[
        ("[Content_Types].xml", CONTENT_TYPES),
        ("3D/3dmodel.model", &assembly),
    ]));
    files.push(b"v 0 0 0\nv 1 0 0\nv 0 1 0\nv 0 0 1\nf 1 2 3\nf 1 -1 2 3\nf 4//1 3/1 2\n".to_vec());
    files.push(b"G91\nG1 X1 Y1 E1\nG00 Z1\nG01 X-1\n".to_vec());
    files
}

#[test]
fn empty_files_through_load_model() {
    let tmp = TempDir::new("empty");
    for ext in ["stl", "3mf", "obj", "gcode", "gco"] {
        let err = load_model(&tmp.write(&format!("empty.{ext}"), b"")).unwrap_err();
        assert!(matches!(err, LoadError::Invalid(_)), "{ext}: {err:?}");
    }
    exercise(b"");
}

#[test]
fn truncated_files() {
    for file in fixtures() {
        // Every prefix for small files, a spread of prefixes for big ones.
        let step = (file.len() / 600).max(1);
        for len in (0..=file.len()).step_by(step) {
            exercise(&file[..len]);
        }
    }
}

#[test]
fn mutated_files() {
    let mut rng = Rng(0x5EED_1234_ABCD_0001);
    for file in fixtures() {
        for _ in 0..200 {
            let mut data = file.clone();
            for _ in 0..1 + rng.below(8) {
                let at = rng.below(data.len());
                data[at] = rng.next() as u8;
            }
            exercise(&data);
        }
    }
}

#[test]
fn mutated_xml_inside_valid_zip() {
    let mut rng = Rng(42);
    let xml = model_xml(
        None,
        &format!(
            "<resources>{}<object id=\"2\"><components><component objectid=\"1\" transform=\"1 0 0 0 1 0 0 0 1 2 0 0\"/>\
             <component objectid=\"2\"/></components></object></resources><build><item objectid=\"2\"/><item objectid=\"1\"/></build>",
            box_object("1", [0.0; 3], [1.0; 3])
        ),
    );
    let bytes = xml.as_bytes();
    for _ in 0..300 {
        let mut data = bytes[..rng.below(bytes.len() + 1)].to_vec();
        if rng.below(2) == 0 && !data.is_empty() {
            let at = rng.below(data.len());
            data[at] = b"<>\"=/ 0123456789-.ex"[rng.below(20)];
        }
        let text = String::from_utf8_lossy(&data);
        let archive = zip(&[("3D/3dmodel.model", &text)]);
        if let Ok(m) = parse_3mf_bytes(&archive, String::new()) {
            check_model(&m);
        }
    }
}

#[test]
fn random_bytes() {
    let mut rng = Rng(0xDEAD_BEEF_CAFE_F00D);
    for i in 0..400 {
        let len = rng.below(if i % 10 == 0 { 20_000 } else { 400 });
        let mut data = rng.bytes(len);
        // Give some inputs plausible magic so they reach the deeper code paths.
        match i % 5 {
            0 => data
                .splice(0..0, b"solid x\nfacet\nvertex 1 2 3\n".iter().copied())
                .for_each(drop),
            1 => data
                .splice(0..0, b"PK\x03\x04".iter().copied())
                .for_each(drop),
            2 => data
                .splice(0..0, b"v 1 2 3\nf 1 1 1\n".iter().copied())
                .for_each(drop),
            3 => data
                .splice(0..0, b"G1 X1 Y2 Z3\n".iter().copied())
                .for_each(drop),
            _ => {}
        }
        exercise(&data);
    }
}

#[test]
fn binary_stl_with_huge_counts() {
    for count in [u32::MAX, u32::MAX / 50, 1 << 31, 1_000_000] {
        for records in [0usize, 1, 2] {
            let mut data = vec![0u8; 80];
            data.extend_from_slice(&count.to_le_bytes());
            for r in 0..records {
                data.extend_from_slice(&[0u8; 12]);
                for c in [
                    [0.0f32, 0.0, r as f32],
                    [1.0, 0.0, 0.0],
                    [f32::INFINITY, f32::NAN, 0.0],
                ] {
                    data.extend(c.iter().flat_map(|v| v.to_le_bytes()));
                }
                data.extend_from_slice(&[0, 0]);
            }
            let m = parse_stl_bytes(&data, String::new()).unwrap();
            assert_eq!(m.num_triangles(), records);
            check_model(&m);
        }
    }
}

#[test]
fn pathological_meshes() {
    // A fan of many triangles around one point (high valence) and a non-manifold fin.
    let mut vertices = vec![[0.0f32, 0.0, 0.0]];
    let mut triangles = Vec::new();
    let k = 5000u32;
    for i in 0..k {
        let a = i as f32 / k as f32 * std::f32::consts::TAU;
        vertices.push([a.cos(), a.sin(), (i % 3) as f32 * 0.01]);
    }
    for i in 0..k {
        triangles.push([0, 1 + i, 1 + (i + 1) % k]);
        triangles.push([0, 1 + (i + 1) % k, 1 + i]); // flipped duplicate: non-manifold edges
    }
    triangles.push([0, 0, 0]);
    let m = ModelData::new(vertices, triangles, vec![], "test", "fan");
    check_model(&m);
}

/// Run `f` on a thread with a deliberately small stack, so a parser that recurses per XML
/// nesting level overflows here (aborting the test binary) instead of passing on a big stack.
fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

fn nested(depth: usize, inner: &str) -> String {
    format!("{}{inner}{}", "<g>".repeat(depth), "</g>".repeat(depth))
}

#[test]
fn deeply_nested_xml_fails_cleanly() {
    // The reviewer's files: 5,000 and 200,000 nested <g> before a valid tetrahedron.
    for data in [
        &include_bytes!("data/deep_5000.3mf")[..],
        &include_bytes!("data/deep_xml.3mf")[..],
    ] {
        let err = on_small_stack(move || parse_3mf_bytes(data, String::new()).unwrap_err());
        assert!(err.to_string().contains("nested more than"), "{err}");
    }

    let tetra = box_object("1", [0.0; 3], [1.0; 3]);
    let body = |depth| {
        format!(
            "<resources>{}{tetra}</resources><build><item objectid=\"1\"/></build>",
            nested(depth, "")
        )
    };
    // Up to the limit the file still loads (the <g> run sits beside <model>'s children).
    let ok = zip(&[("3D/3dmodel.model", &model_xml(None, &body(250)))]);
    let m = on_small_stack(move || parse_3mf_bytes(&ok, String::new()).unwrap());
    assert_eq!(m.num_triangles(), 12);

    // The rels part and external component parts go through the same guard: a deep rels
    // file falls back to the default root part, a deep external part contributes nothing.
    let deep_rels = format!("<Relationships>{}</Relationships>", nested(100_000, ""));
    let deep_part = model_xml(None, &body(100_000));
    let assembly = model_xml(
        None,
        &format!(
            "<resources>{tetra}<object id=\"2\"><components><component objectid=\"1\"/>\
             <component p:path=\"/3D/deep.model\" objectid=\"1\"/></components></object></resources>\
             <build><item objectid=\"2\"/></build>"
        ),
    );
    let archive = zip(&[
        ("_rels/.rels", &deep_rels),
        ("3D/3dmodel.model", &assembly),
        ("3D/deep.model", &deep_part),
    ]);
    let m = on_small_stack(move || parse_3mf_bytes(&archive, String::new()).unwrap());
    assert_eq!(m.num_triangles(), 12);

    // Nesting hidden in DTD entity values counts once per possible expansion level.
    let entity = format!(
        "<?xml version=\"1.0\"?><!DOCTYPE model [<!ENTITY g \"{}\">]>{}",
        nested(30, ""),
        model_xml(None, &body(0)).replace("<resources>", "<resources>&g;")
    );
    let entity = entity.replacen("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n", "", 1);
    let archive = zip(&[("3D/3dmodel.model", &entity)]);
    let err = on_small_stack(move || parse_3mf_bytes(&archive, String::new()).unwrap_err());
    assert!(err.to_string().contains("nested more than"), "{err}");
}

#[test]
fn zip_bombs_fail_before_inflating_or_parsing() {
    // 48 MiB of whitespace deflates to ~50 KB: beyond the package's inflate budget.
    let padded = model_xml(None, &" ".repeat(48 << 20));
    let archive = zip(&[("3D/3dmodel.model", &padded)]);
    let err = parse_3mf_bytes(&archive, String::new()).unwrap_err();
    assert!(
        err.to_string().contains("decompresses to more than"),
        "{err}"
    );

    // 8 MiB of `<a/>` inflates within budget, but its DOM (~330 MB) would not fit the
    // budget a ~10 KB part gets.
    let elements = model_xml(
        None,
        &format!("<resources>{}</resources>", "<a/>".repeat(2 << 20)),
    );
    let archive = zip(&[("3D/3dmodel.model", &elements)]);
    assert!(archive.len() < 100_000, "{}", archive.len());
    let err = parse_3mf_bytes(&archive, String::new()).unwrap_err();
    assert!(err.to_string().contains("too complex"), "{err}");

    // Entity expansion is charged too (each reference may expand 256 times).
    let doc = format!(
        "<?xml version=\"1.0\"?><!DOCTYPE model [<!ENTITY e \"{}\">]>{}",
        "<a/>".repeat(20),
        model_xml(
            None,
            &format!("<resources>{}</resources>", "&e;".repeat(20_000))
        )
        .replacen("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n", "", 1)
    );
    let archive = zip(&[("3D/3dmodel.model", &doc)]);
    let err = parse_3mf_bytes(&archive, String::new()).unwrap_err();
    assert!(err.to_string().contains("too complex"), "{err}");
}

/// Object 0 is `leaf`; objects 1..=layers each instance the previous one `fanout` times, so
/// the build item (the last object) expands to fanout^layers leaves.
fn dag_3mf(layers: usize, fanout: usize, leaf: &str) -> Vec<u8> {
    let mut resources = leaf.to_owned();
    for i in 1..=layers {
        let comps = format!("<component objectid=\"{}\"/>", i - 1).repeat(fanout);
        resources += &format!("<object id=\"{i}\"><components>{comps}</components></object>");
    }
    let body =
        format!("<resources>{resources}</resources><build><item objectid=\"{layers}\"/></build>");
    zip(&[("3D/3dmodel.model", &model_xml(None, &body))])
}

#[test]
fn shared_component_dags_resolve_in_bounded_work() {
    // 8^30 paths that all end in an empty object: nothing to show, found without walking them.
    let empty = dag_3mf(30, 8, "<object id=\"0\"/>");
    let err = parse_3mf_bytes(&empty, String::new()).unwrap_err();
    assert!(err.to_string().contains("No displayable meshes"), "{err}");

    // The same with a one-vertex leaf is refused before anything is emitted.
    let leaf = "<object id=\"0\"><mesh><vertices><vertex x=\"0\" y=\"0\" z=\"0\"/></vertices></mesh></object>";
    let err = parse_3mf_bytes(&dag_3mf(30, 8, leaf), String::new()).unwrap_err();
    assert!(err.to_string().contains("too large to display"), "{err}");

    // A modest DAG still expands every instance: 4^5 boxes (all at the origin).
    let m = parse_3mf_bytes(
        &dag_3mf(5, 4, &box_object("0", [0.0; 3], [1.0; 3])),
        String::new(),
    )
    .unwrap();
    assert_eq!(
        (m.num_vertices(), m.num_triangles()),
        (8, 12 * 4usize.pow(5))
    );
}

/// Vertices that all collide under the original unkeyed vertex hash (a multiply-xorshift of
/// the coordinate bits), the way make_hashflood.py builds them.
fn unkeyed_hash_flood(n: usize) -> Vec<[f32; 3]> {
    const C1: u64 = 0x9E37_79B9_7F4A_7C15;
    const C2: u64 = 0xC2B2_AE3D_27D4_EB4F;
    const TARGET: u64 = 0x0123_4567_89AB_CDEF;
    let inv = (0..6).fold(C1, |x, _| {
        x.wrapping_mul(2u64.wrapping_sub(C1.wrapping_mul(x)))
    });
    let finite = |b: u32| f32::from_bits(b).is_finite() && b != 0x8000_0000;
    let mut out = Vec::with_capacity(n);
    let mut z = 0x3F80_0000u32;
    while out.len() < n {
        z += 1;
        let xy = (TARGET ^ u64::from(z).wrapping_mul(C2)).wrapping_mul(inv);
        let (x, y) = (xy as u32, (xy >> 32) as u32);
        if finite(x) && finite(y) && finite(z) {
            out.push([x, y, z].map(f32::from_bits));
        }
    }
    out
}

#[test]
fn crafted_hash_collisions_do_not_make_dedup_quadratic() {
    // 120k distinct vertices took ~30 s with the unkeyed hash; keyed it is milliseconds.
    let flat = unkeyed_hash_flood(120_000);
    let start = std::time::Instant::now();
    let (unique, inverse) = stlv_core::mesh::dedup_vertices(&flat);
    let mut seq = stlv_core::mesh::VertexDedup::with_capacity(flat.len());
    flat.iter().for_each(|&v| _ = seq.insert(v));
    assert!(start.elapsed().as_secs() < 5, "{:?}", start.elapsed());
    assert_eq!(unique, flat);
    assert!(inverse.iter().enumerate().all(|(i, &j)| j as usize == i));
    assert_eq!(seq.len(), flat.len());
}
