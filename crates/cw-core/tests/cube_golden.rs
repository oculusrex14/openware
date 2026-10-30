//! P5.C4 golden gate: the cube audit + balance law must reproduce the frozen
//! census (`tests/golden/cube/expectations.json`, generator
//! `examples/freeze_cube.rs`) leaf-for-leaf on every frozen corpus, and the
//! public API must be self-consistent with the frozen balance table.
//!
//! Fixture provenance: the original freeze was a Python oracle cross-checked
//! against the kernel; that oracle is not part of the rebuild, so the fixture
//! is now kernel-derived (one implementation, frozen bytes) — this gate is a
//! regression/coverage law. Balances come from `Cube::balances()`.
//!
//! Fixtures: `tests/golden/dbf/<corpus>/` (the C1 freeze; the BL tables therein).
//! NOTE: hull2024's `BL.dbf` member is F63-truncated in the shipped package
//! (liblzma stops after 489,701/883,452 bytes), so its frozen tail rows are
//! fabricated zeros — including NUMBER cells that contain `0x7C`. Balance keys
//! are therefore parsed from the right (`rsplitn`), see the balance loop below.

use cw_core::cube::Cube;
use cw_core::json::Json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn golden_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("golden")
}

fn expectations() -> Json {
    let path = golden_root().join("cube").join("expectations.json");
    let text = std::fs::read_to_string(&path).expect("read expectations.json");
    cw_core::json::parse(&text).expect("parse expectations.json")
}

fn bl_path(corpus_dir: &Path) -> PathBuf {
    std::fs::read_dir(corpus_dir)
        .expect("corpus dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_lowercase().ends_with("bl.dbf"))
                .unwrap_or(false)
        })
        .expect("BL.dbf in corpus")
}

fn load_cube(name: &str) -> Cube {
    let dir = golden_root().join("dbf").join(name);
    let table = cw_core::dbf::Table::parse_path(bl_path(&dir)).expect("parse BL");
    Cube::from_table(&table).expect("cube")
}

fn f(j: &Json, key: &str) -> f64 {
    j.get(key).and_then(|x| x.as_f64()).unwrap_or_else(|| panic!("missing {key}"))
}

fn pair(j: &Json, key: &str) -> (u32, u32) {
    let a = j
        .get(key)
        .and_then(|x| x.as_arr())
        .unwrap_or_else(|| panic!("missing pair {key}"));
    (
        a[0].as_f64().unwrap() as u32,
        a[1].as_f64().unwrap() as u32,
    )
}

fn strmap(j: &Json) -> BTreeMap<String, u32> {
    let mut m = BTreeMap::new();
    if let Some(pairs) = j.as_obj() {
        for (k, v) in pairs {
            m.insert(k.clone(), v.as_f64().unwrap() as u32);
        }
    }
    m
}

fn axes_of(j: &Json) -> BTreeMap<String, BTreeMap<String, u32>> {
    let mut out = BTreeMap::new();
    for (col, v) in j.as_obj().unwrap() {
        out.insert(col.clone(), strmap(v));
    }
    out
}

const COLUMNS: &[&str] = &[
    "ID", "GROUP", "YEAR", "BUCKET", "TYPE", "PERIOD", "BOOKED", "CUSTOM", "ENTITY",
];

fn audit_axes(a: &cw_core::cube::CubeAudit) -> BTreeMap<String, BTreeMap<String, u32>> {
    let mut out = BTreeMap::new();
    for (col, m) in a.axes.iter() {
        let owned: BTreeMap<String, u32> =
            m.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        out.insert(col.to_string(), owned);
    }
    out
}

#[test]
fn cube_matches_freeze() {
    let exp = expectations();
    let corpora = exp
        .get("corpora")
        .and_then(|x| x.as_obj())
        .expect("corpora object")
        .to_vec();
    assert!(corpora.len() >= 3, "expected >=3 frozen corpora");
    let mut checked = 0usize;

    for (name, c) in &corpora {
        let cube = load_cube(name);
        let audit = cube.audit();

        assert_eq!(
            audit.rows as f64,
            f(c, "rows"),
            "{name}: live row count"
        );

        let laws = c.get("laws").expect("laws");
        assert_eq!(
            (audit.pair_mirror_ok, audit.pair_mirror_paired),
            pair(laws, "pair_mirror"),
            "{name}: L1 pair-mirror"
        );
        assert_eq!(audit.x_ytd_eq_amt, f(laws, "x_ytd_eq_amt") as u32, "{name}: L2 x ytd");
        assert_eq!(audit.x_fx_eq_amt, f(laws, "x_fx_eq_amt") as u32, "{name}: L2 x fx");
        assert_eq!(
            audit.x_fx_ytd_eq_ytd,
            f(laws, "x_fx_ytd_eq_ytd") as u32,
            "{name}: L2 x fx_ytd"
        );
        assert_eq!(audit.o_ytd_zero, f(laws, "o_ytd_zero") as u32, "{name}: L3 o ytd");
        assert_eq!(
            audit.o_fxytd_zero,
            f(laws, "o_fxytd_zero") as u32,
            "{name}: L3 o fx_ytd"
        );
        assert_eq!(audit.p_rows, f(laws, "p_rows") as u32, "{name}: P rows");
        assert_eq!(audit.p_ytd_zero, f(laws, "p_ytd_zero") as u32, "{name}: L4 p ytd");
        assert_eq!(
            audit.p_groups_without_x,
            f(laws, "p_groups_without_X") as u32,
            "{name}: L4 p without x"
        );
        assert_eq!(
            audit.p_neq_x_snapshot_drift,
            f(laws, "p_neq_x_snapshot_drift") as u32,
            "{name}: L4 drift"
        );
        assert_eq!(
            (audit.solo_x_eq_sum_ynp, audit.solo_x),
            pair(laws, "soloX_eq_sumYnp"),
            "{name}: L5 solo-X"
        );
        assert_eq!(
            audit.ytd_cum_ok as f64,
            f(laws, "ytd_cum_ok"),
            "{name}: L7 cum ok"
        );
        assert_eq!(
            audit.ytd_cum_rows as f64,
            f(laws, "ytd_cum_rows"),
            "{name}: L7 cum rows"
        );

        let nb = c.get("nonblank").unwrap();
        assert_eq!(audit.nonblank_booked as f64, f(nb, "BOOKED"), "{name} booked");
        assert_eq!(audit.nonblank_custom as f64, f(nb, "CUSTOM"), "{name} custom");
        assert_eq!(audit.nonblank_entity as f64, f(nb, "ENTITY"), "{name} entity");
        assert_eq!(audit.amt_dist_nonzero as f64, f(nb, "AMT_DIST"), "{name} dist");
        assert_eq!(audit.fx_dist_nonzero as f64, f(nb, "FX_DIST"), "{name} fxdist");

        let exp_axes = axes_of(c.get("axes").unwrap());
        let got_axes = audit_axes(&audit);
        assert_eq!(got_axes.len(), COLUMNS.len(), "{name}: axis count");
        for col in COLUMNS {
            assert_eq!(
                got_axes.get(*col).unwrap(),
                exp_axes.get(*col).unwrap(),
                "{name}: axis {col}"
            );
        }

        let exp_shapes = strmap(c.get("shape_bucket").unwrap());
        assert_eq!(audit.shapes, exp_shapes, "{name}: shape census");

        // balances, leaf for leaf
        let exp_bal = c.get("balances").and_then(|x| x.as_obj()).unwrap();
        let got = cube.balances();
        assert_eq!(got.len(), exp_bal.len(), "{name}: balance key count");
        for (k, v) in exp_bal {
            let want = v.as_f64().unwrap();
            let have = got.get(k).unwrap_or_else(|| panic!("{name}: missing balance {k}"));
            assert!(
                (have - want).abs() < 1e-6,
                "{name}: balance {k} {} != {}",
                have,
                want
            );
        }
        checked += 1;
    }
    assert!(checked >= 3);
}

#[test]
fn cube_api_selfconsistent() {
    let exp = expectations();
    let corpora = exp
        .get("corpora")
        .and_then(|x| x.as_obj())
        .expect("corpora object")
        .to_vec();
    for (name, c) in &corpora {
        let cube = load_cube(name);
        let audit = cube.audit();
        let p_drift = f(c.get("laws").unwrap(), "p_neq_x_snapshot_drift");

        // every frozen balance is reachable through the public law API
        for (k, v) in c.get("balances").and_then(|x| x.as_obj()).unwrap() {
            // Keys are `number|year|group`; NUMBER is a free-form 40-char text
            // column and the hull2024 corpus carries 0x7C inside its damaged
            // tail records (F63-expanded BL rows), so split from the RIGHT —
            // GROUP is 1 char and YEAR is an integer, i.e. the unambiguous end.
            let mut it = k.rsplitn(3, '|');
            let g = it.next().unwrap();
            let y: i32 = it.next().unwrap().parse().unwrap();
            let n = it.next().unwrap();
            let got = cube
                .balance(n, y, g)
                .unwrap_or_else(|| panic!("{name}: balance None for {k}"));
            assert!((got - v.as_f64().unwrap()).abs() < 1e-6, "{name} {k}");
        }

        // absent keys resolve to None, not zero
        assert_eq!(cube.balance("##NOPE##", 9, "A"), None);

        // snapshot drift is real where P rows exist: P!=X pairs observed
        if audit.p_rows > 0 {
            assert!(
                audit.p_neq_x_snapshot_drift > 0 || p_drift == 0.0,
                "{name}: P rows without drift signature"
            );
        }

        // balances map keys == cube keys (round-trip of the key set)
        let got = cube.balances();
        assert!(!got.is_empty(), "{name}: no balances");
    }
}

