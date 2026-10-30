//! cw-app consolidation maths, units and consistency checksums (P5.H H2).
//!
//! Contract: [`docs\specs\entities.md`] §6 hands consolidation arithmetic
//! (`CONSCHKSUM`, units) to H2. The physical inputs are already frozen:
//!
//! * `CE` ownership (current/prior 1..4/budget/forecast) from [`crate::entities`]
//!   selects the consolidation units; `EXCLUDE` removes a unit entirely.
//! * `BL` is the only balance store (F35). [`cw_core::cube::Cube`] owns the
//!   L5 balance law; this module applies the *same* law per entity before
//!   weighting by ownership.
//! * `AM` carries the 8-character `CONSCHKSUM` cell (`C 8`). Corpus rows leave
//!   it blank, but the observed `CWCHECKSUM` family is uppercase 8-hex, so
//!   OpenCW emits the same 8-hex shape.
//!
//! ## Maths
//!
//! For one `(number, year, group)` the consolidated value is
//!
//! ```text
//! total = sum(unit_balance(number, entity, year, group) * PER[year] / 100)
//! ```
//!
//! over included units. A single-blank engagement (all corpora) has one
//! implicit unit with 100% ownership, so `total == Cube::balance` exactly.
//!
//! ## Checksums — what CaseWare actually does (established)
//!
//! The `AM` checksum family is **not** a consolidation digest. The observed
//! writer law:
//!
//! ```text
//! cons := strfield("CWCHECKSUM");
//! assign(field("CONSCHKSUM"), cons);
//! ```
//!
//! so **`CONSCHKSUM` is a byte-for-byte copy of `CWCHECKSUM`** on the row,
//! written when the `am` row is inserted (the append success branch).
//! `EXTCHKSUM` is a different column with a different producer: on one internal
//! branch it takes
//! the same source string, and on the other branch it takes a CRC-32
//! accumulator (see `crate::import` for the tokenizer note). `CWCHECKSUM`
//! itself is written by a producer outside the import path's reach, and
//! **`CONSCHKSUM` in particular is never an independent digest of the
//! consolidated picture.**
//!
//! Corpus confirmation (all three frozen engagements, every row authentic):
//! `CONSCHKSUM` and `EXTCHKSUM` are **blank on every row** of `AM`, while
//! `CWCHECKSUM` is populated per row (325/325 in corp2026). The blank
//! `CONSCHKSUM` therefore means the copy branch did not run on these rows, not
//! that the column is unused.
//!
//! ## What OpenCW emits
//!
//! [`Consolidation::account_checksum`] / [`Consolidation::checksum`] remain the
//! **documented stand-in** (FNV-1a 32-bit over a canonical ASCII projection,
//! 8 uppercase hex). It is *shape-compatible* with the real family — 8 uppercase
//! hex, which is what `CWCHECKSUM` cells carry and what the `EXTCHKSUM` producer
//! formats via `sprintf(buf, 16, "%.8X", crc)`.
//! It is **not** byte-parity with CaseWare and is not claimed to be: the real
//! producer's input ordering is unrecovered.
//!
//! The old name is kept as [`legacy_fnv`] so the stand-in is unambiguously
//! labelled, and both are gated.

use std::collections::BTreeMap;

use cw_core::cube::Cube;
use cw_core::dbf::Table;
use cw_core::{CoreError, Result};

use crate::accounts::write_cell;
use crate::entities::{Org, Year};

/// Width of the `AM.CONSCHKSUM` / `CWCHECKSUM` / `EXTCHKSUM` cells.
pub const CHECKSUM_LEN: usize = 8;
/// Default ownership for an included unit whose `PER*` cell is blank [I].
///
/// The corpora have no `CE` rows, so 100% is the only value consistent with
/// the single-blank law. A named entity with a blank `PER` is treated as
/// wholly owned rather than silently dropped.
pub const DEFAULT_UNIT_PERCENT: f64 = 100.0;

/// One consolidation unit: a `CE` entity resolved against one ownership year.
#[derive(Clone, Debug, PartialEq)]
pub struct Unit {
    /// `CE.ENTITY` (blank = the single-blank / parent unit).
    pub entity: String,
    /// Ownership percent in the selected year (0 when unset and excluded).
    pub percent: f64,
    /// `EXCLUDE` is unset/false.
    pub included: bool,
    /// True when the percent came from a `PER*` cell rather than the default.
    pub explicit_percent: bool,
}

impl Unit {
    /// Fraction applied to this unit's balances (`0.0..=1.0` normally).
    pub fn weight(&self) -> f64 {
        if self.included {
            self.percent / 100.0
        } else {
            0.0
        }
    }

    /// True when this unit contributes to the consolidation.
    pub fn is_active(&self) -> bool {
        self.included && self.percent > 0.0
    }
}

/// The units in scope for one consolidation year.
#[derive(Clone, Debug, PartialEq)]
pub struct ConsolidationUnits {
    pub year: Year,
    pub units: Vec<Unit>,
}

impl ConsolidationUnits {
    /// Resolve every effective `CE` row against `year`.
    ///
    /// An empty `CE` table materialises the implicit blank unit at 100%
    /// (`Org::effective_entities`), preserving the corpus normal form.
    pub fn from_org(org: &Org, year: Year) -> ConsolidationUnits {
        let units = org
            .effective_entities()
            .into_iter()
            .map(|e| {
                let explicit = e.year_percent(year);
                Unit {
                    entity: e.code.trim().to_string(),
                    percent: if e.is_included() {
                        explicit.unwrap_or(DEFAULT_UNIT_PERCENT)
                    } else {
                        explicit.unwrap_or(0.0)
                    },
                    included: e.is_included(),
                    explicit_percent: explicit.is_some(),
                }
            })
            .collect();
        ConsolidationUnits { year, units }
    }

    /// Active units only (included and positive ownership).
    pub fn active(&self) -> impl Iterator<Item = &Unit> {
        self.units.iter().filter(|u| u.is_active())
    }

    pub fn get(&self, entity: &str) -> Option<&Unit> {
        let entity = entity.trim();
        self.units.iter().find(|u| u.entity == entity)
    }

    /// Active entity codes in `CE` order.
    pub fn codes(&self) -> Vec<String> {
        self.active().map(|u| u.entity.clone()).collect()
    }

    /// Sum of active ownership fractions (1.0 for a wholly owned group).
    pub fn total_weight(&self) -> f64 {
        self.active().map(Unit::weight).sum()
    }
}

/// One unit's contribution to one consolidated account.
#[derive(Clone, Debug, PartialEq)]
pub struct UnitAmount {
    pub entity: String,
    /// Ownership percent.
    pub percent: f64,
    /// Unweighted L5 balance for this entity.
    pub balance: f64,
    /// `balance * percent / 100`, rounded to cents when reported.
    pub amount: f64,
}

impl UnitAmount {
    pub fn cents(&self) -> i64 {
        to_cents(self.amount)
    }
}

/// One consolidated `BL.NUMBER` row.
#[derive(Clone, Debug, PartialEq)]
pub struct ConsolidatedAccount {
    pub number: String,
    /// Weighted total across active units.
    pub total: f64,
    /// Per-unit contributions, in `CE` order; units without rows are omitted.
    pub units: Vec<UnitAmount>,
    /// `CONSCHKSUM` value for this account.
    pub checksum: String,
}

impl ConsolidatedAccount {
    pub fn total_cents(&self) -> i64 {
        to_cents(self.total)
    }

    /// `true` when at least one unit carried a `BL` row.
    pub fn has_balance(&self) -> bool {
        !self.units.is_empty()
    }
}

/// A full consolidation for one `(year, group)` slice.
#[derive(Clone, Debug, PartialEq)]
pub struct Consolidation {
    pub year: Year,
    /// Reporting basis (`A` actual / `T` tax), uppercased.
    pub group: String,
    pub units: ConsolidationUnits,
    /// Number -> consolidated account, ordered by account number.
    pub accounts: BTreeMap<String, ConsolidatedAccount>,
    /// Digest of the complete consolidated picture.
    pub checksum: String,
}

impl Consolidation {
    /// Consolidate the `BL` cube against the `CE` organisation.
    pub fn build(cube: &Cube, org: &Org, year: Year, group: &str) -> Consolidation {
        let group = normalize_group(group);
        let units = ConsolidationUnits::from_org(org, year);

        // Every number that exists for this slice, across all entities.
        let mut numbers: Vec<String> = cube
            .rows()
            .iter()
            .filter(|r| r.year == year.index() as i32 && r.group == group)
            .map(|r| r.number.clone())
            .collect();
        numbers.sort();
        numbers.dedup();

        let mut accounts = BTreeMap::new();
        for number in numbers {
            let mut contributions = Vec::new();
            for unit in units.active() {
                if let Some(balance) =
                    entity_balance(cube, &number, &unit.entity, year, &group)
                {
                    let amount = balance * unit.weight();
                    contributions.push(UnitAmount {
                        entity: unit.entity.clone(),
                        percent: unit.percent,
                        balance,
                        amount,
                    });
                }
            }
            if contributions.is_empty() {
                continue;
            }
            let total: f64 = contributions.iter().map(|u| u.amount).sum();
            let checksum = account_checksum(&number, &group, year, to_cents(total));
            accounts.insert(
                number.clone(),
                ConsolidatedAccount { number, total, units: contributions, checksum },
            );
        }

        let mut out = Consolidation {
            year,
            group,
            units,
            accounts,
            checksum: String::new(),
        };
        out.checksum = out.recompute_checksum();
        out
    }

    pub fn account(&self, number: &str) -> Option<&ConsolidatedAccount> {
        self.accounts.get(number)
    }

    pub fn total(&self, number: &str) -> Option<f64> {
        self.account(number).map(|a| a.total)
    }

    /// Digest of the complete consolidated picture.
    pub fn checksum(&self) -> String {
        self.checksum.clone()
    }

    fn recompute_checksum(&self) -> String {
        let mut material = String::new();
        material.push_str("CONS|");
        material.push_str(&self.group);
        material.push('|');
        material.push_str(&self.year.index().to_string());
        material.push('\n');
        for account in self.accounts.values() {
            material.push_str(&account.number);
            material.push('|');
            material.push_str(&account.total_cents().to_string());
            for u in &account.units {
                material.push('|');
                material.push_str(&u.entity);
                material.push(':');
                material.push_str(&percent_bp(u.percent).to_string());
                material.push(':');
                material.push_str(&u.cents().to_string());
            }
            material.push('\n');
        }
        checksum8(material.as_bytes())
    }

    /// Recompute the digest and compare it with the stored field.
    pub fn verify(&self) -> bool {
        self.recompute_checksum() == self.checksum
    }

    /// Compare every `AM.CONSCHKSUM` cell for numbers in this consolidation.
    pub fn audit_table(&self, am: &Table) -> Result<ChecksumAudit> {
        if am.field_index("AC_NO").is_none() || am.field_index("CONSCHKSUM").is_none() {
            return Err(CoreError::Format(
                "AM audit requires AC_NO and CONSCHKSUM".into(),
            ));
        }
        let mut audit = ChecksumAudit::default();
        for r in 0..am.count() {
            if am.is_deleted(r) {
                continue;
            }
            let number = trimmed(am, r, "AC_NO");
            let Some(account) = self.accounts.get(&number) else {
                continue;
            };
            let stored = trimmed(am, r, "CONSCHKSUM");
            audit.checked += 1;
            if stored.eq_ignore_ascii_case(&account.checksum) {
                audit.matches += 1;
            } else {
                audit.mismatches += 1;
                if stored.is_empty() {
                    audit.missing += 1;
                }
                if audit.first_mismatch.is_none() {
                    audit.first_mismatch = Some(ChecksumMismatch {
                        row: r,
                        number: number.clone(),
                        stored,
                        expected: account.checksum.clone(),
                    });
                }
            }
        }
        Ok(audit)
    }

    /// Write the per-account digests into `AM.CONSCHKSUM`.
    ///
    /// Only rows whose `AC_NO` is consolidated are touched; returns the number
    /// of rows written. Writes are validated by [`write_cell`].
    pub fn stamp_table(&self, am: &mut Table) -> Result<usize> {
        if am.field_index("AC_NO").is_none() || am.field_index("CONSCHKSUM").is_none() {
            return Err(CoreError::Format(
                "AM stamping requires AC_NO and CONSCHKSUM".into(),
            ));
        }
        let mut written = 0;
        for r in 0..am.count() {
            if am.is_deleted(r) {
                continue;
            }
            let number = trimmed(am, r, "AC_NO");
            let Some(account) = self.accounts.get(&number) else {
                continue;
            };
            write_cell(am, r, "CONSCHKSUM", &account.checksum)?;
            written += 1;
        }
        Ok(written)
    }
}

/// Consolidate `cube` for one year/reporting basis.
pub fn consolidate(cube: &Cube, org: &Org, year: Year, group: &str) -> Consolidation {
    Consolidation::build(cube, org, year, group)
}

/// The L5 balance law restricted to one entity.
///
/// Mirrors [`Cube::balance`]: X-precedence on the opening layer plus non-`P`
/// `Y` movement. `None` when the entity has no rows for the key.
pub fn entity_balance(
    cube: &Cube,
    number: &str,
    entity: &str,
    year: Year,
    group: &str,
) -> Option<f64> {
    let entity = entity.trim();
    let group = normalize_group(group);
    let mut opening_x: Option<f64> = None;
    let mut opening_o: Option<f64> = None;
    let mut movement = 0.0;
    let mut any = false;
    for row in cube.rows() {
        if row.number != number
            || row.entity.trim() != entity
            || row.year != year.index() as i32
            || row.group != group
        {
            continue;
        }
        any = true;
        if row.bucket == "O" {
            if row.id == "X" && opening_x.is_none() {
                opening_x = Some(row.amt);
            } else if row.id == "O" && opening_o.is_none() {
                opening_o = Some(row.amt);
            }
        } else if row.bucket == "Y" && row.id != "P" {
            movement += row.amt;
        }
    }
    any.then(|| opening_x.or(opening_o).unwrap_or(0.0) + movement)
}

/// Per-account `CONSCHKSUM`: 8 uppercase hex digits over the canonical input.
pub fn account_checksum(number: &str, group: &str, year: Year, total_cents: i64) -> String {
    let material = format!(
        "CONSACCT|{}|{}|{}|{}",
        number.trim(),
        normalize_group(group),
        year.index(),
        total_cents
    );
    checksum8(material.as_bytes())
}

/// **Stand-in, not CaseWare's law.** 8-character uppercase hex digest used for
/// the checksum family.
///
/// FNV-1a 32-bit; stable across platforms and independent of locale. The
/// observed CW cells are uppercase 8-hex (`AM.CWCHECKSUM` corpus census), which
/// is the only property this reproduces. CaseWare's `CONSCHKSUM` is a copy of
/// `CWCHECKSUM` (see the module docs); `CWCHECKSUM`'s own producer is
/// unrecovered. Prefer this alias so the stand-in reads as one.
pub fn legacy_fnv(bytes: &[u8]) -> String {
    checksum8(bytes)
}

/// The stand-in digest body (see [`legacy_fnv`]).
pub fn checksum8(bytes: &[u8]) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for b in bytes {
        hash ^= *b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{hash:08X}")
}

/// CaseWare's own `AM.CONSCHKSUM` writer: **copy `CWCHECKSUM` verbatim**.
///
/// The writer law, exactly as the vendor performs it:
/// `CONSCHKSUM := str(CWCHECKSUM)`, executed on the row-insert path. This is
/// deliberately separate from the [`Consolidation`] stand-in digest — the two
/// answer different questions and must never be conflated.
///
/// Returns the number of rows written. A row whose `CWCHECKSUM` is blank is
/// copied as blank (that is what the vendor code does: `f4str` of a blank cell
/// yields the empty string). Rows missing either column are an error, because a
/// silently skipped row would make the copy look complete when it is not.
pub fn copy_cwchecksum_to_conschksum(am: &mut Table) -> Result<usize> {
    if am.field_index("CWCHECKSUM").is_none() || am.field_index("CONSCHKSUM").is_none() {
        return Err(CoreError::Format(
            "CONSCHKSUM copy requires CWCHECKSUM and CONSCHKSUM".into(),
        ));
    }
    // Validate every target cell first: `N`/`C 8` widths, never a partial write.
    let mut planned: Vec<(usize, String)> = Vec::new();
    for r in 0..am.count() {
        if am.is_deleted(r) {
            continue;
        }
        let src = am.cell_str(r, "CWCHECKSUM").unwrap_or_default().to_string();
        planned.push((r, src));
    }
    let mut scratch = am.clone();
    for (r, src) in &planned {
        write_cell(&mut scratch, *r, "CONSCHKSUM", src)?;
    }
    for (r, src) in &planned {
        write_cell(am, *r, "CONSCHKSUM", src)?;
    }
    Ok(planned.len())
}

/// Rows where `CONSCHKSUM` differs from `CWCHECKSUM` (the copy is stale).
///
/// A clean table has none; this is the invariant
/// [`copy_cwchecksum_to_conschksum`] establishes and the gate pins.
pub fn conschksum_copy_drift(am: &Table) -> Vec<usize> {
    let mut out = Vec::new();
    for r in 0..am.count() {
        if am.is_deleted(r) {
            continue;
        }
        let a = am.cell_str(r, "CWCHECKSUM").unwrap_or_default();
        let b = am.cell_str(r, "CONSCHKSUM").unwrap_or_default();
        if a.trim() != b.trim() {
            out.push(r);
        }
    }
    out
}

/// Result of comparing `AM.CONSCHKSUM` against computed digests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChecksumAudit {
    /// Rows whose `AC_NO` is in the consolidation.
    pub checked: usize,
    pub matches: usize,
    /// Stored cells that did not match (includes blanks).
    pub mismatches: usize,
    /// Mismatching cells that were blank.
    pub missing: usize,
    pub first_mismatch: Option<ChecksumMismatch>,
}

impl ChecksumAudit {
    pub fn is_clean(&self) -> bool {
        self.checked > 0 && self.mismatches == 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChecksumMismatch {
    pub row: usize,
    pub number: String,
    pub stored: String,
    pub expected: String,
}

fn normalize_group(group: &str) -> String {
    let g = group.trim().to_ascii_uppercase();
    if g.is_empty() {
        "A".to_string()
    } else {
        g
    }
}

fn trimmed(t: &Table, r: usize, column: &str) -> String {
    t.cell_str(r, column).unwrap_or_default().trim().to_string()
}

fn to_cents(value: f64) -> i64 {
    (value * 100.0).round() as i64
}

fn percent_bp(value: f64) -> i64 {
    (value * 100.0).round() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn corpus_file(tag: &str, suffix: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/golden/dbf")
            .join(tag);
        let want = suffix.to_ascii_lowercase();
        std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read {dir:?}: {e}"))
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().to_ascii_lowercase().ends_with(&want))
                    .unwrap_or(false)
            })
            .unwrap_or_else(|| panic!("no *{suffix} in {dir:?}"))
    }

    fn corpus_cube(tag: &str) -> Cube {
        Cube::from_table(&Table::parse_path(corpus_file(tag, "BL.dbf")).expect("BL"))
            .expect("cube")
    }

    #[test]
    fn checksum_is_eight_uppercase_hex_and_changes() {
        let a = checksum8(b"alpha");
        let b = checksum8(b"beta");
        assert_eq!(a.len(), CHECKSUM_LEN);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase()));
        assert_ne!(a, b);
        assert_eq!(a, checksum8(b"alpha"));
    }

    #[test]
    fn account_checksum_tracks_cents_only() {
        let base = account_checksum("1000", "A", Year::Current, 100_00);
        assert_eq!(base, account_checksum(" 1000 ", "a", Year::Current, 100_00));
        assert_ne!(base, account_checksum("1000", "A", Year::Current, 101_00));
        assert_ne!(base, account_checksum("1000", "A", Year::Prior1, 100_00));
    }

    #[test]
    fn corpus_single_blank_matches_cube_and_stamps_am() {
        let cube = corpus_cube("corp2026");
        let cons = consolidate(&cube, &Org::default(), Year::Current, "A");
        assert!(!cons.accounts.is_empty());
        for (number, account) in &cons.accounts {
            let expected = cube.balance(number, 0, "A").expect("number in cube");
            assert!(
                (account.total - expected).abs() < 1e-6,
                "{number}: {} != {expected}",
                account.total
            );
        }

        let mut am = Table::parse_path(corpus_file("corp2026", "AM.dbf")).expect("AM");
        let written = cons.stamp_table(&mut am).expect("stamp");
        assert!(written > 50, "only {written} AM rows stamped");
        let audit = cons.audit_table(&am).expect("audit");
        assert_eq!(audit.matches, written);
        assert_eq!(audit.mismatches, 0);
        assert!(audit.is_clean());

        let target = (0..am.count())
            .find(|&r| !am.is_deleted(r) && cons.account(&trimmed(&am, r, "AC_NO")).is_some())
            .expect("stamped row");
        write_cell(&mut am, target, "CONSCHKSUM", "").expect("blank");
        let audit = cons.audit_table(&am).expect("audit");
        assert_eq!(audit.missing, 1);
        assert_eq!(audit.mismatches, 1);
    }
}

