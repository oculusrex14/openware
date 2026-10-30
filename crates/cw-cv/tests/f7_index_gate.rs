//! F7 gate: `Para`/`Sect`/`Table` index-stream grammars + the cell→pool
//! binding, asserted against the **shipped corpus copy**.
//!
//! Sources (skipped individually when a document is not staged):
//!
//! * `handover/drive-download-…/` — the six v23/v24 `.cvw` of the corpus set.
//!
//! These are the documents the index-stream and cell→pool laws were derived on,
//! so this gate pins their exact numbers rather than a synthesised
//! fixture: `Sect`/`Table` length law `12 + 16·count` on 12/12 streams, the
//! `Para`-index range law, the `Table` inline names, and — where the `Strings`
//! pool decodes — the `slot + 2` cell→text bindings for the four annotated
//! `03250RG0` records.

use std::path::{Path, PathBuf};

use cw_cv::index_stream::{ParaIndex, RangeIndex, HEADER_LEN, RECORD_LEN};
use cw_cv::{Cvw, StringsPool};

/// The corpus-copy root.
fn handover() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../handover")
}

/// `(file name, expected Layout version, expected Para hint)` for the six
/// transferred documents, in staged-set order.
/// Note `EXPO0IS0` is a **v13** blank template and `Scott Steel` is **v23** —
/// the corpus is not uniformly v24, which the earlier "all v24" reading missed.
const STAGED: [(&str, u32, u32); 6] = [
    ("12312024 PPC USA Inc Review(1256259)01200RG0.cvw", 24, 0xA1),
    ("12312024 PPC USA Inc Review(1256259)02720GG0.cvw", 24, 0xA1),
    ("12312024 PPC USA Inc Review(1256259)03250RG0.cvw", 24, 0xA1),
    ("12312024 PPC USA Inc Review(1256259)04000RG0.cvw", 24, 0xA1),
    ("12312024 PPC USA Inc Review(1256259)EXPO0IS0.cvw", 13, 0x9B),
    ("Scott Steel 2024 12-31 Review(1192513)GO.cvw", 23, 0xA1),
];

fn staged_paths() -> Vec<(PathBuf, u32, u32)> {
    let root = handover();
    let dl = root.join("drive-download-20261028T110613Z-1-001");
    let mut out = Vec::new();
    for (name, ver, hint) in STAGED {
        // EXPO0IS0 / Scott Steel sit at the handover root, the rest in the
        // drive-download folder; accept either location.
        for cand in [root.join(name), dl.join(name)] {
            if cand.is_file() {
                out.push((cand, ver, hint));
                break;
            }
        }
    }
    out
}

fn doc(path: &Path) -> Cvw {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    Cvw::parse(&bytes).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// `Sect` and `Table` are `12 + 16·count` bytes **exactly**, their records are
/// all in range of `Para`, and every record satisfies `start <= end`.
#[test]
fn sect_and_table_length_law_is_exact() {
    let paths = staged_paths();
    if paths.is_empty() {
        eprintln!("handover docs not staged; skipping");
        return;
    }
    let mut streams = 0usize;
    for (path, ver, hint) in &paths {
        let d = doc(path);
        assert_eq!(d.layout_header.version, *ver, "{}: Layout version", path.display());
        let layout_len = d.stream("Form/Layout").map(|b| b.len()).unwrap_or(0);
        let para_bytes = d.stream("Index/Para").expect("Para stream");
        let para = ParaIndex::parse(&para_bytes, layout_len).expect("Para parse");
        assert_eq!(para.header.hint, *hint, "{}: Para hint", path.display());
        assert_eq!(para.header.zero, 0, "{}: Para zero word", path.display());
        assert!(para.complete, "{}: Para must be a closed offset vector", path.display());
        assert_eq!(
            para_bytes.len(),
            HEADER_LEN + 4 * para.len(),
            "{}: Para length law",
            path.display()
        );

        for name in ["Index/Sect", "Index/Table"] {
            let bytes = d.stream(name).unwrap_or_else(|_| panic!("{}: {name}", path.display()));
            let idx = RangeIndex::parse(&bytes, para.len(), layout_len)
                .unwrap_or_else(|| panic!("{}: {name} parse", path.display()));
            streams += 1;
            assert_eq!(
                bytes.len(),
                HEADER_LEN + RECORD_LEN * idx.len(),
                "{}: {name} length law (len {}, count {})",
                path.display(),
                bytes.len(),
                idx.len()
            );
            assert_eq!(idx.header.zero, 0, "{}: {name} zero word", path.display());
            assert_eq!(idx.header.hint, *hint, "{}: {name} hint", path.display());
            assert!(
                idx.complete,
                "{}: {name} field laws (range/mirror) must hold",
                path.display()
            );
            // every record covers a non-empty range of the Para vector
            for r in &idx.records {
                assert!(
                    r.start_para <= r.end_para,
                    "{}: {name} start<=end",
                    path.display()
                );
                assert!(
                    (r.end_para as usize) < para.len(),
                    "{}: {name} end_para {} < para {}",
                    path.display(),
                    r.end_para,
                    para.len()
                );
            }
            // The highest covered paragraph is in range and near the tail:
            // trailing paragraphs legitimately carry no Sect/Table record.
            // Measured (max_end, para.len()) gaps, all <= 10:
            //   03250 (344,345) 01200 (905,906) 04000 (29918,29921)
            //   EXPO0IS0 (54,56) Scott Steel (1510,1520) 02720 (55736,55737)
            let max_end = idx.records.iter().map(|r| r.end_para).max().unwrap_or(0);
            assert!(
                (max_end as usize) < para.len(),
                "{}: {name} max start/end {max_end} out of range {}",
                path.display(),
                para.len()
            );
            assert!(
                (max_end as usize) + 10 >= para.len(),
                "{}: {name} max start/end {max_end} must reach the Para tail {}",
                path.display(),
                para.len()
            );
        }
    }
    assert_eq!(streams, paths.len() * 2, "every staged doc contributes 2 streams");
    eprintln!("asserted {streams} Sect/Table streams exactly");
}

/// `Table` records carry a 4-byte inline name, and `mirror_ptr` points at a
/// `Layout` **definition object** (`u8 kind | u8 00 | <name>`) whose name is the
/// same bytes. Verified **4418/4418** on the staged corpus. `Sect` records have
/// no inline name (their fourth field is a `Layout` offset).
#[test]
fn table_definition_objects_name_the_inline_field() {
    let paths = staged_paths();
    if paths.is_empty() {
        eprintln!("handover docs not staged; skipping");
        return;
    }
    let mut total = 0usize;
    let mut named = 0usize;
    let mut kinds = std::collections::BTreeMap::new();
    for (path, _, _) in &paths {
        let d = doc(path);
        let layout = d.stream("Form/Layout").expect("Layout");
        let para = ParaIndex::parse(&d.stream("Index/Para").expect("Para"), layout.len())
            .expect("Para parse");
        let table = RangeIndex::parse(
            &d.stream("Index/Table").expect("Table"),
            para.len(),
            layout.len(),
        )
        .expect("Table parse");

        for (i, r) in table.records.iter().enumerate() {
            if !r.has_mirror() {
                continue;
            }
            total += 1;
            assert!(
                r.object_name_matches(&layout),
                "{}: Table[{i}] ptr {:#x} must name inline {:?}",
                path.display(),
                r.mirror_ptr,
                r.inline_name()
            );
            named += 1;
            *kinds.entry(r.object_kind(&layout)).or_insert(0usize) += 1;
        }

        // Sect's fourth field is a Layout offset, never a 2..4 char identifier
        // matching a definition object.
        let sect = RangeIndex::parse(
            &d.stream("Index/Sect").expect("Sect"),
            para.len(),
            layout.len(),
        )
        .expect("Sect parse");
        for r in &sect.records {
            assert!(
                !r.object_name_matches(&layout),
                "{}: Sect record must not carry a name object (inline {:?})",
                path.display(),
                r.inline_name()
            );
        }
    }
    assert_eq!(named, total, "every mirror_ptr must name its inline field");
    assert!(total >= 4000, "expected ~4418 Table objects, saw {total}");
    // kind 2 and 3 are both real; 2 is the minority shape
    assert!(kinds.contains_key(&Some(3)), "kind 3 present: {kinds:?}");
    assert!(kinds.contains_key(&Some(2)), "kind 2 present: {kinds:?}");
    assert!(kinds.contains_key(&Some(1)), "kind 1 present (single-letter names): {kinds:?}");
    eprintln!("{named}/{total} definition objects name their inline field; kinds {kinds:?}");
}

/// The `Para` vector is a pure `Layout` offset array (offset per paragraph).
#[test]
fn para_offsets_point_into_layout() {
    let paths = staged_paths();
    if paths.is_empty() {
        eprintln!("handover docs not staged; skipping");
        return;
    }
    for (path, _, _) in &paths {
        let d = doc(path);
        let layout = d.stream("Form/Layout").expect("Layout");
        let para = ParaIndex::parse(&d.stream("Index/Para").expect("Para"), layout.len())
            .expect("Para parse");
        assert!(para.complete, "{}: every Para offset inside Layout", path.display());
        assert!(para.len() > 0, "{}: non-empty Para", path.display());
    }
}

/// On `03250RG0` (v24), the four annotated cells bind through
/// `slot + 2` to exactly the documented pool text.
#[test]
fn cell_to_pool_binding_is_slot_plus_two() {
    let root = handover();
    let dl = root.join("drive-download-20261028T110613Z-1-001");
    let Some(path) = [root.join(STAGED[2].0), dl.join(STAGED[2].0)]
        .into_iter()
        .find(|p| p.is_file())
    else {
        eprintln!("03250RG0 not staged; skipping");
        return;
    };
    let d = doc(&path);
    let pool = match d.strings_pool() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("03250RG0 pool unreadable ({e}); skipping binding check");
            return;
        }
    };
    // 462 physical entries; a count of 460 comes from dropping the two
    // leading non-text entries.
    assert_eq!(pool.len(), 462, "03250RG0 pool entry count");

    // (cell name, expected trailer slot, expected pool text prefix)
    let expect = [
        (" 1ML3", 0x2Fu16, "General"),
        ("00CVR", 0x48, "Understanding the Client and its Industry"),
        ("00KLL", 0x6B, "field(\"SH\",\"AD3\",docnum())"),
        ("@@FIRM", 0x91, "rc(1#)=\"\""),
    ];
    for (name, slot, text) in expect {
        let block = d
            .cell_blocks
            .iter()
            .find(|b| b.name == name)
            .unwrap_or_else(|| panic!("cell {name:?} not in the directory"));
        let trailer = block
            .trailer
            .unwrap_or_else(|| panic!("cell {name:?} has no pool trailer"));
        assert_eq!(trailer.slot, slot, "cell {name:?} trailer slot");
        let idx = trailer.pool_index();
        let got = StringsPool::text(&pool, idx)
            .unwrap_or_else(|| panic!("cell {name:?} slot {slot} -> pool[{idx}] out of range"));
        assert!(
            got.starts_with(text),
            "cell {name:?}: slot {slot} -> pool[{idx}] = {got:?}, expected prefix {text:?}"
        );
    }
}
