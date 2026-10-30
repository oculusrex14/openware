//! F4 gate: cell-number links (leadsheet substitution) + render pipeline.
//!
//! Skips when `a sample copy` is absent (fresh checkout). Read-only over
//! the user-approved mock engagement copy; nothing here touches CaseWare.

use std::path::{Path, PathBuf};

use cw_cv::{
    cell_number_of, render, render_cells, CellEntry, CellIndex, CellKind, CellNumber, CellRef,
    Cvw, LinkTable, TextTable, WksRegistry,
};

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

/// First corpus document whose folder also carries an `FP.dbf`.
fn sample() -> Option<(PathBuf, Cvw, WksRegistry)> {
    for p in corpus() {
        let Some(dir) = p.parent() else { continue };
        let Ok(reg) = WksRegistry::from_engagement_dir(dir) else {
            continue;
        };
        let Ok(bytes) = std::fs::read(&p) else { continue };
        let Ok(doc) = Cvw::parse(&bytes) else { continue };
        if doc.cells().len() > 10 {
            return Some((p, doc, reg));
        }
    }
    None
}

#[test]
fn corpus_cell_numbers_and_index_are_stable() {
    let Some((path, doc, _)) = sample() else {
        eprintln!("corpus not present; skipping");
        return;
    };
    let index = CellIndex::build(doc.cells());
    assert!(!index.is_empty(), "{}: no leadsheet numbers", path.display());
    let numbered = doc
        .cells()
        .iter()
        .filter(|c| cell_number_of(&c.name).is_some())
        .count();
    assert!(numbered > 10, "{numbered} numbered cells");
    // Every index key round-trips through Display/parse.
    for n in index.numbers().take(50) {
        assert_eq!(CellNumber::parse(&n.to_string()), Some(n));
    }
    // Groups are the dotted prefixes; the corpus uses them heavily.
    let grouped = doc
        .cells()
        .iter()
        .filter(|c: &&CellEntry| c.name.contains('.'))
        .count();
    assert!(grouped > 0, "expected group-qualified cell names");
}

#[test]
fn full_render_is_deterministic_and_issue_free_without_links() {
    let Some((_, doc, _)) = sample() else {
        eprintln!("corpus not present; skipping");
        return;
    };
    let texts = TextTable::new();
    let links = LinkTable::new();
    let a = render(&doc, &texts, &links, None);
    let b = render(&doc, &texts, &links, None);
    assert_eq!(a.len(), doc.cells().len());
    assert_eq!(a.issue_count(), 0, "no links => no issues");
    assert_eq!(a.substituted(), 0);
    assert_eq!(a.to_text(), b.to_text(), "render must be byte-stable");
    assert!(a.to_text().starts_with("form "));
}

#[test]
fn real_feed_substitution_uses_the_fp_registry() {
    let Some((_, doc, reg)) = sample() else {
        eprintln!("corpus not present; skipping");
        return;
    };
    let Some(clp) = reg.get("CLP") else {
        eprintln!("no CLP worksheet; skipping");
        return;
    };
    let (cellno, expected) = clp
        .cells()
        .next()
        .map(|c| (c.cellno(), c.text()))
        .expect("CLP has cells");
    // A real document cell that carries a leadsheet number becomes the feed.
    let source = doc
        .cells()
        .iter()
        .find(|c| cell_number_of(&c.name).is_some())
        .map(|c| c.name.clone())
        .expect("numbered cell");
    let mut links = LinkTable::new();
    links.insert(
        source.clone(),
        CellRef::Leadsheet {
            wks: "CLP".into(),
            cellno,
        },
    );
    let out = render_cells(doc.cells(), doc.form(), &TextTable::new(), &links, Some(&reg));
    let c = out.cell(&source).expect("source rendered");
    assert_eq!(c.kind, CellKind::LeadsheetLink);
    assert!(c.issue.is_none(), "{:?}", c.issue);
    assert_eq!(c.text, expected);
    assert_eq!(out.substituted(), 1);
}

#[test]
fn cell_number_chain_substitutes_the_whole_chain() {
    let cells = vec![
        CellEntry { name: "aEQ0.C1".into(), layout_offset: 0 },
        CellEntry { name: "aEQ0.C2".into(), layout_offset: 10 },
        CellEntry { name: "aEQ0.C3".into(), layout_offset: 20 },
    ];
    let mut texts = TextTable::new();
    texts.insert("aEQ0.C1", "seed");
    let mut links = LinkTable::new();
    let c1 = CellRef::parse("C1").unwrap();
    let c2 = CellRef::parse("C2").unwrap();
    links.insert("aEQ0.C2", c1);
    links.insert("aEQ0.C3", c2);
    let out = render_cells(&cells, None, &texts, &links, None);
    for name in ["aEQ0.C2", "aEQ0.C3"] {
        let c = out.cell(name).unwrap();
        assert_eq!(c.kind, CellKind::CellLink);
        assert_eq!(c.text, "seed", "{name}");
        assert!(c.issue.is_none());
    }
    assert_eq!(out.substituted(), 2);
}

#[test]
fn assignment_files_drive_the_pipeline() {
    let links = LinkTable::parse("aEQ0.C2=C1\naEQ0.C3=CLP.2\n").unwrap();
    let texts = TextTable::parse("aEQ0.C1=Midco\n").unwrap();
    assert_eq!(links.len(), 2);
    assert_eq!(texts.get("aEQ0.C1"), Some("Midco"));
    assert_eq!(
        links.get("aEQ0.C3"),
        Some(&CellRef::Leadsheet { wks: "CLP".into(), cellno: 2 })
    );
}

