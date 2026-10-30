//! Freeze `tests/golden/lifecycle/{tables/,law.json}` — the P5.D1 copy probe.
//!
//! ## Provenance (2026-10-28 rebuild)
//!
//! The original fixture was the *probe-source* engagement produced by the COM
//! copy round (`a sample copy`, F64): four tables in the post-AJE working state
//! plus the census CW's `CopyAll` reported. That copy is not part of this
//! rebuild, so the fixture is **kernel-derived** over the frozen C1 corpus
//! `tests/golden/dbf/corp2026` — the same engagement, at the state the package
//! shipped. Only the census constants are re-grounded; every F64 *law* the gate
//! asserts (live rows only, flags packed to 0, live multiset preserved,
//! ClearAccountBalances = BL flip + GL empty, sidecar/document rename rules) is
//! unchanged and is a property of the engine, not of the row counts.
//!
//! Probe census vs frozen corpus: `AC 77/1` and `AM 325/0` are identical (the
//! AJE round never touched the chart or the journal definitions); `BL` is
//! `962 live / 1198 tombstone` (not `984/1200`) and `GL` is `17 live / 3
//! tombstone` (not `54/3`) — the shipped package predates the probe's extra
//! adjusting entries. `tests/golden/dbf/README.md` (C1) is the byte source.
use cw_core::dbf::Table;
use std::path::{Path, PathBuf};

const STEM: &str = "USI Compilation Training(1155962)";
/// The four probe tables the gate stages (`law.json` order = this order).
const TABLES: [&str; 4] = ["AC", "AM", "BL", "GL"];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn main() {
    let corpus = root().join("tests/golden/dbf").join("corp2026");
    let out = root().join("tests/golden/lifecycle");
    std::fs::create_dir_all(out.join("tables")).unwrap();

    let mut census = String::new();
    for t in TABLES {
        let src = corpus.join(format!("{STEM}{t}.dbf"));
        let bytes = std::fs::read(&src).unwrap_or_else(|e| panic!("{}: {e}", src.display()));
        let table = Table::parse(&bytes).unwrap_or_else(|e| panic!("{t}: {e:?}"));
        let (live, deleted) = census_of(&table);
        std::fs::write(out.join("tables").join(format!("{}.dbf", t.to_ascii_lowercase())), &bytes)
            .unwrap();
        if !census.is_empty() {
            census.push_str(", ");
        }
        census.push_str(&format!(
            "\"{t}\": {{\"live\": {live}, \"deleted\": {deleted}, \"records\": {}}}",
            table.count()
        ));
    }
    // The gate stages `tables/ac.fpt` verbatim and compares the copied sidecar
    // byte-for-byte (spec `lifecycle.md` §4: fpt/cdx are renamed, not rewritten).
    let fpt = corpus.join(format!("{STEM}AC.FPT"));
    std::fs::copy(&fpt, out.join("tables").join("ac.fpt"))
        .unwrap_or_else(|e| panic!("{}: {e}", fpt.display()));

    let law = format!(
        r#"{{
  "provenance": "kernel-derived 2026-10-28 by crates/cw-app/examples/freeze_lifecycle.rs over tests/golden/dbf/corp2026 (the C1 verbatim freeze). Replaces the lost COM probe-source copy ('a sample copy', F64); AC 77/1 and AM 325/0 match that probe exactly, BL and GL are re-grounded on the frozen bytes (BL 962 live / 1198 tombstone, GL 17 live / 3 tombstone). Every F64 law the gate asserts is unchanged.",
  "source_stem": "TPROBE",
  "dest_stem": "TOUT",
  "tables": {{{census}}},
  "sidecars": {{"AC": "fpt"}},
  "documents": {{"cvw": "TPROBE01200RG2.cvw", "hub": "TPROBE.ac", "plain": "Doc1.docx"}},
  "skip": ["copy.ini", "CmpChart.txt"],
  "skip_suffix": ".locks",
  "laws": {{
    "copy_all": "live rows only; deletion flags packed to 0; live row multiset preserved byte-for-cell (F64 (3))",
    "clear_account_balances": "BL former-live rows flag-deleted; GL emptied entirely; AM/AC as CopyAll (F64 (4))",
    "documents": "sidecar/hub renamed to the destination stem, plain documents carried, runtime artifacts skipped (F64 (2))",
    "no_selection": "DoCopy raises 'Must complete components information' when no copy flag is set (F64 (1))"
  }}
}}
"#
    );
    std::fs::write(out.join("law.json"), law).unwrap();
    println!("wrote {}/law.json + tables/{{ac,am,bl,gl}}.dbf + tables/ac.fpt", out.display());
}

fn census_of(t: &Table) -> (usize, usize) {
    let mut live = 0;
    let mut del = 0;
    for r in &t.records {
        match r[0] {
            0x2A => del += 1,
            0x20 => live += 1,
            _ => {}
        }
    }
    (live, del)
}
