//! cw-app foreign-exchange translation (P5.H H3).
//!
//! Physical inputs (`inventory\dbf-schema.tsv`, frozen corpus):
//!
//! * `AM.FXCYTD` `N 19.2` - translated current-year-to-date amount.
//! * `AM.FXPYTD` `N 19.2` - translated prior-year-to-date amount.
//! * `AM.C_RATE` `N 13.5` - the row's current rate.
//! * `AM.FXTYPE` `C 1` - rate basis; the corpus carries `H` (historical,
//!   dominant), `C` (current, 5 rows in corp2026 / 93 in hull2024) and blank
//!   (non-posting rows).
//! * `MP.FXCYTD` `N 13.5` + `MP.FXTYPE` `C 1` - map-line FX mirrors.
//!
//! COM surface (`ICWForeignExchangeRates`, `ICWAccountForeignExchange`):
//! `Rate`, `BudgetRate`, `ForecastRate`, `CustomRate`; per account
//! `CurrentRate`, `PriorYear1..4Rate`, `BudgetRate`, `Budget1..4Rate`,
//! `ForecastRate`, `Forecast1..4Rate`. The physical store keeps only the
//! current rate (`C_RATE`), so [`RateBook`] is the in-memory grid the COM
//! object exposes and `C_RATE` is its current-year projection.
//!
//! Truth markers: `[O]` observed in corpus, `[P]` established law, `[I]`
//! inference.
//!
//! ## The arithmetic — [P], the vendor's translation routine
//!
//! The translated amount is computed by one multiply and one decimal
//! re-render, in this exact order:
//!
//! ```text
//! if |rate| <= 1e-06:                 ; the "no rate" window
//!     FXCYTD   = f4double(amount)     ; rate 0 -> untranslated copy
//! else
//!     product  = f4double(amount) * f4double(rate)
//!     rendered = format(product, decimals=2, width=field_width,
//!                       pad='.', sign='-')                ; the field renderer
//!     FXCYTD   = parse(rendered)                        ; _fcvt rounds, then read back
//! ```
//!
//! The compare treats **`|rate| <= 1e-06` as "no rate"**, so the amount passes
//! through unchanged. That is the law the corpus cannot show (no non-zero
//! `C_RATE` exists anywhere) and code can.
//!
//! The re-render is the field renderer: it `memset`s the destination with `'.'`
//! (`0x2e`), picks the decimal count `min(field_decimals, width-1)`, calls
//! `_fcvt(value, decimals, …)` and places the sign (`'-'`, `0x2d`) at
//! `width - decimals - 1`. `_fcvt` rounds **half away from zero** on the exact
//! binary value — so `0.125` at two decimals is `0.13`, where Rust's
//! `format!("{:.2}")` (round-half-to-even) would give `0.12`. [`round_half_away`]
//! implements the vendor rule for that reason.
//!
//! `effective_rate` is `C_RATE` when non-zero, else the [`RateBook`] current
//! rate for the account's `ENTITY`, else `1.0` for `H`/blank (historical local
//! amounts need no translation) and an error for `C` (a current-rate row with
//! no rate cannot be translated honestly). That precedence is [I]: the code path
//! takes whatever rate the caller resolved, and the resolution order lives in
//! the callers, not in this function.

use std::collections::BTreeMap;

use cw_core::dbf::Table;
use cw_core::{CoreError, Result};

use crate::accounts::{write_cell, AmAccount};
use crate::entities::Year;

/// Physical width of `AM.C_RATE` decimals.
pub const RATE_DECIMALS: u32 = 5;
/// Historical-basis rows translate at par when no rate is supplied.
pub const HISTORICAL_PAR: f64 = 1.0;
/// Widest legal rate (fits `N 13.5` with sign) - a sanity bound, not [O].
pub const RATE_MAX: f64 = 99_999_999.0;

/// The vendor's "this rate is zero" window, **established**: the translation
/// routine compares the resolved rate against `-1e-06` and `+1e-06` and takes
/// the untranslated branch when `|rate| <= 1e-06`.
pub const RATE_ZERO_EPSILON: f64 = 1e-06;

/// `AM.FXTYPE` / `MP.FXTYPE` rate basis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FxType {
    /// Blank cell - historical by default for posting rows [I].
    Unset,
    /// `H` - historical rate.
    Historical,
    /// `C` - current rate.
    Current,
    /// Any other letter, preserved so an unknown file still decodes.
    Other(char),
}

impl FxType {
    pub fn parse(raw: &str) -> FxType {
        match raw.trim().to_ascii_uppercase().as_str() {
            "" => FxType::Unset,
            "H" => FxType::Historical,
            "C" => FxType::Current,
            other => FxType::Other(other.chars().next().unwrap_or('?')),
        }
    }

    /// Physical one-letter code (`""` for unset).
    pub fn code(self) -> String {
        match self {
            FxType::Unset => String::new(),
            FxType::Historical => "H".into(),
            FxType::Current => "C".into(),
            FxType::Other(c) => c.to_string(),
        }
    }

    /// True when the basis needs an explicit rate to translate.
    pub fn needs_rate(self) -> bool {
        matches!(self, FxType::Current)
    }

    pub fn label(self) -> &'static str {
        match self {
            FxType::Unset => "unset",
            FxType::Historical => "historical",
            FxType::Current => "current",
            FxType::Other(_) => "other",
        }
    }
}

/// A validated non-negative FX rate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FxRate {
    value: f64,
}

impl FxRate {
    /// Rate must be finite and in `(0, RATE_MAX]`.
    pub fn new(value: f64) -> Result<FxRate> {
        if !value.is_finite() || value <= 0.0 || value > RATE_MAX {
            return Err(CoreError::Format(format!(
                "FX rate {value} outside (0, {RATE_MAX}]"
            )));
        }
        Ok(FxRate { value })
    }

    /// Parse a `C_RATE` cell; blank / `0` mean "unset" (the corpus value).
    pub fn parse(raw: &str) -> Result<Option<FxRate>> {
        let s = raw.trim();
        if s.is_empty() {
            return Ok(None);
        }
        let v = s
            .parse::<f64>()
            .map_err(|_| CoreError::Format(format!("invalid FX rate {s:?}")))?;
        if !v.is_finite() || v < 0.0 {
            return Err(CoreError::Format(format!("invalid FX rate {s:?}")));
        }
        if v == 0.0 {
            return Ok(None);
        }
        FxRate::new(v).map(Some)
    }

    pub fn value(self) -> f64 {
        self.value
    }
}

/// In-memory rate grid keyed by `(entity, year)` (the COM rates object).
///
/// `C_RATE` stores only the current-year entry; prior/budget/forecast rates
/// live here for the `ICWAccountForeignExchange` projection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RateBook {
    rates: BTreeMap<(String, Year), f64>,
}

/// Entity keys are trimmed and uppercased for lookup (CW entity codes are
/// case-insensitive in the org roster; the blank key stays blank).
fn normalize_entity(entity: &str) -> String {
    entity.trim().to_ascii_uppercase()
}

impl RateBook {
    pub fn new() -> RateBook {
        RateBook::default()
    }

    /// Insert a rate for one entity/year. Entity is matched trimmed and
    /// case-insensitively, so the blank (single-entity) key is `""`.
    pub fn set(&mut self, entity: &str, year: Year, rate: f64) -> Result<()> {
        let r = FxRate::new(rate)?;
        self.rates
            .insert((normalize_entity(entity), year), r.value());
        Ok(())
    }

    pub fn get(&self, entity: &str, year: Year) -> Option<f64> {
        self.rates
            .get(&(normalize_entity(entity), year))
            .copied()
    }

    /// Remove one entry (the `ClearForeignExchange` surface).
    pub fn clear(&mut self, entity: &str, year: Year) -> Option<f64> {
        self.rates.remove(&(normalize_entity(entity), year))
    }

    /// Current-year rate for an entity.
    pub fn current(&self, entity: &str) -> Option<f64> {
        self.get(entity, Year::Current)
    }

    pub fn len(&self) -> usize {
        self.rates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rates.is_empty()
    }

    /// The rate grid projected onto one account (COM
    /// `ICWAccountForeignExchange`): current/prior1..4/budget/forecast.
    pub fn account_grid(&self, entity: &str) -> BTreeMap<Year, Option<f64>> {
        let mut out = BTreeMap::new();
        for y in Year::ALL {
            out.insert(y, self.get(entity, y));
        }
        out
    }
}

/// Why a row cannot be translated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FxErrorKind {
    MissingCurrentRate,
    UnknownRateType(char),
}

impl std::fmt::Display for FxErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FxErrorKind::MissingCurrentRate => {
                f.write_str("FXTYPE=C without C_RATE or RateBook rate")
            }
            FxErrorKind::UnknownRateType(c) => {
                write!(f, "unknown FXTYPE {c:?}")
            }
        }
    }
}

/// Round to `dec` decimals **half away from zero**, on the value's exact decimal
/// expansion.
///
/// This is `_fcvt`'s rule, which is what the vendor's field renderer
/// (`X64CODEBASE.dll`) uses. Rust's `{:.N}` formatter rounds
/// half-to-even, so the two disagree on exact ties — `0.125` at two decimals is
/// `0.13` here and `0.12` from the formatter. Ties are reachable: any midpoint
/// `(2k+1)/200` whose denominator reduces to a power of two (e.g. `1/8`,
/// `3/8`, `5/8`, …) is an exact double.
///
/// The implementation inspects the exact decimal expansion rather than
/// multiplying by a scale factor, because `v * 100.0` is itself inexact and is
/// what makes naive `(v*100).round()/100` disagree with the product.
pub fn round_half_away(v: f64, dec: u32) -> f64 {
    if !v.is_finite() {
        return v;
    }
    if v == 0.0 {
        return 0.0;
    }
    let neg = v.is_sign_negative();
    let a = v.abs();
    // 30 fractional digits is far more than any double needs to reveal a tie:
    // adjacent doubles near 1e4 differ by ~1e-12, so the expansion is exact well
    // before the thirtieth place.
    let s = format!("{a:.30}");
    let (int_part, frac_part) = s.split_once('.').unwrap_or((s.as_str(), ""));
    let d = dec as usize;
    let keep: String = frac_part.chars().take(d).collect();
    let first = frac_part.chars().nth(d).unwrap_or('0');
    // `_fcvt` rounds half away from zero: a leading 5 rounds up regardless of
    // what follows, which is exactly the tie rule.
    let round_up = first >= '5';
    let mut units: u128 = format!("{int_part}{keep}").parse().unwrap_or(0);
    if round_up {
        units += 1;
    }
    let out = units as f64 / 10f64.powi(dec as i32);
    if neg {
        -out
    } else {
        out
    }
}

/// `round_half_away(value, 2)`, with `-0.0` normalised to `0.0`.
pub fn round2(v: f64) -> f64 {
    round_half_away(v, 2)
}

pub fn to_cents(v: f64) -> i64 {
    (v * 100.0).round() as i64
}

/// One translated account row.
#[derive(Clone, Debug, PartialEq)]
pub struct FxTranslation {
    pub ac_no: String,
    pub entity: String,
    pub fx_type: FxType,
    /// Basis points are not used; rate is the multiplier from local to group.
    pub rate: f64,
    pub local_cytd: f64,
    pub local_pytd: f64,
    pub expected_cytd: f64,
    pub expected_pytd: f64,
    pub stored_cytd: f64,
    pub stored_pytd: f64,
    /// The `FXCYTD` cell was blank.
    pub cytd_missing: bool,
    /// The `FXPYTD` cell was blank.
    pub pytd_missing: bool,
}

impl FxTranslation {
    pub fn matches(&self) -> bool {
        !self.cytd_missing
            && !self.pytd_missing
            && to_cents(self.stored_cytd) == to_cents(self.expected_cytd)
            && to_cents(self.stored_pytd) == to_cents(self.expected_pytd)
    }
}

/// Resolve the rate applied to one account.
///
/// Order [I]: explicit `C_RATE` -> [`RateBook`] current rate for `ENTITY` ->
/// par for historical/unset -> error for current / unknown types.
pub fn effective_rate(a: &AmAccount, book: &RateBook) -> Result<f64> {
    if let Some(r) = FxRate::parse(&format!("{:.5}", a.c_rate))? {
        return Ok(r.value());
    }
    if let Some(r) = book.current(&a.entity) {
        return FxRate::new(r).map(|r| r.value());
    }
    let ty = FxType::parse(&a.fxtype);
    match ty {
        FxType::Historical | FxType::Unset => Ok(HISTORICAL_PAR),
        FxType::Current => Err(CoreError::Format(format!(
            "account {:?}: {}",
            a.ac_no,
            FxErrorKind::MissingCurrentRate
        ))),
        FxType::Other(c) => Err(CoreError::Format(format!(
            "account {:?}: {}",
            a.ac_no,
            FxErrorKind::UnknownRateType(c)
        ))),
    }
}

/// One translated amount, exactly as the vendor routine computes it.
///
/// Established sequence:
///
/// 1. `product = f4double(amount) * f4double(rate)` — a single multiply, both
///    operands through `f4double`, which is `c4atod` on the field's stored text
///    for numeric fields and `date4long` for date fields. There is **no**
///    intermediate rounding of either operand.
/// 2. `rendered = format(product, decimals = 2, width = field width)` — the
///    field renderer, which `_fcvt`s at the field's declared decimal
///    count (2 for `N 19.2`).
/// 3. the stored value is that rendered text read back, so the visible result is
///    the half-away-from-zero rounding of the exact product.
///
/// The zero-rate branch is taken when `|rate| <= 1e-06` (see
/// [`RATE_ZERO_EPSILON`]): the amount passes through **untranslated**, which is
/// why `FXCYTD` equals `CUR_YTD` on every corpus row that has a blank `C_RATE`.
pub fn translate_amount(amount: f64, rate: f64) -> f64 {
    if rate.abs() <= RATE_ZERO_EPSILON {
        return round2(amount);
    }
    round2(amount * rate)
}

/// Translate one physical `AM` row without writing it.
pub fn translate_row(t: &Table, r: usize, book: &RateBook) -> Result<FxTranslation> {
    let a = AmAccount::from_table(t, r)?;
    let rate = effective_rate(&a, book)?;
    let prior_rate = book.get(&a.entity, Year::Prior1).unwrap_or(rate);
    let cytd_raw = t.cell_str(r, "FXCYTD").unwrap_or_default();
    let pytd_raw = t.cell_str(r, "FXPYTD").unwrap_or_default();
    Ok(FxTranslation {
        ac_no: a.ac_no,
        entity: a.entity,
        fx_type: FxType::parse(&a.fxtype),
        rate,
        local_cytd: a.cur_ytd,
        local_pytd: a.open,
        expected_cytd: translate_amount(a.cur_ytd, rate),
        expected_pytd: translate_amount(a.open, prior_rate),
        stored_cytd: a.fx_cytd,
        stored_pytd: a.fx_pytd,
        cytd_missing: cytd_raw.trim().is_empty(),
        pytd_missing: pytd_raw.trim().is_empty(),
    })
}

/// Write `FXCYTD`/`FXPYTD`/`C_RATE` for one row.
///
/// The rate cell is refreshed only when a non-par rate applies, so a corpus
/// row with a blank `C_RATE` is not gratuitously rewritten to `1.00000`.
/// Returns the translation that was written.
pub fn translate_account(t: &mut Table, r: usize, book: &RateBook) -> Result<FxTranslation> {
    let tr = translate_row(t, r, book)?;
    // Validate both cells before touching bytes (N 19.2 widths).
    let mut scratch = t.clone();
    write_cell(&mut scratch, r, "FXCYTD", &format!("{:.2}", tr.expected_cytd))?;
    write_cell(&mut scratch, r, "FXPYTD", &format!("{:.2}", tr.expected_pytd))?;
    if tr.rate != HISTORICAL_PAR {
        write_cell(&mut scratch, r, "C_RATE", &format!("{:.5}", tr.rate))?;
    }
    write_cell(t, r, "FXCYTD", &format!("{:.2}", tr.expected_cytd))?;
    write_cell(t, r, "FXPYTD", &format!("{:.2}", tr.expected_pytd))?;
    if tr.rate != HISTORICAL_PAR {
        write_cell(t, r, "C_RATE", &format!("{:.5}", tr.rate))?;
    }
    Ok(tr)
}

/// Rows `translate_table` can act on (posting rows with a balance or an
/// existing FX cell). Memory/heading rows have no translatable amount.
pub fn is_translatable(t: &Table, r: usize) -> Result<bool> {
    let a = AmAccount::from_table(t, r)?;
    Ok(a.is_posting()
        && (a.cur_ytd != 0.0
            || a.open != 0.0
            || a.fx_cytd != 0.0
            || a.fx_pytd != 0.0
            || !a.fxtype.trim().is_empty()))
}

/// Translate every translatable row; returns the number written.
pub fn translate_table(t: &mut Table, book: &RateBook) -> Result<usize> {
    let mut rows = Vec::new();
    for r in 0..t.records.len() {
        if t.is_deleted(r) {
            continue;
        }
        if is_translatable(t, r)? {
            rows.push(r);
        }
    }
    for &r in &rows {
        translate_account(t, r, book)?;
    }
    Ok(rows.len())
}

/// One audited mismatch.
#[derive(Clone, Debug, PartialEq)]
pub struct FxMismatch {
    pub ac_no: String,
    pub field: &'static str,
    pub stored: f64,
    pub expected: f64,
    pub rate: f64,
}

/// Corpus-wide `FXCYTD`/`FXPYTD` audit (`stale` = present but wrong).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FxAudit {
    pub checked: usize,
    pub matches: usize,
    pub mismatches: usize,
    pub missing: usize,
    pub first_mismatch: Option<FxMismatch>,
}

impl FxAudit {
    pub fn is_clean(&self) -> bool {
        self.mismatches == 0 && self.missing == 0
    }
}

/// Audit the FX cells of every translatable row.
pub fn audit_table(t: &Table, book: &RateBook) -> Result<FxAudit> {
    let mut out = FxAudit::default();
    for r in 0..t.records.len() {
        if t.is_deleted(r) || !is_translatable(t, r)? {
            continue;
        }
        let tr = translate_row(t, r, book)?;
        out.checked += 1;
        if tr.cytd_missing || tr.pytd_missing {
            out.missing += 1;
        }
        if tr.matches() {
            out.matches += 1;
            continue;
        }
        out.mismatches += 1;
        if out.first_mismatch.is_none() {
            let (field, stored, expected) = if tr.cytd_missing
                || to_cents(tr.stored_cytd) != to_cents(tr.expected_cytd)
            {
                ("FXCYTD", tr.stored_cytd, tr.expected_cytd)
            } else {
                ("FXPYTD", tr.stored_pytd, tr.expected_pytd)
            };
            out.first_mismatch = Some(FxMismatch {
                ac_no: tr.ac_no,
                field,
                stored,
                expected,
                rate: tr.rate,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fx_type_matches_corpus_letters() {
        assert_eq!(FxType::parse("h"), FxType::Historical);
        assert_eq!(FxType::parse("C"), FxType::Current);
        assert_eq!(FxType::parse(" "), FxType::Unset);
        assert_eq!(FxType::parse("X"), FxType::Other('X'));
        assert_eq!(FxType::Current.code(), "C");
        assert!(FxType::Current.needs_rate());
        assert!(!FxType::Historical.needs_rate());
    }

    #[test]
    fn rates_validate_and_round_half_up_to_cents() {
        assert!(FxRate::new(1.35).is_ok());
        assert!(FxRate::new(0.0).is_err());
        assert!(FxRate::new(-1.0).is_err());
        assert!(FxRate::new(f64::INFINITY).is_err());
        assert_eq!(FxRate::parse("").unwrap(), None);
        assert_eq!(FxRate::parse("0").unwrap(), None);
        assert_eq!(FxRate::parse("1.23456").unwrap().unwrap().value(), 1.23456);
        assert_eq!(round2(10.005), 10.01);
        assert_eq!(round2(-10.005), -10.01);
        assert_eq!(round2(0.0), 0.0);
        assert_eq!(to_cents(12.345), 1235);
    }

    #[test]
    fn rate_book_holds_the_com_year_grid() {
        let mut b = RateBook::new();
        b.set("SUB", Year::Current, 1.25).unwrap();
        b.set("SUB", Year::Prior1, 1.20).unwrap();
        b.set("SUB", Year::Budget, 1.30).unwrap();
        assert_eq!(b.current(" sub "), Some(1.25));
        assert_eq!(b.get("SUB", Year::Prior1), Some(1.20));
        assert_eq!(b.get("SUB", Year::Forecast), None);
        assert_eq!(b.account_grid("SUB").len(), Year::ALL.len());
        assert_eq!(b.account_grid("SUB")[&Year::Budget], Some(1.30));
        assert_eq!(b.len(), 3);
        assert_eq!(b.clear("SUB", Year::Prior1), Some(1.20));
        assert_eq!(b.get("SUB", Year::Prior1), None);
    }
}

