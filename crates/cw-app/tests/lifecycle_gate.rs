//! P5.D1 gate: the cw-app engagement-copy engine vs the F64 fixture laws.
//!
//! Golden: `tests/golden/lifecycle/{law.json,tables/}` — the copy-probe source
//! tables plus the census the engine must reproduce. The engine implements CW's
//! ROW-SET semantics (spec `docs\specs\lifecycle.md` §4); byte cosmetics (memo
//! renumbering, cdx rebuild, cvw path rewrite) are documented deviations, not
//! gated.
//!
//! ## Provenance (2026-10-28 rebuild)
//!
//! The original `tables/` were the probe-source engagement captured from the COM
//! copy round (`a sample copy`, F64), which is not part of this rebuild. The
//! fixture is now **kernel-derived** by `examples/freeze_lifecycle.rs` over the
//! frozen C1 corpus `tests/golden/dbf/corp2026` — the same engagement in the
//! state the shipped package carries. Two of the four probe censuses survive
//! unchanged (`AC 77 live + 1 tombstone`, `AM 325 live`); `BL` re-grounds to
//! `962/1198` and `GL` to `17/3` because the frozen package predates the probe's
//! extra adjusting entries. The laws themselves (live rows only, flags packed to
//! 0, live multiset preserved byte-for-cell, `ClearAccountBalances` = BL flip +
//! GL empty, sidecar/document rename/skip) are properties the engine must
//! satisfy on *any* source, so re-grounding the counts weakens nothing: the
//! census is read from `law.json`, which the freezer writes from the same bytes
//! the engine is run over.

use std::collections::BTreeMap;
use std::path::PathBuf;

use cw_app::lifecycle::{copy_engagement, CopyOptions};
use cw_core::dbf::Table;
use cw_core::json::{self, Json};

fn gold() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("..");
    p.push("..");
    p.push("tests");
    p.push("golden");
    p.push("lifecycle");
    p
}

/// The frozen census (`law.json`) — the numbers this gate asserts.
fn law() -> Json {
    let p = gold().join("law.json");
    json::parse(&std::fs::read_to_string(&p).unwrap_or_else(|e| {
        panic!("{}: {e} (run `cargo run -p cw-app --example freeze_lifecycle`)", p.display())
    }))
    .unwrap()
}

/// `(table, src_live, src_deleted)` from the fixture census.
fn census() -> Vec<(String, usize, usize)> {
    let l = law();
    l.get("tables")
        .and_then(|t| t.as_obj())
        .expect("law.json tables")
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                v.get("live").and_then(|x| x.as_usize()).expect("live"),
                v.get("deleted").and_then(|x| x.as_usize()).expect("deleted"),
            )
        })
        .collect()
}

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("ocw-d1-{name}-{}", std::process::id()))
}

/// The destination root for one probe: derived from the source tag so two tests
/// running in parallel threads cannot share it.
fn dst_root(tag: &str) -> PathBuf {
    let p = tmp(&format!("{tag}-dst"));
    let _ = std::fs::remove_dir_all(&p);
    p
}

/// Stage the probe-source engagement: golden tables under the TPROBE stem plus
/// sidecar/document/artifact fixtures.
fn stage(tag: &str) -> (PathBuf, PathBuf) {
    let src = tmp(tag);
    let _ = std::fs::remove_dir_all(&src);
    std::fs::create_dir_all(&src).unwrap();
    for t in ["ac", "am", "bl", "gl"] {
        let bytes = std::fs::read(gold().join("tables").join(format!("{t}.dbf"))).unwrap();
        std::fs::write(src.join(format!("TPROBE{}.dbf", t.to_uppercase())), bytes).unwrap();
    }
    std::fs::write(
        src.join("TPROBEAC.FPT"),
        std::fs::read(gold().join("tables").join("ac.fpt")).unwrap(),
    )
    .unwrap();
    std::fs::write(src.join("TPROBE01200RG2.cvw"), b"CVWPATH").unwrap();
    std::fs::write(src.join("Doc1.docx"), b"DOC").unwrap();
    std::fs::write(src.join("copy.ini"), b"[GROUPS]").unwrap();
    std::fs::write(src.join("x.locks"), b"").unwrap();
    std::fs::write(src.join("TPROBE.ac"), b"AC-HUB").unwrap();
    (src, dst_root(tag))
}

/// live-cell multiset: flag-stripped record bytes -> count (order-free, F64).
fn live_multiset(t: &Table, live_only: bool) -> BTreeMap<Vec<u8>, usize> {
    let mut m = BTreeMap::new();
    for r in &t.records {
        let live = r[0] == b' ';
        if live || !live_only {
            *m.entry(r[1..].to_vec()).or_insert(0) += 1;
        }
    }
    m
}

#[test]
fn copyall_row_set_laws() {
    let (src, dst_root) = stage("copyall");
    let opts = CopyOptions { copy_all: true, ..Default::default() };
    let rep = copy_engagement(&src, &dst_root, "TOUT", &opts).unwrap();
    assert_eq!(rep.source_stem, "TPROBE");
    assert_eq!(rep.dest_stem, "TOUT");

    // F64 (3): live counts preserved, deletion flags packed to 0, live multiset
    // identical. Census from `law.json` (frozen 2026-10-28; see the fixture
    // header for why BL/GL differ from the original COM probe numbers).
    let expect = census();
    assert_eq!(expect.len(), 4, "four probe tables");
    for (sfx, src_live, src_del) in expect.iter().map(|(t, l, d)| (t.as_str(), *l, *d)) {
        let r = rep.tables.get(sfx).unwrap_or_else(|| panic!("no {sfx} report"));
        assert_eq!(r.src_live, src_live, "{sfx} src_live");
        assert_eq!(r.src_deleted, src_del, "{sfx} src_deleted");
        assert_eq!(r.dst_live, r.src_live, "{sfx} live preserved");
        assert_eq!(r.dst_deleted, 0, "{sfx} packed");
        let dst = Table::parse_path(dst_root.join("TOUT").join(format!("TOUT{sfx}.dbf"))).unwrap();
        let src = Table::parse_path(src.join(format!("TPROBE{sfx}.dbf"))).unwrap();
        assert_eq!(
            live_multiset(&src, true),
            live_multiset(&dst, false),
            "{sfx} live-cell multiset"
        );
    }
}

#[test]
fn clear_account_balances_law() {
    let (src, dst_root) = stage("clearbal");
    let opts = CopyOptions { copy_all: true, clear_account_balances: true, ..Default::default() };
    let rep = copy_engagement(&src, &dst_root, "TOUT2", &opts).unwrap();
    // F64 (4): BL former-live -> flag-deleted, GL emptied; AM/AC as CopyAll.
    // Source counts come from the fixture census (law.json).
    let l = law();
    let src_cnt = |t: &str| -> usize {
        l.get("tables")
            .and_then(|x| x.get(t))
            .and_then(|x| x.get("live"))
            .and_then(|x| x.as_usize())
            .unwrap_or_else(|| panic!("law.json tables.{t}.live"))
    };
    let bl_live = src_cnt("BL");
    let gl_live = src_cnt("GL");
    let am_live = src_cnt("AM");
    assert!(bl_live > 0 && gl_live > 0, "fixture must exercise both clears");
    let bl = rep.tables.get("BL").unwrap();
    assert_eq!(bl.dst_live, 0, "BL has no live row after the clear");
    assert_eq!(bl.dst_deleted, bl_live, "every former-live BL row is flag-deleted");
    let gl = rep.tables.get("GL").unwrap();
    assert_eq!(gl.dst_live, 0, "GL is emptied");
    assert_eq!(gl.dst_deleted, 0, "GL keeps no tombstone either");
    let blt = Table::parse_path(dst_root.join("TOUT2").join("TOUT2BL.dbf")).unwrap();
    assert!(blt.records.iter().all(|r| r[0] == b'*'), "BL rows are tombstones");
    assert_eq!(blt.records.len(), bl_live);
    let glt = Table::parse_path(dst_root.join("TOUT2").join("TOUT2GL.dbf")).unwrap();
    assert!(glt.records.is_empty());
    // AM untouched by the clear
    let am = rep.tables.get("AM").unwrap();
    assert_eq!(am.dst_live, am_live);
}

#[test]
fn files_sidecars_documents_rules() {
    let (src, dst_root) = stage("filesrules");
    let opts = CopyOptions { copy_all: true, ..Default::default() };
    let rep = copy_engagement(&src, &dst_root, "TOUT3", &opts).unwrap();
    let d = dst_root.join("TOUT3");
    // sidecar rename, verbatim bytes
    assert_eq!(
        std::fs::read(d.join("TOUT3AC.FPT")).unwrap(),
        std::fs::read(gold().join("tables").join("ac.fpt")).unwrap()
    );
    // cvw rename by suffix (document key preserved)
    assert_eq!(std::fs::read(d.join("TOUT301200RG2.cvw")).unwrap(), b"CVWPATH");
    // hub renamed
    assert_eq!(std::fs::read(d.join("TOUT3.ac")).unwrap(), b"AC-HUB");
    // plain document carried; runtime artifacts skipped (F64 (2))
    assert_eq!(std::fs::read(d.join("Doc1.docx")).unwrap(), b"DOC");
    assert!(!d.join("copy.ini").exists());
    assert!(!d.join("x.locks").exists());
    assert!(rep.files.contains(&"TOUT3AC.dbf".to_string()));
}

#[test]
fn no_selection_is_an_error() {
    let (src, dst_root) = stage("nosel");
    let err = copy_engagement(&src, &dst_root, "TOUT4", &CopyOptions::default()).unwrap_err();
    assert!(format!("{err:?}").contains("no copy selection"));
}

