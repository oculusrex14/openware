//! F1 gate: decode every shipped `.cvw` in the approved corpus copy.
//!
//! Skips when `a sample copy` is absent (fresh checkout). The corpus is
//! the user-approved mock engagement copy; nothing here touches CaseWare.

use std::path::{Path, PathBuf};

use cw_cv::cell::MARKER;
use cw_cv::Cvw;

fn corpus() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sandbox/bridge-work");
    let mut out = Vec::new();
    let Ok(dirs) = std::fs::read_dir(&root) else {
        return out;
    };
    for d in dirs.flatten() {
        let Ok(files) = std::fs::read_dir(d.path()) else {
            continue;
        };
        for f in files.flatten() {
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) == Some("cvw") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn every_corpus_cvw_decodes() {
    let files = corpus();
    if files.is_empty() {
        eprintln!("corpus not present; skipping");
        return;
    }
    let mut cells = 0usize;
    let mut forms = 0usize;
    for p in &files {
        let bytes = std::fs::read(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
        let doc = Cvw::parse(&bytes).unwrap_or_else(|e| panic!("parse {}: {e}", p.display()));
        assert!(
            doc.layout_header.magic_ok,
            "{}: Layout magic",
            p.display()
        );
        assert_eq!(doc.cell_dir.header[0], 4, "{}: cell version", p.display());
        assert_eq!(doc.cell_dir.header[1], MARKER, "{}: cell marker", p.display());
        assert_eq!(
            doc.cell_dir.declared_count() as usize,
            doc.cells().len(),
            "{}: declared count vs walk",
            p.display()
        );
        assert!(
            doc.cell_dir.complete,
            "{}: walk stopped at {} / {}",
            p.display(),
            doc.cell_dir.consumed,
            bytes.len()
        );
        assert_eq!(
            doc.cell_blocks.iter().filter(|b| b.name_offset.is_none()).count(),
            0,
            "{}: unlocatable cell name",
            p.display()
        );
        if doc.form().is_some() {
            forms += 1;
        }
        cells += doc.cells().len();
    }
    eprintln!(
        "decoded {} files, {} cells, {} with FORM identity",
        files.len(),
        cells,
        forms
    );
    assert!(files.len() >= 190, "corpus shrank: {} files", files.len());
    assert!(cells >= 1_000_000, "unexpected cell total: {cells}");
}

