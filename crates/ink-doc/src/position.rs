//! Positions in a document.
//!
//! - Reflowable books use crengine xpointers, the format KOReader stores in `progress`,
//!   highlights and KOSync: `/body/DocFragment[3]/body/div/p[2]/text().42`.
//!   `DocFragment[N]` is the N-th spine item (1-based); element steps carry a 1-based
//!   index in brackets when the element has same-named siblings; `text().K` is the
//!   character offset K in the first text node, `text()[M].K` in the M-th.
//! - Fixed-layout documents (PDF, CBZ) use a page number plus optional quads.
//! - [`FlowAnchor`] is the phase 0 engine's own anchor (spine item, block, word). It is
//!   not portable. The phase 2 engine (`ink-html`) writes xpointers and still reads
//!   `flow:` strings from old sidecars.
//!
//! Every position has a stable string form ([`std::fmt::Display`] and [`std::str::FromStr`])
//! used in sidecars and the plugin API.

use crate::DocError;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// One element step of an xpointer path: `p` or `p[3]`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Step {
    /// Element name.
    pub name: String,
    /// 1-based index among same-named siblings, when written.
    pub index: Option<u32>,
}

/// Text offset at the end of an xpointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextOffset {
    /// `text()[M]`: which text node (1-based); `None` = the first.
    pub node: Option<u32>,
    /// Character offset in that text node.
    pub offset: u32,
}

/// A crengine xpointer.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct XPointer {
    /// Element steps after the root, starting with `body` and `DocFragment[N]` for EPUBs.
    pub steps: Vec<Step>,
    /// Text node and offset, if the pointer points into text.
    pub text: Option<TextOffset>,
    /// Element point `.N` after the last element step (child offset N); `None` when the
    /// pointer names the element itself or points into text.
    pub point: Option<u32>,
}

impl XPointer {
    /// `/body/DocFragment[N]/body`: the start of spine item `n` (1-based).
    pub fn fragment_start(n: u32) -> Self {
        XPointer {
            steps: vec![
                Step { name: "body".into(), index: None },
                Step { name: "DocFragment".into(), index: Some(n) },
                Step { name: "body".into(), index: None },
            ],
            text: None,
            point: None,
        }
    }

    /// `/body/DocFragment[N].0`: crengine's form for "the start of spine item N", used
    /// for TOC entries without a fragment identifier.
    pub fn fragment_point(n: u32) -> Self {
        XPointer {
            steps: vec![Step { name: "body".into(), index: None }, Step { name: "DocFragment".into(), index: Some(n) }],
            text: None,
            point: Some(0),
        }
    }

    /// The spine item (1-based) for `/body/DocFragment[N]/...` pointers.
    pub fn fragment(&self) -> Option<u32> {
        self.steps.iter().find(|s| s.name == "DocFragment").map(|s| s.index.unwrap_or(1))
    }
}

impl fmt::Display for XPointer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for s in &self.steps {
            write!(f, "/{}", s.name)?;
            if let Some(i) = s.index {
                write!(f, "[{i}]")?;
            }
        }
        if let Some(t) = self.text {
            write!(f, "/text()")?;
            if let Some(n) = t.node {
                write!(f, "[{n}]")?;
            }
            write!(f, ".{}", t.offset)?;
        } else if let Some(p) = self.point {
            write!(f, ".{p}")?;
        }
        Ok(())
    }
}

fn parse_step(s: &str) -> Option<(String, Option<u32>)> {
    match s.find('[') {
        None => Some((s.to_string(), None)),
        Some(b) => {
            let idx = s[b + 1..].strip_suffix(']')?.parse().ok()?;
            Some((s[..b].to_string(), Some(idx)))
        }
    }
}

impl FromStr for XPointer {
    type Err = DocError;

    fn from_str(s: &str) -> Result<Self, DocError> {
        let bad = || DocError::BadPosition(s.to_string());
        let rest = s.strip_prefix('/').ok_or_else(bad)?;
        let mut steps = Vec::new();
        let mut text = None;
        let mut point = None;
        let parts: Vec<&str> = rest.split('/').collect();
        for (i, part) in parts.iter().enumerate() {
            if let Some(t) = part.strip_prefix("text()") {
                if i + 1 != parts.len() {
                    return Err(bad());
                }
                let (node, off) = match t.strip_prefix('[') {
                    Some(r) => {
                        let close = r.find(']').ok_or_else(bad)?;
                        (Some(r[..close].parse().map_err(|_| bad())?), &r[close + 1..])
                    }
                    None => (None, t),
                };
                let offset = match off.strip_prefix('.') {
                    Some(o) => o.parse().map_err(|_| bad())?,
                    None if off.is_empty() => 0,
                    None => return Err(bad()),
                };
                text = Some(TextOffset { node, offset });
            } else {
                // A final element point: `name[3].0` or `name.0`.
                let mut part: &str = part;
                if i + 1 == parts.len() {
                    if let Some(dot) = part.rfind('.') {
                        let tail = &part[dot + 1..];
                        if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) && (part[..dot].ends_with(']') || !part[..dot].contains('[')) {
                            point = Some(tail.parse().map_err(|_| bad())?);
                            part = &part[..dot];
                        }
                    }
                }
                let (name, index) = parse_step(part).ok_or_else(bad)?;
                if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || "_-:".contains(c)) {
                    return Err(bad());
                }
                steps.push(Step { name, index });
            }
        }
        if steps.is_empty() {
            return Err(bad());
        }
        Ok(XPointer { steps, text, point })
    }
}

/// A quadrilateral on a fixed-layout page, in page points (PDF user space).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Quad {
    /// Left.
    pub x0: f32,
    /// Top.
    pub y0: f32,
    /// Right.
    pub x1: f32,
    /// Bottom.
    pub y1: f32,
}

/// Position in a fixed-layout document.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PagePosition {
    /// 0-based page index.
    pub page: u32,
    /// Text quads (for highlights); empty for a whole-page position.
    pub quads: Vec<Quad>,
}

/// The phase 0 engine's anchor: spine item, block and word index (all 0-based).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FlowAnchor {
    /// Spine item (chapter) index.
    pub spine: u32,
    /// Block index inside the chapter.
    pub block: u32,
    /// Word index inside the block.
    pub word: u32,
}

/// A position in any document.
#[derive(Clone, Debug, PartialEq)]
pub enum Position {
    /// crengine xpointer (reflowable).
    XPointer(XPointer),
    /// Page and quads (fixed layout).
    Page(PagePosition),
    /// Phase 0 engine anchor (reflowable, not portable).
    Flow(FlowAnchor),
}

impl fmt::Display for Position {
    /// `"/body/..."`, `"page:12"`, `"page:12;x0,y0,x1,y1;..."` or `"flow:spine.block.word"`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Position::XPointer(x) => write!(f, "{x}"),
            Position::Page(p) => {
                write!(f, "page:{}", p.page)?;
                for q in &p.quads {
                    write!(f, ";{},{},{},{}", q.x0, q.y0, q.x1, q.y1)?;
                }
                Ok(())
            }
            Position::Flow(a) => write!(f, "flow:{}.{}.{}", a.spine, a.block, a.word),
        }
    }
}

impl FromStr for Position {
    type Err = DocError;

    fn from_str(s: &str) -> Result<Self, DocError> {
        let bad = || DocError::BadPosition(s.to_string());
        if s.starts_with('/') {
            return Ok(Position::XPointer(s.parse()?));
        }
        if let Some(rest) = s.strip_prefix("page:") {
            let mut parts = rest.split(';');
            let page = parts.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
            let mut quads = Vec::new();
            for q in parts {
                let v: Vec<f32> = q.split(',').map(|n| n.parse().map_err(|_| bad())).collect::<Result<_, _>>()?;
                if v.len() != 4 {
                    return Err(bad());
                }
                quads.push(Quad { x0: v[0], y0: v[1], x1: v[2], y1: v[3] });
            }
            return Ok(Position::Page(PagePosition { page, quads }));
        }
        if let Some(rest) = s.strip_prefix("flow:") {
            let v: Vec<u32> = rest.split('.').map(|n| n.parse().map_err(|_| bad())).collect::<Result<_, _>>()?;
            if let [spine, block, word] = v[..] {
                return Ok(Position::Flow(FlowAnchor { spine, block, word }));
            }
        }
        Err(bad())
    }
}

impl Serialize for Position {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Position {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Kindle-style location number (1-based, one per 128 bytes of text).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Location(pub u32);

impl Location {
    /// Bytes of text per location.
    pub const BYTES: u64 = 128;

    /// The location containing text byte offset `offset` (0-based).
    pub fn from_offset(offset: u64) -> Location {
        Location((offset / Self::BYTES) as u32 + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn koreader_xpointers_round_trip() {
        for s in [
            "/body/DocFragment[14]/body/p[25]/text().0",
            "/body/DocFragment[3]/body/div/h2/text().17",
            "/body/DocFragment[2]/body/div[2]/p[3]/text()[2].5",
            "/body/DocFragment[7]/body/section/p[12]",
            "/body/DocFragment[1]/body/svg:svg",
            "/body/DocFragment[2].0",
            "/body/DocFragment[4]/body/div/div/div.0",
            "/body/DocFragment[1]/body/table[1]/tr[2]/td[1].0",
            "/body/DocFragment/body/p.3",
        ] {
            let x: XPointer = s.parse().unwrap();
            assert_eq!(x.to_string(), s);
        }
        let x: XPointer = "/body/DocFragment[14]/body/p[25]/text().9".parse().unwrap();
        assert_eq!(x.fragment(), Some(14));
        assert_eq!(x.text, Some(TextOffset { node: None, offset: 9 }));
        assert_eq!(XPointer::fragment_start(4).to_string(), "/body/DocFragment[4]/body");
        assert_eq!(XPointer::fragment_point(2).to_string(), "/body/DocFragment[2].0");
        let p: XPointer = "/body/DocFragment[4]/body/div.0".parse().unwrap();
        assert_eq!((p.point, p.text, p.steps.len()), (Some(0), None, 4));
        assert_eq!(p.fragment(), Some(4));
        let s: XPointer = "/body/DocFragment/body/p".parse().unwrap();
        assert_eq!(s.fragment(), Some(1));
    }

    #[test]
    fn bad_xpointers() {
        for s in ["", "body", "/", "/body/p[x]", "/body/text().a", "/body/text().1/p", "/bo dy", "/body/p.x", "/body/p.1/q"] {
            assert!(s.parse::<XPointer>().is_err(), "{s}");
        }
    }

    #[test]
    fn position_strings_and_serde() {
        for s in ["flow:3.10.5", "page:12", "page:0;1,2,3.5,4", "/body/DocFragment[1]/body"] {
            let p: Position = s.parse().unwrap();
            assert_eq!(p.to_string(), s);
            let json = serde_json::to_string(&p).unwrap();
            assert_eq!(serde_json::from_str::<Position>(&json).unwrap(), p);
        }
        assert!("flow:1.2".parse::<Position>().is_err());
        assert!("page:1;1,2".parse::<Position>().is_err());
        assert!("nonsense".parse::<Position>().is_err());
    }

    #[test]
    fn locations() {
        assert_eq!(Location::from_offset(0), Location(1));
        assert_eq!(Location::from_offset(127), Location(1));
        assert_eq!(Location::from_offset(128), Location(2));
    }
}
