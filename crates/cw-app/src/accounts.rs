//! cw-app chart of accounts - the `AM` table model (P5.E1).
//!
//! Contract: `docs\specs\accounts.md`; schema frozen from
//! `inventory\dbf-schema.tsv` (245 / 81 fields / 1603-byte records) and
//! re-verified against `tests\golden\dbf\{hull2024,corp2026}\*AM.dbf`.
//!
//! The model has three layers:
//!
//! 1. [`AM_SCHEMA`] - the 81-column physical layout, grouped into the
//!    families CW's account-properties dialog is built from. The kernel
//!    [`Table`] stays the byte store; this module never rewrites a field
//!    descriptor.
//! 2. [`AmAccount`] - one live chart row decoded into typed fields
//!    (`TYPE` B/I/M, `SIGN`/`DC` D/C, balances as `f64`, flags as [`Flag`]).
//! 3. [`Chart`] / [`AccountProperties`] - the engagement chart index plus the
//!    writable account-properties surface (`POST`/`PRINT`/`CTL`/`TOTAL`/
//!    `SIGN`/`DC`/`COLUMN`/`DOLLAR`/`ULINE`/`PAGE`/`DEN`/`HIDDEN`).
//!
//! Write law: every property write goes through [`write_cell`], which pads to
//! the physical field width and validates the dBase type. Writing the decoded
//! properties back unchanged is byte-identical on the corpus (gate
//! `tests\accounts_gate.rs`).
//!
//! Out of scope (documented in the spec): FPT memo payloads (`MEMO`,
//! `UID_NOTE`, `EXINFO` are exposed as raw pointer cells) and lead-sheet/grouping
//! assignment semantics (P5.A).
//!
//! The engagement-wide renumber cascade that `accounts.md` §6 parked here now
//! lives in [`crate::renumber`] (FEAT.018): it owns the `AC_NO` primary write plus
//! every `AC_NO2`/`AC_NO2CR`/`PAC_NO`, `BL.NUMBER` and `GL.AC_NO`/`B_ACT`
//! reference, so a chart write is never left dangling. This module still never
//! renumbers on its own.

use std::collections::BTreeMap;
use std::fmt;

use cw_core::dbf::Table;
use cw_core::{CoreError, Result};

/// Physical `AM` field count (all three corpus schemas).
pub const AM_FIELD_COUNT: usize = 81;
/// Physical `AM` record length, deletion flag included.
pub const AM_RECORD_LEN: usize = 1603;

/// One field descriptor of the canonical `AM` layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AmField {
    pub name: &'static str,
    /// dBase type letter (`C`/`N`/`M` in the whole corpus).
    pub kind: u8,
    pub len: u8,
    pub dec: u8,
}

const fn f(name: &'static str, kind: u8, len: u8, dec: u8) -> AmField {
    AmField { name, kind, len, dec }
}

/// Canonical `AM` schema in physical order (81 columns).
pub static AM_SCHEMA: [AmField; AM_FIELD_COUNT] = [
    f("LINENO", b'N', 8, 0),
    f("TYPE", b'C', 1, 0),
    f("CLASS", b'C', 2, 0),
    f("AC_NO", b'C', 40, 0),
    f("ENTITY", b'C', 40, 0),
    f("DIV", b'C', 6, 0),
    f("SUB", b'C', 6, 0),
    f("SUB_NO", b'C', 40, 0),
    f("SPLITUP", b'C', 1, 0),
    f("DIST", b'C', 1, 0),
    f("AC_NO2", b'C', 40, 0),
    f("AC_NO2CR", b'C', 40, 0),
    f("AC_DESC", b'C', 50, 0),
    f("MEMO", b'M', 10, 0),
    f("SCHEDULE", b'C', 40, 0),
    f("SCHEDULECR", b'C', 40, 0),
    f("GROUP2", b'C', 40, 0),
    f("GROUP2CR", b'C', 40, 0),
    f("GROUP3", b'C', 40, 0),
    f("GROUP3CR", b'C', 40, 0),
    f("GROUP4", b'C', 40, 0),
    f("GROUP4CR", b'C', 40, 0),
    f("GROUP5", b'C', 40, 0),
    f("GROUP5CR", b'C', 40, 0),
    f("GROUP6", b'C', 40, 0),
    f("GROUP6CR", b'C', 40, 0),
    f("GROUP7", b'C', 40, 0),
    f("GROUP7CR", b'C', 40, 0),
    f("GROUP8", b'C', 40, 0),
    f("GROUP8CR", b'C', 40, 0),
    f("GROUP9", b'C', 40, 0),
    f("GROUP9CR", b'C', 40, 0),
    f("GROUP0", b'C', 40, 0),
    f("GROUP0CR", b'C', 40, 0),
    f("TAC_NO", b'C', 16, 0),
    f("GIFI", b'C', 10, 0),
    f("CC", b'C', 3, 0),
    f("DC", b'C', 3, 0),
    f("UNIT", b'C', 4, 0),
    f("UNIT_B", b'C', 4, 0),
    f("COLUMN", b'C', 1, 0),
    f("DOLLAR", b'C', 1, 0),
    f("ULINE", b'C', 1, 0),
    f("PAGE", b'C', 1, 0),
    f("SIGN", b'C', 1, 0),
    f("POST", b'C', 1, 0),
    f("PRINT", b'C', 1, 0),
    f("CTL", b'C', 2, 0),
    f("TOTAL", b'C', 1, 0),
    f("DEN", b'C', 40, 0),
    f("OPEN", b'N', 19, 2),
    f("CUR_MTH", b'N', 19, 2),
    f("CUR_YTD", b'N', 19, 2),
    f("AJE_BAL", b'N', 19, 2),
    f("EJE_BAL", b'N', 19, 2),
    f("RJE_BAL", b'N', 19, 2),
    f("C1", b'N', 19, 2),
    f("C2", b'N', 19, 2),
    f("MEMO_NO", b'C', 2, 0),
    f("FROM_REF", b'C', 40, 0),
    f("REV_CODE", b'C', 2, 0),
    f("FXCYTD", b'N', 19, 2),
    f("FXPYTD", b'N', 19, 2),
    f("C_RATE", b'N', 13, 5),
    f("STATUS", b'C', 15, 0),
    f("BASELN", b'N', 19, 2),
    f("TAX_ID", b'C', 4, 0),
    f("UID_NOTE", b'M', 10, 0),
    f("UNUSED42", b'C', 1, 0),
    f("CWGUID", b'C', 22, 0),
    f("LOCKED", b'C', 1, 0),
    f("EXINFO", b'M', 10, 0),
    f("PAC_NO", b'C', 16, 0),
    f("FXTYPE", b'C', 1, 0),
    f("CWDATE", b'C', 14, 0),
    f("CREATEDON", b'C', 14, 0),
    f("BALSTAT", b'C', 28, 0),
    f("CWCHECKSUM", b'C', 8, 0),
    f("CONSCHKSUM", b'C', 8, 0),
    f("EXTCHKSUM", b'C', 8, 0),
    f("HIDDEN", b'C', 1, 0),
];

/// Semantic column family (the account-properties dialog sections).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Family {
    /// Table row identity + account number (`LINENO`, `TYPE`, `CLASS`, `AC_NO`, `CWGUID`).
    Identity,
    /// Entity/fund/segment axes (`ENTITY`, `DIV`, `SUB`, `CC`, `UNIT`, ...).
    Segment,
    /// Human text (`AC_DESC`, `MEMO`).
    Description,
    /// Lead-sheet/grouping web (`SCHEDULE*`, `GROUP0..9`).
    Grouping,
    /// Tax mapping (`TAC_NO`, `GIFI`, `TAX_ID`).
    Tax,
    /// Print/statement properties (the 12 writable flag columns).
    Presentation,
    /// Balance mirrors (`OPEN`, `CUR_*`, `*JE_BAL`, `C1/C2`, `BASELN`).
    Balance,
    /// Annotation anchors (`MEMO_NO`, `FROM_REF`, `REV_CODE`).
    Annotation,
    /// FX translation (`FXCYTD`, `FXPYTD`, `C_RATE`, `FXTYPE`).
    Fx,
    /// Cross-reference checksums (`BALSTAT`, `CWCHECKSUM`, `CONSCHKSUM`, `EXTCHKSUM`).
    Integrity,
    /// Row metadata (`STATUS`, `LOCKED`, `CWDATE`, ...).
    Meta,
    /// Unused reserved column(s) (`UNUSED42`).
    Opaque,
}

impl Family {
    pub const ALL: [Family; 12] = [
        Family::Identity,
        Family::Segment,
        Family::Description,
        Family::Grouping,
        Family::Tax,
        Family::Presentation,
        Family::Balance,
        Family::Annotation,
        Family::Fx,
        Family::Integrity,
        Family::Meta,
        Family::Opaque,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Family::Identity => "identity",
            Family::Segment => "segment",
            Family::Description => "description",
            Family::Grouping => "grouping",
            Family::Tax => "tax",
            Family::Presentation => "presentation",
            Family::Balance => "balance",
            Family::Annotation => "annotation",
            Family::Fx => "fx",
            Family::Integrity => "integrity",
            Family::Meta => "meta",
            Family::Opaque => "opaque",
        }
    }
}

/// Family for a physical column name (case-insensitive; `""` when unknown).
pub fn family_of(name: &str) -> Option<Family> {
    let n = name.trim().to_ascii_uppercase();
    Some(match n.as_str() {
        "LINENO" | "TYPE" | "CLASS" | "AC_NO" | "CWGUID" => Family::Identity,
        "ENTITY" | "DIV" | "SUB" | "SUB_NO" | "SPLITUP" | "DIST" | "AC_NO2" | "AC_NO2CR"
        | "CC" | "DC" | "UNIT" | "UNIT_B" => Family::Segment,
        "AC_DESC" | "MEMO" => Family::Description,
        "SCHEDULE" | "SCHEDULECR" => Family::Grouping,
        "TAC_NO" | "GIFI" | "TAX_ID" => Family::Tax,
        "COLUMN" | "DOLLAR" | "ULINE" | "PAGE" | "SIGN" | "POST" | "PRINT" | "CTL" | "TOTAL"
        | "DEN" => Family::Presentation,
        "OPEN" | "CUR_MTH" | "CUR_YTD" | "AJE_BAL" | "EJE_BAL" | "RJE_BAL" | "C1" | "C2"
        | "BASELN" => Family::Balance,
        "MEMO_NO" | "FROM_REF" | "REV_CODE" => Family::Annotation,
        "FXCYTD" | "FXPYTD" | "C_RATE" | "FXTYPE" => Family::Fx,
        "BALSTAT" | "CWCHECKSUM" | "CONSCHKSUM" | "EXTCHKSUM" => Family::Integrity,
        "STATUS" | "LOCKED" | "EXINFO" | "PAC_NO" | "CWDATE" | "CREATEDON" | "HIDDEN"
        | "UID_NOTE" => Family::Meta,
        "UNUSED42" => Family::Opaque,
        "GROUP0" | "GROUP0CR" | "GROUP1" | "GROUP1CR" | "GROUP2" | "GROUP2CR" | "GROUP3"
        | "GROUP3CR" | "GROUP4" | "GROUP4CR" | "GROUP5" | "GROUP5CR" | "GROUP6" | "GROUP6CR"
        | "GROUP7" | "GROUP7CR" | "GROUP8" | "GROUP8CR" | "GROUP9" | "GROUP9CR" => {
            Family::Grouping
        }
        _ => return None,
    })
}

/// Columns of one family, in physical order.
pub fn family_columns(fam: Family) -> impl Iterator<Item = &'static AmField> {
    AM_SCHEMA.iter().filter(move |f| family_of(f.name) == Some(fam))
}

/// Look up a canonical field by name.
pub fn schema_field(name: &str) -> Option<&'static AmField> {
    let n = name.trim().to_ascii_uppercase();
    AM_SCHEMA.iter().find(|f| f.name == n)
}

/// Why a physical `AM` table diverges from [`AM_SCHEMA`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaDrift {
    /// Column name (expected, or actual for `Extra`).
    pub column: String,
    pub kind: DriftKind,
    pub expected: String,
    pub actual: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriftKind {
    /// Expected column absent.
    Missing,
    /// Column present that the canonical schema does not define.
    Extra,
    /// Same name, wrong physical position.
    Order,
    /// Same name, wrong dBase type.
    Type,
    /// Same name, wrong width.
    Length,
    /// Same name, wrong decimal count.
    Decimals,
}

impl fmt::Display for DriftKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            DriftKind::Missing => "missing",
            DriftKind::Extra => "extra",
            DriftKind::Order => "order",
            DriftKind::Type => "type",
            DriftKind::Length => "length",
            DriftKind::Decimals => "decimals",
        };
        f.write_str(s)
    }
}

/// Full physical-schema comparison (order, names, types, widths, decimals).
pub fn schema_drift(t: &Table) -> Vec<SchemaDrift> {
    let mut out = Vec::new();
    let mut actual_names: Vec<String> = Vec::with_capacity(t.fields.len());
    for fd in &t.fields {
        actual_names.push(fd.name.to_ascii_uppercase());
    }
    // expected column missing entirely
    for e in AM_SCHEMA.iter() {
        if !actual_names.iter().any(|n| n == e.name) {
            out.push(SchemaDrift {
                column: e.name.to_string(),
                kind: DriftKind::Missing,
                expected: format!("{}{}.{}", e.kind as char, e.len, e.dec),
                actual: String::new(),
            });
        }
    }
    // actual column not in the schema
    for a in actual_names.iter() {
        if !AM_SCHEMA.iter().any(|e| e.name == a) {
            out.push(SchemaDrift {
                column: a.clone(),
                kind: DriftKind::Extra,
                expected: String::new(),
                actual: a.clone(),
            });
        }
    }
    // per-position comparison for the common prefix
    for (i, fd) in t.fields.iter().enumerate() {
        let name = fd.name.to_ascii_uppercase();
        let Some(e) = AM_SCHEMA.iter().find(|e| e.name == name) else {
            continue;
        };
        let expected_pos = AM_SCHEMA.iter().position(|x| x.name == name).unwrap();
        if expected_pos != i {
            out.push(SchemaDrift {
                column: name.clone(),
                kind: DriftKind::Order,
                expected: expected_pos.to_string(),
                actual: i.to_string(),
            });
            continue;
        }
        if fd.kind != e.kind {
            out.push(SchemaDrift {
                column: name.clone(),
                kind: DriftKind::Type,
                expected: (e.kind as char).to_string(),
                actual: (fd.kind as char).to_string(),
            });
        }
        if fd.len != e.len {
            out.push(SchemaDrift {
                column: name.clone(),
                kind: DriftKind::Length,
                expected: e.len.to_string(),
                actual: fd.len.to_string(),
            });
        }
        if fd.dec != e.dec {
            out.push(SchemaDrift {
                column: name.clone(),
                kind: DriftKind::Decimals,
                expected: e.dec.to_string(),
                actual: fd.dec.to_string(),
            });
        }
    }
    out
}

/// True when the table is the canonical 81-column `AM` layout.
pub fn schema_matches(t: &Table) -> bool {
    schema_drift(t).is_empty()
}

/// CaseWare-style Y/N flag with optional blank ("unset") state.
///
/// The raw text is retained so a parse/write cycle does not normalize casing
/// or width; writes pad/truncate only at the physical boundary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Flag(String);

impl Flag {
    /// Parse `"Y"`, `"N"` or blank. Anything else is a schema-level error.
    pub fn parse(raw: &str) -> Result<Flag> {
        let s = raw.trim();
        match s.to_ascii_uppercase().as_str() {
            "" | "Y" | "N" => Ok(Flag(s.to_string())),
            other => Err(CoreError::Format(format!(
                "invalid Y/N flag {other:?} (expected Y, N or blank)"
            ))),
        }
    }

    pub fn yes() -> Flag {
        Flag("Y".to_string())
    }

    pub fn no() -> Flag {
        Flag("N".to_string())
    }

    pub fn unset() -> Flag {
        Flag(String::new())
    }

    pub fn is_yes(&self) -> bool {
        self.0.eq_ignore_ascii_case("Y")
    }

    pub fn is_no(&self) -> bool {
        self.0.eq_ignore_ascii_case("N")
    }

    /// True when CW stored an explicit value (Y or N).
    pub fn is_set(&self) -> bool {
        !self.0.is_empty()
    }

    pub fn raw(&self) -> &str {
        &self.0
    }
}

/// Debit/credit side (`SIGN`, `DC`); blank is legal in the corpus.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Side {
    Debit,
    Credit,
    #[default]
    Unset,
}

impl Side {
    pub fn parse(raw: &str) -> Result<Side> {
        let s = raw.trim();
        match s.to_ascii_uppercase().as_str() {
            "" => Ok(Side::Unset),
            "D" => Ok(Side::Debit),
            "C" => Ok(Side::Credit),
            other => Err(CoreError::Format(format!(
                "invalid D/C side {other:?} (expected D, C or blank)"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Side::Debit => "D",
            Side::Credit => "C",
            Side::Unset => "",
        }
    }

    pub fn is_set(self) -> bool {
        self != Side::Unset
    }
}

/// CW `AM.TYPE` taxonomy observed in the corpus: balance sheet, income
/// statement, memo/non-posting block (`TYPE='M'`, `AC_NO='z'`), plus a
/// pass-through for unknown letters so a model round-trip never panics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountKind {
    BalanceSheet,
    Income,
    Memo,
    Other(u8),
}

impl AccountKind {
    pub fn parse(raw: &str) -> Result<AccountKind> {
        let s = raw.trim();
        let b = s.as_bytes();
        Ok(match b {
            [] => AccountKind::Other(0),
            [c] => match c.to_ascii_uppercase() {
                b'B' => AccountKind::BalanceSheet,
                b'I' => AccountKind::Income,
                b'M' => AccountKind::Memo,
                other => AccountKind::Other(other),
            },
            _ => {
                return Err(CoreError::Format(format!(
                    "invalid account TYPE {s:?} (expected one letter)"
                )))
            }
        })
    }

    pub fn code(self) -> Option<u8> {
        match self {
            AccountKind::BalanceSheet => Some(b'B'),
            AccountKind::Income => Some(b'I'),
            AccountKind::Memo => Some(b'M'),
            AccountKind::Other(c) if c != 0 => Some(c),
            AccountKind::Other(_) => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AccountKind::BalanceSheet => "balance-sheet",
            AccountKind::Income => "income",
            AccountKind::Memo => "memo",
            AccountKind::Other(_) => "unknown",
        }
    }

    /// Rows that participate in the trial balance / lead sheets.
    pub fn is_posting(self) -> bool {
        matches!(self, AccountKind::BalanceSheet | AccountKind::Income)
    }
}

/// `AC_CLASS` / lead-sheet class is free text (2 chars); no enum is imposed.
pub type AccountClass = String;

/// `CWGetAccountsType` — the six account-set selectors of CW's `GetAccounts`.
///
/// Values are the shipped type-library enum `[49] TKIND_ENUM CWGetAccountsType`
/// (`x64cw42` TYPELIB #1), in declaration
/// order. The enum is the *selector*; the row predicate each class applies is
/// [`AccountsView::predicate`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GetAccountsType {
    /// `gatAccount = 0` — every posting account.
    Account = 0,
    /// `gatReport = 1` — accounts that carry a statement mapping.
    Report = 1,
    /// `gatCashFlow = 2` — accounts in the cash-flow set.
    CashFlow = 2,
    /// `gatPerformance = 3` — performance-indicator accounts.
    Performance = 3,
    /// `gatDistributable = 4` — distributable-profit accounts.
    Distributable = 4,
    /// `gatFlipped = 5` — sign-flipped (contra) accounts.
    Flipped = 5,
}

impl GetAccountsType {
    /// Every selector in enum-declaration order.
    pub const ALL: [GetAccountsType; 6] = [
        GetAccountsType::Account,
        GetAccountsType::Report,
        GetAccountsType::CashFlow,
        GetAccountsType::Performance,
        GetAccountsType::Distributable,
        GetAccountsType::Flipped,
    ];

    /// The raw enum value CW stores/transmits.
    pub fn value(self) -> u8 {
        self as u8
    }

    /// Decode the raw enum value (`None` outside 0..=5).
    pub fn from_value(v: u8) -> Option<GetAccountsType> {
        GetAccountsType::ALL.into_iter().find(|t| t.value() == v)
    }

    /// COM enum member name, verbatim from the typelib.
    pub fn name(self) -> &'static str {
        match self {
            GetAccountsType::Account => "gatAccount",
            GetAccountsType::Report => "gatReport",
            GetAccountsType::CashFlow => "gatCashFlow",
            GetAccountsType::Performance => "gatPerformance",
            GetAccountsType::Distributable => "gatDistributable",
            GetAccountsType::Flipped => "gatFlipped",
        }
    }
}

/// The row predicate one `CWGetAccountsType` class applies to an `AM` row.
///
/// `gatAccount` is the only class the corpus can pin: it is the plain posting
/// set (`TYPE` B/I) and it excludes `POST='N'` rows. Every corpus engagement
/// carries exactly one such row — the computed `NETINC` ("Net Income (Loss)")
/// line — which is why the shipped census reads 324 posting accounts out of 325
/// live `AM` rows in `corp2026` (and 311/349, 351/402 in `mwgh2025`/
/// `hull2024`). Implemented and gated as `tests/accounts_gate.rs::
/// get_accounts_excludes_unposted_netinc`.
///
/// The remaining five classes are statement-side selectors whose physical
/// predicates (`DIST`, sign `flip`, cash-flow/performance mapping) are **not**
/// established: the corpus has no populated `DIST`, no `FLIP` column and no
/// `MP` class markers, so their predicates are recorded as unimplemented rather
/// than guessed. [`AccountsView::predicate`] reports [`AccountsPredicate::Unproven`]
/// for them and the filters below refuse to answer.
pub enum AccountsPredicate {
    /// `gatAccount`: posting rows whose `POST` is not `N`.
    Posting,
    /// Selector whose physical predicate is not established.
    Unproven,
}

impl std::fmt::Debug for AccountsPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            AccountsPredicate::Posting => "Posting",
            AccountsPredicate::Unproven => "Unproven",
        })
    }
}

/// One filtered account set.
#[derive(Debug)]
pub struct AccountsView<'a> {
    pub kind: GetAccountsType,
    rows: Vec<&'a AmAccount>,
}

impl<'a> AccountsView<'a> {
    pub fn rows(&self) -> &[&'a AmAccount] {
        &self.rows
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

impl GetAccountsType {
    /// Which predicate this selector applies.
    pub fn predicate(self) -> AccountsPredicate {
        match self {
            GetAccountsType::Account => AccountsPredicate::Posting,
            _ => AccountsPredicate::Unproven,
        }
    }
}

fn text(t: &Table, r: usize, col: &str) -> Result<String> {
    t.cell_str(r, col)
        .map(|s| s.trim_end().to_string())
        .ok_or_else(|| CoreError::Format(format!("row {r}: no column {col}")))
}

fn flag(t: &Table, r: usize, col: &str) -> Result<Flag> {
    Flag::parse(&text(t, r, col)?)
}

fn num(t: &Table, r: usize, col: &str) -> Result<f64> {
    let raw = text(t, r, col)?;
    let s = raw.trim();
    if s.is_empty() {
        return Ok(0.0);
    }
    s.parse::<f64>()
        .map_err(|_| CoreError::Format(format!("row {r}: {col}={s:?} is not numeric")))
}

fn int(t: &Table, r: usize, col: &str) -> Result<i64> {
    let raw = text(t, r, col)?;
    let s = raw.trim();
    if s.is_empty() {
        return Ok(0);
    }
    s.parse::<i64>()
        .map_err(|_| CoreError::Format(format!("row {r}: {col}={s:?} is not an integer")))
}

/// The writable account-properties surface (the "big" account dialog flags).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountProperties {
    pub column: Flag,
    pub dollar: Flag,
    pub uline: Flag,
    pub page: Flag,
    pub sign: Side,
    pub post: Flag,
    pub print: Flag,
    pub ctl: String,
    pub total: Flag,
    pub den: String,
    pub hidden: Flag,
    /// Debit/credit designation (`DC`); separate from `SIGN`.
    pub dc: Side,
}

/// Physical columns backing [`AccountProperties`], in dialog order.
pub const PROPERTY_COLUMNS: [&str; 12] = [
    "POST", "PRINT", "CTL", "TOTAL", "SIGN", "DC", "COLUMN", "DOLLAR", "ULINE", "PAGE", "DEN",
    "HIDDEN",
];

impl AccountProperties {
    /// Decode the property columns of one physical row.
    pub fn from_table(t: &Table, r: usize) -> Result<AccountProperties> {
        Ok(AccountProperties {
            column: flag(t, r, "COLUMN")?,
            dollar: flag(t, r, "DOLLAR")?,
            uline: flag(t, r, "ULINE")?,
            page: flag(t, r, "PAGE")?,
            sign: Side::parse(&text(t, r, "SIGN")?)?,
            post: flag(t, r, "POST")?,
            print: flag(t, r, "PRINT")?,
            ctl: text(t, r, "CTL")?,
            total: flag(t, r, "TOTAL")?,
            den: text(t, r, "DEN")?,
            hidden: flag(t, r, "HIDDEN")?,
            dc: Side::parse(&text(t, r, "DC")?)?,
        })
    }

    /// Write every property column back through [`write_cell`]. A value
    /// decoded from a corpus row re-encodes byte-identically.
    pub fn write_to(&self, t: &mut Table, r: usize) -> Result<()> {
        write_cell(t, r, "COLUMN", self.column.raw())?;
        write_cell(t, r, "DOLLAR", self.dollar.raw())?;
        write_cell(t, r, "ULINE", self.uline.raw())?;
        write_cell(t, r, "PAGE", self.page.raw())?;
        write_cell(t, r, "SIGN", self.sign.as_str())?;
        write_cell(t, r, "POST", self.post.raw())?;
        write_cell(t, r, "PRINT", self.print.raw())?;
        write_cell(t, r, "CTL", &self.ctl)?;
        write_cell(t, r, "TOTAL", self.total.raw())?;
        write_cell(t, r, "DEN", &self.den)?;
        write_cell(t, r, "HIDDEN", self.hidden.raw())?;
        write_cell(t, r, "DC", self.dc.as_str())?;
        Ok(())
    }
}

/// Partial update used by the properties dialog. `None` = leave untouched.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PropertiesPatch {
    pub column: Option<Flag>,
    pub dollar: Option<Flag>,
    pub uline: Option<Flag>,
    pub page: Option<Flag>,
    pub sign: Option<Side>,
    pub post: Option<Flag>,
    pub print: Option<Flag>,
    pub ctl: Option<String>,
    pub total: Option<Flag>,
    pub den: Option<String>,
    pub hidden: Option<Flag>,
    pub dc: Option<Side>,
}

impl PropertiesPatch {
    pub fn is_empty(&self) -> bool {
        self == &PropertiesPatch::default()
    }

    /// Overlay the patch on an existing property set.
    pub fn apply(&self, p: &mut AccountProperties) {
        if let Some(v) = &self.column {
            p.column = v.clone();
        }
        if let Some(v) = &self.dollar {
            p.dollar = v.clone();
        }
        if let Some(v) = &self.uline {
            p.uline = v.clone();
        }
        if let Some(v) = &self.page {
            p.page = v.clone();
        }
        if let Some(v) = self.sign {
            p.sign = v;
        }
        if let Some(v) = &self.post {
            p.post = v.clone();
        }
        if let Some(v) = &self.print {
            p.print = v.clone();
        }
        if let Some(v) = &self.ctl {
            p.ctl = v.clone();
        }
        if let Some(v) = &self.total {
            p.total = v.clone();
        }
        if let Some(v) = &self.den {
            p.den = v.clone();
        }
        if let Some(v) = &self.hidden {
            p.hidden = v.clone();
        }
        if let Some(v) = self.dc {
            p.dc = v;
        }
    }

    /// Apply atomically: validation/write errors leave the table untouched.
    pub fn write_to(&self, t: &mut Table, r: usize) -> Result<()> {
        let mut props = AccountProperties::from_table(t, r)?;
        self.apply(&mut props);
        // validate widths/types before touching bytes
        let mut scratch = t.clone();
        props.write_to(&mut scratch, r)?;
        props.write_to(t, r)
    }
}

/// Write one text value into a physical cell, enforcing field width and dBase
/// type. `C` cells are left-justified, `N` cells right-justified, all other
/// types are validated but written verbatim.
///
/// Memo (`M`) cells are refused: their payload lives in the FPT sidecar and
/// cannot be rewritten by a plain table cell write.
pub fn write_cell(t: &mut Table, r: usize, column: &str, value: &str) -> Result<()> {
    let idx = t
        .field_index(column)
        .ok_or_else(|| CoreError::Format(format!("table has no column {column}")))?;
    let kind = t.fields[idx].kind;
    let width = t.fields[idx].len as usize;
    let rec_len = t.record_len as usize;
    if r >= t.records.len() {
        return Err(CoreError::Format(format!("row {r} out of range")));
    }
    if t.records[r].len() != rec_len {
        return Err(CoreError::Format(format!(
            "row {r}: record length {} != {rec_len}",
            t.records[r].len()
        )));
    }

    let raw: Vec<u8> = value
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
            "row {r}: {column} value {:?} is {} bytes, field width {width}",
            value,
            raw.len()
        )));
    }

    // type validation
    match kind {
        b'C' => {}
        b'N' => {
            let s = value.trim();
            if !s.is_empty() {
                s.parse::<f64>().map_err(|_| {
                    CoreError::Format(format!("row {r}: {column}={s:?} is not numeric"))
                })?;
            }
        }
        b'D' => {
            let s = value.trim();
            if !s.is_empty()
                && (s.len() != 8 || !s.bytes().all(|b| b.is_ascii_digit()))
            {
                return Err(CoreError::Format(format!(
                    "row {r}: {column}={s:?} is not a YYYYMMDD date"
                )));
            }
        }
        b'L' => {
            let s = value.trim().to_ascii_uppercase();
            if !matches!(s.as_str(), "" | "T" | "F" | "Y" | "N") {
                return Err(CoreError::Format(format!(
                    "row {r}: {column}={s:?} is not a logical"
                )));
            }
        }
        b'M' => {
            return Err(CoreError::Format(format!(
                "row {r}: {column} is a memo (FPT) cell; use the FPT writer"
            )))
        }
        other => {
            return Err(CoreError::Format(format!(
                "row {r}: unsupported dBase type {}",
                other as char
            )))
        }
    }

    let off = t.offsets()[idx];
    let rec = &mut t.records[r];
    rec[off..off + width].fill(b' ');
    if kind == b'N' {
        // right-justify numeric cells, CW style
        let start = off + width - raw.len();
        rec[start..start + raw.len()].copy_from_slice(&raw);
    } else {
        rec[off..off + raw.len()].copy_from_slice(&raw);
    }
    Ok(())
}

/// Physical lead-sheet grouping columns in table order. `GROUP1` does not
/// exist on disk: CW stores nine slots (`GROUP2..GROUP9` then `GROUP0`).
pub const GROUP_COLUMNS: [&str; 9] = [
    "GROUP2", "GROUP3", "GROUP4", "GROUP5", "GROUP6", "GROUP7", "GROUP8", "GROUP9", "GROUP0",
];

/// Credit-side companions of [`GROUP_COLUMNS`].
pub const GROUP_COLUMNS_CR: [&str; 9] = [
    "GROUP2CR", "GROUP3CR", "GROUP4CR", "GROUP5CR", "GROUP6CR", "GROUP7CR", "GROUP8CR",
    "GROUP9CR", "GROUP0CR",
];

/// Slot index for a grouping digit (`0` and `2..=9`; `1` is not a column).
pub fn group_slot(digit: u8) -> Option<usize> {
    match digit {
        0 => Some(8),
        2..=9 => Some((digit - 2) as usize),
        _ => None,
    }
}

/// One decoded `AM` row.
///
/// Group slots follow [`GROUP_COLUMNS`] (`groups[0]` = `GROUP2`, `groups[8]` =
/// `GROUP0`); use [`group_slot`] to address a slot by digit.
#[derive(Clone, Debug, PartialEq)]
pub struct AmAccount {
    pub row: usize,
    pub line_no: i64,
    pub kind: AccountKind,
    pub class: AccountClass,
    pub ac_no: String,
    pub entity: String,
    pub div: String,
    pub sub: String,
    pub sub_no: String,
    pub split_up: Flag,
    pub dist: String,
    pub ac_no2: String,
    pub ac_no2_cr: String,
    pub desc: String,
    pub schedule: String,
    pub schedule_cr: String,
    /// Nine lead-sheet groups in [`GROUP_COLUMNS`] order.
    pub groups: [String; 9],
    /// Credit-side groups in [`GROUP_COLUMNS_CR`] order.
    pub groups_cr: [String; 9],
    pub tac_no: String,
    pub gifi: String,
    pub cc: String,
    pub unit: String,
    pub unit_b: String,
    pub props: AccountProperties,
    pub open: f64,
    pub cur_mth: f64,
    pub cur_ytd: f64,
    pub aje_bal: f64,
    pub eje_bal: f64,
    pub rje_bal: f64,
    pub c1: f64,
    pub c2: f64,
    pub base_ln: f64,
    pub memo_no: String,
    pub from_ref: String,
    pub rev_code: String,
    pub fx_cytd: f64,
    pub fx_pytd: f64,
    pub c_rate: f64,
    pub fxtype: String,
    pub status: String,
    pub tax_id: String,
    pub locked: Flag,
    pub hidden: Flag,
    pub pac_no: String,
    pub cwguid: String,
    pub cwdate: String,
    pub createdon: String,
    pub balstat: String,
    pub cwchecksum: String,
    pub conschksum: String,
    pub extchksum: String,
    /// Raw memo pointer cells (FPT resolution is the caller's job).
    pub memo_ref: String,
    pub uid_note_ref: String,
    pub exinfo_ref: String,
    pub unused42: String,
}

impl AmAccount {
    /// Decode one physical row (memo pointers stay raw).
    pub fn from_table(t: &Table, r: usize) -> Result<AmAccount> {
        let mut groups: [String; 9] = Default::default();
        let mut groups_cr: [String; 9] = Default::default();
        for (slot, col) in GROUP_COLUMNS.iter().enumerate() {
            groups[slot] = text(t, r, col)?;
            groups_cr[slot] = text(t, r, GROUP_COLUMNS_CR[slot])?;
        }
        Ok(AmAccount {
            row: r,
            line_no: int(t, r, "LINENO")?,
            kind: AccountKind::parse(&text(t, r, "TYPE")?)?,
            class: text(t, r, "CLASS")?,
            ac_no: text(t, r, "AC_NO")?,
            entity: text(t, r, "ENTITY")?,
            div: text(t, r, "DIV")?,
            sub: text(t, r, "SUB")?,
            sub_no: text(t, r, "SUB_NO")?,
            split_up: flag(t, r, "SPLITUP")?,
            dist: text(t, r, "DIST")?,
            ac_no2: text(t, r, "AC_NO2")?,
            ac_no2_cr: text(t, r, "AC_NO2CR")?,
            desc: text(t, r, "AC_DESC")?,
            schedule: text(t, r, "SCHEDULE")?,
            schedule_cr: text(t, r, "SCHEDULECR")?,
            groups,
            groups_cr,
            tac_no: text(t, r, "TAC_NO")?,
            gifi: text(t, r, "GIFI")?,
            cc: text(t, r, "CC")?,
            unit: text(t, r, "UNIT")?,
            unit_b: text(t, r, "UNIT_B")?,
            props: AccountProperties::from_table(t, r)?,
            open: num(t, r, "OPEN")?,
            cur_mth: num(t, r, "CUR_MTH")?,
            cur_ytd: num(t, r, "CUR_YTD")?,
            aje_bal: num(t, r, "AJE_BAL")?,
            eje_bal: num(t, r, "EJE_BAL")?,
            rje_bal: num(t, r, "RJE_BAL")?,
            c1: num(t, r, "C1")?,
            c2: num(t, r, "C2")?,
            base_ln: num(t, r, "BASELN")?,
            memo_no: text(t, r, "MEMO_NO")?,
            from_ref: text(t, r, "FROM_REF")?,
            rev_code: text(t, r, "REV_CODE")?,
            fx_cytd: num(t, r, "FXCYTD")?,
            fx_pytd: num(t, r, "FXPYTD")?,
            c_rate: num(t, r, "C_RATE")?,
            fxtype: text(t, r, "FXTYPE")?,
            status: text(t, r, "STATUS")?,
            tax_id: text(t, r, "TAX_ID")?,
            locked: flag(t, r, "LOCKED")?,
            hidden: flag(t, r, "HIDDEN")?,
            pac_no: text(t, r, "PAC_NO")?,
            cwguid: text(t, r, "CWGUID")?,
            cwdate: text(t, r, "CWDATE")?,
            createdon: text(t, r, "CREATEDON")?,
            balstat: text(t, r, "BALSTAT")?,
            cwchecksum: text(t, r, "CWCHECKSUM")?,
            conschksum: text(t, r, "CONSCHKSUM")?,
            extchksum: text(t, r, "EXTCHKSUM")?,
            memo_ref: text(t, r, "MEMO")?,
            uid_note_ref: text(t, r, "UID_NOTE")?,
            exinfo_ref: text(t, r, "EXINFO")?,
            unused42: text(t, r, "UNUSED42")?,
        })
    }

    /// Rows that belong in the trial balance (`TYPE` B/I).
    pub fn is_posting(&self) -> bool {
        self.kind.is_posting()
    }

    pub fn is_hidden(&self) -> bool {
        self.props.hidden.is_yes()
    }

    /// True when `POST='Y'` (CW default for a chart account).
    pub fn is_posted(&self) -> bool {
        self.props.post.is_yes()
    }

    /// GIFI emission gate (F54/F55): non-blank `GIFI` and a year-0 balance.
    pub fn gifi_eligible(&self) -> bool {
        !self.gifi.trim().is_empty() && !(self.open == 0.0 && self.cur_ytd == 0.0)
    }

    /// Signed current-year amount per the `SIGN` column (blank = debit).
    pub fn signed_ytd(&self) -> f64 {
        match self.props.sign {
            Side::Credit => -self.cur_ytd,
            _ => self.cur_ytd,
        }
    }
}

/// A chart-level consistency problem. `row` is the physical AM row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountIssue {
    pub row: usize,
    pub ac_no: String,
    pub kind: AccountIssueKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountIssueKind {
    /// Posting row without an `AC_NO`.
    MissingNumber,
    /// Two posting rows share an `AC_NO`.
    DuplicateNumber,
    /// `TYPE` letter outside B/I/M.
    UnknownType,
    /// `SIGN`/`DC`/flag value outside the legal set (decoder tolerates, gate flags).
    BadProperty,
}

impl fmt::Display for AccountIssueKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            AccountIssueKind::MissingNumber => "missing account number",
            AccountIssueKind::DuplicateNumber => "duplicate account number",
            AccountIssueKind::UnknownType => "unknown account TYPE",
            AccountIssueKind::BadProperty => "bad property flag",
        };
        f.write_str(s)
    }
}

impl fmt::Display for AccountIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "row {} ({}): {}", self.row, self.ac_no, self.kind)
    }
}

/// Chart totals (raw sums; no sign normalization beyond the stored columns).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChartTotals {
    pub open: f64,
    pub cur_mth: f64,
    pub cur_ytd: f64,
    pub aje_bal: f64,
    pub eje_bal: f64,
    pub rje_bal: f64,
    /// Sum of `CUR_YTD` over `TYPE='B'` posting rows.
    pub balance_sheet_ytd: f64,
    /// Sum of `CUR_YTD` over `TYPE='I'` posting rows.
    pub income_ytd: f64,
}

/// The engagement chart of accounts: decoded rows + account-number index.
#[derive(Clone, Debug)]
pub struct Chart {
    accounts: Vec<AmAccount>,
    by_no: BTreeMap<String, Vec<usize>>,
}

impl Chart {
    /// Build the chart. Refuses a non-canonical physical schema, so a silently
    /// drifted AM file cannot be mistaken for a decoded chart.
    pub fn from_table(t: &Table) -> Result<Chart> {
        if t.fields.len() != AM_FIELD_COUNT {
            return Err(CoreError::Format(format!(
                "AM has {} fields, expected {AM_FIELD_COUNT}",
                t.fields.len()
            )));
        }
        let drift = schema_drift(t);
        if let Some(first) = drift.first() {
            return Err(CoreError::Format(format!(
                "AM schema drift at {} ({}): expected {:?}, actual {:?}",
                first.column, first.kind, first.expected, first.actual
            )));
        }
        let mut accounts = Vec::with_capacity(t.records.len());
        let mut by_no: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for r in 0..t.records.len() {
            // tombstones are not chart accounts
            if t.is_deleted(r) {
                continue;
            }
            let a = AmAccount::from_table(t, r)?;
            if a.is_posting() && !a.ac_no.is_empty() {
                by_no.entry(a.ac_no.clone()).or_default().push(accounts.len());
            }
            accounts.push(a);
        }
        Ok(Chart { accounts, by_no })
    }

    pub fn accounts(&self) -> &[AmAccount] {
        &self.accounts
    }

    pub fn len(&self) -> usize {
        self.accounts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    /// Posting rows (`TYPE` B/I), in physical order.
    pub fn posting(&self) -> impl Iterator<Item = &AmAccount> {
        self.accounts.iter().filter(|a| a.is_posting())
    }

    /// First posting account with this number.
    pub fn get(&self, ac_no: &str) -> Option<&AmAccount> {
        self.by_no
            .get(ac_no)
            .and_then(|rows| rows.first())
            .map(|&i| &self.accounts[i])
    }

    /// All posting rows sharing a number (normally one; duplicates are issues).
    pub fn get_all(&self, ac_no: &str) -> impl Iterator<Item = &AmAccount> {
        self.by_no
            .get(ac_no)
            .into_iter()
            .flatten()
            .map(move |&i| &self.accounts[i])
    }

    /// Distinct posting account numbers in sorted order.
    pub fn numbers(&self) -> impl Iterator<Item = &str> {
        self.by_no.keys().map(|s| s.as_str())
    }

    /// `GetAccounts(gatAccount)` — posting rows that are not `POST='N'`.
    ///
    /// This is the shipped 324-of-325 set: `AM` posting rows minus the computed
    /// `NETINC` row, whose `POST` is `N`. Physical order is preserved.
    pub fn gat_account(&self) -> Vec<&AmAccount> {
        self.accounts
            .iter()
            .filter(|a| a.is_posting() && !a.props.post.is_no())
            .collect()
    }

    /// `GetAccounts(kind)` for every `CWGetAccountsType` value.
    ///
    /// [`GetAccountsType::Account`] is implemented ([`Chart::gat_account`]). The
    /// other five classes are refused with
    /// [`CoreError::Format`] naming the unproven predicate: their physical
    /// selectors are not established, and returning the posting set under
    /// another class's name would be a fabricated answer.
    pub fn get_accounts(&self, kind: GetAccountsType) -> Result<AccountsView<'_>> {
        match kind.predicate() {
            AccountsPredicate::Posting => Ok(AccountsView {
                kind,
                rows: self.gat_account(),
            }),
            AccountsPredicate::Unproven => Err(CoreError::Format(format!(
                "GetAccounts({}) predicate is not established: the physical selector \
                 for this class has no code or corpus evidence",
                kind.name()
            ))),
        }
    }

    pub fn totals(&self) -> ChartTotals {
        let mut t = ChartTotals::default();
        for a in self.posting() {
            t.open += a.open;
            t.cur_mth += a.cur_mth;
            t.cur_ytd += a.cur_ytd;
            t.aje_bal += a.aje_bal;
            t.eje_bal += a.eje_bal;
            t.rje_bal += a.rje_bal;
            match a.kind {
                AccountKind::BalanceSheet => t.balance_sheet_ytd += a.cur_ytd,
                AccountKind::Income => t.income_ytd += a.cur_ytd,
                _ => {}
            }
        }
        t
    }

    /// Chart validation: missing/duplicate numbers and unknown types.
    pub fn issues(&self) -> Vec<AccountIssue> {
        let mut out = Vec::new();
        for a in &self.accounts {
            if let AccountKind::Other(_) = a.kind {
                out.push(AccountIssue {
                    row: a.row,
                    ac_no: a.ac_no.clone(),
                    kind: AccountIssueKind::UnknownType,
                });
                continue;
            }
            if !a.is_posting() {
                // memory/non-posting rows (TYPE M and friends) are not chart
                // accounts; duplicate marker numbers are expected there.
                continue;
            }
            if a.ac_no.is_empty() {
                out.push(AccountIssue {
                    row: a.row,
                    ac_no: String::new(),
                    kind: AccountIssueKind::MissingNumber,
                });
                continue;
            }
            if self.by_no.get(&a.ac_no).map_or(0, |v| v.len()) > 1 {
                out.push(AccountIssue {
                    row: a.row,
                    ac_no: a.ac_no.clone(),
                    kind: AccountIssueKind::DuplicateNumber,
                });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_81_columns() {
        assert_eq!(AM_SCHEMA.len(), AM_FIELD_COUNT);
        let mut seen = std::collections::BTreeSet::new();
        for f in AM_SCHEMA.iter() {
            assert!(seen.insert(f.name), "duplicate column {}", f.name);
            assert!(family_of(f.name).is_some(), "no family for {}", f.name);
        }
        // every family is non-empty
        for fam in Family::ALL {
            assert!(family_columns(fam).next().is_some(), "empty family {fam:?}");
        }
        let sum: usize = Family::ALL
            .iter()
            .map(|&f| family_columns(f).count())
            .sum();
        assert_eq!(sum, AM_FIELD_COUNT, "families must partition the schema");
        assert_eq!(
            AM_SCHEMA.iter().map(|f| f.len as usize).sum::<usize>() + 1,
            AM_RECORD_LEN
        );
    }

    #[test]
    fn flags_and_sides_validate() {
        assert!(Flag::parse("Y").unwrap().is_yes());
        assert!(Flag::parse(" n ").unwrap().is_no());
        assert!(!Flag::parse("").unwrap().is_set());
        assert!(Flag::parse("X").is_err());
        assert_eq!(Side::parse("d").unwrap(), Side::Debit);
        assert_eq!(Side::parse("").unwrap(), Side::Unset);
        assert!(Side::parse("DR").is_err());
        assert_eq!(AccountKind::parse("m").unwrap(), AccountKind::Memo);
        assert!(!AccountKind::parse("m").unwrap().is_posting());
        assert!(AccountKind::parse("BI").is_err());
        assert_eq!(AccountKind::Other(b'Q').code(), Some(b'Q'));
    }
}

