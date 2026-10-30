//! F3 gate: `CV OVR` property bag / override laws + corpus write-back identity.
//!
//! Skips when `a sample copy` is absent (fresh checkout). The corpus is
//! the user-approved mock engagement copy; nothing here touches CaseWare.

use std::path::{Path, PathBuf};

use cw_cv::{CellAttrs, Cvw, Origin, OverrideSet, PropBag, PropValue};

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

fn base_bag() -> PropBag {
    let mut b = PropBag::new();
    b.insert("font", PropValue::Text("Arial".into()));
    b.insert("size", PropValue::Real(10.0));
    b.insert("bold", PropValue::Bool(false));
    b.insert("align", PropValue::Text("left".into()));
    b
}

/// The three-state override law is the core `CV OVR` contract.
#[test]
fn cv_ovr_inherit_set_reset_law() {
    let base = base_bag();
    let mut ovr = OverrideSet::new();
    ovr.set("bold", true);
    ovr.reset("size");

    let (eff, origins) = ovr.apply_with_origins(&base);
    assert_eq!(eff.get("bold"), Some(&PropValue::Bool(true)));
    assert!(!eff.contains("size"));
    assert_eq!(origins["font"], Origin::Base);
    assert_eq!(origins["bold"], Origin::Override);
    assert_eq!(origins["size"], Origin::Reset);

    // Absence is inherit, not reset: re-resolving after dropping a key inherits.
    ovr.inherit("bold");
    assert_eq!(ovr.resolve("bold", base.get("bold")).origin, Origin::Base);
}

/// Layer chain: cell > style > document, nearest wins, nearer Reset is terminal.
#[test]
fn cv_ovr_layer_precedence() {
    let mut doc = OverrideSet::new();
    doc.set("font", "Georgia".to_string());
    doc.set("size", 12.0);

    let mut style = OverrideSet::new();
    style.set("size", 9.0);
    style.set("align", "center".to_string());

    let mut cell = OverrideSet::new();
    cell.reset("align");
    cell.set("bold", true);

    let eff = OverrideSet::layered_apply(&PropBag::new(), &[&cell, &style, &doc]);
    assert_eq!(eff.get("font"), Some(&PropValue::Text("Georgia".into())));
    assert_eq!(eff.get("size"), Some(&PropValue::Real(9.0)));
    assert_eq!(eff.get("bold"), Some(&PropValue::Bool(true)));
    assert!(!eff.contains("align"), "cell reset must win over style value");
}

/// Canonical dumps must be stable: the gate itself compares across two runs.
#[test]
fn cv_ovr_dump_is_deterministic() {
    let mut ovr = OverrideSet::new();
    ovr.set("z", 1i64);
    ovr.set("a", "x");
    ovr.reset("m");
    let first = ovr.to_text();
    assert_eq!(first, ovr.clone().to_text());
    assert!(first.starts_with("a\tset\ttext\t\"x\"\n"));
    assert!(first.ends_with("z\tset\tint\t1\n"));

    let mut bag = PropBag::new();
    bag.insert("k2", PropValue::Raw(vec![0x01, 0xFF]));
    bag.insert("k1", PropValue::Rgb(0x00_12_34_56));
    assert_eq!(bag.to_text(), "k1\trgb\t#123456\nk2\traw\t01FF\n");
}

/// Every corpus cell block must split into prefix/name/suffix and reassemble
/// byte-identically. This is the write-back guarantee F3 needs while the
/// attribute bit packing is still opaque.
#[test]
fn corpus_cell_attr_write_back_is_byte_identical() {
    let files = corpus();
    if files.is_empty() {
        eprintln!("corpus not present; skipping");
        return;
    }

    let mut blocks = 0usize;
    let mut with_prefix = 0usize;
    let mut prefixes: Vec<usize> = Vec::new();
    for p in &files {
        let bytes = std::fs::read(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
        let doc = Cvw::parse(&bytes).unwrap_or_else(|e| panic!("parse {}: {e}", p.display()));
        let layout = doc
            .stream("Form/Layout")
            .unwrap_or_else(|e| panic!("Layout {}: {e}", p.display()));

        for block in &doc.cell_blocks {
            let Some(attrs) = CellAttrs::from_layout(&layout, block) else {
                panic!(
                    "{}: block {} @{} unresolved",
                    p.display(),
                    block.name,
                    block.offset
                );
            };
            let start = block.offset as usize;
            let end = start + block.len as usize;
            assert_eq!(
                attrs.reassemble(),
                layout[start..end],
                "{}: write-back mismatch for cell {}",
                p.display(),
                block.name
            );
            assert_eq!(attrs.block_len(), block.len as usize);
            if !attrs.prefix().is_empty() {
                with_prefix += 1;
                prefixes.push(attrs.prefix().len());
            }
            blocks += 1;
        }
    }

    prefixes.sort_unstable();
    let min = prefixes.first().copied().unwrap_or(0);
    let max = prefixes.last().copied().unwrap_or(0);
    eprintln!(
        "cell attr write-back: {} files, {blocks} blocks, {with_prefix} with prefix (len {min}..={max})",
        files.len()
    );
    assert_eq!(blocks, with_prefix, "every cell must carry attribute bytes");
    assert!(blocks > 100_000, "corpus scale regressed");
}

/// Overrides composed from two different paths must have identical effect when
/// the nearer layer decides every key it touches (associativity of layering).
#[test]
fn cv_ovr_apply_is_associative() {
    let base = base_bag();
    let mut cell = OverrideSet::new();
    cell.set("size", 11.0);
    cell.reset("bold");
    let mut style = OverrideSet::new();
    style.set("bold", true);
    style.set("align", "right".to_string());

    let a = OverrideSet::layered_apply(&base, &[&cell, &style]);
    let b = cell.apply(&style.apply(&base));
    assert_eq!(a, b);
    assert!(!a.contains("bold"), "cell reset must not be resurrected by style");
}

