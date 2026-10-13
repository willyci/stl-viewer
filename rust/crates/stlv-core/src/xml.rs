//! Guards for handing untrusted XML to roxmltree (directly, or through usvg).
//!
//! roxmltree's tokenizer recurses once per level of element nesting with no limit of its own,
//! so a few kilobytes of `<g><g><g>...` overflow any thread's stack, which aborts the whole
//! process (it is not a panic). And its DOM costs far more than the text: it reserves a node
//! per `<` and an attribute per `=` up front, so a megabyte of zipped `<a/><a/>...` commits
//! gigabytes. [`bounds`] is a cheap iterative pre-scan that over-approximates both.

/// Deepest element nesting we hand to roxmltree. Each level costs it roughly 1 KB of stack,
/// so this stays far below the 1 MB main thread and 2 MB worker threads; real 3MF/SVG files
/// nest fewer than 20 levels.
pub const MAX_NESTING: usize = 256;

/// Approximate size of one roxmltree node or attribute record (with source positions).
pub const DOM_ITEM_BYTES: u64 = 80;

/// roxmltree's limits on entity expansion: nesting depth, and expansions per reference in text.
const ENTITY_EXPANSION_DEPTH: usize = 10;
const EXPANSIONS_PER_REFERENCE: u64 = 256;

/// Upper bounds on what parsing a document into a DOM costs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// Element nesting depth, i.e. parser recursion depth.
    pub nesting: usize,
    /// Bytes of DOM: node and attribute records, plus copies of unescaped text and of
    /// expanded entities.
    pub dom_bytes: u64,
}

/// Bounds for `xml`, which must be what the parser sees (decoded to UTF-8).
///
/// Nesting is the deepest run of unclosed start tags, plus [`ENTITY_EXPANSION_DEPTH`] times
/// the number of start tags written anywhere inside `<!...>` declarations (a DTD's entity
/// values are parsed as content wherever they are referenced). Comments, CDATA sections,
/// processing instructions and quoted attribute values are skipped as a parser would;
/// malformed markup only makes the bounds larger, never smaller.
pub fn bounds(xml: &[u8]) -> Bounds {
    let scan = scan(xml);
    let (mut lt, mut eq, mut amp) = (0u64, 0u64, 0u64);
    for &b in xml {
        lt += u64::from(b == b'<');
        eq += u64::from(b == b'=');
        amp += u64::from(b == b'&');
    }
    // Every `<` may add an element and the text after it, every `=` an attribute; text with
    // references is copied; each `&` may expand the costliest declared literal 256 times.
    let records = (2 * lt + eq).saturating_mul(DOM_ITEM_BYTES);
    let expansions = amp
        .saturating_mul(EXPANSIONS_PER_REFERENCE)
        .saturating_mul(scan.literal_cost);
    Bounds {
        nesting: scan
            .depth
            .saturating_add(scan.declared_tags.saturating_mul(ENTITY_EXPANSION_DEPTH)),
        dom_bytes: records
            .saturating_add(xml.len() as u64)
            .saturating_add(expansions),
    }
}

/// [`bounds`] within [`MAX_NESTING`] and `max_dom_bytes`, else a message saying which is not.
pub fn check(xml: &[u8], max_dom_bytes: u64) -> Result<(), String> {
    let b = bounds(xml);
    if b.nesting > MAX_NESTING {
        Err(format!(
            "XML elements are nested more than {MAX_NESTING} levels deep"
        ))
    } else if b.dom_bytes > max_dom_bytes {
        Err(format!(
            "XML is too complex to load (needs more than {} MB of memory)",
            max_dom_bytes >> 20
        ))
    } else {
        Ok(())
    }
}

struct Scan {
    depth: usize,
    /// Start tags inside declarations.
    declared_tags: usize,
    /// DOM bytes one expansion of the costliest literal inside a declaration may add.
    literal_cost: u64,
}

fn scan(xml: &[u8]) -> Scan {
    let mut s = Scan {
        depth: 0,
        declared_tags: 0,
        literal_cost: 0,
    };
    let mut depth = 0usize;
    let mut i = 0;
    while let Some(lt) = find(xml, i, b"<") {
        let rest = &xml[lt + 1..];
        i = if rest.starts_with(b"/") {
            depth = depth.saturating_sub(1);
            lt + 2
        } else if rest.starts_with(b"!--") {
            skip_past(xml, lt + 4, b"-->")
        } else if rest.starts_with(b"![CDATA[") {
            skip_past(xml, lt + 9, b"]]>")
        } else if rest.starts_with(b"?") {
            skip_past(xml, lt + 2, b"?>")
        } else if rest.starts_with(b"!") {
            let end = skip_declaration(xml, lt + 2, &mut s.literal_cost);
            s.declared_tags = s
                .declared_tags
                .saturating_add(start_tags(&xml[lt + 2..end]));
            end
        } else {
            depth += 1;
            s.depth = s.depth.max(depth);
            let (end, self_closing) = skip_start_tag(xml, lt + 1);
            if self_closing {
                depth -= 1;
            }
            end
        };
    }
    s
}

/// Skips a start tag's name and attributes from `i`; returns the index after its `>` and
/// whether it ended with `/>`.
fn skip_start_tag(b: &[u8], mut i: usize) -> (usize, bool) {
    let mut slash = false;
    while i < b.len() {
        match b[i] {
            q @ (b'"' | b'\'') => {
                i = skip_past(b, i + 1, &[q]);
                slash = false;
                continue;
            }
            b'>' => return (i + 1, slash),
            c => slash = c == b'/',
        }
        i += 1;
    }
    (i, false)
}

/// Skips a `<!...>` declaration (e.g. a DOCTYPE with an internal subset) from `i`, honouring
/// quoted literals, comments and processing instructions; returns the index after it and
/// raises `literal_cost` to the DOM cost of its costliest literal.
fn skip_declaration(b: &[u8], mut i: usize, literal_cost: &mut u64) -> usize {
    let mut in_subset = false;
    while let Some(&c) = b.get(i) {
        i = match c {
            b'"' | b'\'' => {
                let end = skip_past(b, i + 1, &[c]);
                let literal = &b[i + 1..end];
                let items = literal
                    .iter()
                    .map(|&x| match x {
                        b'<' => 2,
                        b'=' => 1,
                        _ => 0,
                    })
                    .sum::<u64>()
                    + 1;
                *literal_cost = (*literal_cost).max(items * DOM_ITEM_BYTES + literal.len() as u64);
                end
            }
            b'<' if b[i..].starts_with(b"<!--") => skip_past(b, i + 4, b"-->"),
            b'<' if b[i..].starts_with(b"<?") => skip_past(b, i + 2, b"?>"),
            b'>' if !in_subset => return i + 1,
            _ => {
                in_subset = match c {
                    b'[' => true,
                    b']' => false,
                    _ => in_subset,
                };
                i + 1
            }
        };
    }
    b.len()
}

/// Every `<` in `b` that could open an element, wherever it is (literal, comment, ...).
fn start_tags(b: &[u8]) -> usize {
    (0..b.len())
        .filter(|&i| b[i] == b'<' && !matches!(b.get(i + 1), Some(b'/' | b'!' | b'?')))
        .count()
}

fn find(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    let hay = b.get(from..)?;
    if let [c] = needle {
        return hay.iter().position(|x| x == c).map(|p| from + p);
    }
    hay.windows(needle.len())
        .position(|w| w == needle)
        .map(|p| from + p)
}

/// Index just past the next `needle` at or after `from`, or the end of `b`.
fn skip_past(b: &[u8], from: usize, needle: &[u8]) -> usize {
    find(b, from, needle).map_or(b.len(), |p| p + needle.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nesting(s: &str) -> usize {
        bounds(s.as_bytes()).nesting
    }

    fn dom(s: &str) -> u64 {
        bounds(s.as_bytes()).dom_bytes
    }

    #[test]
    fn counts_open_elements() {
        assert_eq!(nesting(""), 0);
        assert_eq!(nesting("<a/>"), 1);
        assert_eq!(nesting("<a><b/><c><d/></c></a>"), 3);
        assert_eq!(nesting("<?xml version='1.0'?><m><r><o/></r></m>"), 3);
        let deep = format!("<m>{}{}</m>", "<g>".repeat(5000), "</g>".repeat(5000));
        assert_eq!(nesting(&deep), 5001);
        // Unterminated input still counts what was opened.
        assert_eq!(nesting("<a><b><c"), 3);
    }

    #[test]
    fn skips_non_element_markup() {
        assert_eq!(nesting("<a><!-- <b><c><d> --></a>"), 1);
        assert_eq!(nesting("<a><![CDATA[<b><c>]]></a>"), 1);
        assert_eq!(nesting("<a><?pi <b><c>?></a>"), 1);
        assert_eq!(nesting("<a x='/>' y=\"<b>\"><c/></a>"), 2);
        assert_eq!(nesting("<a><b x='1'/><c/></a>"), 2);
    }

    #[test]
    fn fake_close_tags_cannot_hide_depth() {
        // Close tags inside comments/attributes do not count; stray real ones clamp at zero.
        let s = format!(
            "<a x='{}'><!--{}-->{}",
            "</a>".repeat(100),
            "</a>".repeat(100),
            "<g>".repeat(300)
        );
        assert_eq!(nesting(&s), 301);
        let s = format!("{}{}", "</a>".repeat(100), "<g>".repeat(300));
        assert_eq!(nesting(&s), 300);
    }

    #[test]
    fn declared_markup_is_multiplied_by_expansion_depth() {
        let s =
            "<!DOCTYPE m [<!ENTITY e \"<a><b/></a>\"> <!-- ] > --> <!ENTITY f '>]'>]><m>&e;</m>";
        assert_eq!(nesting(s), 1 + 2 * ENTITY_EXPANSION_DEPTH);
        let flat = "<!DOCTYPE m [<!ENTITY e \"text\">]><m><n>&e;</n></m>";
        assert_eq!(nesting(flat), 2);
        // A parser may end a declaration at a different `>` (e.g. roxmltree ends <!ATTLIST at
        // the first one), so start tags hidden in its comments still count.
        let s = format!(
            "<!DOCTYPE m [<!ATTLIST a <!-- >]><m>{}<x/>-->]><m/>",
            "<g>".repeat(300)
        );
        assert!(
            nesting(&s) > 300 * ENTITY_EXPANSION_DEPTH,
            "{}",
            nesting(&s)
        );
    }

    #[test]
    fn dom_bytes_cover_records_and_entity_expansion() {
        let s = "<m><v x=\"1\" y=\"2\"/></m>";
        assert_eq!(dom(s), (2 * 3 + 2) * DOM_ITEM_BYTES + s.len() as u64);
        // Plain references (no DTD literal) expand to nothing extra.
        let plain = "<m a=\"&amp;\">&lt;&#65;</m>";
        assert_eq!(
            dom(plain),
            (2 * 2 + 1) * DOM_ITEM_BYTES + plain.len() as u64
        );
        // Each reference may expand the declared literal up to 256 times.
        let ent = "<!DOCTYPE m [<!ENTITY e \"<a b='1'/>\">]><m>&e;&e;</m>";
        let literal = (2 + 1 + 1) * DOM_ITEM_BYTES + 10;
        assert!(dom(ent) >= 2 * EXPANSIONS_PER_REFERENCE * literal);
        // A megabyte of `<a/>` costs about 40 times its size.
        let bomb = "<a/>".repeat(1 << 18);
        assert!(dom(&bomb) > 40 << 20);
    }

    #[test]
    fn check_limits() {
        let ok = format!(
            "{}{}",
            "<g>".repeat(MAX_NESTING),
            "</g>".repeat(MAX_NESTING)
        );
        assert!(check(ok.as_bytes(), u64::MAX).is_ok());
        let deep = format!("<g>{ok}</g>");
        assert!(
            check(deep.as_bytes(), u64::MAX)
                .unwrap_err()
                .contains("256")
        );
        let cost = bounds(ok.as_bytes()).dom_bytes;
        assert!(check(ok.as_bytes(), cost).is_ok());
        assert!(
            check(ok.as_bytes(), cost - 1)
                .unwrap_err()
                .contains("too complex")
        );
    }
}
