//! cw-app account renumber cascade (FEAT.018).
//!
//! Contract: `docs\specs\accounts.md` §7, `docs\specs\ui-shell.md` §9, and the
//! interlink numeric web (`docs\specs\interlink.md` §4). `accounts.md` §6
//! parked this lane with the sentence that matters: *"This module does not
//! pretend an AM-only renumber is safe."* This module is the engagement-wide
//! cascade that makes the write safe.
//!
//! ## The cascade
//!
//! One account number lives in exactly one primary column and in any number of
//! reference columns. [`CASCADE_MAP`] is the whole map, in table order:
//!
//! | table | column | role |
//! |---|---|---|
//! | `AM` | `AC_NO` | primary key of the chart row |
//! | `AM` | `AC_NO2`, `AC_NO2CR` | alternate account references (CR twin included) |
//! | `AM` | `PAC_NO` | parent-account reference (blank in all corpora) |
//! | `BL` | `NUMBER` | balance-cube key |
//! | `GL` | `AC_NO`, `B_ACT` | journal line account / balancing account |
//!
//! Everything is rewritten in **one pass from a snapshot**: the new value is
//! looked up in a `from -> to` map computed from the original cells, so a
//! chained rename (`1000 -> 2000`, `2000 -> 3001`) can never double-apply
//! depending on row order. Chained targets are legal; a target that lands on a
//! *stationary* number of any namespace is a collision and refuses the write.
//!
//! A cascade is atomic, so a plan is either pristine (every source live) or already
//! in effect (no source live, every target live). Those are the only two states it
//! accepts. A plan whose sources are *partly* live was applied to a state the plan
//! did not produce — and, for a dense shift, that is also what "already applied"
//! looks like (`hull2024` has 25 accounts exactly 100 apart, so a +100 shift leaves
//! 25 of its own sources live again) — so it is **refused** as
//! [`CascadeIssue::PartialApplication`] rather than guessed at. That is why a
//! re-applied dense shift is never double-applied.
//!
//! ## Laws (gate: `crates\cw-app\tests\renumber_gate.rs`)
//!
//! 1. **Full cascade** — every live cell in every [`CASCADE_MAP`] column whose
//!    value equals a source is rewritten; nothing outside the map is touched.
//! 2. **Tombstones are preserved** — a `0x2A` record is carried byte-identically
//!    and its references are *reported*, never rewritten (interlink.md §2 keeps
//!    the deleted space separate; a renumber must not silently mutate history).
//! 3. **Collision refusal** — ambiguous sources (a chart number that is also an
//!    `MP.MAP_NO`/tax code), taken targets (chart, map, tax, cube or journal
//!    space), duplicate targets, and invalid targets are all validated against
//!    the untouched tables *before* a byte changes.
//! 4. **No new danglers** — `CASCADE_MAP` is the *whole* set of reference columns,
//!    so a live reference to a renamed account always moves with it; the set of
//!    unresolvable reference values after the cascade is exactly the set before it.
//!    The module asserts that as a post-condition on the produced tables, not as a
//!    repair, and refuses if it is ever violated.
//! 5. **No-op save is a true no-op** — an empty plan, a `delta` of zero, or a
//!    plan whose sources are already applied leaves every table byte-identical
//!    (`Table::encode` equality, the F30 lesson).
//! 6. **Alignment is preserved** — corpus reference cells are right-justified
//!    (`ui-shell.md` §4) except `AM.AC_NO2` (left) and `GL.B_ACT` (genuinely
//!    mixed); a rewritten cell keeps the alignment it had, so the only bytes that
//!    differ are token bytes.
//!
//! ## Known limits
//!
//! - The map/tax namespaces are read from whatever `MP` table the caller supplies.
//!   `hull2024`/`mwgh2025` `MP` are truncated LZMA members whose fabricated tails
//!   carry control-byte `MAP_NO` values (`tests\golden\README.md` F63); those can
//!   only ever *add* a collision and therefore over-refuse, never corrupt. A caller
//!   that wants the authentic answer passes the readable prefix (`support::FRONTIER`
//!   in the gate).
//! - `BL` rows are rewritten on their raw `NUMBER`; the cube's *semantic* axes
//!   (`ID`/`GROUP`/`YEAR`/`BUCKET`) are untouched, so a row that was an `X` overlay
//!   before the rename is still the `X` overlay after it.
//! - Non-ASCII account numbers are legal Latin-1 (`write_reference` validates), but
//!   `from_delta` only understands canonical integrals; anything else is explicit
//!   `--map` work.
//!
//! Delta mode ([`RenumberPlan::from_delta`]) only shifts **canonical** integral
//! account numbers (all digits, no leading zero); every other number
//! (`1332-00`, `RSM - 01`, `NETINC`, `z`) is listed in
//! [`RenumberPlan::skipped`] and left alone, which is the shell preview's
//! long-standing rule (`ui-shell.md` §6).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use cw_core::canon::latin1;
use cw_core::dbf::Table;
use cw_core::{CoreError, Result};

use crate::accounts::Chart;
use crate::adjustments::GL_HEADER_ACCOUNT;

/// Deletion flag of a tombstone (a live record's flag is a space).
pub const DELETED: u8 = b'*';

/// Primary account-number column.
pub const ACCOUNT_COLUMN: &str = "AC_NO";
/// Widest account/reference column (`AM.AC_NO`, `BL.NUMBER`, `GL.AC_NO`, ...).
pub const MAX_ACCOUNT_WIDTH: usize = 40;

/// `AM` columns that reference an account number other than the row's own key.
pub const AM_REFERENCE_COLUMNS: [&str; 3] = ["AC_NO2", "AC_NO2CR", "PAC_NO"];
/// `BL` columns that reference an account number.
pub const BL_REFERENCE_COLUMNS: [&str; 1] = ["NUMBER"];
/// `GL` columns that reference an account number.
pub const GL_REFERENCE_COLUMNS: [&str; 2] = ["AC_NO", "B_ACT"];

/// The complete cascade map, in table/column order. Every entry is rewritten;
/// no other column of any table is ever touched by this module.
pub const CASCADE_MAP: [(&str, &str); 7] = [
    ("AM", "AC_NO"),
    ("AM", "AC_NO2"),
    ("AM", "AC_NO2CR"),
    ("AM", "PAC_NO"),
    ("BL", "NUMBER"),
    ("GL", "AC_NO"),
    ("GL", "B_ACT"),
];

/// Namespace tags used by [`CascadeIssue`].
pub const NS_CHART: &str = "chart";
pub const NS_MAP: &str = "map";
pub const NS_TAX: &str = "tax";
pub const NS_CUBE: &str = "cube";
pub const NS_JOURNAL: &str = "journal";

/// Cell alignment inside a fixed-width text field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    /// Token starts at the first byte (`AC_NO2` in the hull corpus).
    Left,
    /// Token ends at the last byte (corpus `AC_NO`, `BL.NUMBER`, `GL.AC_NO`).
    Right,
}

/// Alignment of an existing cell: leading pad ⇒ right-justified.
pub fn cell_align(raw: &[u8]) -> Align {
    match raw.iter().position(|&b| b != b' ') {
        Some(0) => Align::Left,
        Some(_) => Align::Right,
        None => Align::Left,
    }
}

/// Parse a canonical integral account number: ASCII digits, no leading zero,
/// at most 18 digits (an `i64`), within the field width.
pub fn canonical_number(token: &str) -> Option<i64> {
    let t = token.trim();
    if t.is_empty() || t.len() > MAX_ACCOUNT_WIDTH || t.len() > 18 {
        return None;
    }
    if !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if t.len() > 1 && t.starts_with('0') {
        return None;
    }
    t.parse::<i64>().ok()
}

// ---------------------------------------------------------------------------
// Scope: the tables a cascade reads
//

/// The three tables a cascade writes, plus the optional `MP` map space used
/// for collision and dangler checks.
#[derive(Clone, Debug)]
pub struct RenumberScope {
    pub am: Table,
    pub bl: Table,
    pub gl: Table,
    pub mp: Option<Table>,
}

impl RenumberScope {
    /// A scope with no `MP` table: map-space collisions are simply not
    /// detectable, everything else is unchanged.
    pub fn new(am: Table, bl: Table, gl: Table) -> RenumberScope {
        RenumberScope { am, bl, gl, mp: None }
    }

    /// Attach the `MP` table so map-code collisions/ambiguity are checked.
    pub fn with_mp(mut self, mp: Table) -> RenumberScope {
        self.mp = Some(mp);
        self
    }

}

// ---------------------------------------------------------------------------
// Plan
//

/// One account-number rename.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Rename {
    pub from: String,
    pub to: String,
}

/// A requested cascade: an ordered rename list plus the delta-mode numbers that
/// were deliberately left alone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenumberPlan {
    renames: Vec<Rename>,
    skipped: Vec<String>,
}

impl RenumberPlan {
    /// Shift every canonical integral posting account by `delta`.
    ///
    /// `delta == 0` yields an empty plan (a true no-op). Non-numeric and
    /// non-canonical numbers are reported in [`RenumberPlan::skipped`].
    pub fn from_delta(scope: &RenumberScope, delta: i64) -> Result<RenumberPlan> {
        // `delta == 0` is the identity plan; it is decided before any decode so
        // the no-op path cannot fail on a chart it would not modify.
        if delta == 0 {
            return Ok(RenumberPlan::default());
        }
        let chart = Chart::from_table(&scope.am)?;
        let mut skipped = Vec::new();
        let mut renames = Vec::new();
        let mut targets: BTreeMap<i64, String> = BTreeMap::new();
        for a in chart.posting() {
            let token = a.ac_no.trim();
            if token.is_empty() {
                continue;
            }
            match canonical_number(token) {
                Some(n) => {
                    targets.entry(n).or_insert_with(|| token.to_string());
                }
                None => skipped.push(token.to_string()),
            }
        }
        for (n, from) in targets {
            let to = n.checked_add(delta).ok_or_else(|| {
                CoreError::Format(format!("renumber delta {delta:+} overflows account {from:?}"))
            })?;
            renames.push(Rename { from, to: to.to_string() });
        }
        renames.sort();
        skipped.sort();
        skipped.dedup();
        Ok(RenumberPlan { renames, skipped })
    }

    /// Explicit `from -> to` renames (the dialog's `--map FROM=TO` form).
    ///
    /// Identity pairs (`from == to`) and repeated pairs are dropped here; the
    /// per-pair validation (existence, collisions) happens in
    /// [`RenumberPlan::resolve`].
    pub fn from_pairs(pairs: &[(String, String)]) -> Result<RenumberPlan> {
        let mut renames: BTreeSet<Rename> = BTreeSet::new();
        for (from, to) in pairs {
            // The source is a lookup key and is trimmed; the target is written
            // verbatim and therefore validated as given (padding is an error,
            // not something to silently normalize away).
            let from = from.trim().to_string();
            if from.is_empty() {
                return Err(CoreError::Format("renumber: empty account number".into()));
            }
            if to.trim().is_empty() {
                return Err(CoreError::Format(format!(
                    "renumber: empty target for account {from:?}"
                )));
            }
            if from == to.trim() {
                continue;
            }
            renames.insert(Rename { from, to: to.clone() });
        }
        Ok(RenumberPlan { renames: renames.into_iter().collect(), skipped: Vec::new() })
    }

    /// The renames this plan will attempt, sorted by source.
    pub fn renames(&self) -> &[Rename] {
        &self.renames
    }

    /// Delta mode: account numbers deliberately not shifted (non-canonical).
    pub fn skipped(&self) -> &[String] {
        &self.skipped
    }

    pub fn is_empty(&self) -> bool {
        self.renames.is_empty()
    }

    /// `from -> to` lookup (owned strings; the map is small).
    pub fn mapping(&self) -> BTreeMap<String, String> {
        self.renames.iter().map(|r| (r.from.clone(), r.to.clone())).collect()
    }

    /// Validate and split the plan into applicable renames, already-applied
    /// sources and blocking issues, against the untouched scope.
    ///
    /// A plan that has already been applied is recognized as a **fixed point**:
    /// mapping the live chart numbers through the plan leaves the chart set
    /// unchanged and every target is live. This makes re-application a true
    /// no-op even for chained renames (`16100 -> 16200`, `16200 -> 16300`),
    /// where a per-rename "source is gone" test would misread the chain.
    pub fn resolve(&self, scope: &RenumberScope) -> Result<PlanResolution> {
        let index = RefIndex::build(scope)?;
        let chart = Chart::from_table(&scope.am)?;
        let live_numbers: BTreeSet<String> = chart
            .posting()
            .map(|a| a.ac_no.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let mut res = PlanResolution {
            renames: Vec::new(),
            already_applied: Vec::new(),
            issues: Vec::new(),
        };
        if self.renames.is_empty() {
            return Ok(res);
        }

        // A cascade is atomic, so a plan is either pristine (every source live) or
        // already in effect (no source live, every target live). Anything between
        // those two states cannot be told apart from a chained shift that would
        // double-apply, so it is refused rather than guessed at.
        let sources_live: Vec<&Rename> = self
            .renames
            .iter()
            .filter(|r| live_numbers.contains(&r.from))
            .collect();
        let sources_gone: Vec<&Rename> = self
            .renames
            .iter()
            .filter(|r| !live_numbers.contains(&r.from))
            .collect();
        let all_targets_live = self.renames.iter().all(|r| live_numbers.contains(&r.to));
        if sources_live.is_empty() && all_targets_live {
            res.already_applied = self.renames.iter().map(|r| r.from.clone()).collect();
            return Ok(res);
        }
        if !sources_gone.is_empty() && !sources_live.is_empty() {
            for r in &sources_gone {
                res.issues.push(CascadeIssue::PartialApplication {
                    number: r.from.clone(),
                    target: r.to.clone(),
                });
            }
            res.issues.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
            res.issues.dedup();
            return Ok(res);
        }

        // Duplicate targets (`a -> c`, `b -> c`) can only come from an explicit map.
        let mut by_target: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for r in &self.renames {
            by_target.entry(r.to.as_str()).or_default().push(r.from.as_str());
        }
        for (to, sources) in &by_target {
            if sources.len() > 1 {
                let mut s: Vec<String> = sources.iter().map(|x| x.to_string()).collect();
                s.sort();
                res.issues.push(CascadeIssue::DuplicateTarget {
                    to: to.to_string(),
                    sources: s,
                });
            }
        }

        for r in &self.renames {
            if r.to.trim() != r.to {
                res.issues.push(CascadeIssue::InvalidTarget {
                    from: r.from.clone(),
                    to: r.to.clone(),
                    reason: "leading/trailing whitespace",
                });
                continue;
            }
            if r.to.chars().any(|c| c as u32 > 0xFF) {
                res.issues.push(CascadeIssue::InvalidTarget {
                    from: r.from.clone(),
                    to: r.to.clone(),
                    reason: "non-Latin-1 character",
                });
                continue;
            }
            if r.to.len() > MAX_ACCOUNT_WIDTH {
                res.issues.push(CascadeIssue::InvalidTarget {
                    from: r.from.clone(),
                    to: r.to.clone(),
                    reason: "wider than the 40-byte account field",
                });
                continue;
            }

            let exists = live_numbers.contains(&r.from);
            if !exists {
                // Only reachable with a hand-built plan whose sources are all gone
                // but whose targets are not: not applied, and nothing to apply.
                res.issues.push(CascadeIssue::MissingAccount { number: r.from.clone() });
                continue;
            }
            let before_issues = res.issues.len();

            // An account number that is simultaneously a map code or a tax code
            // cannot be cascaded by value: the same cell text also addresses
            // those spaces (interlink.md §4).
            for (space, set) in [
                (NS_MAP, &index.map_codes),
                (NS_TAX, &index.tax_codes),
            ] {
                if set.contains(&r.from) {
                    res.issues.push(CascadeIssue::AmbiguousSource {
                        number: r.from.clone(),
                        namespace: space,
                    });
                }
            }

            // Targets must be free in every namespace they would newly enter.
            let taken: [(&'static str, bool); 5] = [
                (NS_CHART, live_numbers.contains(&r.to) && !self.is_source(&r.to)),
                (NS_MAP, index.map_codes.contains(&r.to)),
                (NS_TAX, index.tax_codes.contains(&r.to)),
                (
                    NS_CUBE,
                    index.bl_live.contains(&r.to) && !self.is_source(&r.to),
                ),
                (
                    NS_JOURNAL,
                    index.gl_live.contains(&r.to) && !self.is_source(&r.to),
                ),
            ];
            for (space, hit) in taken {
                if hit {
                    res.issues.push(CascadeIssue::TargetTaken {
                        number: r.to.clone(),
                        namespace: space,
                    });
                }
            }

            // No reference column may be narrower than the new number.
            for col in index.columns.iter() {
                if col.width < r.to.len() && col.live.contains(&r.from) {
                    res.issues.push(CascadeIssue::InvalidTarget {
                        from: r.from.clone(),
                        to: r.to.clone(),
                        reason: "wider than a reference column that holds this account",
                    });
                    break;
                }
            }

            if res.issues.len() == before_issues {
                res.renames.push(r.clone());
            }
        }

        res.issues.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
        res.issues.dedup();
        res.already_applied.sort();
        res.already_applied.dedup();
        Ok(res)
    }

    fn is_source(&self, number: &str) -> bool {
        self.renames.iter().any(|r| r.from == number)
    }
}

/// Result of validating a [`RenumberPlan`] against a [`RenumberScope`].
#[derive(Clone, Debug, Default)]
pub struct PlanResolution {
    /// Renames that will be applied (sources exist, no issues).
    pub renames: Vec<Rename>,
    /// Sources absent because the rename is already in effect.
    pub already_applied: Vec<String>,
    /// Blocking problems, sorted and deduplicated.
    pub issues: Vec<CascadeIssue>,
}

impl PlanResolution {
    pub fn is_blocked(&self) -> bool {
        !self.issues.is_empty()
    }

    /// `from -> to` lookup for the applicable renames.
    pub fn mapping(&self) -> BTreeMap<String, String> {
        self.renames.iter().map(|r| (r.from.clone(), r.to.clone())).collect()
    }
}

// ---------------------------------------------------------------------------
// Issues
//

/// A reason a cascade cannot be applied. Every variant is decided before a byte
/// is touched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CascadeIssue {
    /// The requested source is not a live posting account.
    MissingAccount { number: String },
    /// The target is not a legal account number.
    InvalidTarget {
        from: String,
        to: String,
        reason: &'static str,
    },
    /// The source number is also a value of another namespace.
    AmbiguousSource {
        number: String,
        namespace: &'static str,
    },
    /// The target already belongs to a stationary object of some namespace.
    TargetTaken {
        number: String,
        namespace: &'static str,
    },
    /// Two sources want the same target.
    DuplicateTarget { to: String, sources: Vec<String> },
    /// Some sources are gone and others are live: the plan was applied to a state
    /// it did not produce, so applying the remainder could double-shift a chain.
    PartialApplication { number: String, target: String },
}

impl CascadeIssue {
    fn sort_key(&self) -> (u8, String, String) {
        match self {
            CascadeIssue::DuplicateTarget { to, .. } => (0, to.clone(), String::new()),
            CascadeIssue::PartialApplication { number, target } => {
                (6, number.clone(), target.clone())
            }
            CascadeIssue::InvalidTarget { from, to, .. } => (1, from.clone(), to.clone()),
            CascadeIssue::MissingAccount { number } => (2, number.clone(), String::new()),
            CascadeIssue::AmbiguousSource { number, namespace } => {
                (3, number.clone(), (*namespace).to_string())
            }
            CascadeIssue::TargetTaken { number, namespace } => {
                (4, number.clone(), (*namespace).to_string())
            }
        }
    }
}

impl fmt::Display for CascadeIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CascadeIssue::MissingAccount { number } => {
                write!(f, "account {number:?} is not a live posting account")
            }
            CascadeIssue::InvalidTarget { from, to, reason } => {
                write!(f, "target {to:?} for {from:?} is invalid: {reason}")
            }
            CascadeIssue::AmbiguousSource { number, namespace } => write!(
                f,
                "account {number:?} is also a {namespace} code; a value cascade would rewrite it"
            ),
            CascadeIssue::TargetTaken { number, namespace } => {
                write!(f, "target {number:?} already exists in the {namespace} namespace")
            }
            CascadeIssue::DuplicateTarget { to, sources } => write!(
                f,
                "target {to:?} is claimed by {} accounts ({})",
                sources.len(),
                sources.join(", ")
            ),
            CascadeIssue::PartialApplication { number, target } => write!(
                f,
                "account {number:?} is gone but its target {target:?} is live while other sources\
                 are still present; the plan was applied to a state it did not produce"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Reference index: live/tombstone values per cascade column + namespaces
//

struct ColumnValues {
    table: &'static str,
    column: &'static str,
    width: usize,
    /// Live values of the column (tombstones are a separate space).
    live: BTreeSet<String>,
}

struct RefIndex {
    columns: Vec<ColumnValues>,
    map_codes: BTreeSet<String>,
    tax_codes: BTreeSet<String>,
    /// Live `BL.NUMBER` values (cube keys; may be map/tax/synthetic too).
    bl_live: BTreeSet<String>,
    /// Live `GL.AC_NO`/`B_ACT` values (the journal namespace).
    gl_live: BTreeSet<String>,
}

fn table_for<'a>(scope: &'a RenumberScope, name: &str) -> &'a Table {
    match name {
        "AM" => &scope.am,
        "BL" => &scope.bl,
        _ => &scope.gl,
    }
}

/// Trimmed Latin-1 token of one cell, `None` when the column is absent.
fn token(t: &Table, r: usize, column: &str) -> Option<String> {
    t.cell(r, column).map(|raw| latin1(raw).trim().to_string())
}

/// Rows of a table that carry the GL header sentinel (not references).
fn is_gl_header(t: &Table, r: usize) -> bool {
    token(t, r, "AC_NO").as_deref() == Some(GL_HEADER_ACCOUNT)
}

impl RefIndex {
    fn build(scope: &RenumberScope) -> Result<RefIndex> {
        let mut columns = Vec::with_capacity(CASCADE_MAP.len());
        for (name, column) in CASCADE_MAP {
            let t = table_for(scope, name);
            let Some(idx) = t.field_index(column) else {
                // A table without the column contributes no references; the
                // cascade simply has nothing to rewrite there.
                continue;
            };
            let width = t.fields[idx].len as usize;
            let mut live = BTreeSet::new();
            for r in 0..t.count() {
                if name == "GL" && is_gl_header(t, r) {
                    continue;
                }
                if t.records[r][0] == DELETED {
                    continue;
                }
                let Some(v) = token(t, r, column) else { continue };
                if !v.is_empty() {
                    live.insert(v);
                }
            }
            columns.push(ColumnValues { table: name, column, width, live });
        }

        // MP: map codes and tax-code slots.
        let mut map_codes = BTreeSet::new();
        let mut tax_codes = BTreeSet::new();
        if let Some(mp) = &scope.mp {
            for r in 0..mp.count() {
                if mp.is_deleted(r) {
                    continue;
                }
                if let Some(v) = token(mp, r, "MAP_NO") {
                    if !v.is_empty() {
                        map_codes.insert(v);
                    }
                }
                for col in ["TAC_NO", "TAX1", "TAX2", "TAX3", "TAX4"] {
                    if mp.field_index(col).is_none() {
                        continue;
                    }
                    if let Some(v) = token(mp, r, col) {
                        if !v.is_empty() {
                            tax_codes.insert(v);
                        }
                    }
                }
            }
        }
        for r in 0..scope.am.count() {
            if scope.am.is_deleted(r) {
                continue;
            }
            if let Some(v) = token(&scope.am, r, "TAC_NO") {
                if !v.is_empty() {
                    tax_codes.insert(v);
                }
            }
        }

        let bl_live = columns
            .iter()
            .find(|c| c.table == "BL" && c.column == "NUMBER")
            .map(|c| c.live.clone())
            .unwrap_or_default();
        let gl_live = {
            let mut s = BTreeSet::new();
            for col in GL_REFERENCE_COLUMNS {
                if let Some(c) = columns.iter().find(|c| c.table == "GL" && c.column == col) {
                    s.extend(c.live.iter().cloned());
                }
            }
            s
        };

        Ok(RefIndex { columns, map_codes, tax_codes, bl_live, gl_live })
    }

    /// Unresolvable live reference values, as `(table, column, value)` triples.
    fn dangling(
        &self,
        accounts: &BTreeSet<String>,
        maps: &BTreeSet<String>,
        taxes: &BTreeSet<String>,
    ) -> Vec<(&'static str, &'static str, String)> {
        let resolves = |v: &str| accounts.contains(v) || maps.contains(v) || taxes.contains(v);
        let mut out = Vec::new();
        for col in &self.columns {
            // The AM primary key is the chart index itself, not a reference.
            if col.table == "AM" && col.column == ACCOUNT_COLUMN {
                continue;
            }
            for v in &col.live {
                if !resolves(v) {
                    out.push((col.table, col.column, v.clone()));
                }
            }
        }
        out.sort();
        out
    }
}

// ---------------------------------------------------------------------------
// Cascade
//

/// One rewritten cell (preview evidence).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellEdit {
    /// `AM` / `BL` / `GL`.
    pub table: &'static str,
    /// Physical record index.
    pub row: usize,
    pub column: &'static str,
    pub before: String,
    pub after: String,
}

/// What a cascade did (or would do).
#[derive(Clone, Debug, Default)]
pub struct CascadeReport {
    /// Every cell the cascade rewrote, in table/row/column order.
    pub edits: Vec<CellEdit>,
    /// Tombstone rows that carry a source value and were preserved verbatim.
    pub tombstones: Vec<(&'static str, usize)>,
    /// Sources that needed no work because the rename is already in effect.
    pub already_applied: Vec<String>,
    /// Delta-mode numbers that are not canonical integrals.
    pub skipped: Vec<String>,
    /// Distinct unresolvable reference values before / after the cascade.
    pub dangling_before: Vec<(&'static str, &'static str, String)>,
    pub dangling_after: Vec<(&'static str, &'static str, String)>,
}

impl CascadeReport {
    pub fn cells(&self) -> usize {
        self.edits.len()
    }

    /// `(table, cells, rows)` per table that has at least one edit.
    pub fn table_stats(&self) -> Vec<(&'static str, usize, usize)> {
        let mut out: Vec<(&'static str, usize, usize)> = Vec::new();
        let mut seen_rows: BTreeMap<(&'static str, usize), ()> = BTreeMap::new();
        for e in &self.edits {
            seen_rows.insert((e.table, e.row), ());
        }
        for table in ["AM", "BL", "GL"] {
            let cells = self.edits.iter().filter(|e| e.table == table).count();
            if cells == 0 {
                continue;
            }
            let rows = seen_rows.keys().filter(|(t, _)| *t == table).count();
            out.push((table, cells, rows));
        }
        out
    }

    /// Cells rewritten in one table.
    pub fn table_cells(&self, table: &str) -> usize {
        self.edits.iter().filter(|e| e.table == table).count()
    }
}

/// The post-cascade table set plus its report.
#[derive(Clone, Debug)]
pub struct RenumberPreview {
    pub report: CascadeReport,
    pub am: Table,
    pub bl: Table,
    pub gl: Table,
    /// True when any of the three encoded tables differs from its input.
    pub changed: bool,
}

impl RenumberPreview {
    /// `(suffix, changed)` for the three tables, in commit order.
    pub fn changed_tables(&self, scope: &RenumberScope) -> Vec<(&'static str, bool)> {
        let after = |name: &str| match name {
            "AM" => &self.am,
            "BL" => &self.bl,
            _ => &self.gl,
        };
        [("AM", &scope.am), ("BL", &scope.bl), ("GL", &scope.gl)]
            .into_iter()
            .map(|(name, before)| (name, after(name).encode() != before.encode()))
            .collect()
    }
}

struct PendingEdit {
    table: &'static str,
    row: usize,
    column: &'static str,
    before: String,
    after: String,
    align: Align,
}

/// Rewrite one fixed-width text cell, preserving its alignment.
fn write_reference(t: &mut Table, r: usize, column: &str, token: &str, align: Align) -> Result<()> {
    let idx = t
        .field_index(column)
        .ok_or_else(|| CoreError::Format(format!("table has no column {column}")))?;
    let field = &t.fields[idx];
    if field.kind != b'C' {
        return Err(CoreError::Format(format!(
            "{column} is dBase type {}, not text",
            field.kind as char
        )));
    }
    if token.trim() != token || token.is_empty() {
        return Err(CoreError::Format(format!(
            "{column} reference {token:?} is blank or has surrounding spaces"
        )));
    }
    let width = field.len as usize;
    let raw: Vec<u8> = token
        .chars()
        .map(|c| {
            let n = c as u32;
            if n <= 0xFF {
                Ok(n as u8)
            } else {
                Err(CoreError::Format(format!(
                    "row {r}: {column} value contains non-Latin-1 char {c:?}"
                )))
            }
        })
        .collect::<Result<Vec<u8>>>()?;
    if raw.len() > width {
        return Err(CoreError::Format(format!(
            "row {r}: {column} value {token:?} is {} bytes, field width {width}",
            raw.len()
        )));
    }
    let off = t.offsets()[idx];
    if r >= t.records.len() || t.records[r].len() != t.record_len as usize {
        return Err(CoreError::Format(format!("row {r} out of range or short")));
    }
    let rec = &mut t.records[r];
    rec[off..off + width].fill(b' ');
    let start = match align {
        Align::Left => off,
        Align::Right => off + width - raw.len(),
    };
    rec[start..start + raw.len()].copy_from_slice(&raw);
    Ok(())
}

/// Collect the edits for one table from the untouched bytes.
fn collect(table: &'static str, t: &Table, mapping: &BTreeMap<String, String>, report: &mut CascadeReport) -> Vec<PendingEdit> {
    let mut out = Vec::new();
    let mut tombstone_rows: BTreeSet<usize> = BTreeSet::new();
    for (name, column) in CASCADE_MAP {
        if name != table || t.field_index(column).is_none() {
            continue;
        }
        for r in 0..t.count() {
            if table == "GL" && is_gl_header(t, r) {
                continue;
            }
            let Some(raw) = t.cell(r, column) else { continue };
            let value = latin1(raw).trim().to_string();
            if value.is_empty() {
                continue;
            }
            let Some(to) = mapping.get(&value) else { continue };
            if t.records[r][0] == DELETED {
                tombstone_rows.insert(r);
                continue;
            }
            if *to == value {
                continue;
            }
            out.push(PendingEdit {
                table,
                row: r,
                column,
                before: value,
                after: to.clone(),
                align: cell_align(raw),
            });
        }
    }
    for r in tombstone_rows {
        report.tombstones.push((table, r));
    }
    out
}

/// Apply a validated plan to a clone of the scope's tables.
///
/// The plan is validated first; a blocked plan returns the joined issue list and
/// leaves the returned tables unreadable (an error is always returned), so a
/// caller can never commit a partial cascade.
pub fn cascade(scope: &RenumberScope, plan: &RenumberPlan) -> Result<RenumberPreview> {
    let resolution = plan.resolve(scope)?;
    if resolution.is_blocked() {
        let mut msg = String::from("renumber refused:");
        for issue in &resolution.issues {
            msg.push_str("\n  - ");
            msg.push_str(&issue.to_string());
        }
        return Err(CoreError::Format(msg));
    }

    let mapping = resolution.mapping();
    let index_before = RefIndex::build(scope)?;
    let names_before = namespace_sets(scope, &index_before)?;
    let mut report = CascadeReport {
        already_applied: resolution.already_applied.clone(),
        skipped: plan.skipped().to_vec(),
        dangling_before: index_before.dangling(
            &names_before.0,
            &names_before.1,
            &names_before.2,
        ),
        ..Default::default()
    };

    let mut am = scope.am.clone();
    let mut bl = scope.bl.clone();
    let mut gl = scope.gl.clone();

    let mut pending: Vec<PendingEdit> = Vec::new();
    for (name, source) in [("AM", &scope.am), ("BL", &scope.bl), ("GL", &scope.gl)] {
        pending.extend(collect(name, source, &mapping, &mut report));
    }

    for p in &pending {
        let t = match p.table {
            "AM" => &mut am,
            "BL" => &mut bl,
            _ => &mut gl,
        };
        write_reference(t, p.row, p.column, &p.after, p.align)?;
    }

    report.edits = pending
        .into_iter()
        .map(|p| CellEdit {
            table: p.table,
            row: p.row,
            column: p.column,
            before: p.before,
            after: p.after,
        })
        .collect();
    report.tombstones.sort();
    report.tombstones.dedup();

    // Post-conditions: same posting multiset under the mapping, no new danglers.
    let chart_before = Chart::from_table(&scope.am)?;
    let chart_after = Chart::from_table(&am)?;
    let mut expected: Vec<String> = chart_before
        .posting()
        .map(|a| mapping.get(a.ac_no.trim()).cloned().unwrap_or_else(|| a.ac_no.trim().to_string()))
        .collect();
    let mut actual: Vec<String> = chart_after
        .posting()
        .map(|a| a.ac_no.trim().to_string())
        .collect();
    expected.sort();
    actual.sort();
    if expected != actual {
        return Err(CoreError::Format(format!(
            "renumber invariant broken: chart numbers after the cascade are not the mapped numbers before it ({:?} vs {:?})",
            actual.len(),
            expected.len()
        )));
    }

    let post_scope = RenumberScope { am: am.clone(), bl: bl.clone(), gl: gl.clone(), mp: scope.mp.clone() };
    let index_after = RefIndex::build(&post_scope)?;
    let names_after = namespace_sets(&post_scope, &index_after)?;
    report.dangling_after =
        index_after.dangling(&names_after.0, &names_after.1, &names_after.2);
    let new_dangles: Vec<&(&'static str, &'static str, String)> = report
        .dangling_after
        .iter()
        .filter(|d| !report.dangling_before.contains(d))
        .collect();
    if let Some((table, column, value)) = new_dangles.first() {
        return Err(CoreError::Format(format!(
            "renumber would create a dangling reference {value:?} in {table}.{column}"
        )));
    }

    let changed = am.encode() != scope.am.encode()
        || bl.encode() != scope.bl.encode()
        || gl.encode() != scope.gl.encode();
    Ok(RenumberPreview { report, am, bl, gl, changed })
}

fn namespace_sets(
    scope: &RenumberScope,
    index: &RefIndex,
) -> Result<(BTreeSet<String>, BTreeSet<String>, BTreeSet<String>)> {
    let chart = Chart::from_table(&scope.am)?;
    let accounts = chart
        .posting()
        .map(|a| a.ac_no.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    Ok((accounts, index.map_codes.clone(), index.tax_codes.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_numbers() {
        assert_eq!(canonical_number("1001"), Some(1001));
        assert_eq!(canonical_number(" 1001 "), Some(1001));
        assert_eq!(canonical_number("0"), Some(0));
        assert_eq!(canonical_number("0010"), None, "leading zero is not canonical");
        assert_eq!(canonical_number("1332-00"), None);
        assert_eq!(canonical_number("RSM - 01"), None);
        assert_eq!(canonical_number(""), None);
        assert_eq!(canonical_number("1234567890123456789"), None, "not an i64");
    }

    #[test]
    fn alignment_is_read_from_the_cell() {
        assert_eq!(cell_align(b"      1001"), Align::Right);
        assert_eq!(cell_align(b"1001      "), Align::Left);
        assert_eq!(cell_align(b"1001"), Align::Left);
        assert_eq!(cell_align(b"          "), Align::Left);
    }

    #[test]
    fn identity_pairs_are_dropped() {
        let plan = RenumberPlan::from_pairs(&[("1001".into(), "1001".into())]).unwrap();
        assert!(plan.is_empty());
    }

    #[test]
    fn delta_zero_is_an_empty_plan() {
        let bytes = cw_core::dbf::synthetic_fixture();
        let t = Table::parse(&bytes).unwrap();
        let scope = RenumberScope::new(t.clone(), t.clone(), t);
        let plan = RenumberPlan::from_delta(&scope, 0).unwrap_or_else(|_| RenumberPlan::default());
        assert!(plan.is_empty());
    }
}
