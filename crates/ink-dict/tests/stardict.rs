//! Dictionaries written by `ink_dict::writer` in every supported file layout, read back.

use std::path::PathBuf;

use ink_dict::writer::{self, Spec};
use ink_dict::{Dictionary, Library};

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ink-dict-test-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn base() -> Spec {
    let mut s = Spec::plain("t", "Test", &[("apple", "a round fruit"), ("Apple", "a company"), ("banana", "a long fruit"), ("zebra", "a striped animal"), ("Éclair", "a pastry"), ("in spite of", "without being affected by")]);
    s.synonyms = vec![("pomme".into(), "apple".into()), ("bananas".into(), "banana".into())];
    s
}

fn check(d: &Dictionary) {
    assert_eq!(d.len(), 6);
    let a = d.find("apple");
    assert_eq!(a.len(), 2);
    assert_eq!(d.headword(a[0]), "apple", "exact case first");
    assert_eq!(d.definition(a[0]).unwrap(), "a round fruit");
    assert_eq!(d.definition(a[1]).unwrap(), "a company");
    let a = d.find("APPLE");
    assert_eq!(a.len(), 2);
    assert_eq!(d.definition(d.find("zebra")[0]).unwrap(), "a striped animal");
    assert_eq!(d.definition(d.find("Éclair")[0]).unwrap(), "a pastry");
    assert_eq!(d.definition(d.find("pomme")[0]).unwrap(), "a round fruit");
    assert_eq!(d.definition(d.find("bananas")[0]).unwrap(), "a long fruit");
    assert_eq!(d.definition(d.find("in spite of")[0]).unwrap(), "without being affected by");
    assert!(d.find("cherry").is_empty());
    assert!(d.find("").is_empty());
}

#[test]
fn plain_dict_32bit() {
    let dir = tmp("plain");
    let ifo = writer::write(&dir, &base()).unwrap();
    check(&Dictionary::open(&ifo).unwrap());
}

#[test]
fn dictzip_small_chunks_64bit_gz_idx() {
    let dir = tmp("dz");
    let mut s = base();
    s.dictzip = true;
    s.chunk_len = 7; // entries span chunk borders
    s.offset64 = true;
    s.gzip_idx = true;
    let ifo = writer::write(&dir, &s).unwrap();
    assert!(dir.join("t.dict.dz").is_file() && dir.join("t.idx.gz").is_file());
    let d = Dictionary::open(&ifo).unwrap();
    assert_eq!(d.info.idxoffsetbits, 64);
    check(&d);
}

#[test]
fn big_dictzip_matches_plain() {
    // Many entries over several 58315-byte chunks, read in random order.
    let words: Vec<(String, String)> = (0..3000).map(|i| (format!("w{i:05}"), format!("definition number {i} ").repeat(1 + i % 7))).collect();
    let pairs: Vec<(&str, &str)> = words.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let mut s = Spec::plain("big", "Big", &pairs);
    let plain = Dictionary::open(&writer::write(&tmp("big-plain"), &s).unwrap()).unwrap();
    s.dictzip = true;
    let dz_dir = tmp("big-dz");
    let dz = Dictionary::open(&writer::write(&dz_dir, &s).unwrap()).unwrap();
    assert!(std::fs::metadata(dz_dir.join("big.dict.dz")).unwrap().len() < 200_000);
    for i in [2999usize, 0, 1500, 17, 2500, 1501, 3] {
        let w = format!("w{i:05}");
        let a = plain.definition(plain.find(&w)[0]).unwrap();
        let b = dz.definition(dz.find(&w)[0]).unwrap();
        assert_eq!(a, b);
        assert_eq!(a, words[i].1.trim());
    }
}

#[test]
fn typed_fields_without_sequence() {
    let dir = tmp("typed");
    let s = Spec {
        stem: "typed".into(),
        bookname: "Typed".into(),
        sametypesequence: String::new(),
        offset64: false,
        dictzip: false,
        chunk_len: 100,
        gzip_idx: false,
        entries: vec![("word".into(), vec![('t', b"w3:d".to_vec()), ('W', vec![1, 2, 3, 0, 5]), ('h', b"<b>bold</b> text<br>line two".to_vec())])],
        synonyms: vec![],
    };
    let d = Dictionary::open(&writer::write(&dir, &s).unwrap()).unwrap();
    assert_eq!(d.fields(0).unwrap().len(), 3);
    assert_eq!(d.definition(0).unwrap(), "/w3:d/\n\nbold text\nline two");
}

#[test]
fn sequence_with_two_fields() {
    let dir = tmp("seq2");
    let mut s = Spec::plain("s2", "S2", &[]);
    s.sametypesequence = "tm".into();
    s.entries = vec![("cat".into(), vec![('t', b"kat".to_vec()), ('m', b"a small animal".to_vec())])];
    let d = Dictionary::open(&writer::write(&dir, &s).unwrap()).unwrap();
    assert_eq!(d.definition(0).unwrap(), "/kat/\n\na small animal");
}

#[test]
fn library_lookup_uses_forms_and_all_dictionaries() {
    let root = tmp("lib");
    writer::write(&root.join("b-second"), &Spec::plain("second", "Second", &[("run", "to move fast (second)")])).unwrap();
    writer::write(&root.join("a-first"), &writer::sample_english()).unwrap();
    std::fs::create_dir_all(root.join("broken")).unwrap();
    std::fs::write(root.join("broken/x.ifo"), "nonsense").unwrap();
    std::fs::create_dir_all(root.join("empty")).unwrap();
    let lib = Library::open_dir(&root);
    assert_eq!(lib.dicts.len(), 2);
    assert_eq!(lib.dicts[0].title(), "Sample English Dictionary", "folder order");
    assert_eq!(lib.errors.len(), 1);

    let r = lib.lookup("“Households,”");
    assert_eq!(r.query, "Households");
    assert_eq!(r.form, "Households", "found through .syn, ASCII case ignored");
    assert_eq!(r.hits[0].headword, "household");
    assert!(r.hits[0].text.starts_with("household noun\na house and its occupants"), "{}", r.hits[0].text);

    let r = lib.lookup("dignity.");
    assert!(r.hits[0].text.contains("• the state or quality"), "{}", r.hits[0].text);
    let r = lib.lookup("mutinies");
    assert_eq!((r.form.as_str(), r.hits[0].headword.as_str()), ("mutiny", "mutiny"));
    let r = lib.lookup("Running");
    assert_eq!(r.form, "run");
    assert_eq!(r.hits[0].dict, "Second");
    // Each dictionary takes its own first form: "households" is a synonym in the
    // sample, the second dictionary only has "household".
    writer::write(&root.join("c-third"), &Spec::plain("third", "Third", &[("household", "people in one house (third)")])).unwrap();
    let lib = Library::open_dir(&root);
    let r = lib.lookup("households");
    assert_eq!(r.form, "households");
    assert_eq!(r.hits.iter().map(|h| (h.dict.as_str(), h.headword.as_str())).collect::<Vec<_>>(), vec![("Sample English Dictionary", "household"), ("Third", "household")]);
    let r = lib.lookup("verona");
    assert_eq!(r.hits[0].headword, "Verona");
    let r = lib.lookup("fairest");
    assert_eq!(r.form, "fair");
    let r = lib.lookup("xyzzy");
    assert!(r.hits.is_empty() && r.form.is_empty());
    assert!(lib.lookup("  ").hits.is_empty());
}

#[test]
fn missing_and_broken_files() {
    let lib = Library::open_dir(&tmp("none").join("missing"));
    assert!(lib.dicts.is_empty() && lib.errors.is_empty());
    let dir = tmp("broken");
    let ifo = writer::write(&dir, &base()).unwrap();
    std::fs::remove_file(dir.join("t.dict")).unwrap();
    assert!(Dictionary::open(&ifo).unwrap_err().to_string().contains("no .dict"));
    let ifo = writer::write(&dir, &base()).unwrap();
    // A synonym pointing past the index is skipped; the others still work.
    std::fs::write(dir.join("t.syn"), b"aaa\0\0\0\0\x63pomme\0\0\0\0\x01").unwrap();
    let d = Dictionary::open(&ifo).unwrap();
    assert!(d.find("aaa").is_empty());
    assert_eq!(d.headword(d.find("pomme")[0]), "apple");
    // Truncated .dict: entries past the end give errors, not panics.
    std::fs::write(dir.join("t.dict"), b"abc").unwrap();
    let d = Dictionary::open(&ifo).unwrap();
    assert!((0..d.len()).any(|i| d.definition(i).is_err()));
    // Garbage .dict.dz.
    std::fs::remove_file(dir.join("t.dict")).unwrap();
    std::fs::write(dir.join("t.dict.dz"), b"\x1f\x8b\x08\x04\0\0\0\0\0\0\x0a\0RA\x06\0\x01\0\x10\0\x01\0").unwrap();
    // Opening may fail or succeed; reading every entry must not panic.
    if let Ok(d) = Dictionary::open(&ifo) {
        for i in 0..d.len() {
            let _ = d.definition(i);
        }
    }
}
