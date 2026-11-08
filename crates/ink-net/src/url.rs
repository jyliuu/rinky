//! URL helpers without the `url` crate: RFC 3986 reference resolution (section 5.2),
//! origins for redirect checks, percent-encoding and path joining.
//!
//! URLs are handled as strings. Only what rinky needs is implemented: resolving OPDS
//! `href`s and `Location` headers against the URL a document was fetched from.

/// The five components of a URI reference (RFC 3986 section 3). `None` = absent,
/// `Some("")` = present but empty (`http://a/b?` has an empty query).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parts<'a> {
    /// Scheme without the colon.
    pub scheme: Option<&'a str>,
    /// Authority without the leading `//`.
    pub authority: Option<&'a str>,
    /// Path (may be empty).
    pub path: &'a str,
    /// Query without the `?`.
    pub query: Option<&'a str>,
    /// Fragment without the `#`.
    pub fragment: Option<&'a str>,
}

fn scheme_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.first().is_none_or(|c| !c.is_ascii_alphabetic()) {
        return None;
    }
    for (i, &c) in b.iter().enumerate().skip(1) {
        match c {
            b':' => return Some(i),
            c if c.is_ascii_alphanumeric() || c == b'+' || c == b'-' || c == b'.' => {}
            _ => return None,
        }
    }
    None
}

/// Splits a URI reference into its components.
pub fn parse(s: &str) -> Parts<'_> {
    let mut rest = s;
    let mut p = Parts::default();
    if let Some(i) = rest.find('#') {
        p.fragment = Some(&rest[i + 1..]);
        rest = &rest[..i];
    }
    if let Some(i) = rest.find('?') {
        p.query = Some(&rest[i + 1..]);
        rest = &rest[..i];
    }
    if let Some(n) = scheme_len(rest) {
        p.scheme = Some(&rest[..n]);
        rest = &rest[n + 1..];
    }
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find('/').unwrap_or(after.len());
        p.authority = Some(&after[..end]);
        rest = &after[end..];
    }
    p.path = rest;
    p
}

/// RFC 3986 section 5.2.4.
///
/// Linear in the path length: the input is a cursor that only moves forward (`/./x` and
/// `/../x` advance by 2 or 3 bytes and keep their `/`), and removing the last output
/// segment scans only that segment, which is then dropped.
pub fn remove_dot_segments(path: &str) -> String {
    let mut input = path;
    let mut out = String::with_capacity(path.len());
    while !input.is_empty() {
        if let Some(r) = input.strip_prefix("../") {
            input = r;
        } else if let Some(r) = input.strip_prefix("./") {
            input = r;
        } else if input.starts_with("/./") {
            input = &input[2..];
        } else if input == "/." {
            input = "/";
        } else if input.starts_with("/../") || input == "/.." {
            input = if input == "/.." { "/" } else { &input[3..] };
            match out.rfind('/') {
                Some(i) => out.truncate(i),
                None => out.clear(),
            }
        } else if input == "." || input == ".." {
            input = "";
        } else {
            let start = usize::from(input.starts_with('/'));
            let end = input[start..].find('/').map(|i| i + start).unwrap_or(input.len());
            out.push_str(&input[..end]);
            input = &input[end..];
        }
    }
    out
}

fn merge(base: &Parts<'_>, rel_path: &str) -> String {
    if base.authority.is_some() && base.path.is_empty() {
        format!("/{rel_path}")
    } else {
        match base.path.rfind('/') {
            Some(i) => format!("{}{}", &base.path[..=i], rel_path),
            None => rel_path.to_string(),
        }
    }
}

fn compose(scheme: Option<&str>, authority: Option<&str>, path: &str, query: Option<&str>, fragment: Option<&str>) -> String {
    let mut s = String::new();
    if let Some(sc) = scheme {
        s.push_str(sc);
        s.push(':');
    }
    if let Some(a) = authority {
        s.push_str("//");
        s.push_str(a);
    }
    s.push_str(path);
    if let Some(q) = query {
        s.push('?');
        s.push_str(q);
    }
    if let Some(f) = fragment {
        s.push('#');
        s.push_str(f);
    }
    s
}

/// Resolves `reference` against `base` (RFC 3986 section 5.2.2, strict).
///
/// References with a scheme other than http(s) (`data:`, `mailto:`, `urn:`) are returned
/// unchanged. Surrounding whitespace in `reference` is ignored.
pub fn resolve(base: &str, reference: &str) -> String {
    let reference = reference.trim();
    let r = parse(reference);
    if let Some(sc) = r.scheme {
        if !matches!(sc.to_ascii_lowercase().as_str(), "http" | "https") {
            return reference.to_string();
        }
        return compose(r.scheme, r.authority, &remove_dot_segments(r.path), r.query, r.fragment);
    }
    let b = parse(base);
    let (authority, path, query);
    if r.authority.is_some() {
        authority = r.authority;
        path = remove_dot_segments(r.path);
        query = r.query;
    } else {
        authority = b.authority;
        if r.path.is_empty() {
            path = b.path.to_string();
            query = r.query.or(b.query);
        } else {
            path = if r.path.starts_with('/') { remove_dot_segments(r.path) } else { remove_dot_segments(&merge(&b, r.path)) };
            query = r.query;
        }
    }
    compose(b.scheme, authority, &path, query, r.fragment)
}

/// `(scheme, host, port)` with lowercase scheme and host and the default port filled in.
/// `None` for URLs without an authority.
pub fn origin(url: &str) -> Option<(String, String, u16)> {
    let p = parse(url);
    let scheme = p.scheme?.to_ascii_lowercase();
    let auth = p.authority?;
    let hostport = auth.rsplit_once('@').map(|(_, h)| h).unwrap_or(auth);
    let (host, port) = if let Some(rest) = hostport.strip_prefix('[') {
        // [v6]:port
        let end = rest.find(']')?;
        let host = &hostport[..end + 2];
        let port = rest[end + 1..].strip_prefix(':').and_then(|p| p.parse().ok());
        (host, port)
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (h, p.parse().ok()),
            None => (hostport, None),
        }
    };
    let default = match scheme.as_str() {
        "https" => 443,
        "http" => 80,
        _ => 0,
    };
    Some((scheme, host.to_ascii_lowercase(), port.unwrap_or(default)))
}

/// True when both URLs have the same scheme, host and port.
pub fn same_origin(a: &str, b: &str) -> bool {
    match (origin(a), origin(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Percent-encodes everything except the RFC 3986 unreserved characters
/// (`A-Z a-z 0-9 - . _ ~`). Spaces become `%20`.
pub fn encode_component(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.extend(['%', HEX[usize::from(b >> 4)] as char, HEX[usize::from(b & 15)] as char]);
        }
    }
    out
}

/// Decodes `%XX` escapes (invalid escapes are kept as they are; bytes that are not UTF-8
/// are replaced).
pub fn decode_component(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let h = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(v) = h {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Joins a base URL and a path with exactly one `/` between them
/// (`https://h/sync/` + `/users/auth` = `https://h/sync/users/auth`).
pub fn join_path(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'))
}

/// The last non-empty path segment of `url`, percent-decoded (`""` if none).
pub fn last_segment(url: &str) -> String {
    let p = parse(url);
    p.path.rsplit('/').find(|s| !s.is_empty()).map(decode_component).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3986_normal_examples() {
        let base = "http://a/b/c/d;p?q";
        let cases = [
            ("g:h", "g:h"),
            ("g", "http://a/b/c/g"),
            ("./g", "http://a/b/c/g"),
            ("g/", "http://a/b/c/g/"),
            ("/g", "http://a/g"),
            ("//g", "http://g"),
            ("?y", "http://a/b/c/d;p?y"),
            ("g?y", "http://a/b/c/g?y"),
            ("#s", "http://a/b/c/d;p?q#s"),
            ("g#s", "http://a/b/c/g#s"),
            ("g?y#s", "http://a/b/c/g?y#s"),
            (";x", "http://a/b/c/;x"),
            ("g;x", "http://a/b/c/g;x"),
            ("g;x?y#s", "http://a/b/c/g;x?y#s"),
            ("", "http://a/b/c/d;p?q"),
            (".", "http://a/b/c/"),
            ("./", "http://a/b/c/"),
            ("..", "http://a/b/"),
            ("../", "http://a/b/"),
            ("../g", "http://a/b/g"),
            ("../..", "http://a/"),
            ("../../", "http://a/"),
            ("../../g", "http://a/g"),
        ];
        for (r, want) in cases {
            assert_eq!(resolve(base, r), want, "reference {r:?}");
        }
    }

    #[test]
    fn rfc3986_abnormal_examples() {
        let base = "http://a/b/c/d;p?q";
        let cases = [
            ("../../../g", "http://a/g"),
            ("../../../../g", "http://a/g"),
            ("/./g", "http://a/g"),
            ("/../g", "http://a/g"),
            ("g.", "http://a/b/c/g."),
            (".g", "http://a/b/c/.g"),
            ("g..", "http://a/b/c/g.."),
            ("..g", "http://a/b/c/..g"),
            ("./../g", "http://a/b/g"),
            ("./g/.", "http://a/b/c/g/"),
            ("g/./h", "http://a/b/c/g/h"),
            ("g/../h", "http://a/b/c/h"),
            ("g;x=1/./y", "http://a/b/c/g;x=1/y"),
            ("g;x=1/../y", "http://a/b/c/y"),
            ("g?y/./x", "http://a/b/c/g?y/./x"),
            ("g?y/../x", "http://a/b/c/g?y/../x"),
            ("g#s/./x", "http://a/b/c/g#s/./x"),
            ("g#s/../x", "http://a/b/c/g#s/../x"),
            ("http:g", "http:g"),
        ];
        for (r, want) in cases {
            assert_eq!(resolve(base, r), want, "reference {r:?}");
        }
    }

    /// The earlier (quadratic) transcription of RFC 3986 section 5.2.4, kept as the
    /// reference for the linear version.
    fn remove_dot_segments_reference(path: &str) -> String {
        let mut input = path.to_string();
        let mut out = String::new();
        while !input.is_empty() {
            if let Some(r) = input.strip_prefix("../") {
                input = r.to_string();
            } else if let Some(r) = input.strip_prefix("./") {
                input = r.to_string();
            } else if input.starts_with("/./") {
                input.replace_range(..3, "/");
            } else if input == "/." {
                input = "/".into();
            } else if input.starts_with("/../") || input == "/.." {
                if input == "/.." {
                    input = "/".into();
                } else {
                    input.replace_range(..4, "/");
                }
                match out.rfind('/') {
                    Some(i) => out.truncate(i),
                    None => out.clear(),
                }
            } else if input == "." || input == ".." {
                input.clear();
            } else {
                let start = usize::from(input.starts_with('/'));
                let end = input[start..].find('/').map(|i| i + start).unwrap_or(input.len());
                out.push_str(&input[..end]);
                input.replace_range(..end, "");
            }
        }
        out
    }

    #[test]
    fn dot_segments_match_the_reference_on_every_short_path() {
        // Every string over {a, ., /} up to 8 characters (9841 paths).
        let alphabet = ['a', '.', '/'];
        let mut paths = vec![String::new()];
        let mut level = vec![String::new()];
        for _ in 0..8 {
            level = level.iter().flat_map(|p| alphabet.iter().map(move |c| format!("{p}{c}"))).collect();
            paths.extend(level.iter().cloned());
        }
        assert_eq!(paths.len(), 9841);
        for p in &paths {
            assert_eq!(remove_dot_segments(p), remove_dot_segments_reference(p), "path {p:?}");
        }
    }

    #[test]
    fn opds_style_references() {
        let base = "https://www.gutenberg.org/ebooks.opds/";
        assert_eq!(resolve(base, "/ebooks/search.opds/?sort_order=downloads"), "https://www.gutenberg.org/ebooks/search.opds/?sort_order=downloads");
        assert_eq!(resolve(base, "data:image/png;base64,iVBO/./x"), "data:image/png;base64,iVBO/./x");
        assert_eq!(resolve("http://h:8080/opds/root.xml", "new.xml"), "http://h:8080/opds/new.xml");
        assert_eq!(resolve("http://h/opds", " sub?page=2 "), "http://h/sub?page=2");
        assert_eq!(resolve("http://h", "x"), "http://h/x");
    }

    #[test]
    fn origins() {
        assert_eq!(origin("HTTPS://Example.COM/x"), Some(("https".into(), "example.com".into(), 443)));
        assert_eq!(origin("http://u:p@h:8080/"), Some(("http".into(), "h".into(), 8080)));
        assert_eq!(origin("http://[::1]:9/"), Some(("http".into(), "[::1]".into(), 9)));
        assert!(same_origin("http://h/a", "http://H:80/b"));
        assert!(!same_origin("http://h/a", "https://h/a"));
        assert!(!same_origin("http://h/a", "http://g/a"));
        assert!(!same_origin("data:x", "data:x"));
    }

    #[test]
    fn encoding() {
        assert_eq!(encode_component("a b&c/ø~"), "a%20b%26c%2F%C3%B8~");
        assert_eq!(decode_component("a%20b%2Fc%C3%B8%zz%"), "a b/cø%zz%");
        assert_eq!(join_path("https://h/sync/", "/users/auth"), "https://h/sync/users/auth");
        assert_eq!(join_path("https://h", "syncs/progress"), "https://h/syncs/progress");
        assert_eq!(last_segment("https://h/cache/epub/1342/pg1342-images-3.epub?x=1"), "pg1342-images-3.epub");
        assert_eq!(last_segment("https://h/a%20b/"), "a b");
        assert_eq!(last_segment("https://h"), "");
    }
}
