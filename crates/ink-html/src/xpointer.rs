//! crengine xpointers over a chapter [`Dom`].
//!
//! Generation follows crengine's `ldomXPointer::toStringV2` (normalized xpointers, DOM
//! version >= 20200223): an element step is `name[i]` when its parent has more than one
//! child element with that name and `name` otherwise; a text step is `text()[i]` /
//! `text()`; text points end in `.offset` (Unicode scalar values), element points in
//! `.child`. The path starts with `/body/DocFragment[N]/body` (`DocFragment` without an
//! index when the book has a single spine item). Resolution follows
//! `createXPointerV2`.

use crate::dom::{Dom, NodeId, NodeKind, NONE};
use ink_doc::{Step, TextOffset, XPointer};

/// A position inside one chapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DomPos {
    /// Character `offset` (Unicode scalar values) in text node `node`.
    Text {
        /// Text node.
        node: NodeId,
        /// Offset in chars.
        offset: u32,
    },
    /// An element, optionally before its child `point`.
    Element {
        /// Element.
        node: NodeId,
        /// Child offset (`.N`).
        point: Option<u32>,
    },
}

impl DomPos {
    /// The node.
    pub fn node(&self) -> NodeId {
        match *self {
            DomPos::Text { node, .. } | DomPos::Element { node, .. } => node,
        }
    }
}

/// Document-order key of a position inside its chapter: (node order, char offset).
pub fn order_key(dom: &Dom, pos: DomPos) -> (u32, u32) {
    match pos {
        DomPos::Text { node, offset } => (dom.node(node).order, offset),
        DomPos::Element { node, point } => match point {
            Some(k) => {
                let kids = &dom.node(node).children;
                match kids.get(k as usize) {
                    Some(&c) => (dom.node(c).order, 0),
                    None if kids.is_empty() => (dom.node(node).order, 0),
                    None => (dom.node(dom.last_descendant(node)).order + 1, 0),
                }
            }
            None => (dom.node(node).order, 0),
        },
    }
}

fn step_of(dom: &Dom, id: NodeId) -> Step {
    let node = dom.node(id);
    if let Some(&(i, total)) = dom.steps.get(id as usize).filter(|_| dom.steps.len() == dom.nodes.len()) {
        // Precomputed by `Dom::finish`.
        let index = (total > 1).then_some(i);
        return match &node.kind {
            NodeKind::Element(e) => Step { name: e.name.clone(), index },
            NodeKind::Text(_) => Step { name: "text()".into(), index },
        };
    }
    let parent = node.parent;
    let siblings: &[NodeId] = if parent == NONE { &[] } else { &dom.node(parent).children };
    match &node.kind {
        NodeKind::Element(e) => {
            let same: Vec<NodeId> = siblings.iter().copied().filter(|&s| dom.element(s).is_some_and(|x| x.name == e.name)).collect();
            let index = (same.len() > 1).then(|| same.iter().position(|&s| s == id).unwrap_or(0) as u32 + 1);
            Step { name: e.name.clone(), index }
        }
        NodeKind::Text(_) => {
            let texts: Vec<NodeId> = siblings.iter().copied().filter(|&s| dom.text(s).is_some()).collect();
            let index = (texts.len() > 1).then(|| texts.iter().position(|&s| s == id).unwrap_or(0) as u32 + 1);
            Step { name: "text()".into(), index }
        }
    }
}

/// The xpointer of `pos` in DocFragment `fragment` (1-based) of a book with
/// `fragments` DocFragments.
pub fn to_xpointer(dom: &Dom, fragment: u32, fragments: u32, pos: DomPos) -> XPointer {
    let mut steps = vec![
        Step { name: "body".into(), index: None },
        Step { name: "DocFragment".into(), index: (fragments > 1).then_some(fragment) },
    ];
    let node = pos.node();
    let mut chain = Vec::new();
    let mut p = node;
    while p != NONE {
        chain.push(p);
        p = dom.node(p).parent;
    }
    chain.reverse();
    let mut text = None;
    for &n in &chain {
        let st = step_of(dom, n);
        if dom.text(n).is_some() {
            let offset = match pos {
                DomPos::Text { offset, .. } => offset,
                _ => 0,
            };
            text = Some(TextOffset { node: st.index, offset });
        } else if n == dom.body {
            steps.push(Step { name: "body".into(), index: None });
        } else {
            steps.push(st);
        }
    }
    let point = match pos {
        DomPos::Element { point, .. } => point,
        DomPos::Text { .. } => None,
    };
    XPointer { steps, text, point }
}

/// Resolves the steps after `DocFragment[N]` (`body/...`) in `dom`. Strict: `None` when
/// a step does not exist.
pub fn resolve(dom: &Dom, xp: &XPointer) -> Option<DomPos> {
    let frag_at = xp.steps.iter().position(|s| s.name == "DocFragment");
    let rest: &[Step] = match frag_at {
        Some(i) => &xp.steps[i + 1..],
        None => &xp.steps[..],
    };
    if rest.is_empty() {
        // `/body/DocFragment[N]` or `.../DocFragment[N].K`: the chapter start.
        return Some(DomPos::Element { node: dom.body, point: Some(0) });
    }
    if !rest[0].name.eq_ignore_ascii_case("body") {
        return None;
    }
    let mut cur = dom.body;
    for st in &rest[1..] {
        cur = child_by_step(dom, cur, st)?;
    }
    if let Some(t) = xp.text {
        let idx = t.node.unwrap_or(1) as usize;
        let tn = dom.node(cur).children.iter().copied().filter(|&c| dom.text(c).is_some()).nth(idx.checked_sub(1)?)?;
        let len = dom.text(tn)?.chars().count() as u32;
        if t.offset > len {
            return None;
        }
        return Some(DomPos::Text { node: tn, offset: t.offset });
    }
    if let Some(p) = xp.point {
        if dom.text(cur).is_some() {
            return Some(DomPos::Text { node: cur, offset: p });
        }
        if p as usize > dom.node(cur).children.len() {
            return None;
        }
    }
    Some(DomPos::Element { node: cur, point: xp.point })
}

fn child_by_step(dom: &Dom, parent: NodeId, st: &Step) -> Option<NodeId> {
    let kids = &dom.node(parent).children;
    let idx = st.index.unwrap_or(1) as usize;
    if st.name.bytes().all(|b| b.is_ascii_digit()) {
        let n: usize = st.name.parse().ok()?;
        return kids.get(n.checked_sub(1)?).copied();
    }
    // Prefixed names (`svg:svg`) match the local name.
    let name = st.name.rsplit(':').next().unwrap_or(&st.name);
    kids.iter()
        .copied()
        .filter(|&c| dom.element(c).is_some_and(|e| e.name == name || e.name.eq_ignore_ascii_case(name)))
        .nth(idx.checked_sub(1)?)
}

/// Like [`resolve`], but a missing step resolves to the deepest existing ancestor (and an
/// out-of-range text offset is clamped), so positions saved for a slightly different
/// DOM still land nearby. `None` only when the path does not start with `body`.
pub fn resolve_lenient(dom: &Dom, xp: &XPointer) -> Option<DomPos> {
    if let Some(p) = resolve(dom, xp) {
        return Some(p);
    }
    let frag_at = xp.steps.iter().position(|s| s.name == "DocFragment");
    let rest: &[Step] = match frag_at {
        Some(i) => &xp.steps[i + 1..],
        None => &xp.steps[..],
    };
    if rest.is_empty() || !rest[0].name.eq_ignore_ascii_case("body") {
        return Some(DomPos::Element { node: dom.body, point: Some(0) });
    }
    let mut cur = dom.body;
    for st in &rest[1..] {
        match child_by_step(dom, cur, st) {
            Some(c) => cur = c,
            None => return Some(DomPos::Element { node: cur, point: None }),
        }
    }
    if let Some(t) = xp.text {
        let texts: Vec<NodeId> = dom.node(cur).children.iter().copied().filter(|&c| dom.text(c).is_some()).collect();
        if let Some(&tn) = texts.get(t.node.unwrap_or(1).saturating_sub(1) as usize).or(texts.last()) {
            let len = dom.text(tn).map(|s| s.chars().count()).unwrap_or(0) as u32;
            return Some(DomPos::Text { node: tn, offset: t.offset.min(len) });
        }
    }
    Some(DomPos::Element { node: cur, point: None })
}

/// Byte offset of char `offset` in `s` (clamped to the end).
pub fn char_to_byte(s: &str, offset: u32) -> usize {
    s.char_indices().nth(offset as usize).map(|(i, _)| i).unwrap_or(s.len())
}

/// Char offset of byte offset `byte` in `s`.
pub fn byte_to_char(s: &str, byte: usize) -> u32 {
    s[..byte.min(s.len())].chars().count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::{Builder, Classifier};
    use crate::style::{StructStyle, WhiteSpace};

    struct Defaults;
    impl Classifier for Defaults {
        fn classify(&mut self, dom: &Dom, id: NodeId, parent_ws: WhiteSpace) -> StructStyle {
            crate::dom::default_struct(dom.name(id), parent_ws)
        }
    }

    #[test]
    fn generate_and_resolve() {
        let src = r#"<html><body><div><p>One</p><p>Two <i>it</i> three</p></div><div><img src="a.png"/></div></body></html>"#;
        let mut c = Defaults;
        let dom = crate::parse::parse_chapter(src, &mut c);
        let _ = Builder::new(None);
        let texts: Vec<NodeId> = dom.preorder.iter().copied().filter(|&n| dom.text(n).is_some()).collect();
        let x = to_xpointer(&dom, 3, 5, DomPos::Text { node: texts[2], offset: 1 });
        assert_eq!(x.to_string(), "/body/DocFragment[3]/body/div[1]/p[2]/i/text().1");
        let y = to_xpointer(&dom, 3, 5, DomPos::Text { node: texts[3], offset: 2 });
        assert_eq!(y.to_string(), "/body/DocFragment[3]/body/div[1]/p[2]/text()[2].2");
        for xp in [x, y] {
            let p = resolve(&dom, &xp).unwrap();
            assert_eq!(to_xpointer(&dom, 3, 5, p), xp);
        }
        let img = dom.preorder.iter().copied().find(|&n| dom.name(n) == "img").unwrap();
        let z = to_xpointer(&dom, 1, 1, DomPos::Element { node: img, point: Some(0) });
        assert_eq!(z.to_string(), "/body/DocFragment/body/div[2]/img.0");
        assert_eq!(resolve(&dom, &z), Some(DomPos::Element { node: img, point: Some(0) }));
        let start: XPointer = "/body/DocFragment[3].0".parse().unwrap();
        assert_eq!(resolve(&dom, &start), Some(DomPos::Element { node: dom.body, point: Some(0) }));
        let bad: XPointer = "/body/DocFragment[3]/body/div[1]/p[7]/text().0".parse().unwrap();
        assert_eq!(resolve(&dom, &bad), None);
        let div1 = dom.node(dom.body).children[0];
        assert_eq!(resolve_lenient(&dom, &bad), Some(DomPos::Element { node: div1, point: None }));
        let far: XPointer = "/body/DocFragment[3]/body/div[1]/p[1]/text().99".parse().unwrap();
        assert_eq!(resolve(&dom, &far), None);
        assert_eq!(resolve_lenient(&dom, &far), Some(DomPos::Text { node: texts[0], offset: 3 }));
        // Order keys follow document order.
        let k1 = order_key(&dom, DomPos::Text { node: texts[2], offset: 1 });
        let k2 = order_key(&dom, DomPos::Element { node: dom.body, point: Some(1) });
        assert!(k1 < k2);
        assert_eq!(char_to_byte("añb", 2), 3);
        assert_eq!(byte_to_char("añb", 3), 2);
    }
}
