//! Computed styles: the user-agent stylesheet, the cascade (origin, `!important`,
//! specificity, order), inheritance, units and the user's overrides.
//!
//! Lengths are resolved to device pixels here, except percentages of the containing
//! block width (resolved by the layout). One CSS `px` is `font_px / 16` device pixels,
//! so a book's absolute sizes follow the user's font size the way `em` sizes do.

use crate::css::{self, Declaration, Selector, SelectorElement, Stylesheet, Tok};
use std::collections::HashMap;
use std::sync::Arc;

/// `display`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Display {
    /// `inline` (also `ruby`, `run-in`, unknown values).
    #[default]
    Inline,
    /// `inline-block` (laid out as an inline atom of text here).
    InlineBlock,
    /// `block` (also `flex`, `grid`, `flow-root`).
    Block,
    /// `list-item`.
    ListItem,
    /// `table` / `inline-table`.
    Table,
    /// `table-row-group`, `table-header-group`, `table-footer-group`.
    TableRowGroup,
    /// `table-row`.
    TableRow,
    /// `table-cell`.
    TableCell,
    /// `table-caption`.
    TableCaption,
    /// `table-column`, `table-column-group` (not rendered).
    TableColumn,
    /// `none`.
    None,
}

impl Display {
    /// Block-level for crengine's DOM rules: anything but inline and none.
    pub fn is_block_like(self) -> bool {
        !matches!(self, Display::Inline | Display::None)
    }
}

/// `white-space`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum WhiteSpace {
    /// `normal`.
    #[default]
    Normal,
    /// `nowrap`.
    NoWrap,
    /// `pre-line`.
    PreLine,
    /// `pre`.
    Pre,
    /// `pre-wrap` / `break-spaces`.
    PreWrap,
}

impl WhiteSpace {
    /// crengine parses `pre-line`, `pre`, `pre-wrap` and `break-spaces` text as PRE.
    pub fn parse_as_pre(self) -> bool {
        matches!(self, WhiteSpace::PreLine | WhiteSpace::Pre | WhiteSpace::PreWrap)
    }
    /// Spaces are kept when rendering.
    pub fn keeps_spaces(self) -> bool {
        matches!(self, WhiteSpace::Pre | WhiteSpace::PreWrap)
    }
    /// Newlines force line breaks when rendering.
    pub fn keeps_newlines(self) -> bool {
        matches!(self, WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::PreLine)
    }
    /// Lines may wrap.
    pub fn wraps(self) -> bool {
        !matches!(self, WhiteSpace::Pre | WhiteSpace::NoWrap)
    }
}

/// `text-align`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum TextAlign {
    /// `left` / `start`.
    #[default]
    Left,
    /// `right` / `end`.
    Right,
    /// `center`.
    Center,
    /// `justify`.
    Justify,
}

/// `vertical-align` (inline boxes).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum VAlign {
    /// `baseline` and unsupported values.
    #[default]
    Baseline,
    /// `sub`.
    Sub,
    /// `super`.
    Super,
    /// `middle` (images).
    Middle,
    /// `top` / `text-top`.
    Top,
    /// `bottom` / `text-bottom`.
    Bottom,
    /// A length (device px, positive = up).
    Raise(f32),
}

/// `list-style-type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum ListStyle {
    /// `disc`.
    #[default]
    Disc,
    /// `circle`.
    Circle,
    /// `square`.
    Square,
    /// `decimal`.
    Decimal,
    /// `decimal-leading-zero`.
    DecimalLeadingZero,
    /// `lower-alpha` / `lower-latin`.
    LowerAlpha,
    /// `upper-alpha` / `upper-latin`.
    UpperAlpha,
    /// `lower-roman`.
    LowerRoman,
    /// `upper-roman`.
    UpperRoman,
    /// `lower-greek`.
    LowerGreek,
    /// `none`.
    None,
}

/// `page-break-*` / `break-*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum PageBreak {
    /// `auto`.
    #[default]
    Auto,
    /// `always`, `page`, `left`, `right`.
    Always,
    /// `avoid`.
    Avoid,
}

/// `text-transform`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum TextTransform {
    /// `none`.
    #[default]
    None,
    /// `uppercase`.
    Upper,
    /// `lowercase`.
    Lower,
    /// `capitalize`.
    Capitalize,
}

/// `hyphens`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Hyphens {
    /// `none`.
    None,
    /// `manual` (soft hyphens only).
    Manual,
    /// `auto` (the user's hyphenation setting decides).
    #[default]
    Auto,
}

/// A length that may depend on the containing block width.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum Len {
    /// Device pixels.
    Px(f32),
    /// Percent of the containing block width.
    Pct(f32),
    /// `auto`.
    #[default]
    Auto,
}

/// Largest length in device px a stylesheet can set (larger values are clamped).
pub const MAX_LEN_PX: f32 = 100_000.0;
/// Largest percentage a stylesheet can set.
pub const MAX_LEN_PCT: f32 = 10_000.0;

impl Len {
    /// Resolves against `base` (auto = `auto_value`).
    pub fn resolve(self, base: f32, auto_value: f32) -> f32 {
        match self {
            Len::Px(v) => v,
            Len::Pct(p) => base * p / 100.0,
            Len::Auto => auto_value,
        }
    }
    /// True for `auto`.
    pub fn is_auto(self) -> bool {
        matches!(self, Len::Auto)
    }
}

/// Four box edges.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Edges<T> {
    /// Top.
    pub top: T,
    /// Right.
    pub right: T,
    /// Bottom.
    pub bottom: T,
    /// Left.
    pub left: T,
}

impl<T: Copy> Edges<T> {
    /// The same value on all sides.
    pub fn all(v: T) -> Self {
        Edges { top: v, right: v, bottom: v, left: v }
    }
}

impl Edges<Len> {
    /// Every side resolved against the containing block width `base` (`auto` = 0).
    pub fn resolve(&self, base: f32) -> Edges<f32> {
        let r = |l: Len| l.resolve(base, 0.0);
        Edges { top: r(self.top), right: r(self.right), bottom: r(self.bottom), left: r(self.left) }
    }
}

/// `line-height`.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum LineHeight {
    /// `normal`: the user's line spacing.
    #[default]
    Normal,
    /// A factor of the font size.
    Factor(f32),
    /// Device pixels.
    Px(f32),
}

/// The computed style of an element (text nodes use their parent's).
#[derive(Clone, Debug, PartialEq)]
pub struct ComputedStyle {
    /// `display`.
    pub display: Display,
    /// `white-space`.
    pub white_space: WhiteSpace,
    /// `float` left (-1), none (0), right (1).
    pub float: i8,
    /// `clear` set.
    pub clear: bool,
    /// Family names in priority order (lowercase); generic names as written.
    pub font_family: Arc<[String]>,
    /// Font size in device pixels.
    pub font_size: f32,
    /// Weight >= 600.
    pub bold: bool,
    /// Italic or oblique.
    pub italic: bool,
    /// `font-variant: small-caps`.
    pub small_caps: bool,
    /// `line-height`.
    pub line_height: LineHeight,
    /// `text-align`.
    pub text_align: TextAlign,
    /// `text-indent`.
    pub text_indent: Len,
    /// Margins (`Auto` allowed).
    pub margin: Edges<Len>,
    /// Padding.
    pub padding: Edges<Len>,
    /// Border widths in device px (0 when the style is none/hidden).
    pub border: Edges<f32>,
    /// `width`.
    pub width: Len,
    /// `height`.
    pub height: Len,
    /// `max-width` (`Auto` = none).
    pub max_width: Len,
    /// `max-height` (`Auto` = none).
    pub max_height: Len,
    /// `page-break-before`.
    pub break_before: PageBreak,
    /// `page-break-after`.
    pub break_after: PageBreak,
    /// `page-break-inside: avoid`.
    pub avoid_break_inside: bool,
    /// `vertical-align`.
    pub vertical_align: VAlign,
    /// `list-style-type`.
    pub list_style: ListStyle,
    /// `list-style-position: inside`.
    pub list_inside: bool,
    /// `text-transform`.
    pub text_transform: TextTransform,
    /// `text-decoration: underline`.
    pub underline: bool,
    /// `text-decoration: line-through`.
    pub line_through: bool,
    /// `hyphens`.
    pub hyphens: Hyphens,
    /// `letter-spacing` in device px.
    pub letter_spacing: f32,
    /// `word-spacing` in device px.
    pub word_spacing: f32,
    /// `visibility: hidden`.
    pub hidden: bool,
    /// Text gray level (0 = black) from `color`.
    pub color: u8,
    /// Language from `lang` / `xml:lang` (inherited), lowercase.
    pub lang: Option<Arc<str>>,
}

impl ComputedStyle {
    /// The initial style of the root with base font size `font_px`.
    pub fn root(font_px: f32) -> Self {
        ComputedStyle {
            display: Display::Block,
            white_space: WhiteSpace::Normal,
            float: 0,
            clear: false,
            font_family: Arc::from(vec!["serif".to_string()]),
            font_size: font_px,
            bold: false,
            italic: false,
            small_caps: false,
            line_height: LineHeight::Normal,
            text_align: TextAlign::Left,
            text_indent: Len::Px(0.0),
            margin: Edges::all(Len::Px(0.0)),
            padding: Edges::all(Len::Px(0.0)),
            border: Edges::all(0.0),
            width: Len::Auto,
            height: Len::Auto,
            max_width: Len::Auto,
            max_height: Len::Auto,
            break_before: PageBreak::Auto,
            break_after: PageBreak::Auto,
            avoid_break_inside: false,
            vertical_align: VAlign::Baseline,
            list_style: ListStyle::Disc,
            list_inside: false,
            text_transform: TextTransform::None,
            underline: false,
            line_through: false,
            hyphens: Hyphens::Auto,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            hidden: false,
            color: 0,
            lang: None,
        }
    }

    /// A child's starting point: inherited properties copied, the others initial.
    pub fn inherit_from(parent: &ComputedStyle) -> Self {
        ComputedStyle {
            display: Display::Inline,
            float: 0,
            clear: false,
            margin: Edges::all(Len::Px(0.0)),
            padding: Edges::all(Len::Px(0.0)),
            border: Edges::all(0.0),
            width: Len::Auto,
            height: Len::Auto,
            max_width: Len::Auto,
            max_height: Len::Auto,
            break_before: PageBreak::Auto,
            break_after: PageBreak::Auto,
            avoid_break_inside: false,
            vertical_align: VAlign::Baseline,
            // text-decoration is not inherited but propagates to inline descendants.
            ..parent.clone()
        }
    }

    /// True when the family list asks for a monospace face first.
    pub fn is_monospace(&self) -> bool {
        self.font_family.first().is_some_and(|f| {
            let f = f.to_ascii_lowercase();
            f == "monospace" || f.contains("mono") || f.contains("courier") || f == "consolas"
        })
    }
}

/// The user-agent stylesheet: HTML rendering defaults with book-like typography
/// (close to KOReader's `epub.css`).
pub const UA_CSS: &str = r#"
html, body, div, p, address, article, aside, blockquote, center, dd, details, dialog, dir, dl, dt,
fieldset, figcaption, figure, footer, form, h1, h2, h3, h4, h5, h6, header, hgroup, hr, legend,
listing, main, menu, nav, ol, plaintext, pre, search, section, summary, ul, xmp, canvas, video,
noscript, frameset, frame, iframe, noframes, optgroup, output, multicol, map, datalist, textarea,
math[display="block"] { display: block }
head, link, meta, script, style, title, template, base, basefont, param, area, track, source,
noembed, rp, col, colgroup { display: none }
[hidden] { display: none }
li { display: list-item }
table { display: table }
caption { display: table-caption }
thead, tbody, tfoot { display: table-row-group }
tr { display: table-row }
td, th { display: table-cell }
body { text-align: justify; margin: 0 }
p { text-indent: 1.2em; margin: 0 }
h1, h2, h3, h4, h5, h6 { font-weight: bold; margin-top: 0.7em; margin-bottom: 0.5em; hyphens: none;
  page-break-after: avoid; text-indent: 0; text-align: left }
h1 { font-size: 150% } h2 { font-size: 140% } h3 { font-size: 130% }
h4 { font-size: 120% } h5 { font-size: 110% } h6 { font-size: 100% }
blockquote { margin: 0.5em 1em 0.5em 2em }
figure { margin: 0.5em 1em }
pre, xmp, listing, plaintext, textarea { white-space: pre; font-family: monospace; text-align: left; margin: 0.5em 0;
  font-size: 85% }
code, kbd, samp, tt { font-family: monospace }
b, strong, th, dt { font-weight: bold }
i, em, cite, var, dfn, address { font-style: italic }
u, ins { text-decoration: underline }
s, strike, del { text-decoration: line-through }
small { font-size: smaller }
big { font-size: larger }
sub { vertical-align: sub; font-size: 70% }
sup { vertical-align: super; font-size: 70% }
ol, ul, menu, dir { margin: 0 0 0 1em; padding-left: 1.4em }
ul, menu, dir { list-style-type: disc }
ol { list-style-type: decimal }
ul ul, ol ul { list-style-type: circle }
ul ul ul, ol ul ul, ul ol ul { list-style-type: square }
dl { margin: 0.5em 0 }
dt { margin-top: 0.3em }
dd { margin-left: 1.3em }
hr { border-top: 1px solid; margin: 0.5em 0 }
table { font-size: 80%; margin: 3px 0; text-indent: 0 }
table table { font-size: 100% }
td, th { padding: 3px }
th { text-align: center }
table[border] td, table[border] th { border: 1px solid }
table[border="0"] td, table[border="0"] th { border: none }
caption { text-align: center; padding: 4px }
center { text-align: center }
a[href] { text-decoration: underline }
"#;

/// Origins in cascade order (later wins at equal importance).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    /// The built-in stylesheet.
    UserAgent = 0,
    /// HTML presentational attributes (`align`, `width`, ...).
    Hint = 1,
    /// The book's stylesheets.
    Author = 2,
    /// `style=""` attributes.
    Inline = 3,
}

struct IndexedRule {
    selector: Selector,
    decls: Arc<[Declaration]>,
    origin: Origin,
    order: u32,
}

/// Rules indexed by the key of their rightmost compound.
#[derive(Default)]
pub struct RuleSet {
    rules: Vec<IndexedRule>,
    by_id: HashMap<String, Vec<u32>>,
    by_class: HashMap<String, Vec<u32>>,
    by_tag: HashMap<String, Vec<u32>>,
    universal: Vec<u32>,
}

impl RuleSet {
    /// Adds every rule of `sheet` with `origin`.
    pub fn add_sheet(&mut self, sheet: &Stylesheet, origin: Origin) {
        for r in &sheet.rules {
            let decls: Arc<[Declaration]> = Arc::from(r.decls.clone());
            for s in &r.selectors {
                let i = self.rules.len() as u32;
                let key = &s.parts[0].0;
                if let Some(id) = key.ids.first() {
                    self.by_id.entry(id.clone()).or_default().push(i);
                } else if let Some(c) = key.classes.first() {
                    self.by_class.entry(c.clone()).or_default().push(i);
                } else if let Some(t) = &key.tag {
                    self.by_tag.entry(t.clone()).or_default().push(i);
                } else {
                    self.universal.push(i);
                }
                self.rules.push(IndexedRule { selector: s.clone(), decls: decls.clone(), origin, order: i });
            }
        }
    }

    /// Number of selector entries.
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// True when there are no rules.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Matching declarations for `e`, as (origin, specificity, order, decls).
    fn matching<E: SelectorElement>(&self, e: E, out: &mut Vec<(Origin, u32, u32, Arc<[Declaration]>)>) {
        let mut check = |ids: &[u32]| {
            for &i in ids {
                let r = &self.rules[i as usize];
                if css::matches(&r.selector, e) {
                    out.push((r.origin, r.selector.specificity, r.order, r.decls.clone()));
                }
            }
        };
        if let Some(id) = e.id() {
            if let Some(v) = self.by_id.get(id) {
                check(v);
            }
        }
        if let Some(cls) = e.attr("class") {
            let mut seen: Vec<&str> = Vec::new();
            for c in cls.split_ascii_whitespace() {
                if seen.contains(&c) {
                    continue;
                }
                seen.push(c);
                if let Some(v) = self.by_class.get(c) {
                    check(v);
                }
            }
        }
        if let Some(v) = self.by_tag.get(e.local_name()) {
            check(v);
        }
        check(&self.universal);
    }
}

thread_local! {
    static UA_RULES: Arc<RuleSet> = {
        let mut r = RuleSet::default();
        r.add_sheet(&css::parse_stylesheet(UA_CSS), Origin::UserAgent);
        Arc::new(r)
    };
}

/// The built-in rule set.
pub fn ua_rules() -> Arc<RuleSet> {
    UA_RULES.with(|r| r.clone())
}

/// What the cascade needs besides the rules.
#[derive(Clone, Debug, PartialEq)]
pub struct StyleContext {
    /// Base font size in device px (the user's size).
    pub font_px: f32,
    /// Page content width in device px (for `vw`).
    pub page_width: f32,
    /// Page content height in device px (for `vh`).
    pub page_height: f32,
}

impl StyleContext {
    fn px_scale(&self) -> f32 {
        self.font_px / 16.0
    }
}

/// Element data the cascade reads beyond selector matching.
pub trait StyledElement: SelectorElement {
    /// The `style` attribute.
    fn style_attr(&self) -> Option<&str> {
        self.attr("style")
    }
    /// Presentational hints (`align`, `width`, `height`, `valign`, `<font size>`) as declarations.
    fn hints(&self) -> Vec<Declaration> {
        let mut out = Vec::new();
        let name = self.local_name();
        if let Some(a) = self.attr("align") {
            let a = a.to_ascii_lowercase();
            if matches!(name, "img" | "table" | "object" | "embed") {
                if a == "left" || a == "right" {
                    out.push(Declaration { name: "float".into(), value: vec![Tok::Ident(a)], important: false });
                }
            } else if matches!(a.as_str(), "left" | "right" | "center" | "justify") {
                out.push(Declaration { name: "text-align".into(), value: vec![Tok::Ident(a)], important: false });
            }
        }
        if matches!(name, "img" | "image" | "svg" | "td" | "th" | "table" | "hr" | "col" | "object" | "embed" | "video") {
            for dim in ["width", "height"] {
                if let Some(v) = self.attr(dim) {
                    if let Some(t) = html_length(v) {
                        out.push(Declaration { name: dim.into(), value: vec![t], important: false });
                    }
                }
            }
        }
        if name == "font" {
            if let Some(s) = self.attr("size") {
                let pct = match s.trim() {
                    "1" | "-2" => 63.0,
                    "2" | "-1" => 82.0,
                    "4" | "+1" => 113.0,
                    "5" | "+2" => 150.0,
                    "6" | "+3" => 200.0,
                    "7" | "+4" => 300.0,
                    _ => 100.0,
                };
                out.push(Declaration { name: "font-size".into(), value: vec![Tok::Pct(pct)], important: false });
            }
            if let Some(f) = self.attr("face") {
                out.push(Declaration { name: "font-family".into(), value: vec![Tok::Str(f.to_string())], important: false });
            }
        }
        out
    }
    /// `lang` or `xml:lang`.
    fn lang(&self) -> Option<&str> {
        self.attr("lang")
    }
}

fn html_length(v: &str) -> Option<Tok> {
    let v = v.trim();
    if let Some(p) = v.strip_suffix('%') {
        return p.trim().parse::<f32>().ok().map(Tok::Pct);
    }
    let n = v.trim_end_matches("px").trim().parse::<f32>().ok()?;
    Some(Tok::Dim(n, "px".into()))
}

/// The cascade for one chapter: UA rules plus the book's rules.
pub struct Cascade {
    /// Built-in rules.
    pub ua: Arc<RuleSet>,
    /// The book's rules for this chapter (empty when publisher styles are off).
    pub author: Arc<RuleSet>,
    /// Use `style=""` attributes and presentational hints.
    pub publisher: bool,
    /// Units.
    pub ctx: StyleContext,
}

/// Properties needed while building the DOM (crengine's whitespace rules).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct StructStyle {
    /// `display`.
    pub display: Display,
    /// `white-space`.
    pub white_space: WhiteSpace,
    /// Floated.
    pub float: bool,
}

impl Cascade {
    fn gather<E: StyledElement>(&self, e: E) -> Vec<(u8, Origin, u32, u32, Declaration)> {
        let mut matched = Vec::new();
        self.ua.matching(e, &mut matched);
        if self.publisher {
            self.author.matching(e, &mut matched);
        }
        let mut decls: Vec<(u8, Origin, u32, u32, Declaration)> = Vec::new();
        for (origin, spec, order, ds) in matched {
            for d in ds.iter() {
                decls.push((d.important as u8, origin, spec, order, d.clone()));
            }
        }
        if self.publisher {
            for d in e.hints() {
                decls.push((0, Origin::Hint, 0, 0, d));
            }
            if let Some(s) = e.style_attr() {
                for (i, d) in css::parse_declarations(s).into_iter().enumerate() {
                    decls.push((d.important as u8, Origin::Inline, u32::MAX, i as u32, d));
                }
            }
        } else if matches!(e.local_name(), "img" | "image" | "svg") {
            // Image sizes from attributes stay meaningful without publisher styles.
            for d in e.hints() {
                if d.name == "width" || d.name == "height" {
                    decls.push((0, Origin::Hint, 0, 0, d));
                }
            }
        }
        // Stable sort: importance, then origin (important UA before author would be
        // reversed by CSS rules for user origin; there is none here), then specificity, order.
        decls.sort_by_key(|d| (d.0, d.1, d.2, d.3));
        decls
    }

    /// Only `display`, `white-space` and `float` (used while parsing).
    pub fn structure<E: StyledElement>(&self, e: E, parent_ws: WhiteSpace) -> StructStyle {
        let mut s = StructStyle { display: Display::Inline, white_space: parent_ws, float: false };
        for (_, _, _, _, d) in self.gather(e) {
            match d.name.as_str() {
                "display" => {
                    if let Some(v) = parse_display(&d.value) {
                        s.display = v;
                    }
                }
                "white-space" => {
                    if let Some(v) = parse_white_space(&d.value, parent_ws) {
                        s.white_space = v;
                    }
                }
                "float" => s.float = matches!(ident(&d.value).as_deref(), Some("left" | "right")),
                _ => {}
            }
        }
        if matches!(e.local_name(), "img" | "image") && s.display.is_block_like() && !matches!(s.display, Display::Block | Display::ListItem) {
            s.display = Display::Block;
        }
        s
    }

    /// The full computed style of `e` given its parent's.
    pub fn compute<E: StyledElement>(&self, e: E, parent: &ComputedStyle) -> ComputedStyle {
        let decls = self.gather(e);
        let mut s = ComputedStyle::inherit_from(parent);
        if let Some(l) = e.lang() {
            let l = l.trim().to_ascii_lowercase();
            s.lang = if l.is_empty() { None } else { Some(Arc::from(l.as_str())) };
        }
        // Pass 1: font size (everything in em depends on it).
        for (_, _, _, _, d) in &decls {
            match d.name.as_str() {
                "font-size" => {
                    if let Some(v) = self.font_size(&d.value, parent) {
                        s.font_size = v;
                    }
                }
                "font" => {
                    if let Some(sz) = font_shorthand_size(&d.value) {
                        if let Some(v) = self.font_size(&sz, parent) {
                            s.font_size = v;
                        }
                    }
                }
                _ => {}
            }
        }
        if s.font_size.is_nan() {
            s.font_size = parent.font_size;
        }
        s.font_size = s.font_size.clamp(self.ctx.font_px * 0.3, self.ctx.font_px * 5.0);
        for (_, _, _, _, d) in &decls {
            self.apply(&mut s, d, parent);
        }
        if matches!(e.local_name(), "img" | "image") && s.display.is_block_like() && !matches!(s.display, Display::Block | Display::ListItem) {
            s.display = Display::Block;
        }
        s
    }

    /// A length; hostile values are clamped to +-[`MAX_LEN_PX`] device px (percentages to
    /// +-[`MAX_LEN_PCT`]) and NaN is ignored, so layout coordinates stay finite and far
    /// from the integer limits of the canvas.
    fn len(&self, t: &Tok, font: f32) -> Option<Len> {
        Some(match self.len_raw(t, font)? {
            Len::Px(v) if v.is_nan() => return None,
            Len::Px(v) => Len::Px(v.clamp(-MAX_LEN_PX, MAX_LEN_PX)),
            Len::Pct(p) if p.is_nan() => return None,
            Len::Pct(p) => Len::Pct(p.clamp(-MAX_LEN_PCT, MAX_LEN_PCT)),
            Len::Auto => Len::Auto,
        })
    }

    fn len_raw(&self, t: &Tok, font: f32) -> Option<Len> {
        let px = self.ctx.px_scale();
        Some(match t {
            Tok::Num(n) if *n == 0.0 => Len::Px(0.0),
            Tok::Num(n) => Len::Px(n * px), // quirks: unitless = px
            Tok::Pct(p) => Len::Pct(*p),
            Tok::Dim(v, u) => Len::Px(match u.as_str() {
                "px" => v * px,
                "pt" => v * px * 4.0 / 3.0,
                "pc" => v * px * 16.0,
                "in" => v * px * 96.0,
                "cm" => v * px * 96.0 / 2.54,
                "mm" => v * px * 96.0 / 25.4,
                "q" => v * px * 96.0 / 101.6,
                "em" => v * font,
                "rem" => v * self.ctx.font_px,
                "ex" | "ch" => v * font * 0.5,
                "vw" => v * self.ctx.page_width / 100.0,
                "vh" => v * self.ctx.page_height / 100.0,
                "vmin" => v * self.ctx.page_width.min(self.ctx.page_height) / 100.0,
                "vmax" => v * self.ctx.page_width.max(self.ctx.page_height) / 100.0,
                _ => return None,
            }),
            Tok::Ident(i) if i.eq_ignore_ascii_case("auto") => Len::Auto,
            _ => return None,
        })
    }

    fn font_size(&self, v: &[Tok], parent: &ComputedStyle) -> Option<f32> {
        let base = self.ctx.font_px;
        let t = v.first()?;
        Some(match t {
            Tok::Ident(k) => match k.to_ascii_lowercase().as_str() {
                "xx-small" => base * 9.0 / 16.0,
                "x-small" => base * 10.0 / 16.0,
                "small" => base * 13.0 / 16.0,
                "medium" => base,
                "large" => base * 18.0 / 16.0,
                "x-large" => base * 24.0 / 16.0,
                "xx-large" => base * 32.0 / 16.0,
                "xxx-large" => base * 48.0 / 16.0,
                "smaller" => parent.font_size / 1.2,
                "larger" => parent.font_size * 1.2,
                "inherit" | "unset" => parent.font_size,
                "initial" => base,
                _ => return None,
            },
            Tok::Pct(p) => parent.font_size * p / 100.0,
            Tok::Dim(_, u) if u == "em" || u == "ex" || u == "ch" => match self.len(t, parent.font_size)? {
                Len::Px(v) => v,
                _ => return None,
            },
            _ => match self.len(t, parent.font_size)? {
                Len::Px(v) => v,
                _ => return None,
            },
        })
    }

    fn edges(&self, v: &[Tok], font: f32) -> Option<Edges<Len>> {
        let vals: Vec<Len> = v.iter().filter_map(|t| self.len(t, font)).collect();
        if vals.is_empty() || vals.len() != v.iter().filter(|t| !matches!(t, Tok::Comma)).count() {
            return None;
        }
        Some(match vals.len() {
            1 => Edges::all(vals[0]),
            2 => Edges { top: vals[0], right: vals[1], bottom: vals[0], left: vals[1] },
            3 => Edges { top: vals[0], right: vals[1], bottom: vals[2], left: vals[1] },
            _ => Edges { top: vals[0], right: vals[1], bottom: vals[2], left: vals[3] },
        })
    }

    fn border_width(&self, t: &Tok, font: f32) -> Option<f32> {
        match t {
            Tok::Ident(k) => match k.to_ascii_lowercase().as_str() {
                "thin" => Some(self.ctx.px_scale()),
                "medium" => Some(3.0 * self.ctx.px_scale()),
                "thick" => Some(5.0 * self.ctx.px_scale()),
                _ => None,
            },
            _ => match self.len(t, font)? {
                Len::Px(v) => Some(v.max(0.0)),
                _ => None,
            },
        }
    }

    /// `border` / `border-top` style shorthands: (width, visible).
    fn border_parts(&self, v: &[Tok], font: f32) -> (Option<f32>, Option<bool>) {
        let mut w = None;
        let mut visible = None;
        for t in v {
            if let Tok::Ident(k) = t {
                let k = k.to_ascii_lowercase();
                match k.as_str() {
                    "none" | "hidden" => visible = Some(false),
                    "solid" | "dotted" | "dashed" | "double" | "groove" | "ridge" | "inset" | "outset" => visible = Some(true),
                    _ => {
                        if let Some(x) = self.border_width(t, font) {
                            w = Some(x);
                        }
                    }
                }
            } else if let Some(x) = self.border_width(t, font) {
                w = Some(x);
            }
        }
        (w, visible)
    }

    fn apply(&self, s: &mut ComputedStyle, d: &Declaration, parent: &ComputedStyle) {
        let v = &d.value;
        let font = s.font_size;
        let kw = ident(v);
        if matches!(kw.as_deref(), Some("inherit")) {
            inherit_prop(s, parent, &d.name);
            return;
        }
        match d.name.as_str() {
            "display" => {
                if let Some(x) = parse_display(v) {
                    s.display = x;
                }
            }
            "white-space" => {
                if let Some(x) = parse_white_space(v, parent.white_space) {
                    s.white_space = x;
                }
            }
            "float" => {
                s.float = match kw.as_deref() {
                    Some("left") => -1,
                    Some("right") => 1,
                    _ => 0,
                }
            }
            "clear" => s.clear = matches!(kw.as_deref(), Some("left" | "right" | "both")),
            "font-family" => {
                let f = css::family_names(v);
                if !f.is_empty() {
                    s.font_family = Arc::from(f);
                }
            }
            "font-weight" => {
                if let Some(w) = css::parse_weight(v) {
                    s.bold = match kw.as_deref() {
                        Some("bolder") => true,
                        Some("lighter") => false,
                        _ => w >= 600,
                    };
                }
            }
            "font-style" => s.italic = matches!(kw.as_deref(), Some("italic" | "oblique")),
            "font-variant" => s.small_caps = matches!(kw.as_deref(), Some("small-caps")),
            "font" => {
                // style weight variant size[/line-height] family
                let mut i = 0;
                s.bold = false;
                s.italic = false;
                while i < v.len() {
                    match &v[i] {
                        Tok::Ident(k) => {
                            let k = k.to_ascii_lowercase();
                            match k.as_str() {
                                "italic" | "oblique" => s.italic = true,
                                "bold" | "bolder" => s.bold = true,
                                "small-caps" => s.small_caps = true,
                                "normal" | "lighter" => {}
                                _ => break,
                            }
                        }
                        Tok::Num(n) if *n >= 100.0 => s.bold = *n >= 600.0,
                        _ => break,
                    }
                    i += 1;
                }
                // size
                i += 1;
                if i < v.len() && v[i] == Tok::Slash {
                    if let Some(lh) = self.line_height(&v[i + 1..i + 2], font) {
                        s.line_height = lh;
                    }
                    i += 2;
                }
                if i < v.len() {
                    let f = css::family_names(&v[i..]);
                    if !f.is_empty() {
                        s.font_family = Arc::from(f);
                    }
                }
            }
            "font-size" => {}
            "line-height" => {
                if let Some(lh) = self.line_height(v, font) {
                    s.line_height = lh;
                }
            }
            "text-align" => {
                s.text_align = match kw.as_deref() {
                    Some("left" | "start") => TextAlign::Left,
                    Some("right" | "end") => TextAlign::Right,
                    Some("center" | "-webkit-center") => TextAlign::Center,
                    Some("justify") => TextAlign::Justify,
                    _ => s.text_align,
                }
            }
            "text-indent" => {
                if let Some(l) = v.first().and_then(|t| self.len(t, font)) {
                    s.text_indent = l;
                }
            }
            "margin" => {
                if let Some(e) = self.edges(v, font) {
                    s.margin = e;
                }
            }
            "padding" => {
                if let Some(e) = self.edges(v, font) {
                    s.padding = e;
                }
            }
            "margin-top" | "margin-right" | "margin-bottom" | "margin-left" | "padding-top" | "padding-right" | "padding-bottom"
            | "padding-left" => {
                if let Some(l) = v.first().and_then(|t| self.len(t, font)) {
                    let (edges, side) = d.name.split_once('-').unwrap();
                    let e = if edges == "margin" { &mut s.margin } else { &mut s.padding };
                    match side {
                        "top" => e.top = l,
                        "right" => e.right = l,
                        "bottom" => e.bottom = l,
                        _ => e.left = l,
                    }
                }
            }
            "margin-block-start" | "margin-block-end" | "margin-inline-start" | "margin-inline-end" => {
                if let Some(l) = v.first().and_then(|t| self.len(t, font)) {
                    match d.name.as_str() {
                        "margin-block-start" => s.margin.top = l,
                        "margin-block-end" => s.margin.bottom = l,
                        "margin-inline-start" => s.margin.left = l,
                        _ => s.margin.right = l,
                    }
                }
            }
            "padding-inline-start" => {
                if let Some(l) = v.first().and_then(|t| self.len(t, font)) {
                    s.padding.left = l;
                }
            }
            "border" => {
                let (w, vis) = self.border_parts(v, font);
                let width = match vis {
                    Some(false) => 0.0,
                    Some(true) => w.unwrap_or(3.0 * self.ctx.px_scale()),
                    None => w.unwrap_or(0.0).min(0.0),
                };
                s.border = Edges::all(width);
            }
            "border-top" | "border-right" | "border-bottom" | "border-left" => {
                let (w, vis) = self.border_parts(v, font);
                let width = match vis {
                    Some(false) => 0.0,
                    Some(true) => w.unwrap_or(3.0 * self.ctx.px_scale()),
                    None => 0.0,
                };
                match &d.name[7..] {
                    "top" => s.border.top = width,
                    "right" => s.border.right = width,
                    "bottom" => s.border.bottom = width,
                    _ => s.border.left = width,
                }
            }
            "border-width" => {
                let ws: Vec<f32> = v.iter().filter_map(|t| self.border_width(t, font)).collect();
                if !ws.is_empty() {
                    let get = |i: usize| ws[i.min(ws.len() - 1)];
                    let e = match ws.len() {
                        1 => Edges::all(ws[0]),
                        2 => Edges { top: ws[0], right: ws[1], bottom: ws[0], left: ws[1] },
                        3 => Edges { top: ws[0], right: ws[1], bottom: ws[2], left: ws[1] },
                        _ => Edges { top: get(0), right: get(1), bottom: get(2), left: get(3) },
                    };
                    // Only sides that already have a visible style keep a width.
                    s.border = Edges {
                        top: if s.border.top > 0.0 { e.top } else { 0.0 },
                        right: if s.border.right > 0.0 { e.right } else { 0.0 },
                        bottom: if s.border.bottom > 0.0 { e.bottom } else { 0.0 },
                        left: if s.border.left > 0.0 { e.left } else { 0.0 },
                    };
                }
            }
            "border-style" => {
                let styles: Vec<bool> = v
                    .iter()
                    .filter_map(|t| match t {
                        Tok::Ident(k) => Some(!matches!(k.to_ascii_lowercase().as_str(), "none" | "hidden")),
                        _ => None,
                    })
                    .collect();
                if !styles.is_empty() {
                    let get = |i: usize| styles[i.min(styles.len() - 1)];
                    let (t, r, b, l) = match styles.len() {
                        1 => (get(0), get(0), get(0), get(0)),
                        2 => (get(0), get(1), get(0), get(1)),
                        3 => (get(0), get(1), get(2), get(1)),
                        _ => (get(0), get(1), get(2), get(3)),
                    };
                    let def = 3.0 * self.ctx.px_scale();
                    let pick = |on: bool, cur: f32| if on { if cur > 0.0 { cur } else { def } } else { 0.0 };
                    s.border = Edges {
                        top: pick(t, s.border.top),
                        right: pick(r, s.border.right),
                        bottom: pick(b, s.border.bottom),
                        left: pick(l, s.border.left),
                    };
                }
            }
            "width" | "height" | "max-width" | "max-height" => {
                let l = match kw.as_deref() {
                    Some("none") => Some(Len::Auto),
                    _ => v.first().and_then(|t| self.len(t, font)),
                };
                if let Some(l) = l {
                    match d.name.as_str() {
                        "width" => s.width = l,
                        "height" => s.height = l,
                        "max-width" => s.max_width = l,
                        _ => s.max_height = l,
                    }
                }
            }
            "page-break-before" | "break-before" | "page-break-after" | "break-after" => {
                let b = match kw.as_deref() {
                    Some("always" | "page" | "left" | "right" | "recto" | "verso") => PageBreak::Always,
                    Some("avoid" | "avoid-page") => PageBreak::Avoid,
                    _ => PageBreak::Auto,
                };
                if d.name.ends_with("before") {
                    s.break_before = b;
                } else {
                    s.break_after = b;
                }
            }
            "page-break-inside" | "break-inside" => s.avoid_break_inside = matches!(kw.as_deref(), Some("avoid" | "avoid-page")),
            "vertical-align" => {
                s.vertical_align = match kw.as_deref() {
                    Some("sub") => VAlign::Sub,
                    Some("super") => VAlign::Super,
                    Some("middle") => VAlign::Middle,
                    Some("top" | "text-top") => VAlign::Top,
                    Some("bottom" | "text-bottom") => VAlign::Bottom,
                    Some(_) => VAlign::Baseline,
                    None => match v.first().and_then(|t| self.len(t, font)) {
                        Some(Len::Px(p)) => VAlign::Raise(p),
                        Some(Len::Pct(p)) => VAlign::Raise(p / 100.0 * font * 1.2),
                        _ => s.vertical_align,
                    },
                }
            }
            "list-style-type" | "list-style" => {
                for t in v {
                    if let Tok::Ident(k) = t {
                        let k = k.to_ascii_lowercase();
                        if let Some(ls) = list_style(&k) {
                            s.list_style = ls;
                        } else if k == "inside" {
                            s.list_inside = true;
                        } else if k == "outside" {
                            s.list_inside = false;
                        }
                    }
                }
            }
            "list-style-position" => s.list_inside = matches!(kw.as_deref(), Some("inside")),
            "text-transform" => {
                s.text_transform = match kw.as_deref() {
                    Some("uppercase") => TextTransform::Upper,
                    Some("lowercase") => TextTransform::Lower,
                    Some("capitalize") => TextTransform::Capitalize,
                    _ => TextTransform::None,
                }
            }
            "text-decoration" | "text-decoration-line" => {
                let words: Vec<String> = v.iter().filter_map(|t| if let Tok::Ident(k) = t { Some(k.to_ascii_lowercase()) } else { None }).collect();
                if words.iter().any(|w| w == "none") {
                    s.underline = false;
                    s.line_through = false;
                } else {
                    if words.iter().any(|w| w == "underline") {
                        s.underline = true;
                    }
                    if words.iter().any(|w| w == "line-through") {
                        s.line_through = true;
                    }
                }
            }
            "hyphens" | "-epub-hyphens" | "-webkit-hyphens" | "-moz-hyphens" | "adobe-hyphenate" => {
                s.hyphens = match kw.as_deref() {
                    Some("none") => Hyphens::None,
                    Some("manual") => Hyphens::Manual,
                    Some("auto" | "explicit") => Hyphens::Auto,
                    _ => s.hyphens,
                }
            }
            "letter-spacing" | "word-spacing" => {
                let px = match kw.as_deref() {
                    Some("normal") => Some(0.0),
                    _ => match v.first().and_then(|t| self.len(t, font)) {
                        Some(Len::Px(p)) => Some(p),
                        _ => None,
                    },
                };
                if let Some(p) = px {
                    if d.name == "letter-spacing" {
                        s.letter_spacing = p.clamp(-font * 0.2, font);
                    } else {
                        s.word_spacing = p.clamp(-font * 0.2, font * 2.0);
                    }
                }
            }
            "visibility" => s.hidden = matches!(kw.as_deref(), Some("hidden" | "collapse")),
            "color" => {
                if let Some(g) = parse_color(v) {
                    s.color = g;
                }
            }
            _ => {}
        }
    }

    fn line_height(&self, v: &[Tok], font: f32) -> Option<LineHeight> {
        let f = match v.first()? {
            Tok::Ident(k) if k.eq_ignore_ascii_case("normal") => return Some(LineHeight::Normal),
            Tok::Num(n) => *n,
            Tok::Pct(p) => p / 100.0,
            t => match self.len(t, font)? {
                Len::Px(px) => px / font.max(1.0),
                _ => return None,
            },
        };
        // At most 20 lines' worth (hostile values: 1e39, NaN).
        (!f.is_nan()).then_some(LineHeight::Factor(f.clamp(0.0, 20.0)))
    }
}

fn ident(v: &[Tok]) -> Option<String> {
    match v.first() {
        Some(Tok::Ident(s)) => Some(s.to_ascii_lowercase()),
        _ => None,
    }
}

fn inherit_prop(s: &mut ComputedStyle, p: &ComputedStyle, name: &str) {
    match name {
        "display" => s.display = p.display,
        "margin" => s.margin = p.margin,
        "padding" => s.padding = p.padding,
        "width" => s.width = p.width,
        "height" => s.height = p.height,
        "float" => s.float = p.float,
        "vertical-align" => s.vertical_align = p.vertical_align,
        "border" => s.border = p.border,
        _ => {}
    }
}

fn list_style(k: &str) -> Option<ListStyle> {
    Some(match k {
        "disc" => ListStyle::Disc,
        "circle" => ListStyle::Circle,
        "square" => ListStyle::Square,
        "decimal" => ListStyle::Decimal,
        "decimal-leading-zero" => ListStyle::DecimalLeadingZero,
        "lower-alpha" | "lower-latin" => ListStyle::LowerAlpha,
        "upper-alpha" | "upper-latin" => ListStyle::UpperAlpha,
        "lower-roman" => ListStyle::LowerRoman,
        "upper-roman" => ListStyle::UpperRoman,
        "lower-greek" => ListStyle::LowerGreek,
        "none" => ListStyle::None,
        _ => return None,
    })
}

/// `display` keyword.
pub fn parse_display(v: &[Tok]) -> Option<Display> {
    let k = ident(v)?;
    // `display: block flow` and similar two-value forms: first keyword decides.
    Some(match k.as_str() {
        "inline" | "ruby" | "run-in" | "contents" => Display::Inline,
        "inline-block" | "inline-flex" | "inline-grid" => Display::InlineBlock,
        "block" | "flex" | "grid" | "flow-root" | "-webkit-box" | "box" => Display::Block,
        "list-item" => Display::ListItem,
        "table" | "inline-table" => Display::Table,
        "table-row-group" | "table-header-group" | "table-footer-group" => Display::TableRowGroup,
        "table-row" => Display::TableRow,
        "table-cell" => Display::TableCell,
        "table-caption" => Display::TableCaption,
        "table-column" | "table-column-group" => Display::TableColumn,
        "none" => Display::None,
        _ => return None,
    })
}

fn parse_white_space(v: &[Tok], parent: WhiteSpace) -> Option<WhiteSpace> {
    Some(match ident(v)?.as_str() {
        "normal" => WhiteSpace::Normal,
        "nowrap" => WhiteSpace::NoWrap,
        "pre" => WhiteSpace::Pre,
        "pre-wrap" | "break-spaces" => WhiteSpace::PreWrap,
        "pre-line" => WhiteSpace::PreLine,
        "inherit" => parent,
        _ => return None,
    })
}

/// A color as a gray level for e-ink (0 = black, 255 = white).
pub fn parse_color(v: &[Tok]) -> Option<u8> {
    let (r, g, b) = match v.first()? {
        Tok::Hash(h) => {
            let h = h.as_bytes();
            let hex = |s: &[u8]| u8::from_str_radix(std::str::from_utf8(s).ok()?, 16).ok();
            match h.len() {
                3 | 4 => (hex(&[h[0], h[0]])?, hex(&[h[1], h[1]])?, hex(&[h[2], h[2]])?),
                6 | 8 => (hex(&h[0..2])?, hex(&h[2..4])?, hex(&h[4..6])?),
                _ => return None,
            }
        }
        Tok::Func(f, args) if f == "rgb" || f == "rgba" => {
            let n: Vec<f32> = args
                .iter()
                .filter_map(|t| match t {
                    Tok::Num(n) => Some(*n),
                    Tok::Pct(p) => Some(p * 2.55),
                    _ => None,
                })
                .collect();
            if n.len() < 3 {
                return None;
            }
            (n[0] as u8, n[1] as u8, n[2] as u8)
        }
        Tok::Ident(k) => match k.to_ascii_lowercase().as_str() {
            "black" | "navy" | "darkblue" | "maroon" | "darkred" | "darkgreen" | "currentcolor" | "inherit" => (0, 0, 0),
            "white" => (255, 255, 255),
            "gray" | "grey" => (128, 128, 128),
            "silver" | "lightgray" | "lightgrey" => (192, 192, 192),
            "darkgray" | "darkgrey" => (169, 169, 169),
            "dimgray" | "dimgrey" => (105, 105, 105),
            _ => (0, 0, 0),
        },
        _ => return None,
    };
    let y = (r as f32 * 0.299 + g as f32 * 0.587 + b as f32 * 0.114) as u8;
    // Light colors on white paper would vanish on e-ink: keep text at most mid-gray.
    Some(y.min(140))
}

fn font_shorthand_size(v: &[Tok]) -> Option<Vec<Tok>> {
    for t in v {
        match t {
            Tok::Ident(k) if matches!(k.to_ascii_lowercase().as_str(), "italic" | "oblique" | "bold" | "bolder" | "lighter" | "normal" | "small-caps") => {}
            Tok::Num(n) if *n >= 100.0 => {}
            Tok::Dim(..) | Tok::Pct(_) | Tok::Num(_) | Tok::Ident(_) => return Some(vec![t.clone()]),
            _ => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (name, attrs, parent) per element.
    type Tree<'a> = [(&'a str, &'a [(&'a str, &'a str)], Option<usize>)];

    /// A tiny element tree for cascade tests.
    #[derive(Clone, Copy)]
    struct E<'a> {
        tree: &'a Tree<'a>,
        i: usize,
    }

    impl SelectorElement for E<'_> {
        fn local_name(&self) -> &str {
            self.tree[self.i].0
        }
        fn attr(&self, name: &str) -> Option<&str> {
            self.tree[self.i].1.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
        }
        fn parent(&self) -> Option<Self> {
            self.tree[self.i].2.map(|p| E { tree: self.tree, i: p })
        }
        fn prev_sibling(&self) -> Option<Self> {
            let p = self.tree[self.i].2;
            (0..self.i).rev().find(|&j| self.tree[j].2 == p).map(|j| E { tree: self.tree, i: j })
        }
        fn next_sibling(&self) -> Option<Self> {
            let p = self.tree[self.i].2;
            (self.i + 1..self.tree.len()).find(|&j| self.tree[j].2 == p).map(|j| E { tree: self.tree, i: j })
        }
        fn is_root(&self) -> bool {
            self.tree[self.i].2.is_none()
        }
    }
    impl StyledElement for E<'_> {}

    fn cascade(css: &str) -> Cascade {
        let mut author = RuleSet::default();
        author.add_sheet(&css::parse_stylesheet(css), Origin::Author);
        Cascade {
            ua: ua_rules(),
            author: Arc::new(author),
            publisher: true,
            ctx: StyleContext { font_px: 32.0, page_width: 1000.0, page_height: 1500.0 },
        }
    }

    const TREE: &Tree = &[
        ("body", &[], None),
        ("div", &[("class", "chapter"), ("id", "c1")], Some(0)),
        ("p", &[("class", "first")], Some(1)),
        ("p", &[("style", "text-indent: 2em; color: #888")], Some(1)),
        ("span", &[("lang", "de")], Some(3)),
    ];

    fn styles(css: &str) -> Vec<ComputedStyle> {
        let c = cascade(css);
        let mut out: Vec<ComputedStyle> = Vec::new();
        for (i, item) in TREE.iter().enumerate() {
            let parent = item.2.map(|p| out[p].clone()).unwrap_or_else(|| ComputedStyle::root(32.0));
            out.push(c.compute(E { tree: TREE, i }, &parent));
        }
        out
    }

    #[test]
    fn cascade_specificity_inheritance_units() {
        let s = styles(
            "p { text-indent: 3em; text-align: center } .first { text-align: right } div > p:first-child { font-size: 50% }
             #c1 p { margin-top: 10px } div p { margin-top: 20px !important } div { font-family: 'X', serif; line-height: 1.5 }
             p + p { font-weight: bold }",
        );
        assert_eq!(s[1].display, Display::Block);
        assert_eq!(s[2].text_align, TextAlign::Right, "class beats type");
        assert_eq!(s[3].text_align, TextAlign::Center);
        assert_eq!(s[2].font_size, 16.0, "50% of 32");
        assert_eq!(s[2].text_indent, Len::Px(3.0 * 16.0), "em uses the element's own font size");
        assert_eq!(s[3].text_indent, Len::Px(64.0), "style attribute wins");
        assert_eq!(s[2].margin.top, Len::Px(20.0 * 2.0), "!important beats the id rule; 1px = 2 device px at 32px");
        assert_eq!(&*s[4].font_family, &["X".to_string(), "serif".to_string()][..], "inherited");
        assert_eq!(s[4].line_height, LineHeight::Factor(1.5));
        assert!(s[3].bold && !s[2].bold, "adjacent sibling");
        assert_eq!(s[4].lang.as_deref(), Some("de"));
        assert_eq!(s[3].color, 136);
        assert_eq!(s[4].display, Display::Inline);
    }

    #[test]
    fn publisher_styles_off_keeps_ua_only() {
        let mut c = cascade("p { text-align: center; display: none }");
        c.publisher = false;
        let s = c.compute(E { tree: TREE, i: 2 }, &ComputedStyle::root(32.0));
        assert_eq!(s.display, Display::Block);
        assert_eq!(s.text_align, ComputedStyle::root(32.0).text_align);
        assert_eq!(s.text_indent, Len::Px(1.2 * 32.0));
    }

    #[test]
    fn borders_boxes_breaks_lists() {
        let c = cascade(
            "div { border: 2px solid black; padding: 1em 2em; margin: auto; page-break-before: always; list-style: upper-roman inside }
             p { border-top: none; border-bottom: thin dotted; page-break-inside: avoid; vertical-align: super; text-transform: uppercase }",
        );
        let root = ComputedStyle::root(32.0);
        let d = c.compute(E { tree: TREE, i: 1 }, &root);
        assert_eq!(d.border, Edges::all(4.0));
        assert_eq!(d.padding.left, Len::Px(64.0));
        assert!(d.margin.left.is_auto());
        assert_eq!(d.break_before, PageBreak::Always);
        assert_eq!((d.list_style, d.list_inside), (ListStyle::UpperRoman, true));
        let p = c.compute(E { tree: TREE, i: 2 }, &d);
        assert_eq!((p.border.top, p.border.bottom), (0.0, 2.0));
        assert!(p.avoid_break_inside);
        assert_eq!(p.vertical_align, VAlign::Super);
        assert_eq!(p.text_transform, TextTransform::Upper);
        assert_eq!(p.list_style, ListStyle::UpperRoman, "inherited");
    }

    #[test]
    fn structure_only() {
        let c = cascade(".first { display: none } p { white-space: pre-wrap; float: left }");
        let s = c.structure(E { tree: TREE, i: 2 }, WhiteSpace::Normal);
        assert_eq!(s, StructStyle { display: Display::None, white_space: WhiteSpace::PreWrap, float: true });
        assert!(WhiteSpace::PreLine.parse_as_pre() && !WhiteSpace::NoWrap.parse_as_pre());
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color(&[Tok::Hash("000".into())]), Some(0));
        assert_eq!(parse_color(&[Tok::Hash("ffffff".into())]), Some(140));
        assert_eq!(parse_color(&[Tok::Func("rgb".into(), vec![Tok::Num(0.0), Tok::Comma, Tok::Num(0.0), Tok::Comma, Tok::Num(0.0)])]), Some(0));
    }
}
