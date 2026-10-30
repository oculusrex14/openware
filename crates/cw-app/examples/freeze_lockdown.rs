//! Freeze `tests/golden/lockdown/{before.ac,law.json}` — the P5.D4b lockdown law.
//!
//! ## Provenance (2026-10-28 rebuild)
//!
//! The original fixture was one COM `client.Lockdown()` round on a sample copy
//! (`the sample set`, F68): `before.ac` / `after.ac` hubs, `sh-after.dbf`,
//! `fp-after.dbf` and `lockdown-corp2026.json`. Those working copies are not part
//! of this rebuild.
//!
//! What survives, and in which class of authority:
//!
//! - **`before.ac`** is rebuilt here from real corpus bytes: the frozen package
//!   `tests/golden/pkg/corp2026.ac_` is a CFB whose ten streams are the hub's own
//!   container metadata (`\x05DocumentSummaryInformation` at its real 5,204 bytes,
//!   `\x05SummaryInformation`, `DmsVersionInfo`, `FileInformation`,
//!   `CaseWareProtectionInfo`, `CaseWareUserlist`, `CompressionVersion`,
//!   `DmsProperties`, `TemplateInfo`). CW's expand synthesizes the working `.ac`
//!   from exactly these (spec `backup.md` §7.2), so the property-set stream the
//!   lockdown writer patches is the genuine corpus stream — not a synthetic
//!   stand-in. Only the `CaseWareDocument` payload stream is dropped.
//! - **`law.json`** records (a) the corpus census the law is stated over, (b) the
//!   spec-recorded lockdown constants (`lockdown.md` §2.1–§2.4, F68) — FILETIME
//!   stamp/day-end, the pid map, the FP cell map and the Excel serial, and (c) the
//!   pristine property values read out of `before.ac`.
//!
//! ## What the gate no longer does, and why it is not a weakening
//!
//! The pre-rebuild gate compared our `SH`/`FP` output **cell-for-cell against
//! CW's own destination tables**. Those tables are gone. In their place the gate
//! derives the expected destination *independently of the engine*: the corpus
//! table with exactly the cells the spec says CW writes overwritten, and it
//! asserts every other cell — tombstones included — is byte-identical. That is
//! the same claim on the touched cells and a strictly stronger claim on the
//! untouched ones (the old comparison would have accepted an engine that
//! reproduced CW's own collateral edits; this one forbids any). The one thing it
//! cannot do is discover a CW edit the spec does not record — flagged as the
//! residual at each assertion.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cw_app::lockdown::{
    excel_serial, select_documents, PropertySet, PID_LAST_CHANGE, PID_LOCKDOWN_DATE,
    PID_LOCKDOWN_STAMP, PID_LOCK_STATE, SUMMARY_STREAM,
};
use cw_core::cfb::CfbBuilder;
use cw_core::dbf::Table;
use cw_core::pkg::Pkg;

const STEM: &str = "USI Compilation Training(1155962)";

/// Spec `lockdown.md` §2.1 (F68): the operation stamp as FILETIME ticks,
/// 2026-10-23T08:27:53Z.
const STAMP_FILETIME: u64 = 134_372_176_730_000_000;
/// Spec §2.1: the lockdown date marker, 2026-10-23T23:59:59Z (day end).
const DATE_FILETIME: u64 = 134_372_735_990_000_000;
/// Spec §2.1: CW's last-change wall clock, 08:28:08.466Z — stamp + ~15 s. Our
/// writer stores the stamp itself (documented cosmetic deviation).
const CW_LAST_CHANGE: u64 = 134_372_176_884_660_000;
/// Spec §2.2: opaque `FP/CLP` cell 278 value (Excel serial of 2026-08-24).
const FP_DATE_VALUE: &str = "46258";
/// Spec §2.4: `MSD` milestone snapshot rows CW writes; v1 skips them (residual).
const MILESTONE_SNAPSHOTS: usize = 47;

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

fn main() {
    let golden = root().join("tests").join("golden");
    let out = golden.join("lockdown");
    std::fs::create_dir_all(&out).unwrap();

    // ---- before.ac: the corpus hub, built from the frozen package's own
    // container streams (the payload stream is not part of a working hub).
    let pkg_bytes = std::fs::read(golden.join("pkg").join("corp2026.ac_")).unwrap();
    let pkg = Pkg::parse(&pkg_bytes).unwrap();
    let mut hub = CfbBuilder::new();
    let mut carried = Vec::new();
    for name in [
        "\u{5}DocumentSummaryInformation",
        "\u{5}SummaryInformation",
        "CaseWareProtectionInfo",
        "CaseWareUserlist",
        "CompressionVersion",
        "DmsProperties",
        "DmsVersionInfo",
        "FileInformation",
        "TemplateInfo",
    ] {
        let stream = pkg.stream(name).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        carried.push((name.to_string(), stream.len()));
        hub.add(name, stream);
    }
    let hub_bytes = hub.build().unwrap();
    std::fs::write(out.join("before.ac"), &hub_bytes).unwrap();

    // ---- pristine summary properties (the pid table the writer must respect).
    let cfb = cw_core::cfb::Cfb::parse(&hub_bytes).unwrap();
    let summary = cfb.stream(SUMMARY_STREAM).unwrap();
    let set = PropertySet::parse(&summary).unwrap();
    let mut pids = String::new();
    for pid in set.pids() {
        if !pids.is_empty() {
            pids.push_str(",\n");
        }
        pids.push_str(&format!(
            "      {{\"pid\": {pid}, \"vt\": {}, \"value\": {}}}",
            match set.get(pid) {
                Some(cw_app::lockdown::PropValue::FileTime(_)) => 64,
                Some(cw_app::lockdown::PropValue::LpWStr(_)) => 31,
                Some(cw_app::lockdown::PropValue::LpStr(_)) => 30,
                Some(cw_app::lockdown::PropValue::I2(_)) => 2,
                Some(cw_app::lockdown::PropValue::I4(_)) => 3,
                Some(cw_app::lockdown::PropValue::Bool(_)) => 11,
                Some(cw_app::lockdown::PropValue::Empty) => 0,
                Some(cw_app::lockdown::PropValue::Other(v)) => v,
                None => 0,
            },
            jstr(
                &match set.get(pid) {
                    Some(cw_app::lockdown::PropValue::FileTime(v)) => v.to_string(),
                    Some(cw_app::lockdown::PropValue::LpWStr(v))
                    | Some(cw_app::lockdown::PropValue::LpStr(v)) => v,
                    Some(cw_app::lockdown::PropValue::I2(v)) => v.to_string(),
                    Some(cw_app::lockdown::PropValue::I4(v)) => v.to_string(),
                    Some(cw_app::lockdown::PropValue::Bool(v)) => v.to_string(),
                    _ => String::new(),
                }
            )
        ));
    }

    // ---- corpus census the law is stated over.
    let corpus = golden.join("dbf").join("corp2026");
    let sh = Table::parse(&std::fs::read(corpus.join(format!("{STEM}SH.dbf"))).unwrap()).unwrap();
    let selected = select_documents(&sh);
    let locked_guids: Vec<String> = selected.iter().map(|g| jstr(g)).collect();
    let fp = Table::parse(&std::fs::read(corpus.join(format!("{STEM}FP.dbf"))).unwrap()).unwrap();
    let hi = Table::parse(&std::fs::read(corpus.join(format!("{STEM}HI.dbf"))).unwrap()).unwrap();
    let live = |t: &Table| t.records.iter().filter(|r| r[0] == 0x20).count();
    let deleted = |t: &Table| t.records.iter().filter(|r| r[0] == 0x2A).count();

    // The CLP cells the lockdown rewrites, as the corpus stores them.
    let mut clp = Vec::new();
    for r in 0..fp.count() {
        let wks = fp.cell_str(r, "WKS").unwrap_or_default().trim().to_string();
        let cellno = fp.cell_str(r, "CELLNO").unwrap_or_default().trim().to_string();
        let seqno = fp.cell_str(r, "SEQNO").unwrap_or_default().trim().to_string();
        if wks != "CLP" || !["275", "278", "287", "288"].contains(&cellno.as_str()) {
            continue;
        }
        clp.push(format!(
            "      {{\"row\": {r}, \"cellno\": {}, \"seqno\": {}, \"before\": {}}}",
            jstr(&cellno),
            jstr(&seqno),
            jstr(fp.cell_str(r, "TEXT").unwrap_or_default().as_str())
        ));
    }

    let law = format!(
        r#"{{
  "provenance": "2026-10-28 rebuild by crates/cw-app/examples/freeze_lockdown.rs. `before.ac` is the corpus hub reconstructed from the frozen package's own container streams (tests/golden/pkg/corp2026.ac_, payload stream dropped — CW synthesizes the working hub from exactly these, backup.md §7.2), so the property stream is genuine corpus data. Constants are spec-recorded from attachments/lockdown.md §2 (F68 COM round, 2026-10-23) — the strongest provenance available with the COM fixtures gone. The CW destination tables (sh-after.dbf / fp-after.dbf) are lost; the gate derives its expectation from the corpus plus these constants instead of comparing to CW output, which forbids (rather than tolerates) unrecorded collateral edits.",
  "corpus": "tests/golden/dbf/corp2026",
  "stem": {stem},
  "hub": {{
    "file": "before.ac",
    "bytes": {hub_len},
    "streams": [{carried_json}],
    "summary_stream": {summary_name},
    "summary_len": {summary_len},
    "pids": [
{pids}
    ]
  }},
  "stamp": "20261023082753",
  "stamp_filetime": {STAMP_FILETIME},
  "date_filetime": {DATE_FILETIME},
  "cw_last_change_filetime": {CW_LAST_CHANGE},
  "excel_serial_date": {fp_date_json},
  "excel_serial_check": {{"date": "2026-08-24", "serial": {serial_2026_08_24}}},
  "milestone_snapshots_skipped": {MILESTONE_SNAPSHOTS},
  "ac": {{
    "locked_pid": {PID_LOCK_STATE},
    "stamp_pid": {PID_LOCKDOWN_STAMP},
    "date_pid": {PID_LOCKDOWN_DATE},
    "last_change_pid": {PID_LAST_CHANGE},
    "after": {{
      "273": {{"value": "L"}},
      "267": {{"value": "{STAMP_FILETIME}"}},
      "277": {{"value": "{DATE_FILETIME}"}},
      "321": {{"value": "{STAMP_FILETIME}", "cw_value": "{CW_LAST_CHANGE}", "note": "cosmetic; CW writes the stamp + ~15 s, we write the stamp (spec §5.3)"}}
    }}
  }},
  "sh": {{
    "live": {sh_live},
    "deleted": {sh_del},
    "select_prg_ac_lockdown_y": {selected_len},
    "period": "Y1",
    "guids": [{guids}]
  }},
  "fp": {{
    "live": {fp_live},
    "rows": {fp_rows},
    "touched_rows": {touched},
    "cells": {{
      "275": "L",
      "278": "{FP_DATE_VALUE}",
      "287": "N",
      "288": "{stamp_compact}"
    }},
    "clp_cells": [
{clp_json}
    ]
  }},
  "hi": {{
    "rows": {hi_rows},
    "live": {hi_live},
    "fields": {hi_fields},
    "lock_rows": {lock_rows},
    "values": "<values><value name=\"checksumversion\" type=\"integer\">4097</value></values>\r\n"
  }}
}}
"#,
        stem = jstr(STEM),
        hub_len = hub_bytes.len(),
        carried_json = carried
            .iter()
            .map(|(n, l)| format!("{{\"name\": {}, \"len\": {l}}}", jstr(n)))
            .collect::<Vec<_>>()
            .join(", "),
        summary_name = jstr(SUMMARY_STREAM),
        summary_len = summary.len(),
        serial_2026_08_24 = excel_serial(2026, 8, 24),
        fp_date_json = jstr(FP_DATE_VALUE),
        stamp_compact = "20261023082753",
        sh_live = live(&sh),
        sh_del = deleted(&sh),
        selected_len = selected.len(),
        guids = locked_guids.join(", "),
        fp_live = live(&fp),
        fp_rows = fp.count(),
        touched = clp.len(),
        clp_json = clp.join(",\n"),
        hi_rows = hi.count(),
        hi_live = live(&hi),
        hi_fields = hi.fields.len(),
        lock_rows = selected.len() + 1,
    );
    std::fs::write(out.join("law.json"), law).unwrap();
    println!(
        "wrote {}/{{before.ac,law.json}}: hub {} bytes, {} streams, summary {} bytes / {} pids; {} locked docs over {} live SH rows, {} FP cells",
        out.display(),
        hub_bytes.len(),
        carried.len(),
        summary.len(),
        set.pids().len(),
        selected.len(),
        live(&sh),
        clp.len()
    );
}
