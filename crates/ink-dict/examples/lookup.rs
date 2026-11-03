//! `cargo run -p ink-dict --example lookup -- DIR WORD...`: looks words up in every
//! dictionary under DIR (`DIR/<name>/*.ifo`) and prints the form found and the first
//! 300 characters of each definition. `--write-sample DIR` writes the sample dictionary.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 2 && args[0] == "--write-sample" {
        let ifo = ink_dict::writer::write(std::path::Path::new(&args[1]), &ink_dict::writer::sample_english()).expect("write");
        println!("{}", ifo.display());
        return;
    }
    let Some((dir, words)) = args.split_first() else {
        eprintln!("usage: lookup DIR WORD... | --write-sample DIR");
        std::process::exit(2);
    };
    let t = std::time::Instant::now();
    let lib = ink_dict::Library::open_dir(std::path::Path::new(dir));
    println!("opened {} dictionaries in {:?}", lib.dicts.len(), t.elapsed());
    for d in &lib.dicts {
        println!("  {} ({} words, {:?})", d.title(), d.len(), d.info);
    }
    for (p, e) in &lib.errors {
        println!("  error {}: {e}", p.display());
    }
    for w in words {
        let t = std::time::Instant::now();
        let r = lib.lookup(w);
        println!("{w:?} -> query {:?} form {:?}, {} hits in {:?}", r.query, r.form, r.hits.len(), t.elapsed());
        for h in &r.hits {
            let s: String = h.text.chars().take(300).collect();
            println!("  [{}] {}: {}", h.dict, h.headword, s.replace('\n', " | "));
        }
    }
}
