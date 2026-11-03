use super::*;
use crate::download::{sha256_hex, Hooks};
use crate::testserver::{file_bytes, TestServer};
use crate::UreqClient;

const PG_ROOT: &str = include_str!("../../testdata/opds/gutenberg-root.xml");
const PG_SEARCH: &str = include_str!("../../testdata/opds/gutenberg-search-austen.xml");
const PG_1342: &str = include_str!("../../testdata/opds/gutenberg-1342.xml");
const PG_OSD: &str = include_str!("../../testdata/opds/gutenberg-osd.xml");
const EDGE: &str = include_str!("../../testdata/opds/edge-cases.xml");
const TC_HOME: &str = include_str!("../../testdata/opds/test-catalog-home.json");
const TC_NAV: &str = include_str!("../../testdata/opds/test-catalog-navigation.json");
const TC_PUBS: &str = include_str!("../../testdata/opds/test-catalog-publications.json");

#[test]
fn gutenberg_navigation_root() {
    let f = parse_feed(PG_ROOT, "https://www.gutenberg.org/ebooks.opds/").unwrap();
    assert_eq!(f.title, "Project Gutenberg");
    assert_eq!(f.version, OpdsVersion::Atom1);
    assert_eq!(f.kind(), FeedKind::Navigation);
    let titles: Vec<_> = f.entries.iter().map(|e| e.title.as_str()).collect();
    assert_eq!(titles, vec!["Popular", "Latest", "Random"]);
    assert_eq!(f.entries[0].navigation().unwrap().href, "https://www.gutenberg.org/ebooks/search.opds/?sort_order=downloads");
    assert_eq!(f.entries[0].description(), Some("Our most popular books."));
    // data: thumbnails pass through unresolved.
    assert!(f.entries[0].thumbnail().unwrap().href.starts_with("data:image/png;base64,"));
    assert_eq!(f.search(), Some(SearchLink::Description("https://www.gutenberg.org/catalog/osd-books.xml".into())));
    assert_eq!(f.start_url(), Some("https://www.gutenberg.org/ebooks.opds/"));
    assert_eq!(f.icon.as_deref(), Some("https://www.gutenberg.org/gutenberg/favicon.ico"));
}

#[test]
fn gutenberg_search_results_paginate() {
    let f = parse_feed(PG_SEARCH, "https://www.gutenberg.org/ebooks/search.opds/?query=austen").unwrap();
    assert_eq!(f.title, "Books: austen");
    assert_eq!(f.entries.len(), 27);
    assert_eq!(f.entries[2].title, "Pride and Prejudice");
    assert_eq!(f.next_url(), Some("https://www.gutenberg.org/ebooks/search.opds/?query=austen&start_index=26"));
    assert_eq!(f.items_per_page, Some(25));
    assert_eq!(f.start_index, Some(1));
    assert!(f.entries.iter().all(|e| e.navigation().is_some()), "every result leads somewhere");
    assert!(f.entries.iter().all(|e| !e.authors.contains(&"Project Gutenberg".to_string())), "feed author is not an entry author");
}

#[test]
fn gutenberg_book_acquisitions() {
    let f = parse_feed(PG_1342, "https://www.gutenberg.org/ebooks/1342.opds").unwrap();
    assert_eq!(f.kind(), FeedKind::Acquisition);
    assert_eq!(f.entries.len(), 2);
    let e = &f.entries[0];
    assert_eq!(e.title, "Pride and Prejudice");
    assert_eq!(e.id, "urn:gutenberg:1342:2");
    assert_eq!(e.authors, vec!["Austen, Jane"]);
    assert_eq!(e.language.as_deref(), Some("en"));
    assert_eq!(e.published.as_deref(), Some("1998-06-01T00:00:00+00:00"));
    assert!(e.categories.contains(&"England -- Fiction".to_string()));
    assert!(e.categories.contains(&"Language and Literatures: English literature".to_string()));
    let content = e.content.as_deref().unwrap();
    assert!(content.starts_with("This edition had all images removed.\nTitle: Pride and Prejudice"), "{content}");
    assert!(content.contains("(8th & 9th grade)"));
    assert!(!content.contains('<'));
    let acq = e.acquisitions();
    assert_eq!(acq.len(), 2);
    assert_eq!(f.entries[1].acquisitions().len(), 4);
    assert_eq!(f.entries[1].best_acquisition().unwrap().href, "https://www.gutenberg.org/ebooks/1342.epub3.images");
    let best = e.best_acquisition().unwrap();
    assert_eq!(best.mime, "application/epub+zip");
    assert!(best.href.starts_with("https://www.gutenberg.org/ebooks/1342.epub"));
    assert!(acq.iter().any(|l| l.length == Some(561102) && l.title.as_deref() == Some("EPUB (older e-readers, no images)")));
    assert_eq!(e.cover().unwrap().href, "https://www.gutenberg.org/cache/epub/1342/pg1342.cover.medium.jpg");
    assert_eq!(e.thumbnail().unwrap().href, "https://www.gutenberg.org/cache/epub/1342/pg1342.cover.small.jpg");
    assert!(e.links.iter().any(|l| l.rel == "related" && l.href == "https://www.gutenberg.org/ebooks/author/68.opds"));
    assert!(e.is_book());
}

#[test]
fn gutenberg_opensearch_description() {
    let urls = parse_opensearch(PG_OSD, "https://www.gutenberg.org/catalog/osd-books.xml").unwrap();
    assert_eq!(urls.len(), 3);
    let t = pick_search_template(&urls).unwrap();
    assert_eq!(t, "http://m.gutenberg.org/ebooks/search.opds/?query={searchTerms}");
    assert_eq!(fill_template(t, " jane austen "), "http://m.gutenberg.org/ebooks/search.opds/?query=jane%20austen");
    assert!(parse_opensearch(PG_ROOT, "http://x/").is_err());
}

#[test]
fn edge_cases_from_real_catalogs() {
    let f = parse_feed(EDGE, "https://cat.example/opds/root.xml").unwrap();
    assert_eq!(f.title, "Edge & cases feed", "whitespace, NBSP included, collapses in titles");
    assert_eq!(f.icon.as_deref(), Some("https://cat.example/favicon.ico"));
    assert_eq!((f.total_results, f.items_per_page, f.start_index), (Some(40), Some(4), Some(5)));
    // Single-quoted attributes, relative href.
    assert_eq!(f.next_url(), Some("https://cat.example/opds/page2.xml"));
    // An OpenSearch description wins over the Atom template, as in KOReader.
    assert_eq!(f.search(), Some(SearchLink::Description("https://cat.example/opds-opensearch.xml".into())));
    let facets = f.facets();
    assert_eq!(facets.len(), 2);
    assert_eq!(facets[0].0, "Sort");
    assert_eq!(facets[0].1.len(), 2);
    assert!(facets[0].1[0].active_facet);
    assert_eq!(facets[0].1[0].count, Some(12));
    assert_eq!(facets[0].1[0].href, "https://cat.example/opds/root.xml?sort=title");
    assert_eq!(facets[1].0, "Sprog");

    let titles: Vec<_> = f.entries.iter().map(|e| e.title.as_str()).collect();
    assert_eq!(
        titles,
        vec![
            "First",
            "Second",
            "Двадцать тысяч льё под водой",
            "HTML and XHTML content",
            "CDATA <title> & co",
            "Navigation without rel",
            "Unclosed child",
            "After the broken one"
        ]
    );
    // Self-closing content does not swallow the next entry.
    assert_eq!(f.entries[0].content, None);
    assert_eq!(f.entries[0].thumbnail().unwrap().href, "https://cat.example/opds/FIRST_IMG");
    assert_eq!(f.entries[1].thumbnail().unwrap().href, "https://cat.example/opds/SECOND_IMG");
    assert_eq!(f.entries[1].content.as_deref(), Some("A description."));
    // Non-ASCII attributes and names.
    let e = &f.entries[2];
    assert_eq!(e.categories, vec!["Приключения", "fiction"]);
    assert_eq!(e.links[0].title.as_deref(), Some("Все книги серии «Капитан Немо»"));
    assert_eq!(e.authors, vec!["Жюль Верн", "Second Author"]);
    assert_eq!(e.best_acquisition().unwrap().href, "https://cat.example/get/third.fb2.zip");
    // HTML summary, XHTML content, dc fields, price and indirect acquisition.
    let e = &f.entries[3];
    assert_eq!(e.summary.as_deref(), Some("Line one & more\nLine two here"));
    assert_eq!(e.content.as_deref(), Some("Para one\nPara two\nafter break"));
    assert_eq!(e.language.as_deref(), Some("da"));
    assert_eq!(e.publisher.as_deref(), Some("Forlaget"));
    assert_eq!(e.published.as_deref(), Some("1913"));
    let buy = e.links.iter().find(|l| l.rel.ends_with("/buy")).unwrap();
    assert_eq!(buy.price.as_deref(), Some("49.95 DKK"));
    assert_eq!(buy.indirect, vec!["application/vnd.adobe.adept+xml", "application/epub+zip"]);
    assert_eq!(buy.final_mime(), "application/epub+zip");
    // The free PDF beats the paid EPUB and the sample.
    assert_eq!(e.best_acquisition().unwrap().href, "https://cat.example/free/4.pdf");
    // CDATA and a PDF link with a query string; page streaming counts as a book.
    let e = &f.entries[4];
    assert_eq!(e.summary.as_deref(), Some("Summary with <b>markup</b> kept as text"));
    assert_eq!(e.links[0].href, "http://example.org/books/file.pdf?opds");
    assert!(e.acquisitions().is_empty());
    assert!(e.is_book());
    assert_eq!(e.links[1].count, Some(24));
    // A catalog link without rel is the navigation target, not the facet before it.
    assert_eq!(f.entries[5].navigation().unwrap().href, "https://cat.example/opds/author/75357");
    // An unclosed <link> does not break the entries after it.
    assert_eq!(f.entries[6].navigation().unwrap().href, "https://cat.example/opds/sub/7");
    assert_eq!(f.entries[7].navigation().unwrap().href, "https://cat.example/opds/sub/8");
    assert_eq!(f.kind(), FeedKind::Acquisition);
}

#[test]
fn broken_markup_inside_captured_text() {
    // A missing </content>: </entry> ends the capture and the entry; the next entry and
    // its acquisition link are intact. A stray </b> inside a summary is ignored.
    let xml = r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>T</title>
<entry><title>A</title><content type="xhtml"><div>one<br>two</div></entry>
<entry><title>B</title><summary>left</b> right</summary>
<link rel="http://opds-spec.org/acquisition" href="b.epub" type="application/epub+zip"/></entry>
</feed>"#;
    let f = parse_feed(xml, "http://h/c/feed.xml").unwrap();
    let titles: Vec<_> = f.entries.iter().map(|e| e.title.as_str()).collect();
    assert_eq!(titles, vec!["A", "B"]);
    assert_eq!(f.entries[0].content.as_deref(), Some("one\ntwo"));
    assert_eq!(f.entries[1].summary.as_deref(), Some("left right"));
    assert_eq!(f.entries[1].best_acquisition().map(|l| l.href.as_str()), Some("http://h/c/b.epub"));
}

#[test]
fn atom_template_search_when_there_is_no_description() {
    let xml = r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>Calibre</title>
        <link rel="search" type="application/atom+xml" href="/opds/search/{searchTerms}" title="Search"/></feed>"#;
    let f = parse_feed(xml, "http://calibre.local:8080/opds").unwrap();
    let Some(SearchLink::Template(t)) = f.search() else { panic!("{:?}", f.search()) };
    assert_eq!(fill_template(&t, "a/b ø"), "http://calibre.local:8080/opds/search/a%2Fb%20%C3%B8");
}

#[test]
fn single_entry_documents_and_errors() {
    let xml = r#"<?xml version="1.0"?><entry xmlns="http://www.w3.org/2005/Atom"><title>Solo</title><link rel="http://opds-spec.org/acquisition" href="solo.epub" type="application/epub+zip"/></entry>"#;
    let f = parse_feed(xml, "http://h/a/b.xml").unwrap();
    assert_eq!(f.title, "Solo");
    assert_eq!(f.entries[0].acquisitions()[0].href, "http://h/a/solo.epub");
    assert!(matches!(parse_feed("<html><body>login</body></html>", "http://h/"), Err(NetError::Format(_))));
    assert!(parse_feed("", "http://h/").is_err());
    assert!(parse_any(b"{\"metadata\":{\"title\":\"x\"}}", "application/json", "http://h/").is_ok());
    assert!(parse_any(b"  {\"metadata\":{}}", "", "http://h/").is_ok());
    assert!(parse_any(b"<feed><title>t</title></feed>", "", "http://h/").is_ok());
}

#[test]
fn opds2_test_catalog_home() {
    let f = parse_opds2(TC_HOME, "https://test.opds.io/2.0/home.json").unwrap();
    assert_eq!(f.version, OpdsVersion::Json2);
    assert_eq!(f.title, "OPDS 2.0 Test Catalog");
    // 8 navigation + 2 publications + group "French Classics" (1 link + 5) + 2 + 3.
    assert_eq!(f.entries.len(), 21);
    assert_eq!(f.link("self").unwrap().href, "https://test.opds.io/2.0/home.json");
    let books = f.entries.iter().filter(|e| e.is_book()).count();
    assert_eq!(books, 10);
    let french = f.entries.iter().find(|e| e.title == "French Classics").unwrap();
    assert!(french.navigation().is_some());
    assert_eq!(f.kind(), FeedKind::Acquisition);
}

#[test]
fn opds2_test_catalog_publications() {
    let f = parse_opds2(TC_PUBS, "https://test.opds.io/2.0/publications.json").unwrap();
    assert_eq!(f.total_results, Some(14));
    assert_eq!(f.entries.len(), 14);
    let e = &f.entries[0];
    assert_eq!(e.title, "Voyage au centre de la Terre");
    assert_eq!(e.authors, vec!["Jules Verne"]);
    assert_eq!(e.language.as_deref(), Some("fr"));
    assert_eq!(e.publisher.as_deref(), Some("Feedbooks"));
    assert!(e.summary.as_deref().unwrap().starts_with("Le professeur Lidenbrock"));
    assert_eq!(e.categories, vec!["Science Fiction", "Action & Aventure"]);
    assert_eq!(e.best_acquisition().unwrap().href, "https://test.opds.io/assets/centredelaterre/file.epub");
    assert_eq!(e.cover().unwrap().href, "https://test.opds.io/assets/centredelaterre/normal.jpg");
    assert_eq!(e.thumbnail().unwrap().href, "https://test.opds.io/assets/centredelaterre/small.jpg");
    assert_eq!(f.entries[1].title, "Title in multiple languages");
    assert_eq!(f.entries[2].authors, vec!["Jules Verne", "Second Author"]);
    assert_eq!(f.entries[3].authors, vec!["Jules Verne"]);
    assert_eq!(f.entries[4].authors, vec!["Jules Verne", "Second Author"]);
    let sample = f.entries.iter().find(|e| e.title == "Sample").unwrap();
    assert_eq!(sample.links[0].rel, "http://opds-spec.org/acquisition/sample");
    let buy = f.entries.iter().find(|e| e.title == "Buy").unwrap();
    assert!(!buy.best_acquisition().unwrap().is_free_acquisition());
}

#[test]
fn opds2_test_catalog_navigation() {
    let f = parse_opds2(TC_NAV, "https://test.opds.io/2.0/navigation.json").unwrap();
    let hrefs: Vec<_> = f.entries.iter().map(|e| e.navigation().unwrap().href.as_str()).collect();
    assert_eq!(
        hrefs,
        vec![
            "https://test.opds.io/2.0/home.json",
            "https://test.opds.io/2.0/home.json",
            "https://test.opds.io/2.0/home.json",
            "https://test.opds.io/2.0/404.json",
            "https://test.opds.io/2.0/navigation.json"
        ]
    );
    assert_eq!(f.kind(), FeedKind::Navigation);
}

#[test]
fn opds2_huge_paging_numbers_do_not_overflow() {
    let json = format!(r#"{{"metadata":{{"title":"T","itemsPerPage":{m},"currentPage":{m}}},"links":[]}}"#, m = u64::MAX);
    let f = parse_opds2(&json, "http://h/").unwrap();
    assert_eq!(f.start_index, Some(u64::MAX));
}

#[test]
fn templates() {
    assert_eq!(fill_template("http://h/s?q={searchTerms}&p={startPage?}&l={language}", "x y"), "http://h/s?q=x%20y&p=&l=*");
    assert_eq!(fill_template("http://h/s?q={searchTerms}&i={startIndex}&c={count?}", "x"), "http://h/s?q=x&i=1&c=");
    assert_eq!(fill_template("http://h/search{?query}", "moby dick"), "http://h/search?query=moby%20dick");
    assert_eq!(fill_template("http://h/search{?query,title,author}", "a"), "http://h/search?query=a");
    assert_eq!(fill_template("http://h/s?x=1{&query}", "a"), "http://h/s?x=1&query=a");
    assert_eq!(fill_template("http://h/s?q={os:searchTerms}", "a+b"), "http://h/s?q=a%2Bb");
    assert_eq!(fill_template("http://h/broken{searchTerms", "a"), "http://h/broken{searchTerms");
}

#[test]
fn entities_and_html_text() {
    assert_eq!(decode_entities("a &amp; b &lt;c&gt; &#233;&#xE9; &nbsp;&unknown; & &;"), "a & b <c> \u{e9}\u{e9} \u{a0}&unknown; & &;");
    assert_eq!(html_to_text("<p>One<br>two</p><ul><li>a</li><li>b &amp; c</li></ul>"), "One\ntwo\na\nb & c");
    assert_eq!(html_to_text("x < y"), "x < y");
}

#[test]
fn client_browses_paginates_searches_and_downloads() {
    let s = TestServer::start().unwrap();
    let http = UreqClient::new();
    let c = OpdsClient::new(&http);
    let root = c.fetch(&s.url("/opds/root.xml")).unwrap();
    assert_eq!(root.title, "rinky test catalog");
    assert_eq!(root.kind(), FeedKind::Navigation);
    let new_url = root.entries[0].navigation().unwrap().href.clone();
    assert_eq!(new_url, s.url("/opds/new.xml?page=1"));
    let pages = c.fetch_pages(&new_url, 10).unwrap();
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[1].prev_url(), Some(s.url("/opds/new.xml?page=1").as_str()));
    let books: Vec<_> = pages.iter().flat_map(|p| p.entries.iter()).collect();
    let titles: Vec<_> = books.iter().map(|e| e.title.as_str()).collect();
    assert_eq!(titles, vec!["Alpha & Omega", "Beta", "Gamma", "Delta \u{e6}\u{f8}\u{e5}"]);
    assert_eq!(pages[0].total_results, Some(4));
    assert!(c.next_page(&pages[1]).unwrap().is_none());
    assert_eq!(s.state.requests_to("/opds/new.xml")[0].header("accept"), Some(OPDS_ACCEPT));

    // Search through the OpenSearch description.
    let results = c.search(&root, "moby dick").unwrap();
    let titles: Vec<_> = results.entries.iter().map(|e| e.title.as_str()).collect();
    assert_eq!(titles, vec!["Result for moby", "Result for dick"]);
    let sreq = &s.state.requests_to("/opds/search")[0];
    assert_eq!(sreq.query, "q=moby%20dick&page=");

    // Download the first book through its acquisition link.
    let book = books[0];
    let link = book.best_acquisition().unwrap();
    assert_eq!(link.length, Some(4000));
    let dir = std::env::temp_dir().join(format!("ink-net-opds-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let dl = c.download_for(book, link, &dir);
    assert_eq!(dl.dest, dir.join("Alpha & Omega.epub"));
    let out = dl.run(&http, &mut Hooks::default()).unwrap();
    assert_eq!(out.sha256, sha256_hex(&file_bytes(4000)));
    assert_eq!(out.mime, "application/epub+zip");
    let _ = std::fs::remove_dir_all(&dir);

    // OPDS 2 over HTTP, chosen by content type.
    let f2 = c.fetch(&s.url("/opds2/catalog.json")).unwrap();
    assert_eq!(f2.version, OpdsVersion::Json2);
    assert_eq!(f2.next_url(), Some(s.url("/opds2/catalog.json?page=2").as_str()));
    assert_eq!(f2.entries.len(), 3);
    assert_eq!(f2.entries[1].authors, vec!["Eve Novelist"]);
    let Some(SearchLink::Template(t)) = f2.search() else { panic!() };
    assert_eq!(fill_template(&t, "x"), s.url("/opds2/search?query=x"));

    // Errors keep their status.
    assert!(matches!(c.fetch(&s.url("/opds/nope.xml")), Err(NetError::Server { status: 404, .. })));
}

#[test]
fn credentials_go_with_feed_requests_and_downloads() {
    let s = TestServer::start().unwrap();
    let http = UreqClient::new();
    let mut c = OpdsClient::new(&http);
    c.auth = Some(("test".into(), "secret".into()));
    let resp = http.send(c.request(&s.url("/auth/basic"))).unwrap();
    assert_eq!(resp.status, 200);
    let entry = OpdsEntry { title: "Private".into(), ..Default::default() };
    let link = OpdsLink { href: s.url("/files/100"), rel: REL_ACQUISITION.into(), mime: "application/pdf".into(), ..Default::default() };
    let dl = c.download_for(&entry, &link, Path::new("/tmp/x"));
    assert_eq!(dl.dest, Path::new("/tmp/x/Private.pdf"));
    assert_eq!(dl.headers, vec![("Authorization".to_string(), "Basic dGVzdDpzZWNyZXQ=".to_string())]);
}
