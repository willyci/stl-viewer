//! Shared SVG loading: hostile documents are refused instead of overflowing the stack,
//! linked images resolve next to the file but never over the network, and text gets fonts.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{Harness, temp_dir};
use image::{Rgba, RgbaImage};
use stlv_media::{ImageViewer, rasterize_svg_fit};

const H: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="100" height="100">"#;

fn nested(n: usize) -> String {
    format!(
        "{H}{}<rect width=\"50\" height=\"50\" fill=\"red\"/>{}</svg>",
        "<g>".repeat(n),
        "</g>".repeat(n)
    )
}

fn red_count(img: &RgbaImage) -> usize {
    img.pixels()
        .filter(|p| p[0] > 200 && p[1] < 60 && p[2] < 60 && p[3] > 200)
        .count()
}

/// `ImageViewer::load` on a thread with the 1 MB stack of the Windows main thread.
fn load_on_ui_stack(path: &Path) -> Result<(u32, u32), String> {
    let path = path.to_path_buf();
    std::thread::Builder::new()
        .stack_size(1 << 20)
        .spawn(move || {
            ImageViewer::new()
                .load(&path)
                .map(|info| (info.width, info.height))
        })
        .unwrap()
        .join()
        .expect("load panicked")
}

#[test]
fn hostile_svgs_are_refused_not_fatal() {
    let dir = temp_dir("svg-hostile");
    let ring = |n: usize| {
        let defs: String = (0..n)
            .map(|i| {
                format!(
                    r#"<clipPath id="c{i}"><rect width="9" height="9" clip-path="url(#c{})"/></clipPath>"#,
                    (i + 1) % n
                )
            })
            .collect();
        format!(r#"{H}<defs>{defs}</defs><rect width="9" height="9" clip-path="url(#c0)"/></svg>"#)
    };
    let pattern_chain: String = (0..5000)
        .map(|i| {
            format!(
                r#"<pattern id="p{i}" width="9" height="9"><rect width="9" height="9" fill="url(#p{})"/></pattern>"#,
                i + 1
            )
        })
        .collect();
    let cases = [
        ("deep_5000.svg", nested(5000)),
        ("deep_100000.svg", nested(100_000)),
        ("clip_cycle.svg", ring(3)),
        (
            "pattern_chain.svg",
            format!(
                r#"{H}<defs>{pattern_chain}</defs><rect width="9" height="9" fill="url(#p0)"/></svg>"#
            ),
        ),
    ];
    for (name, svg) in cases {
        let path = dir.join(name);
        std::fs::write(&path, svg).unwrap();
        let err = load_on_ui_stack(&path).unwrap_err();
        assert!(err.contains("SVG"), "{name}: {err}");
        assert!(rasterize_svg_fit(&path, 64, 64).is_err(), "{name}");
    }
    // 1000 levels still open (usvg and Qt accept them), even from a 1 MB stack.
    let path = dir.join("deep_1000.svg");
    std::fs::write(&path, nested(1000)).unwrap();
    assert_eq!(load_on_ui_stack(&path), Ok((100, 100)));
    let img = rasterize_svg_fit(&path, 64, 64).unwrap();
    assert_eq!(img.dimensions(), (64, 64));
    assert_eq!(red_count(&img), 32 * 32);
}

#[test]
fn linked_images_are_local_only() {
    let dir = temp_dir("svg-links");
    let red = RgbaImage::from_pixel(4, 4, Rgba([255, 0, 0, 255]));
    red.save(dir.join("red.png")).unwrap();
    let svg = |href: &str| {
        format!(
            r##"{H}<rect width="100" height="100" fill="#303030"/><image width="100" height="100" xlink:href="{href}"/></svg>"##
        )
    };
    std::fs::write(dir.join("relative.svg"), svg("red.png")).unwrap();
    let abs = dir.join("red.png");
    std::fs::write(dir.join("absolute.svg"), svg(&abs.to_string_lossy())).unwrap();
    // The same file through the loopback admin share: an SMB connection.
    let s = abs.to_string_lossy();
    let unc = format!(r"\\127.0.0.1\{}${}", &s[..1], &s[2..]);
    std::fs::write(dir.join("unc.svg"), svg(&unc)).unwrap();

    let red_of = |name: &str| red_count(&rasterize_svg_fit(&dir.join(name), 50, 50).unwrap());
    assert_eq!(red_of("relative.svg"), 50 * 50);
    assert_eq!(red_of("absolute.svg"), 50 * 50);
    assert_eq!(red_of("unc.svg"), 0);

    // The viewer resolves relative links against the SVG's folder, not the working dir.
    let mut h = Harness::new(300.0, 300.0);
    let mut v = ImageViewer::new();
    v.load(&dir.join("relative.svg")).unwrap();
    assert!(h.run_until(
        &mut v,
        Duration::from_secs(10),
        ImageViewer::show,
        ImageViewer::is_ready
    ));
    // Base raster: 100 x 100 user units at 4x.
    let base = &h.uploads_of_size([400, 400])[0];
    let reds = base
        .pixels
        .iter()
        .filter(|p| p.r() > 200 && p.g() < 60)
        .count();
    assert_eq!(reds, 400 * 400);
}

#[test]
fn text_gets_fonts_with_prefixes_and_gzip() {
    let dir = temp_dir("svg-text");
    let prefixed = r##"<svg:svg xmlns:svg="http://www.w3.org/2000/svg" width="200" height="100">
        <svg:rect width="200" height="100" fill="#303030"/>
        <svg:text x="8" y="72" font-family="Arial" font-size="64" fill="#ffffff">ABC</svg:text></svg:svg>"##;
    std::fs::write(dir.join("prefixed.svg"), prefixed).unwrap();
    std::fs::write(dir.join("prefixed.svgz"), gzip_stored(prefixed.as_bytes())).unwrap();
    for name in ["prefixed.svg", "prefixed.svgz"] {
        let img = rasterize_svg_fit(&dir.join(name), 200, 100).unwrap();
        let white = img.pixels().filter(|p| p[0] > 200 && p[1] > 200).count();
        assert!(white > 500, "{name}: text missing ({white} white px)");
    }
}

/// gzip of `data` with uncompressed deflate blocks.
fn gzip_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
    let chunks: Vec<&[u8]> = data.chunks(0xffff).collect();
    for (i, chunk) in chunks.iter().enumerate() {
        out.push(u8::from(i + 1 == chunks.len()));
        let len = chunk.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    out.extend_from_slice(&(!crc).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}
