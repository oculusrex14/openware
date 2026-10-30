//! P5.D4a gate: the `HI` history/audit reader + writer vs the frozen corpus.
//!
//! Golden: `tests/golden/history/law.json` (built by
//! `examples/freeze_history.rs`).
//!
//! ## Provenance (2026-10-28 rebuild)
//!
//! The pre-rebuild fixture was `tests\golden\lifecycle\history-corp2026.json`,
//! produced by the D4a tooling from the approved copy `a sample copy`, and the
//! gate read the corpus from `sandbox/comwork2`. Neither exists in this rebuild.
//! Both are replaced by `tests/golden/dbf/corp2026` — the C1 verbatim freeze of
//! the shipped package for the same engagement — and the fixture is regenerated
//! from it.
//!
//! ## Readable-prefix law
//!
//! `corp2026/HI` is one of the twelve tables whose `.dbf` member is a truncated
//! LZMA stream (F63). It declares 344,192 bytes / 1,710 records; the reference
//! decoder returns 304,190 bytes, so rows 1,510..1,710 are zero-fill synthesis
//! with shifted text (`guid_law_gate.rs` pins the same frontier as `DAMAGED`).
//! Everything asserted below about row *values* is therefore stated over the
//! contiguous readable prefix `rows[..1510]`; the frontier is a frozen constant
//! in the fixture, so a newly damaged table cannot silently shrink the law, and
//! the geometry / schema / no-op-save assertions still cover the whole file.
//!
//! What this costs against the original fixture: the shipped package predates the
//! review's signoff round, so the corpus carries 7 `DOCSIGN1` rows rather than the
//! probe's 57 — the F47 "signoff rows are SH-anchored and checksum-bearing" law is
//! unchanged and is still asserted on every one of them.

use std::collections::BTreeMap;
use std::path::PathBuf;

use cw_app::history::{
    kind_cells, values_xml, HistoryEvent, HistoryKind, HistoryLog, CHECKSUM_VERSION, SUFFIX,
};
use cw_core::canon::records_sha;
use cw_core::dbf::Table;
use cw_core::fpt::Fpt;
use cw_core::guid::{CwGuid, Family, Rng};
use cw_core::json::Json;
use cw_core::sha256::sha256_hex;

const STEM: &str = "USI Compilation Training(1155962)";

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().to_path_buf()
}

fn gold() -> PathBuf {
    repo().join("tests").join("golden").join("history")
}

/// The frozen corpus (C1) — was `sandbox/comwork2` before the rebuild.
fn corpus() -> PathBuf {
    repo().join("tests").join("golden").join("dbf").join("corp2026")
}

fn law() -> Json {
    let p = gold().join("law.json");
    cw_core::json::parse(&std::fs::read_to_string(&p).unwrap_or_else(|e| {
        panic!("{}: {e} (run `cargo run -p cw-app --example freeze_history`)", p.display())
    }))
    .unwrap()
}

fn s<'a>(j: &'a Json, key: &str) -> &'a str {
    j.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

fn n(j: &Json, key: &str) -> usize {
    j.get(key).and_then(|v| v.as_usize()).unwrap_or(usize::MAX)
}

fn census_map(j: &Json, key: &str) -> BTreeMap<String, usize> {
    j.get(key)
        .and_then(|v| v.as_obj())
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_usize().unwrap()))
        .collect()
}

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("ocw-d4a-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// The readable prefix of the corpus table (`law.json` `frontier`), as bytes
/// the writer round-trips. Value-level assertions run on this slice only.
fn prefix() -> HistoryLog {
    let law = law();
    let frontier = n(&law, "frontier");
    let dbf = std::fs::read(corpus().join(format!("{STEM}HI.dbf"))).unwrap();
    let fpt = std::fs::read(corpus().join(format!("{STEM}HI.fpt"))).unwrap();
    let mut table = Table::parse(&dbf).unwrap();
    assert_eq!(table.count(), n(&law, "records"), "declared record count");
    table.records.truncate(frontier);
    HistoryLog::from_bytes(&table.encode(), &fpt).unwrap()
}

#[test]
fn corpus_rows_decode_to_the_golden_law() {
    let law = law();
    let full = HistoryLog::open(corpus(), STEM).unwrap();
    // Geometry and schema cover the whole file, fabricated tail included.
    assert_eq!(full.table.count(), n(&law, "records"));
    assert_eq!(full.table.header_len as usize, n(&law, "header_len"));
    assert_eq!(full.table.record_len as usize, n(&law, "record_len"));

    let fields = law.get("fields").and_then(|v| v.as_arr()).unwrap();
    assert_eq!(full.table.fields.len(), fields.len());
    for (f, j) in full.table.fields.iter().zip(fields) {
        assert_eq!(f.name, s(j, "name"));
        assert_eq!(f.kind, s(j, "type").bytes().next().unwrap());
        assert_eq!(f.len as usize, n(j, "len"));
        assert_eq!(f.dec as usize, n(j, "dec"));
    }

    // Whole-file digests: the fixture records the shipped bytes verbatim.
    let dbf = std::fs::read(corpus().join(format!("{STEM}HI.dbf"))).unwrap();
    let fpt = std::fs::read(corpus().join(format!("{STEM}HI.fpt"))).unwrap();
    assert_eq!(sha256_hex(&dbf), s(&law, "dbf_sha256"));
    assert_eq!(sha256_hex(&fpt), s(&law, "fpt_sha256"));

    // Value-level law: the readable prefix only (see the module header).
    let pre = prefix();
    let prefix = law.get("prefix").unwrap();
    assert_eq!(pre.table.count(), n(prefix, "records"), "frontier rows");
    assert_eq!(records_sha(&pre.table, Some(&pre.fpt)), s(prefix, "records_sha"));

    let census = pre.census().unwrap();
    assert_eq!(census.records, n(prefix, "records"));
    assert_eq!(census.live, n(prefix, "live"));
    assert_eq!(census.deleted, n(prefix, "deleted"));
    assert_eq!(census.users, n(prefix, "users"));
    assert_eq!(census.categories, census_map(prefix, "categories"));
    assert_eq!(census.types, census_map(prefix, "types"));
    assert_eq!(census.object_types, census_map(prefix, "object_types"));
    assert_eq!(census.first_date, s(prefix, "first_date"));
    assert_eq!(census.last_date, s(prefix, "last_date"));

    // F47: every role-1 signoff row is SH-anchored and carries a checksum.
    let signoffs: Vec<_> = pre
        .all()
        .unwrap()
        .into_iter()
        .filter(|e| e.kind() == HistoryKind::Signoff(1))
        .collect();
    assert_eq!(signoffs.len(), census.types.get("DOCSIGN1").copied().unwrap_or(0));
    assert!(signoffs.len() >= 7, "the shipped package carries a signoff round");
    for e in &signoffs {
        assert_eq!(e.object_type, "SH");
        assert_eq!(e.category, "DOCS");
        assert!(e.is_document_event());
    }

    // The pinned frontier is a fact about fixed bytes, not a search result.
    let boundary = law.get("boundary").unwrap();
    let raw = Table::parse(&dbf).unwrap();
    assert_eq!(raw.records[n(&law, "frontier") - 1][0], n(boundary, "last_readable_flag") as u8);
    assert_eq!(raw.records[n(&law, "frontier")][0], n(boundary, "first_unreadable_flag") as u8);
}

#[test]
fn named_rows_and_operation_classes() {
    let law = law();
    let log = prefix();
    let samples = law.get("samples").unwrap();

    for (key, want_kind) in [
        ("first_user", HistoryKind::UserAccess),
        ("second_user", HistoryKind::UserAccess),
        ("first_property", HistoryKind::DocumentProperty),
        ("first_delete", HistoryKind::DocumentDelete),
        ("first_signoff", HistoryKind::Signoff(1)),
        ("last", HistoryKind::DocumentAccess),
    ] {
        let sample = samples.get(key).unwrap();
        let idx = n(sample, "index");
        let ev = log.event(idx).unwrap();
        for field in [
            "category",
            "type",
            "object_id",
            "object_type",
            "user",
            "checksum",
            "start_date",
            "end_date",
            "cwguid",
            "cw_date",
            "created_on",
            "values",
        ] {
            let got = match field {
                "category" => &ev.category,
                "type" => &ev.event_type,
                "object_id" => &ev.object_id,
                "object_type" => &ev.object_type,
                "user" => &ev.user,
                "checksum" => &ev.checksum,
                "start_date" => &ev.start_date,
                "end_date" => &ev.end_date,
                "cwguid" => &ev.cwguid,
                "cw_date" => &ev.cw_date,
                "created_on" => &ev.created_on,
                _ => &ev.values,
            };
            assert_eq!(got, s(sample, field), "{key}.{field} @ {idx}");
        }
        assert!(!ev.deleted, "{key}");
        assert_eq!(ev.kind(), want_kind, "{key}");
    }

    // Delete rows carry the deleted display name in VALUES (no OBJECTID).
    let del_idx = n(samples.get("first_delete").unwrap(), "index");
    let del = log.event(del_idx).unwrap();
    assert_eq!(del.value("deletedid").as_deref(), Some("1000 (M3)"));
    assert_eq!(del.value("deletedname").as_deref(), Some("Cash Workpaper & Memo"));
    assert!(del.object_id.is_empty());
    assert!(del.object_type.is_empty());

    // Signoff rows are anchored SH documents with a checksum target (F47).
    let sign_idx = n(samples.get("first_signoff").unwrap(), "index");
    let sign = log.event(sign_idx).unwrap();
    assert_eq!(sign.object_type, "SH");
    assert_eq!(sign.object_id, "D3p38Cv2EXWOCG4qIElZ2t");
    assert_eq!(sign.checksum, "C2ynOVTBRIDE2+ax+uu1mw");
    assert_eq!(sign.username().as_deref(), Some("Nithiya"));
    assert_eq!(sign.user_initials().as_deref(), Some("N"));
    assert_eq!(sign.value("version").as_deref(), Some("8.11"));
    assert!(sign.is_document_event());
    assert!(CwGuid::parse(&sign.cwguid).unwrap().matches(Family::B));

    // Legacy factory row: 8-char CWGUID is kept verbatim (F59).
    let first = log.event(0).unwrap();
    assert_eq!(first.cwguid, "+jFTqvGp");
    assert_eq!(first.event_type, "FACCUSR");
    assert_eq!(first.checksum_version(), Some(4097));
}

#[test]
fn noop_save_is_byte_identical() {
    // Whole file: the writer must re-emit the shipped bytes exactly, fabricated
    // tail included (the tail is opaque record bytes to the writer).
    let log = HistoryLog::open(corpus(), STEM).unwrap();
    let (dbf, fpt) = log.encode();
    assert_eq!(dbf, std::fs::read(corpus().join(format!("{STEM}HI.dbf"))).unwrap());
    assert_eq!(fpt, std::fs::read(corpus().join(format!("{STEM}HI.fpt"))).unwrap());

    let again = HistoryLog::from_bytes(&dbf, &fpt).unwrap();
    assert_eq!(again.encode(), (dbf, fpt));
    assert_eq!(again.all().unwrap(), log.all().unwrap());
}

#[test]
fn append_signoff_and_delete_roundtrip() {
    // Start from the readable prefix: appending behind a fabricated tail would
    // compare CW-shaped rows against synthetic ones.
    let src = corpus();
    let before = std::fs::read(src.join(format!("{STEM}HI.dbf"))).unwrap();
    let mut log = prefix();
    let base = log.table.count();
    assert_eq!(base, n(&law(), "frontier"));
    let mut rng = Rng::new_seeded(0xD4A_2026);

    // 1) Signoff role 4 put: CW-shaped DOCS/DOCSIGN4 row, SH anchored.
    let idx = log
        .record_document_event(
            HistoryKind::Signoff(4),
            "AbCdEfGhIjKlMnOpQrStUv",
            "E109586",
            "   OC",
            &[("version", "9.99")],
            "ZzYyXxWwVvUuTtSsRrQqPp",
            "20261023120000",
            &mut rng,
        )
        .unwrap();
    assert_eq!(idx, base);
    let ev = log.event(idx).unwrap();
    assert_eq!(ev.category, "DOCS");
    assert_eq!(ev.event_type, "DOCSIGN4");
    assert_eq!(ev.kind(), HistoryKind::Signoff(4));
    assert_eq!(ev.object_type, "SH");
    assert_eq!(ev.object_id, "AbCdEfGhIjKlMnOpQrStUv");
    assert_eq!(ev.checksum, "ZzYyXxWwVvUuTtSsRrQqPp");
    assert_eq!(ev.user, "E109586");
    assert_eq!(ev.username().as_deref(), Some("E109586"));
    assert_eq!(ev.user_initials().as_deref(), Some("   OC"));
    assert_eq!(ev.value("version").as_deref(), Some("9.99"));
    assert_eq!(ev.checksum_version(), Some(CHECKSUM_VERSION));
    assert_eq!(ev.cwguid.len(), 22);
    assert!(CwGuid::parse(&ev.cwguid).unwrap().matches(Family::B));
    assert_eq!(ev.start_date, "20261023120000");
    assert_eq!(ev.end_date, "20261023120000");
    assert_eq!(ev.created_on, "20261023120000");
    assert!(ev.is_document_event());

    // 2) Delete with a >512-byte deleted name => multi-block memo chain.
    let long_name = format!("Long {}", "x".repeat(700));
    let idx2 = log
        .record_document_event(
            HistoryKind::DocumentDelete,
            "",
            "E109586",
            "OC",
            &[("deletedid", "9999 (M1)"), ("deletedname", &long_name)],
            "",
            "20261023120100",
            &mut rng,
        )
        .unwrap();
    let del = log.event(idx2).unwrap();
    assert_eq!(del.kind(), HistoryKind::DocumentDelete);
    assert_eq!(del.value("deletedname").as_deref(), Some(long_name.as_str()));
    assert!(del.object_id.is_empty());
    assert!(del.object_type.is_empty());

    // Encode -> parse -> identical event stream, and an identical second save.
    let (dbf, fpt) = log.encode();
    let reopened = HistoryLog::from_bytes(&dbf, &fpt).unwrap();
    assert_eq!(reopened.all().unwrap(), log.all().unwrap());
    assert_eq!(reopened.encode(), (dbf.clone(), fpt.clone()));

    // Filesystem round-trip under a fresh stem.
    let dir = tmp("write");
    log.save(&dir, "D4A").unwrap();
    let on_disk = HistoryLog::open(&dir, "D4A").unwrap();
    assert_eq!(on_disk.all().unwrap(), log.all().unwrap());
    let again = on_disk.encode();
    on_disk.save(&dir, "D4A").unwrap();
    assert_eq!(again, HistoryLog::open(&dir, "D4A").unwrap().encode());
    let _ = std::fs::remove_dir_all(&dir);

    // Source engagement untouched.
    assert_eq!(before, std::fs::read(src.join(format!("{STEM}HI.dbf"))).unwrap());
}

#[test]
fn writer_shape_matches_corpus_style() {
    let xml = values_xml(
        &[
            ("username".to_string(), "Jane Lei".to_string()),
            ("userinitials".to_string(), "   JL242".to_string()),
        ],
        CHECKSUM_VERSION,
    );
    assert_eq!(
        xml,
        "<values><value name=\"username\">Jane Lei</value>\
         <value name=\"userinitials\" xml:space=\"preserve\">   JL242</value>\
         <value name=\"checksumversion\" type=\"integer\">4097</value></values>\r\n"
    );
    assert_eq!(
        kind_cells(&HistoryKind::DocumentAccess).unwrap(),
        ("DOCA".into(), "DOCAUSR".into())
    );
    assert_eq!(SUFFIX, "HI");
    assert_eq!(
        Table::parse(&std::fs::read(corpus().join(format!("{STEM}HI.dbf"))).unwrap())
            .unwrap()
            .field_index("CWGUID"),
        Some(11)
    );
    assert_eq!(
        Fpt::parse(&std::fs::read(corpus().join(format!("{STEM}HI.fpt"))).unwrap())
            .unwrap()
            .block_size,
        512
    );
    let _ = HistoryEvent::at("20260101000000");
}
