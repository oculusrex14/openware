//! Gate: P5.H H3 FX translation (`FXCYTD`/`C_RATE`/`FXTYPE`) + fund
//! accounting (`DIV`/`SUB`/`UNIT`).
//!
//! Truths exercised:
//! 1. Corpus FX/segment truth: `FXTYPE` is H/C/blank, all FX amounts and rates
//!    are zero/blank, and every fund segment is blank (single unfunded fund).
//! 2. `FXCYTD`/`FXPYTD` stamp from `C_RATE`/`RateBook`, and audit detects a
//!    stale cell.
//! 3. A current-rate row without a rate is refused; a `RateBook` supplies it.
//! 4. Fund roll-up by DIV/SUB/UNIT ties out to the chart totals.

use std::path::PathBuf;

use cw_app::accounts::{write_cell, Chart, AM_SCHEMA};
use cw_app::entities::{blank_table, Entity, Org, Year};
use cw_app::fund::{FundKey, FundLedger, FundRoster, UNIT_LEN};
use cw_app::fx::{
    audit_table, effective_rate, translate_account, FxType, RateBook,
};
use cw_core::dbf::Table;

fn corpus_file(tag: &str, suffix: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/dbf")
        .join(tag);
    let want = suffix.to_ascii_lowercase();
    std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {dir:?}: {e}"))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase().ends_with(&want))
                .unwrap_or(false)
        })
        .unwrap_or_else(|| panic!("no *{suffix} in {dir:?}"))
}

fn load_am(tag: &str) -> Table {
    Table::parse_path(corpus_file(tag, "AM.dbf")).expect("AM")
}

/// Minimal synthetic canonical `AM` chart.
fn am_table(rows: &[(&str, &str, &str, &str, f64, f64)]) -> Table {
    let cols: Vec<(&str, u8, u8, u8)> = AM_SCHEMA
        .iter()
        .map(|f| (f.name, f.kind, f.len, f.dec))
        .collect();
    let mut t = blank_table(&cols);
    let rec_len = t.record_len as usize;
    for (ac_no, div, sub, unit, cur_ytd, open) in rows {
        t.records.push(vec![b' '; rec_len]);
        let r = t.records.len() - 1;
        for (col, val) in [
            ("TYPE", "B"),
            ("AC_NO", *ac_no),
            ("DIV", *div),
            ("SUB", *sub),
            ("UNIT", *unit),
            ("CUR_YTD", &format!("{cur_ytd:.2}")),
            ("OPEN", &format!("{open:.2}")),
        ] {
            write_cell(&mut t, r, col, val).unwrap();
        }
    }
    t
}

#[test]
fn corpus_fx_and_fund_truth() {
    for tag in ["corp2026", "hull2024", "mwgh2025"] {
        let t = load_am(tag);
        let chart = Chart::from_table(&t).expect("chart");
        let mut hist = 0;
        let mut current = 0;
        let mut unset = 0;
        for a in chart.posting() {
            match FxType::parse(&a.fxtype) {
                FxType::Historical => hist += 1,
                FxType::Current => current += 1,
                FxType::Unset => unset += 1,
                FxType::Other(c) => panic!("{tag}: unknown FXTYPE {c:?}"),
            }
            assert_eq!(a.fx_cytd, 0.0, "{tag}: FXCYTD should be zero");
            assert_eq!(a.fx_pytd, 0.0, "{tag}: FXPYTD should be zero");
            assert_eq!(a.c_rate, 0.0, "{tag}: C_RATE should be zero");
            assert!(a.div.is_empty(), "{tag}: DIV should be blank");
            assert!(a.sub.is_empty(), "{tag}: SUB should be blank");
            assert!(a.unit.is_empty(), "{tag}: UNIT should be blank");
        }
        assert!(hist > current, "{tag}: H should dominate (H={hist}, C={current})");
        assert!(hist + current + unset == chart.posting().count());

        let ledger = FundLedger::from_chart(&chart);
        assert!(ledger.is_single_unfunded(), "{tag}: single unfunded fund");
        assert!(ledger.ties_out(&chart), "{tag}: fund totals must tie out");
        let key = FundKey::unfunded();
        assert_eq!(
            ledger.get(&key).unwrap().account_count(),
            chart.posting().count()
        );
    }
}

#[test]
fn stamps_and_audits_stale_fx() {
    let mut t = am_table(&[("1000", "", "", "", 1000.0, 500.0)]);
    write_cell(&mut t, 0, "FXTYPE", "C").unwrap();
    write_cell(&mut t, 0, "C_RATE", "1.35000").unwrap();

    let book = RateBook::new();
    let before = audit_table(&t, &book).unwrap();
    assert_eq!(before.checked, 1);
    assert_eq!(before.missing, 1, "blank FXCYTD/FXPYTD are missing");

    let tr = translate_account(&mut t, 0, &book).unwrap();
    assert_eq!(tr.rate, 1.35);
    assert_eq!(tr.expected_cytd, 1350.0);
    assert_eq!(tr.expected_pytd, 675.0);

    let chart = Chart::from_table(&t).unwrap();
    let a = chart.get("1000").unwrap();
    assert_eq!(a.fx_cytd, 1350.0);
    assert_eq!(a.fx_pytd, 675.0);

    let clean = audit_table(&t, &book).unwrap();
    assert!(clean.is_clean(), "{clean:?}");
    assert_eq!(clean.matches, 1);

    write_cell(&mut t, 0, "FXCYTD", "1300.00").unwrap();
    let stale = audit_table(&t, &book).unwrap();
    assert_eq!(stale.mismatches, 1);
    let m = stale.first_mismatch.as_ref().unwrap();
    assert_eq!(m.field, "FXCYTD");
    assert_eq!(m.stored, 1300.0);
    assert_eq!(m.expected, 1350.0);
}

#[test]
fn rate_book_supplies_missing_current_rate() {
    let mut t = am_table(&[("2000", "100", "", "A", 1000.0, 500.0)]);
    write_cell(&mut t, 0, "ENTITY", "SUB").unwrap();
    write_cell(&mut t, 0, "FXTYPE", "C").unwrap();

    let chart = Chart::from_table(&t).unwrap();
    let a = chart.get("2000").unwrap();
    let empty = RateBook::new();
    assert!(effective_rate(a, &empty).is_err(), "C needs a rate");

    let mut book = RateBook::new();
    book.set("SUB", Year::Current, 1.25).unwrap();
    book.set("SUB", Year::Prior1, 1.20).unwrap();
    let tr = translate_account(&mut t, 0, &book).unwrap();
    assert_eq!(tr.expected_cytd, 1250.0);
    assert_eq!(tr.expected_pytd, 600.0);
    assert_eq!(tr.rate, 1.25);

    let chart = Chart::from_table(&t).unwrap();
    let a = chart.get("2000").unwrap();
    assert_eq!(a.c_rate, 1.25, "C_RATE is the current-year projection");
    assert_eq!(a.fx_cytd, 1250.0);
    assert_eq!(a.fx_pytd, 600.0);
    assert_eq!(book.account_grid("SUB")[&Year::Prior1], Some(1.20));
}

#[test]
fn fund_rollup_ties_out_and_validates_roster() {
    let t = am_table(&[
        ("1000", "100", "01", "A", 500.0, 50.0),
        ("2000", "100", "02", "A", 250.0, 25.0),
        ("3000", "200", "", "B", -100.0, -10.0),
        ("4000", "", "", "", 40.0, 4.0),
    ]);
    let chart = Chart::from_table(&t).unwrap();
    let ledger = FundLedger::from_chart(&chart);

    assert_eq!(ledger.len(), 4);
    assert!(!ledger.is_single_unfunded());
    assert!(ledger.ties_out(&chart));
    assert_eq!(ledger.totals().cur_ytd, 690.0);
    assert_eq!(ledger.total(&FundKey::new("100", "01", "A")), Some(500.0));
    assert_eq!(ledger.by_division()["100"].cur_ytd, 750.0);
    assert_eq!(ledger.by_division()[""].cur_ytd, 40.0);

    let mut org = Org::default();
    org.entities.push(Entity::new("PARENT", "Parent").unwrap());
    org.entities[0].division = "100".into();
    let roster = FundRoster::from_org(&org);
    assert!(roster.divisions.contains("100"));
    let undeclared: Vec<_> = ledger
        .issues(&roster)
        .into_iter()
        .filter(|i| i.kind == cw_app::fund::FundIssueKind::UndeclaredDivision)
        .collect();
    assert_eq!(undeclared.len(), 1, "division 200 is not in CE");
    assert_eq!(undeclared[0].key.division, "200");

    // Physical UNIT width is honoured by the key validator.
    assert_eq!(FundKey::new("", "", "ABCDE").too_wide(), Some(("UNIT", UNIT_LEN)));
}

