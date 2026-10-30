//! P5.D2 gate: cw-app year-end close vs the F46/F65 law.
//!
//! Golden: `tests/golden/yearend/law.json` + `on/{bl,sh,ts,wr,fp,gl,hi}.dbf` +
//! `off/{…}.dbf`. The engine is run over a pristine staged copy of
//! `tests/golden/dbf/corp2026`.
//!
//! ## Provenance
//!
//! The **numbers** are spec-recorded: `attachments/yearend.md` v1.1 (F46/F65)
//! measured them on `fixture-yec1` (options off) and `fixture-yec2` (options on),
//! two COM close runs over this corpus, and every one is asserted below as a
//! literal (1,077 rows, 52 closing accounts, 12 `P` twins, 26 year-1 `Y` rows, RE
//! opening `-6,806,301.24`, net income `-3,061,019.24`, `SH` 93 -> 86). The
//! destination **tables** (`on/`, `off/`) are self-frozen by
//! `examples/freeze_yearend.rs` because CW's own yec1/yec2 tables are not part of
//! this rebuild: they are a regression pin on the row-set projection, not a second
//! implementation, and the spec literals carry the independent authority.
//!
//! Row-set comparison ignores CWGUID/CWDATE/CREATEDON (byte cosmetics); the four
//! FP cells whose TEXT is a fresh interlink GUID are blanked for the key and
//! flagged as the documented P5.F residual.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cw_app::yearend::{close_year, CloseStamp, YearEndOptions, RE_REQUIRED};
use cw_core::dbf::{RawRecord, Table};
use cw_core::json;

const SRC_STEM: &str = "USI Compilation Training(1155962)";
const IGNORED: [&str; 3] = ["CWGUID", "CWDATE", "CREATEDON"];
const FP_OPEN_TIME_CELLS: [&str; 2] = ["146", "254"];

fn golden() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("..");
    p.push("..");
    p.push("tests");
    p.push("golden");
    p
}

fn tmp(name: &str) -> PathBuf {
    // Keep fixture scratch inside the repo (AGENTS.md: writes only under OpenCW).
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("..");
    p.push("..");
    p.push("target");
    p.push("testbin");
    p.push(format!("d2-{name}-{}", std::process::id()));
    p
}

fn corpus() -> PathBuf {
    golden().join("dbf").join("corp2026")
}

/// Stage a pristine engagement from the frozen corpus + a fake hub/documents.
fn stage(tag: &str) -> (PathBuf, PathBuf) {
    let src = tmp(tag);
    let _ = std::fs::remove_dir_all(&src);
    std::fs::create_dir_all(&src).unwrap();
    for e in std::fs::read_dir(corpus()).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        if name.starts_with(SRC_STEM) && (lower.ends_with(".dbf") || lower.ends_with(".fpt") || lower.ends_with(".cdx")) {
            std::fs::copy(e.path(), src.join(&name)).unwrap();
        }
    }
    std::fs::write(src.join(format!("{SRC_STEM}.ac")), b"AC-HUB").unwrap();
    std::fs::write(src.join(format!("{SRC_STEM}01200RG2.cvw")), b"CVW").unwrap();
    std::fs::write(src.join("Doc1.docx"), b"DOC").unwrap();
    std::fs::write(src.join("copy.ini"), b"[GROUPS]").unwrap();
    std::fs::write(src.join("x.locks"), b"").unwrap();
    let root = tmp(&format!("{tag}-out"));
    let _ = std::fs::remove_dir_all(&root);
    (src, root)
}

fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut m = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        if e.path().is_file() {
            m.insert(
                e.file_name().to_string_lossy().to_string(),
                std::fs::read(e.path()).unwrap(),
            );
        }
    }
    m
}

fn out_table(dir: &Path, stem: &str, sfx: &str) -> Table {
    let want = format!("{stem}{sfx}.dbf").to_ascii_lowercase();
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().to_string_lossy().to_string();
        if name.to_ascii_lowercase() == want {
            return parse_table(&e.path());
        }
    }
    panic!("missing output table {stem}{sfx}.dbf in {}", dir.display());
}

/// CW can write an emptied table as a header-only file (no 0x1A EOF): fixture
/// `HI` destination is 481 bytes ending at the descriptor terminator. Accept
/// that shape for golden comparison (row-set law is unaffected).
fn parse_table(p: &Path) -> Table {
    let bytes = std::fs::read(p).unwrap();
    match Table::parse(&bytes) {
        Ok(t) => t,
        Err(_) => {
            let mut b = bytes;
            if b.last() != Some(&0x1A) {
                b.push(0x1A);
            }
            Table::parse(&b).unwrap_or_else(|e| panic!("parse {}: {e:?}", p.display()))
        }
    }
}

/// Cell offsets/windows keyed by uppercased field name.
fn layout(t: &Table) -> Vec<(String, usize, usize)> {
    let mut v = Vec::new();
    let mut off = 1usize;
    for f in &t.fields {
        v.push((f.name.to_ascii_uppercase(), off, f.len as usize));
        off += f.len as usize;
    }
    v
}

fn cell<'a>(t: &Table, rec: &'a [u8], name: &str) -> &'a [u8] {
    for (n, o, w) in layout(t) {
        if n == name.to_ascii_uppercase() {
            return &rec[o..o + w];
        }
    }
    panic!("missing field {name}");
}

fn text(t: &Table, rec: &[u8], name: &str) -> String {
    cw_core::canon::latin1(cell(t, rec, name)).trim().to_string()
}

/// Order-free semantic multiset: flag + every cell except the byte-cosmetic
/// fields. `blank_fp_text` blanks TEXT for the open-time GUID-rewire cells.
fn semantic(t: &Table, blank_fp_text: bool) -> BTreeMap<Vec<u8>, usize> {
    let mut keep = Vec::new();
    let mut text_idx = None;
    for (i, f) in t.fields.iter().enumerate() {
        let n = f.name.to_ascii_uppercase();
        if IGNORED.contains(&n.as_str()) {
            continue;
        }
        if n == "TEXT" {
            text_idx = Some(keep.len());
        }
        keep.push(i);
    }
    let offs = t.offsets();
    let mut m = BTreeMap::new();
    for r in &t.records {
        let fp_cell = blank_fp_text
            && text_idx.is_some()
            && FP_OPEN_TIME_CELLS.contains(&text(t, r, "CELLNO").as_str());
        let mut key = vec![r[0]];
        for (pos, &i) in keep.iter().enumerate() {
            let f = &t.fields[i];
            let raw = &r[offs[i]..offs[i] + f.len as usize];
            if fp_cell && Some(pos) == text_idx {
                key.extend(std::iter::repeat(b' ').take(f.len as usize));
            } else {
                key.extend_from_slice(raw);
            }
        }
        *m.entry(key).or_insert(0) += 1;
    }
    m
}

fn assert_same(tag: &str, got: &Table, want: &Table, blank_fp_text: bool) {
    assert_eq!(
        semantic(got, blank_fp_text),
        semantic(want, blank_fp_text),
        "{tag}: row-set mismatch ({} got / {} want)",
        got.records.len(),
        want.records.len()
    );
}

fn live(t: &Table) -> Vec<&RawRecord> {
    t.records.iter().filter(|r| r[0] == b' ').collect()
}

fn count_year0_p(o: &Table, id: &str, year: &str) -> usize {
    o.records
        .iter()
        .filter(|r| {
            r[0] == b' '
                && text(o, r, "YEAR") == year
                && text(o, r, "ID") == id
        })
        .count()
}

fn law() -> json::Json {
    let p = golden().join("yearend").join("law.json");
    json::parse(&std::fs::read_to_string(&p).unwrap_or_else(|e| {
        panic!("{}: {e} (run `cargo run -p cw-app --example freeze_yearend`)", p.display())
    }))
    .unwrap()
}

/// A frozen destination table for one side (`on` = options-on, `off` = off).
fn dest_golden(side: &str, sfx: &str) -> PathBuf {
    golden()
        .join("yearend")
        .join(side)
        .join(format!("{}.dbf", sfx.to_ascii_lowercase()))
}

#[test]
fn close_on_matches_cw_destination() {
    let (src, root) = stage("on");
    let before = snapshot(&src);
    let stamp = CloseStamp::new("20261022053523", 0xC0FFEE);
    let opts = YearEndOptions::balance_carry("2920-00");
    let rep = close_year(&src, &root, "YEC", &opts, &stamp).unwrap();
    assert_eq!(rep.source_stem, SRC_STEM);
    assert_eq!(rep.closing_accounts, 52, "closing B accounts");
    assert_eq!(rep.p_twins, 12, "P twins");
    assert_eq!(rep.net_income_cents, -306_101_924, "spec: income result");

    let d = root.join("YEC");

    // BL: all 1077 semantic rows leaf-for-leaf
    let bl = out_table(&d, "YEC", "BL");
    assert_eq!(bl.records.len(), 1077);
    assert_eq!(live(&bl).len(), 1077);
    assert_eq!(count_year0_p(&bl, "O", "0"), 52);
    assert_eq!(count_year0_p(&bl, "X", "0"), 52);
    assert_eq!(count_year0_p(&bl, "P", "1"), 12);
    assert_eq!(
        bl.records.iter().filter(|r| text(&bl, r, "YEAR") == "1" && text(&bl, r, "BUCKET") == "Y").count(),
        26,
        "14 carried Y rows + 12 P twins"
    );
    let re_row = bl
        .records
        .iter()
        .find(|r| {
            text(&bl, r, "YEAR") == "0"
                && text(&bl, r, "ID") == "O"
                && text(&bl, r, "NUMBER") == "2920-00"
        })
        .expect("RE opening row");
    assert_eq!(text(&bl, re_row, "AMT"), "-6806301.24");
    assert_same("BL", &bl, &Table::parse_path(dest_golden("on", "BL")).unwrap(), false);

    // Side tables: SH/TS/WR/GL/HI row-set laws
    for sfx in ["SH", "TS", "WR", "GL", "HI"] {
        let got = out_table(&d, "YEC", sfx);
        let want = parse_table(&dest_golden("on", sfx));
        assert_same(sfx, &got, &want, false);
    }

    // FP: dates/profile cells + the four fresh-GUID cells allowed to differ
    let fp = out_table(&d, "YEC", "FP");
    let fp_want = Table::parse_path(dest_golden("on", "FP")).unwrap();
    assert_same("FP", &fp, &fp_want, true);
    let fp_open = fp
        .records
        .iter()
        .filter(|r| FP_OPEN_TIME_CELLS.contains(&text(&fp, r, "CELLNO").as_str()))
        .count();
    assert_eq!(fp_open, 4, "open-time GUID rewiring cells");

    // AM is carried byte-identically (no period-column rewrite, F65)
    let am = std::fs::read(
        std::fs::read_dir(&src)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.file_name().unwrap().to_string_lossy().eq_ignore_ascii_case(&format!("{SRC_STEM}AM.dbf")))
            .unwrap(),
    )
    .unwrap();
    let am_out = std::fs::read(
        std::fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.file_name().unwrap().to_string_lossy().eq_ignore_ascii_case("YECAM.dbf"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(am, am_out, "AM carried byte-identically");

    // untransformed tables are byte-identical (parse -> encode no-op)
    for sfx in ["AC", "AN", "CE", "CV", "EC", "GR", "GS", "MP", "PR", "RA", "RL", "TM", "TP", "WT"] {
        let find = |dir: &Path, stem: &str| -> Vec<u8> {
            let want = format!("{stem}{sfx}.dbf").to_ascii_lowercase();
            let p = std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .find(|p| p.file_name().unwrap().to_string_lossy().to_ascii_lowercase() == want)
                .unwrap_or_else(|| panic!("missing {sfx} in {}", dir.display()));
            std::fs::read(p).unwrap()
        };
        assert_eq!(find(&src, SRC_STEM), find(&d, "YEC"), "{sfx} carried verbatim");
    }

    // law.json agrees with the measured run and with the spec constants.
    let l = law();
    let spec = l.get("spec").unwrap();
    let measured = l.get("measured").unwrap();
    let n = |j: &json::Json, k: &str| j.get(k).and_then(|v| v.as_usize()).unwrap_or(usize::MAX);
    assert_eq!(n(spec, "bl_rows_on"), 1077, "spec: destination BL rows");
    assert_eq!(n(spec, "closing_accounts"), 52);
    assert_eq!(n(spec, "p_twins"), 12);
    assert_eq!(n(spec, "sh_live_after"), 86);
    assert_eq!(n(spec, "sh_signoff_cleared"), 52);
    assert_eq!(measured.get("re_opening").and_then(|v| v.as_str()), Some("-6806301.24"));
    assert_eq!(measured.get("net_income_cents").and_then(|v| v.as_str()), Some("-306101924"));
    assert_eq!(n(measured, "closing_accounts"), rep.closing_accounts);
    assert_eq!(n(measured, "p_twins"), rep.p_twins);
    assert_eq!(n(measured, "bl_records"), 1077);
    assert_eq!(n(measured, "year0_o"), 52);
    assert_eq!(n(measured, "year0_x"), 52);
    assert_eq!(n(measured, "year1_y"), 26);
    assert_eq!(n(measured, "p_rows"), 12);
    assert_eq!(rep.files.iter().any(|f| f.eq_ignore_ascii_case("YECBL.dbf")), true);

    // source never mutated
    assert_eq!(before, snapshot(&src), "source engagement unchanged");

    // no stray runtime artifacts in the destination
    assert!(!d.join("copy.ini").exists());
    assert!(!d.join("x.locks").exists());
}

#[test]
fn close_off_empties_bl_and_reproduces_side_tables() {
    let (src, root) = stage("off");
    let stamp = CloseStamp::new("20261022053146", 7);
    let rep = close_year(&src, &root, "YEO", &YearEndOptions::default(), &stamp).unwrap();
    assert_eq!(rep.closing_accounts, 0);
    assert_eq!(rep.net_income_cents, 0);

    let d = root.join("YEO");
    let bl = out_table(&d, "YEO", "BL");
    assert_eq!(bl.records.len(), 0, "options-off destination BL is empty");
    assert_same("BL-off", &bl, &Table::parse_path(dest_golden("off", "BL")).unwrap(), false);

    // The spec (yearend.md §6) states the side tables are the same in both runs
    // except BL/FP: SH/TS/WR/GL/HI are option-independent.
    for sfx in ["SH", "TS", "WR", "GL", "HI"] {
        let got = out_table(&d, "YEO", sfx);
        let want = parse_table(&dest_golden("on", sfx));
        assert_same(sfx, &got, &want, false);
    }
    let fp = out_table(&d, "YEO", "FP");
    let fp_want = Table::parse_path(dest_golden("off", "FP")).unwrap();
    assert_same("FP-off", &fp, &fp_want, true);
}

#[test]
fn retained_earning_account_is_required() {
    let (src, root) = stage("re");
    let err = close_year(
        &src,
        &root,
        "YER",
        &YearEndOptions {
            update_next_year_opening_balance: true,
            ..Default::default()
        },
        &CloseStamp::default(),
    )
    .unwrap_err();
    assert!(format!("{err:?}").contains(RE_REQUIRED), "exact CW validation text");
    assert!(!root.join("YER").exists(), "no destination on validation failure");
}

#[test]
fn close_is_deterministic() {
    let (src, root) = stage("det");
    let stamp = CloseStamp::new("20260101010101", 99);
    let opts = YearEndOptions::balance_carry("2920-00");
    let a = close_year(&src, &root.join("a"), "YEA", &opts, &stamp).unwrap();
    let b = close_year(&src, &root.join("b"), "YEB", &opts, &stamp).unwrap();
    assert_eq!(a.tables, b.tables);
    let ta = out_table(&root.join("a").join("YEA"), "YEA", "BL");
    let tb = out_table(&root.join("b").join("YEB"), "YEB", "BL");
    assert_eq!(ta.records, tb.records, "same options + stamp => same bytes");
}

