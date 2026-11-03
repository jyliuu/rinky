//! Writes the generated fixtures: `fixture.pdf` (10 pages) and `comic.cbz` (12 pages).
//!
//!     cargo run -p ink-mupdf --example mupdf-fixtures -- OUTDIR

use ink_mupdf::testgen;
use std::path::PathBuf;

fn main() -> std::io::Result<()> {
    let out = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    std::fs::create_dir_all(&out)?;
    let pdf = testgen::fixture_pdf();
    std::fs::write(out.join("fixture.pdf"), &pdf)?;
    let cbz = testgen::fixture_cbz(12);
    std::fs::write(out.join("comic.cbz"), &cbz)?;
    println!("{}: fixture.pdf {} bytes, comic.cbz {} bytes", out.display(), pdf.len(), cbz.len());
    Ok(())
}
