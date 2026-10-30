//! P5.E3b gate: `AJE`/`EJE`/`RJE` balance flow + `BL` cube propagation (F35).
//!
//! Golden inputs: `tests/golden/dbf/{corp2026,hull2024,mwgh2025}/*{GL,BL}.dbf`.
//! The gate locks:
//!
//! - family routing for all 13 `CWAdjustingEntryType` classes;
//! - balanced-entry propagation to `BUCKET='Y'` rows (`ID='X'`, `TYPE='N'`);
//! - `RJE` → `TYPE='R'` mirrored into `GROUP='A'`/`'T'`;
//! - the Y/R pruning law (zero reclass rows pruned, non-zero kept);
//! - Repost cosmetics (`AMT_DIST`/`FX_DIST` normalization, `O`-layer `CWDATE`
//!   touch only, cumulative `AMT_YTD`);
//! - atomic refusal of an unbalanced journal;
//! - idempotency + `O`-layer immutability on all three golden corpora.

use std::collections::BTreeMap;
use std::path::PathBuf;

use cw_app::adjustments::{AdjustmentEntry, AdjustmentLine, Adjustments, EntryType};
use cw_app::recalc::{
    append_row, find_row, recalc, BlKey, BlRow, EntryFamily, RecalcOptions, BUCKET_OPENING,
    BUCKET_YEAR, GROUP_ACTUAL, GROUP_TAX, OVERLAY_X, TYPE_NORMAL, TYPE_RECLASS,
};
use cw_core::cube::Cube;
use cw_core::dbf::Table;
use cw_core::guid::Rng;

const STAMP: &str = "20260102030405";

fn corpus_dir(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("..");
    p.push("..");
    p.push("tests");
    p.push("golden");
    p.push("dbf");
    p.push(name);
    p
}

fn corpus_table(corpus: &str, suffix: &str) -> Table {
    let dir = corpus_dir(corpus);
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_ascii_lowercase();
        if name.ends_with(suffix) {
            return Table::parse_path(&path).unwrap();
        }
    }
    panic!("no {suffix} in {}", dir.display());
}

fn gl(corpus: &str) -> Table {
    corpus_table(corpus, "gl.dbf")
}

fn bl(corpus: &str) -> Table {
    corpus_table(corpus, "bl.dbf")
}

fn empty_bl() -> Table {
    let mut t = bl("corp2026");
    t.records.clear();
    t
}

fn opts() -> RecalcOptions {
    RecalcOptions::fixture(STAMP, 0x5EED_1234)
}

/// Decode synthetic entries into an `Adjustments` journal using the golden GL
/// layout (rows are real encoded `GlRow`s, so the decode path is the shipped one).
fn journal(entries: &[AdjustmentEntry]) -> Adjustments {
    let mut t = gl("corp2026");
    t.records = entries
        .iter()
        .flat_map(|e| e.gl_rows.iter().map(|r| r.encode().unwrap()))
        .collect();
    Adjustments::from_table(&t).unwrap()
}

fn entry(
    id: &str,
    class: EntryType,
    period: Option<u8>,
    lines: &[(&str, f64)],
) -> AdjustmentEntry {
    let lines: Vec<AdjustmentLine> = lines
        .iter()
        .map(|(acct, amt)| AdjustmentLine::new(*acct, *amt))
        .collect();
    AdjustmentEntry::render(id, "REF", "recalc gate", class, "20261231", period, &lines).unwrap()
}

fn cell(t: &Table, r: usize, name: &str) -> String {
    t.cell_str(r, name).unwrap().trim().to_string()
}

fn live_rows(t: &Table) -> Vec<usize> {
    (0..t.count()).filter(|r| !t.is_deleted(*r)).collect()
}

fn blank_cell(t: &mut Table, r: usize, name: &str) {
    let offs = t.offsets();
    let i = t.field_index(name).unwrap();
    let width = t.fields[i].len as usize;
    for b in &mut t.records[r][offs[i]..offs[i] + width] {
        *b = b' ';
    }
}

fn o_balances(cube: &Cube) -> BTreeMap<(String, String), f64> {
    let mut pref: BTreeMap<(String, String), (bool, f64)> = BTreeMap::new();
    for row in cube.rows() {
        if row.bucket != BUCKET_OPENING || row.year != 0 || row.id == "P" {
            continue;
        }
        let slot = pref
            .entry((row.number.clone(), row.group.clone()))
            .or_insert((false, row.amt));
        if row.id == "X" {
            *slot = (true, row.amt);
        } else if !slot.0 {
            slot.1 = row.amt;
        }
    }
    pref.into_iter().map(|(k, (_, v))| (k, v)).collect()
}

#[test]
fn family_routing_covers_all_entry_classes() {
    for class in EntryType::ALL {
        let family = EntryFamily::of(class);
        let expected = match class {
            EntryType::Reclassifying => EntryFamily::Rje,
            EntryType::Eliminating => EntryFamily::Eje,
            _ => EntryFamily::Aje,
        };
        assert_eq!(family, expected, "{class:?}");
        assert_eq!(family.movement() == TYPE_RECLASS, family == EntryFamily::Rje);
    }
    assert_eq!(EntryFamily::Aje.am_column(), "AJE_BAL");
    assert_eq!(EntryFamily::Eje.am_column(), "EJE_BAL");
    assert_eq!(EntryFamily::Rje.am_column(), "RJE_BAL");
    assert_eq!(EntryFamily::Rje.groups(), &[GROUP_ACTUAL, GROUP_TAX]);
    assert_eq!(EntryFamily::Eje.groups(), &[GROUP_ACTUAL]);
}

#[test]
fn aje_propagates_y_rows_and_is_idempotent() {
    let j = journal(&[entry(
        "AJE #1",
        EntryType::Normal,
        Some(1),
        &[("1000-00", 100.0), ("2000-00", -100.0)],
    )]);
    let mut bl = empty_bl();
    let report = recalc(&mut bl, &j, &opts()).unwrap();

    assert_eq!(report.entries, 1);
    assert_eq!(report.lines, 2);
    let aje = report.family(EntryFamily::Aje);
    assert_eq!(aje.entries, 1);
    assert_eq!(aje.debits, 100.0);
    assert_eq!(aje.credits, 100.0);
    assert_eq!(aje.net, 0.0);
    assert_eq!(report.net(), 0.0);
    assert_eq!(report.y_rows_written, 2);
    assert_eq!(report.y_rows_updated, 0);
    assert_eq!(report.y_rows_tombstoned, 0);

    let live = live_rows(&bl);
    assert_eq!(live.len(), 2);
    for r in &live {
        assert_eq!(cell(&bl, *r, "ID"), OVERLAY_X);
        assert_eq!(cell(&bl, *r, "GROUP"), GROUP_ACTUAL);
        assert_eq!(cell(&bl, *r, "BUCKET"), BUCKET_YEAR);
        assert_eq!(cell(&bl, *r, "TYPE"), TYPE_NORMAL);
        assert_eq!(cell(&bl, *r, "YEAR"), "0");
        assert_eq!(cell(&bl, *r, "PERIOD"), "1");
        assert_eq!(cell(&bl, *r, "CWDATE"), STAMP);
        assert_eq!(cell(&bl, *r, "AMT_DIST"), "0.00");
        assert_eq!(cell(&bl, *r, "FX_DIST"), "0.00");
    }
    let amounts: BTreeMap<String, f64> = live
        .iter()
        .map(|r| (cell(&bl, *r, "NUMBER"), cell(&bl, *r, "AMT").parse().unwrap()))
        .collect();
    assert_eq!(amounts["1000-00"], 100.0);
    assert_eq!(amounts["2000-00"], -100.0);

    let cube = Cube::from_table(&bl).unwrap();
    assert_eq!(cube.balance("1000-00", 0, "A"), Some(100.0));
    assert_eq!(cube.balance("2000-00", 0, "A"), Some(-100.0));

    // Idempotent: reconcile-in-place, no new rows, byte-identical emission.
    let first = bl.encode();
    let again = recalc(&mut bl, &j, &opts()).unwrap();
    assert_eq!(again.y_rows_written, 0);
    assert_eq!(again.y_rows_updated, 2);
    assert_eq!(again.y_rows_tombstoned, 0);
    assert_eq!(bl.encode(), first);
}

#[test]
fn eje_and_rje_flow_with_reclass_mirroring() {
    let j = journal(&[
        entry(
            "EJE #1",
            EntryType::Eliminating,
            Some(2),
            &[("3000-00", 250.0), ("4000-00", -250.0)],
        ),
        entry(
            "RJE #1",
            EntryType::Reclassifying,
            Some(3),
            &[("5000-00", 75.0), ("6000-00", -75.0)],
        ),
    ]);
    let mut bl = empty_bl();
    let report = recalc(&mut bl, &j, &opts()).unwrap();

    assert_eq!(report.family(EntryFamily::Eje).net, 0.0);
    assert_eq!(report.family(EntryFamily::Rje).net, 0.0);
    assert_eq!(report.family(EntryFamily::Eje).entries, 1);
    assert_eq!(report.family(EntryFamily::Rje).entries, 1);

    let live = live_rows(&bl);
    // EJE: 2 × (Y,N,A); RJE: 2 accounts × (A,T) = 4 × (Y,R)
    assert_eq!(live.len(), 6);
    let mut n_rows = 0;
    let mut r_actual = 0;
    let mut r_tax = 0;
    for r in &live {
        match (cell(&bl, *r, "TYPE").as_str(), cell(&bl, *r, "GROUP").as_str()) {
            ("N", "A") => n_rows += 1,
            ("R", "A") => r_actual += 1,
            ("R", "T") => r_tax += 1,
            other => panic!("unexpected row class {other:?}"),
        }
        assert_eq!(cell(&bl, *r, "PERIOD"), if cell(&bl, *r, "TYPE") == "N" { "2" } else { "3" });
    }
    assert_eq!((n_rows, r_actual, r_tax), (2, 2, 2));

    let cube = Cube::from_table(&bl).unwrap();
    assert_eq!(cube.balance("5000-00", 0, "A"), Some(75.0));
    assert_eq!(cube.balance("5000-00", 0, "T"), Some(75.0));
    assert_eq!(cube.balance("3000-00", 0, "A"), Some(250.0));
    assert_eq!(cube.balance("3000-00", 0, "T"), None);

    // Flow rows are per family/group/period.
    assert_eq!(report.flows.len(), 6);
    assert!(report
        .flows
        .iter()
        .any(|f| f.family == EntryFamily::Rje && f.group == GROUP_TAX && f.period == 3));
}

#[test]
fn zero_reclass_rows_are_pruned() {
    let j = journal(&[entry(
        "RJE #2",
        EntryType::Reclassifying,
        Some(1),
        &[("7000-00", 100.0), ("7000-00", -100.0)],
    )]);
    let mut bl = empty_bl();

    // Pre-existing zero + non-zero Y/R rows outside the touched number set.
    let mut zero = BlRow::movement("9999-00", "", TYPE_RECLASS);
    zero.amt = 0.0;
    append_row(&mut bl, &zero, STAMP, &mut Rng::new_seeded(1)).unwrap();
    let mut keep = BlRow::movement("9998-00", "", TYPE_RECLASS);
    keep.amt = 12.0;
    append_row(&mut bl, &keep, STAMP, &mut Rng::new_seeded(2)).unwrap();

    let report = recalc(&mut bl, &j, &opts()).unwrap();
    assert_eq!(
        report.zero_reclass_pruned, 3,
        "two mirrored zero aggregates + one stale row"
    );
    assert_eq!(live_rows(&bl).len(), 1);
    assert!(find_row(&bl, &zero.key()).is_none(), "zero Y/R tombstoned");
    assert!(find_row(&bl, &keep.key()).is_some());
    // The zero aggregate for 7000-00 was not materialized.
    assert!(live_rows(&bl).iter().all(|r| cell(&bl, *r, "NUMBER") != "7000-00"));
}

#[test]
fn dist_normalization_and_o_layer_cwdate_touch() {
    let mut bl = empty_bl();
    let mut rng = Rng::new_seeded(9);
    let mut o = BlRow::movement("1100-00", "", "");
    o.bucket = BUCKET_OPENING.to_string();
    o.kind = String::new();
    o.period = 0;
    o.amt = 500.0;
    o.fx = 500.0;
    let o_row = append_row(&mut bl, &o, "20250101000000", &mut rng).unwrap();

    let mut y = BlRow::movement("1100-00", "", TYPE_NORMAL);
    y.amt = 40.0;
    y.amt_ytd = 40.0;
    let y_row = append_row(&mut bl, &y, "20250101000000", &mut rng).unwrap();
    blank_cell(&mut bl, y_row, "AMT_DIST");
    blank_cell(&mut bl, y_row, "FX_DIST");

    // An untouched number exercises the Repost normalization pass directly.
    let mut untouched = BlRow::movement("1101-00", "", TYPE_NORMAL);
    untouched.amt = 7.0;
    untouched.amt_ytd = 7.0;
    let untouched_row = append_row(&mut bl, &untouched, "20250101000000", &mut rng).unwrap();
    blank_cell(&mut bl, untouched_row, "AMT_DIST");
    blank_cell(&mut bl, untouched_row, "FX_DIST");

    let j = journal(&[entry(
        "AJE #2",
        EntryType::Normal,
        Some(1),
        &[("1100-00", 10.0), ("2100-00", -10.0)],
    )]);
    let report = recalc(&mut bl, &j, &opts()).unwrap();

    assert_eq!(report.o_rows_touched, 1);
    assert_eq!(cell(&bl, o_row, "CWDATE"), STAMP);
    assert_eq!(cell(&bl, o_row, "AMT"), "500.00", "O values are never re-valued");
    assert_eq!(cell(&bl, y_row, "AMT_DIST"), "0.00");
    assert_eq!(cell(&bl, y_row, "FX_DIST"), "0.00");
    assert_eq!(cell(&bl, y_row, "AMT"), "10.00", "derived Y row re-derived from journal");
    assert_eq!(cell(&bl, untouched_row, "AMT_DIST"), "0.00");
    assert_eq!(cell(&bl, untouched_row, "FX_DIST"), "0.00");
    assert_eq!(cell(&bl, untouched_row, "AMT"), "7.00", "untouched value preserved");
    assert!(report.dist_normalized >= 1);
}

#[test]
fn multi_period_ytd_is_cumulative() {
    let j = journal(&[
        entry(
            "AJE #3",
            EntryType::Normal,
            Some(1),
            &[("8000-00", 10.0), ("8001-00", -10.0)],
        ),
        entry(
            "AJE #4",
            EntryType::Normal,
            Some(2),
            &[("8000-00", 5.0), ("8001-00", -5.0)],
        ),
    ]);
    let mut bl = empty_bl();
    recalc(&mut bl, &j, &opts()).unwrap();

    let mut ytd: Vec<(u8, f64, f64)> = live_rows(&bl)
        .into_iter()
        .filter(|r| cell(&bl, *r, "NUMBER") == "8000-00")
        .map(|r| {
            (
                cell(&bl, r, "PERIOD").parse().unwrap(),
                cell(&bl, r, "AMT").parse().unwrap(),
                cell(&bl, r, "AMT_YTD").parse().unwrap(),
            )
        })
        .collect();
    ytd.sort_by_key(|(p, _, _)| *p);
    assert_eq!(ytd, vec![(1, 10.0, 10.0), (2, 5.0, 15.0)]);
}

#[test]
fn unbalanced_journal_is_refused_atomically() {
    let good = entry(
        "AJE #5",
        EntryType::Normal,
        Some(1),
        &[("1200-00", 30.0), ("2200-00", -30.0)],
    );
    let mut bad = good.clone();
    bad.gl_rows[1].set("NET", "31.00").unwrap();
    let j = journal(&[bad]);

    let mut bl = empty_bl();
    let before = bl.encode();
    let err = recalc(&mut bl, &j, &opts()).unwrap_err().to_string();
    assert!(err.contains("unbalanced"), "{err}");
    assert_eq!(bl.encode(), before, "failed recalc must not mutate the cube");
}

#[test]
fn golden_corpora_propagate_and_stay_idempotent() {
    for corpus in ["corp2026", "hull2024", "mwgh2025"] {
        let j = Adjustments::from_table(&gl(corpus)).unwrap();
        assert!(!j.is_empty(), "{corpus}: journal decoded");
        assert!(j.unbalanced().next().is_none(), "{corpus}: corpus journal balanced");

        let mut table = bl(corpus);
        let before_cube = Cube::from_table(&table).unwrap();
        let before_o = o_balances(&before_cube);

        let report = recalc(&mut table, &j, &opts()).unwrap();
        assert_eq!(report.entries, j.len(), "{corpus}: every entry routed");
        assert!(
            report.families.iter().map(|f| f.entries).sum::<usize>() == j.len(),
            "{corpus}: tri-ledger partition"
        );
        assert!(report.net().abs() < 1e-9, "{corpus}: journal nets to zero");

        let after_cube = Cube::from_table(&table).unwrap();
        assert_eq!(o_balances(&after_cube), before_o, "{corpus}: O layer immutable");

        // Y/R pruning + repost cosmetics.
        for row in after_cube.rows() {
            if row.bucket == BUCKET_YEAR && row.kind == TYPE_RECLASS {
                assert!(row.amt.abs() >= 0.005, "{corpus}: zero Y/R pruned");
            }
            if row.bucket == BUCKET_YEAR && row.year == 0 {
                assert!(row.amt_dist.is_finite(), "{corpus}: AMT_DIST normalized");
                assert!(row.fx_dist.is_finite(), "{corpus}: FX_DIST normalized");
            }
        }
        for r in live_rows(&table) {
            if cell(&table, r, "BUCKET") == BUCKET_YEAR && cell(&table, r, "YEAR") == "0" {
                for name in ["AMT_DIST", "FX_DIST"] {
                    let raw = cell(&table, r, name);
                    assert!(!raw.is_empty() && raw != ".", "{corpus}: {name} blank at row {r}");
                }
            }
        }

        // Every non-pruned account flow has a live cube row.
        for flow in &report.flows {
            if flow.kind == TYPE_RECLASS && flow.amount.abs() < 0.005 {
                continue;
            }
            let key = BlKey {
                number: flow.number.clone(),
                entity: flow.entity.clone(),
                year: 0,
                bucket: BUCKET_YEAR.to_string(),
                kind: flow.kind.clone(),
                period: flow.period,
                id: OVERLAY_X.to_string(),
                group: flow.group.clone(),
            };
            assert!(find_row(&table, &key).is_some(), "{corpus}: missing row {key:?}");
        }

        // Idempotent over real journals.
        let first = table.encode();
        let again = recalc(&mut table, &j, &opts()).unwrap();
        assert_eq!(again.y_rows_written, 0, "{corpus}: no growth on rerun");
        assert_eq!(again.y_rows_tombstoned, 0, "{corpus}: no churn on rerun");
        assert_eq!(table.encode(), first, "{corpus}: byte-identical rerun");
        assert_eq!(
            Table::parse(&first).unwrap().encode(),
            first,
            "{corpus}: emitted BL re-parses"
        );
    }
}

