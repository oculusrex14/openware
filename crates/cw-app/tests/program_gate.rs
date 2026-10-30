//! P5.G G1 gate: `PR` program checklist + response documents.
//!
//! Corpus facts (frozen `tests\golden\dbf\corp2026`):
//! - `PR` = 33 columns / 297-byte records / 0 rows in corp2026, hull2024 and
//!   mwgh2025 (inventory `dbf-schema.tsv`).
//! - `SH.TYPE='F'` rows are the CaseView program/checklist forms (0400 program,
//!   0272 checklist, 0120/0126/0300/0315.10/0325 questionnaires).
//!
//! Provenance: `PR` is not an F63-damaged table, but it is **empty in every
//! corpus**, so there is nothing to pin beyond its schema. The 33-column /
//! 297-byte layout is a format constant asserted inline from the shipped
//! type-library header; the area census (`tests/golden/area/domain.json`)
//! records the zero population so a corpus that gains rows is a visible fixture
//! change rather than a silent widening.
//!
//! The gate therefore proves: schema identity + byte-stable no-op save on the
//! real table, writer add/decode/tombstone on a synthetic row set built from
//! that same header, and response-document resolution against the real `SH`
//! register. It deliberately does **not** claim a populated CW `PR` fixture
//! (none exists; findings record the gap).

mod support;

use std::path::{Path, PathBuf};

use cw_app::program::{
    ChecklistItem, ProgramChecklist, ResponseDocs, PR_FIELD_COUNT, PR_RECORD_LEN, PR_SCHEMA,
};
use cw_core::dbf::Table;
use cw_core::guid::{CwGuid, Family, Rng};
use support::{table_fixture, usize_at};

const STEM: &str = "USI Compilation Training(1155962)";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn corpus() -> PathBuf {
    repo().join("tests/golden/dbf/corp2026")
}

fn pr_path() -> PathBuf {
    corpus().join(format!("{STEM}PR.dbf"))
}

fn sh_path() -> PathBuf {
    corpus().join(format!("{STEM}SH.dbf"))
}

fn load() -> (Vec<u8>, ProgramChecklist) {
    let bytes = std::fs::read(pr_path()).unwrap();
    let list = ProgramChecklist::from_bytes(&bytes).unwrap();
    (bytes, list)
}

#[test]
fn pr_schema_frozen_and_noop_save_identical() {
    let (bytes, list) = load();
    assert_eq!(list.table().fields.len(), PR_FIELD_COUNT);
    assert_eq!(list.table().record_len as usize, PR_RECORD_LEN);
    for (got, want) in list.table().fields.iter().zip(PR_SCHEMA.iter()) {
        assert_eq!(got.name, want.name);
        assert_eq!(got.kind, want.kind);
        assert_eq!(got.len, want.len);
        assert_eq!(got.dec, want.dec);
    }
    // Corp2026 PR is empty; a no-op save must still be byte-identical.
    assert_eq!(list.count(), 0);
    assert_eq!(list.encode(), bytes, "PR no-op save changed bytes");

    // The area census records the zero population in all three corpora.
    for tag in ["corp2026", "hull2024", "mwgh2025"] {
        let f = table_fixture(tag, "pr");
        assert_eq!(usize_at(&f, "records"), 0, "{tag} PR records");
        assert_eq!(usize_at(&f, "live"), 0, "{tag} PR live");
        assert_eq!(usize_at(&f, "deleted"), 0, "{tag} PR deleted");
    }
}

#[test]
fn schema_drift_is_rejected() {
    let bytes = std::fs::read(pr_path()).unwrap();
    let mut table = Table::parse(&bytes).unwrap();
    table.fields[4].name = "PROC".to_string();
    let err = ProgramChecklist::from_table(table).unwrap_err().to_string();
    assert!(err.contains("schema drift"), "{err}");
}

#[test]
fn add_item_encodes_and_decodes() {
    let (bytes, mut list) = load();
    let mut rng = Rng::new_seeded(0x0c0ffee);
    let stamp = "20261023180000";

    let i0 = list.add_item("DOCGUID0123456789ABCDE", "Inspect the bank confirmation", &mut rng, stamp)
        .unwrap();
    let i1 = list.add_item("DOCGUID0123456789ABCDE", "Agree the cash lead sheet", &mut rng, stamp)
        .unwrap();
    list.set_response(i0, Some("1"), Some("agreed to confirmation"))
        .unwrap();

    let item = &list.items()[i0];
    assert_eq!(item.no, 1);
    assert_eq!(item.lineno, 1);
    assert_eq!(item.doc_guid, "DOCGUID0123456789ABCDE");
    assert_eq!(item.text, "Inspect the bank confirmation");
    assert_eq!(item.item_type, "1");
    assert!(item.is_answered());
    assert_eq!(item.response_code(), Some(1));
    assert_eq!(item.explanation, "agreed to confirmation");
    assert_eq!(item.cw_date, stamp);
    assert_eq!(item.created_on, stamp);
    assert_eq!(item.cwguid.len(), 22);
    assert!(CwGuid::parse(&item.cwguid).unwrap().matches(Family::A));
    assert_eq!(list.items()[i1].no, 2);

    let out = list.encode();
    assert_ne!(out, bytes, "written PR must differ from the empty table");
    let reloaded = ProgramChecklist::from_bytes(&out).unwrap();
    assert_eq!(reloaded.count(), 2);
    let again = &reloaded.items()[i0];
    assert_eq!(again.text, item.text);
    assert_eq!(again.check, "1");
    assert_eq!(again.explanation, "agreed to confirmation");
    assert_eq!(again.cwguid, item.cwguid);
    assert_eq!(again.no, 1);
    assert_eq!(again.lineno, 1);

    // RemoveAt = physical tombstone; the live view shrinks, the row stays.
    let mut list = reloaded;
    list.remove_at(0).unwrap();
    let out2 = list.encode();
    let reloaded2 = ProgramChecklist::from_bytes(&out2).unwrap();
    assert_eq!(reloaded2.count(), 1);
    assert_eq!(reloaded2.table().count(), 2);
    assert!(reloaded2.table().is_deleted(0));
    assert_eq!(reloaded2.items()[0].text, "Agree the cash lead sheet");
}

#[test]
fn response_documents_resolve_from_sh() {
    let sh_bytes = std::fs::read(sh_path()).unwrap();
    let sh = Table::parse(&sh_bytes).unwrap();
    let docs = ResponseDocs::from_sh(&sh).unwrap();

    let program = docs
        .documents()
        .iter()
        .find(|d| d.schedule_key() == "0400")
        .expect("0400 program form");
    assert_eq!(program.title, "Program for Review Engagements");
    assert_eq!(program.cv_id, "04000RG2");
    assert_eq!(program.doc_type, "F");
    assert!(!program.guid.is_empty());

    let checklist = docs.documents().iter().find(|d| d.schedule_key() == "0272");
    assert!(checklist.is_some(), "0272 disclosure checklist form");
    assert_eq!(docs.documents().len(), 7, "live SH TYPE='F' forms in corp2026");

    // DOCGUID wins.
    let by_guid = ChecklistItem {
        doc_guid: program.guid.clone(),
        ..blank_item()
    };
    assert_eq!(docs.resolve(&by_guid).unwrap().guid, program.guid);

    // Otherwise WP_REF / AUTH_REF schedule token resolves.
    let by_ref = ChecklistItem {
        wp_ref: "0400 (M1)".into(),
        ..blank_item()
    };
    assert_eq!(docs.resolve(&by_ref).unwrap().guid, program.guid);
    let by_auth = ChecklistItem {
        auth_ref: "0272".into(),
        ..blank_item()
    };
    assert_eq!(docs.resolve(&by_auth).unwrap().schedule_key(), "0272");

    let unknown = ChecklistItem {
        doc_guid: "ZZZZZZZZZZZZZZZZZZZZZZ".into(),
        ..blank_item()
    };
    assert!(docs.resolve(&unknown).is_none());
}

#[test]
fn response_views_track_progress() {
    let (_, mut list) = load();
    let sh = Table::parse(&std::fs::read(sh_path()).unwrap()).unwrap();
    let docs = ResponseDocs::from_sh(&sh).unwrap();
    let program = docs
        .documents()
        .iter()
        .find(|d| d.schedule_key() == "0400")
        .unwrap();
    let checklist = docs
        .documents()
        .iter()
        .find(|d| d.schedule_key() == "0272")
        .unwrap();

    let mut rng = Rng::new_seeded(7);
    let stamp = "20261023180000";
    list.add_item(&program.guid, "Step one", &mut rng, stamp).unwrap();
    list.add_item(&program.guid, "Step two", &mut rng, stamp).unwrap();
    list.add_item(&checklist.guid, "Disclosure present", &mut rng, stamp)
        .unwrap();
    list.set_response(0, Some("1"), None).unwrap();

    let views = list.response_views(&docs);
    assert_eq!(views.len(), 2);
    // Sorted by schedule: 0272 before 0400.
    assert_eq!(views[0].document.as_ref().unwrap().schedule_key(), "0272");
    assert_eq!((views[0].total, views[0].answered), (1, 0));
    assert_eq!(views[1].document.as_ref().unwrap().schedule_key(), "0400");
    assert_eq!((views[1].total, views[1].answered), (2, 1));
    assert_eq!(list.items_for(&program.guid).len(), 2);
}

fn blank_item() -> ChecklistItem {
    ChecklistItem {
        lineno: 0,
        doc_guid: String::new(),
        no: 0,
        item_type: String::new(),
        text: String::new(),
        done_by: String::new(),
        date: String::new(),
        auth_program: String::new(),
        auth_ref: String::new(),
        wp_ref: String::new(),
        check: String::new(),
        explanation: String::new(),
        memo_no: String::new(),
        assertion_initials: std::array::from_fn(|_| String::new()),
        staff: String::new(),
        budget: None,
        actual: None,
        prior: None,
        score: None,
        milestones: std::array::from_fn(|_| String::new()),
        memo: String::new(),
        cwguid: String::new(),
        cw_date: String::new(),
        created_on: String::new(),
        deleted: false,
    }
}

