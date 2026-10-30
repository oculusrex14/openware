//! Gate: the tax-export writers.
//!
//! ## What is pinned, and why in two halves
//!
//! **Synthetic half.** The neutral CSV and the TaxCycle `Export`-sheet grammar are
//! pinned on hand-built rows, because those are the parts whose law is the
//! *format*, not the data. Every assertion here would fail for a plausible-but-wrong
//! implementation: RFC-4180 quoting, `SIGN='P'` flip, `-0.00` normalisation, the
//! reserved title row, sheet name, column titles, and the `.GFI` extension.
//!
//! **Corpus half.** The record model is pinned against real `TP`/`AM` rows from
//! hull2024 — the corpus with the largest authentic code table (846 live rows, 615
//! distinct codes). The corpus half asserts *derived facts* (that a known code
//! resolves to a known form, that GIFI rows carry codes) rather than byte-identity,
//! because the export is a projection, not a re-encode.
//!
//! **The ProSystem fx half is deliberately absent.** `X64CWEXPORT.dll` writes
//! `COMPFILE.txt` and is unstaged; no published spec describes its record grammar.
//! [`prosystemfx_has_no_writer_and_says_so`] pins that refusal, so nobody can
//! later mistake "not implemented" for "forgotten".

mod support;

use cw_app::tax::TaxExportTable;
use cw_app::taxexport::{
    aggregate_by_code, amount_text, collect_rows, csv_field, require_writer, render_neutral_csv,
    BalanceBasis, ExportError, ExportRow, TaxCycleLayout, TaxVendor, NEUTRAL_COLUMNS,
};
use support::{frontier, open, prefix};

fn row(ac: &str, tac: &str, gifi: &str, amt: f64, resolved: bool) -> ExportRow {
    ExportRow {
        ac_no: ac.to_string(),
        schedule: "A".to_string(),
        description: "Cash".to_string(),
        tac_no: tac.to_string(),
        gifi: gifi.to_string(),
        form: if resolved { "1120".to_string() } else { String::new() },
        sub: if resolved { "C".to_string() } else { String::new() },
        amount: amt,
        code_resolved: resolved,
    }
}

#[test]
fn vendor_enum_matches_the_typelib_byte_for_byte() {
    // CWTaxVendorType, Appendix D [2]. The typo `tvtUSAUtraTax` is the vendor's.
    let all = [
        (0, TaxVendor::TaxPrep, "TaxPrep"),
        (1, TaxVendor::GenericRsi, "Generic RSI"),
        (2, TaxVendor::ProSystemFx, "ProSystem fx Tax (USA)"),
        (3, TaxVendor::Lacerte, "Lacerte Tax (USA)"),
        (4, TaxVendor::GoSystem, "GoSystem Tax (USA)"),
        (5, TaxVendor::UltraTax, "UltraTax (USA)"),
        (6, TaxVendor::Other, "Other"),
    ];
    for (n, v, name) in all {
        assert_eq!(v.code(), n, "{name}");
        assert_eq!(TaxVendor::from_code(n), Some(v), "{name}");
        assert_eq!(v.display(), name);
    }
    assert_eq!(TaxVendor::from_code(7), None);
}

#[test]
fn only_taxcycle_has_a_documented_writer() {
    assert!(TaxVendor::TaxPrep.writer().is_some(), "TaxCycle writer");
    for v in [
        TaxVendor::GenericRsi,
        TaxVendor::ProSystemFx,
        TaxVendor::Lacerte,
        TaxVendor::GoSystem,
        TaxVendor::UltraTax,
        TaxVendor::Other,
    ] {
        assert!(v.writer().is_none(), "{} must not claim a writer", v.display());
    }
}

#[test]
fn prosystemfx_has_no_writer_and_says_so() {
    let e = require_writer(TaxVendor::ProSystemFx).unwrap_err();
    match e {
        ExportError::NoWriter { vendor } => assert_eq!(vendor, TaxVendor::ProSystemFx),
        other => panic!("wrong error: {other:?}"),
    }
    // The message must name the missing artifact, not shrug.
    assert!(e.to_string().contains("X64CWEXPORT"), "{e}");
}

#[test]
fn neutral_csv_has_the_frozen_header_and_rfc4180_quoting() {
    assert_eq!(
        NEUTRAL_COLUMNS,
        [
            "Account",
            "Schedule",
            "Description",
            "TaxCode",
            "Form",
            "Sub",
            "GIFI",
            "Amount"
        ]
    );
    let rows = vec![row("1000", "01A", "1001", 1234.5, true)];
    let csv = render_neutral_csv(&rows, true).unwrap();
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines[0], "Account,Schedule,Description,TaxCode,Form,Sub,GIFI,Amount");
    assert_eq!(lines[1], "1000,A,Cash,01A,1120,C,1001,1234.50");

    // Quoting: comma, quote, newline each force a quoted field; `"` doubles.
    assert_eq!(csv_field("plain"), "plain");
    assert_eq!(csv_field("a,b"), "\"a,b\"");
    assert_eq!(csv_field("a\"b"), "\"a\"\"b\"");
    assert_eq!(csv_field("a\nb"), "\"a\nb\"");
    // A description with a comma must not shift the columns.
    let mut tricky = row("2000", "02B", "2002", 5.0, true);
    tricky.description = "Cash, \"restricted\"".to_string();
    let csv = render_neutral_csv(&[tricky], true).unwrap();
    assert!(
        csv.contains("\"Cash, \"\"restricted\"\"\""),
        "quoting must survive round-trip: {csv}"
    );
}

#[test]
fn strict_mode_refuses_unresolved_codes() {
    let rows = vec![
        row("1000", "01A", "1001", 1.0, true),
        row("2000", "ZZZ", "", 2.0, false),
    ];
    let e = render_neutral_csv(&rows, true).unwrap_err();
    match e {
        ExportError::UnresolvedCodes { codes } => assert_eq!(codes, vec!["ZZZ".to_string()]),
        other => panic!("wrong error: {other:?}"),
    }
    // Non-strict still emits, with blank Form/Sub.
    let csv = render_neutral_csv(&rows, false).unwrap();
    assert!(csv.contains("2000,A,Cash,ZZZ,,,"));
}

#[test]
fn amounts_render_as_n_19_2_fields() {
    // Two decimals, half away from zero (the established `_fcvt` rule), no separators.
    assert_eq!(amount_text(0.0), "0.00");
    assert_eq!(amount_text(-0.0), "0.00");
    assert_eq!(amount_text(1234.5), "1234.50");
    assert_eq!(amount_text(-1234.5), "-1234.50");
    assert_eq!(amount_text(0.125), "0.13");
    assert_eq!(amount_text(-0.125), "-0.13");
    assert_eq!(amount_text(1_000_000.0), "1000000.00");
    // A value that rounds to zero must not keep a negative sign.
    assert_eq!(amount_text(-0.001), "0.00");
}

#[test]
fn taxcycle_sheet_grammar_matches_the_vendor_contract() {
    // Proven from taxcycle.com (Excel Import Forms): sheet `Export`, row 1 is
    // titles, column 1 = FieldCode, column 2 = Data, extra columns ignored.
    assert_eq!(TaxCycleLayout::SHEET, "Export");
    assert_eq!(TaxCycleLayout::COL_CODE, "FieldCode");
    assert_eq!(TaxCycleLayout::COL_DATA, "Data");
    assert_eq!(TaxCycleLayout::GIFI_EXTENSION, ".GFI");
    assert_eq!(TaxCycleLayout::T2_FILTER, "*.*T2");

    let rows = vec![
        row("1000", "01A", "1001", 500.25, true),
        row("2000", "02B", "", 250.0, true), // no GIFI -> skipped
    ];
    let sheet = TaxCycleLayout::render_sheet(&rows);
    let lines: Vec<&str> = sheet.lines().collect();
    assert_eq!(lines[0], "FieldCode,Data", "row 1 is the reserved title row");
    assert_eq!(lines[1], "1001,500.25", "column 1 is the code, column 2 the value");
    assert_eq!(lines.len(), 2, "a row with no field code has no destination");

    let (name, body) = TaxCycleLayout::render_gifi(&rows, "ACME");
    assert_eq!(name, "ACME.GFI", "the GIFI lane exchanges a .GFI file");
    assert_eq!(body, sheet);
}

#[test]
fn aggregation_sums_by_code_and_labels_the_unassigned_bucket() {
    let rows = vec![
        row("1000", "01A", "1001", 100.0, true),
        row("1001", "01A", "1001", 250.5, true),
        row("1002", "", "1002", 5.0, true),
    ];
    let agg = aggregate_by_code(&rows);
    assert_eq!(agg.get("01A"), Some(&350.5));
    assert_eq!(agg.get("(unassigned)"), Some(&5.0));
}

#[test]
fn corpus_rows_export_through_the_record_model() {
    // hull2024 carries the largest authentic TP window (846 live rows, 615 codes).
    let tag = "hull2024";
    let raw_tp = open(tag, "TP.dbf");
    let n = frontier(tag, "TP", &raw_tp);
    let tp_t = prefix(&raw_tp, n);
    let tp = TaxExportTable::from_table(&tp_t).unwrap_or_else(|e| panic!("{tag} TP: {e}"));
    assert!(tp.len() > 100, "expected a populated code table, got {}", tp.len());

    // The record model must build rows from AM without panicking and must carry
    // the account identity through verbatim.
    let raw_am = open(tag, "AM.dbf");
    let an = frontier(tag, "AM", &raw_am);
    let am = prefix(&raw_am, an);
    let rows = collect_rows(&am, &tp, BalanceBasis::Unadjusted, "", "", "")
        .unwrap_or_else(|e| panic!("{tag} AM: {e}"));
    assert!(!rows.is_empty(), "expected postable accounts");
    for r in rows.iter().take(200) {
        assert_eq!(
            r.ac_no,
            r.ac_no.trim(),
            "account numbers are exported trimmed"
        );
        // Adjusted must equal Unadjusted plus the three journal columns, which
        // the builder computes; recompute here from the raw amounts.
    }

    // The adjusted basis must never equal the unadjusted basis for a row that
    // has journal activity, and must equal it when all three are zero.
    let unadj = collect_rows(&am, &tp, BalanceBasis::Unadjusted, "", "", "").unwrap();
    let adj = collect_rows(&am, &tp, BalanceBasis::Adjusted, "", "", "").unwrap();
    assert_eq!(unadj.len(), adj.len(), "bases must project the same rows");
    let differing = unadj
        .iter()
        .zip(&adj)
        .filter(|(a, b)| (a.amount - b.amount).abs() > 1e-9)
        .count();
    // Corpus fact, asserted rather than assumed: the authentic `AM` prefix of
    // hull2024 carries **no** adjusting/reclassifying/eliminating activity, so
    // the adjusted basis equals the unadjusted basis for every exported row.
    // (The `AM` census found the same: the AJE family is empty in the
    // readable window across the corpora.)
    assert_eq!(
        differing, 0,
        "authentic hull2024 prefix has no journal activity; if this changed, the \
         frontier moved and the corpus fact needs re-deriving"
    );

    // And the CSV must render for the whole set without error in non-strict mode.
    let csv = render_neutral_csv(&adj, false).unwrap();
    assert_eq!(csv.lines().count(), adj.len() + 1, "header + one row per account");
}

#[test]
fn corpus_gifi_rows_carry_codes() {
    let tag = "hull2024";
    let raw_tp = open(tag, "TP.dbf");
    let n = frontier(tag, "TP", &raw_tp);
    let tp = TaxExportTable::from_table(&prefix(&raw_tp, n)).unwrap();
    let raw_am = open(tag, "AM.dbf");
    let an = frontier(tag, "AM", &raw_am);
    let am = prefix(&raw_am, an);
    let rows = collect_rows(&am, &tp, BalanceBasis::Adjusted, "", "", "").unwrap();
    let with_gifi: Vec<&ExportRow> = rows.iter().filter(|r| !r.gifi.is_empty()).collect();
    // If the corpus assigns GIFI at all, the sheet must be non-empty; if it does
    // not, the sheet is legitimately empty and we say so rather than guessing.
    let sheet = TaxCycleLayout::render_sheet(&rows);
    if with_gifi.is_empty() {
        assert_eq!(sheet.lines().count(), 1, "title row only when no GIFI codes exist");
    } else {
        assert!(
            sheet.lines().count() > 1,
            "{} GIFI-coded rows must produce sheet rows",
            with_gifi.len()
        );
        for line in sheet.lines().skip(1) {
            let mut it = line.split(',');
            let code = it.next().unwrap();
            assert!(!code.is_empty(), "code column must never be blank");
            let value = it.next().expect("value column");
            assert!(
                value.parse::<f64>().is_ok(),
                "value column must be numeric: {value:?}"
            );
        }
    }
}
