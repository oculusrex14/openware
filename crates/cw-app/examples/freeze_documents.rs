//! Freeze `tests/golden/documents/law.json` — the P5.G G4 `SH`/`WR`/`WT` law.
//!
//! ## Provenance (2026-10-28 rebuild)
//!
//! The original gate had no fixture: it asserted constants transcribed from the
//! three corpora directly, and those constants were read off the **full** tables.
//! `hull2024/SH` is one of the twelve tables whose `.dbf` member is a truncated
//! LZMA stream (F63): it declares 664 records / 1,157,370 bytes and the reference
//! decoder returns 1,049,873 bytes, i.e. 603 records (`guide_law_gate.rs` pins the
//! same frontier in its `DAMAGED` table). Rows 603..664 are zero-fill synthesis
//! whose row flags are not even 0x20/0x2A, so the full-table census the old gate
//! asserted — 118 live / 546 tombstones — cannot be measured from these bytes.
//!
//! This fixture therefore records, per corpus, the **frontier** (the number of
//! rows the law is asserted over, `None` = the whole table) and the census of
//! `rows[..frontier]`. The frontier is a frozen constant, never a search, so newly
//! damaged bytes fail loudly instead of silently shrinking the asserted set.
//! `corp2026` and `mwgh2025` are readable end to end and keep their original
//! numbers exactly.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cw_app::documents::{DocumentBook, NoteBook, ReferenceBook};
use cw_core::dbf::Table;

/// (corpus dir, engagement stem, SH readable frontier from
/// `cw-core/tests/guid_law_gate.rs` `DAMAGED`; `None` = readable end to end).
const CORPORA: [(&str, &str, Option<usize>); 3] = [
    ("hull2024", "2024 Hull Lift Truck Review(1148441)", Some(603)),
    ("mwgh2025", "MWGH LLC 9-30-25 REVIEW(1239263)", None),
    ("corp2026", "USI Compilation Training(1155962)", None),
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
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

fn usizemap(m: &BTreeMap<String, usize>) -> String {
    let parts: Vec<String> = m.iter().map(|(k, v)| format!("{}: {v}", jstr(k))).collect();
    format!("{{{}}}", parts.join(", "))
}

fn chmap(m: &BTreeMap<char, usize>) -> String {
    let parts: Vec<String> = m.iter().map(|(k, v)| format!("{}: {v}", jstr(&k.to_string()))).collect();
    format!("{{{}}}", parts.join(", "))
}

fn main() {
    let golden = root().join("tests/golden");
    let out = golden.join("documents");
    std::fs::create_dir_all(&out).unwrap();

    let mut entries = String::new();
    for (corpus, stem, frontier) in CORPORA {
        let dir = golden.join("dbf").join(corpus);
        let sh_raw = std::fs::read(dir.join(format!("{stem}SH.dbf"))).unwrap();
        let mut table = Table::parse(&sh_raw).unwrap();
        let declared = table.count();
        if let Some(f) = frontier {
            assert!(f < declared, "{corpus}: frontier must cut the fabricated tail");
            table.records.truncate(f);
        }
        // Re-encode the slice so the loader sees a self-consistent record count.
        let sh_bytes = table.encode();
        let book = DocumentBook::from_bytes(&sh_bytes).unwrap();
        assert_eq!(book.encode(), sh_bytes, "{corpus}: SH no-op save");
        let c = book.census();

        let wr_raw = std::fs::read(dir.join(format!("{stem}WR.dbf"))).unwrap();
        let wr_fpt = std::fs::read(dir.join(format!("{stem}WR.fpt")))
            .or_else(|_| std::fs::read(dir.join(format!("{stem}WR.FPT"))))
            .unwrap_or_default();
        let wr = ReferenceBook::from_bytes(&wr_raw, &wr_fpt).unwrap();
        let wt_raw = std::fs::read(dir.join(format!("{stem}WT.dbf"))).unwrap();
        let wt = NoteBook::from_bytes(&wt_raw).unwrap();

        // Corp-specific detail the gate pins (register lattice, forms, tree).
        let ordered = book.ordered();
        let app: Vec<i64> = ordered.iter().filter(|d| d.is_app()).filter_map(|d| d.lineno).collect();
        let forms: Vec<String> = book
            .forms()
            .iter()
            .map(|f| {
                format!(
                    "{{\"cvid\": {}, \"schedule\": {}, \"title\": {}}}",
                    jstr(f.cvid.trim()),
                    jstr(f.schedule.trim()),
                    jstr(f.title.trim())
                )
            })
            .collect();
        let headings: Vec<String> =
            book.headings().iter().map(|h| jstr(h.title.trim())).collect();

        if !entries.is_empty() {
            entries.push_str(",\n");
        }
        entries.push_str(&format!(
            r#"  {key}: {{
    "stem": {stem},
    "frontier": {frontier},
    "declared_records": {declared_records},
    "sh": {{
      "live": {live},
      "deleted": {deleted},
      "by_type": {by_type},
      "by_prg": {by_prg},
      "headings": {headings},
      "forms": {forms},
      "analyses": {analyses},
      "index_rows": {index_rows},
      "archived": {archived},
      "rolled": {rolled},
      "locked": {locked},
      "blank_lineno": {blank_lineno},
      "with_parent": {with_parent},
      "roots": {roots},
      "cycles": {cycles},
      "app_lineno_first": {app_first},
      "app_lineno_last": {app_last},
      "app_rows": {app_rows}
    }},
    "wr": {{"live": {wr_live}, "physical": {wr_phys}}},
    "wt": {{"live": {wt_live}, "physical": {wt_phys}}},
    "forms_detail": [{form_titles}],
    "heading_titles": [{heading_titles}]
  }}"#,
            key = jstr(corpus),
            stem = jstr(stem),
            frontier = frontier.map(|f| f.to_string()).unwrap_or_else(|| "null".into()),
            declared_records = declared,
            live = c.live,
            deleted = c.deleted,
            by_type = chmap(&c.by_type),
            by_prg = usizemap(&c.by_prg),
            headings = c.headings,
            forms = c.forms,
            analyses = c.analyses,
            index_rows = c.index_rows,
            archived = c.archived,
            rolled = c.rolled,
            locked = c.locked,
            blank_lineno = c.blank_lineno,
            with_parent = c.with_parent,
            roots = c.roots,
            cycles = c.cycles,
            app_first = app.first().map(|v| v.to_string()).unwrap_or_else(|| "null".into()),
            app_last = app.last().map(|v| v.to_string()).unwrap_or_else(|| "null".into()),
            app_rows = app.len(),
            wr_live = wr.count(),
            wr_phys = wr.table().count(),
            wt_live = wt.count(),
            wt_phys = wt.table().count(),
            form_titles = forms.join(", "),
            heading_titles = headings.join(", "),
        ));
        println!(
            "{corpus}: SH {}/{} over {} rows (declared {declared}), WR {}/{}, WT {}/{}",
            c.live, c.deleted, table.count(), wr.count(), wr.table().count(), wt.count(), wt.table().count()
        );
    }

    let law = format!(
        r#"{{
  "provenance": "kernel-derived 2026-10-28 by crates/cw-app/examples/freeze_documents.rs over tests/golden/dbf/{{hull2024,mwgh2025,corp2026}} (the C1 verbatim freeze). Per corpus, `frontier` is the SH readable-row count from cw-core/tests/guid_law_gate.rs DAMAGED (null = the table is readable end to end); the census is measured over rows[..frontier]. hull2024/SH is truncated LZMA (664 declared / 603 readable), which is why its census differs from the pre-rebuild full-table constants (118 live / 546 tombstones); corp2026 and mwgh2025 keep their original numbers exactly.",
  "corpora": {{
{entries}
  }}
}}
"#
    );
    std::fs::write(out.join("law.json"), law).unwrap();
    println!("wrote {}/law.json", out.display());
}
