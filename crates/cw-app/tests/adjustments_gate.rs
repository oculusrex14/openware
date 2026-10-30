//! P5.E3a gate: `GL` row codec + adjusting-entry / line / balance model.
//!
//! Golden input: `tests/golden/dbf/{corp2026,hull2024,mwgh2025}/*GL.dbf` and the
//! area census `tests/golden/area/domain.json` (`examples/freeze_area.rs`).
//!
//! Provenance: `GL` is **not** one of the twelve F63-damaged tables (see
//! `tests/support/mod.rs`), so all three journals are authentic end to end and
//! this gate asserts whole tables. The 51-column / 668-byte layout is a format
//! constant from the shipped type-library header, asserted inline; the fixture
//! pins the population census (rows, entries, lines, issue counters).
//!
//! The gate locks:
//!
//! - the physical 51-column / 668-byte layout + drift detection;
//! - byte-identical row decode/encode on every live and deleted record;
//! - the entry law: 1 `Z…Z` header + n line rows per `REFNO`, every entry
//!   balanced, classes decoded from `GL.STATUS` (F34);
//! - the 13-code `CWAdjustingEntryType` ↔ `STATUS` map, including the
//!   unstoreable prior-period class;
//! - canonical AJE row construction (`render`), date window checks and
//!   validation guards (width, type, memo, unknown column);
//! - structural issue detection (missing header, mixed status, unbalanced).

mod support;

use std::path::PathBuf;

use cw_app::adjustments::{
    gl_schema_drift, gl_schema_matches, AdjustmentEntry, AdjustmentIssueKind, AdjustmentLine,
    Adjustments, Balance, EntryType, GlRow, AJE_BUCKET, AJE_JOURNAL_SOURCE, GL_FIELD_COUNT,
    GL_HEADER_ACCOUNT, GL_RECORD_LEN, GL_SCHEMA,
};
use cw_core::dbf::Table;
use support::{table_fixture, usize_at};

fn gl(name: &str, file: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("..");
    p.push("..");
    p.push("tests");
    p.push("golden");
    p.push("dbf");
    p.push(name);
    p.push(file);
    p
}

fn corp() -> PathBuf {
    gl("corp2026", "USI Compilation Training(1155962)GL.dbf")
}

fn hull() -> PathBuf {
    gl("hull2024", "2024 Hull Lift Truck Review(1148441)GL.dbf")
}

fn mwgh() -> PathBuf {
    gl("mwgh2025", "MWGH LLC 9-30-25 REVIEW(1239263)gl.dbf")
}

fn table(path: PathBuf) -> Table {
    Table::parse_path(path).unwrap()
}

/// Live (non-deleted) rows, in file order.
fn live_rows(t: &Table) -> Vec<Vec<u8>> {
    (0..t.count())
        .filter(|r| !t.is_deleted(*r))
        .map(|r| t.records[r].clone())
        .collect()
}

#[test]
fn physical_schema_matches_inventory() {
    for (path, label) in [(corp(), "corp"), (hull(), "hull"), (mwgh(), "mwgh")] {
        let t = table(path);
        assert_eq!(t.fields.len(), GL_FIELD_COUNT, "{label} GL field count");
        assert_eq!(t.record_len as usize, GL_RECORD_LEN, "{label} record len");
        assert_eq!(
            t.fields.iter().map(|f| f.len as usize).sum::<usize>() + 1,
            GL_RECORD_LEN,
            "{label}: widths must sum to the record length"
        );
        assert!(gl_schema_matches(&t), "{label} drift: {:?}", gl_schema_drift(&t));
    }
    assert_eq!(GL_SCHEMA.len(), GL_FIELD_COUNT);
    assert_eq!(GL_SCHEMA[0].name, "AC_NO");
    assert_eq!(GL_SCHEMA[50].name, "MTYPE");

    let mut bad = table(corp());
    bad.fields[17].len = 18; // NET N19.2 -> N18.2
    assert!(!gl_schema_matches(&bad));
    assert!(gl_schema_drift(&bad)
        .iter()
        .any(|d| d.column == "NET" && d.expected == "N19.2" && d.actual == "N18.2"));
    assert!(Adjustments::from_table(&bad).is_err());
}

#[test]
fn row_codec_is_byte_identical() {
    for (path, label) in [(corp(), "corp"), (hull(), "hull"), (mwgh(), "mwgh")] {
        let original = table(path);
        let mut t = original.clone();
        for r in 0..t.count() {
            let row = GlRow::from_table(&t, r).unwrap();
            let bytes = row.encode().unwrap();
            assert_eq!(bytes, t.records[r], "{label} row {r} re-encode");
            assert_eq!(GlRow::parse_record(&bytes).unwrap(), row);
            assert_eq!(row.is_deleted(), t.is_deleted(r));
            row.write_to(&mut t, r).unwrap();
        }
        assert_eq!(t.encode(), original.encode(), "{label} whole-file round-trip");
    }
}

#[test]
fn journal_census_and_balance_law() {
    let cases = [
        (corp(), "corp", "corp2026", 3usize, 17usize, 14usize),
        (hull(), "hull", "hull2024", 13, 70, 57),
        (mwgh(), "mwgh", "mwgh2025", 25, 98, 73),
    ];
    for (path, label, tag, entries, rows, lines) in cases {
        let t = table(path);
        let j = Adjustments::from_table(&t).unwrap();
        assert_eq!(j.len(), entries, "{label} entries");
        assert_eq!(j.line_count(), lines, "{label} lines");
        assert_eq!(
            j.entries().iter().map(|e| e.row_count()).sum::<usize>(),
            rows,
            "{label} physical rows"
        );
        assert!(j.issues().is_empty(), "{label} issues: {:?}", j.issues());
        assert_eq!(j.unbalanced().count(), 0, "{label} unbalanced");

        for e in j.entries() {
            assert!(e.header_row.is_some(), "{}: header", e.system_id);
            assert_eq!(e.row_count(), e.line_count() + 1, "{}: 1+n rows", e.system_id);
            assert!(e.is_balanced(), "{}: {:?}", e.system_id, e.difference());
            assert_eq!(e.gl_rows[0].get("AC_NO").unwrap(), GL_HEADER_ACCOUNT);
            assert!(e.gl_rows[0].is_header());
            for row in e.gl_rows.iter().skip(1) {
                assert!(!row.is_header(), "line row must not be a header");
                assert_eq!(row.get("STATUS").unwrap().trim(), e.entry_type.status().unwrap());
                assert_eq!(row.get("JOURSOURCE").unwrap().trim(), AJE_JOURNAL_SOURCE);
                assert_eq!(row.get("BUCKET").unwrap().trim(), AJE_BUCKET);
            }
            assert_eq!(e.journal_source, AJE_JOURNAL_SOURCE);
            assert_eq!(e.gl_rows.len(), e.row_count());
        }
        assert!(j.balance().is_balanced(), "{label} journal nets to zero");

        // re-emission of the decoded rows equals the live record bytes exactly
        let expected: Vec<u8> = live_rows(&t).concat();
        assert_eq!(j.encode_rows().unwrap().concat(), expected, "{label} re-emit");

        // The area fixture pins this census over whole tables.
        let f = table_fixture(tag, "gl");
        assert_eq!(usize_at(&f, "records"), t.count(), "{label} records");
        assert_eq!(usize_at(&f, "live"), live_rows(&t).len(), "{label} live");
        assert_eq!(usize_at(&f, "entries"), entries, "{label} fixture entries");
        assert_eq!(usize_at(&f, "lines"), lines, "{label} fixture lines");
        assert_eq!(usize_at(&f, "rows"), rows, "{label} fixture rows");
        assert_eq!(usize_at(&f, "issues"), 0, "{label} fixture issues");
        assert_eq!(usize_at(&f, "unbalanced"), 0, "{label} fixture unbalanced");
    }
}

#[test]
fn entry_types_come_from_gl_status() {
    let j = Adjustments::from_table(&table(corp())).unwrap();
    assert_eq!(j.get("2").unwrap().entry_type, EntryType::Reclassifying);
    assert_eq!(j.get("2").unwrap().user_reference, "5003");
    assert_eq!(j.get("AJE #01").unwrap().entry_type, EntryType::Normal);
    assert_eq!(j.get("AJE#02").unwrap().entry_type, EntryType::Normal);
    assert_eq!(j.get("AJE #01").unwrap().line_count(), 9);

    let h = Adjustments::from_table(&table(hull())).unwrap();
    assert_eq!(h.get("FJE0").unwrap().entry_type, EntryType::TaxFederal);
    assert_eq!(h.get("FJE16").unwrap().entry_type, EntryType::Reclassifying);
    assert_eq!(h.get("AJE 04").unwrap().line_count(), 4);

    let m = Adjustments::from_table(&table(mwgh())).unwrap();
    assert_eq!(m.get("TAX 12").unwrap().entry_type, EntryType::TaxFederal);
    assert_eq!(m.get("TAX 12").unwrap().line_count(), 8);
    assert_eq!(m.get("54").unwrap().entry_type, EntryType::Reclassifying);
    assert_eq!(m.get("1").unwrap().entry_type, EntryType::Normal);
    assert_eq!(
        m.type_counts(),
        vec![
            (EntryType::Normal, 3),
            (EntryType::Reclassifying, 6),
            (EntryType::TaxFederal, 16),
        ]
    );
    assert!(m.get_in("AJ", "TAX 12").is_some());
    assert!(m.get_in("ZZ", "TAX 12").is_none());
    assert_eq!(
        m.get_in("AJ", "TAX 12").unwrap().journal_source,
        AJE_JOURNAL_SOURCE
    );
}

#[test]
fn all_thirteen_entry_types_roundtrip() {
    assert_eq!(EntryType::ALL.len(), 13);
    let mut seen = 0;
    for t in EntryType::ALL {
        assert_eq!(EntryType::from_code(t.code()), Some(t));
        assert_eq!(t.code() as usize, seen);
        seen += 1;
        match t.status() {
            Some(status) => {
                let mut row = GlRow::blank();
                row.set("STATUS", status).unwrap();
                assert_eq!(row.entry_type().unwrap(), t, "{:?}", t);
                assert_eq!(EntryType::from_status(status).unwrap(), t);
            }
            None => {
                assert_eq!(t, EntryType::PriorPeriod);
                assert!(!t.has_storage_code());
            }
        }
    }
    assert_eq!(EntryType::Normal.status(), Some("N"));
    assert_eq!(EntryType::Reclassifying.status(), Some("R"));
    assert_eq!(EntryType::UnrecordedProposed.status(), Some("U"));
    assert_eq!(EntryType::Eliminating.status(), Some("E"));
    assert_eq!(EntryType::TaxFederal.status(), Some("T"));
    assert_eq!(EntryType::TaxState.status(), Some("V"));
    assert_eq!(EntryType::TaxCity.status(), Some("C"));
    assert_eq!(EntryType::OtherBasis.status(), Some("B"));
    assert_eq!(EntryType::UnrecordedProjected.status(), Some("L"));
    assert_eq!(EntryType::UnrecordedJudgmental.status(), Some("G"));
    assert_eq!(EntryType::Other2.status(), Some("2"));
    assert_eq!(EntryType::Other3.status(), Some("3"));

    let mut row = GlRow::blank();
    row.set("STATUS", "X").unwrap();
    assert!(row.entry_type().is_err());
    assert!(EntryType::from_status("7").is_err());
    assert!(EntryType::TaxState.is_tax());
    assert!(EntryType::UnrecordedProjected.is_unrecorded());
}

#[test]
fn render_builds_canonical_aje_rows() {
    let lines = vec![
        AdjustmentLine::new("1000-00", 250.25),
        AdjustmentLine::new("4000-00", -250.25),
    ];
    let e = AdjustmentEntry::render(
        "AJE #77",
        "REF-77",
        "Test accrual",
        EntryType::Normal,
        "20231231",
        Some(1),
        &lines,
    )
    .unwrap();

    assert_eq!(e.row_count(), 3);
    assert_eq!(e.line_count(), 2);
    assert!(e.is_balanced());
    assert_eq!(e.balance(), Balance { debits: 250.25, credits: 250.25 });

    let header = &e.gl_rows[0];
    assert!(header.is_header());
    assert_eq!(header.get("REFNO").unwrap(), "AJE #77");
    assert_eq!(header.get("FROM_REF").unwrap(), "REF-77");
    assert_eq!(header.get("JE_DESC").unwrap(), "Test accrual");
    assert_eq!(header.get("STATUS").unwrap(), "N");
    assert_eq!(header.get("JOURSOURCE").unwrap(), "AJ");
    assert_eq!(header.get("BUCKET").unwrap(), "Y");
    assert_eq!(header.get("DTYPE").unwrap(), "G");
    assert_eq!(header.get("DATE").unwrap(), "20231231");
    assert_eq!(header.get("PER").unwrap(), "1");
    assert_eq!(header.get("NET").unwrap(), "");
    assert_eq!(header.get("B_ACT").unwrap(), GL_HEADER_ACCOUNT);

    let first = &e.gl_rows[1];
    assert_eq!(first.get("AC_NO").unwrap(), "1000-00");
    assert_eq!(first.get("B_ACT").unwrap(), "1000-00");
    assert_eq!(first.get("NET").unwrap(), "250.25");
    assert_eq!(first.get("CUR_MTH").unwrap(), "250.25");
    assert_eq!(first.get("PER").unwrap(), "1");
    assert_eq!(e.gl_rows[2].get("NET").unwrap(), "-250.25");

    for row in &e.gl_rows {
        assert_eq!(row.encode().unwrap().len(), GL_RECORD_LEN);
        assert_eq!(GlRow::parse_record(&row.encode().unwrap()).unwrap(), *row);
    }

    // a rendered entry re-decodes to the same model through the table store
    let mut t = table(corp());
    let start = t.records.len();
    t.records.extend(e.gl_rows.iter().map(|r| r.encode().unwrap()));
    let j = Adjustments::from_table(&t).unwrap();
    assert_eq!(j.len(), 4);
    let added = j.get("AJE #77").unwrap();
    assert_eq!(added.entry_type, EntryType::Normal);
    assert_eq!(added.line_count(), 2);
    assert_eq!(added.lines[0].amount, 250.25);
    assert!(t.records.len() > start);
}

#[test]
fn render_rejects_invalid_entries() {
    let one = [AdjustmentLine::new("1000-00", 10.0)];
    assert!(AdjustmentEntry::render(
        "AJE #1",
        "",
        "unbalanced",
        EntryType::Normal,
        "20231231",
        None,
        &one,
    )
    .is_err());

    let two = [
        AdjustmentLine::new("1000-00", 10.0),
        AdjustmentLine::new("4000-00", -10.0),
    ];
    assert!(AdjustmentEntry::render(
        "AJE #1",
        "",
        "prior period",
        EntryType::PriorPeriod,
        "20231231",
        None,
        &two,
    )
    .is_err());

    assert!(AdjustmentEntry::render(
        "AJE #1",
        "",
        &"x".repeat(36),
        EntryType::Normal,
        "20231231",
        None,
        &two,
    )
    .is_err(), "JE_DESC is C35");

    let blank = [
        AdjustmentLine::new("", 10.0),
        AdjustmentLine::new("4000-00", -10.0),
    ];
    assert!(AdjustmentEntry::render(
        "AJE #1",
        "",
        "blank account",
        EntryType::Normal,
        "20231231",
        None,
        &blank,
    )
    .is_err());
}

#[test]
fn date_window_and_cell_guards() {
    let j = Adjustments::from_table(&table(corp())).unwrap();
    let e = j.get("AJE #01").unwrap();
    assert!(e.date_in_range("20230101", "20231231").unwrap());
    assert!(!e.date_in_range("20240101", "20241231").unwrap());
    assert!(e.date_in_range("2023-01-01", "20231231").is_err());
    assert!(e.date_in_range("20230101", "20231331").is_err());

    let mut row = GlRow::blank();
    assert!(row.set("NOPE", "x").is_err());
    assert!(row.set("DESC_NOTE", "memo text").is_err(), "FPT cell");
    assert!(row.set("AC_NO", &"A".repeat(41)).is_err());
    assert!(row.set("NET", "not-a-number").is_err());
    assert!(row.set("DATE", "2023/12/31").is_err());
    assert!(row.set("AC_NO", "caf\u{0100}").is_err(), "non-Latin-1 refused");
    assert!(row.set("AC_NO", "caf\u{00e9}").is_ok(), "Latin-1 accepted");
    assert!(row.set("OVERRIDE", "Y").is_ok());
    assert!(row.set("OVERRIDE", "maybe").is_err());
    assert_eq!(row.get("AC_NO").unwrap(), "caf\u{00e9}");

    // a failed write leaves the table untouched
    let mut t = table(corp());
    let before = t.encode();
    let bad = GlRow::parse_record(&t.records[3]).unwrap();
    let out_of_range = t.count() + 5;
    assert!(bad.write_to(&mut t, out_of_range).is_err());
    assert_eq!(t.encode(), before);
}

#[test]
fn write_back_updates_line_amount_atomically() {
    let original = table(corp());
    let mut t = original.clone();
    let rows: Vec<usize> = (0..t.count()).filter(|r| !t.is_deleted(*r)).collect();
    let line_row = rows[1]; // first line of the first entry

    let mut row = GlRow::from_table(&t, line_row).unwrap();
    assert_eq!(row.get("AC_NO").unwrap(), "2640LT");
    assert_eq!(row.net().unwrap(), 490521.00);
    row.set("NET", "1.00").unwrap();
    row.set("CUR_MTH", "1.00").unwrap();
    row.write_to(&mut t, line_row).unwrap();

    let back = GlRow::from_table(&t, line_row).unwrap();
    assert_eq!(back.net().unwrap(), 1.00);
    assert_eq!(back.get("NET").unwrap(), "1.00");

    let j = Adjustments::from_table(&t).unwrap();
    let e = j.get("2").unwrap();
    assert_eq!(e.lines[0].amount, 1.00);
    assert!(!e.is_balanced(), "mutation must surface as an unbalanced entry");
    assert!(j
        .issues()
        .iter()
        .any(|i| i.system_id == "2" && i.kind == AdjustmentIssueKind::Unbalanced));
}

#[test]
fn structural_issues_are_reported() {
    // mixed STATUS
    let mut t = table(corp());
    let rows: Vec<usize> = (0..t.count()).filter(|r| !t.is_deleted(*r)).collect();
    let mut line = GlRow::from_table(&t, rows[1]).unwrap();
    line.set("STATUS", "X").unwrap();
    line.write_to(&mut t, rows[1]).unwrap();
    let j = Adjustments::from_table(&t).unwrap();
    assert!(j
        .issues()
        .iter()
        .any(|i| i.kind == AdjustmentIssueKind::MixedStatus));

    // missing header
    let mut t = table(corp());
    let mut header = GlRow::from_table(&t, rows[0]).unwrap();
    assert!(header.is_header());
    header.set("AC_NO", "9999-99").unwrap();
    header.write_to(&mut t, rows[0]).unwrap();
    let j = Adjustments::from_table(&t).unwrap();
    assert!(j
        .issues()
        .iter()
        .any(|i| i.kind == AdjustmentIssueKind::MissingHeader));

    // empty entry (header only)
    let mut t = table(corp());
    t.records.truncate(rows[1]);
    let j = Adjustments::from_table(&t).unwrap();
    assert_eq!(j.len(), 1);
    assert!(j
        .issues()
        .iter()
        .any(|i| i.kind == AdjustmentIssueKind::EmptyEntry));
}

