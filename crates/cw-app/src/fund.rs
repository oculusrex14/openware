//! cw-app fund accounting: the `DIV` / `SUB` / `UNIT` segment axes (P5.H H3).
//!
//! Physical columns (`inventory\dbf-schema.tsv`, identical in all corpora):
//!
//! * `AM.DIV` `C 6` - division/fund (the `ICWAccountReportProperties.Division`
//!   surface).
//! * `AM.SUB` `C 6` - subdivision / sub-fund.
//! * `AM.UNIT` `C 4` - reporting unit.
//! * `CE.DIV` `C 6` - the entity's declared division.
//! * `GL.DIV` `C 6` + `GL.SUB` `C 6` - the same axes on journal lines.
//!
//! Corpus truth [O]: `AM.DIV`, `AM.SUB`, `AM.UNIT` are blank on every row of
//! hull2024 and corp2026, and `CE` has no rows. The three corpora therefore
//! use the **single unfunded normal form** - exactly one implicit fund
//! (`""`/`""`/`""`) whose totals equal [`crate::accounts::ChartTotals`].
//! Non-blank keys are [I] (no populated fixture); the model preserves the
//! physical widths and treats blank as the unfunded/default fund.
//!
//! Scope: key decode, fund roll-up, tie-out against the chart, and roster /
//! hierarchy validation. Posting to a fund (GL) and report presentation stay
//! in their own lanes.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::accounts::{AmAccount, Chart, ChartTotals};

/// Physical width of `AM.DIV` / `CE.DIV` / `GL.DIV`.
pub const DIV_LEN: usize = 6;
/// Physical width of `AM.SUB` / `GL.SUB`.
pub const SUB_LEN: usize = 6;
/// Physical width of `AM.UNIT`.
pub const UNIT_LEN: usize = 4;

/// One `(division, subdivision, unit)` triple.
///
/// Blank components are meaningful: the all-blank key is the engagement's
/// unfunded/default fund (the corpus normal form), not "no fund".
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FundKey {
    pub division: String,
    pub subdivision: String,
    pub unit: String,
}

impl FundKey {
    pub fn new(division: &str, subdivision: &str, unit: &str) -> FundKey {
        FundKey {
            division: division.trim().to_string(),
            subdivision: subdivision.trim().to_string(),
            unit: unit.trim().to_string(),
        }
    }

    /// Decode an `AM` row's three segment cells.
    pub fn from_account(a: &AmAccount) -> FundKey {
        FundKey::new(&a.div, &a.sub, &a.unit)
    }

    /// The corpus normal form (`""`/`""`/`""`).
    pub fn unfunded() -> FundKey {
        FundKey::default()
    }

    pub fn is_unfunded(&self) -> bool {
        self.division.is_empty() && self.subdivision.is_empty() && self.unit.is_empty()
    }

    /// A subdivision or unit without a division is a hierarchy violation.
    pub fn has_orphan_component(&self) -> bool {
        self.division.is_empty() && (!self.subdivision.is_empty() || !self.unit.is_empty())
    }

    /// Validate physical widths; returns the offending axis.
    pub fn too_wide(&self) -> Option<(&'static str, usize)> {
        if self.division.chars().count() > DIV_LEN {
            return Some(("DIV", DIV_LEN));
        }
        if self.subdivision.chars().count() > SUB_LEN {
            return Some(("SUB", SUB_LEN));
        }
        if self.unit.chars().count() > UNIT_LEN {
            return Some(("UNIT", UNIT_LEN));
        }
        None
    }

    /// Stable display key: `DIV/SUB/UNIT`, `-` for a blank component.
    pub fn label(&self) -> String {
        let part = |s: &str| {
            if s.is_empty() {
                "-".to_string()
            } else {
                s.to_string()
            }
        };
        format!(
            "{}/{}/{}",
            part(&self.division),
            part(&self.subdivision),
            part(&self.unit)
        )
    }
}

impl fmt::Display for FundKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

/// Aggregated balances for one fund key.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FundTotals {
    pub open: f64,
    pub cur_mth: f64,
    pub cur_ytd: f64,
    /// Posting account numbers contributing, in chart order.
    pub accounts: Vec<String>,
}

impl FundTotals {
    pub fn account_count(&self) -> usize {
        self.accounts.len()
    }

    fn add(&mut self, a: &AmAccount) {
        self.open += a.open;
        self.cur_mth += a.cur_mth;
        self.cur_ytd += a.cur_ytd;
        self.accounts.push(a.ac_no.clone());
    }
}

/// Fund roll-up over the posting rows of a [`Chart`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FundLedger {
    funds: BTreeMap<FundKey, FundTotals>,
}

impl FundLedger {
    /// Roll every posting row up by `(DIV, SUB, UNIT)`.
    ///
    /// The all-blank corpus normal form produces exactly one unfunded fund.
    pub fn from_chart(chart: &Chart) -> FundLedger {
        let mut funds: BTreeMap<FundKey, FundTotals> = BTreeMap::new();
        for a in chart.posting() {
            funds
                .entry(FundKey::from_account(a))
                .or_default()
                .add(a);
        }
        FundLedger { funds }
    }

    pub fn funds(&self) -> impl Iterator<Item = (&FundKey, &FundTotals)> {
        self.funds.iter()
    }

    pub fn len(&self) -> usize {
        self.funds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.funds.is_empty()
    }

    pub fn get(&self, key: &FundKey) -> Option<&FundTotals> {
        self.funds.get(key)
    }

    pub fn total(&self, key: &FundKey) -> Option<f64> {
        self.get(key).map(|f| f.cur_ytd)
    }

    /// The corpus normal form: one all-blank fund, or no posting rows.
    pub fn is_single_unfunded(&self) -> bool {
        self.funds.is_empty()
            || (self.funds.len() == 1 && self.funds.keys().next().unwrap().is_unfunded())
    }

    /// Sum one numeric column across funds.
    pub fn totals(&self) -> ChartTotals {
        let mut t = ChartTotals::default();
        for f in self.funds.values() {
            t.open += f.open;
            t.cur_mth += f.cur_mth;
            t.cur_ytd += f.cur_ytd;
        }
        t
    }

    /// True when the fund roll-up exactly reproduces the chart totals
    /// (cent-tolerant, the storage quantum).
    pub fn ties_out(&self, chart: &Chart) -> bool {
        let mine = self.totals();
        let theirs = chart.totals();
        let c = |v: f64| (v * 100.0).round() as i64;
        c(mine.open) == c(theirs.open)
            && c(mine.cur_mth) == c(theirs.cur_mth)
            && c(mine.cur_ytd) == c(theirs.cur_ytd)
    }

    /// Roll the funds up one level: division -> totals.
    pub fn by_division(&self) -> BTreeMap<String, FundTotals> {
        let mut out: BTreeMap<String, FundTotals> = BTreeMap::new();
        for (key, f) in &self.funds {
            let t = out.entry(key.division.clone()).or_default();
            t.open += f.open;
            t.cur_mth += f.cur_mth;
            t.cur_ytd += f.cur_ytd;
            t.accounts.extend(f.accounts.iter().cloned());
        }
        out
    }

    /// Roll the funds up one level: subdivision -> totals (within division).
    pub fn by_subdivision(&self) -> BTreeMap<(String, String), FundTotals> {
        let mut out: BTreeMap<(String, String), FundTotals> = BTreeMap::new();
        for (key, f) in &self.funds {
            let t = out
                .entry((key.division.clone(), key.subdivision.clone()))
                .or_default();
            t.open += f.open;
            t.cur_mth += f.cur_mth;
            t.cur_ytd += f.cur_ytd;
            t.accounts.extend(f.accounts.iter().cloned());
        }
        out
    }
}

/// Declared segment codes. `CE.DIV` seeds divisions [O]; subdivisions and
/// units have no physical roster table, so callers declare them explicitly
/// or leave the set empty to skip that check.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FundRoster {
    pub divisions: BTreeSet<String>,
    pub subdivisions: BTreeSet<String>,
    pub units: BTreeSet<String>,
}

impl FundRoster {
    /// Seed divisions from the live `CE` rows (`CE.DIV`).
    pub fn from_org(org: &crate::entities::Org) -> FundRoster {
        let divisions = org
            .effective_entities()
            .into_iter()
            .map(|e| e.division.trim().to_string())
            .filter(|d| !d.is_empty())
            .collect();
        FundRoster { divisions, ..FundRoster::default() }
    }

    pub fn declare_division(&mut self, code: &str) {
        let c = code.trim();
        if !c.is_empty() {
            self.divisions.insert(c.to_string());
        }
    }

    pub fn declare_subdivision(&mut self, code: &str) {
        let c = code.trim();
        if !c.is_empty() {
            self.subdivisions.insert(c.to_string());
        }
    }

    pub fn declare_unit(&mut self, code: &str) {
        let c = code.trim();
        if !c.is_empty() {
            self.units.insert(c.to_string());
        }
    }
}

/// One fund-ledger problem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FundIssue {
    pub key: FundKey,
    pub account: String,
    pub kind: FundIssueKind,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FundIssueKind {
    /// `SUB` or `UNIT` set while `DIV` is blank.
    OrphanComponent,
    /// A segment exceeds its physical field width.
    SegmentTooWide,
    /// Division not declared in the roster (`CE.DIV` / caller list).
    UndeclaredDivision,
    UndeclaredSubdivision,
    UndeclaredUnit,
}

impl fmt::Display for FundIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} [{}] {}: {}",
            self.kind,
            self.key.label(),
            if self.account.is_empty() {
                "<no account>"
            } else {
                &self.account
            },
            self.detail
        )
    }
}

impl FundLedger {
    /// Validate keys against widths and an optional roster.
    ///
    /// Empty roster axes are not enforced (no physical table exists for
    /// `SUB`/`UNIT`); a division roster seeded from `CE` is enforced.
    pub fn issues(&self, roster: &FundRoster) -> Vec<FundIssue> {
        let mut out = Vec::new();
        for (key, totals) in &self.funds {
            let account = totals.accounts.first().cloned().unwrap_or_default();
            if let Some((axis, width)) = key.too_wide() {
                out.push(FundIssue {
                    key: key.clone(),
                    account: account.clone(),
                    kind: FundIssueKind::SegmentTooWide,
                    detail: format!("{axis} exceeds {width} chars"),
                });
            }
            if key.has_orphan_component() {
                out.push(FundIssue {
                    key: key.clone(),
                    account: account.clone(),
                    kind: FundIssueKind::OrphanComponent,
                    detail: "SUB/UNIT set with blank DIV".into(),
                });
            }
            if !key.division.is_empty() && !roster.divisions.contains(&key.division) {
                out.push(FundIssue {
                    key: key.clone(),
                    account: account.clone(),
                    kind: FundIssueKind::UndeclaredDivision,
                    detail: format!("division {:?} is not in the roster", key.division),
                });
            }
            if !key.subdivision.is_empty()
                && !roster.subdivisions.is_empty()
                && !roster.subdivisions.contains(&key.subdivision)
            {
                out.push(FundIssue {
                    key: key.clone(),
                    account: account.clone(),
                    kind: FundIssueKind::UndeclaredSubdivision,
                    detail: format!(
                        "subdivision {:?} is not in the roster",
                        key.subdivision
                    ),
                });
            }
            if !key.unit.is_empty()
                && !roster.units.is_empty()
                && !roster.units.contains(&key.unit)
            {
                out.push(FundIssue {
                    key: key.clone(),
                    account: account.clone(),
                    kind: FundIssueKind::UndeclaredUnit,
                    detail: format!("unit {:?} is not in the roster", key.unit),
                });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{AccountKind, AccountProperties, Flag};

    fn account(ac_no: &str, div: &str, sub: &str, unit: &str, ytd: f64) -> AmAccount {
        AmAccount {
            row: 0,
            line_no: 0,
            kind: AccountKind::BalanceSheet,
            class: String::new(),
            ac_no: ac_no.into(),
            entity: String::new(),
            div: div.into(),
            sub: sub.into(),
            sub_no: String::new(),
            split_up: Flag::unset(),
            dist: String::new(),
            ac_no2: String::new(),
            ac_no2_cr: String::new(),
            desc: String::new(),
            schedule: String::new(),
            schedule_cr: String::new(),
            groups: Default::default(),
            groups_cr: Default::default(),
            tac_no: String::new(),
            gifi: String::new(),
            cc: String::new(),
            unit: unit.into(),
            unit_b: String::new(),
            props: AccountProperties::default(),
            open: 0.0,
            cur_mth: 0.0,
            cur_ytd: ytd,
            aje_bal: 0.0,
            eje_bal: 0.0,
            rje_bal: 0.0,
            c1: 0.0,
            c2: 0.0,
            base_ln: 0.0,
            memo_no: String::new(),
            from_ref: String::new(),
            rev_code: String::new(),
            fx_cytd: 0.0,
            fx_pytd: 0.0,
            c_rate: 0.0,
            fxtype: String::new(),
            status: String::new(),
            tax_id: String::new(),
            locked: Flag::unset(),
            hidden: Flag::unset(),
            pac_no: String::new(),
            cwguid: String::new(),
            cwdate: String::new(),
            createdon: String::new(),
            balstat: String::new(),
            cwchecksum: String::new(),
            conschksum: String::new(),
            extchksum: String::new(),
            memo_ref: String::new(),
            uid_note_ref: String::new(),
            exinfo_ref: String::new(),
            unused42: String::new(),
        }
    }

    #[test]
    fn key_shape_and_hierarchy() {
        assert!(FundKey::unfunded().is_unfunded());
        assert_eq!(FundKey::new(" 100 ", "01", "").label(), "100/01/-");
        assert!(FundKey::new("", "01", "").has_orphan_component());
        assert!(!FundKey::new("100", "", "").has_orphan_component());
        assert_eq!(FundKey::new("1234567", "", "").too_wide(), Some(("DIV", 6)));
        assert_eq!(FundKey::new("", "", "12345").too_wide(), Some(("UNIT", 4)));
    }

    #[test]
    fn rollup_and_tie_out() {
        // FundLedger::from_chart is the only constructor; exercise the
        // aggregation helpers directly.
        let mut l = FundLedger::default();
        let a = account("1000", "100", "01", "A", 500.0);
        let b = account("2000", "100", "02", "A", 250.0);
        let c = account("3000", "", "", "", -100.0);
        l.funds.entry(FundKey::from_account(&a)).or_default().add(&a);
        l.funds.entry(FundKey::from_account(&b)).or_default().add(&b);
        l.funds.entry(FundKey::from_account(&c)).or_default().add(&c);
        assert_eq!(l.len(), 3);
        assert_eq!(l.total(&FundKey::new("100", "01", "A")), Some(500.0));
        let by_div = l.by_division();
        assert_eq!(by_div["100"].cur_ytd, 750.0);
        assert_eq!(by_div[""].cur_ytd, -100.0);
        let by_sub = l.by_subdivision();
        assert_eq!(by_sub[&("100".into(), "02".into())].cur_ytd, 250.0);
        assert_eq!(l.totals().cur_ytd, 650.0);
    }

    #[test]
    fn issues_detect_orphans_and_undeclared_codes() {
        let mut roster = FundRoster::default();
        roster.declare_division("100");
        roster.declare_unit("A");
        let mut l = FundLedger::default();
        for a in [
            account("1000", "999", "", "B", 1.0),
            account("2000", "", "01", "", 2.0),
        ] {
            l.funds.entry(FundKey::from_account(&a)).or_default().add(&a);
        }
        let kinds: Vec<FundIssueKind> = l.issues(&roster).iter().map(|i| i.kind).collect();
        assert!(kinds.contains(&FundIssueKind::UndeclaredDivision));
        assert!(kinds.contains(&FundIssueKind::UndeclaredUnit));
        assert!(kinds.contains(&FundIssueKind::OrphanComponent));
    }
}

