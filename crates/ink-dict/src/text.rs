//! Plain text from HTML-like definition markup (StarDict types `h`, `x`, `g`, `k`).
//!
//! Tags are dropped; block tags (`br`, `p`, `div`, `li`, headings, table rows, XDXF `def`)
//! end a line; `<script>`/`<style>` content is dropped; entities are decoded. Spaces are
//! collapsed inside lines and empty lines are dropped, so the result is one line per block.
//! A newline in the text is a space for HTML ([`html_to_text`]) and a line break for the
//! other markups ([`markup_to_text`]).

const BLOCK: &[&str] = &["br", "p", "div", "li", "ul", "ol", "dl", "dt", "dd", "h1", "h2", "h3", "h4", "h5", "h6", "tr", "table", "blockquote", "hr", "section", "article", "def", "pre"];

/// Converts HTML to plain text (newlines in the text are spaces).
pub fn html_to_text(s: &str) -> String {
    convert(s, false)
}

/// Converts XDXF, Pango or other XML-like markup to plain text (newlines kept).
pub fn markup_to_text(s: &str) -> String {
    convert(s, true)
}

fn convert(s: &str, keep_newlines: bool) -> String {
    let mut out = String::with_capacity(s.len());
    let push_text = |out: &mut String, t: &str| {
        if keep_newlines || !t.contains('\n') {
            push_decoded(out, t)
        } else {
            push_decoded(out, &t.replace('\n', " "))
        }
    };
    let mut rest = s;
    while let Some(lt) = rest.find('<') {
        push_text(&mut out, &rest[..lt]);
        let after = &rest[lt + 1..];
        let Some(gt) = after.find('>') else {
            push_text(&mut out, &rest[lt..]);
            rest = "";
            break;
        };
        let tag = &after[..gt];
        rest = &after[gt + 1..];
        if tag.starts_with('!') || tag.starts_with('?') {
            continue;
        }
        let closing = tag.starts_with('/');
        let body = tag.trim_start_matches('/');
        let name = &body[..body.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(body.len())];
        let is = |n: &&str| name.eq_ignore_ascii_case(n);
        if !closing && ["script", "style", "rref"].iter().any(is) {
            // Raw text and XDXF resource references (sound files): skip to the closing tag.
            let close = format!("</{name}");
            rest = find_ignore_case(rest, &close).map_or("", |n| &rest[n..]);
        } else if !closing && is(&"li") {
            out.push_str("\n• ");
        } else if BLOCK.iter().any(is) {
            out.push('\n');
        }
    }
    push_text(&mut out, rest);
    tidy(&out)
}

/// Byte position of the first `needle` (ASCII, starting with an ASCII char) in `hay`,
/// ignoring ASCII case.
fn find_ignore_case(hay: &str, needle: &str) -> Option<usize> {
    hay.as_bytes().windows(needle.len()).position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

/// Appends `t` to `out` with its character references decoded; unknown or malformed
/// ones are kept as written.
pub(crate) fn push_decoded(out: &mut String, t: &str) {
    let mut rest = t;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        match after.find(';').filter(|n| *n <= 10).and_then(|n| decode_entity(&after[..n]).map(|c| (n, c))) {
            Some((n, c)) => {
                out.push(c);
                rest = &after[n + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
}

fn decode_entity(e: &str) -> Option<char> {
    if let Some(num) = e.strip_prefix('#') {
        let n = if let Some(h) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) { u32::from_str_radix(h, 16).ok()? } else { num.parse().ok()? };
        return char::from_u32(n);
    }
    Some(match e {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "middot" => '·',
        "bull" => '•',
        "deg" => '°',
        "copy" => '©',
        "eacute" => 'é',
        "egrave" => 'è',
        "aacute" => 'á',
        "uuml" => 'ü',
        "ouml" => 'ö',
        "auml" => 'ä',
        _ => return None,
    })
}

/// Collapses spaces per line, trims lines, drops empty lines.
fn tidy(s: &str) -> String {
    let lines: Vec<String> = s.split('\n').map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|l| !l.is_empty()).collect();
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_entities_and_lines() {
        assert_eq!(html_to_text("<b>run</b> &amp; <i>go</i><br>verb"), "run & go\nverb");
        assert_eq!(html_to_text("<p>one</p><p>two</p>"), "one\ntwo");
        assert_eq!(html_to_text("<ol><li>a</li><li>b</li></ol>"), "• a\n• b");
        assert_eq!(html_to_text("x<style>p{}</style>y<script>1<2</script>z"), "xyz");
        assert_eq!(html_to_text("a<SCRIPT>x</Script>b<rref>s.wav"), "ab");
        assert_eq!(html_to_text("caf&eacute; &#233; &#xE9; &bogus; a & b"), "café é é &bogus; a & b");
        assert_eq!(html_to_text("cut <b"), "cut <b");
        assert_eq!(markup_to_text("<k>word</k>\n<def>first</def>"), "word\nfirst");
        assert_eq!(markup_to_text("a\n\n\n\nb"), "a\nb");
        assert_eq!(markup_to_text("[ˈrʌn] <rref>z_run__gb_1.wav</rref> NAmE"), "[ˈrʌn] NAmE");
        assert_eq!(html_to_text("a\nb"), "a b");
    }
}
