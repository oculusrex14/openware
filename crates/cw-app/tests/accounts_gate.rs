//! P5.E1 gate: the `AM` chart model + account-properties surface.
//!
//! Golden input: `tests/golden/dbf/{corp2026,hull2024,mwgh2025}/*AM.dbf` (the
//! frozen 81-column corpus tables) and the area census
//! `tests/golden/area/domain.json` (`examples/freeze_area.rs`).
//!
//! Provenance: `AM` is **not** one of the twelve F63-damaged tables (see
//! `tests/support/mod.rs`), so every row of every corpus is authentic and this
//! gate asserts whole tables — no readable-frontier cap applies here. The
//! fixture pins the population census (chart size, posting split, kind split,
//! issue count); the physical schema is a format constant asserted inline from
//! the shipped type-library header, not a measurement.
//!
//! The gate locks:
//!
//! - the physical schema against `inventory/dbf-schema.tsv` (81 fields,
//!   1603-byte records, family partition);
//! - chart decode on both corpora (TYPE B/I/M taxonomy, memo rows excluded
//!   from the posting index, no false duplicate issues);
//! - byte-identical property round-trip on every live row;
//! - atomic property-patch writes + width/type refusal;
//! - schema-drift detection and FPT memo refusal.

mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;

use cw_app::accounts::{
    family_columns, schema_drift, schema_matches, write_cell, AccountIssueKind, AccountKind,
    AccountProperties, AccountsPredicate, AmAccount, Chart, DriftKind, Family, Flag,
    GetAccountsType, PropertiesPatch, Side, AM_SCHEMA,
};
use cw_core::dbf::Table;
use support::{map_at, table_fixture, usize_at, CORPORA};

fn corpus(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("..");
    p.push("..");
    p.push("tests");
    p.push("golden");
    p.push("dbf");
    p.push(name);
    p.push("USI Compilation Training(1155962)AM.dbf");
    p
}

fn mwgh() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("..");
    p.push("..");
    p.push("tests");
    p.push("golden");
    p.push("dbf");
    p.push("mwgh2025");
    p.push("MWGH LLC 9-30-25 REVIEW(1239263)am.dbf");
    p
}

fn hull() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("..");
    p.push("..");
    p.push("tests");
    p.push("golden");
    p.push("dbf");
    p.push("hull2024");
    p.push("2024 Hull Lift Truck Review(1148441)AM.dbf");
    p
}

#[test]
fn physical_schema_matches_inventory() {
    let t = Table::parse_path(corpus("corp2026")).unwrap();
    assert_eq!(t.fields.len(), 81, "AM field count");
    assert_eq!(t.record_len, 1603, "AM record length");
    assert_eq!(
        t.fields.iter().map(|f| f.len as usize).sum::<usize>() + 1,
        t.record_len as usize,
        "declared widths must sum to the record length"
    );
    assert!(schema_matches(&t), "drift: {:?}", schema_drift(&t));
    assert_eq!(AM_SCHEMA.len(), 81);

    // families partition every column exactly once
    let mut counts: BTreeMap<Family, usize> = BTreeMap::new();
    for fam in Family::ALL {
        counts.insert(fam, family_columns(fam).count());
    }
    assert_eq!(counts.values().sum::<usize>(), 81);
    assert_eq!(counts[&Family::Identity], 5);
    assert_eq!(counts[&Family::Segment], 12);
    assert_eq!(counts[&Family::Description], 2);
    assert_eq!(counts[&Family::Grouping], 20);
    assert_eq!(counts[&Family::Tax], 3);
    assert_eq!(counts[&Family::Presentation], 10);
    assert_eq!(counts[&Family::Balance], 9);
    assert_eq!(counts[&Family::Annotation], 3);
    assert_eq!(counts[&Family::Fx], 4);
    assert_eq!(counts[&Family::Integrity], 4);
    assert_eq!(counts[&Family::Meta], 8);
    assert_eq!(counts[&Family::Opaque], 1);

    // the hull corpus is the same physical layout
    let h = Table::parse_path(hull()).unwrap();
    assert!(schema_matches(&h), "hull drift: {:?}", schema_drift(&h));
}

#[test]
fn chart_decodes_every_corpus() {
    let corp = Chart::from_table(&Table::parse_path(corpus("corp2026")).unwrap()).unwrap();
    assert_eq!(corp.len(), 325);
    assert_eq!(corp.posting().count(), 325);
    assert!(corp.issues().is_empty(), "corp issues: {:?}", corp.issues());
    let totals = corp.totals();
    assert_eq!(
        totals.balance_sheet_ytd + totals.income_ytd,
        totals.cur_ytd,
        "kind totals must sum to the chart total"
    );
    assert!(corp.numbers().count() > 100);

    let h = Chart::from_table(&Table::parse_path(hull()).unwrap()).unwrap();
    assert_eq!(h.len(), 402);
    assert_eq!(h.posting().count(), 352, "TYPE M rows are non-posting");
    assert_eq!(
        h.accounts()
            .iter()
            .filter(|a| a.kind == AccountKind::Memo)
            .count(),
        50
    );
    // the memo block uses the marker number 'z' 50 times - not a duplicate issue
    assert!(h.get("z").is_none(), "memo marker is not a posting account");
    assert_eq!(h.get_all("z").count(), 0);
    let issues = h.issues();
    assert!(
        issues
            .iter()
            .all(|i| i.kind != AccountIssueKind::DuplicateNumber),
        "false duplicate issues: {issues:?}"
    );

    // mwgh2025 completes the census: the same 81-column table, a third chart.
    let m = Chart::from_table(&Table::parse_path(mwgh()).unwrap()).unwrap();
    assert_eq!(m.len(), 349);
    assert_eq!(m.posting().count(), 312, "37 TYPE M memo rows");
    assert!(m.issues().is_empty(), "mwgh issues: {:?}", m.issues());

    // The area fixture pins exactly these numbers, over whole tables (`AM` is
    // undamaged end to end in all three corpora).
    for (tag, chart) in [("corp2026", &corp), ("hull2024", &h), ("mwgh2025", &m)] {
        let f = table_fixture(tag, "am");
        assert_eq!(usize_at(&f, "records"), chart.len(), "{tag} records (no tombstones)");
        assert_eq!(usize_at(&f, "live"), chart.len(), "{tag} live");
        assert_eq!(usize_at(&f, "fields"), 81, "{tag} field count");
        assert_eq!(usize_at(&f, "record_len"), 1603, "{tag} record length");
        assert_eq!(usize_at(&f, "posting"), chart.posting().count(), "{tag} posting");
        assert_eq!(usize_at(&f, "numbers"), chart.numbers().count(), "{tag} numbers");
        assert_eq!(usize_at(&f, "issues"), chart.issues().len(), "{tag} issues");
        let kinds = map_at(&f, "kinds");
        let mut derived: BTreeMap<String, usize> = BTreeMap::new();
        for a in chart.accounts() {
            let key = match a.kind {
                AccountKind::BalanceSheet => "BalanceSheet",
                AccountKind::Income => "Income",
                AccountKind::Memo => "Memo",
                AccountKind::Other(_) => "Other",
            };
            *derived.entry(key.to_string()).or_insert(0) += 1;
        }
        assert_eq!(kinds, derived, "{tag} kind census");
    }
}

#[test]
fn property_roundtrip_is_byte_identical() {
    let original = Table::parse_path(corpus("corp2026")).unwrap();
    let mut t = original.clone();
    for r in 0..t.records.len() {
        let props = AccountProperties::from_table(&t, r).unwrap();
        props.write_to(&mut t, r).unwrap();
    }
    assert_eq!(
        t.encode(),
        original.encode(),
        "decoded properties must re-encode byte-identically"
    );
}

#[test]
fn property_patch_writes_and_is_atomic() {
    let mut t = Table::parse_path(corpus("corp2026")).unwrap();
    let before = t.encode();

    let patch = PropertiesPatch {
        print: Some(Flag::yes()),
        total: Some(Flag::yes()),
        hidden: Some(Flag::yes()),
        dc: Some(Side::Credit),
        sign: Some(Side::Credit),
        column: Some(Flag::yes()),
        dollar: Some(Flag::yes()),
        uline: Some(Flag::yes()),
        page: Some(Flag::yes()),
        ..Default::default()
    };
    patch.write_to(&mut t, 0).unwrap();
    let props = AccountProperties::from_table(&t, 0).unwrap();
    assert!(props.print.is_yes());
    assert!(props.total.is_yes());
    assert!(props.hidden.is_yes());
    assert_eq!(props.dc, Side::Credit);
    assert_eq!(props.sign, Side::Credit);
    assert!(props.column.is_yes() && props.dollar.is_yes());
    assert!(props.uline.is_yes() && props.page.is_yes());
    assert_eq!(t.cell_str(0, "DC").unwrap().trim(), "C");
    assert_eq!(t.cell_str(0, "SIGN").unwrap().trim(), "C");

    // invalid width / value leaves the table untouched (atomic)
    let after_valid = t.encode();
    let bad = PropertiesPatch {
        ctl: Some("XYZ".to_string()), // CTL is C2
        ..Default::default()
    };
    assert!(bad.write_to(&mut t, 0).is_err());
    assert_eq!(t.encode(), after_valid, "failed patch must not write");

    let bad_den = PropertiesPatch {
        den: Some("caf\u{00e9}".to_string()), // latin-1 is fine...
        ..Default::default()
    };
    assert!(bad_den.write_to(&mut t, 0).is_ok(), "Latin-1 DEN accepted");
    let bad_unicode = PropertiesPatch {
        den: Some("caf\u{0100}".to_string()), // ...non-Latin-1 is not
        ..Default::default()
    };
    assert!(bad_unicode.write_to(&mut t, 0).is_err());

    // no-op patch is byte-identical to the untouched row set
    let mut fresh = Table::parse_path(corpus("corp2026")).unwrap();
    PropertiesPatch::default()
        .write_to(&mut fresh, 0)
        .unwrap();
    assert_eq!(fresh.encode(), before);
}

#[test]
fn schema_drift_is_detected() {
    let mut t = Table::parse_path(corpus("corp2026")).unwrap();
    t.fields[3].len = 39; // AC_NO C40 -> C39
    let drift = schema_drift(&t);
    assert!(drift.iter().any(|d| d.column == "AC_NO"
        && d.kind == DriftKind::Length
        && d.expected == "40"
        && d.actual == "39"));
    assert!(!schema_matches(&t));
    assert!(Chart::from_table(&t).is_err());
}

#[test]
fn memo_and_type_guards() {
    let mut t = Table::parse_path(corpus("corp2026")).unwrap();
    assert!(write_cell(&mut t, 0, "MEMO", "text").is_err(), "FPT cell");
    assert!(write_cell(&mut t, 0, "AC_NO", &"X".repeat(41)).is_err());
    assert!(write_cell(&mut t, 0, "CUR_YTD", "not-a-number").is_err());
    assert!(write_cell(&mut t, 0, "AC_DESC", "Renamed").is_ok());
    assert_eq!(t.cell_str(0, "AC_DESC").unwrap().trim(), "Renamed");

    // GIFI gate: hull has no GIFI mapping at all; a mapped + funded row passes
    let mut h = Table::parse_path(hull()).unwrap();
    assert!(!AmAccount::from_table(&h, 0).unwrap().gifi_eligible());
    write_cell(&mut h, 0, "GIFI", "1000").unwrap();
    write_cell(&mut h, 0, "OPEN", "1.00").unwrap();
    let a = AmAccount::from_table(&h, 0).unwrap();
    assert!(a.gifi_eligible());
    assert_eq!(a.open, 1.0);
    assert!(a.is_posted(), "corpus row 0 has POST='Y'");
}


// --- `CWGetAccountsType` ----------------------------------------------------

/// `GetAccounts(gatAccount)` excludes exactly the computed `POST='N'` rows.
///
/// Proven population law (all three frozen corpora, every row authentic):
/// `AM` carries one `POST='N'` posting row per engagement — the computed
/// `NETINC` "Net Income (Loss)" balance-sheet line — so the shipped
/// `GetAccounts` census is live-minus-one. `corp2026` is the documented
/// 324-of-325 observation.
#[test]
fn get_accounts_excludes_unposted_netinc() {
    for (tag, path, live, posting, gat) in [
        ("corp2026", corpus("corp2026"), 325usize, 325usize, 324usize),
        (
            "hull2024",
            hull(),
            402,
            352,
            351,
        ),
    ] {
        let chart = Chart::from_table(&Table::parse_path(&path).unwrap()).unwrap();
        assert_eq!(chart.len(), live, "{tag}: chart size");
        assert_eq!(chart.posting().count(), posting, "{tag}: posting rows");

        let view = chart.get_accounts(GetAccountsType::Account).unwrap();
        assert_eq!(view.len(), gat, "{tag}: GetAccounts(gatAccount)");
        assert_eq!(view.kind, GetAccountsType::Account);

        // exactly one posting row is unposted, and it is the computed NETINC line
        let unposted: Vec<_> = chart
            .posting()
            .filter(|a| a.props.post.is_no())
            .collect();
        assert_eq!(unposted.len(), 1, "{tag}: one POST='N' posting row");
        assert_eq!(unposted[0].ac_no.trim(), "NETINC", "{tag}: unposted account");
        assert_eq!(
            unposted[0].props.post.raw(),
            "N",
            "{tag}: raw POST cell is N, not blank"
        );
        // every gatAccount row is posted (Y or, defensively, anything but N)
        assert!(view.rows().iter().all(|a| !a.props.post.is_no()));
        // the filter is the posting set minus that row, in physical order
        let posting_rows: Vec<&AmAccount> = chart.posting().collect();
        let expected: Vec<&AmAccount> = posting_rows
            .iter()
            .copied()
            .filter(|a| !a.props.post.is_no())
            .collect();
        assert_eq!(view.rows(), &expected[..]);
    }
}

/// The five statement-side classes refuse rather than guess a predicate.
#[test]
fn get_accounts_refuses_unproven_classes() {
    let chart = Chart::from_table(&Table::parse_path(&corpus("corp2026")).unwrap()).unwrap();
    for kind in [
        GetAccountsType::Report,
        GetAccountsType::CashFlow,
        GetAccountsType::Performance,
        GetAccountsType::Distributable,
        GetAccountsType::Flipped,
    ] {
        assert!(matches!(kind.predicate(), AccountsPredicate::Unproven));
        let err = chart.get_accounts(kind).unwrap_err().to_string();
        assert!(err.contains(kind.name()), "{err} must name the selector");
    }
    assert!(matches!(
        GetAccountsType::Account.predicate(),
        AccountsPredicate::Posting
    ));
}

/// Enum values and member names are the shipped typelib's, verbatim.
#[test]
fn get_accounts_enum_matches_typelib() {
    let expect = [
        ("gatAccount", 0u8),
        ("gatReport", 1),
        ("gatCashFlow", 2),
        ("gatPerformance", 3),
        ("gatDistributable", 4),
        ("gatFlipped", 5),
    ];
    assert_eq!(GetAccountsType::ALL.len(), expect.len());
    for (kind, (name, value)) in GetAccountsType::ALL.iter().zip(expect) {
        assert_eq!(kind.name(), name);
        assert_eq!(kind.value(), value);
        assert_eq!(GetAccountsType::from_value(value), Some(*kind));
    }
    assert_eq!(GetAccountsType::from_value(6), None);
}
