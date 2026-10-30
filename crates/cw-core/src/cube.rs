//! The BL balance cube — P5.C4 (F28/F35/F46/F60/F61).
//!
//! `BL.dbf` is the canonical balance store (F35: the AM balance columns are a
//! dead legacy cache). Every row is one cell of the cube
//! `NUMBER x ENTITY x YEAR x BUCKET x PERIOD x TYPE` plus the two overlay
//! axes CW folds into the row: `ID` (O official / X computed overlay / P
//! prior-carried snapshot) and `GROUP` (A actual / T tax reporting basis).
//!
//! Corpus-established value laws (F61, `the tooling scripts`):
//! * **L1 pair-mirror** — bucket-O `X` rows are value-identical to their `O`
//!   sibling (100%); `X` is the maintained overlay (carries AMT_YTD/FX fill),
//!   so the O-layer value is *X-precedence*: X when present, else O.
//! * **L2/L3 fill** — `X`: AMT_YTD==AMT, FX==AMT, FX_YTD==AMT_YTD; `O`: AMT
//!   only (YTD/FX_YTD zero); `P`: AMT only, never without an X sibling.
//! * **L4 snapshot** — `P` = the at-close frozen twin of `X` (equal but for
//!   documented post-close drift); never counted in balances.
//! * **L5 balance** — `bal(number,year,group) = Olayer(X-precedence) +
//!   sum(Y rows, ID!=P, same group)`. Y rows typed N/R/T carry the period
//!   movement (PERIOD 1..12; corpora sit at 1); O rows are TYPE=''/PERIOD=0.
//! * solo-X O-bucket rows are CW markers (NETINC family), not sum-derived.
//!
//! Port contract: `cw-core\tests\cube_golden.rs` must reproduce
//! `tests\golden\cube\expectations.json` leaf-for-leaf. Fixture provenance:
//! the original freeze was a Python oracle (two implementations, one truth);
//! that oracle is not part of the rebuild, so the fixture is now kernel-derived
//! (`examples/freeze_cube.rs`) and the gate is a regression/coverage law over
//! frozen bytes.

use crate::dbf::Table;
use std::collections::BTreeMap;

/// The exact BL schema (all three golden corpora identical).
pub const CUBE_COLUMNS: &[&str] = &[
    "ID", "GROUP", "NUMBER", "ENTITY", "YEAR", "BUCKET", "TYPE", "BOOKED", "PERIOD", "CUSTOM",
    "AMT", "AMT_YTD", "AMT_DIST", "FX", "FX_YTD", "FX_DIST", "CWGUID", "CWDATE", "CREATEDON",
];

/// Columns whose domain censuses are frozen (axes + blankness markers).
pub const AUDIT_COLUMNS: &[&str] = &[
    "ID", "GROUP", "YEAR", "BUCKET", "TYPE", "PERIOD", "BOOKED", "CUSTOM", "ENTITY",
];

fn cell(table: &Table, r: usize, name: &str) -> Option<String> {
    let raw = table.cell(r, name)?;
    let s: String = crate::canon::latin1(raw).into();
    Some(s.trim().to_string())
}

fn cell_or(table: &Table, r: usize, name: &str) -> String {
    cell(table, r, name).unwrap_or_default()
}

fn num(table: &Table, r: usize, name: &str) -> f64 {
    match cell(table, r, name) {
        Some(s) if !s.is_empty() && s != "." => s.parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn is_live(table: &Table, r: usize) -> bool {
    table.records.get(r).map(|rec| rec[0] == b' ').unwrap_or(false)
}

/// One cube cell: the semantic projection of a live BL record.
#[derive(Debug, Clone, PartialEq)]
pub struct CubeRow {
    pub number: String,
    pub entity: String,
    pub year: i32,
    /// `O`pening / `Y`TD activity bucket (raw).
    pub bucket: String,
    /// movement type: `` / N / R / T (raw).
    pub kind: String,
    /// overlay id: `O`fficial / `X` computed / `P` prior-carried (raw).
    pub id: String,
    /// reporting basis: `A`ctual / `T`ax (raw).
    pub group: String,
    pub booked: String,
    pub period: i32,
    pub custom: String,
    pub amt: f64,
    pub amt_ytd: f64,
    pub amt_dist: f64,
    pub fx: f64,
    pub fx_ytd: f64,
    pub fx_dist: f64,
}

/// The balance cube over one engagement's BL table (live rows only).
#[derive(Debug, Clone, Default)]
pub struct Cube {
    rows: Vec<CubeRow>,
}

impl Cube {
    /// Parse a loaded `BL` table. Errors when semantic columns are missing.
    pub fn from_table(table: &Table) -> crate::Result<Cube> {
        for c in CUBE_COLUMNS {
            if table.field_index(c).is_none() {
                return Err(crate::CoreError::Format(format!("BL missing column {c}")));
            }
        }
        let mut rows = Vec::new();
        for r in 0..table.count() {
            if !is_live(table, r) {
                continue;
            }
            rows.push(CubeRow {
                number: cell_or(table, r, "NUMBER"),
                entity: cell_or(table, r, "ENTITY"),
                year: cell_or(table, r, "YEAR").parse::<i32>().unwrap_or(0),
                bucket: cell_or(table, r, "BUCKET"),
                kind: cell_or(table, r, "TYPE"),
                id: cell_or(table, r, "ID"),
                group: cell_or(table, r, "GROUP"),
                booked: cell_or(table, r, "BOOKED"),
                period: cell_or(table, r, "PERIOD").parse::<i32>().unwrap_or(0),
                custom: cell_or(table, r, "CUSTOM"),
                amt: num(table, r, "AMT"),
                amt_ytd: num(table, r, "AMT_YTD"),
                amt_dist: num(table, r, "AMT_DIST"),
                fx: num(table, r, "FX"),
                fx_ytd: num(table, r, "FX_YTD"),
                fx_dist: num(table, r, "FX_DIST"),
            });
        }
        Ok(Cube { rows })
    }

    /// Live row count.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// True when the cube holds no live rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Every live row.
    pub fn rows(&self) -> &[CubeRow] {
        &self.rows
    }

    /// The L5 balance law: O-layer (X-precedence) + non-P Y movement, scoped
    /// to one `number`/`year`/`group`. `None` when the key has no rows.
    pub fn balance(&self, number: &str, year: i32, group: &str) -> Option<f64> {
        let mut o_layer_x: Option<f64> = None;
        let mut o_layer_o: Option<f64> = None;
        let mut y_sum = 0.0;
        let mut any = false;
        for r in &self.rows {
            if r.number != number || r.year != year || r.group != group {
                continue;
            }
            any = true;
            if r.bucket == "O" {
                if r.id == "X" && o_layer_x.is_none() {
                    o_layer_x = Some(r.amt);
                } else if r.id == "O" && o_layer_o.is_none() {
                    o_layer_o = Some(r.amt);
                }
            } else if r.bucket == "Y" && r.id != "P" {
                y_sum += r.amt;
            }
        }
        if !any {
            return None;
        }
        Some(o_layer_x.or(o_layer_o).unwrap_or(0.0) + y_sum)
    }
}

/// Law/shape audit; every counter mirrors `the tooling scripts` exactly.
#[derive(Debug, Default, PartialEq)]
pub struct CubeAudit {
    pub rows: usize,
    pub pair_mirror_ok: u32,
    pub pair_mirror_paired: u32,
    pub x_ytd_eq_amt: u32,
    pub x_fx_eq_amt: u32,
    pub x_fx_ytd_eq_ytd: u32,
    pub o_ytd_zero: u32,
    pub o_fxytd_zero: u32,
    pub p_rows: u32,
    pub p_ytd_zero: u32,
    pub p_groups_without_x: u32,
    pub p_neq_x_snapshot_drift: u32,
    pub solo_x: u32,
    pub solo_x_eq_sum_ynp: u32,
    pub ytd_cum_ok: u32,
    pub ytd_cum_rows: u32,
    pub nonblank_booked: u32,
    pub nonblank_custom: u32,
    pub nonblank_entity: u32,
    pub amt_dist_nonzero: u32,
    pub fx_dist_nonzero: u32,
    pub axes: BTreeMap<&'static str, BTreeMap<String, u32>>,
    pub shapes: BTreeMap<String, u32>,
}

fn f64_key(v: f64) -> u64 {
    // deterministic bucketing for sort parity with the python oracle
    (v * 1e6).round() as i64 as u64
}

impl Cube {
    /// Full law audit (see module docs + freeze for the counter semantics).
    pub fn audit(&self) -> CubeAudit {
        let mut a = CubeAudit {
            rows: self.rows.len(),
            ..Default::default()
        };

        // per-(number,year,bucket,group) law groups + per-(number,year,group)
        // balance groups, indexed once.
        let mut bg: BTreeMap<(String, String, String, String), Vec<usize>> = BTreeMap::new();
        let mut ysum_np: BTreeMap<(String, String, String), f64> = BTreeMap::new();
        for (i, r) in self.rows.iter().enumerate() {
            bg.entry((
                r.number.clone(),
                r.year.to_string(),
                r.bucket.clone(),
                r.group.clone(),
            ))
            .or_default()
            .push(i);
            if r.bucket == "Y" && r.id != "P" {
                *ysum_np
                    .entry((r.number.clone(), r.year.to_string(), r.group.clone()))
                    .or_default() += r.amt;
            }
        }

        // axes census
        for col in AUDIT_COLUMNS {
            let mut m: BTreeMap<String, u32> = BTreeMap::new();
            for r in &self.rows {
                let v = match *col {
                    "ID" => r.id.clone(),
                    "GROUP" => r.group.clone(),
                    "YEAR" => r.year.to_string(),
                    "BUCKET" => r.bucket.clone(),
                    "TYPE" => r.kind.clone(),
                    "PERIOD" => r.period.to_string(),
                    "BOOKED" => r.booked.clone(),
                    "CUSTOM" => r.custom.clone(),
                    "ENTITY" => r.entity.clone(),
                    _ => unreachable!(),
                };
                *m.entry(v).or_default() += 1;
            }
            a.axes.insert(col, m);
        }

        // per-group laws
        for (_k, idx) in &bg {
            let bucket = self.rows[idx[0]].bucket.clone();
            if bucket != "O" {
                continue;
            }
            let has_o = idx.iter().any(|&i| self.rows[i].id == "O");
            let has_x = idx.iter().any(|&i| self.rows[i].id == "X");
            if has_o && has_x {
                a.pair_mirror_paired += 1;
                let o = idx
                    .iter()
                    .find(|&&i| self.rows[i].id == "O")
                    .map(|&i| self.rows[i].amt)
                    .unwrap();
                let x = idx
                    .iter()
                    .find(|&&i| self.rows[i].id == "X")
                    .map(|&i| self.rows[i].amt)
                    .unwrap();
                if (o - x).abs() < 1e-2 {
                    a.pair_mirror_ok += 1;
                }
            }
            if has_x && !has_o {
                a.solo_x += 1;
                let x = idx
                    .iter()
                    .find(|&&i| self.rows[i].id == "X")
                    .map(|&i| self.rows[i].amt)
                    .unwrap();
                let r0 = &self.rows[idx[0]];
                let key = (r0.number.clone(), r0.year.to_string(), r0.group.clone());
                if (x - ysum_np.get(&key).copied().unwrap_or(0.0)).abs() < 1e-2 {
                    a.solo_x_eq_sum_ynp += 1;
                }
            }
        }

        // row-level laws
        for r in &self.rows {
            match r.id.as_str() {
                "X" => {
                    a.x_ytd_eq_amt += ((r.amt_ytd - r.amt).abs() < 1e-2) as u32;
                    a.x_fx_eq_amt += ((r.fx - r.amt).abs() < 1e-2) as u32;
                    a.x_fx_ytd_eq_ytd += ((r.fx_ytd - r.amt_ytd).abs() < 1e-2) as u32;
                }
                "O" => {
                    a.o_ytd_zero += (r.amt_ytd == 0.0) as u32;
                    a.o_fxytd_zero += (r.fx_ytd == 0.0) as u32;
                }
                "P" => {
                    a.p_rows += 1;
                    a.p_ytd_zero += (r.amt_ytd == 0.0) as u32;
                }
                _ => {}
            }
            a.nonblank_booked += (!r.booked.is_empty()) as u32;
            a.nonblank_custom += (!r.custom.is_empty()) as u32;
            a.nonblank_entity += (!r.entity.is_empty()) as u32;
            a.amt_dist_nonzero += (r.amt_dist != 0.0) as u32;
            a.fx_dist_nonzero += (r.fx_dist != 0.0) as u32;
        }

        // P anatomy per law group
        for (_k, idx) in &bg {
            let has_p = idx.iter().any(|&i| self.rows[i].id == "P");
            if !has_p {
                continue;
            }
            let has_x = idx.iter().any(|&i| self.rows[i].id == "X");
            if !has_x {
                a.p_groups_without_x += 1;
            } else {
                let p = idx
                    .iter()
                    .find(|&&i| self.rows[i].id == "P")
                    .map(|&i| self.rows[i].amt)
                    .unwrap();
                let x = idx
                    .iter()
                    .find(|&&i| self.rows[i].id == "X")
                    .map(|&i| self.rows[i].amt)
                    .unwrap();
                if (p - x).abs() >= 1e-2 {
                    a.p_neq_x_snapshot_drift += 1;
                }
            }
        }

        // shapes: "BUCKET|idcount idcount..." mirroring python '%s%d' joined '+'
        for (_k, idx) in &bg {
            let mut counts: BTreeMap<String, u32> = BTreeMap::new();
            for &i in idx {
                *counts.entry(self.rows[i].id.clone()).or_default() += 1;
            }
            let parts: Vec<String> = counts.into_iter().map(|(k, v)| format!("{k}{v}")).collect();
            let key = format!("{}|{}", self.rows[idx[0]].bucket, parts.join("+"));
            *a.shapes.entry(key).or_default() += 1;
        }

        // YTD cumulative over periods on Y-bucket X rows (P excluded by id)
        let mut per: BTreeMap<(String, String, String, String, String), Vec<usize>> =
            BTreeMap::new();
        for (i, r) in self.rows.iter().enumerate() {
            if r.bucket == "Y" && r.id == "X" {
                per.entry((
                    r.number.clone(),
                    r.year.to_string(),
                    r.group.clone(),
                    r.kind.clone(),
                    r.period.to_string(),
                ))
                .or_default()
                .push(i);
            }
        }
        for (_k, mut idx) in per {
            idx.sort_by_key(|&i| f64_key(self.rows[i].period as f64));
            let mut cum = 0.0;
            for i in idx {
                cum += self.rows[i].amt;
                a.ytd_cum_rows += 1;
                a.ytd_cum_ok += ((self.rows[i].amt_ytd - cum).abs() < 1e-2) as u32;
            }
        }

        a
    }

    /// The full balance table keyed `number|year|group` (freeze parity shape),
    /// computed through [`Cube::balance`].
    pub fn balances(&self) -> BTreeMap<String, f64> {
        let mut keys: BTreeMap<(String, i32, String), ()> = BTreeMap::new();
        for r in &self.rows {
            keys.insert((r.number.clone(), r.year, r.group.clone()), ());
        }
        let mut out = BTreeMap::new();
        for ((n, y, g), _) in keys {
            let v = self.balance(&n, y, &g).unwrap_or(0.0);
            let rounded = (v * 1e6).round() / 1e6;
            out.insert(format!("{n}|{y}|{g}"), rounded);
        }
        out
    }
}

