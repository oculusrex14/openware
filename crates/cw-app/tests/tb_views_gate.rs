//! P5.E4 gate: trial-balance views (period/YTD + auto year-vs-year compare).
//!
//! Golden inputs: `tests/golden/dbf/{corp2026,hull2024}/*BL.dbf` through the
//! frozen cube law (`cw_core::cube`). The gate locks:
//!
//! - TB YTD == `Cube::balance` leaf-for-leaf on the golden corpus;
//! - period column = the selected period's movement, YTD = opening + through-p;
//! - auto-compare binds `YEAR+1` only when that layer holds data;
//! - explicit/off comparison modes and variance arithmetic;
//! - group (`A`/`T`) and entity scoping, synthetic-number hiding;
//! - prior-only accounts appear with zero current amounts;
//! - axis validation and cents-rounded totals.

use std::path::PathBuf;

use cw_app::recalc::{
    append_row, BlRow, BUCKET_OPENING, BUCKET_YEAR, GROUP_ACTUAL, GROUP_TAX, OVERLAY_X,
    TYPE_NORMAL,
};
use cw_app::tb::{Compare, TbColumnKey, TbOptions, TbView, MAX_CUBE_YEAR, PERIOD_MAX};
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
        let name = path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_ascii_lowercase();
        if name.ends_with(suffix) {
            return Table::parse_path(&path).unwrap();
        }
    }
    panic!("no {suffix} in {}", dir.display());
}

fn bl(corpus: &str) -> Table {
    corpus_table(corpus, "bl.dbf")
}

fn empty_bl() -> Table {
    let mut t = bl("corp2026");
    t.records.clear();
    t
}

/// Push one typed BL row through the shipped writer (deterministic GUIDs).
#[allow(clippy::too_many_arguments)]
fn push(
    t: &mut Table,
    number: &str,
    entity: &str,
    year: i32,
    group: &str,
    bucket: &str,
    kind: &str,
    period: u8,
    id: &str,
    amt: f64,
    seed: u64,
) {
    let mut row = BlRow::movement(number, entity, kind);
    row.year = year;
    row.group = group.to_string();
    row.bucket = bucket.to_string();
    row.kind = kind.to_string();
    row.period = period;
    row.id = id.to_string();
    row.amt = amt;
    row.fx = amt;
    if bucket == BUCKET_YEAR {
        row.amt_ytd = amt;
        row.fx_ytd = amt;
    }
    append_row(t, &row, STAMP, &mut Rng::new_seeded(seed)).unwrap();
}

fn opening(t: &mut Table, number: &str, entity: &str, year: i32, group: &str, amt: f64, seed: u64) {
    push(
        t,
        number,
        entity,
        year,
        group,
        BUCKET_OPENING,
        "",
        0,
        "X",
        amt,
        seed,
    );
}

fn movement(
    t: &mut Table,
    number: &str,
    entity: &str,
    year: i32,
    group: &str,
    period: u8,
    amt: f64,
    seed: u64,
) {
    push(
        t,
        number,
        entity,
        year,
        group,
        BUCKET_YEAR,
        TYPE_NORMAL,
        period,
        OVERLAY_X,
        amt,
        seed,
    );
}

fn cents(v: f64) -> i64 {
    (v * 100.0).round() as i64
}

#[test]
fn golden_ytd_matches_the_cube_balance_law() {
    let table = bl("corp2026");
    let cube = Cube::from_table(&table).unwrap();
    let opts = TbOptions::new(0, 1);
    let view = TbView::from_cube(&cube, &opts).unwrap();

    assert_eq!(view.year, 0);
    assert_eq!(view.period, 1);
    assert_eq!(view.group, GROUP_ACTUAL);
    assert_eq!(view.prior_year, Some(1), "corp2026 has a prior layer");
    assert!(view.len() > 100, "corp chart-scale row set, got {}", view.len());

    for row in view.rows() {
        let cur = cube.balance(&row.number, 0, GROUP_ACTUAL);
        assert_eq!(
            row.ytd,
            cur.map(|v| (v * 100.0).round() / 100.0).unwrap_or(0.0),
            "{}: TB YTD must equal the frozen balance law",
            row.number
        );
        let prior = cube.balance(&row.number, 1, GROUP_ACTUAL);
        assert_eq!(
            row.prior_ytd,
            prior.map(|v| (v * 100.0).round() / 100.0).unwrap_or(0.0),
            "{}: prior YTD must equal the year-1 balance law",
            row.number
        );
        // A period column cannot exceed the through-period movement.
        assert_eq!(
            cents(row.opening) + cents(row.ytd_movement),
            cents(row.ytd),
            "{}",
            row.number
        );
        assert_eq!(
            cents(row.ytd) - cents(row.prior_ytd),
            cents(row.variance_ytd),
            "{}: variance ties",
            row.number
        );
    }

    // Anchors straight from tests/golden/cube/expectations.json.
    assert_eq!(view.row("1332-00").unwrap().ytd, 30087.0);
    assert_eq!(view.row("1332-00").unwrap().prior_ytd, 1139786.0);
    assert_eq!(view.row("1343-00").unwrap().ytd, -105788.0);

    // CW synthetics are hidden by default.
    assert!(!view.rows().iter().any(|r| r.number == "NETINC"));
    assert!(!view.rows().iter().any(|r| r.number.starts_with("GROUP__")));

    // Rows are sorted; totals are the exact sum of printed rows.
    let mut sorted = view.rows().to_vec();
    sorted.sort_by(|a, b| a.number.cmp(&b.number));
    assert_eq!(sorted, view.rows());

    let totals = view.totals();
    let ytd: i64 = view.rows().iter().map(|r| cents(r.ytd)).sum();
    let prior: i64 = view.rows().iter().map(|r| cents(r.prior_ytd)).sum();
    assert_eq!(cents(totals.ytd), ytd);
    assert_eq!(cents(totals.prior_ytd), prior);
    assert_eq!(cents(totals.variance_ytd), ytd - prior);
    assert_eq!(
        view.balance("1332-00"),
        Some(30087.0),
        "balance() is the YTD column"
    );
}

#[test]
fn period_columns_are_movement_and_ytd_is_balance() {
    let mut table = empty_bl();
    opening(&mut table, "1000", "", 0, GROUP_ACTUAL, 1000.0, 1);
    movement(&mut table, "1000", "", 0, GROUP_ACTUAL, 1, 100.0, 2);
    movement(&mut table, "1000", "", 0, GROUP_ACTUAL, 2, 50.0, 3);
    movement(&mut table, "1000", "", 0, GROUP_ACTUAL, 3, -25.0, 4);
    let cube = Cube::from_table(&table).unwrap();

    let p1 = TbView::from_cube(&cube, &TbOptions::new(0, 1).compare(Compare::Off)).unwrap();
    let r1 = p1.row("1000").unwrap();
    assert_eq!(
        (r1.opening, r1.period, r1.ytd_movement, r1.ytd),
        (1000.0, 100.0, 100.0, 1100.0)
    );
    assert!(!p1.has_prior());
    assert_eq!(p1.columns().len(), 4);
    assert_eq!(
        p1.columns().iter().map(|c| c.key).collect::<Vec<_>>(),
        vec![
            TbColumnKey::Opening,
            TbColumnKey::Period,
            TbColumnKey::YtdMovement,
            TbColumnKey::Ytd
        ]
    );

    let p2 = TbView::from_cube(&cube, &TbOptions::new(0, 2).compare(Compare::Off)).unwrap();
    let r2 = p2.row("1000").unwrap();
    assert_eq!(
        (r2.opening, r2.period, r2.ytd_movement, r2.ytd),
        (1000.0, 50.0, 150.0, 1150.0),
        "future periods are excluded from YTD"
    );

    let p3 = TbView::from_cube(&cube, &TbOptions::new(0, 3).compare(Compare::Off)).unwrap();
    let r3 = p3.row("1000").unwrap();
    assert_eq!(
        (r3.period, r3.ytd_movement, r3.ytd),
        (-25.0, 125.0, 1125.0)
    );
    assert_eq!(p3.totals().ytd, 1125.0);
}

#[test]
fn auto_compare_binds_next_layer_only_when_present() {
    let table = bl("corp2026");
    let cube = Cube::from_table(&table).unwrap();

    let auto = TbView::from_cube(&cube, &TbOptions::new(0, 1)).unwrap();
    assert_eq!(auto.prior_year, Some(1));
    assert_eq!(auto.columns().len(), 9);
    assert_eq!(
        auto.columns().iter().map(|c| c.key).collect::<Vec<_>>(),
        vec![
            TbColumnKey::Opening,
            TbColumnKey::Period,
            TbColumnKey::YtdMovement,
            TbColumnKey::Ytd,
            TbColumnKey::PriorOpening,
            TbColumnKey::PriorPeriod,
            TbColumnKey::PriorYtd,
            TbColumnKey::VariancePeriod,
            TbColumnKey::VarianceYtd,
        ]
    );
    assert!(auto.columns().iter().any(|c| c.label.contains("(Y1)")));

    let off = TbView::from_cube(&cube, &TbOptions::new(0, 1).compare(Compare::Off)).unwrap();
    assert_eq!(off.prior_year, None);
    assert!(off
        .rows()
        .iter()
        .all(|r| r.prior_ytd == 0.0 && r.variance_ytd == 0.0 && r.prior_opening == 0.0));

    let explicit2 = TbView::from_cube(&cube, &TbOptions::new(0, 1).compare(Compare::Year(2)))
        .unwrap();
    assert_eq!(explicit2.prior_year, Some(2));
    assert_eq!(
        explicit2.row("1332-00").unwrap().prior_ytd,
        cube.balance("1332-00", 2, GROUP_ACTUAL).unwrap(),
        "explicit comparison reads the requested layer"
    );

    // Current layer with no prior data -> auto yields no comparative columns.
    let synthetic = {
        let mut t = empty_bl();
        opening(&mut t, "1000", "", 0, GROUP_ACTUAL, 10.0, 5);
        Cube::from_table(&t).unwrap()
    };
    let none = TbView::from_cube(&synthetic, &TbOptions::new(0, 1)).unwrap();
    assert_eq!(none.prior_year, None);
    assert_eq!(none.columns().len(), 4);

    // Highest year cannot auto-compare (no layer above the F28 window).
    let top = TbView::from_cube(&cube, &TbOptions::new(MAX_CUBE_YEAR, 1)).unwrap();
    assert_eq!(top.prior_year, None);
    assert!(top.is_empty());
}

#[test]
fn group_entity_scope_and_synthetic_gate() {
    let mut table = empty_bl();
    // Same number on two actual entities plus one tax-basis row.
    opening(&mut table, "9000", "", 0, GROUP_ACTUAL, 100.0, 10);
    movement(&mut table, "9000", "", 0, GROUP_ACTUAL, 1, 10.0, 11);
    opening(&mut table, "9000", "E1", 0, GROUP_ACTUAL, 200.0, 12);
    movement(&mut table, "9000", "E1", 0, GROUP_ACTUAL, 1, 20.0, 13);
    opening(&mut table, "9000", "", 0, GROUP_TAX, 500.0, 14);
    movement(&mut table, "9000", "", 0, GROUP_TAX, 1, 50.0, 15);
    // Prior-year-only account.
    opening(&mut table, "8000", "", 1, GROUP_ACTUAL, 300.0, 16);
    movement(&mut table, "8000", "", 1, GROUP_ACTUAL, 1, 30.0, 17);
    // CW marker that must not leak into a TB.
    opening(&mut table, "NETINC", "", 0, GROUP_ACTUAL, 999.0, 18);
    let cube = Cube::from_table(&table).unwrap();

    let all = TbView::from_cube(&cube, &TbOptions::new(0, 1)).unwrap();
    assert_eq!(all.prior_year, Some(1));
    let r = all.row("9000").unwrap();
    assert_eq!(
        (r.opening, r.period, r.ytd, r.prior_ytd),
        (300.0, 30.0, 330.0, 0.0),
        "actual entities aggregate"
    );
    let prior_only = all.row("8000").unwrap();
    assert_eq!(
        (
            prior_only.opening,
            prior_only.ytd,
            prior_only.prior_opening,
            prior_only.prior_ytd,
            prior_only.variance_ytd
        ),
        (0.0, 0.0, 300.0, 330.0, -330.0),
        "prior-only account is carried with a zero current side"
    );
    assert!(all.row("NETINC").is_none());
    assert_eq!(all.totals().variance_ytd, 0.0);

    let e1 = TbView::from_cube(
        &cube,
        &TbOptions::new(0, 1).entity("E1"),
    )
    .unwrap();
    assert_eq!(e1.entity.as_deref(), Some("E1"));
    assert_eq!(e1.prior_year, None, "no prior data for entity E1");
    let r = e1.row("9000").unwrap();
    assert_eq!((r.opening, r.period, r.ytd), (200.0, 20.0, 220.0));
    assert!(e1.row("8000").is_none(), "entity filter applies to prior too");

    let blank = TbView::from_cube(&cube, &TbOptions::new(0, 1).entity("")).unwrap();
    let r = blank.row("9000").unwrap();
    assert_eq!((r.opening, r.ytd), (100.0, 110.0));

    let tax = TbView::from_cube(&cube, &TbOptions::new(0, 1).tax()).unwrap();
    assert_eq!(tax.group, GROUP_TAX);
    assert_eq!(tax.prior_year, None, "no tax-basis prior layer");
    let r = tax.row("9000").unwrap();
    assert_eq!((r.opening, r.period, r.ytd), (500.0, 50.0, 550.0));
    assert!(tax.row("8000").is_none(), "actual rows never leak into tax");

    let with_marker = TbView::from_cube(
        &cube,
        &TbOptions::new(0, 1).with_synthetics(),
    )
    .unwrap();
    assert_eq!(with_marker.row("NETINC").unwrap().opening, 999.0);
}

#[test]
fn opening_layer_uses_x_precedence_and_skips_p_snapshots() {
    let mut table = empty_bl();
    // O and X pair with deliberately different values: X is authoritative.
    push(
        &mut table,
        "1000",
        "",
        0,
        GROUP_ACTUAL,
        BUCKET_OPENING,
        "",
        0,
        "O",
        10.0,
        20,
    );
    opening(&mut table, "1000", "", 0, GROUP_ACTUAL, 40.0, 21);
    // A P snapshot must never be counted.
    push(
        &mut table,
        "1000",
        "",
        0,
        GROUP_ACTUAL,
        BUCKET_YEAR,
        TYPE_NORMAL,
        1,
        "P",
        500.0,
        22,
    );
    movement(&mut table, "1000", "", 0, GROUP_ACTUAL, 1, 5.0, 23);
    let cube = Cube::from_table(&table).unwrap();

    let view = TbView::from_cube(&cube, &TbOptions::new(0, 1).compare(Compare::Off)).unwrap();
    let r = view.row("1000").unwrap();
    assert_eq!(
        (r.opening, r.period, r.ytd),
        (40.0, 5.0, 45.0),
        "X opening + non-P movement only"
    );
    assert_eq!(r.ytd, cube.balance("1000", 0, GROUP_ACTUAL).unwrap());
}

#[test]
fn views_reject_out_of_window_axes() {
    let table = bl("corp2026");
    let cube = Cube::from_table(&table).unwrap();

    for (year, period) in [(MAX_CUBE_YEAR + 1, 1), (-1, 1), (0, 0), (0, PERIOD_MAX + 1)] {
        let err = TbView::from_cube(&cube, &TbOptions::new(year, period)).unwrap_err();
        assert!(
            err.to_string().contains("TB"),
            "year={year} period={period}: {err}"
        );
    }
    let bad_group = TbOptions {
        group: "X".into(),
        ..TbOptions::new(0, 1)
    };
    assert!(TbView::from_cube(&cube, &bad_group).is_err());

    let same_year = TbOptions::new(1, 1).compare(Compare::Year(1));
    assert!(TbView::from_cube(&cube, &same_year).is_err());
}

#[test]
fn from_table_matches_from_cube() {
    for corpus in ["corp2026", "hull2024"] {
        let table = bl(corpus);
        let cube = Cube::from_table(&table).unwrap();
        let opts = TbOptions::new(0, 1);
        let a = TbView::from_table(&table, &opts).unwrap();
        let b = TbView::from_cube(&cube, &opts).unwrap();
        assert_eq!(a.rows(), b.rows(), "{corpus}");
        assert_eq!(a.totals(), b.totals(), "{corpus}");
        assert_eq!(a.prior_year, Some(1), "{corpus}");
        assert!(!a.is_empty(), "{corpus}");
    }
}

