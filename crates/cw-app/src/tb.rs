//! cw-app trial-balance views (P5.E4).
//!
//! Contract: `docs\specs\tb-views.md`. A trial balance is a *projection* of the
//! balance cube ([`cw_core::cube::Cube`], F28/F35/F61) ??no numbers are
//! recomputed here. The module owns the three presentation laws the TB screen
//! adds on top of the cube:
//!
//! 1. **Period vs YTD** ??the `period` column is the movement in exactly
//!    `PERIOD=p`; the `ytd` column is the *balance* at the end of `p`
//!    (`opening + movement through p`). `ytd_movement` is exposed separately so
//!    a screen can print either the movement-only or the balance form.
//! 2. **Auto-compare year vs year** ??BL years are offsets (`0` = current, the
//!    year-end close shifts +1, F46), so the prior layer of year `Y` is `Y+1`.
//!    [`Compare::Auto`] binds that layer only when it actually holds data for
//!    the selected group/entity; [`Compare::Off`] and [`Compare::Year`] are the
//!    explicit forms.
//! 3. **Account-number aggregation** ??rows are keyed by `NUMBER`; `ENTITY` is
//!    an optional filter (aggregate across entities by default, single entity
//!    when selected). CW synthetics (`NETINC`, `GROUP__*`) are hidden unless
//!    [`TbOptions::include_synthetics`] is set.
//!
//! Amounts are rounded to cents on aggregation (the cube stores 2-decimal
//! money), so totals tie to the printed rows exactly. The O-layer uses the
//! frozen X-precedence law; `ID='P'` rows are never counted (F61 L4/L5).

use std::collections::BTreeMap;

use cw_core::cube::Cube;
use cw_core::dbf::Table;
use cw_core::{CoreError, Result};

use crate::recalc::{BUCKET_OPENING, BUCKET_YEAR, GROUP_ACTUAL, GROUP_TAX};

/// Highest legal `BL.YEAR` offset (F28 window `0..=4`).
pub const MAX_CUBE_YEAR: i32 = 4;
/// First legal period (`BL.PERIOD` on Y rows).
pub const PERIOD_MIN: u8 = 1;
/// Last legal period.
pub const PERIOD_MAX: u8 = 12;
/// Prefix of CW synthetic grouping numbers (`GROUP__NI`, ...).
pub const SYNTHETIC_PREFIX: &str = "GROUP__";
/// Synthetic net-income marker number.
pub const NET_INCOME: &str = "NETINC";

/// Prior-layer selection for the comparative columns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Compare {
    /// No prior-year columns; variances are zero.
    Off,
    /// Bind year `view.year + 1` when that layer has data (the normal case).
    #[default]
    Auto,
    /// Bind an explicit `BL.YEAR` offset (must differ from the view year).
    Year(i32),
}

impl Compare {
    pub fn label(self) -> String {
        match self {
            Compare::Off => "off".to_string(),
            Compare::Auto => "auto".to_string(),
            Compare::Year(y) => format!("year {y}"),
        }
    }
}

/// TB screen options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TbOptions {
    /// `BL.YEAR` offset (0 = current, 0..=4 per F28).
    pub year: i32,
    /// Period cutoff (1..=12).
    pub period: u8,
    /// Reporting basis: `A` actual or `T` tax.
    pub group: String,
    /// Entity filter; `None` aggregates every entity (`Some("")` = blank only).
    pub entity: Option<String>,
    /// Prior-year comparison mode.
    pub compare: Compare,
    /// Include CW synthetic numbers (`NETINC`, `GROUP__*`).
    pub include_synthetics: bool,
}

impl Default for TbOptions {
    fn default() -> Self {
        TbOptions {
            year: 0,
            period: PERIOD_MIN,
            group: GROUP_ACTUAL.to_string(),
            entity: None,
            compare: Compare::Auto,
            include_synthetics: false,
        }
    }
}

impl TbOptions {
    /// Current-year actual view at `period`, auto-comparing.
    pub fn new(year: i32, period: u8) -> TbOptions {
        TbOptions { year, period, ..Default::default() }
    }

    /// Switch to the tax reporting basis.
    pub fn tax(mut self) -> TbOptions {
        self.group = GROUP_TAX.to_string();
        self
    }

    /// Restrict to one entity (`Some("")` selects the blank entity).
    pub fn entity(mut self, entity: impl Into<String>) -> TbOptions {
        self.entity = Some(entity.into());
        self
    }

    /// Select the prior-year comparison mode.
    pub fn compare(mut self, compare: Compare) -> TbOptions {
        self.compare = compare;
        self
    }

    /// Include CW synthetic numbers in the view.
    pub fn with_synthetics(mut self) -> TbOptions {
        self.include_synthetics = true;
        self
    }

    /// Validate and normalize the axes (`group` is upper-cased, `entity`
    /// trimmed). Errors are caller-facing text, never panics.
    pub fn validate(&self) -> Result<TbOptions> {
        if !(0..=MAX_CUBE_YEAR).contains(&self.year) {
            return Err(CoreError::Format(format!(
                "TB YEAR {} outside the 0..={MAX_CUBE_YEAR} window (F28)",
                self.year
            )));
        }
        if !(PERIOD_MIN..=PERIOD_MAX).contains(&self.period) {
            return Err(CoreError::Format(format!(
                "TB PERIOD {} outside the {PERIOD_MIN}..={PERIOD_MAX} window",
                self.period
            )));
        }
        let group = self.group.trim().to_ascii_uppercase();
        if group != GROUP_ACTUAL && group != GROUP_TAX {
            return Err(CoreError::Format(format!(
                "TB GROUP {:?} is not A (actual) or T (tax)",
                self.group
            )));
        }
        let entity = self.entity.as_ref().map(|e| e.trim().to_string());
        match self.compare {
            Compare::Year(y) => {
                if !(0..=MAX_CUBE_YEAR).contains(&y) {
                    return Err(CoreError::Format(format!(
                        "TB compare YEAR {y} outside the 0..={MAX_CUBE_YEAR} window (F28)"
                    )));
                }
                if y == self.year {
                    return Err(CoreError::Format(format!(
                        "TB compare YEAR {y} equals the view year"
                    )));
                }
            }
            Compare::Auto | Compare::Off => {}
        }
        Ok(TbOptions {
            year: self.year,
            period: self.period,
            group,
            entity,
            compare: self.compare,
            include_synthetics: self.include_synthetics,
        })
    }
}

/// One printed TB column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TbColumnKey {
    /// Opening balance of the view year.
    Opening,
    /// Movement in the selected period.
    Period,
    /// Movement through the selected period.
    YtdMovement,
    /// Balance at the selected period (`opening + ytd_movement`).
    Ytd,
    /// Opening balance of the comparison year.
    PriorOpening,
    /// Comparison-year movement in the selected period.
    PriorPeriod,
    /// Comparison-year balance at the selected period.
    PriorYtd,
    /// `Period` minus `PriorPeriod`.
    VariancePeriod,
    /// `Ytd` minus `PriorYtd`.
    VarianceYtd,
}

impl TbColumnKey {
    /// True for columns sourced from the comparison year.
    pub fn is_prior(self) -> bool {
        matches!(
            self,
            TbColumnKey::PriorOpening
                | TbColumnKey::PriorPeriod
                | TbColumnKey::PriorYtd
                | TbColumnKey::VariancePeriod
                | TbColumnKey::VarianceYtd
        )
    }
}

/// A rendered column header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TbColumn {
    pub key: TbColumnKey,
    pub label: String,
}

/// One account-number row of the trial balance. All amounts are cents-rounded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TbRow {
    pub number: String,
    pub opening: f64,
    pub period: f64,
    pub ytd_movement: f64,
    pub ytd: f64,
    pub prior_opening: f64,
    pub prior_period: f64,
    pub prior_ytd_movement: f64,
    pub prior_ytd: f64,
    pub variance_period: f64,
    pub variance_ytd: f64,
}

/// Column totals (same fields as a row, no number).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TbTotals {
    pub opening: f64,
    pub period: f64,
    pub ytd_movement: f64,
    pub ytd: f64,
    pub prior_opening: f64,
    pub prior_period: f64,
    pub prior_ytd_movement: f64,
    pub prior_ytd: f64,
    pub variance_period: f64,
    pub variance_ytd: f64,
}

/// A rendered trial balance for one year/period/basis.
#[derive(Clone, Debug, PartialEq)]
pub struct TbView {
    /// Validated options actually used.
    pub year: i32,
    pub period: u8,
    pub group: String,
    pub entity: Option<String>,
    /// Bound comparison layer (`None` when comparison is off or absent).
    pub prior_year: Option<i32>,
    /// Rows sorted by account number.
    rows: Vec<TbRow>,
}

impl TbView {
    /// Build a view from a parsed cube.
    pub fn from_cube(cube: &Cube, opts: &TbOptions) -> Result<TbView> {
        let opts = opts.validate()?;
        let prior_year = resolve_prior(cube, &opts);
        let current = aggregate(cube, &opts, opts.year);
        let prior = prior_year
            .map(|y| aggregate(cube, &opts, y))
            .unwrap_or_default();

        let mut numbers: Vec<String> = current.keys().cloned().collect();
        for n in prior.keys() {
            if !current.contains_key(n) {
                numbers.push(n.clone());
            }
        }
        numbers.sort();

        let mut rows = Vec::with_capacity(numbers.len());
        for number in numbers {
            let c = current.get(&number).copied().unwrap_or_default();
            let p = prior.get(&number).copied().unwrap_or_default();
            let opening = money(c.opening());
            let period = money(c.period);
            let ytd_movement = money(c.ytd_movement);
            let ytd = money(opening + ytd_movement);
            let prior_opening = money(p.opening());
            let prior_period = money(p.period);
            let prior_ytd_movement = money(p.ytd_movement);
            let prior_ytd = money(prior_opening + prior_ytd_movement);
            let (variance_period, variance_ytd) = if prior_year.is_some() {
                (money(period - prior_period), money(ytd - prior_ytd))
            } else {
                (0.0, 0.0)
            };
            rows.push(TbRow {
                number,
                opening,
                period,
                ytd_movement,
                ytd,
                prior_opening,
                prior_period,
                prior_ytd_movement,
                prior_ytd,
                variance_period,
                variance_ytd,
            });
        }

        Ok(TbView {
            year: opts.year,
            period: opts.period,
            group: opts.group,
            entity: opts.entity,
            prior_year,
            rows,
        })
    }

    /// Build a view straight from a loaded `BL` table.
    pub fn from_table(table: &Table, opts: &TbOptions) -> Result<TbView> {
        let cube = Cube::from_table(table)?;
        TbView::from_cube(&cube, opts)
    }

    /// Rows in account-number order.
    pub fn rows(&self) -> &[TbRow] {
        &self.rows
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// True when comparative columns are bound to a comparison year.
    pub fn has_prior(&self) -> bool {
        self.prior_year.is_some()
    }

    /// One row by exact account number (binary search; rows are sorted).
    pub fn row(&self, number: &str) -> Option<&TbRow> {
        let i = self.rows.binary_search_by(|r| r.number.as_str().cmp(number)).ok()?;
        Some(&self.rows[i])
    }

    /// YTD balance for one account (`None` when the number is not in the view).
    pub fn balance(&self, number: &str) -> Option<f64> {
        self.row(number).map(|r| r.ytd)
    }

    /// Column totals over every row.
    pub fn totals(&self) -> TbTotals {
        let mut t = TbTotals::default();
        for r in &self.rows {
            t.opening += r.opening;
            t.period += r.period;
            t.ytd_movement += r.ytd_movement;
            t.ytd += r.ytd;
            t.prior_opening += r.prior_opening;
            t.prior_period += r.prior_period;
            t.prior_ytd_movement += r.prior_ytd_movement;
            t.prior_ytd += r.prior_ytd;
            t.variance_period += r.variance_period;
            t.variance_ytd += r.variance_ytd;
        }
        t.opening = money(t.opening);
        t.period = money(t.period);
        t.ytd_movement = money(t.ytd_movement);
        t.ytd = money(t.ytd);
        t.prior_opening = money(t.prior_opening);
        t.prior_period = money(t.prior_period);
        t.prior_ytd_movement = money(t.prior_ytd_movement);
        t.prior_ytd = money(t.prior_ytd);
        t.variance_period = money(t.variance_period);
        t.variance_ytd = money(t.variance_ytd);
        t
    }

    /// Column set for this view (period/YTD current, then optional prior and
    /// variance columns). Labels are presentation text, ASCII-only.
    pub fn columns(&self) -> Vec<TbColumn> {
        let p = self.period;
        let y = self.year;
        let mut out = vec![
            TbColumn {
                key: TbColumnKey::Opening,
                label: format!("Opening (Y{y})"),
            },
            TbColumn {
                key: TbColumnKey::Period,
                label: format!("P{p} movement (Y{y})"),
            },
            TbColumn {
                key: TbColumnKey::YtdMovement,
                label: format!("P{p} YTD movement (Y{y})"),
            },
            TbColumn {
                key: TbColumnKey::Ytd,
                label: format!("P{p} YTD balance (Y{y})"),
            },
        ];
        if let Some(py) = self.prior_year {
            out.push(TbColumn {
                key: TbColumnKey::PriorOpening,
                label: format!("Opening (Y{py})"),
            });
            out.push(TbColumn {
                key: TbColumnKey::PriorPeriod,
                label: format!("P{p} movement (Y{py})"),
            });
            out.push(TbColumn {
                key: TbColumnKey::PriorYtd,
                label: format!("P{p} YTD balance (Y{py})"),
            });
            out.push(TbColumn {
                key: TbColumnKey::VariancePeriod,
                label: format!("P{p} movement variance"),
            });
            out.push(TbColumn {
                key: TbColumnKey::VarianceYtd,
                label: format!("P{p} YTD variance"),
            });
        }
        out
    }
}

/// One number's raw accumulation before cent rounding.
#[derive(Clone, Copy, Debug, Default)]
struct Acc {
    opening_x: Option<f64>,
    opening_o: Option<f64>,
    period: f64,
    ytd_movement: f64,
}

impl Acc {
    fn opening(self) -> f64 {
        self.opening_x.or(self.opening_o).unwrap_or(0.0)
    }
}

/// Aggregate the cube to number level for one year, honouring the period
/// cutoff, group and entity scope. Opening rows are first-X-precedence *per
/// entity* (the frozen L5 law), then summed across entities; movement rows sum
/// directly.
fn aggregate(cube: &Cube, opts: &TbOptions, year: i32) -> BTreeMap<String, Acc> {
    let mut by_entity: BTreeMap<(String, String), Acc> = BTreeMap::new();
    for r in cube.rows() {
        if r.year != year || r.group != opts.group {
            continue;
        }
        if let Some(entity) = &opts.entity {
            if r.entity != *entity {
                continue;
            }
        }
        if !opts.include_synthetics && is_synthetic(&r.number) {
            continue;
        }
        // P rows are at-close snapshots and never count (F61 L4).
        if r.id == "P" {
            continue;
        }
        let acc = by_entity
            .entry((r.number.clone(), r.entity.clone()))
            .or_default();
        if r.bucket == BUCKET_OPENING {
            if r.id == "X" {
                if acc.opening_x.is_none() {
                    acc.opening_x = Some(r.amt);
                }
            } else if acc.opening_o.is_none() {
                acc.opening_o = Some(r.amt);
            }
        } else if r.bucket == BUCKET_YEAR
            && r.period >= PERIOD_MIN as i32
            && r.period <= opts.period as i32
        {
            acc.ytd_movement += r.amt;
            if r.period == opts.period as i32 {
                acc.period += r.amt;
            }
        }
    }

    let mut out: BTreeMap<String, Acc> = BTreeMap::new();
    for ((number, _), acc) in by_entity {
        let slot = out.entry(number).or_default();
        if slot.opening_x.is_none() {
            slot.opening_x = Some(0.0);
        }
        slot.opening_x = Some(slot.opening_x.unwrap_or(0.0) + acc.opening());
        slot.period += acc.period;
        slot.ytd_movement += acc.ytd_movement;
    }
    out
}

/// Resolve the comparison layer. `Auto` binds `year + 1` only when that layer
/// has a non-P live row for the selected group/entity (synthetics included: the
/// layer is a data probe, not a printed row set).
fn resolve_prior(cube: &Cube, opts: &TbOptions) -> Option<i32> {
    match opts.compare {
        Compare::Off => None,
        Compare::Year(y) => Some(y),
        Compare::Auto => {
            let candidate = opts.year + 1;
            if candidate > MAX_CUBE_YEAR {
                return None;
            }
            let has_data = cube.rows().iter().any(|r| {
                r.year == candidate
                    && r.id != "P"
                    && r.group == opts.group
                    && opts
                        .entity
                        .as_ref()
                        .map(|e| r.entity == *e)
                        .unwrap_or(true)
            });
            has_data.then_some(candidate)
        }
    }
}

/// CW synthetic number (`NETINC` marker or `GROUP__*` family).
pub fn is_synthetic(number: &str) -> bool {
    number.trim() == NET_INCOME || number.trim().starts_with(SYNTHETIC_PREFIX)
}

/// Round to cents; normalizes negative zero.
fn money(v: f64) -> f64 {
    let r = (v * 100.0).round() / 100.0;
    if r == 0.0 {
        0.0
    } else {
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_numbers_are_recognized() {
        assert!(is_synthetic("NETINC"));
        assert!(is_synthetic("GROUP__NI"));
        assert!(!is_synthetic("1332-00"));
        assert!(!is_synthetic("GROUP"));
    }

    #[test]
    fn options_validate_axes() {
        assert!(TbOptions::new(0, 1).validate().is_ok());
        assert!(TbOptions::new(5, 1).validate().is_err());
        assert!(TbOptions::new(0, 0).validate().is_err());
        assert!(TbOptions::new(0, 13).validate().is_err());
        assert!(TbOptions::new(0, 1).compare(Compare::Year(0)).validate().is_err());
        assert!(TbOptions::new(0, 1).compare(Compare::Year(1)).validate().is_ok());
        assert!(TbOptions::new(0, 1)
            .compare(Compare::Year(MAX_CUBE_YEAR + 1))
            .validate()
            .is_err());
        let tax = TbOptions::new(0, 1).tax().validate().unwrap();
        assert_eq!(tax.group, GROUP_TAX);
    }

    #[test]
    fn money_rounds_and_normalizes_zero() {
        assert_eq!(money(1.006), 1.01);
        assert_eq!(money(1.004), 1.0);
        assert_eq!(money(-0.001), 0.0);
        assert_eq!(money(2.0).to_bits(), 2.0f64.to_bits());
    }
}

