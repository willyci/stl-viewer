//! Hardened SVG loading shared by the image viewer and the folder-grid thumbnailer.
//!
//! usvg recurses without a bound over nested elements and over references between elements
//! (clip paths, masks, patterns, filters, markers, `use`, nested SVG images), and never ends
//! on reference cycles longer than two, so a few KB of hostile SVG overflow any stack and
//! abort the process. Every document is therefore scanned iteratively first and refused when
//! its nesting plus reference depth exceeds `MAX_DEPTH` or its references form a cycle; the
//! tree is then parsed, rendered and dropped only on threads with a `STACK_SIZE` stack.
//! `<image>` hrefs resolve against the SVG's folder and never reach the network.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::{Arc, OnceLock};

use image::RgbaImage;
use resvg::usvg::roxmltree::{self, Node, NodeId};
use resvg::{tiny_skia, usvg};

/// Longest chain of nested elements and followed references accepted, summed over nested
/// SVG images (usvg itself refuses more than 1024 nested elements).
pub(crate) const MAX_DEPTH: u32 = 1024;

/// Stack of every thread that parses, renders or drops a `usvg::Tree`. Only reserved: pages
/// are committed as the recursion actually uses them.
pub(crate) const STACK_SIZE: usize = 64 << 20;

/// Render the SVG at `path` scaled (up or down) to fit within `max_w` x `max_h`, keeping its
/// aspect ratio, as straight-alpha RGBA. Blocks while a large-stack thread does the work, so
/// it is safe to call from any thread.
pub fn rasterize_svg_fit(path: &Path, max_w: u32, max_h: u32) -> Result<RgbaImage, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    on_svg_stack(|| {
        let text = svg_text(&data)?;
        let doc = SvgDoc::parse(&text, path.parent())?;
        let tree = doc.tree(doc.has_text())?;
        fit_raster(&tree, max_w, max_h)
    })
}

/// Intrinsic size of the SVG at `path` (user units), parsed with the same safeguards.
pub fn svg_size(path: &Path) -> Result<(f32, f32), String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    on_svg_stack(|| {
        let text = svg_text(&data)?;
        let doc = SvgDoc::parse(&text, path.parent())?;
        let tree = doc.tree(false)?;
        Ok((tree.size().width(), tree.size().height()))
    })
}

/// Run `f` on a scoped thread with a `STACK_SIZE` stack and wait for it; a panic becomes an
/// error.
pub(crate) fn on_svg_stack<T: Send>(
    f: impl FnOnce() -> Result<T, String> + Send,
) -> Result<T, String> {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .name("stlv-svg".into())
            .stack_size(STACK_SIZE)
            .spawn_scoped(s, f)
            .map_err(|e| format!("Failed to start SVG parser: {e}"))?
            .join()
            .unwrap_or_else(|_| Err("SVG parser crashed".into()))
    })
}

fn fit_raster(tree: &usvg::Tree, max_w: u32, max_h: u32) -> Result<RgbaImage, String> {
    let (w, h) = (tree.size().width(), tree.size().height());
    let scale = (max_w as f32 / w).min(max_h as f32 / h);
    if !scale.is_finite() || scale <= 0.0 {
        return Err(format!("unusable SVG size {w}x{h}"));
    }
    let px = |len: f32, max: u32| ((len * scale).round() as u32).clamp(1, max);
    let (pw, ph) = (px(w, max_w), px(h, max_h));
    let mut pixmap = tiny_skia::Pixmap::new(pw, ph).ok_or("cannot allocate SVG pixmap")?;
    resvg::render(
        tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let rgba = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    RgbaImage::from_raw(pw, ph, rgba).ok_or_else(|| "bad SVG pixmap".into())
}

/// SVG text of plain or gzip-compressed (svgz) data.
pub(crate) fn svg_text(data: &[u8]) -> Result<Cow<'_, str>, String> {
    const NOT_UTF8: &str = "Failed to parse SVG: not UTF-8 text";
    if data.starts_with(&[0x1f, 0x8b]) {
        let raw = usvg::decompress_svgz(data).map_err(|e| format!("Failed to parse SVG: {e}"))?;
        String::from_utf8(raw)
            .map(Cow::Owned)
            .map_err(|_| NOT_UTF8.into())
    } else {
        std::str::from_utf8(data)
            .map(Cow::Borrowed)
            .map_err(|_| NOT_UTF8.into())
    }
}

/// Parse XML whose elements nest at most `budget` deep.
fn parse_xml(text: &str, budget: u32) -> Result<roxmltree::Document<'_>, String> {
    if markup_depth(text) > budget {
        return Err(too_deep());
    }
    let opt = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    roxmltree::Document::parse_with_options(text, opt)
        .map_err(|e| format!("Failed to parse SVG: {e}"))
}

/// How deep elements nest in `xml`, found without building a tree (roxmltree recurses once
/// per nested element). Markup inside DTD entity values counts as if expanded ten times
/// over, roxmltree's entity nesting limit. Malformed input is left to the parser.
fn markup_depth(xml: &str) -> u32 {
    let b = xml.as_bytes();
    // Index just past the next `pat` at or after `from`.
    let past = |from: usize, pat: &[u8]| {
        b.get(from..)
            .and_then(|rest| rest.windows(pat.len()).position(|w| w == pat))
            .map_or(b.len(), |p| from + p + pat.len())
    };
    let (mut depth, mut max, mut entities) = (0u32, 0u32, 0u32);
    let mut i = 0;
    while let Some(p) = b[i..].iter().position(|&c| c == b'<') {
        i += p;
        let rest = &b[i..];
        if rest.starts_with(b"<!--") {
            i = past(i + 4, b"-->");
        } else if rest.starts_with(b"<![CDATA[") {
            i = past(i + 9, b"]]>");
        } else if rest.starts_with(b"<?") {
            i = past(i + 2, b"?>");
        } else if rest.starts_with(b"<!") {
            // DOCTYPE: skip to its `>` outside quotes, comments and the `[...]` subset;
            // quoted strings in it are entity values.
            let (mut quote, mut subset) = (None, false);
            let mut start = i;
            i += 2;
            while i < b.len() {
                let c = b[i];
                match quote {
                    Some(q) if c == q => {
                        quote = None;
                        entities = entities.max(markup_depth(&xml[start..i]));
                    }
                    Some(_) => {}
                    None if b[i..].starts_with(b"<!--") => {
                        i = past(i + 4, b"-->");
                        continue;
                    }
                    None if b[i..].starts_with(b"<?") => {
                        i = past(i + 2, b"?>");
                        continue;
                    }
                    None => match c {
                        b'"' | b'\'' => {
                            quote = Some(c);
                            start = i + 1;
                        }
                        b'[' => subset = true,
                        b']' => subset = false,
                        b'>' if !subset => break,
                        _ => {}
                    },
                }
                i += 1;
            }
        } else if rest.starts_with(b"</") {
            depth = depth.saturating_sub(1);
            i = past(i, b">");
        } else {
            // Start tag. Attribute values may hold `>` but not `<`.
            let mut quote = None;
            let mut end = None;
            for (k, &c) in rest.iter().enumerate().skip(1) {
                match (quote, c) {
                    (_, b'<') => break,
                    (Some(q), _) if c == q => quote = None,
                    (Some(_), _) => {}
                    (None, b'"' | b'\'') => quote = Some(c),
                    (None, b'>') => {
                        end = Some(k);
                        break;
                    }
                    _ => {}
                }
            }
            match end {
                Some(k) => {
                    if rest[k - 1] != b'/' {
                        depth += 1;
                        max = max.max(depth);
                    }
                    i += k + 1;
                }
                None => i += 1,
            }
        }
    }
    max.saturating_add(entities.saturating_mul(10))
}

/// A parsed SVG document that passed `scan`.
pub(crate) struct SvgDoc<'t> {
    doc: roxmltree::Document<'t>,
    scan: Scan,
    /// Folder relative `<image>` hrefs resolve against (absolute when possible).
    dir: Option<PathBuf>,
}

impl<'t> SvgDoc<'t> {
    /// Parse and check `text`; `dir` is the SVG's folder. Recurses as deep as `MAX_DEPTH`
    /// allows: call on a `STACK_SIZE` thread.
    pub fn parse(text: &'t str, dir: Option<&Path>) -> Result<Self, String> {
        let doc = parse_xml(text, MAX_DEPTH)?;
        let scan = scan(&doc, MAX_DEPTH)?;
        let dir = dir.map(|d| std::path::absolute(d).unwrap_or_else(|_| d.to_path_buf()));
        Ok(Self { doc, scan, dir })
    }

    /// Whether text elements need the (slow to load) system fonts.
    pub fn has_text(&self) -> bool {
        self.scan.has_text
    }

    /// Convert to a render tree, with the system fonts when `fonts`. Call on a `STACK_SIZE`
    /// thread, and drop the tree on one too.
    pub fn tree(&self, fonts: bool) -> Result<usvg::Tree, String> {
        let mut opt = usvg::Options {
            image_href_resolver: href_resolver(self.dir.clone(), MAX_DEPTH - self.scan.depth),
            ..Default::default()
        };
        if fonts {
            opt.fontdb = system_fonts();
        }
        usvg::Tree::from_xmltree(&self.doc, &opt).map_err(|e| format!("Failed to parse SVG: {e}"))
    }
}

fn system_fonts() -> Arc<usvg::fontdb::Database> {
    static DB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    })
    .clone()
}

/// `<image>` / `<feImage>` href resolver. Data URLs may hold nested SVGs, which get the
/// remaining depth `budget`; strings are local files under `dir` (none for nested SVGs, as
/// in usvg).
fn href_resolver(dir: Option<PathBuf>, budget: u32) -> usvg::ImageHrefResolver<'static> {
    usvg::ImageHrefResolver {
        resolve_data: Box::new(move |mime, data, opt| match mime {
            "image/svg+xml" => nested_svg(&data, opt, budget),
            "text/plain" => raster(&data).or_else(|| nested_svg(&data, opt, budget)),
            "image/jpg" | "image/jpeg" => Some(usvg::ImageKind::JPEG(data)),
            "image/png" => Some(usvg::ImageKind::PNG(data)),
            "image/gif" => Some(usvg::ImageKind::GIF(data)),
            "image/webp" => Some(usvg::ImageKind::WEBP(data)),
            _ => None,
        }),
        resolve_string: match dir {
            Some(dir) => Box::new(move |href, opt| {
                let Some(path) = local_path(&dir, href) else {
                    log::warn!("SVG image '{href}' is not a local file. Skipped.");
                    return None;
                };
                let data = std::fs::read(&path)
                    .inspect_err(|e| log::warn!("SVG image '{}': {e}", path.display()))
                    .ok()?;
                let ext = stlv_core::files::ext_lower(&path);
                if ext == "svg" || ext == "svgz" {
                    nested_svg(&data, opt, budget)
                } else {
                    raster(&Arc::new(data))
                }
            }),
            None => Box::new(|_, _| None),
        },
    }
}

/// Raster image kind sniffed from the magic bytes.
fn raster(data: &Arc<Vec<u8>>) -> Option<usvg::ImageKind> {
    let d = data.as_slice();
    let data = data.clone();
    if d.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(usvg::ImageKind::PNG(data))
    } else if d.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(usvg::ImageKind::JPEG(data))
    } else if d.starts_with(b"GIF87a") || d.starts_with(b"GIF89a") {
        Some(usvg::ImageKind::GIF(data))
    } else if d.len() >= 12 && d.starts_with(b"RIFF") && &d[8..12] == b"WEBP" {
        Some(usvg::ImageKind::WEBP(data))
    } else {
        None
    }
}

/// An SVG used as an image inside another, checked against what is left of the depth budget.
fn nested_svg(data: &[u8], opt: &usvg::Options, budget: u32) -> Option<usvg::ImageKind> {
    let text = svg_text(data).ok()?;
    let doc = parse_xml(&text, budget)
        .inspect_err(|e| log::warn!("Nested SVG image skipped: {e}"))
        .ok()?;
    let scan = scan(&doc, budget)
        .inspect_err(|e| log::warn!("Nested SVG image skipped: {e}"))
        .ok()?;
    let nested = usvg::Options {
        dpi: opt.dpi,
        font_size: opt.font_size,
        languages: opt.languages.clone(),
        shape_rendering: opt.shape_rendering,
        text_rendering: opt.text_rendering,
        image_rendering: opt.image_rendering,
        default_size: opt.default_size,
        fontdb: if scan.has_text {
            system_fonts()
        } else {
            opt.fontdb.clone()
        },
        image_href_resolver: href_resolver(None, budget - scan.depth),
        ..Default::default()
    };
    usvg::Tree::from_xmltree(&doc, &nested)
        .ok()
        .map(usvg::ImageKind::SVG)
}

/// `href` as a local file path resolved against `dir`. URLs and UNC / device paths are
/// refused: merely showing a downloaded SVG must not make Windows connect to (and send the
/// user's NTLM credentials to) another host. Paths on the SVG's own share stay allowed.
fn local_path(dir: &Path, href: &str) -> Option<PathBuf> {
    let b = href.as_bytes();
    let sep = |c: u8| c == b'\\' || c == b'/';
    // `\\server\share`, `//server/share`, `\\?\`, `\\.\`, `\??\`.
    if b.len() >= 2 && sep(b[0]) && (sep(b[1]) || b[1] == b'?') {
        return None;
    }
    // `http:`, `file:` and other URL schemes, but not drive letters (`C:`).
    if let Some(colon) = href.find(':')
        && colon > 1
        && href[..colon]
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'))
    {
        return None;
    }
    let path = dir.join(href);
    fn prefix(p: &Path) -> Option<Prefix<'_>> {
        match p.components().next() {
            Some(Component::Prefix(p)) => Some(p.kind()),
            _ => None,
        }
    }
    match prefix(&path) {
        None | Some(Prefix::Disk(_) | Prefix::VerbatimDisk(_)) => Some(path),
        p if p == prefix(dir) => Some(path),
        _ => None,
    }
}

/// What `scan` learned about a document.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Scan {
    /// Longest chain of nested elements and references from the root element.
    pub depth: u32,
    pub has_text: bool,
}

/// Properties (attributes or CSS) whose `url(#id)` usvg follows.
const URL_PROPS: [&str; 9] = [
    "clip-path",
    "fill",
    "filter",
    "marker",
    "marker-end",
    "marker-mid",
    "marker-start",
    "mask",
    "stroke",
];

/// Elements whose `href="#id"` usvg follows (`<a>` links, for one, it does not).
const HREF_ELEMENTS: [&str; 8] = [
    "feImage",
    "filter",
    "linearGradient",
    "pattern",
    "radialGradient",
    "textPath",
    "tref",
    "use",
];

const IN_PROGRESS: u32 = u32::MAX;

/// Measure how deep usvg would recurse into `doc`, without recursing: the longest path from
/// the root element over child links and followed references. Fails when it exceeds
/// `budget` or when references form a cycle.
pub(crate) fn scan(doc: &roxmltree::Document, budget: u32) -> Result<Scan, String> {
    let mut ids = HashMap::new();
    for n in doc.descendants().filter(Node::is_element) {
        if let Some(id) = n.attribute("id") {
            ids.entry(id).or_insert(n);
        }
    }
    let css = StyleRefs::new(doc, &ids);
    let refs = |n: Node<'_, '_>, out: &mut Vec<usize>| {
        let mut add = |id: &str| {
            if let Some(t) = ids.get(id) {
                out.push(t.id().get_usize());
            }
        };
        for a in n.attributes() {
            let name = a.name();
            if name == "style" || URL_PROPS.contains(&name) {
                url_ids(a.value()).for_each(&mut add);
            } else if name == "href"
                && let Some(id) = a.value().strip_prefix('#')
                && HREF_ELEMENTS.contains(&n.tag_name().name())
            {
                add(id);
            }
        }
        css.targets(n, out);
    };

    // Iterative DFS; `cost[n]` is 0 (unvisited), IN_PROGRESS, or the depth below `n`.
    let mut cost = vec![0u32; doc.descendants().count()];
    let mut has_text = false;
    let edges = |n: Node<'_, '_>| {
        let mut out: Vec<usize> = n
            .children()
            .filter(Node::is_element)
            .map(|c| c.id().get_usize())
            .collect();
        refs(n, &mut out);
        out
    };
    let root = doc.root_element();
    // (node, its edges, next edge, deepest edge so far)
    let mut stack = vec![(root.id().get_usize(), edges(root), 0, 0u32)];
    cost[root.id().get_usize()] = IN_PROGRESS;
    let mut depth = 0;
    while let Some((n, out, next, best)) = stack.last_mut() {
        if let Some(&e) = out.get(*next) {
            *next += 1;
            match cost[e] {
                0 => {
                    if stack.len() >= budget as usize {
                        return Err(too_deep());
                    }
                    let node = doc.get_node(NodeId::new(e as u32)).ok_or("bad SVG node")?;
                    cost[e] = IN_PROGRESS;
                    has_text |= node.tag_name().name() == "text";
                    stack.push((e, edges(node), 0, 0));
                }
                IN_PROGRESS => return Err("SVG has circular references".into()),
                c => *best = (*best).max(c),
            }
        } else {
            let c = *best + 1;
            if c > budget {
                return Err(too_deep());
            }
            cost[*n] = c;
            stack.pop();
            match stack.last_mut() {
                Some((_, _, _, best)) => *best = (*best).max(c),
                None => depth = c,
            }
        }
    }
    Ok(Scan { depth, has_text })
}

fn too_deep() -> String {
    format!("SVG is nested too deeply (limit {MAX_DEPTH})")
}

/// The ids of every `url(#id)` in `value`.
fn url_ids(value: &str) -> impl Iterator<Item = &str> {
    value.match_indices("url(").filter_map(move |(i, _)| {
        let rest = value[i + 4..].trim_start().trim_start_matches(['"', '\'']);
        let id = rest.strip_prefix('#')?;
        let end = id
            .find(|c: char| c == ')' || c == '"' || c == '\'' || c.is_whitespace())
            .unwrap_or(id.len());
        (end > 0).then(|| &id[..end])
    })
}

/// References made by `<style>` sheets, indexed by a superset of the elements each rule
/// can match: the most specific part of the rule's last compound selector (`g > .a:hover`
/// matches at most the elements of class `a`).
#[derive(Default)]
struct StyleRefs {
    any: Vec<usize>,
    by_tag: HashMap<String, Vec<usize>>,
    by_class: HashMap<String, Vec<usize>>,
    by_id: HashMap<String, Vec<usize>>,
}

impl StyleRefs {
    fn new(doc: &roxmltree::Document, ids: &HashMap<&str, Node>) -> Self {
        let mut refs = Self::default();
        for style in doc.descendants().filter(|n| n.tag_name().name() == "style") {
            if style.attribute("type").is_some_and(|t| t != "text/css") {
                continue;
            }
            let css = strip_comments(style.text().unwrap_or(""));
            for (selectors, block) in css_rules(&css) {
                for decl in block.split(';') {
                    let Some((prop, value)) = decl.split_once(':') else {
                        continue;
                    };
                    if !URL_PROPS.contains(&prop.trim()) {
                        continue;
                    }
                    for id in url_ids(value) {
                        let Some(t) = ids.get(id) else { continue };
                        for sel in selectors.split(',') {
                            refs.add(sel, t.id().get_usize());
                        }
                    }
                }
            }
        }
        refs
    }

    fn add(&mut self, selector: &str, target: usize) {
        // The last compound selector, without attribute selectors and pseudo-class arguments.
        let mut last = String::new();
        let (mut nest, mut combinator) = (0u32, false);
        for c in selector.chars() {
            if nest == 0 {
                if c.is_whitespace() || matches!(c, '>' | '+' | '~') {
                    combinator = true;
                    continue;
                }
                if combinator {
                    last.clear();
                    combinator = false;
                }
            }
            match c {
                '[' | '(' => nest += 1,
                ']' | ')' => nest = nest.saturating_sub(1),
                c if nest == 0 => last.push(c),
                _ => {}
            }
        }
        let ident = |s: &str| {
            let end = s
                .find(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
                .unwrap_or(s.len());
            s[..end].to_owned()
        };
        let list = if let Some(i) = last.find('#') {
            self.by_id.entry(ident(&last[i + 1..])).or_default()
        } else if let Some(i) = last.find('.') {
            self.by_class.entry(ident(&last[i + 1..])).or_default()
        } else {
            match ident(&last) {
                tag if tag.is_empty() => &mut self.any,
                tag => self.by_tag.entry(tag).or_default(),
            }
        };
        list.push(target);
    }

    fn targets(&self, n: Node, out: &mut Vec<usize>) {
        out.extend_from_slice(&self.any);
        fn get<'m>(map: &'m HashMap<String, Vec<usize>>, key: &str) -> &'m [usize] {
            map.get(key).map(Vec::as_slice).unwrap_or_default()
        }
        out.extend_from_slice(get(&self.by_tag, n.tag_name().name()));
        if let Some(id) = n.attribute("id") {
            out.extend_from_slice(get(&self.by_id, id));
        }
        for class in n.attribute("class").unwrap_or("").split_whitespace() {
            out.extend_from_slice(get(&self.by_class, class));
        }
    }
}

fn strip_comments(css: &str) -> Cow<'_, str> {
    if !css.contains("/*") {
        return Cow::Borrowed(css);
    }
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        rest = rest[i + 2..]
            .find("*/")
            .map_or("", |j| &rest[i + 2 + j + 2..]);
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// `(selectors, declarations)` of each rule set; at-rules are skipped like usvg's CSS
/// parser does.
fn css_rules(css: &str) -> Vec<(&str, &str)> {
    let mut rules = Vec::new();
    let mut rest = css;
    while let Some(open) = rest.find(['{', ';']) {
        let prelude = rest[..open].trim();
        if rest.as_bytes()[open] == b';' {
            // `@import ...;` or stray text.
            rest = &rest[open + 1..];
            continue;
        }
        let mut depth = 0;
        let mut close = rest.len();
        for (i, c) in rest[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = open + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        if !prelude.starts_with('@') {
            rules.push((prelude, &rest[open + 1..close]));
        }
        rest = rest.get(close + 1..).unwrap_or("");
    }
    rules
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="100" height="100">"#;

    fn scan_str(body: &str) -> Result<Scan, String> {
        let text = format!("{H}{body}</svg>");
        scan(&parse_xml(&text, MAX_DEPTH)?, MAX_DEPTH)
    }

    fn nested(n: usize) -> String {
        format!(
            "{}<rect width=\"5\" height=\"5\"/>{}",
            "<g>".repeat(n),
            "</g>".repeat(n)
        )
    }

    #[test]
    fn nesting_depth_is_limited() {
        assert_eq!(scan_str(&nested(10)).unwrap().depth, 12);
        assert!(scan_str(&nested(1000)).is_ok());
        let err = scan_str(&nested(5000)).unwrap_err();
        assert!(err.contains("too deeply"), "{err}");
    }

    #[test]
    fn markup_depth_without_parsing() {
        assert_eq!(markup_depth("<a><b/><c x='>' y=\"/>\"><d></d></c></a>"), 3);
        assert_eq!(
            markup_depth("<a><!-- <b><b> --><![CDATA[<c><c>]]><?pi <d>?></a>"),
            1
        );
        // Markup in entity values counts ten times.
        let dtd = r#"<!DOCTYPE svg [ <!ENTITY e "<g><g>&f;</g></g>"> ]><svg>&e;</svg>"#;
        assert_eq!(markup_depth(dtd), 1 + 2 * 10);
        // A quote in a DTD comment does not hide the document.
        let doc = format!("<!DOCTYPE svg [ <!-- ' --> ]>{}", nested(50));
        assert_eq!(markup_depth(&doc), 50);
        // roxmltree itself recurses per element: deep input never reaches it.
        let text = format!("{H}{}</svg>", nested(100_000));
        assert!(
            parse_xml(&text, MAX_DEPTH)
                .unwrap_err()
                .contains("too deeply")
        );
    }

    #[test]
    fn reference_chains_count() {
        let chain = |n: usize| {
            let defs: String = (0..n)
                .map(|i| {
                    format!(
                        r#"<clipPath id="c{i}"><rect width="9" height="9" clip-path="url(#c{})"/></clipPath>"#,
                        i + 1
                    )
                })
                .collect();
            format!(
                r#"<defs>{defs}<clipPath id="c{n}"><rect width="8" height="8"/></clipPath></defs><rect width="9" height="9" clip-path="url(#c0)"/>"#
            )
        };
        // svg > rect -> (clipPath > rect) x n -> clipPath > rect
        assert_eq!(scan_str(&chain(3)).unwrap().depth, 2 + 2 * 3 + 2);
        assert!(scan_str(&chain(2000)).is_err());
    }

    #[test]
    fn reference_cycles_are_refused() {
        for body in [
            // Attributes, a 3-cycle (usvg only breaks 1- and 2-cycles).
            r#"<clipPath id="a"><rect clip-path="url(#b)"/></clipPath><clipPath id="b"><rect clip-path="url(#c)"/></clipPath><clipPath id="c"><rect clip-path="url(#a)"/></clipPath><rect clip-path="url(#a)"/>"#,
            // Inline style, quoted.
            r#"<mask id="m"><rect style="mask: url('#m')"/></mask>"#,
            // feImage -> element -> filter.
            r##"<filter id="f"><feImage xlink:href="#e"/></filter><g id="e"><rect filter="url(#f)"/></g>"##,
            // Stylesheet.
            r#"<style>.x { fill: url(#p) } /* .y { fill: url(#q) } */</style><pattern id="p"><rect class="x"/></pattern>"#,
            r#"<style><![CDATA[ g > rect:first-child, .z { clip-path:url(#c) } ]]></style><clipPath id="c"><rect/></clipPath>"#,
        ] {
            let err = scan_str(body).unwrap_err();
            assert!(err.contains("circular"), "{body}: {err}");
        }
    }

    #[test]
    fn harmless_references_pass() {
        for body in [
            // Hyperlinks to an ancestor are not followed by usvg.
            r##"<g id="top"><a xlink:href="#top"><rect width="5" height="5"/></a></g>"##,
            // Rules in at-rules and comments are ignored, as by usvg.
            r#"<style>@media print { .x { fill: url(#p) } } /* .x { fill: url(#p) } */</style><pattern id="p"><rect class="x"/></pattern>"#,
            // Illustrator-style class fills.
            r#"<style>.st0{fill:url(#g1);} .st1{fill:url(#p1)}</style><linearGradient id="g1"><stop offset="0"/></linearGradient><pattern id="p1"><rect class="st0"/></pattern><rect class="st1"/>"#,
            // References to missing ids.
            r#"<rect fill="url(#nope)" clip-path="url(#)"/>"#,
        ] {
            assert!(scan_str(body).is_ok(), "{body}");
        }
    }

    #[test]
    fn text_is_detected() {
        assert!(scan_str("<text>hi</text>").unwrap().has_text);
        assert!(!scan_str("<rect/>").unwrap().has_text);
        let prefixed = r#"<svg:svg xmlns:svg="http://www.w3.org/2000/svg"><svg:g><svg:text>A</svg:text></svg:g></svg:svg>"#;
        assert!(scan(&parse_xml(prefixed, 9).unwrap(), 9).unwrap().has_text);
    }

    #[test]
    fn css_selectors_key_on_last_compound() {
        let mut refs = StyleRefs::default();
        refs.add(" g > rect.a:not(.b) ", 1);
        refs.add("svg #i[fill='#x']", 2);
        refs.add("circle", 3);
        refs.add("*", 4);
        refs.add(":first-child", 5);
        refs.add("g [x]", 6);
        assert_eq!(refs.by_class["a"], [1]);
        assert_eq!(refs.by_id["i"], [2]);
        assert_eq!(refs.by_tag["circle"], [3]);
        assert_eq!(refs.any, [4, 5, 6]);
        assert_eq!(
            css_rules("@import 'x'; a{fill:red} @media x { b { c:d } } e,f{}"),
            [("a", "fill:red"), ("e,f", "")]
        );
        assert_eq!(
            url_ids("url( \"#a\" ) url(#b) url(c) url(#)").collect::<Vec<_>>(),
            ["a", "b"]
        );
    }

    #[test]
    fn hrefs_stay_local() {
        let dir = Path::new(r"C:\svgs");
        assert_eq!(
            local_path(dir, "img/a.png"),
            Some(PathBuf::from(r"C:\svgs\img/a.png"))
        );
        assert_eq!(
            local_path(dir, r"D:\x.png"),
            Some(PathBuf::from(r"D:\x.png"))
        );
        assert_eq!(
            local_path(dir, r"..\x.png"),
            Some(PathBuf::from(r"C:\svgs\..\x.png"))
        );
        for href in [
            r"\\attacker\share\x.png",
            "//attacker/share/x.png",
            r"\\?\UNC\attacker\share\x.png",
            r"\\.\pipe\x",
            r"\??\UNC\attacker\share\x.png",
            "http://attacker/x.png",
            "file://attacker/share/x.png",
            "file:///C:/x.png",
        ] {
            assert_eq!(local_path(dir, href), None, "{href}");
        }
        // An SVG on a share may use files next to it.
        let share = Path::new(r"\\server\share\svgs");
        assert_eq!(
            local_path(share, "a.png"),
            Some(PathBuf::from(r"\\server\share\svgs\a.png"))
        );
        assert_eq!(local_path(share, r"\\other\share\a.png"), None);
    }
}
