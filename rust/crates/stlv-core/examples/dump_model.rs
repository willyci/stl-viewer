//! Differential-testing and benchmarking helper for stlv-core.
//!
//! ```text
//! dump_model info [--mesh-out DIR] FILE...   one JSON line per file (format, counts, bounds,
//!                                           volume); with --mesh-out also writes DIR/<name>.raw
//!                                           (parsed mesh) and DIR/<name>.rmesh (render mesh)
//! dump_model render IN.raw OUT.rmesh        compute_render_mesh on a raw mesh
//! dump_model bench FILE [REPEAT]            time load_model + compute_render_mesh + unique_edges
//! ```
//! `.raw`: u32 nv, u32 nt, f32 xyz * nv, u32 abc * nt.
//! `.rmesh`: u32 np, u32 ni, f32 xyz * np (positions), f32 xyz * np (normals), u32 * ni.

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use stlv_core::mesh::{DEFAULT_FEATURE_ANGLE_DEG, RenderMesh, compute_render_mesh, unique_edges};
use stlv_core::{ModelData, load_model};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("info") => info(&args[1..]),
        Some("render") if args.len() == 3 => render(Path::new(&args[1]), Path::new(&args[2])),
        Some("bench") if args.len() >= 2 => bench(Path::new(&args[1]), args.get(2).and_then(|n| n.parse().ok()).unwrap_or(3)),
        _ => Err("usage: dump_model info [--mesh-out DIR] FILE... | render IN.raw OUT.rmesh | bench FILE [N]".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn json_f64(x: f64) -> String {
    if x.is_finite() {
        format!("{x:?}")
    } else {
        "null".into()
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out + "\""
}

fn info(args: &[String]) -> Result<()> {
    let (mesh_out, files) = match args {
        [flag, dir, rest @ ..] if flag == "--mesh-out" => (Some(PathBuf::from(dir)), rest),
        _ => (None, args),
    };
    for file in files {
        let path = Path::new(file);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match load_model(path) {
            Ok(m) => {
                let bounds: Vec<String> = m
                    .bounds
                    .iter()
                    .map(|[lo, hi]| format!("[{}, {}]", json_f64(*lo), json_f64(*hi)))
                    .collect();
                let mut line = format!(
                    "{{\"file\": {}, \"format\": {}, \"vertices\": {}, \"triangles\": {}, \"lines\": {}, \"bounds\": [{}], \"volume\": {}",
                    json_str(&name),
                    json_str(&m.format),
                    m.num_vertices(),
                    m.num_triangles(),
                    m.num_lines(),
                    bounds.join(", "),
                    json_f64(m.volume)
                );
                if let Some(dir) = &mesh_out {
                    write_raw(&dir.join(format!("{name}.raw")), &m)?;
                    let mesh = compute_render_mesh(&m, DEFAULT_FEATURE_ANGLE_DEG);
                    write_rmesh(&dir.join(format!("{name}.rmesh")), &mesh)?;
                    line += &format!(", \"render_points\": {}", mesh.positions.len());
                }
                println!("{line}}}");
            }
            Err(e) => println!(
                "{{\"file\": {}, \"error\": {}}}",
                json_str(&name),
                json_str(&e.to_string())
            ),
        }
    }
    Ok(())
}

fn render(input: &Path, output: &Path) -> Result<()> {
    let data = std::fs::read(input)?;
    let u32_at = |i: usize| -> Result<u32> {
        let b = data.get(i..i + 4).ok_or("truncated raw mesh")?;
        Ok(u32::from_le_bytes(b.try_into()?))
    };
    let (nv, nt) = (u32_at(0)? as usize, u32_at(4)? as usize);
    let mut vertices = Vec::with_capacity(nv);
    for v in 0..nv {
        let at = 8 + 12 * v;
        let f = |k: usize| u32_at(at + 4 * k).map(f32::from_bits);
        vertices.push([f(0)?, f(1)?, f(2)?]);
    }
    let base = 8 + 12 * nv;
    let mut triangles = Vec::with_capacity(nt);
    for t in 0..nt {
        let at = base + 12 * t;
        triangles.push([u32_at(at)?, u32_at(at + 4)?, u32_at(at + 8)?]);
    }
    let model = ModelData::new(vertices, triangles, vec![], "raw", "raw");
    write_rmesh(
        output,
        &compute_render_mesh(&model, DEFAULT_FEATURE_ANGLE_DEG),
    )
}

fn bench(path: &Path, repeat: usize) -> Result<()> {
    for _ in 0..repeat.max(1) {
        let t0 = Instant::now();
        let model = load_model(path)?;
        let t1 = Instant::now();
        let mesh = compute_render_mesh(&model, DEFAULT_FEATURE_ANGLE_DEG);
        let t2 = Instant::now();
        let edges = unique_edges(&mesh.indices);
        let t3 = Instant::now();
        let ms = |a: Instant, b: Instant| (b - a).as_secs_f64() * 1000.0;
        println!(
            "{}: {} tris, {} verts -> {} render points, {} edges | load {:.0} ms, render mesh {:.0} ms, edges {:.0} ms, load+render {:.0} ms",
            model.filename,
            model.num_triangles(),
            model.num_vertices(),
            mesh.positions.len(),
            edges.len(),
            ms(t0, t1),
            ms(t1, t2),
            ms(t2, t3),
            ms(t0, t2)
        );
    }
    Ok(())
}

fn write_raw(path: &Path, m: &ModelData) -> Result<()> {
    let mut w = BufWriter::new(std::fs::File::create(path)?);
    w.write_all(&(m.vertices.len() as u32).to_le_bytes())?;
    w.write_all(&(m.triangles.len() as u32).to_le_bytes())?;
    for c in m.vertices.iter().flatten() {
        w.write_all(&c.to_le_bytes())?;
    }
    for i in m.triangles.iter().flatten() {
        w.write_all(&i.to_le_bytes())?;
    }
    Ok(w.flush()?)
}

fn write_rmesh(path: &Path, mesh: &RenderMesh) -> Result<()> {
    let mut w = BufWriter::new(std::fs::File::create(path)?);
    w.write_all(&(mesh.positions.len() as u32).to_le_bytes())?;
    w.write_all(&(mesh.indices.len() as u32).to_le_bytes())?;
    for c in mesh.positions.iter().chain(&mesh.normals).flatten() {
        w.write_all(&c.to_le_bytes())?;
    }
    for i in &mesh.indices {
        w.write_all(&i.to_le_bytes())?;
    }
    Ok(w.flush()?)
}
