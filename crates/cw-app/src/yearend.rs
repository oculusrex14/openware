//! cw-app year-end close / roll-forward (P5.D2).
//!
//! Behavior contract: `docs\specs\yearend.md` (A6/F46 model + F65 destination
//! laws), fixture `a sample copy{1,2}`. The close is a pure
//! destination-building operation (source never mutated):
//!
//! - BL: whole-cube year shift +1 for live rows; O pairs kept and X twins'
//!   DIST cells normalized (blank, except the RE account, whose DIST carries
//!   the year's NETINC marker); new year0 O/X pair per balance-sheet account
//!   = `O + Y` (RE absorbs the income-statement result); Y rows carry to
//!   year 1 with P twins; zero Y rows pruned; year0 Y bucket empty.
//! - GL / HI: emptied (new engagement starts without entries/history).
//! - SH: signoff columns A1..8/AD/AC/AU cleared, internal IR/LS index rows
//!   retired, fresh `wtb` index row appended (new year's working TB).
//! - TS: every live row tombstoned.
//! - WR: live memo slots cleared.
//! - FP: CLP client-profile cells rolled (dates +1 calendar year, RE account
//!   / option mask written, roll flag reset, version cell bumped).
//! - every other table is carried verbatim (tombstones included).
//!
//! Documented deviations (spec §7): byte cosmetics (memo renumbering, CDX
//! rebuild, `.cvw` roll-forward + `.bak`, `.ac` document-summary rewrite) and
//! the two FP cells that CW rewires with fresh interlink GUIDs (P5.F lane).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use cw_core::canon::latin1;
use cw_core::dbf::{RawRecord, Table};
use cw_core::guid::{CwGuid, Rng};
use cw_core::{CoreError, Result};

/// Deletion-flag byte for a live record.
pub const LIVE: u8 = b' ';
/// Deletion-flag byte for a deleted (tombstone) record.
pub const DELETED: u8 = b'*';

/// Exact CW validation text (F46 §2) for opening balances without an RE account.
pub const RE_REQUIRED: &str =
    "When moving current year balances to next year's opening, a Retained Earnings account must be specified";

/// ICWYearEndClose option surface (subset exercised by the fixtures; F46 §2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct YearEndOptions {
    /// `UpdateNextYearOpeningBalanceData` — the master "carry balances" switch:
    /// false ⇒ the destination BL is empty (fixture yec1).
    pub update_next_year_opening_balance: bool,
    /// `UpdatePriorYearBalanceData` — prior-year layers are carried either way
    /// in v1 (both fixtures with the close ON set it; residual in spec §7).
    pub update_prior_year_balance: bool,
    /// `CopyForeignExchange` — FX mirrors AMT in the single-currency corpus;
    /// carried either way (residual).
    pub copy_foreign_exchange: bool,
    /// `CopyOutstandingTransactions` — destination GL is empty even when set
    /// (F46 §3.3: no closing journal, no outstanding entries in the corpus).
    pub copy_outstanding_transactions: bool,
    /// `RetainedEarningAccount`, AM `AC_NO` form (required when opening is on).
    pub retained_earning_account: String,
    /// `BalanceType` — factory default 4; enum semantics parked (F46 §6).
    pub balance_type: i32,
}

impl Default for YearEndOptions {
    fn default() -> Self {
        YearEndOptions {
            update_next_year_opening_balance: false,
            update_prior_year_balance: false,
            copy_foreign_exchange: false,
            copy_outstanding_transactions: false,
            retained_earning_account: String::new(),
            balance_type: 4,
        }
    }
}

impl YearEndOptions {
    /// The options-on fixture (yec2): the four transfer flags + RE account.
    pub fn balance_carry(retained_earning_account: impl Into<String>) -> Self {
        YearEndOptions {
            update_next_year_opening_balance: true,
            update_prior_year_balance: true,
            copy_foreign_exchange: true,
            copy_outstanding_transactions: true,
            retained_earning_account: retained_earning_account.into(),
            balance_type: 4,
        }
    }

    /// 20-char CLP option mask (FP cell 130, F65): bits 1/2/9/11 =
    /// UpdateNextYearOpeningBalanceData / UpdatePriorYearBalanceData /
    /// CopyForeignExchange / CopyOutstandingTransactions. Remaining bits are
    /// not exercised by the fixtures (spec §7 open question).
    fn fp_mask(&self) -> String {
        let mut m = ['0'; 20];
        if self.update_next_year_opening_balance {
            m[0] = '1';
        }
        if self.update_prior_year_balance {
            m[1] = '1';
        }
        if self.copy_foreign_exchange {
            m[8] = '1';
        }
        if self.copy_outstanding_transactions {
            m[10] = '1';
        }
        m.iter().collect()
    }
}

/// Timestamp / RNG knobs for deterministic runs (CW stores local-time stamps
/// and random GUIDs; both are byte-cosmetic and ignored by the row-set gate).
#[derive(Clone, Debug)]
pub struct CloseStamp {
    /// 14-char `YYYYMMDDhhmmss` CWDATE/CREATEDON value for new/changed rows.
    pub stamp: String,
    /// Seed for the CWGUID allocator.
    pub seed: u64,
}

impl Default for CloseStamp {
    fn default() -> Self {
        CloseStamp { stamp: "20260101000000".to_string(), seed: 0x0C0F_FEE5 }
    }
}

impl CloseStamp {
    pub fn new(stamp: impl Into<String>, seed: u64) -> Self {
        CloseStamp { stamp: stamp.into(), seed }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableDelta {
    pub src_live: usize,
    pub dst_live: usize,
    pub src_deleted: usize,
    pub dst_deleted: usize,
}

#[derive(Clone, Debug, Default)]
pub struct CloseReport {
    pub source_stem: String,
    pub dest_stem: String,
    pub retained_earning_account: String,
    /// Income-statement result absorbed into the RE opening balance (cents).
    pub net_income_cents: i64,
    /// Balance-sheet accounts that received a year0 O/X pair (fixture: 52).
    pub closing_accounts: usize,
    /// Prior-year-carried P twins added (fixture: 12).
    pub p_twins: usize,
    pub tables: BTreeMap<String, TableDelta>,
    pub files: Vec<String>,
    pub notes: Vec<String>,
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
    fn of(t: &Table) -> Layout {
        let mut cells = Vec::with_capacity(t.fields.len());
        let mut off = 1usize;
        for f in &t.fields {
            cells.push((f.name.to_ascii_uppercase(), off, f.len as usize));
            off += f.len as usize;
        }
        Layout { cells }
    }

    fn span(&self, name: &str) -> Result<(usize, usize)> {
        let key = name.to_ascii_uppercase();
        self.cells
            .iter()
            .find(|(n, _, _)| *n == key)
            .map(|(_, o, w)| (*o, *w))
            .ok_or_else(|| CoreError::Format(format!("missing field {name}")))
    }

    fn get(&self, rec: &[u8], name: &str) -> Result<String> {
        let (o, w) = self.span(name)?;
        Ok(latin1(&rec[o..o + w]))
    }

    fn set(&self, rec: &mut [u8], name: &str, val: &str, align: Align) -> Result<()> {
        let (o, w) = self.span(name)?;
        let bytes = val.as_bytes();
        if bytes.len() > w {
            return Err(CoreError::Format(format!(
                "value {val:?} wider than {name} ({w})"
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

    fn setn(&self, rec: &mut [u8], name: &str, val: &str) -> Result<()> {
        self.set(rec, name, val, Align::Right)
    }
}

fn blank_record(t: &Table) -> RawRecord {
    let mut r = vec![b' '; t.record_len as usize];
    r[0] = LIVE;
    r
}

fn census(t: &Table) -> (usize, usize) {
    let mut live = 0;
    let mut del = 0;
    for r in &t.records {
        match r[0] {
            LIVE => live += 1,
            DELETED => del += 1,
            _ => {}
        }
    }
    (live, del)
}

fn cents(s: &str) -> i64 {
    let s = s.trim();
    if s.is_empty() {
        return 0;
    }
    let (neg, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.trim_start_matches('+')),
    };
    let (int, frac) = match s.split_once('.') {
        Some((a, b)) => (a, b),
        None => (s, ""),
    };
    let mut f = frac.as_bytes().to_vec();
    f.resize(2, b'0');
    let v = int.parse::<i64>().unwrap_or(0) * 100 + (f[0] - b'0') as i64 * 10 + (f[1] - b'0') as i64;
    if neg {
        -v
    } else {
        v
    }
}

fn fmt_cents(v: i64) -> String {
    let sign = if v < 0 { "-" } else { "" };
    let a = v.abs();
    format!("{sign}{}.{:02}", a / 100, a % 100)
}

/// Shift a single `YEAR` digit; years outside 0..=3 are dropped (F28 window).
fn shifted_year(c: &str) -> Option<char> {
    let d = c.trim().chars().next()?;
    match d {
        '0' => Some('1'),
        '1' => Some('2'),
        '2' => Some('3'),
        '3' => Some('4'),
        _ => None,
    }
}

fn new_guid(table: &str, rng: &mut Rng) -> String {
    CwGuid::allocate_for_table(table, rng).encode()
}

// ---- BL closing law ------------------------------------------------------

#[derive(Clone, Debug, Default)]
struct BlStats {
    net_income_cents: i64,
    closing_accounts: usize,
    p_twins: usize,
    year0_pairs: usize,
}

fn transform_bl(
    t: &Table,
    am: &Table,
    opts: &YearEndOptions,
    stamp: &CloseStamp,
    rng: &mut Rng,
) -> Result<(Vec<RawRecord>, BlStats)> {
    if !opts.update_next_year_opening_balance {
        return Ok((Vec::new(), BlStats::default()));
    }
    let re = opts.retained_earning_account.trim();
    let lay = Layout::of(t);
    let al = Layout::of(am);

    // chart: AC_NO -> TYPE (B balance sheet / I income statement)
    let mut typ: BTreeMap<String, String> = BTreeMap::new();
    for r in am.records.iter().filter(|r| r[0] == LIVE) {
        let no = al.get(r, "AC_NO")?.trim().to_string();
        let ty = al.get(r, "TYPE")?.trim().to_string();
        typ.insert(no, ty);
    }
    if !typ.contains_key(re) {
        return Err(CoreError::Format(format!(
            "retained earnings account not in chart: {re}"
        )));
    }

    let mut o_amt: BTreeMap<String, i64> = BTreeMap::new();
    let mut y_amt: BTreeMap<String, i64> = BTreeMap::new();
    let mut o_accounts: BTreeSet<String> = BTreeSet::new();
    let mut o_bucket_x: BTreeSet<String> = BTreeSet::new();
    let mut netinc_x: BTreeMap<String, i64> = BTreeMap::new();

    for r in t.records.iter().filter(|r| r[0] == LIVE) {
        let year = lay.get(r, "YEAR")?;
        let number = lay.get(r, "NUMBER")?.trim().to_string();
        let bucket = lay.get(r, "BUCKET")?;
        let id = lay.get(r, "ID")?;
        if number == "NETINC" && bucket.trim() == "O" && id.trim() == "X" {
            netinc_x.insert(year.trim().to_string(), cents(&lay.get(r, "AMT")?));
        }
        if year.trim() != "0" {
            continue;
        }
        if bucket.trim() == "O" {
            if id.trim() == "O" {
                *o_amt.entry(number.clone()).or_insert(0) += cents(&lay.get(r, "AMT")?);
                o_accounts.insert(number);
            } else {
                o_bucket_x.insert(number);
            }
        } else if bucket.trim() == "Y" {
            *y_amt.entry(number).or_insert(0) += cents(&lay.get(r, "AMT")?);
        }
    }

    let mut accounts: BTreeSet<String> = BTreeSet::new();
    accounts.extend(o_accounts.iter().cloned());
    accounts.extend(y_amt.keys().cloned());
    let itot: i64 = accounts
        .iter()
        .filter(|n| typ.get(*n).map(|t| t == "I").unwrap_or(false))
        .map(|n| o_amt.get(n).copied().unwrap_or(0) + y_amt.get(n).copied().unwrap_or(0))
        .sum();
    let y_total: i64 = y_amt.values().sum();

    let mut out: Vec<RawRecord> = Vec::new();
    let mut stats = BlStats { net_income_cents: itot, ..Default::default() };

    // 1) shift live rows +1 (zero Y rows pruned); P twins for carried Y rows.
    for r in t.records.iter().filter(|r| r[0] == LIVE) {
        let year = lay.get(r, "YEAR")?;
        let Some(ny) = shifted_year(&year) else { continue };
        let bucket = lay.get(r, "BUCKET")?;
        if bucket.trim() == "Y" {
            let zero = ["AMT", "AMT_YTD", "FX", "FX_YTD"]
                .iter()
                .all(|f| cents(&lay.get(r, f).unwrap_or_default()) == 0);
            if zero {
                continue;
            }
        }
        let number = lay.get(r, "NUMBER")?.trim().to_string();
        let id = lay.get(r, "ID")?;
        let mut c = r.clone();
        lay.set(&mut c, "YEAR", &ny.to_string(), Align::Left)?;
        if id.trim() == "X" {
            let dist = if number == re && bucket.trim() == "O" {
                fmt_cents(*netinc_x.get(year.trim()).unwrap_or(&0))
            } else {
                String::new()
            };
            lay.setn(&mut c, "AMT_DIST", &dist)?;
            lay.setn(&mut c, "FX_DIST", &dist)?;
        }
        out.push(c.clone());
        if bucket.trim() == "Y" && number != "NETINC" {
            let mut p = c;
            lay.set(&mut p, "ID", "P", Align::Left)?;
            for f in ["AMT_YTD", "AMT_DIST", "FX", "FX_YTD", "FX_DIST"] {
                lay.setn(&mut p, f, &fmt_cents(0))?;
            }
            lay.set(&mut p, "CWGUID", &new_guid("BL", rng), Align::Left)?;
            lay.set(&mut p, "CWDATE", &stamp.stamp, Align::Left)?;
            out.push(p);
            stats.p_twins += 1;
        }
    }

    // 2) RE year-1 Y placeholder: carries the year's distribution total.
    {
        let mut r = blank_record(t);
        lay.set(&mut r, "ID", "X", Align::Left)?;
        lay.set(&mut r, "GROUP", "A", Align::Left)?;
        lay.setn(&mut r, "NUMBER", re)?;
        lay.set(&mut r, "YEAR", "1", Align::Left)?;
        lay.set(&mut r, "BUCKET", "Y", Align::Left)?;
        lay.set(&mut r, "TYPE", "N", Align::Left)?;
        lay.setn(&mut r, "PERIOD", "1")?;
        lay.setn(&mut r, "AMT_DIST", &fmt_cents(y_total))?;
        lay.setn(&mut r, "FX_DIST", &fmt_cents(y_total))?;
        lay.set(&mut r, "CWGUID", &new_guid("BL", rng), Align::Left)?;
        lay.set(&mut r, "CWDATE", &stamp.stamp, Align::Left)?;
        lay.set(&mut r, "CREATEDON", &stamp.stamp, Align::Left)?;
        out.push(r);
    }

    // 3) new year0 closing layer: O/X pair per B account (solo-X markers excluded).
    for n in &accounts {
        if typ.get(n).map(|s| s.as_str()) != Some("B") {
            continue;
        }
        if o_bucket_x.contains(n) && !o_accounts.contains(n) {
            continue; // NETINC-family solo-X marker: not an account balance
        }
        let mut v = o_amt.get(n).copied().unwrap_or(0) + y_amt.get(n).copied().unwrap_or(0);
        if n == re {
            v += itot;
        }
        let mut o = blank_record(t);
        lay.set(&mut o, "ID", "O", Align::Left)?;
        lay.set(&mut o, "GROUP", "A", Align::Left)?;
        lay.setn(&mut o, "NUMBER", n)?;
        lay.set(&mut o, "YEAR", "0", Align::Left)?;
        lay.set(&mut o, "BUCKET", "O", Align::Left)?;
        lay.set(&mut o, "TYPE", "", Align::Left)?;
        lay.setn(&mut o, "PERIOD", "0")?;
        lay.setn(&mut o, "AMT", &fmt_cents(v))?;
        lay.set(&mut o, "CWGUID", &new_guid("BL", rng), Align::Left)?;
        lay.set(&mut o, "CWDATE", &stamp.stamp, Align::Left)?;
        lay.set(&mut o, "CREATEDON", &stamp.stamp, Align::Left)?;
        let mut x = o.clone();
        lay.set(&mut x, "ID", "X", Align::Left)?;
        for f in ["AMT_YTD", "FX", "FX_YTD"] {
            lay.setn(&mut x, f, &fmt_cents(v))?;
        }
        out.push(o);
        out.push(x);
        stats.closing_accounts += 1;
        stats.year0_pairs += 1;
    }
    Ok((out, stats))
}

// ---- destination side tables --------------------------------------------

const SH_RETIRED_IR: [&str; 6] = ["wtb", "FSZ", "WTB", "BWTB", "bud8", "grp"];

fn transform_sh(t: &Table, stamp: &CloseStamp, rng: &mut Rng) -> Result<Vec<RawRecord>> {
    let lay = Layout::of(t);
    let mut out = Vec::with_capacity(t.records.len() + 1);
    for r in &t.records {
        let mut c = r.clone();
        if r[0] == LIVE {
            let prg = lay.get(r, "PRG")?.trim().to_string();
            let sched = lay.get(r, "SCHEDULE")?.trim().to_string();
            let name = lay.get(r, "NAME")?.trim().to_string();
            let ty = lay.get(r, "TYPE")?.trim().to_string();
            let retire = (prg == "IR" && SH_RETIRED_IR.contains(&sched.as_str()))
                || (prg == "EX" && name == "LS" && ty == "A");
            if retire {
                c[0] = DELETED;
                out.push(c);
                continue;
            }
            for n in 1..=8 {
                for f in [format!("A{n}"), format!("AD{n}"), format!("AC{n}"), format!("AU{n}")] {
                    lay.set(&mut c, &f, "", Align::Left)?;
                }
            }
            lay.set(&mut c, "CWDATE", &stamp.stamp, Align::Left)?;
        }
        out.push(c);
    }
    let mut w = blank_record(t);
    lay.setn(&mut w, "LINENO", "0")?;
    lay.set(&mut w, "PRG", "IR", Align::Left)?;
    lay.set(&mut w, "SCHEDULE", "wtb", Align::Left)?;
    lay.set(&mut w, "CWGUID", &new_guid("SH", rng), Align::Left)?;
    lay.set(&mut w, "CWDATE", &stamp.stamp, Align::Left)?;
    lay.set(&mut w, "CREATEDON", &stamp.stamp, Align::Left)?;
    out.push(w);
    Ok(out)
}

fn transform_ts(t: &Table) -> Vec<RawRecord> {
    t.records
        .iter()
        .map(|r| {
            let mut c = r.clone();
            if r[0] == LIVE {
                c[0] = DELETED;
            }
            c
        })
        .collect()
}

fn transform_wr(t: &Table, stamp: &CloseStamp) -> Result<Vec<RawRecord>> {
    let lay = Layout::of(t);
    let mut out = Vec::with_capacity(t.records.len());
    for r in &t.records {
        let mut c = r.clone();
        if r[0] == LIVE {
            lay.set(&mut c, "MEMO", "", Align::Left)?;
            lay.set(&mut c, "CWDATE", &stamp.stamp, Align::Left)?;
        }
        out.push(c);
    }
    Ok(out)
}

/// Excel 1900 serial date (1899-12-30 epoch) → proleptic Gregorian.
fn excel_to_ymd(n: i64) -> (i64, i64, i64) {
    civil_from_days(days_from_civil(1899, 12, 30) + n)
}

fn ymd_to_excel(y: i64, m: i64, d: i64) -> i64 {
    days_from_civil(y, m, d) - days_from_civil(1899, 12, 30)
}

fn add_one_year(y: i64, m: i64, d: i64) -> (i64, i64, i64) {
    if m == 2 && d == 29 {
        (y + 1, 2, 28)
    } else {
        (y + 1, m, d)
    }
}

// Howard Hinnant civil-date algorithms (public domain).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn transform_fp(
    t: &Table,
    opts: &YearEndOptions,
    stamp: &CloseStamp,
) -> Result<Vec<RawRecord>> {
    let lay = Layout::of(t);
    let re = opts.retained_earning_account.trim();
    let mask = opts.fp_mask();
    let mut out = Vec::with_capacity(t.records.len());
    for r in &t.records {
        let mut c = r.clone();
        let wks = lay.get(r, "WKS")?.trim().to_string();
        let cell = lay.get(r, "CELLNO")?.trim().to_string();
        let seq = lay.get(r, "SEQNO")?.trim().to_string();
        let text = lay.get(r, "TEXT")?;
        let mut changed = false;
        if wks == "CLP" {
            match (cell.as_str(), seq.as_str()) {
                ("12", "2") | ("132", "2") => {
                    let val = if opts.update_next_year_opening_balance {
                        format!("{re:>20}  ")
                    } else {
                        String::new()
                    };
                    lay.set(&mut c, "TEXT", &val, Align::Left)?;
                    changed = true;
                }
                ("130", "1") => {
                    let val = if opts.update_next_year_opening_balance {
                        format!("{mask}  ")
                    } else {
                        format!("{}  ", "0".repeat(20))
                    };
                    lay.set(&mut c, "TEXT", &val, Align::Left)?;
                    changed = true;
                }
                ("134", "1") => {
                    lay.set(&mut c, "TEXT", "1", Align::Left)?;
                    changed = true;
                }
                ("282", "1") => {
                    lay.set(&mut c, "TEXT", "", Align::Left)?;
                    changed = true;
                }
                _ => {
                    let s = text.trim();
                    if !s.is_empty() && s.chars().all(|ch| ch.is_ascii_digit()) {
                        if let Ok(n) = s.parse::<i64>() {
                            if (30_000..=60_000).contains(&n) {
                                let (y, m, d) = excel_to_ymd(n);
                                let (y2, m2, d2) = add_one_year(y, m, d);
                                lay.set(&mut c, "TEXT", &ymd_to_excel(y2, m2, d2).to_string(), Align::Left)?;
                                changed = true;
                            }
                        }
                    }
                }
            }
        }
        if changed {
            lay.set(&mut c, "CWDATE", &stamp.stamp, Align::Left)?;
        }
        out.push(c);
    }
    Ok(out)
}

// ---- destination build ---------------------------------------------------

/// Runtime artifacts / non-engagement files CaseWare does not carry (F64 (2),
/// F65 file census).
fn skip_file(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    // `.DS_Store` / AppleDouble `._<name>` companions: filesystem noise, not
    // engagement content (CW never carries them into a new year).
    if name.starts_with('.') {
        return true;
    }
    l == "copy.ini"
        || l == "cmpchart.txt"
        || l.ends_with(".locks")
        || l.ends_with(".bak")
        || l.ends_with(".ac_")
        || l.ends_with(".zip")
        || l.ends_with(".tmp")
}

fn find_source_stem(src_dir: &Path) -> Result<String> {
    let mut stem: Option<String> = None;
    for e in fs::read_dir(src_dir).map_err(CoreError::from)? {
        let e = e.map_err(CoreError::from)?;
        let name = e.file_name().to_string_lossy().to_string();
        // OS noise (`.DS_Store`, macOS `._<name>` resource forks on exFAT/NTFS/
        // SMB volumes) is never the engagement hub.
        if name.starts_with('.') {
            continue;
        }
        if name.to_ascii_lowercase().ends_with(".ac") {
            if stem.is_some() {
                return Err(CoreError::Format("multiple .ac hubs in source".into()));
            }
            stem = Some(name[..name.len() - 3].to_string());
        }
    }
    stem.ok_or_else(|| CoreError::Format("no .ac hub in source dir".into()))
}

/// Locate `<stem><SFX>.dbf` case-insensitively (corpora mix `BL.dbf`/`bl.dbf`).
fn find_table(src_dir: &Path, stem: &str, sfx: &str) -> Result<PathBuf> {
    let want = format!("{sfx}.dbf");
    for e in fs::read_dir(src_dir).map_err(CoreError::from)? {
        let e = e.map_err(CoreError::from)?;
        let name = e.file_name().to_string_lossy().to_string();
        if let Some(rest) = name.strip_prefix(stem) {
            if rest.eq_ignore_ascii_case(&want) {
                return Ok(e.path());
            }
        }
    }
    Err(CoreError::Format(format!("missing source table {stem}{sfx}.dbf")))
}

/// Close the engagement at `src_dir` into `<dst_root>/<dest_stem>/` (F46/F65).
pub fn close_year(
    src_dir: &Path,
    dst_root: &Path,
    dest_stem: &str,
    opts: &YearEndOptions,
    stamp: &CloseStamp,
) -> Result<CloseReport> {
    if dest_stem.is_empty() {
        return Err(CoreError::Format("empty destination stem".into()));
    }
    if opts.update_next_year_opening_balance
        && opts.retained_earning_account.trim().is_empty()
    {
        return Err(CoreError::Format(RE_REQUIRED.into()));
    }
    let src_stem = find_source_stem(src_dir)?;
    let dst_dir = dst_root.join(dest_stem);
    fs::create_dir_all(&dst_dir).map_err(CoreError::from)?;

    let am = Table::parse_path(find_table(src_dir, &src_stem, "AM")?)?;

    let mut rng = Rng::new_seeded(stamp.seed);
    let mut report = CloseReport {
        source_stem: src_stem.clone(),
        dest_stem: dest_stem.to_string(),
        retained_earning_account: opts.retained_earning_account.trim().to_string(),
        ..Default::default()
    };

    let mut entries: Vec<PathBuf> = fs::read_dir(src_dir)
        .map_err(CoreError::from)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    entries.sort();

    for path in entries {
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        if skip_file(&name) {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".dbf") && name.starts_with(&src_stem) {
            let sfx = name[src_stem.len()..name.len() - 4].to_ascii_uppercase();
            let t = Table::parse_path(&path)?;
            let (sl, sd) = census(&t);
            let recs = match sfx.as_str() {
                "BL" => {
                    let (recs, stats) = transform_bl(&t, &am, opts, stamp, &mut rng)?;
                    report.net_income_cents = stats.net_income_cents;
                    report.closing_accounts = stats.closing_accounts;
                    report.p_twins = stats.p_twins;
                    let _ = stats.year0_pairs;
                    recs
                }
                "SH" => transform_sh(&t, stamp, &mut rng)?,
                "TS" => transform_ts(&t),
                "WR" => transform_wr(&t, stamp)?,
                "FP" => transform_fp(&t, opts, stamp)?,
                "GL" | "HI" => Vec::new(),
                _ => t.records.clone(),
            };
            let out = Table {
                raw_header: t.raw_header.clone(),
                version: t.version,
                header_len: t.header_len,
                record_len: t.record_len,
                fields: t.fields.clone(),
                records: recs,
            };
            let (dl, dd) = census(&out);
            let fname = format!("{dest_stem}{sfx}.dbf");
            out.write_path(dst_dir.join(&fname))?;
            report.tables.insert(sfx, TableDelta { src_live: sl, dst_live: dl, src_deleted: sd, dst_deleted: dd });
            report.files.push(fname);
        } else {
            let fname = match name.strip_prefix(src_stem.as_str()) {
                Some(rest) => format!("{dest_stem}{rest}"),
                None => name.clone(),
            };
            fs::copy(&path, dst_dir.join(&fname)).map_err(CoreError::from)?;
            report.files.push(fname);
        }
    }
    report.files.sort();
    report.notes.push("CDX/FPT carried verbatim (CW rebuilds); .cvw roll-forward + .bak and .ac document summary = P5.F byte cosmetics".into());
    if opts.update_next_year_opening_balance && !opts.update_prior_year_balance {
        report.notes.push("prior-year layers carried with opening-only options (single-fixture residual)".into());
    }
    if opts.copy_foreign_exchange {
        report.notes.push("FX mirrors AMT (single-currency corpus)".into());
    }
    Ok(report)
}

/// Convenience: the options-on fixture close (F46/F65).
pub fn close_year_default(
    src_dir: &Path,
    dst_root: &Path,
    dest_stem: &str,
    retained_earning_account: &str,
) -> Result<CloseReport> {
    close_year(
        src_dir,
        dst_root,
        dest_stem,
        &YearEndOptions::balance_carry(retained_earning_account),
        &CloseStamp::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_date_roundtrip() {
        for n in [30_000, 43_405, 45_230, 45_596, 60_000] {
            let (y, m, d) = excel_to_ymd(n);
            assert_eq!(ymd_to_excel(y, m, d), n);
        }
        assert_eq!(excel_to_ymd(45_230), (2023, 10, 31));
        assert_eq!(excel_to_ymd(45_596), (2024, 10, 31));
    }

    #[test]
    fn cents_roundtrip() {
        assert_eq!(cents("        -6806301.24"), -680_630_124);
        assert_eq!(fmt_cents(-680_630_124), "-6806301.24");
        assert_eq!(fmt_cents(5_395_76), "5395.76");
        assert_eq!(cents(""), 0);
    }
}

