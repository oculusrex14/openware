//! P5.E5 gate: `TP` tax-export codes + `AM`/`MP` tax + GIFI assignment.
//!
//! Golden inputs: `<corpus>/*TP.dbf`, `*AM.dbf`, `*MP.dbf`, plus the area census
//! `tests/golden/area/domain.json` (`examples/freeze_area.rs`).
//!
//! Provenance: the 18-column / 174-byte `TP` layout is a format constant from the
//! shipped type-library header (spec `tax-export.md`, asserted inline). The
//! population censuses come from the fixture, which is a kernel-derived
//! regression pin; `tests/support/mod.rs` states the F63 frontier law.
//!
//! # Why every `TP` assertion is scoped to a readable prefix
//!
//! `TP` is the most heavily damaged table in the freeze: the member's LZMA
//! stream is truncated in the shipped `.ac_`, so rows past the frontier are
//! fabricated. The consequence is not cosmetic — it decides which corpus can
//! carry which law:
//!
//! | corpus | records | frontier | live rows in prefix | distinct codes |
//! |---|---|---|---|---|
//! | corp2026 | 22,150 | 2,299 | **0** (every prefix row is a tombstone) | 0 |
//! | hull2024 | 39,195 | 2,184 | 846 | 615 |
//! | mwgh2025 | 25,417 | 2,305 | 2,081 | 445 |
//!
//! `corp2026` therefore has **no tax-code data at all**: its 19,805 "live" rows
//! all sit in the fabricated tail. Its `AM` and `MP` tables carry no `TAC_NO` or
//! `GIFI` values either, so corp2026 contributes the empty-set laws only (an
//! empty code table, an empty reference set, a schema check) and the populated
//! laws are asserted on hull2024 (the larger authentic code table) and mwgh2025.
//!
//! Locks:
//!
//! - the physical 18-column / 174-byte schema on all three corpora;
//! - COM `Get` by `Id` (`TCODE`) or `ItemIdentifier` (`CWGUID`);
//! - `(FORM, ENTITY, YEAR)` scope projection + user-defined mutability;
//! - `AM.TAC_NO` / `MP.TAC_NO` / `MP.TAX1..TAX4` validated assignment;
//! - `AM.GIFI` / `MP.GIFI` assignment + F55 row-gate audit;
//! - byte-identical `TP` re-encode over every readable prefix.

mod support;

use std::collections::BTreeMap;

use cw_app::tax::{
    account_gifi, account_tax_code, account_tax_issues, assign_account_gifi,
    assign_account_tax_code, assign_mapping_gifi, assign_mapping_tax_code, clear_account_gifi,
    clear_account_tax_code, clear_mapping_tax_code, gifi_code, gifi_issues, gifi_rows, mapping_gifi,
    mapping_tax_code, mapping_tax_issues, rewrite_codes, set_code_description, set_code_percent,
    tp_schema_drift, tp_schema_matches, GifiIssueKind, TaxExportTable, TP_SCHEMA,
};
use cw_core::dbf::Table;
use support::{frontier, map_at, open, prefix, table_fixture, triples_at, usize_at, CORPORA};

/// The authentic `TP` window for one corpus (F63).
fn tp_of(tag: &str) -> (Table, TaxExportTable) {
    let raw = open(tag, "TP.dbf");
    let n = frontier(tag, "TP", &raw);
    let t = prefix(&raw, n);
    let tp = TaxExportTable::from_table(&t)
        .unwrap_or_else(|e| panic!("{tag} TP prefix {n}: {e}"));
    (t, tp)
}

/// The authentic `MP` window for one corpus (F63).
fn mp_of(tag: &str) -> Table {
    let raw = open(tag, "MP.dbf");
    prefix(&raw, frontier(tag, "MP", &raw))
}

fn first_live(t: &Table) -> usize {
    (0..t.records.len())
        .find(|&r| !t.is_deleted(r))
        .expect("live row")
}

#[test]
fn physical_schema_matches_inventory_on_all_corpora() {
    // Format constant: 18 descriptors / 174-byte records (spec `tax-export.md`).
    assert_eq!(TP_SCHEMA.len(), 18);
    for (tag, _) in CORPORA {
        let t = open(tag, "TP.dbf");
        assert_eq!(t.fields.len(), 18, "{tag} field count");
        assert_eq!(t.record_len, 174, "{tag} record length");
        assert_eq!(
            t.fields.iter().map(|f| f.len as usize).sum::<usize>() + 1,
            t.record_len as usize,
            "{tag}: declared widths must sum to the record length"
        );
        assert!(
            tp_schema_matches(&t),
            "{tag} drift: {:?}",
            tp_schema_drift(&t)
        );

        // The census pins the record count and the readable frontier.
        let f = table_fixture(&tag, "tp");
        assert_eq!(usize_at(&f, "records"), t.count(), "{tag} census records");
        assert_eq!(
            usize_at(&f, "frontier"),
            frontier(&tag, "TP", &t),
            "{tag} pinned frontier"
        );
    }
}

#[test]
fn hull_code_table_decodes_as_observed() {
    // hull2024 is the largest authentic `TP` window: 846 live rows / 615 codes
    // inside the first 2,184 of 39,195 records.
    let (t, tp) = tp_of("hull2024");
    assert_eq!(t.count(), 2_184, "hull2024 TP readable frontier");
    let f = table_fixture("hull2024", "tp");
    assert_eq!(usize_at(&f, "frontier"), 2_184);
    assert_eq!(usize_at(&f, "records"), 39_195, "hull TP physical records");
    assert_eq!(tp.len(), 846, "live rows in the prefix");
    assert_eq!(usize_at(&f, "live"), 846);
    assert_eq!(tp.deleted(), 2_184 - 846, "tombstoned rows in the prefix");
    assert_eq!(tp.codes().len(), 615, "distinct TCODE");
    assert_eq!(usize_at(&f, "codes"), 615);

    // UDEF census: 227 `U` + 27 `X` = 254 user-defined rows.
    assert_eq!(tp.user_defined().len(), 254, "UDEF U+X rows");
    assert_eq!(
        tp.user_defined()
            .iter()
            .filter(|r| r.user_defined.raw() == "U")
            .count(),
        227
    );
    assert_eq!(
        tp.user_defined()
            .iter()
            .filter(|r| r.user_defined.raw() == "X")
            .count(),
        27
    );
    let udef = map_at(&f, "udef");
    assert_eq!(udef.get("U").copied(), Some(227));
    assert_eq!(udef.get("X").copied(), Some(27));
    assert_eq!(udef.get("").copied(), Some(592), "standard rows");

    // Every live row is an export row (`TYPE='W'`).
    assert!(
        tp.rows().iter().all(|r| r.row_type == "W"),
        "hull TP TYPE must be W on every live row"
    );
    assert_eq!(map_at(&f, "types").get("W").copied(), Some(846));

    // The `01A` code: two scopes, 1120/C/Y14 and 1120/C/Y15.
    let first = tp.get("01A").expect("code 01A");
    assert_eq!(first.code, "01A");
    assert_eq!(first.form, "1120");
    assert_eq!(first.entity, "C");
    assert_eq!(first.year, "14");
    assert_eq!(first.detail, "*");
    assert_eq!(first.percent, 100.0);
    assert!(!first.is_user_defined(), "shipped row is standard");
    assert_eq!(first.row_type, "W");
    assert_eq!(tp.all_by_code("01A").len(), 2, "2 scopes for 01A");

    // The fixture pins the same projection.
    let p = f.get("first_01a").expect("pinned 01A row");
    assert_eq!(usize_at(p, "row"), first.row);
    assert_eq!(support::str_at(p, "form"), "1120");
    assert_eq!(support::str_at(p, "entity"), "C");
    assert_eq!(support::str_at(p, "year"), "14");
    assert_eq!(support::f64_at(p, "percent"), 100.0);
    assert_eq!(usize_at(&f, "max_scopes"), 2, "widest fan-out is 01A");

    // COM Get accepts ItemIdentifier as well as Id.
    let by_guid = tp.get(&first.cwguid).expect("CW GUID lookup");
    assert_eq!(by_guid.row, first.row);
    assert!(tp.get("no-such-code").is_none());
    assert!(tp.validate("no-such-code").is_err());
}

#[test]
fn corp2026_tax_table_is_tombstones_only() {
    // The F63 boundary lands inside corp2026's tombstone block, so the whole
    // authentic window is deleted rows and no code law is assertable.
    let (t, tp) = tp_of("corp2026");
    assert_eq!(t.count(), 2_299, "corp2026 TP readable frontier");
    assert_eq!(tp.len(), 0, "no live row inside the authentic window");
    assert_eq!(tp.deleted(), 2_299);
    assert!(tp.codes().is_empty());
    assert!(tp.user_defined().is_empty());
    assert!(tp.get("01A").is_none());

    let f = table_fixture("corp2026", "tp");
    assert_eq!(usize_at(&f, "frontier"), 2_299);
    assert_eq!(usize_at(&f, "records"), 22_150);
    assert_eq!(usize_at(&f, "live"), 0);
    assert_eq!(usize_at(&f, "codes"), 0);
    assert!(f.get("first_01a").is_none() || f.get("first_01a").and_then(|x| x.as_str()).is_none());

    // Its AM/MP tax cells are empty too, so the reference audits are vacuous.
    let am = open("corp2026", "AM.dbf");
    assert_eq!(account_tax_issues(&am, &tp).len(), 0);
    assert!(gifi_issues(&am, &BTreeMap::new()).is_empty());
}

#[test]
fn scope_projection_and_user_mutability() {
    let (_, tp) = tp_of("hull2024");

    assert!(tp.resolve("01A", "1120", "C", "14").is_some_and(|r| r.code == "01A"));
    assert!(tp.resolve("01A", "1120", "C", "99").is_none());
    assert!(tp.resolve("01A", "1120S", "C", "14").is_none());
    assert!(tp.validate_for_scope("01A", "1120", "C", "14").is_ok());
    // `01A` has exactly two authentic scopes (1120/C/Y14 and 1120/C/Y15). The
    // pre-amendment gate asserted `01A` resolves for 1120S/S/Y14 as well; that
    // row is a fabricated-tail record, so the scope law is asserted on the two
    // rows that are actually in the file.
    assert!(
        tp.validate_for_scope("01A", "1120S", "S", "14").is_err(),
        "1120S/S is a fabricated-tail scope for 01A"
    );
    assert_eq!(tp.all_by_code("01A").len(), 2);

    // Blank YEAR is a wildcard: every observed user row carries YEAR=''.
    let user = tp
        .rows()
        .iter()
        .find(|r| r.user_defined.raw() == "U")
        .expect("user row");
    assert_eq!(user.year, "");
    assert!(user.applies_to(&user.form, &user.entity, "24"));
    assert!(user.applies_to(&user.form, &user.entity, "25"), "blank YEAR is all years");
    assert!(user.is_user_defined());
    assert_eq!(user.code, "05.02");
    assert_eq!(user.form, "1120S");

    // The same laws on the second populated corpus.
    let (_, mwgh) = tp_of("mwgh2025");
    assert!(mwgh.get("01A").is_some());
    assert_eq!(mwgh.all_by_code("01A").len(), 9, "mwgh 01A fan-out");
    let mu = mwgh
        .rows()
        .iter()
        .find(|r| r.user_defined.raw() == "U")
        .expect("mwgh user row");
    assert_eq!(mu.year, "");
    assert!(mu.applies_to(&mu.form, &mu.entity, "26"));
}

#[test]
fn account_tax_assignment_is_validated_and_atomic() {
    let (_, tp) = tp_of("hull2024");
    let mut am = open("hull2024", "AM.dbf");
    let row = first_live(&am);
    let before = account_tax_code(&am, row).unwrap();

    assign_account_tax_code(&mut am, row, &tp, "01A").unwrap();
    assert_eq!(account_tax_code(&am, row).unwrap(), "01A");

    let err = assign_account_tax_code(&mut am, row, &tp, "ZZ-NOT-A-CODE").unwrap_err();
    assert!(
        err.to_string().contains("is not valid"),
        "COM-matching message: {err}"
    );
    assert_eq!(
        account_tax_code(&am, row).unwrap(),
        "01A",
        "refused write must not touch the cell"
    );

    clear_account_tax_code(&mut am, row).unwrap();
    assert_eq!(account_tax_code(&am, row).unwrap(), "");
    assign_account_tax_code(&mut am, row, &tp, &before).unwrap();
    assert_eq!(account_tax_code(&am, row).unwrap(), before);

    // `AM` carries 112 distinct codes, 47 of which resolve inside the readable
    // `TP` window; the other 65 point at fabricated-tail rows (F63 residual,
    // pinned by the census) and are reported by the audit, never silently
    // accepted.
    let f = table_fixture("hull2024", "tp");
    assert_eq!(usize_at(&f, "am_refs"), 112, "distinct AM.TAC_NO values");
    assert_eq!(
        usize_at(&f, "am_refs_missing"),
        65,
        "codes whose only TP row is in the fabricated tail"
    );
    let issues = account_tax_issues(&am, &tp);
    assert_eq!(issues.len(), 65 + 59, "one issue per AM row, not per distinct code");
    assert!(
        issues.iter().all(|i| tp.get(&i.code).is_none()),
        "every reported reference is genuinely absent from the prefix"
    );
    // Restoring the corpus cell leaves exactly that residual.
    assert_eq!(account_tax_issues(&am, &tp).len(), issues.len());

    // mwgh2025 has only 3 residual references (4 rows).
    let (_, mtp) = tp_of("mwgh2025");
    let mam = open("mwgh2025", "AM.dbf");
    assert_eq!(
        usize_at(&table_fixture("mwgh2025", "tp"), "am_refs_missing"),
        3
    );
    assert_eq!(account_tax_issues(&mam, &mtp).len(), 4);
}

#[test]
fn mapping_tax_slots_assign_and_audit() {
    let (_, tp) = tp_of("hull2024");
    let mp_raw = mp_of("hull2024");
    let mut mp = mp_raw.clone();
    let row = first_live(&mp);
    assert_eq!(mapping_tax_code(&mp, row, 0).unwrap(), "");
    assert_eq!(mapping_tax_code(&mp, row, 4).unwrap(), "");

    assign_mapping_tax_code(&mut mp, row, 0, &tp, "01A").unwrap();
    assign_mapping_tax_code(&mut mp, row, 2, &tp, "01A").unwrap();
    assert_eq!(mapping_tax_code(&mp, row, 0).unwrap(), "01A");
    assert_eq!(mapping_tax_code(&mp, row, 2).unwrap(), "01A");
    assert_eq!(mapping_tax_code(&mp, row, 1).unwrap(), "");

    assert!(assign_mapping_tax_code(&mut mp, row, 0, &tp, "NOPE").is_err());
    assert!(mapping_tax_code(&mp, row, 0).unwrap() == "01A");
    assert!(assign_mapping_tax_code(&mut mp, row, 5, &tp, "01A").is_err());

    // An over-width code is refused by `write_cell` too.
    assert!(assign_mapping_tax_code(&mut mp, row, 0, &tp, &"1".repeat(17)).is_err());
    assert_eq!(mapping_tax_code(&mp, row, 0).unwrap(), "01A");

    clear_mapping_tax_code(&mut mp, row, 0).unwrap();
    clear_mapping_tax_code(&mut mp, row, 2).unwrap();
    assert!(
        mapping_tax_issues(&mp_raw, &tp).is_empty(),
        "hull MP tax cells are blank in the authentic prefix"
    );
    assert_eq!(
        usize_at(&table_fixture("hull2024", "tp"), "mp_slots"),
        0,
        "no MP.TAXn value in any authentic prefix"
    );
    assert_eq!(
        usize_at(&table_fixture("hull2024", "tp"), "mp_issues"),
        0
    );
}

#[test]
fn gifi_assignment_and_f55_row_gate() {
    let mut am = open("hull2024", "AM.dbf");
    let row = first_live(&am);
    let mut amounts: BTreeMap<String, f64> = BTreeMap::new();

    // No corpus maps GIFI: every `AM.GIFI` and `MP.GIFI` cell is blank.
    for (tag, _) in CORPORA {
        let t = open(tag, "AM.dbf");
        assert_eq!(
            account_gifi(&t, first_live(&t)).unwrap(),
            "",
            "{tag} AM.GIFI is blank"
        );
        assert!(
            gifi_rows(&t, &amounts).is_empty(),
            "{tag} AM.GIFI is all blank"
        );
        assert!(gifi_issues(&t, &amounts).is_empty(), "{tag}");
        let m = mp_of(tag);
        assert_eq!(
            mapping_gifi(&m, first_live(&m)).unwrap(),
            "",
            "{tag} MP.GIFI is blank"
        );
    }

    assign_account_gifi(&mut am, row, " 3620 ").unwrap();
    assert_eq!(account_gifi(&am, row).unwrap(), "3620");
    assert!(assign_account_gifi(&mut am, row, "362").is_err());
    assert_eq!(account_gifi(&am, row).unwrap(), "3620");

    // Two distinct account numbers: one balance-bearing, one zero.
    let mut seen = std::collections::BTreeSet::new();
    let mut live = None;
    let mut zero = None;
    for r in 0..am.records.len() {
        if am.is_deleted(r) {
            continue;
        }
        let ac = am
            .cell_str(r, "AC_NO")
            .unwrap_or_default()
            .trim_end()
            .to_string();
        if ac.is_empty() || !seen.insert(ac.clone()) {
            continue;
        }
        if live.is_none() {
            live = Some((r, ac));
        } else if zero.is_none() {
            zero = Some((r, ac));
            break;
        }
    }
    let (live_row, live_ac) = live.expect("balance-bearing account");
    let (zero_row, _zero_ac) = zero.expect("zero-balance account");
    amounts.insert(live_ac.clone(), 123.45);

    // The F55 gate emits the tagged, balance-bearing row.
    assign_account_gifi(&mut am, live_row, "1000").unwrap();
    let rows = gifi_rows(&am, &amounts);
    let hit = rows.iter().find(|g| g.row == live_row).expect("emitted row");
    assert_eq!(hit.code, "1000");
    assert_eq!(hit.ac_no, live_ac);
    assert_eq!(hit.amount, 123.45);

    // A zero-balance tag is flagged by the F55 audit but still stored.
    assign_account_gifi(&mut am, zero_row, "2599").unwrap();
    assert!(gifi_issues(&am, &amounts)
        .iter()
        .any(|i| i.row == zero_row && i.kind == GifiIssueKind::TaggedWithoutBalance));
    assert!(gifi_rows(&am, &amounts).iter().all(|g| g.row != zero_row));
    assert_eq!(account_gifi(&am, zero_row).unwrap(), "2599");

    clear_account_gifi(&mut am, row).unwrap();
    clear_account_gifi(&mut am, live_row).unwrap();
    clear_account_gifi(&mut am, zero_row).unwrap();
}

#[test]
fn mapping_gifi_round_trips() {
    let mut mp = mp_of("hull2024");
    let row = first_live(&mp);
    assert_eq!(mapping_gifi(&mp, row).unwrap(), "");
    assign_mapping_gifi(&mut mp, row, "1570").unwrap();
    assert_eq!(mapping_gifi(&mp, row).unwrap(), "1570");
    assert!(assign_mapping_gifi(&mut mp, row, "15x0").is_err());
    assign_mapping_gifi(&mut mp, row, "").unwrap();
    assert_eq!(mapping_gifi(&mp, row).unwrap(), "");
}

#[test]
fn user_rows_mutate_standard_rows_refuse() {
    let (mut tp, codes) = tp_of("hull2024");
    let std_row = (0..tp.records.len())
        .find(|&r| {
            !tp.is_deleted(r)
                && tp
                    .cell_str(r, "UDEF")
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
        })
        .expect("standard row");
    let user_row = (0..tp.records.len())
        .find(|&r| {
            !tp.is_deleted(r)
                && tp.cell_str(r, "UDEF").unwrap_or_default().trim() == "U"
        })
        .expect("user row");
    assert!(codes.get(&tp.cell_str(std_row, "TCODE").unwrap_or_default().trim_end().to_string()).is_some());

    let err = set_code_description(&mut tp, std_row, "NOPE").unwrap_err();
    assert!(
        err.to_string().contains("Cannot modify non-user-defined"),
        "COM-matching refusal: {err}"
    );
    assert_eq!(
        tp.cell_str(std_row, "DESCRIPT").unwrap_or_default(),
        support::open("hull2024", "TP.dbf").cell_str(std_row, "DESCRIPT").unwrap_or_default(),
        "refused write must not touch the cell"
    );

    let original = tp.cell_str(user_row, "DESCRIPT").unwrap_or_default();
    set_code_description(&mut tp, user_row, "P5-E5 test description").unwrap();
    assert_eq!(
        tp.cell_str(user_row, "DESCRIPT").unwrap_or_default().trim_end(),
        "P5-E5 test description"
    );
    set_code_percent(&mut tp, user_row, 50.0).unwrap();
    assert_eq!(tp.cell_str(user_row, "PER").unwrap_or_default().trim(), "50.00");
    set_code_description(&mut tp, user_row, original.trim_end()).unwrap();
    set_code_percent(&mut tp, user_row, 100.0).unwrap();
}

#[test]
fn rewrite_codes_is_byte_identical() {
    // Every readable prefix re-encodes byte-identically, tombstones included.
    for (tag, _) in CORPORA {
        let (t, _) = tp_of(tag);
        let before = t.encode();
        let mut out = t.clone();
        rewrite_codes(&mut out).unwrap();
        assert_eq!(out.encode(), before, "{tag} TP re-encode drift");
        assert!(tp_schema_matches(&out), "{tag}");
    }
}

#[test]
fn gifi_code_law() {
    assert_eq!(gifi_code(" 3620 ").unwrap(), "3620");
    assert!(gifi_code("362").is_err());
    assert!(gifi_code("36200").is_err());
    assert!(gifi_code("36a0").is_err());
}
