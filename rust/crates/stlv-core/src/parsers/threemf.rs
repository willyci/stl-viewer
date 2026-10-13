use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use roxmltree::{Document, Node, ParsingOptions};
use zip::ZipArchive;

use super::{LoadError, file_name};
use crate::format::thousands;
use crate::mesh::VertexDedup;
use crate::model::ModelData;
use crate::xml::DOM_ITEM_BYTES;

const DEFAULT_ROOT_MODEL: &str = "3D/3dmodel.model";
/// Largest decompressed XML part we read (roxmltree is limited to 4 GiB).
const MAX_PART_BYTES: u64 = 1 << 30;
/// Zip-bomb guard: real 3MF parts inflate 3-12x, so a package may inflate to this many times
/// its size in all (but at least [`MIN_INFLATE_BUDGET`]).
const INFLATE_RATIO: u64 = 64;
const MIN_INFLATE_BUDGET: u64 = 32 << 20;
/// roxmltree's DOM costs ~80 bytes per element and attribute (see [`crate::xml::bounds`]), which
/// real parts keep under ~120 bytes per compressed byte; a bomb of `<a/>` needs 40,000. A part
/// may use this many (within the clamp below).
const DOM_BYTES_PER_COMPRESSED_BYTE: u64 = 384;
const MIN_DOM_BUDGET: u64 = 64 << 20;
const MAX_DOM_BUDGET: u64 = 4 << 30;
/// Instanced assemblies can expand exponentially; refuse beyond this many emitted elements
/// (vertices, triangles and object instances).
const MAX_EMITTED: u64 = 50_000_000;

/// 3MF zip package: build items, component assemblies (incl. external model parts), transforms.
pub fn parse_3mf(path: &Path) -> Result<ModelData, LoadError> {
    let file = std::fs::File::open(path)?;
    parse_3mf_reader(BufReader::new(file), file_name(path))
}

/// [`parse_3mf`] on an in-memory file.
pub fn parse_3mf_bytes(data: &[u8], filename: String) -> Result<ModelData, LoadError> {
    parse_3mf_reader(Cursor::new(data), filename)
}

fn parse_3mf_reader<R: Read + Seek>(
    mut reader: R,
    filename: String,
) -> Result<ModelData, LoadError> {
    let archive_len = reader.seek(SeekFrom::End(0))?;
    let zip = ZipArchive::new(reader)
        .map_err(|_| LoadError::invalid("File is not a valid 3MF zip archive."))?;
    let mut package = Package::new(zip, archive_len);
    let root = package
        .root_model()
        .ok_or_else(|| LoadError::invalid("Could not locate 3D/3dmodel.model in 3MF archive."))?;

    let root_failed =
        |e: String| LoadError::Invalid(format!("Failed to read root XML inside 3MF model: {e}"));
    let part = package.read_part(&root).map_err(root_failed)?;
    let doc = part.document().map_err(root_failed)?;
    let model_el = doc.root_element();
    let mut resolver = Resolver::new(package);
    resolver.parsed.insert(root.clone());
    resolver.objects.add_model(&root, model_el);
    // Deviation: geometry is scaled to millimetres from the root model's unit.
    let to_mm = Affine::scale(unit_scale(attr(model_el, "unit")));
    let items: Vec<(String, Affine)> = child_elements(model_el, "build")
        .next()
        .into_iter()
        .flat_map(|build| child_elements(build, "item"))
        .filter_map(|item| {
            let id = attr(item, "objectid").filter(|id| !id.is_empty())?;
            Some((id.to_owned(), Affine::parse(attr(item, "transform"))))
        })
        .collect();
    drop(doc);

    let mut out = Output::default();
    for (id, transform) in &items {
        if let Some(obj) = resolver.lookup(&root, id) {
            resolver.emit(obj, transform.then(&to_mm), &mut out)?;
        }
    }

    // Python's fallbacks: top-level objects (not used as components), then everything.
    if out.dedup.is_empty() && !resolver.objects.defs.is_empty() {
        let referenced = resolver.component_targets();
        for obj in 0..resolver.objects.defs.len() {
            if !referenced.contains(&resolver.objects.keys[obj]) {
                resolver.emit(obj, to_mm, &mut out)?;
            }
        }
        if out.dedup.is_empty() {
            for obj in 0..resolver.objects.defs.len() {
                resolver.emit(obj, to_mm, &mut out)?;
            }
        }
    }
    if out.dedup.is_empty() {
        return Err(LoadError::invalid(
            "No displayable meshes or build items found in 3MF file.",
        ));
    }
    Ok(ModelData::new(
        out.dedup.into_vertices(),
        out.triangles,
        vec![],
        "3MF Model",
        filename,
    ))
}

/// A decoded XML part of the package.
struct Part {
    text: String,
    /// Most memory its DOM may take.
    dom_budget: u64,
}

impl Part {
    /// roxmltree with DTDs allowed (like ElementTree), after the [`crate::xml`] guards against
    /// deep nesting and oversized DOMs.
    fn document(&self) -> Result<Document<'_>, String> {
        crate::xml::check(self.text.as_bytes(), self.dom_budget)?;
        let options = ParsingOptions {
            allow_dtd: true,
            nodes_limit: u32::try_from(self.dom_budget / DOM_ITEM_BYTES).unwrap_or(u32::MAX),
            ..Default::default()
        };
        Document::parse_with_options(&self.text, options).map_err(|e| e.to_string())
    }
}

/// Element children with the given local name (namespaces ignored, case-sensitive like Python).
fn child_elements<'a, 'i>(
    node: Node<'a, 'i>,
    local_name: &'static str,
) -> impl Iterator<Item = Node<'a, 'i>> {
    node.children()
        .filter(move |c| c.is_element() && c.tag_name().name() == local_name)
}

/// Python `get_attrib_insensitive`: local attribute name compared case-insensitively.
fn attr<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|a| a.name().eq_ignore_ascii_case(name))
        .map(|a| a.value())
}

fn unit_scale(unit: Option<&str>) -> f64 {
    match unit.map(|u| u.trim().to_ascii_lowercase()).as_deref() {
        Some("micron") => 0.001,
        Some("centimeter") => 10.0,
        Some("inch") => 25.4,
        Some("foot") => 304.8,
        Some("meter") => 1000.0,
        _ => 1.0,
    }
}

/// Zip entry access with Python's lookup rules.
struct Package<R> {
    zip: ZipArchive<R>,
    /// `(name with '\' as '/', stored name)` per entry: Python's zipfile reads Windows-style
    /// stored names with forward slashes.
    names: Vec<(String, String)>,
    lookups: HashMap<String, Option<String>>,
    archive_len: u64,
    /// Bytes the remaining parts may still inflate to.
    inflate_budget: u64,
}

impl<R: Read + Seek> Package<R> {
    fn new(zip: ZipArchive<R>, archive_len: u64) -> Self {
        let names = zip
            .file_names()
            .map(|n| (n.replace('\\', "/"), n.to_owned()))
            .collect();
        Self {
            zip,
            names,
            lookups: HashMap::new(),
            archive_len,
            inflate_budget: archive_len
                .saturating_mul(INFLATE_RATIO)
                .max(MIN_INFLATE_BUDGET),
        }
    }

    /// Normalise `\` to `/`, strip leading `/`; exact match, else first case-insensitive match.
    fn find(&mut self, path: &str) -> Option<String> {
        if let Some(found) = self.lookups.get(path) {
            return found.clone();
        }
        let norm = path.replace('\\', "/");
        let norm = norm.trim_start_matches('/');
        let lower = norm.to_lowercase();
        let found = self
            .names
            .iter()
            .find(|(n, _)| n == norm)
            .or_else(|| self.names.iter().find(|(n, _)| n.to_lowercase() == lower))
            .map(|(_, stored)| stored.clone());
        self.lookups.insert(path.to_owned(), found.clone());
        found
    }

    /// Deviation: the root part named by `_rels/.rels` (type `.../3dmodel`), else the
    /// conventional `3D/3dmodel.model`.
    fn root_model(&mut self) -> Option<String> {
        let targets: Vec<String> = self
            .find("_rels/.rels")
            .and_then(|rels| self.read_part(&rels).ok())
            .map(|part| {
                let Ok(doc) = part.document() else {
                    return Vec::new();
                };
                child_elements(doc.root_element(), "Relationship")
                    .filter(|r| {
                        attr(*r, "type")
                            .is_some_and(|t| t.to_ascii_lowercase().ends_with("/3dmodel"))
                    })
                    .filter_map(|r| attr(r, "target").map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        targets
            .iter()
            .find_map(|t| self.find(t))
            .or_else(|| self.find(DEFAULT_ROOT_MODEL))
    }

    /// Inflates and decodes entry `name` within the package's inflate budget.
    fn read_part(&mut self, name: &str) -> Result<Part, String> {
        let entry = self.zip.by_name(name).map_err(|e| e.to_string())?;
        let limit = MAX_PART_BYTES.min(self.inflate_budget);
        let dom_budget = (entry.compressed_size().min(self.archive_len))
            .saturating_mul(DOM_BYTES_PER_COMPRESSED_BYTE)
            .clamp(MIN_DOM_BUDGET, MAX_DOM_BUDGET);
        // The declared size may lie; it only saves regrowing the buffer.
        let mut bytes = Vec::with_capacity(entry.size().min(limit) as usize + 1);
        entry
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > limit {
            return Err(format!(
                "{name} decompresses to more than {} bytes",
                thousands(limit)
            ));
        }
        self.inflate_budget -= bytes.len() as u64;
        Ok(Part {
            text: decode_xml_bytes(bytes)?,
            dom_budget,
        })
    }
}

/// UTF-8 (BOM handled by roxmltree), BOM-marked UTF-16, or a declared ISO-8859-1 /
/// windows-1252 part (non-conformant, but ElementTree honours the declaration).
fn decode_xml_bytes(bytes: Vec<u8>) -> Result<String, String> {
    let utf16 = |be: bool| {
        let units = bytes[2..].chunks_exact(2).map(|c| {
            if be {
                u16::from_be_bytes([c[0], c[1]])
            } else {
                u16::from_le_bytes([c[0], c[1]])
            }
        });
        char::decode_utf16(units)
            .collect::<Result<String, _>>()
            .map_err(|e| e.to_string())
    };
    match bytes.get(..2) {
        Some([0xFF, 0xFE]) => utf16(false),
        Some([0xFE, 0xFF]) => utf16(true),
        _ => match declared_encoding(&bytes).as_deref() {
            Some("iso-8859-1" | "latin-1" | "latin1" | "l1" | "iso_8859-1") => {
                Ok(bytes.iter().map(|&b| char::from(b)).collect())
            }
            Some("windows-1252" | "cp1252") => Ok(bytes.iter().map(|&b| cp1252(b)).collect()),
            _ => String::from_utf8(bytes).map_err(|e| e.to_string()),
        },
    }
}

/// Lower-cased `encoding` pseudo-attribute of a leading `<?xml ...?>` declaration.
fn declared_encoding(bytes: &[u8]) -> Option<String> {
    let rest = bytes.strip_prefix(b"<?xml")?;
    let decl = &rest[..rest.windows(2).take(512).position(|w| w == b"?>")?];
    let decl = std::str::from_utf8(decl).ok()?;
    let value = decl
        .split_once("encoding")?
        .1
        .trim_start()
        .strip_prefix('=')?;
    let value = value.trim_start();
    let quote = value.chars().next().filter(|q| matches!(q, '"' | '\''))?;
    let value = &value[1..];
    Some(value[..value.find(quote)?].trim().to_ascii_lowercase())
}

/// windows-1252 byte to char; its five unassigned bytes map to the C1 controls (as in WHATWG).
fn cp1252(b: u8) -> char {
    const HIGH: [u16; 32] = [
        0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160,
        0x2039, 0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013,
        0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
    ];
    match b {
        0x80..=0x9F => char::from_u32(u32::from(HIGH[usize::from(b - 0x80)])).unwrap_or('\u{FFFD}'),
        _ => char::from(b),
    }
}

/// Affine transform acting on ROW vectors: `p' = p · m + t` (3MF Core spec, §3.3).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Affine {
    m: [[f64; 3]; 3],
    t: [f64; 3],
}

impl Affine {
    const IDENTITY: Self = Self {
        m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        t: [0.0; 3],
    };

    fn scale(s: f64) -> Self {
        Self {
            m: [[s, 0.0, 0.0], [0.0, s, 0.0], [0.0, 0.0, s]],
            t: [0.0; 3],
        }
    }

    /// `"m00 m01 m02 m10 m11 m12 m20 m21 m22 m30 m31 m32"`; anything other than 12 finite
    /// numbers is the identity (like Python).
    fn parse(s: Option<&str>) -> Self {
        let Some(s) = s else { return Self::IDENTITY };
        let nums: Option<Vec<f64>> = s
            .split_whitespace()
            .map(|n| n.parse::<f64>().ok().filter(|v| v.is_finite()))
            .collect();
        match nums.as_deref() {
            Some(&[m00, m01, m02, m10, m11, m12, m20, m21, m22, m30, m31, m32]) => Self {
                m: [[m00, m01, m02], [m10, m11, m12], [m20, m21, m22]],
                t: [m30, m31, m32],
            },
            _ => Self::IDENTITY,
        }
    }

    /// Apply `self`, then `outer`.
    fn then(&self, outer: &Self) -> Self {
        let row = |v: [f64; 3]| {
            [0, 1, 2].map(|j| v[0] * outer.m[0][j] + v[1] * outer.m[1][j] + v[2] * outer.m[2][j])
        };
        let t = row(self.t);
        Self {
            m: self.m.map(row),
            t: [0, 1, 2].map(|j| t[j] + outer.t[j]),
        }
    }

    fn apply(&self, v: [f32; 3]) -> [f32; 3] {
        let [x, y, z] = v.map(f64::from);
        [0, 1, 2]
            .map(|j| (x * self.m[0][j] + y * self.m[1][j] + z * self.m[2][j] + self.t[j]) as f32)
    }
}

#[derive(Clone, Debug)]
struct Component {
    object_id: Option<String>,
    /// Production-extension `p:path`; `None` means the parent's model file.
    path: Option<String>,
    transform: Affine,
}

#[derive(Debug, Default)]
struct ObjectDef {
    vertices: Vec<[f32; 3]>,
    /// Only triangles whose indices are valid for `vertices`.
    triangles: Vec<[u32; 3]>,
    components: Vec<Component>,
}

/// Objects of every parsed model part, keyed by `(normalised part name, object id)`, in first
/// definition order (a redefinition replaces the object in place, like a Python dict).
#[derive(Default)]
struct Objects {
    keys: Vec<(String, String)>,
    defs: Vec<ObjectDef>,
    index: HashMap<String, HashMap<String, usize>>,
}

impl Objects {
    fn get(&self, part: &str, id: &str) -> Option<usize> {
        self.index.get(part)?.get(id).copied()
    }

    fn add_model(&mut self, part: &str, model_el: Node) {
        let Some(resources) = child_elements(model_el, "resources").next() else {
            return;
        };
        for obj in child_elements(resources, "object") {
            let Some(id) = attr(obj, "id").filter(|id| !id.is_empty()) else {
                continue;
            };
            let def = parse_object(obj);
            match self.get(part, id) {
                Some(i) => self.defs[i] = def,
                None => {
                    let i = self.defs.len();
                    self.index
                        .entry(part.to_owned())
                        .or_default()
                        .insert(id.to_owned(), i);
                    self.keys.push((part.to_owned(), id.to_owned()));
                    self.defs.push(def);
                }
            }
        }
    }
}

fn parse_object(obj: Node) -> ObjectDef {
    let mut def = ObjectDef::default();
    let mut raw_triangles: Vec<[u32; 3]> = Vec::new();
    for details in obj.children().filter(Node::is_element) {
        match details.tag_name().name() {
            "mesh" => {
                // Python keeps the last <vertices> / <triangles> of each <mesh>.
                let last = |name: &str| {
                    details
                        .children()
                        .rfind(|c| c.is_element() && c.tag_name().name() == name)
                };
                if let Some(vertices) = last("vertices") {
                    // Deviation: an unparsable coordinate reads as 0 instead of failing the file.
                    let coord = |v: Node, name: &str| {
                        attr(v, name).map_or(0.0, |s| s.trim().parse::<f64>().unwrap_or(0.0)) as f32
                    };
                    let vs = child_elements(vertices, "vertex")
                        .map(|v| [coord(v, "x"), coord(v, "y"), coord(v, "z")]);
                    def.vertices.extend(vs);
                }
                if let Some(triangles) = last("triangles") {
                    // Missing indices read as 0 (Python); unparsable or negative ones drop the triangle.
                    let index = |t: Node, name: &str| {
                        attr(t, name).map_or(Some(0), |s| s.trim().parse::<u32>().ok())
                    };
                    let ts = child_elements(triangles, "triangle")
                        .filter_map(|t| Some([index(t, "v1")?, index(t, "v2")?, index(t, "v3")?]));
                    raw_triangles.extend(ts);
                }
            }
            "components" => {
                let comps = child_elements(details, "component").map(|c| Component {
                    object_id: attr(c, "objectid").map(str::to_owned),
                    path: attr(c, "path").filter(|p| !p.is_empty()).map(str::to_owned),
                    transform: Affine::parse(attr(c, "transform")),
                });
                def.components.extend(comps);
            }
            _ => {}
        }
    }
    let count = def.vertices.len();
    def.triangles = raw_triangles
        .into_iter()
        .filter(|t| t.iter().all(|&i| (i as usize) < count))
        .collect();
    def
}

#[derive(Default)]
struct Output {
    dedup: VertexDedup,
    triangles: Vec<[u32; 3]>,
    /// Work done so far, in [`MAX_EMITTED`] units.
    emitted: u64,
}

/// Memoised [`Resolver::cost`] of an object.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Cost {
    Unknown,
    /// On the current depth-first path.
    Open,
    /// `None`: a component cycle is reachable, so the expansion depends on the path taken.
    Known(Option<u64>),
}

struct Resolver<R> {
    package: Package<R>,
    parsed: HashSet<String>,
    objects: Objects,
    /// Per object: its components resolved to `(object, transform)`, once looked up.
    children: Vec<Option<Vec<(usize, Affine)>>>,
    costs: Vec<Cost>,
}

impl<R: Read + Seek> Resolver<R> {
    fn new(package: Package<R>) -> Self {
        Self {
            package,
            parsed: HashSet::new(),
            objects: Objects::default(),
            children: Vec::new(),
            costs: Vec::new(),
        }
    }

    /// Object `id` of the model part `part_path`, parsing that part on first use.
    fn lookup(&mut self, part_path: &str, id: &str) -> Option<usize> {
        let part = self.package.find(part_path)?;
        if self.parsed.insert(part.clone()) {
            // Unreadable parts contribute nothing (Python ignores them silently).
            if let Ok(contents) = self.package.read_part(&part)
                && let Ok(doc) = contents.document()
            {
                self.objects.add_model(&part, doc.root_element());
            }
        }
        self.objects.get(&part, id)
    }

    /// Keys of every object some parsed object uses as a component.
    fn component_targets(&mut self) -> HashSet<(String, String)> {
        let mut targets = Vec::new();
        for (key, def) in self.objects.keys.iter().zip(&self.objects.defs) {
            for comp in &def.components {
                if let Some(id) = &comp.object_id {
                    targets.push((
                        comp.path.clone().unwrap_or_else(|| key.0.clone()),
                        id.clone(),
                    ));
                }
            }
        }
        targets
            .into_iter()
            .filter_map(|(path, id)| Some((self.package.find(&path)?, id)))
            .collect()
    }

    /// Looks up `obj`'s components (parsing the parts they live in) unless already done.
    fn resolve(&mut self, obj: usize) {
        if self.children.len() <= obj {
            self.children.resize(obj + 1, None);
        }
        if self.children[obj].is_some() {
            return;
        }
        let parent_part = self.objects.keys[obj].0.clone();
        let components = self.objects.defs[obj].components.clone();
        let resolved = components
            .iter()
            .filter_map(|comp| {
                let id = comp.object_id.as_deref()?;
                let path = comp.path.as_deref().unwrap_or(&parent_part);
                Some((self.lookup(path, id)?, comp.transform))
            })
            .collect();
        self.children[obj] = Some(resolved);
    }

    /// `obj`'s `i`-th resolved component.
    fn child(&self, obj: usize, i: usize) -> Option<(usize, Affine)> {
        self.children.get(obj)?.as_ref()?.get(i).copied()
    }

    fn cost_of(&self, obj: usize) -> Cost {
        self.costs.get(obj).copied().unwrap_or(Cost::Unknown)
    }

    fn set_cost(&mut self, obj: usize, cost: Cost) {
        if self.costs.len() <= obj {
            self.costs.resize(obj + 1, Cost::Unknown);
        }
        self.costs[obj] = cost;
    }

    /// What [`Self::emit`] of `obj` adds to [`Output::emitted`]: one per object instance plus
    /// its vertices and triangles, with geometry-free subtrees counting 0 (they are skipped).
    /// Memoised, so a shared sub-assembly is sized once however often it is instanced.
    /// `None` if a component cycle is reachable.
    fn cost(&mut self, root: usize) -> Option<u64> {
        struct Frame {
            obj: usize,
            next: usize,
            sum: u64,
            cyclic: bool,
        }
        if let Cost::Known(cost) = self.cost_of(root) {
            return cost;
        }
        let own = |r: &Self, obj: usize| {
            let def = &r.objects.defs[obj];
            (def.vertices.len() + def.triangles.len()) as u64
        };
        self.resolve(root);
        self.set_cost(root, Cost::Open);
        let mut stack = vec![Frame {
            obj: root,
            next: 0,
            sum: own(self, root),
            cyclic: false,
        }];
        let mut result = None;
        while let Some(top) = stack.last_mut() {
            if let Some((child, _)) = self.child(top.obj, top.next) {
                top.next += 1;
                match self.cost_of(child) {
                    Cost::Known(Some(c)) => top.sum = top.sum.saturating_add(c),
                    Cost::Known(None) | Cost::Open => top.cyclic = true,
                    Cost::Unknown => {
                        self.resolve(child);
                        self.set_cost(child, Cost::Open);
                        let sum = own(self, child);
                        stack.push(Frame {
                            obj: child,
                            next: 0,
                            sum,
                            cyclic: false,
                        });
                    }
                }
                continue;
            }
            let (obj, sum, cyclic) = (top.obj, top.sum, top.cyclic);
            result = (!cyclic).then(|| if sum == 0 { 0 } else { sum.saturating_add(1) });
            self.set_cost(obj, Cost::Known(result));
            stack.pop();
            if let Some(parent) = stack.last_mut() {
                match result {
                    Some(c) => parent.sum = parent.sum.saturating_add(c),
                    None => parent.cyclic = true,
                }
            }
        }
        result
    }

    /// Append object `obj` and its component tree, transformed by `transform`, to `out`.
    /// Iterative depth-first walk; an object already on the current path is skipped (cycles).
    /// Acyclic assemblies are sized up front and geometry-free subtrees skipped, so the walk
    /// stays within [`MAX_EMITTED`] work; with cycles every visit counts against it.
    fn emit(&mut self, obj: usize, transform: Affine, out: &mut Output) -> Result<(), LoadError> {
        struct Frame {
            obj: usize,
            transform: Affine,
            next: usize,
        }
        match self.cost(obj) {
            Some(0) => return Ok(()),
            Some(cost) if out.emitted.saturating_add(cost) > MAX_EMITTED => {
                return Err(too_large());
            }
            _ => {}
        }
        let mut on_path = vec![false; self.objects.defs.len()];
        on_path[obj] = true;
        self.emit_mesh(obj, &transform, out)?;
        let mut stack = vec![Frame {
            obj,
            transform,
            next: 0,
        }];
        while let Some(top) = stack.last_mut() {
            let Some((child, child_transform)) = self.child(top.obj, top.next) else {
                on_path[top.obj] = false;
                stack.pop();
                continue;
            };
            top.next += 1;
            if on_path[child] || self.cost_of(child) == Cost::Known(Some(0)) {
                continue;
            }
            on_path[child] = true;
            let transform = child_transform.then(&top.transform);
            self.emit_mesh(child, &transform, out)?;
            stack.push(Frame {
                obj: child,
                transform,
                next: 0,
            });
        }
        Ok(())
    }

    fn emit_mesh(&self, obj: usize, transform: &Affine, out: &mut Output) -> Result<(), LoadError> {
        let def = &self.objects.defs[obj];
        out.emitted += 1 + (def.vertices.len() + def.triangles.len()) as u64;
        if out.emitted > MAX_EMITTED {
            return Err(too_large());
        }
        let ids: Vec<u32> = if *transform == Affine::IDENTITY {
            def.vertices.iter().map(|&v| out.dedup.insert(v)).collect()
        } else {
            def.vertices
                .iter()
                .map(|&v| out.dedup.insert(transform.apply(v)))
                .collect()
        };
        out.triangles
            .extend(def.triangles.iter().map(|t| t.map(|i| ids[i as usize])));
        Ok(())
    }
}

fn too_large() -> LoadError {
    LoadError::Invalid(format!(
        "3MF model is too large to display (more than {} vertices, triangles and instances).",
        thousands(MAX_EMITTED)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transforms_act_on_row_vectors() {
        // 90 degrees about Z: x axis -> y axis (row-vector convention), then translate.
        let rot = Affine::parse(Some("0 1 0 -1 0 0 0 0 1 10 20 30"));
        assert_eq!(rot.apply([1.0, 0.0, 0.0]), [10.0, 21.0, 30.0]);
        assert_eq!(rot.apply([0.0, 1.0, 0.0]), [9.0, 20.0, 30.0]);
        let shift = Affine::parse(Some(" 1 0 0  0 1 0 0 0 1 5 0 0 "));
        // Child rotated first, then shifted by the parent.
        assert_eq!(rot.then(&shift).apply([1.0, 0.0, 0.0]), [15.0, 21.0, 30.0]);
        assert_eq!(shift.then(&rot).apply([1.0, 0.0, 0.0]), [10.0, 26.0, 30.0]);
    }

    #[test]
    fn invalid_transforms_are_identity() {
        for s in [
            "",
            "1 0 0",
            "1 0 0 0 1 0 0 0 1 0 0 x",
            "1 0 0 0 1 0 0 0 1 0 0 0 0",
            "nan 0 0 0 1 0 0 0 1 0 0 0",
        ] {
            assert_eq!(Affine::parse(Some(s)), Affine::IDENTITY, "{s:?}");
        }
        assert_eq!(Affine::parse(None), Affine::IDENTITY);
    }

    #[test]
    fn units() {
        assert_eq!(unit_scale(None), 1.0);
        assert_eq!(unit_scale(Some("inch")), 25.4);
        assert_eq!(unit_scale(Some("Meter")), 1000.0);
        assert_eq!(unit_scale(Some("furlong")), 1.0);
    }

    #[test]
    fn utf16_parts_decode() {
        let mut le = vec![0xFF, 0xFE];
        le.extend("<a/>".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(decode_xml_bytes(le).unwrap(), "<a/>");
        let mut be = vec![0xFE, 0xFF];
        be.extend("<b/>".encode_utf16().flat_map(u16::to_be_bytes));
        assert_eq!(decode_xml_bytes(be).unwrap(), "<b/>");
        assert!(decode_xml_bytes(vec![0xC3]).is_err());
    }

    #[test]
    fn declared_single_byte_encodings_decode() {
        let latin1 = b"<?xml version='1.0' encoding='ISO-8859-1'?><a n='Pi\xE8ce'/>".to_vec();
        assert_eq!(
            decode_xml_bytes(latin1).unwrap(),
            "<?xml version='1.0' encoding='ISO-8859-1'?><a n='Pi\u{E8}ce'/>"
        );
        let cp = b"<?xml version=\"1.0\" encoding = \"Windows-1252\" ?><a n='\x80\xE8'/>".to_vec();
        assert!(
            decode_xml_bytes(cp)
                .unwrap()
                .ends_with("<a n='\u{20AC}\u{E8}'/>")
        );
        let undeclared = b"<?xml version='1.0'?><a n='\xE8'/>".to_vec();
        assert!(decode_xml_bytes(undeclared).is_err());
    }
}
