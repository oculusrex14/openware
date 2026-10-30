//! P5.G G3 gate: `AN` annotations/tickmarks/references + `TS` issues.
//!
//! Proven against the frozen corpora:
//! - `mwgh2025` AN live 242 (A 159 / R 47 / N 19 / T 17), 2 tickmark defs.
//! - `hull2024` AN live 373 (A 204 / T 73 / N 47 / R 31 / U 18).
//! - `corp2026` AN live 0 (8 tombstoned), TS live 47 (C 43 / N 4).
//!
//! The gate proves schema identity, byte-stable no-op saves of both the DBF
//! and the FPT sidecar, the census laws, and the COM-shaped writer paths
//! (add note/tickmark/issue, resolve/reopen, tombstone) on the real headers.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use cw_app::review::{
    Anchor, AnnotationBook, AnnotationDraft, AnnotationType, IssueBook, IssueDraft, IssueStatus,
    AN_FIELD_COUNT, AN_RECORD_LEN, TS_FIELD_COUNT, TS_RECORD_LEN,
};
use cw_core::guid::{CwGuid, Family, Rng};

const MWGH: &str = "MWGH LLC 9-30-25 REVIEW(1239263)";
const HULL: &str = "2024 Hull Lift Truck Review(1148441)";
const CORP: &str = "USI Compilation Training(1155962)";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn corpus(name: &str) -> PathBuf {
    repo().join("tests/golden/dbf").join(name)
}

fn pair(dir: &Path, stem: &str, table: &str) -> (Vec<u8>, Vec<u8>) {
    let dbf = std::fs::read(dir.join(format!("{stem}{table}.dbf"))).unwrap();
    let fpt = std::fs::read(dir.join(format!("{stem}{table}.fpt")))
        .or_else(|_| std::fs::read(dir.join(format!("{stem}{table}.FPT"))))
        .unwrap_or_default();
    (dbf, fpt)
}

#[test]
fn an_schema_and_noop_save_are_byte_stable() {
    for (name, stem, live) in [( "mwgh2025", MWGH, 242usize), ("hull2024", HULL, 373)] {
        let dir = corpus(name);
        let (dbf, fpt) = pair(&dir, stem, "AN");
        let book = AnnotationBook::from_bytes(&dbf, &fpt).unwrap();
        assert_eq!(book.table().fields.len(), AN_FIELD_COUNT, "{name} AN columns");
        assert_eq!(book.table().record_len as usize, AN_RECORD_LEN, "{name} AN record len");
        for (got, (w_name, w_kind, w_len, w_dec)) in book
            .table()
            .fields
            .iter()
            .zip(cw_app::review::AN_SCHEMA.iter())
        {
            assert_eq!(got.name, *w_name, "{name} AN field name");
            assert_eq!(got.kind, *w_kind, "{name} AN field kind");
            assert_eq!(got.len, *w_len, "{name} AN field len");
            assert_eq!(got.dec, *w_dec, "{name} AN field dec");
        }
        assert_eq!(book.count(), live, "{name} live AN");
        assert_eq!(book.encode(), dbf, "{name} AN no-op save changed bytes");
        assert_eq!(book.encode_fpt(), fpt, "{name} AN.fpt no-op save changed bytes");
    }
}

#[test]
fn an_census_matches_the_fixture() {
    let mwgh = {
        let (dbf, fpt) = pair(&corpus("mwgh2025"), MWGH, "AN");
        AnnotationBook::from_bytes(&dbf, &fpt).unwrap()
    };
    let c = mwgh.census();
    assert_eq!(c.live, 242);
    assert_eq!(c.by_type.get(&'A'), Some(&159));
    assert_eq!(c.by_type.get(&'R'), Some(&47));
    assert_eq!(c.by_type.get(&'N'), Some(&19));
    assert_eq!(c.by_type.get(&'T'), Some(&17));
    assert_eq!(c.by_type.get(&'U'), None);
    assert_eq!(c.document_global, 33, "`**` document/global anchors");
    assert_eq!(c.anchored, 209);
    assert_eq!(c.with_text, 83, "live rows with a CONTENT memo");
    assert_eq!(c.tickmark_definitions, 2);

    let usage = mwgh.tickmark_usage();
    assert_eq!(usage[0].count, 16);
    assert_eq!(usage[1].count, 1);
    assert_eq!(mwgh.tickmarks().len(), 17);
    assert_eq!(mwgh.notes().len(), 19);
    assert_eq!(mwgh.references().len(), 47);
    assert!(mwgh.issues().is_empty(), "no live TYPE='I' in the corpora");

    // Every decoded row was structurally valid (TYPE parse in decode).
    for a in mwgh.annotations() {
        if a.kind.argument_is_id() && a.kind != AnnotationType::Annotation {
            assert!(!a.argument.is_empty(), "{:?} row without ARGUMENT", a.kind);
        }
    }

    let hull = {
        let (dbf, fpt) = pair(&corpus("hull2024"), HULL, "AN");
        AnnotationBook::from_bytes(&dbf, &fpt).unwrap()
    };
    let h = hull.census();
    assert_eq!((h.live, h.anchored, h.document_global, h.with_text), (373, 317, 56, 150));
    assert_eq!(h.by_type.get(&'T'), Some(&73));
    assert_eq!(h.by_type.get(&'U'), Some(&18));
}

#[test]
fn an_writer_adds_edits_and_tombstones() {
    let (dbf, fpt) = pair(&corpus("mwgh2025"), MWGH, "AN");
    let mut book = AnnotationBook::from_bytes(&dbf, &fpt).unwrap();
    let before = book.count();
    let mut rng = Rng::new_seeded(0xA17);
    let stamp = "20261023180000";

    let note = AnnotationDraft::note("CASH: agree to confirmation", "e109586")
        .anchored(&Anchor::account("AMehBHB1k2ABwDOW0IBZO0"))
        .on_document("JBnZOVNk070I4mS0n7T9xQ")
        .on_field("CUR_YTD")
        .with_content("CASH: agree to confirmation");
    let idx = book.add(note, &mut rng, stamp).unwrap();
    assert_eq!(book.count(), before + 1);
    let a = &book.annotations()[idx];
    assert_eq!(a.kind, AnnotationType::Note);
    assert_eq!(a.argument, "");
    assert_eq!(a.object_type, "AM");
    assert_eq!(a.object_id, "AMehBHB1k2ABwDOW0IBZO0");
    assert_eq!(a.document_id, "JBnZOVNk070I4mS0n7T9xQ");
    assert_eq!(a.field, "CUR_YTD");
    assert_eq!(a.content, "CASH: agree to confirmation");
    assert_eq!(a.user, "e109586");
    assert_eq!(a.last_user, "e109586");
    assert!(!a.before);
    assert_eq!(a.cw_date, stamp);
    assert_eq!(a.an_date, stamp);
    assert_eq!(a.cwguid.len(), 22);
    assert!(CwGuid::parse(&a.cwguid).unwrap().matches(Family::B));
    let note_guid = a.cwguid.clone();

    // Tickmark / reference / issue links require an ARGUMENT id.
    let tick = AnnotationDraft::tickmark("OKkh23BMUK3L9ZN7scaRuS", "e109586")
        .anchored(&Anchor::mapping("MP0000000000000000000A"));
    let ti = book.add(tick, &mut rng, stamp).unwrap();
    assert_eq!(book.annotations()[ti].kind, AnnotationType::Tickmark);
    let bad = AnnotationDraft::reference("", "e109586");
    assert!(book.add(bad, &mut rng, stamp).is_err());

    // Reload through the encoded images (DBF + appended FPT blocks).
    let out_dbf = book.encode();
    let out_fpt = book.encode_fpt();
    let mut reloaded = AnnotationBook::from_bytes(&out_dbf, &out_fpt).unwrap();
    assert_eq!(reloaded.count(), before + 2);
    let again = reloaded
        .annotations()
        .iter()
        .find(|x| x.cwguid == note_guid)
        .expect("added note survives reload");
    assert_eq!(again.content, "CASH: agree to confirmation");
    assert_eq!(again.an_date, stamp);

    // Edit path appends a new memo block and bumps LASTUSER/CWDATE.
    let pos = reloaded.index_of(&note_guid).unwrap();
    reloaded
        .set_content(pos, "CASH: agreed, no exception", "e109587", "20261023180500")
        .unwrap();
    let edited = reloaded
        .annotations()
        .iter()
        .find(|x| x.cwguid == note_guid)
        .unwrap();
    assert_eq!(edited.content, "CASH: agreed, no exception");
    assert_eq!(edited.last_user, "e109587");
    assert_eq!(edited.cw_date, "20261023180500");

    // Remove = tombstone; the live view shrinks, the physical row stays.
    reloaded.remove_at(pos).unwrap();
    let round = AnnotationBook::from_bytes(&reloaded.encode(), &reloaded.encode_fpt()).unwrap();
    assert_eq!(round.count(), before + 1);
    assert_eq!(round.table().count(), book.table().count());
    assert!(round.table().is_deleted(book.table().count() - 2));
}

#[test]
fn ts_schema_and_corpus_laws() {
    let (dbf, fpt) = pair(&corpus("corp2026"), CORP, "TS");
    let book = IssueBook::from_bytes(&dbf, &fpt).unwrap();
    assert_eq!(book.table().fields.len(), TS_FIELD_COUNT);
    assert_eq!(book.table().record_len as usize, TS_RECORD_LEN);
    for (got, (w_name, w_kind, w_len, w_dec)) in book
        .table()
        .fields
        .iter()
        .zip(cw_app::review::TS_SCHEMA.iter())
    {
        assert_eq!(got.name, *w_name, "TS field name");
        assert_eq!(got.kind, *w_kind, "TS field kind");
        assert_eq!(got.len, *w_len, "TS field len");
        assert_eq!(got.dec, *w_dec, "TS field dec");
    }
    assert_eq!(book.count(), 47);
    assert_eq!(book.encode(), dbf, "TS no-op save changed bytes");
    assert_eq!(book.encode_fpt(), fpt, "TS.fpt no-op save changed bytes");

    let c = book.census();
    assert_eq!((c.live, c.deleted), (47, 3));
    assert_eq!(c.by_status.get("C"), Some(&43));
    assert_eq!(c.by_status.get("N"), Some(&4));
    assert_eq!((c.open, c.complete), (4, 43));
    assert_eq!(c.by_priority.get("1"), Some(&47));
    assert_eq!(c.documents, 27, "distinct live DOCID values (45/47 non-blank rows)");

    let mut ids = BTreeSet::new();
    for i in book.issues() {
        assert!(ids.insert(i.id.clone()), "duplicate TS id {}", i.id);
        assert_eq!(i.status_kind().unwrap(), if i.is_open() { IssueStatus::New } else { IssueStatus::Complete });
        if i.is_complete() {
            assert_eq!(i.percent, Some(100), "{} percent", i.id);
            assert!(!i.complete_on.is_empty(), "{} complete date", i.id);
            assert!(!i.complete_by.is_empty(), "{} complete by", i.id);
        } else {
            assert_eq!(i.percent, Some(0), "{} percent", i.id);
        }
        if !i.doc_id.is_empty() {
            assert_eq!(i.doc_id.len(), 22, "{} DOCID", i.id);
        }
    }
    let blank_subject: Vec<&str> = book
        .issues()
        .iter()
        .filter(|i| i.subject.is_empty())
        .map(|i| i.id.as_str())
        .collect();
    assert_eq!(blank_subject, vec!["VF-21"], "one live row has a blank SUBJECT");
    assert_eq!(book.open_issues().len(), 4);
    assert_eq!(book.by_id("VF-2").unwrap().subject, "CASH:");
}

#[test]
fn ts_writer_lifecycle_and_issue_links() {
    let (dbf, fpt) = pair(&corpus("corp2026"), CORP, "TS");
    let mut issues = IssueBook::from_bytes(&dbf, &fpt).unwrap();
    let before = issues.count();
    let suggested = issues.suggest_id();
    assert!(!suggested.is_empty());
    assert!(!issues.issues().iter().any(|i| i.id == suggested));

    let mut rng = Rng::new_seeded(0x75);
    let stamp = "20261023180000";
    let draft = IssueDraft::new("TICKMARK: unexplained variance", "e109586")
        .described("Balance moved 12% QoQ; obtain support.")
        .assigned_to("e109585")
        .on_document("KlwYd8DoUqxN4Rm680b7WZ")
        .anchored(&Anchor::account("AMehBHB1k2ABwDOW0IBZO0"));
    let idx = issues.add(draft, &mut rng, stamp).unwrap();
    let issue = &issues.issues()[idx];
    assert_eq!(issue.id, suggested);
    assert_eq!(issue.status, "N");
    assert_eq!(issue.percent, Some(0));
    assert_eq!(issue.description, "Balance moved 12% QoQ; obtain support.");
    assert_eq!(issue.assigned_to, "e109585");
    assert_eq!(issue.priority, "1");
    assert!(issue.is_open());
    assert!(!issue.roll && !issue.archive && !issue.cleared);
    assert_eq!(issue.cw_date_d, stamp);
    assert_eq!(issue.cw_date_r, stamp);
    assert!(CwGuid::parse(&issue.cwguid).unwrap().matches(Family::B));
    let issue_guid = issue.cwguid.clone();

    // Resolve -> complete (percent 100, COMPLETEON/COMPLETEBY).
    issues
        .resolve(idx, "Agreed to lead sheet 1200.", "e109586", "20261023181500")
        .unwrap();
    let done = &issues.issues()[idx];
    assert!(done.is_complete());
    assert_eq!(done.percent, Some(100));
    assert_eq!(done.resolution, "Agreed to lead sheet 1200.");
    assert_eq!(done.complete_on, "20261023181500");
    assert_eq!(done.complete_by, "e109586");
    assert_eq!(done.modified_on, "20261023181500");

    // Reopen -> N, percent 0, completion stamps cleared, resolution kept.
    issues.reopen(idx, "e109586", "20261023182000").unwrap();
    let reopened = &issues.issues()[idx];
    assert!(reopened.is_open());
    assert_eq!(reopened.percent, Some(0));
    assert!(reopened.complete_on.is_empty());
    assert!(reopened.complete_by.is_empty());
    assert_eq!(reopened.resolution, "Agreed to lead sheet 1200.");

    // Reload and confirm the writer persisted the row + memo payload.
    let round = IssueBook::from_bytes(&issues.encode(), &issues.encode_fpt()).unwrap();
    assert_eq!(round.count(), before + 1);
    let persisted = round.index_of(&issue_guid).unwrap();
    assert_eq!(
        round.issues()[persisted].description,
        "Balance moved 12% QoQ; obtain support."
    );
    assert_eq!(round.issues()[persisted].resolution, "Agreed to lead sheet 1200.");

    // AN -> TS issue link (`TYPE='I'`, ARGUMENT=TS.CWGUID), with dangler split.
    let (an_dbf, an_fpt) = pair(&corpus("mwgh2025"), MWGH, "AN");
    let mut annotations = AnnotationBook::from_bytes(&an_dbf, &an_fpt).unwrap();
    let link = AnnotationDraft::issue(&issue_guid, "e109586").on_document("JBnZOVNk070I4mS0n7T9xQ");
    annotations.add(link, &mut rng, stamp).unwrap();
    let dangler = AnnotationDraft::issue("ZZZZZZZZZZZZZZZZZZZZZZ", "e109586");
    annotations.add(dangler, &mut rng, stamp).unwrap();
    let (linked, dangling) = round.linked_annotations(&annotations);
    assert_eq!(linked, vec![issue_guid]);
    assert_eq!(dangling, vec!["ZZZZZZZZZZZZZZZZZZZZZZ".to_string()]);

    // Tombstone.
    issues.remove_at(idx).unwrap();
    let final_book = IssueBook::from_bytes(&issues.encode(), &issues.encode_fpt()).unwrap();
    assert_eq!(final_book.count(), before);
    assert_eq!(final_book.table().count(), issues.table().count());
    assert!(final_book.table().is_deleted(issues.table().count() - 1));
}


