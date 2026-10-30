//! P5.E3b — adjusting-entry recalculation and `BL` cube propagation (F35).
//!
//! The COM chain is `RecalculateAdjustingEntries()` (journal aggregation) followed
//! by `Repost()` (balance-cube rebuild). This module fuses the two into one
//! deterministic, idempotent kernel operation over the canonical `BL` store
//! (`cw_core::cube`) plus the decoded `GL` journal ([`Adjustments`]):
//!
//! * **tri-ledger balance flow** — every entry is routed to one of the three
//!   entry families (`AJE` / `EJE` / `RJE`, the `AM.AJE_BAL` / `EJE_BAL` /
//!   `RJE_BAL` legacy columns). Debit/credit/net totals are reported per family;
//!   the `AM` columns themselves are **never written** (F35: they are a dead
//!   derived cache — `BL` is the canonical store).
//! * **`BL` propagation** — balanced entries become `BUCKET='Y'` movement rows:
//!   `TYPE='N'` for AJE/EJE, `TYPE='R'` for RJE (mirrored into both the actual
//!   `GROUP='A'` and tax `GROUP='T'` basis per F61). Rows are `ID='X'` (the
//!   maintained overlay), `YEAR=opts.year`, one row per account/period (the
//!   line `PER` else the entry `PER`, default 1).
//! * **Y/R pruning law** — zero-amount `BUCKET='Y' TYPE='R'` rows are pruned
//!   (tombstoned), and derived `ID='X'` `Y/N|R` rows for every touched number
//!   are reconciled: matched keys are updated in place, stale keys are removed
//!   and missing keys appended. That makes a second run byte-identical.
//! * **Repost cosmetics** — `AMT_DIST`/`FX_DIST` blanks normalize to `0.00` on
//!   surviving `Y` rows; the `O`-layer row of each touched number is
//!   `CWDATE`-stamped only (never re-valued); `AMT_YTD` on `X` `Y` rows is the
//!   ascending-period cumulative `AMT`.
//!
//! Out of scope (documented residuals): map-node `ID='X' BUCKET='O'` overlay
//! pairs (the groupings lane), multi-entity `ENTITY` normalization,
//! `BOOKED` (storage unknown), `SaveChanges`/`.csl` change files, and the
//! `AM` derived-cache columns (output-only, not modelled here).

use std::collections::{BTreeMap, BTreeSet};

use cw_core::canon::latin1;
use cw_core::cube::CUBE_COLUMNS;
use cw_core::dbf::{RawRecord, Table};
use cw_core::guid::{CwGuid, Rng};
use cw_core::{CoreError, Result};

use crate::adjustments::{AdjustmentLine, Adjustments, EntryType, BALANCE_EPSILON};

/// Deletion flag: live record.
pub const LIVE: u8 = b' ';
/// Deletion flag: tombstone.
pub const DELETED: u8 = b'*';

/// Opening layer bucket (`PERIOD=0`, `TYPE=''`).
pub const BUCKET_OPENING: &str = "O";
/// Current-year activity bucket (the adjusting-entry target).
pub const BUCKET_YEAR: &str = "Y";
/// Normal movement type (AJE / EJE).
pub const TYPE_NORMAL: &str = "N";
/// Reclass movement type (RJE) — the prunable class.
pub const TYPE_RECLASS: &str = "R";
/// Computed overlay id.
pub const OVERLAY_X: &str = "X";
/// Reporting basis: actual.
pub const GROUP_ACTUAL: &str = "A";
/// Reporting basis: tax.
pub const GROUP_TAX: &str = "T";
/// Default year offset (current year).
pub const CURRENT_YEAR: i32 = 0;

// ---------------------------------------------------------------------------
// Tri-ledger families
// ---------------------------------------------------------------------------

/// The three CaseWare adjusting-entry ledgers (`AM.AJE_BAL/EJE_BAL/RJE_BAL`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntryFamily {
    /// Ordinary adjusting entries (normal, unrecorded, tax, other classes).
    Aje,
    /// Eliminating entries.
    Eje,
    /// Reclassifying entries (the `TYPE='R'` movement class).
    Rje,
}

impl EntryFamily {
    /// All families in ledger order.
    pub const ALL: [EntryFamily; 3] = [EntryFamily::Aje, EntryFamily::Eje, EntryFamily::Rje];

    /// Route a decoded `GL.STATUS` class to its ledger.
    pub fn of(entry_type: EntryType) -> EntryFamily {
        match entry_type {
            EntryType::Reclassifying => EntryFamily::Rje,
            EntryType::Eliminating => EntryFamily::Eje,
            _ => EntryFamily::Aje,
        }
    }

    /// Short ledger code (`AJE` / `EJE` / `RJE`).
    pub fn code(self) -> &'static str {
        match self {
            EntryFamily::Aje => "AJE",
            EntryFamily::Eje => "EJE",
            EntryFamily::Rje => "RJE",
        }
    }

    /// Legacy `AM` column this ledger mirrored (F35: **not** a live store).
    pub fn am_column(self) -> &'static str {
        match self {
            EntryFamily::Aje => "AJE_BAL",
            EntryFamily::Eje => "EJE_BAL",
            EntryFamily::Rje => "RJE_BAL",
        }
    }

    /// `BL.TYPE` movement code (F34/F44).
    pub fn movement(self) -> &'static str {
        match self {
            EntryFamily::Rje => TYPE_RECLASS,
            _ => TYPE_NORMAL,
        }
    }

    /// `BL.GROUP` bases the family propagates into (F61: `R` is mirrored).
    pub fn groups(self) -> &'static [&'static str] {
        match self {
            EntryFamily::Rje => &[GROUP_ACTUAL, GROUP_TAX],
            _ => &[GROUP_ACTUAL],
        }
    }

    fn index(self) -> usize {
        match self {
            EntryFamily::Aje => 0,
            EntryFamily::Eje => 1,
            EntryFamily::Rje => 2,
        }
    }
}

// ---------------------------------------------------------------------------
// Options / reports
// ---------------------------------------------------------------------------

/// Knobs for a deterministic recalculation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecalcOptions {
    /// `BL.YEAR` offset (0 = current, 0..=4 per F28).
    pub year: i32,
    /// `CWDATE` stamp for new/changed rows (`YYYYMMDDhhmmss`).
    pub stamp: String,
    /// Deterministic `CWGUID` seed (fixture/gate determinism).
    pub seed: u64,
    /// Mirror `TYPE='R'` rows into `GROUP='T'` as well as `GROUP='A'`.
    pub mirror_reclass: bool,
    /// Apply the Y/R pruning law (Repost).
    pub prune_zero_reclass: bool,
    /// Normalize blank `AMT_DIST`/`FX_DIST` to `0.00` on surviving `Y` rows.
    pub normalize_dist: bool,
}

impl Default for RecalcOptions {
    fn default() -> Self {
        RecalcOptions {
            year: CURRENT_YEAR,
            stamp: "20260101000000".to_string(),
            seed: 0x0E3B_5EED_0A1E_0001,
            mirror_reclass: true,
            prune_zero_reclass: true,
            normalize_dist: true,
        }
    }
}

impl RecalcOptions {
    /// Deterministic fixture profile.
    pub fn fixture(stamp: impl Into<String>, seed: u64) -> RecalcOptions {
        RecalcOptions { stamp: stamp.into(), seed, ..Default::default() }
    }
}

/// Debit/credit/net totals for one entry family.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FamilyFlow {
    pub entries: usize,
    pub lines: usize,
    pub debits: f64,
    pub credits: f64,
    pub net: f64,
}

/// One account-level contribution of the journal to a ledger.
#[derive(Clone, Debug, PartialEq)]
pub struct AccountFlow {
    pub number: String,
    pub entity: String,
    pub family: EntryFamily,
    pub group: String,
    /// `BL.TYPE` movement code (`N` / `R`).
    pub kind: String,
    pub period: u8,
    pub amount: f64,
}

/// Outcome of one recalculation (per-family flows + row churn).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecalcReport {
    pub year: i32,
    pub entries: usize,
    pub lines: usize,
    /// `[AJE, EJE, RJE]` in ledger order.
    pub families: [FamilyFlow; 3],
    /// Account-level flows, deterministic order.
    pub flows: Vec<AccountFlow>,
    /// New `Y` rows appended.
    pub y_rows_written: usize,
    /// Existing `Y` rows updated in place.
    pub y_rows_updated: usize,
    /// Stale derived `Y` rows tombstoned (key no longer produced).
    pub y_rows_tombstoned: usize,
    /// Zero `Y/R` rows pruned (Y/R pruning law).
    pub zero_reclass_pruned: usize,
    /// Blank `AMT_DIST`/`FX_DIST` cells normalized to `0.00`.
    pub dist_normalized: usize,
    /// `AMT_YTD` cells recomputed on `X` `Y` rows.
    pub ytd_stamped: usize,
    /// `O`-layer rows whose `CWDATE` was repost-stamped.
    pub o_rows_touched: usize,
}

impl RecalcReport {
    /// Flow for one ledger.
    pub fn family(&self, family: EntryFamily) -> &FamilyFlow {
        &self.families[family.index()]
    }

    /// Journal-wide net across all three ledgers (must be zero per entry, so
    /// only meaningful as a tie-out).
    pub fn net(&self) -> f64 {
        self.families.iter().map(|f| f.net).sum()
    }
}

// ---------------------------------------------------------------------------
// Typed BL row + table bridge
// ---------------------------------------------------------------------------

/// Semantic key of one `BL` cube cell (the full address minus overlay `GROUP`
/// folded in by [`BlKey::group`]).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct BlKey {
    pub number: String,
    pub entity: String,
    pub year: i32,
    pub bucket: String,
    pub kind: String,
    pub period: u8,
    pub id: String,
    pub group: String,
}

impl BlKey {
    /// Key for a derived current-year movement row.
    pub fn movement(number: impl Into<String>, entity: impl Into<String>, period: u8) -> BlKey {
        BlKey {
            number: number.into(),
            entity: entity.into(),
            year: CURRENT_YEAR,
            bucket: BUCKET_YEAR.to_string(),
            kind: TYPE_NORMAL.to_string(),
            period,
            id: OVERLAY_X.to_string(),
            group: GROUP_ACTUAL.to_string(),
        }
    }

    /// Debug address text for diagnostics.
    pub fn label(&self) -> String {
        format!(
            "{}|{}|{}{}{}{}|{}",
            self.number,
            self.entity,
            self.year,
            self.bucket,
            self.kind,
            self.period,
            self.group
        )
    }
}

/// A typed `BL` row (only the axes + numeric cells the recalculation owns).
#[derive(Clone, Debug, PartialEq)]
pub struct BlRow {
    pub id: String,
    pub group: String,
    pub number: String,
    pub entity: String,
    pub year: i32,
    pub bucket: String,
    pub kind: String,
    pub period: u8,
    pub amt: f64,
    pub amt_ytd: f64,
    pub amt_dist: f64,
    pub fx: f64,
    pub fx_ytd: f64,
    pub fx_dist: f64,
}

impl BlRow {
    /// Build a derived (`ID='X'`) current-year `Y` movement row.
    pub fn movement(number: impl Into<String>, entity: impl Into<String>, kind: &str) -> BlRow {
        BlRow {
            id: OVERLAY_X.to_string(),
            group: GROUP_ACTUAL.to_string(),
            number: number.into(),
            entity: entity.into(),
            year: CURRENT_YEAR,
            bucket: BUCKET_YEAR.to_string(),
            kind: kind.to_string(),
            period: 1,
            amt: 0.0,
            amt_ytd: 0.0,
            amt_dist: 0.0,
            fx: 0.0,
            fx_ytd: 0.0,
            fx_dist: 0.0,
        }
    }

    /// The row's cube address.
    pub fn key(&self) -> BlKey {
        BlKey {
            number: self.number.clone(),
            entity: self.entity.clone(),
            year: self.year,
            bucket: self.bucket.clone(),
            kind: self.kind.clone(),
            period: self.period,
            id: self.id.clone(),
            group: self.group.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Align {
    Left,
    Right,
}

/// Field-name → (offset, width) map for one table layout.
struct Layout {
    cells: Vec<(String, usize, usize)>,
}

impl Layout {
    fn of(t: &Table) -> Result<Layout> {
        for c in CUBE_COLUMNS {
            t.field_index(c).ok_or_else(|| {
                CoreError::Format(format!("BL missing column {c}"))
            })?;
        }
        let mut cells = Vec::with_capacity(t.fields.len());
        let mut off = 1usize;
        for f in &t.fields {
            cells.push((f.name.to_ascii_uppercase(), off, f.len as usize));
            off += f.len as usize;
        }
        if off != t.record_len as usize {
            return Err(CoreError::Format(format!(
                "BL widths sum to {off} but record_len is {}",
                t.record_len
            )));
        }
        Ok(Layout { cells })
    }

    fn span(&self, name: &str) -> Result<(usize, usize)> {
        let key = name.to_ascii_uppercase();
        self.cells
            .iter()
            .find(|(n, _, _)| *n == key)
            .map(|(_, o, w)| (*o, *w))
            .ok_or_else(|| CoreError::Format(format!("missing BL field {name}")))
    }

    fn get(&self, rec: &[u8], name: &str) -> Result<String> {
        let (o, w) = self.span(name)?;
        Ok(latin1(&rec[o..o + w]))
    }

    fn trim(&self, rec: &[u8], name: &str) -> Result<String> {
        Ok(self.get(rec, name)?.trim().to_string())
    }

    fn getf(&self, rec: &[u8], name: &str) -> Result<f64> {
        let s = self.trim(rec, name)?;
        if s.is_empty() || s == "." {
            return Ok(0.0);
        }
        s.parse::<f64>()
            .map_err(|_| CoreError::Format(format!("BL {name}={s:?} is not numeric")))
    }

    fn set(&self, rec: &mut [u8], name: &str, val: &str, align: Align) -> Result<()> {
        let (o, w) = self.span(name)?;
        let bytes = val.as_bytes();
        if bytes.len() > w {
            return Err(CoreError::Format(format!(
                "value {val:?} wider than BL {name} ({w})"
            )));
        }
        let cell = &mut rec[o..o + w];
        for b in cell.iter_mut() {
            *b = b' ';
        }
        match align {
            Align::Left => cell[..bytes.len()].copy_from_slice(bytes),
            Align::Right => {
                let start = w - bytes.len();
                cell[start..].copy_from_slice(bytes);
            }
        }
        Ok(())
    }

    fn setf(&self, rec: &mut [u8], name: &str, v: f64) -> Result<()> {
        self.set(rec, name, &fmt_money(v), Align::Right)
    }
}

fn format_year(year: i32) -> Result<String> {
    if !(0..=4).contains(&year) {
        return Err(CoreError::Format(format!(
            "BL YEAR {year} outside the 0..=4 window (F28)"
        )));
    }
    Ok(year.to_string())
}

/// Cents → CW numeric text (`-12.30`).
pub fn fmt_money(v: f64) -> String {
    format!("{:.2}", v)
}

fn to_cents(v: f64) -> i64 {
    (v * 100.0).round() as i64
}

fn from_cents(c: i64) -> f64 {
    c as f64 / 100.0
}

fn is_blank(s: &str) -> bool {
    s.is_empty() || s == "."
}

fn blank_record(t: &Table) -> RawRecord {
    let mut r = vec![b' '; t.record_len as usize];
    r[0] = LIVE;
    r
}

fn live(rec: &[u8]) -> bool {
    rec[0] == LIVE
}

/// Locate a live row by full semantic key.
pub fn find_row(t: &Table, key: &BlKey) -> Option<usize> {
    let layout = Layout::of(t).ok()?;
    for r in 0..t.count() {
        let rec = &t.records[r];
        if !live(rec) {
            continue;
        }
        if row_matches(&layout, rec, key) {
            return Some(r);
        }
    }
    None
}

fn row_matches(layout: &Layout, rec: &[u8], key: &BlKey) -> bool {
    let year = layout
        .trim(rec, "YEAR")
        .ok()
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(i32::MIN);
    year == key.year
        && layout.trim(rec, "NUMBER").map(|s| s == key.number).unwrap_or(false)
        && layout.trim(rec, "ENTITY").map(|s| s == key.entity).unwrap_or(false)
        && layout.trim(rec, "BUCKET").map(|s| s == key.bucket).unwrap_or(false)
        && layout.trim(rec, "TYPE").map(|s| s == key.kind).unwrap_or(false)
        && layout
            .trim(rec, "PERIOD")
            .ok()
            .and_then(|s| s.parse::<u8>().ok())
            == Some(key.period)
        && layout.trim(rec, "ID").map(|s| s == key.id).unwrap_or(false)
        && layout.trim(rec, "GROUP").map(|s| s == key.group).unwrap_or(false)
}

/// Append one typed row (stamps `CWGUID`/`CWDATE`/`CREATEDON`).
pub fn append_row(t: &mut Table, row: &BlRow, stamp: &str, rng: &mut Rng) -> Result<usize> {
    let layout = Layout::of(t)?;
    let mut rec = blank_record(t);
    layout.set(&mut rec, "ID", &row.id, Align::Left)?;
    layout.set(&mut rec, "GROUP", &row.group, Align::Left)?;
    layout.set(&mut rec, "NUMBER", &row.number, Align::Left)?;
    layout.set(&mut rec, "ENTITY", &row.entity, Align::Left)?;
    layout.set(&mut rec, "YEAR", &format_year(row.year)?, Align::Left)?;
    layout.set(&mut rec, "BUCKET", &row.bucket, Align::Left)?;
    layout.set(&mut rec, "TYPE", &row.kind, Align::Left)?;
    layout.set(&mut rec, "BOOKED", "", Align::Left)?;
    layout.set(&mut rec, "PERIOD", &row.period.to_string(), Align::Right)?;
    layout.set(&mut rec, "CUSTOM", "", Align::Left)?;
    layout.setf(&mut rec, "AMT", row.amt)?;
    layout.setf(&mut rec, "AMT_YTD", row.amt_ytd)?;
    layout.setf(&mut rec, "AMT_DIST", row.amt_dist)?;
    layout.setf(&mut rec, "FX", row.fx)?;
    layout.setf(&mut rec, "FX_YTD", row.fx_ytd)?;
    layout.setf(&mut rec, "FX_DIST", row.fx_dist)?;
    layout.set(&mut rec, "CWGUID", &CwGuid::allocate_for_table("BL", rng).encode(), Align::Left)?;
    layout.set(&mut rec, "CWDATE", stamp, Align::Left)?;
    layout.set(&mut rec, "CREATEDON", stamp, Align::Left)?;
    t.records.push(rec);
    Ok(t.records.len() - 1)
}

/// Overwrite the numeric/movement cells of an existing row (identity cells and
/// `CWGUID`/`CREATEDON` are preserved).
pub fn write_row(t: &mut Table, r: usize, row: &BlRow, stamp: &str) -> Result<()> {
    let layout = Layout::of(t)?;
    let rec = &mut t.records[r];
    layout.set(rec, "TYPE", &row.kind, Align::Left)?;
    layout.set(rec, "YEAR", &format_year(row.year)?, Align::Left)?;
    layout.set(rec, "BUCKET", &row.bucket, Align::Left)?;
    layout.set(rec, "PERIOD", &row.period.to_string(), Align::Right)?;
    layout.setf(rec, "AMT", row.amt)?;
    layout.setf(rec, "AMT_YTD", row.amt_ytd)?;
    layout.setf(rec, "AMT_DIST", row.amt_dist)?;
    layout.setf(rec, "FX", row.fx)?;
    layout.setf(rec, "FX_YTD", row.fx_ytd)?;
    layout.setf(rec, "FX_DIST", row.fx_dist)?;
    layout.set(rec, "CWDATE", stamp, Align::Left)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Recalculation
// ---------------------------------------------------------------------------

/// Aggregate key: one `BL` derived movement row.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct AggKey {
    number: String,
    entity: String,
    period: u8,
    kind: String,
    group: String,
}

/// Validate the journal and the options before touching the cube.
fn validate(adjustments: &Adjustments, opts: &RecalcOptions) -> Result<()> {
    if !(0..=4).contains(&opts.year) {
        return Err(CoreError::Format(format!(
            "recalc year {} outside the 0..=4 window (F28)",
            opts.year
        )));
    }
    if opts.stamp.len() != 14 || !opts.stamp.bytes().all(|b| b.is_ascii_digit()) {
        return Err(CoreError::Format(format!(
            "recalc stamp {:?} is not YYYYMMDDhhmmss",
            opts.stamp
        )));
    }
    let unbalanced: Vec<&str> = adjustments
        .unbalanced()
        .map(|e| e.system_id.as_str())
        .collect();
    if !unbalanced.is_empty() {
        return Err(CoreError::Format(format!(
            "cannot recalculate: unbalanced adjusting entries {:?} (F34 T1)",
            unbalanced
        )));
    }
    for entry in adjustments.entries() {
        if entry.lines.is_empty() {
            return Err(CoreError::Format(format!(
                "cannot recalculate: entry {:?} has no lines",
                entry.system_id
            )));
        }
        if !entry.entry_type.has_storage_code() {
            return Err(CoreError::Format(format!(
                "cannot recalculate: entry {:?} class {} is not storable",
                entry.system_id,
                entry.entry_type.label()
            )));
        }
        for line in &entry.lines {
            if line.account.trim().is_empty() {
                return Err(CoreError::Format(format!(
                    "cannot recalculate: entry {:?} has a blank account",
                    entry.system_id
                )));
            }
            if !line.amount.is_finite() {
                return Err(CoreError::Format(format!(
                    "cannot recalculate: entry {:?} has a non-finite amount",
                    entry.system_id
                )));
            }
            let period = line.period.or(entry.period).unwrap_or(1);
            if !(1..=12).contains(&period) {
                return Err(CoreError::Format(format!(
                    "cannot recalculate: entry {:?} period {period} outside 1..=12",
                    entry.system_id
                )));
            }
            if line.account.as_bytes().len() > 40 {
                return Err(CoreError::Format(format!(
                    "cannot recalculate: account {:?} wider than BL.NUMBER (40)",
                    line.account
                )));
            }
            if line.entity.as_bytes().len() > 40 {
                return Err(CoreError::Format(format!(
                    "cannot recalculate: entity {:?} wider than BL.ENTITY (40)",
                    line.entity
                )));
            }
        }
    }
    Ok(())
}

/// Line amount routed to a family's ledger (cents).
fn line_cents(line: &AdjustmentLine) -> i64 {
    to_cents(line.amount)
}

/// Recalculate the adjusting journal into the `BL` cube (F35/Repost).
///
/// The operation is atomic: validation and all edits happen on a copy which is
/// swapped in only on success. Running it twice over the same inputs yields a
/// byte-identical table (idempotent).
pub fn recalc(
    bl: &mut Table,
    adjustments: &Adjustments,
    opts: &RecalcOptions,
) -> Result<RecalcReport> {
    validate(adjustments, opts)?;

    let mut work = bl.clone();
    let layout = Layout::of(&work)?;

    // --- 1. tri-ledger balance flow + aggregate desired rows ---------------
    let mut report = RecalcReport { year: opts.year, ..Default::default() };
    let mut agg: BTreeMap<AggKey, i64> = BTreeMap::new();
    let mut account_flows: BTreeMap<(String, String, usize, String, u8), i64> = BTreeMap::new();
    let mut touched_numbers: BTreeSet<String> = BTreeSet::new();
    // (number, entity, group, kind) whose YTD cumulation must be refreshed.
    let mut ytd_keys: BTreeSet<(String, String, String, String)> = BTreeSet::new();

    for entry in adjustments.entries() {
        let family = EntryFamily::of(entry.entry_type);
        let idx = family.index();
        let flow = &mut report.families[idx];
        flow.entries += 1;
        report.entries += 1;

        for line in &entry.lines {
            let cents = line_cents(line);
            flow.lines += 1;
            report.lines += 1;
            if cents >= 0 {
                flow.debits += from_cents(cents);
            } else {
                flow.credits += from_cents(-cents);
            }
            flow.net += from_cents(cents);

            let number = line.account.trim().to_string();
            let entity = line.entity.trim().to_string();
            let period = line.period.or(entry.period).unwrap_or(1);
            let kind = family.movement().to_string();
            touched_numbers.insert(number.clone());

            let groups: Vec<&str> = if family == EntryFamily::Rje && opts.mirror_reclass {
                EntryFamily::Rje.groups().to_vec()
            } else {
                vec![GROUP_ACTUAL]
            };
            for group in groups {
                let key = AggKey {
                    number: number.clone(),
                    entity: entity.clone(),
                    period,
                    kind: kind.clone(),
                    group: group.to_string(),
                };
                *agg.entry(key).or_insert(0) += cents;
                *account_flows
                    .entry((
                        number.clone(),
                        entity.clone(),
                        idx,
                        group.to_string(),
                        period,
                    ))
                    .or_insert(0) += cents;
                ytd_keys.insert((
                    number.clone(),
                    entity.clone(),
                    group.to_string(),
                    kind.clone(),
                ));
            }
        }
    }

    report.flows = account_flows
        .iter()
        .map(|((number, entity, idx, group, period), cents)| AccountFlow {
            number: number.clone(),
            entity: entity.clone(),
            family: EntryFamily::ALL[*idx],
            group: group.clone(),
            kind: EntryFamily::ALL[*idx].movement().to_string(),
            period: *period,
            amount: from_cents(*cents),
        })
        .collect();

    // --- 2. reconcile derived ID='X' Y/N|R rows for touched numbers --------
    // Desired rows keyed by full BL key; RJE rows are mirrored per options.
    let mut desired: BTreeMap<BlKey, BlRow> = BTreeMap::new();
    for (key, cents) in &agg {
        let key_struct = BlKey {
            number: key.number.clone(),
            entity: key.entity.clone(),
            year: opts.year,
            bucket: BUCKET_YEAR.to_string(),
            kind: key.kind.clone(),
            period: key.period,
            id: OVERLAY_X.to_string(),
            group: key.group.clone(),
        };
        // Y/R pruning law: a zero reclass movement is not materialized.
        if key.kind == TYPE_RECLASS
            && *cents == 0
            && opts.prune_zero_reclass
        {
            report.zero_reclass_pruned += 1;
            continue;
        }
        let amount = from_cents(*cents);
        let mut row = BlRow::movement(key.number.clone(), key.entity.clone(), &key.kind);
        row.year = opts.year;
        row.group = key.group.clone();
        row.period = key.period;
        row.amt = amount;
        row.amt_ytd = amount; // refreshed by the cumulative pass below
        row.amt_dist = 0.0;
        row.fx = amount; // single-currency corpus: X FX mirrors AMT (F61 §3)
        row.fx_ytd = amount;
        row.fx_dist = 0.0;
        desired.insert(key_struct, row);
    }

    // Existing derived rows for the touched numbers (all periods/types A/T).
    let mut existing: BTreeMap<BlKey, usize> = BTreeMap::new();
    for r in 0..work.count() {
        let rec = &work.records[r];
        if !live(rec) {
            continue;
        }
        if layout.trim(rec, "BUCKET")? != BUCKET_YEAR
            || layout.trim(rec, "ID")? != OVERLAY_X
        {
            continue;
        }
        let year = layout.trim(rec, "YEAR")?.parse::<i32>().unwrap_or(-1);
        if year != opts.year {
            continue;
        }
        let group = layout.trim(rec, "GROUP")?;
        if group != GROUP_ACTUAL && group != GROUP_TAX {
            continue;
        }
        let kind = layout.trim(rec, "TYPE")?;
        if kind != TYPE_NORMAL && kind != TYPE_RECLASS {
            continue;
        }
        let number = layout.trim(rec, "NUMBER")?;
        if !touched_numbers.contains(&number) {
            continue;
        }
        let key = BlKey {
            number,
            entity: layout.trim(rec, "ENTITY")?,
            year,
            bucket: BUCKET_YEAR.to_string(),
            kind,
            period: layout.trim(rec, "PERIOD")?.parse::<u8>().unwrap_or(0),
            id: OVERLAY_X.to_string(),
            group,
        };
        existing.insert(key, r);
    }

    // Update matches, append new rows, tombstone stale ones.
    let mut rng = Rng::new_seeded(opts.seed);
    for (key, row) in &desired {
        match existing.remove(key) {
            Some(r) => {
                write_row(&mut work, r, row, &opts.stamp)?;
                report.y_rows_updated += 1;
            }
            None => {
                append_row(&mut work, row, &opts.stamp, &mut rng)?;
                report.y_rows_written += 1;
            }
        }
    }
    for (key, r) in &existing {
        let is_zero_reclass = key.kind == TYPE_RECLASS
            && layout.getf(&work.records[*r], "AMT")?.abs() < 0.005;
        if is_zero_reclass && opts.prune_zero_reclass {
            report.zero_reclass_pruned += 1;
        } else {
            report.y_rows_tombstoned += 1;
        }
        work.records[*r][0] = DELETED;
    }

    // --- 3. global Y/R prune (Repost law, untouched numbers included) -----
    if opts.prune_zero_reclass {
        for r in 0..work.count() {
            let rec = &work.records[r];
            if !live(rec) {
                continue;
            }
            if layout.trim(rec, "YEAR")?.parse::<i32>().unwrap_or(-1) != opts.year
                || layout.trim(rec, "BUCKET")? != BUCKET_YEAR
                || layout.trim(rec, "TYPE")? != TYPE_RECLASS
            {
                continue;
            }
            let group = layout.trim(rec, "GROUP")?;
            if group != GROUP_ACTUAL && group != GROUP_TAX {
                continue;
            }
            if layout.getf(rec, "AMT")?.abs() < BALANCE_EPSILON {
                work.records[r][0] = DELETED;
                report.zero_reclass_pruned += 1;
            }
        }
    }

    // --- 4. Repost cosmetics: DIST normalization on surviving Y rows ------
    if opts.normalize_dist {
        for r in 0..work.count() {
            let rec = &work.records[r];
            if !live(rec)
                || layout.trim(rec, "YEAR")?.parse::<i32>().unwrap_or(-1) != opts.year
                || layout.trim(rec, "BUCKET")? != BUCKET_YEAR
            {
                continue;
            }
            let (fix_amt_dist, fix_fx_dist) = (
                is_blank(&layout.trim(rec, "AMT_DIST")?),
                is_blank(&layout.trim(rec, "FX_DIST")?),
            );
            if fix_amt_dist {
                layout.setf(&mut work.records[r], "AMT_DIST", 0.0)?;
            }
            if fix_fx_dist {
                layout.setf(&mut work.records[r], "FX_DIST", 0.0)?;
            }
            if fix_amt_dist || fix_fx_dist {
                report.dist_normalized += 1;
            }
        }
    }

    // --- 5. cumulative AMT_YTD on X Y rows (ascending period) -------------
    for (number, entity, group, kind) in &ytd_keys {
        let mut rows: Vec<(u8, usize)> = Vec::new();
        for r in 0..work.count() {
            let rec = &work.records[r];
            if !live(rec)
                || layout.trim(rec, "YEAR")?.parse::<i32>().unwrap_or(-1) != opts.year
                || layout.trim(rec, "BUCKET")? != BUCKET_YEAR
                || layout.trim(rec, "ID")? != OVERLAY_X
                || layout.trim(rec, "NUMBER")? != *number
                || layout.trim(rec, "ENTITY")? != *entity
                || layout.trim(rec, "GROUP")? != *group
                || layout.trim(rec, "TYPE")? != *kind
            {
                continue;
            }
            let period = layout.trim(rec, "PERIOD")?.parse::<u8>().unwrap_or(0);
            rows.push((period, r));
        }
        rows.sort_by_key(|(p, r)| (*p, *r));
        let mut cum_cents = 0i64;
        for (_, r) in rows {
            cum_cents += to_cents(layout.getf(&work.records[r], "AMT")?);
            let target = from_cents(cum_cents);
            let current = layout.getf(&work.records[r], "AMT_YTD")?;
            if (current - target).abs() >= 0.005 {
                layout.setf(&mut work.records[r], "AMT_YTD", target)?;
                report.ytd_stamped += 1;
            }
        }
    }

    // --- 6. O layer: CWDATE touch only (never re-valued) -------------------
    for number in &touched_numbers {
        for group in [GROUP_ACTUAL, GROUP_TAX] {
            let mut target: Option<usize> = None;
            let mut fallback: Option<usize> = None;
            for r in 0..work.count() {
                let rec = &work.records[r];
                if !live(rec)
                    || layout.trim(rec, "YEAR")?.parse::<i32>().unwrap_or(-1) != opts.year
                    || layout.trim(rec, "BUCKET")? != BUCKET_OPENING
                    || layout.trim(rec, "NUMBER")? != *number
                    || layout.trim(rec, "GROUP")? != group
                {
                    continue;
                }
                match layout.trim(rec, "ID")?.as_str() {
                    OVERLAY_X => {
                        target = Some(r);
                        break;
                    }
                    "O" => fallback = fallback.or(Some(r)),
                    _ => {}
                }
            }
            if let Some(r) = target.or(fallback) {
                layout.set(&mut work.records[r], "CWDATE", &opts.stamp, Align::Left)?;
                report.o_rows_touched += 1;
            }
        }
    }

    *bl = work;
    Ok(report)
}

/// Alias for the COM `Repost()` step; the fused kernel operation is identical.
pub fn repost(
    bl: &mut Table,
    adjustments: &Adjustments,
    opts: &RecalcOptions,
) -> Result<RecalcReport> {
    recalc(bl, adjustments, opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_route_all_entry_classes() {
        assert_eq!(EntryFamily::of(EntryType::Normal), EntryFamily::Aje);
        assert_eq!(EntryFamily::of(EntryType::Reclassifying), EntryFamily::Rje);
        assert_eq!(EntryFamily::of(EntryType::Eliminating), EntryFamily::Eje);
        assert_eq!(EntryFamily::of(EntryType::TaxFederal), EntryFamily::Aje);
        assert_eq!(EntryFamily::of(EntryType::UnrecordedProposed), EntryFamily::Aje);
        assert_eq!(EntryFamily::Rje.movement(), TYPE_RECLASS);
        assert_eq!(EntryFamily::Aje.movement(), TYPE_NORMAL);
        assert_eq!(EntryFamily::Rje.am_column(), "RJE_BAL");
        assert_eq!(EntryFamily::Rje.groups(), &[GROUP_ACTUAL, GROUP_TAX]);
    }

    #[test]
    fn options_validate_year_and_stamp() {
        assert!(format_year(7).is_err());
        assert!(format_year(0).is_ok());
        assert!(format_year(4).is_ok());
    }
}

