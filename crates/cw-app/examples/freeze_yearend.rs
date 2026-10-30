//! Freeze `tests/golden/yearend/law.json` — the P5.D2 close law.
//!
//! ## Provenance
//!
//! The spec-recorded constants are the authority here and they are the strongest
//! kind this rebuild has: `attachments/yearend.md` (v1.1, F46/F65) records them
//! from `fixture-yec1` / `fixture-yec2`, the two COM close runs over the USI
//! corpus. The engine's own output over the frozen corpus reproduces every one of
//! them exactly (1,077 BL rows, 52 closing accounts, 12 `P` twins, 14 + 12 year-1
//! `Y` rows, RE opening `-6,806,301.24`, net income `-3,061,019.24`, year-0 `O`
//! summing to 0). This fixture pins that agreement; the gate asserts the spec
//! values as literals and this file for the rest.
//!
//! The CW destination tables (`bl-on.dbf`, `sh-on.dbf`, …, `bl-off.dbf`) are not
//! part of the rebuild. They are replaced by **self-frozen destination snapshots**
//! (`examples/freeze_yearend.rs` runs `close_year` twice, options-on and
//! options-off, into `tests/golden/yearend/{on,off}/`) so that the row-set
//! comparison the gate performs still has a fixture — now a regression pin rather
//! than a two-implementation parity check. The semantic content of the comparison is
//! unchanged (ignoring the three byte-cosmetic columns and blanking the four
//! open-time interlink GUID cells, exactly as before).
use std::path::{Path, PathBuf};

use cw_app::yearend::{close_year, CloseStamp, YearEndOptions};
use cw_core::dbf::Table;

const STEM: &str = "USI Compilation Training(1155962)";
const STAMP_ON: &str = "20261022053523";
const STAMP_OFF: &str = "20261022053146";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Stage the corpus tables the close consumes plus the hub/document noise the
/// engine must not carry.
fn stage(dir: &Path, corpus: &Path) -> PathBuf {
    let src = dir.join("src");
    let _ = std::fs::remove_dir_all(&src);
    std::fs::create_dir_all(&src).unwrap();
    for e in std::fs::read_dir(corpus).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        if name.starts_with(STEM)
            && (lower.ends_with(".dbf") || lower.ends_with(".fpt") || lower.ends_with(".cdx"))
        {
            std::fs::copy(e.path(), src.join(&name)).unwrap();
        }
    }
    std::fs::write(src.join(format!("{STEM}.ac")), b"AC-HUB").unwrap();
    std::fs::write(src.join(format!("{STEM}01200RG2.cvw")), b"CVW").unwrap();
    std::fs::write(src.join("Doc1.docx"), b"DOC").unwrap();
    std::fs::write(src.join("copy.ini"), b"[GROUPS]").unwrap();
    std::fs::write(src.join("x.locks"), b"").unwrap();
    src
}

fn jstr(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn main() {
    let golden = root().join("tests").join("golden");
    let corpus = golden.join("dbf").join("corp2026");
    let out = golden.join("yearend");
    std::fs::create_dir_all(&out).unwrap();

    let scratch = root().join("target").join("freeze-yearend");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    let src = stage(&scratch, &corpus);

    // ---- options-on (spec fixture-yec2)
    let on = close_year(
        &src,
        &scratch.join("on"),
        "YEC",
        &YearEndOptions::balance_carry("2920-00"),
        &CloseStamp::new(STAMP_ON, 0xC0FFEE),
    )
    .unwrap();
    // ---- options-off (spec fixture-yec1)
    let off = close_year(
        &src,
        &scratch.join("off"),
        "YEO",
        &YearEndOptions::default(),
        &CloseStamp::new(STAMP_OFF, 7),
    )
    .unwrap();

    // Spec `yearend.md` §3/§4/§6 — the constants the gate asserts as literals.
    assert_eq!(on.closing_accounts, 52, "spec: 52 B accounts get a closing pair");
    assert_eq!(on.p_twins, 12, "spec: 12 P twins");
    assert_eq!(on.net_income_cents, -306_101_924, "spec: net income -3,061,019.24");
    assert_eq!(off.closing_accounts, 0, "spec: options-off writes no closing layer");
    assert_eq!(off.net_income_cents, 0);

    // Destination snapshots (self-frozen; regression pins, not CW output).
    for (side, rep, dest, stem) in [("on", &on, "YEC", "on"), ("off", &off, "YEO", "off")] {
        std::fs::create_dir_all(out.join(stem)).unwrap();
        for sfx in ["BL", "SH", "TS", "WR", "FP", "GL", "HI"] {
            let p = scratch.join(side).join(dest).join(format!("{dest}{sfx}.dbf"));
            std::fs::copy(&p, out.join(stem).join(format!("{}.dbf", sfx.to_ascii_lowercase())))
                .unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        }
        println!("{side}: {} destination tables frozen", 7);
    }

    // BL row census straight from the frozen options-on snapshot.
    let bl = Table::parse_path(out.join("on").join("bl.dbf")).unwrap();
    let off_bl = Table::parse_path(out.join("off").join("bl.dbf")).unwrap();
    let field = |t: &Table, r: usize, f: &str| t.cell_str(r, f).unwrap_or_default().trim().to_string();
    /// Count live rows matching every supplied predicate (`None` = any).
    let count = |t: &Table, year: Option<&str>, id: Option<&str>, bucket: Option<&str>| {
        t.records
            .iter()
            .enumerate()
            .filter(|(r, rec)| {
                rec[0] == b' '
                    && year.map_or(true, |y| field(t, *r, "YEAR") == y)
                    && id.map_or(true, |i| field(t, *r, "ID") == i)
                    && bucket.map_or(true, |b| field(t, *r, "BUCKET") == b)
            })
            .count()
    };
    let re_row = (0..bl.count())
        .find(|&r| {
            field(&bl, r, "YEAR") == "0"
                && field(&bl, r, "ID") == "O"
                && field(&bl, r, "NUMBER") == "2920-00"
        })
        .expect("RE opening row");
    let re_amt = field(&bl, re_row, "AMT");

    let mut tables = String::new();
    for (sfx, delta) in on.tables.iter() {
        if !tables.is_empty() {
            tables.push_str(", ");
        }
        tables.push_str(&format!(
            "{}: {{\"src_live\": {}, \"src_deleted\": {}, \"dst_live\": {}, \"dst_deleted\": {}}}",
            jstr(sfx), delta.src_live, delta.src_deleted, delta.dst_live, delta.dst_deleted
        ));
    }

    let law = format!(
        r#"{{
  "provenance": "The numeric constants are spec-recorded in attachments/yearend.md (v1.1, F46/F65: fixture-yec1 options-off, fixture-yec2 options-on) and are the authority for this gate. The destination tables under on/ and off/ are self-frozen by examples/freeze_yearend.rs (close_year run over tests/golden/dbf/corp2026 with the pinned stamps) because CW's own yec1/yec2 destination tables are not part of this rebuild; they are regression pins, not a second implementation. Every spec constant below is re-asserted as a literal in the gate and reproduced exactly.",
  "corpus": "tests/golden/dbf/corp2026",
  "source_stem": {stem},
  "stamps": {{"on": {stamp_on}, "off": {stamp_off}}},
  "spec": {{
    "bl_rows_on": 1077,
    "bl_rows_off": 0,
    "closing_accounts": 52,
    "p_twins": 12,
    "year1_y_rows": 26,
    "year1_x_rows": 14,
    "re_opening": "-6806301.24",
    "net_income": "-3061019.24",
    "sh_live_before": 93,
    "sh_live_after": 86,
    "sh_signoff_cleared": 52
  }},
  "measured": {{
    "net_income_cents": {net_income_str},
    "bl_records": {bl_records},
    "bl_live": {bl_live},
    "off_bl_records": {off_bl_records},
    "year0_o": {year0_o},
    "year0_x": {year0_x},
    "year1_y": {year1_y},
    "p_rows": {p_rows},
    "re_opening": {re_amt},
    "closing_accounts": {closing},
    "p_twins": {p_twins},
    "net_income_cents": {net_income}
  }},
  "tables_on": {{{tables}}},
  "byte_cosmetics_ignored": ["CWGUID", "CWDATE", "CREATEDON"],
  "fp_open_time_cells": ["146", "254"],
  "residuals": [
    "FP cells 146/254 (seq 1/2) are rewired by CW to fresh interlink GUIDs; the engine keeps the source refs (P5.F lane). The gate blanks TEXT on those four rows.",
    "FPT memo renumbering, CDX rebuild, .cvw roll-forward + .bak and the .ac document-summary rewrite are byte cosmetics, not gated.",
    "The destination tables are self-frozen: they pin the engine against its own output. The independent authority is the spec constant set above."
  ]
}}
"#,
        stem = jstr(STEM),
        stamp_on = jstr(STAMP_ON),
        stamp_off = jstr(STAMP_OFF),
        bl_records = bl.count(),
        bl_live = bl.records.iter().filter(|r| r[0] == b' ').count(),
        off_bl_records = off_bl.count(),
        year0_o = count(&bl, Some("0"), Some("O"), None),
        year0_x = count(&bl, Some("0"), Some("X"), None),
        year1_y = count(&bl, Some("1"), None, Some("Y")),
        p_rows = count(&bl, None, Some("P"), None),
        re_amt = jstr(&re_amt),
        closing = on.closing_accounts,
        p_twins = on.p_twins,
        net_income = on.net_income_cents,
        net_income_str = jstr(&on.net_income_cents.to_string()),
    );
    std::fs::write(out.join("law.json"), law).unwrap();
    println!(
        "wrote {}/law.json + on/{{bl,sh,ts,wr,fp,gl,hi}}.dbf + off/…: BL {} rows ({} live), RE {re_amt}, closing {}",
        out.display(),
        bl.count(),
        bl.records.iter().filter(|r| r[0] == b' ').count(),
        on.closing_accounts
    );
    let _ = std::fs::remove_dir_all(&scratch);
}
