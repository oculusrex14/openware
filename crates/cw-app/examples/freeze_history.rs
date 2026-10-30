//! Freeze `tests/golden/history/law.json` — the P5.D4a `HI` audit-trail law.
//!
//! ## Provenance (2026-10-28 rebuild)
//!
//! The original fixture (`tests\golden\lifecycle\history-corp2026.json`) was
//! produced by the D4a tooling from the approved copy `a sample copy`, a snapshot
//! taken *before* the engagement was locked down (its signoff census is 57/1 and
//! it carries a different row count). Neither that copy nor the `sandbox/`
//! working directory exists in this rebuild, and the gate's old corpus path
//! (`sandbox/comwork2`) is gone.
//!
//! The fixture is now **kernel-derived** from `tests/golden/dbf/corp2026` — the
//! C1 verbatim freeze of the shipped package, which is the same engagement at a
//! later state. Two consequences, both encoded below rather than hidden:
//!
//! 1. **Readable-prefix law** (same amendment as `tests/guid_law_gate.rs`): the
//!    `HI` `.dbf` member is a truncated LZMA stream. `corp2026/HI` declares
//!    344,192 bytes; the reference decoder returns 304,190, i.e. 1,510 of the
//!    1,710 records (`DAMAGED` in `guid_law_gate.rs`). Row 1,510 onward is
//!    zero-fill synthesis with shifted text, so every *value-level* assertion is
//!    made over `rows[..1510]` and the frontier is pinned as a constant here —
//!    not searched — so newly damaged bytes fail loudly instead of silently
//!    shrinking the asserted set.
//! 2. **Field-level laws that survive re-grounding.** The schema, geometry and
//!    no-op-save byte identity hold on the whole file; the operation census
//!    (category/type/object-type histograms, date span, named sample rows) is
//!    asserted on the prefix. The signoff census is 7 × `DOCSIGN1` (not the
//!    probe's 57) because the shipped package predates the review's signoff round.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cw_app::history::{HistoryKind, HistoryLog};
use cw_core::dbf::Table;
use cw_core::sha256::sha256_hex;

const STEM: &str = "USI Compilation Training(1155962)";

/// First row of the fabricated tail (`guid_law_gate::DAMAGED` corp2026/HI).
const FRONTIER: usize = 1510;

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

fn main() {
    let corpus = root().join("tests/golden/dbf/corp2026");
    let out = root().join("tests/golden/history");
    std::fs::create_dir_all(&out).unwrap();

    let dbf = std::fs::read(corpus.join(format!("{STEM}HI.dbf"))).unwrap();
    let fpt = std::fs::read(corpus.join(format!("{STEM}HI.fpt"))).unwrap();
    let full = HistoryLog::from_bytes(&dbf, &fpt).unwrap();

    // Prefix slice: the readable rows only, re-encoded through the same writer.
    let mut prefix_table = Table::parse(&dbf).unwrap();
    let declared = prefix_table.count();
    assert!(FRONTIER < declared, "frontier must cut the fabricated tail");
    prefix_table.records.truncate(FRONTIER);
    let prefix_dbf = prefix_table.encode();
    let prefix = HistoryLog::from_bytes(&prefix_dbf, &fpt).unwrap();
    let census = prefix.census().unwrap();

    // Fields
    let mut fields = String::new();
    for (i, f) in prefix.table.fields.iter().enumerate() {
        if i > 0 {
            fields.push_str(",\n");
        }
        fields.push_str(&format!(
            "    {{\"name\": {}, \"type\": {}, \"len\": {}, \"dec\": {}}}",
            jstr(&f.name),
            jstr(&(f.kind as char).to_string()),
            f.len,
            f.dec
        ));
    }

    // Named sample rows (the six the gate checks) + the last prefix row.
    let mut first_user = None;
    let mut second_user = None;
    let mut first_property = None;
    let mut first_delete = None;
    let mut first_signoff = None;
    for i in 0..prefix.table.count() {
        let ev = prefix.event(i).unwrap();
        match ev.kind() {
            HistoryKind::UserAccess if first_user.is_none() => first_user = Some(i),
            HistoryKind::UserAccess if second_user.is_none() => second_user = Some(i),
            HistoryKind::DocumentProperty if first_property.is_none() => first_property = Some(i),
            HistoryKind::DocumentDelete if first_delete.is_none() => first_delete = Some(i),
            HistoryKind::Signoff(1) if first_signoff.is_none() => first_signoff = Some(i),
            _ => {}
        }
    }
    let samples: [(&str, usize); 6] = [
        ("first_user", first_user.expect("a sign-in row")),
        ("second_user", second_user.expect("a second sign-in row")),
        ("first_property", first_property.expect("a document-property row")),
        ("first_delete", first_delete.expect("a delete row")),
        ("first_signoff", first_signoff.expect("a role-1 signoff row")),
        ("last", prefix.table.count() - 1),
    ];
    let mut sample_json = String::new();
    for (i, (key, idx)) in samples.iter().enumerate() {
        let ev = prefix.event(*idx).unwrap();
        if i > 0 {
            sample_json.push_str(",\n");
        }
        sample_json.push_str(&format!(
            "    {k}: {{\"index\": {idx}, \"category\": {c}, \"type\": {t}, \"object_id\": {oi}, \"object_type\": {ot}, \"user\": {u}, \"checksum\": {ck}, \"start_date\": {sd}, \"end_date\": {ed}, \"cwguid\": {g}, \"cw_date\": {cd}, \"created_on\": {co}, \"values\": {v}, \"kind\": {kd}}}",
            k = jstr(key),
            idx = idx,
            c = jstr(&ev.category),
            t = jstr(&ev.event_type),
            oi = jstr(&ev.object_id),
            ot = jstr(&ev.object_type),
            u = jstr(&ev.user),
            ck = jstr(&ev.checksum),
            sd = jstr(&ev.start_date),
            ed = jstr(&ev.end_date),
            g = jstr(&ev.cwguid),
            cd = jstr(&ev.cw_date),
            co = jstr(&ev.created_on),
            v = jstr(&ev.values),
            kd = jstr(&format!("{:?}", ev.kind())),
        ));
    }

    // Frontier evidence: the flag byte and the first timestamp column at the
    // boundary, so a moved boundary is visible in the report as well as in the
    // assertion.
    let raw = Table::parse(&dbf).unwrap();
    let last_ok = &raw.records[FRONTIER - 1];
    let first_bad = &raw.records[FRONTIER];
    let created = raw.field_index("CREATEDON").map(|i| raw.offsets()[i]);
    let boundary = format!(
        "{{\"last_readable_row\": {}, \"last_readable_flag\": {}, \"last_readable_createdon\": {}, \"first_unreadable_row\": {}, \"first_unreadable_flag\": {}}}",
        FRONTIER - 1,
        last_ok[0],
        jstr(match created {
            Some(o) => cw_core::canon::latin1(&last_ok[o..o + 14]).trim().to_string(),
            None => String::new(),
        }.as_str()),
        FRONTIER,
        first_bad[0]
    );

    let law = format!(
        r#"{{
  "provenance": "kernel-derived 2026-10-28 by crates/cw-app/examples/freeze_history.rs over tests/golden/dbf/corp2026 (the C1 verbatim freeze), replacing the lost D4a snapshot 'a sample copy' and the missing sandbox/comwork2 working directory. Value-level facts are asserted over the readable prefix (frontier {FRONTIER}, pinned below); geometry, schema and the no-op-save law hold on the whole file. See tests/golden/README.md (F63) and guid_law_gate.rs (DAMAGED).",
  "corpus": "tests/golden/dbf/corp2026",
  "stem": {stem},
  "dbf_sha256": {dbf_sha},
  "fpt_sha256": {fpt_sha},
  "records": {declared},
  "header_len": {header_len},
  "record_len": {record_len},
  "frontier": {FRONTIER},
  "boundary": {boundary},
  "fields": [
{fields}
  ],
  "prefix": {{
    "records": {pcount},
    "records_sha": {psha},
    "live": {live},
    "deleted": {deleted},
    "users": {users},
    "categories": {cats},
    "types": {types},
    "object_types": {otypes},
    "first_date": {first_date},
    "last_date": {last_date}
  }},
  "samples": {{
{sample_json}
  }}
}}
"#,
        stem = jstr(STEM),
        dbf_sha = jstr(&sha256_hex(&dbf)),
        fpt_sha = jstr(&sha256_hex(&fpt)),
        declared = declared,
        header_len = full.table.header_len,
        record_len = full.table.record_len,
        pcount = prefix.table.count(),
        psha = jstr(&cw_core::canon::records_sha(&prefix.table, Some(&prefix.fpt))),
        live = census.live,
        deleted = census.deleted,
        users = census.users,
        cats = usizemap(&census.categories),
        types = usizemap(&census.types),
        otypes = usizemap(&census.object_types),
        first_date = jstr(census.first_date.as_str()),
        last_date = jstr(census.last_date.as_str()),
    );
    std::fs::write(out.join("law.json"), law).unwrap();
    println!(
        "wrote {}/law.json: {} declared rows, prefix {} ({} live), {} types",
        out.display(),
        declared,
        prefix.table.count(),
        census.live,
        census.types.len()
    );
}
