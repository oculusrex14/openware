//! cw-app groups / mappings - the `MP` tree model (P5.E2).
//!
//! Contract: `docs\specs\groupings.md` (v1.1); schema frozen from
//! `inventory\dbf-schema.tsv` (67 fields / 1332-byte records) and re-verified
//! against `tests\golden\dbf\{hull2024,corp2026,mwgh2025}\*MP.dbf`.
//!
//! Three layers, mirroring [`crate::accounts`]:
//!
//! 1. [`MP_SCHEMA`] - the 67-column physical layout.
//! 2. [`MapNode`] - one decoded grouping node (`MAP_NO`, `AC_DESC`, `TITLE`,
//!    `MAPTYPE`, `CLASS`, dotted hierarchy, lead-schedule link).
//! 3. [`MpTable`] / [`MpScheme`] - the per-scheme trees plus the account
//!    assignment surface on the `AM` `GROUPn` columns.
//!
//! ## What the corpus actually shows (F36a amendment)
//!
//! `MP.ID` is a **scheme selector**, not a parent link: it is one character
//! (`'1'..'9'`, `'M'`, `'W'`, or blank) and constant across a whole scheme
//! block (hull: 1/2/3/5/8/9/M/W, corp: 1/7/8/M, mwgh: 1/8/9/M/W).
//! Hierarchy is encoded in the dotted `MAP_NO` (`111` -> `111.100`).
//!
//! Assignment law (T2/F37, verified): `AM.GROUPn` holds the node's `MAP_NO`
//! **verbatim** and the digit `n` selects the scheme `MP.ID='n'`
//! (hull `GROUP2`<->scheme `2`, `GROUP9`<->scheme `9`; mwgh `GROUP8`<->`8`).
//! `AM` has only nine slot columns - there is **no** `GROUP1` - so slot 1 is
//! unaddressable on an account (use [`ASSIGNABLE_DIGITS`]).
//!
//! `MP.GROUP1` is the node's **lead-schedule link** (the `AM.SCHEDULE` code
//! space: `8200`, `4000`, `8100`, ...), not a grouping assignment. The
//! `AM`-side counterpart is `SCHEDULE` / `SCHEDULECR`.
//!
//! Write law: assignment writes go through [`crate::accounts::write_cell`]
//! (width/type validation, atomic refusal). Decoding and re-writing a corpus
//! `MP` table is byte-identical (`tests\groupings_gate.rs`).

use std::collections::BTreeMap;
use std::fmt;

use cw_core::dbf::Table;
use cw_core::{CoreError, Result};

use crate::accounts::{
    group_slot, write_cell, DriftKind, Flag, SchemaDrift, Side, GROUP_COLUMNS,
};

/// Physical `MP` field count (all three corpus schemas).
pub const MP_FIELD_COUNT: usize = 67;
/// Physical `MP` record length, deletion flag included.
pub const MP_RECORD_LEN: usize = 1332;

/// One field descriptor of the canonical `MP` layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MpField {
    pub name: &'static str,
    /// dBase type letter (`C`/`N`/`M`/`L` in the corpus).
    pub kind: u8,
    pub len: u8,
    pub dec: u8,
}

const fn f(name: &'static str, kind: u8, len: u8, dec: u8) -> MpField {
    MpField { name, kind, len, dec }
}

/// Canonical `MP` schema in physical order (67 columns).
pub static MP_SCHEMA: [MpField; MP_FIELD_COUNT] = [
    f("LINENO", b'N', 8, 0),
    f("ID", b'C', 1, 0),
    f("TYPE", b'C', 1, 0),
    f("FLIPTYPE", b'C', 1, 0),
    f("CLASS", b'C', 2, 0),
    f("MAP_NO", b'C', 40, 0),
    f("MAP_NOCR", b'C', 40, 0),
    f("AC_DESC", b'C', 50, 0),
    f("TAC_NO", b'C', 16, 0),
    f("TAX1", b'C', 16, 0),
    f("TAX2", b'C', 16, 0),
    f("TAX3", b'C', 16, 0),
    f("TAX4", b'C', 16, 0),
    f("GIFI", b'C', 10, 0),
    f("CC", b'C', 3, 0),
    f("DC", b'C', 3, 0),
    f("COLUMN", b'C', 1, 0),
    f("DOLLAR", b'C', 1, 0),
    f("ULINE", b'C', 1, 0),
    f("PAGE", b'C', 1, 0),
    f("SIGN", b'C', 1, 0),
    f("PRINT", b'C', 1, 0),
    f("CTL", b'C', 2, 0),
    f("TOTAL", b'C', 1, 0),
    f("MEMO", b'M', 10, 0),
    f("GROUP0", b'C', 40, 0),
    f("GROUP0CR", b'C', 40, 0),
    f("GROUP1", b'C', 40, 0),
    f("GROUP1CR", b'C', 40, 0),
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
    f("TITLE", b'L', 1, 0),
    f("UNITS", b'C', 4, 0),
    f("UNITS_B", b'C', 4, 0),
    f("DEN", b'C', 40, 0),
    f("BM", b'N', 19, 2),
    f("FXCYTD", b'N', 13, 5),
    f("PROP", b'C', 1, 0),
    f("CONV", b'N', 1, 0),
    f("MAPTYPE", b'C', 1, 0),
    f("FORMULA", b'M', 10, 0),
    f("WP_REF", b'C', 40, 0),
    f("ECODE", b'C', 25, 0),
    f("TAGS", b'M', 10, 0),
    f("TAX_ID", b'C', 4, 0),
    f("CWGUID", b'C', 22, 0),
    f("FXTYPE", b'C', 1, 0),
    f("CWDATE", b'C', 14, 0),
    f("CREATEDON", b'C', 14, 0),
    f("MT1", b'C', 16, 0),
    f("MT2", b'C', 16, 0),
    f("MT3", b'C', 16, 0),
    f("AUTOFILL", b'C', 1, 0),
];

/// Look up a canonical `MP` field by name.
pub fn mp_schema_field(name: &str) -> Option<&'static MpField> {
    let n = name.trim().to_ascii_uppercase();
    MP_SCHEMA.iter().find(|f| f.name == n)
}

/// Full physical-schema comparison against [`MP_SCHEMA`] (order, names, types,
/// widths, decimals). Shares [`SchemaDrift`] with the `AM` model.
pub fn mp_schema_drift(t: &Table) -> Vec<SchemaDrift> {
    let mut out = Vec::new();
    let actual: Vec<String> = t
        .fields
        .iter()
        .map(|fd| fd.name.to_ascii_uppercase())
        .collect();

    for e in MP_SCHEMA.iter() {
        if !actual.iter().any(|n| n == e.name) {
            out.push(SchemaDrift {
                column: e.name.to_string(),
                kind: DriftKind::Missing,
                expected: format!("{}{}.{}", e.kind as char, e.len, e.dec),
                actual: String::new(),
            });
        }
    }
    for a in actual.iter() {
        if !MP_SCHEMA.iter().any(|e| e.name == a) {
            out.push(SchemaDrift {
                column: a.clone(),
                kind: DriftKind::Extra,
                expected: String::new(),
                actual: a.clone(),
            });
        }
    }
    for (i, fd) in t.fields.iter().enumerate() {
        let name = fd.name.to_ascii_uppercase();
        let Some(e) = MP_SCHEMA.iter().find(|e| e.name == name) else {
            continue;
        };
        let expected_pos = MP_SCHEMA.iter().position(|x| x.name == name).unwrap();
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

/// True when the table is the canonical 67-column `MP` layout.
pub fn mp_schema_matches(t: &Table) -> bool {
    mp_schema_drift(t).is_empty()
}

fn text(t: &Table, r: usize, col: &str) -> Result<String> {
    t.cell_str(r, col)
        .map(|s| s.trim_end().to_string())
        .ok_or_else(|| CoreError::Format(format!("row {r}: no column {col}")))
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

/// `MP.ID` scheme selector.
///
/// The corpus uses digits `1..=9` (leadsheet grouping schemes), `M` (the
/// master statement-mapping tree), the stray letter `W`, and blank.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scheme {
    /// Digit scheme; the digit is also the `AM.GROUPn` slot.
    Digit(u8),
    /// `MP.ID='M'` - the statement mapping tree.
    Mapping,
    /// Any other single scheme letter observed in a file.
    Other(u8),
    /// Blank `MP.ID` (sentinel/stray row).
    Unassigned,
}

impl Scheme {
    pub fn parse(raw: &str) -> Result<Scheme> {
        let s = raw.trim();
        let bytes = s.as_bytes();
        Ok(match bytes {
            [] => Scheme::Unassigned,
            [b'0'..=b'9'] => Scheme::Digit(bytes[0] - b'0'),
            [b'm' | b'M'] => Scheme::Mapping,
            [c] if c.is_ascii_alphabetic() => Scheme::Other(c.to_ascii_uppercase()),
            _ => {
                return Err(CoreError::Format(format!(
                    "invalid MP.ID scheme {s:?} (expected one digit/letter or blank)"
                )))
            }
        })
    }

    pub fn as_str(self) -> String {
        match self {
            Scheme::Digit(d) => ((b'0' + d) as char).to_string(),
            Scheme::Mapping => "M".to_string(),
            Scheme::Other(c) => (c as char).to_string(),
            Scheme::Unassigned => String::new(),
        }
    }

    /// Slot digit when this is a digit scheme.
    pub fn digit(self) -> Option<u8> {
        match self {
            Scheme::Digit(d) => Some(d),
            _ => None,
        }
    }

    /// `AM` column carrying assignments for this scheme, if any.
    pub fn am_column(self) -> Option<&'static str> {
        let d = self.digit()?;
        let slot = group_slot(d)?;
        Some(GROUP_COLUMNS[slot])
    }

    pub fn label(self) -> &'static str {
        match self {
            Scheme::Digit(_) => "leadsheet grouping",
            Scheme::Mapping => "statement mapping",
            Scheme::Other(_) => "other",
            Scheme::Unassigned => "unassigned",
        }
    }
}

impl fmt::Display for Scheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scheme::Digit(d) => write!(f, "digit {d}"),
            Scheme::Mapping => f.write_str("M"),
            Scheme::Other(c) => write!(f, "scheme {}", *c as char),
            Scheme::Unassigned => f.write_str("(blank)"),
        }
    }
}

/// Digits that have an `AM` assignment column. Slot 1 has none.
pub const ASSIGNABLE_DIGITS: [u8; 9] = [0, 2, 3, 4, 5, 6, 7, 8, 9];

/// Node behavior (`CWBehaviourType`), decoded from `TITLE` + `MAPTYPE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    /// `TITLE='Y'` - a heading, never assigned to an account.
    Title,
    /// Normal assignable node (blank `TITLE`, blank `MAPTYPE`).
    Assignable,
    /// `TITLE='F'` - filtered node.
    Filtered,
    /// `MAPTYPE='C'` - calculated node (has a `FORMULA` memo pointer).
    Calculated,
    /// `MAPTYPE='U'` - unassignable node.
    Unassignable,
    /// Letter outside the observed set; kept so decode never panics.
    Other(u8),
}

impl NodeKind {
    /// Decode precedence: `TITLE` letter first, then `MAPTYPE` letter.
    pub fn parse(title: &str, map_type: &str) -> NodeKind {
        let t = title.trim().to_ascii_uppercase();
        let m = map_type.trim().to_ascii_uppercase();
        match t.as_bytes() {
            [b'Y'] => return NodeKind::Title,
            [b'F'] => return NodeKind::Filtered,
            [] | [b'N'] => {}
            [c] => return NodeKind::Other(*c),
            _ => return NodeKind::Other(0),
        }
        match m.as_bytes() {
            [b'C'] => NodeKind::Calculated,
            [b'U'] => NodeKind::Unassignable,
            [] => NodeKind::Assignable,
            [c] => NodeKind::Other(*c),
            _ => NodeKind::Other(0),
        }
    }

    /// Accounts may only be assigned to plain nodes.
    pub fn is_assignable(self) -> bool {
        matches!(self, NodeKind::Assignable)
    }

    /// Rows that render as a statement/leadsheet line.
    pub fn is_heading(self) -> bool {
        matches!(self, NodeKind::Title)
    }

    pub fn label(self) -> &'static str {
        match self {
            NodeKind::Title => "title",
            NodeKind::Assignable => "normal",
            NodeKind::Filtered => "filtered",
            NodeKind::Calculated => "calculated",
            NodeKind::Unassignable => "unassignable",
            NodeKind::Other(_) => "unknown",
        }
    }
}

impl fmt::Display for NodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// `MP` presentation columns (the subset shared with the account dialog; `MP`
/// has no `POST`/`HIDDEN`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MapProperties {
    pub column: Flag,
    pub dollar: Flag,
    pub uline: Flag,
    pub page: Flag,
    pub sign: Side,
    pub print: Flag,
    pub ctl: String,
    pub total: Flag,
    pub den: String,
    pub dc: Side,
}

/// Physical `MP` columns backing [`MapProperties`].
pub const MAP_PROPERTY_COLUMNS: [&str; 10] = [
    "COLUMN", "DOLLAR", "ULINE", "PAGE", "SIGN", "PRINT", "CTL", "TOTAL", "DEN", "DC",
];

impl MapProperties {
    pub fn from_table(t: &Table, r: usize) -> Result<MapProperties> {
        Ok(MapProperties {
            column: Flag::parse(&text(t, r, "COLUMN")?)?,
            dollar: Flag::parse(&text(t, r, "DOLLAR")?)?,
            uline: Flag::parse(&text(t, r, "ULINE")?)?,
            page: Flag::parse(&text(t, r, "PAGE")?)?,
            sign: Side::parse(&text(t, r, "SIGN")?)?,
            print: Flag::parse(&text(t, r, "PRINT")?)?,
            ctl: text(t, r, "CTL")?,
            total: Flag::parse(&text(t, r, "TOTAL")?)?,
            den: text(t, r, "DEN")?,
            dc: Side::parse(&text(t, r, "DC")?)?,
        })
    }

    /// Round-trip every property column; byte-identical on the corpus.
    pub fn write_to(&self, t: &mut Table, r: usize) -> Result<()> {
        write_cell(t, r, "COLUMN", self.column.raw())?;
        write_cell(t, r, "DOLLAR", self.dollar.raw())?;
        write_cell(t, r, "ULINE", self.uline.raw())?;
        write_cell(t, r, "PAGE", self.page.raw())?;
        write_cell(t, r, "SIGN", self.sign.as_str())?;
        write_cell(t, r, "PRINT", self.print.raw())?;
        write_cell(t, r, "CTL", &self.ctl)?;
        write_cell(t, r, "TOTAL", self.total.raw())?;
        write_cell(t, r, "DEN", &self.den)?;
        write_cell(t, r, "DC", self.dc.as_str())?;
        Ok(())
    }
}

/// One decoded `MP` grouping node.
#[derive(Clone, Debug, PartialEq)]
pub struct MapNode {
    /// Physical `MP` row.
    pub row: usize,
    pub line_no: i64,
    pub scheme: Scheme,
    /// `TYPE` mirrors the account side (`B`/`I`), blank on pure headings.
    pub account_type: String,
    pub flip_type: String,
    /// Lead sheet class letter pair (`QA`, `OE`, `CL`, ...).
    pub class: String,
    /// Node code, exactly as stored (`8300.02`, `MFG-100`, ...).
    pub map_no: String,
    pub map_no_cr: String,
    pub desc: String,
    pub title_raw: String,
    pub map_type_raw: String,
    pub kind: NodeKind,
    pub tac_no: String,
    pub gifi: String,
    pub props: MapProperties,
    /// `MP.GROUP1` - the node's lead-schedule code (`AM.SCHEDULE` space).
    pub lead_schedule: String,
    /// `MP.GROUP1CR` - credit-side lead-schedule code.
    pub lead_schedule_cr: String,
    pub units: String,
    pub units_b: String,
    pub bm: f64,
    pub fx_cytd: f64,
    pub conv: i64,
    pub wp_ref: String,
    pub ecode: String,
    pub tax_id: String,
    pub cwguid: String,
    /// Raw memo (FPT) pointer cells (`MEMO`, `FORMULA`, `TAGS`).
    pub memo_ref: String,
    pub formula_ref: String,
    pub tags_ref: String,
}

impl MapNode {
    /// Decode one physical `MP` row.
    pub fn from_table(t: &Table, r: usize) -> Result<MapNode> {
        let title_raw = text(t, r, "TITLE")?;
        let map_type_raw = text(t, r, "MAPTYPE")?;
        Ok(MapNode {
            row: r,
            line_no: int(t, r, "LINENO")?,
            scheme: Scheme::parse(&text(t, r, "ID")?)?,
            account_type: text(t, r, "TYPE")?,
            flip_type: text(t, r, "FLIPTYPE")?,
            class: text(t, r, "CLASS")?,
            map_no: text(t, r, "MAP_NO")?,
            map_no_cr: text(t, r, "MAP_NOCR")?,
            desc: text(t, r, "AC_DESC")?,
            kind: NodeKind::parse(&title_raw, &map_type_raw),
            title_raw,
            map_type_raw,
            tac_no: text(t, r, "TAC_NO")?,
            gifi: text(t, r, "GIFI")?,
            props: MapProperties::from_table(t, r)?,
            lead_schedule: text(t, r, "GROUP1")?,
            lead_schedule_cr: text(t, r, "GROUP1CR")?,
            units: text(t, r, "UNITS")?,
            units_b: text(t, r, "UNITS_B")?,
            bm: num(t, r, "BM")?,
            fx_cytd: num(t, r, "FXCYTD")?,
            conv: int(t, r, "CONV")?,
            wp_ref: text(t, r, "WP_REF")?,
            ecode: text(t, r, "ECODE")?,
            tax_id: text(t, r, "TAX_ID")?,
            cwguid: text(t, r, "CWGUID")?,
            memo_ref: text(t, r, "MEMO")?,
            formula_ref: text(t, r, "FORMULA")?,
            tags_ref: text(t, r, "TAGS")?,
        })
    }

    /// Blank `MAP_NO` rows are sentinels and cannot be assignment targets.
    pub fn has_code(&self) -> bool {
        !self.map_no.trim().is_empty()
    }

    pub fn is_calculated(&self) -> bool {
        self.kind == NodeKind::Calculated
    }

    /// True when the node carries a lead-schedule code.
    pub fn has_lead_schedule(&self) -> bool {
        !self.lead_schedule.trim().is_empty()
    }
}

/// A structural problem in an `MP` table (non-fatal; the tree still builds).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MpIssue {
    pub row: usize,
    pub scheme: Scheme,
    pub map_no: String,
    pub kind: MpIssueKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MpIssueKind {
    /// Two nodes in one scheme share a `MAP_NO`.
    DuplicateCode,
    /// Blank `MAP_NO` (allowed for scheme sentinels, flagged otherwise).
    MissingCode,
    /// `TITLE`/`MAPTYPE` letter outside the observed set.
    UnknownNodeKind,
    /// A node whose dotted parent prefix is absent from the scheme.
    OrphanCode,
}

impl fmt::Display for MpIssueKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            MpIssueKind::DuplicateCode => "duplicate MAP_NO",
            MpIssueKind::MissingCode => "blank MAP_NO",
            MpIssueKind::UnknownNodeKind => "unknown TITLE/MAPTYPE",
            MpIssueKind::OrphanCode => "orphan MAP_NO (missing parent prefix)",
        })
    }
}

/// A dangling or illegal account-to-node assignment (F37 gate hook).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssignmentIssue {
    pub row: usize,
    pub ac_no: String,
    /// Assignment digit (`2..=9`, `0`).
    pub digit: u8,
    /// Value found in the `AM` slot.
    pub code: String,
    pub kind: AssignmentIssueKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignmentIssueKind {
    /// No `MP` node with that code in the slot's scheme.
    UnknownTarget,
    /// The target is a title/calculated/filtered/unassignable node.
    NotAssignable,
    /// `AM` slot value does not round-trip verbatim (padding/case drift).
    NotVerbatim,
}

impl fmt::Display for AssignmentIssueKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            AssignmentIssueKind::UnknownTarget => "assignment target missing from MP",
            AssignmentIssueKind::NotAssignable => "assignment target is not assignable",
            AssignmentIssueKind::NotVerbatim => "assignment value is not verbatim",
        })
    }
}

/// Infer a node's parent code from its `MAP_NO`.
///
/// `MP` stores no parent pointer (`MP.ID` is the scheme, `GROUPn` are
/// assignment/lead-schedule links), so the hierarchy is derived:
///
/// 1. **Dotted nesting**: `111.100` -> `111` (longest dotted prefix present).
/// 2. **Standard mapping numbering**: a 3/4-digit code nests under the
///    longest shorter zero-suffix peer - `111` -> `110`, `130` -> `100`,
///    `173` -> `170` (the CaseWare 100/110/111 template convention).
///
/// `lookup` maps an existing code to an index in the scheme. Codes that match
/// neither rule are roots. The rule is a documented inference (spec v1.1),
/// not a stored field; it is centralised here so a future RE pass can replace it.
pub fn inferred_parent<T>(code: &str, lookup: impl Fn(&str) -> Option<T>) -> Option<T> {
    if let Some(pos) = code.rfind('.') {
        return lookup(&code[..pos]);
    }
    let bytes = code.as_bytes();
    if bytes.is_empty() || !bytes.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // Try the longest zero-filled ancestor first: 111 -> 110 -> 100.
    for cut in (1..bytes.len()).rev() {
        let mut candidate = String::with_capacity(bytes.len());
        candidate.push_str(&code[..cut]);
        candidate.extend(std::iter::repeat('0').take(bytes.len() - cut));
        if candidate != code {
            if let Some(hit) = lookup(&candidate) {
                return Some(hit);
            }
        }
    }
    None
}

/// One scheme's node set + derived hierarchy.
#[derive(Clone, Debug)]
pub struct MpScheme {
    scheme: Scheme,
    nodes: Vec<MapNode>,
    by_code: BTreeMap<String, usize>,
    /// Child indices per parent index; roots are not listed here.
    children: BTreeMap<usize, Vec<usize>>,
    roots: Vec<usize>,
}

impl MpScheme {
    fn from_nodes(scheme: Scheme, nodes: Vec<MapNode>) -> MpScheme {
        let mut by_code: BTreeMap<String, usize> = BTreeMap::new();
        for (i, n) in nodes.iter().enumerate() {
            if n.has_code() {
                by_code.entry(n.map_no.clone()).or_insert(i);
            }
        }
        let mut parent: Vec<Option<usize>> = vec![None; nodes.len()];
        for (i, n) in nodes.iter().enumerate() {
            if !n.has_code() {
                continue;
            }
            if let Some(p) = inferred_parent(&n.map_no, |c| by_code.get(c).copied()) {
                if p != i {
                    parent[i] = Some(p);
                }
            }
        }
        let mut children: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        let mut roots = Vec::new();
        for (i, p) in parent.iter().enumerate() {
            match p {
                Some(p) => children.entry(*p).or_default().push(i),
                None => roots.push(i),
            }
        }
        MpScheme {
            scheme,
            nodes,
            by_code,
            children,
            roots,
        }
    }

    pub fn scheme(&self) -> Scheme {
        self.scheme
    }

    /// Every node in physical row order (blank-code sentinels included).
    pub fn nodes(&self) -> &[MapNode] {
        &self.nodes
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn get(&self, code: &str) -> Option<&MapNode> {
        self.by_code.get(code).map(|&i| &self.nodes[i])
    }

    pub fn contains(&self, code: &str) -> bool {
        self.by_code.contains_key(code)
    }

    /// Coded nodes only, in physical order.
    pub fn coded(&self) -> impl Iterator<Item = &MapNode> {
        self.nodes.iter().filter(|n| n.has_code())
    }

    /// Root node indices (coded roots first; blank sentinels are roots too).
    pub fn roots(&self) -> &[usize] {
        &self.roots
    }

    pub fn node_at(&self, idx: usize) -> Option<&MapNode> {
        self.nodes.get(idx)
    }

    pub fn children_of(&self, code: &str) -> Vec<&MapNode> {
        self.by_code
            .get(code)
            .and_then(|&i| self.children.get(&i))
            .map(|v| v.iter().map(|&i| &self.nodes[i]).collect())
            .unwrap_or_default()
    }

    /// Parent chain from the node up to a root (excluding the node).
    pub fn ancestors(&self, code: &str) -> Vec<&MapNode> {
        let mut out = Vec::new();
        let mut cur = self.by_code.get(code).copied();
        while let Some(i) = cur {
            let n = &self.nodes[i];
            match inferred_parent(&n.map_no, |c| self.by_code.get(c).copied()) {
                Some(pi) => {
                    out.push(&self.nodes[pi]);
                    cur = Some(pi);
                }
                None => break,
            }
        }
        out
    }

    /// Dotted path (`Assets / Current Assets / Cash`), root first.
    pub fn path(&self, code: &str) -> Vec<&MapNode> {
        let mut chain = self.ancestors(code);
        chain.reverse();
        if let Some(n) = self.get(code) {
            chain.push(n);
        }
        chain
    }

    pub fn depth(&self, code: &str) -> usize {
        self.ancestors(code).len()
    }

    /// Every descendant (pre-order), excluding the node itself.
    pub fn descendants(&self, code: &str) -> Vec<&MapNode> {
        let Some(&i) = self.by_code.get(code) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut stack: Vec<usize> = self
            .children
            .get(&i)
            .map(|v| v.iter().rev().copied().collect())
            .unwrap_or_default();
        while let Some(idx) = stack.pop() {
            out.push(&self.nodes[idx]);
            if let Some(kids) = self.children.get(&idx) {
                stack.extend(kids.iter().rev().copied());
            }
        }
        out
    }

    /// Pre-order traversal over coded nodes starting at the roots.
    pub fn walk(&self) -> Vec<&MapNode> {
        let mut out = Vec::new();
        for &root in &self.roots {
            if !self.nodes[root].has_code() {
                continue;
            }
            out.push(&self.nodes[root]);
            out.extend(self.descendants(&self.nodes[root].map_no));
        }
        out
    }

    /// Structural issues for this scheme.
    pub fn issues(&self) -> Vec<MpIssue> {
        let mut out = Vec::new();
        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        for n in &self.nodes {
            if !n.has_code() {
                out.push(MpIssue {
                    row: n.row,
                    scheme: self.scheme,
                    map_no: String::new(),
                    kind: MpIssueKind::MissingCode,
                });
                continue;
            }
            if seen.insert(n.map_no.as_str(), n.row).is_some() {
                out.push(MpIssue {
                    row: n.row,
                    scheme: self.scheme,
                    map_no: n.map_no.clone(),
                    kind: MpIssueKind::DuplicateCode,
                });
            }
            if matches!(n.kind, NodeKind::Other(_)) {
                out.push(MpIssue {
                    row: n.row,
                    scheme: self.scheme,
                    map_no: n.map_no.clone(),
                    kind: MpIssueKind::UnknownNodeKind,
                });
            }
        }
        out
    }
}

/// All grouping schemes of one engagement (`MP` decoded).
#[derive(Clone, Debug)]
pub struct MpTable {
    schemes: BTreeMap<Scheme, MpScheme>,
    deleted: usize,
    /// Nodes whose `MP.ID` was blank, kept out of the scheme map.
    unassigned: Vec<MapNode>,
}

impl MpTable {
    /// Build every scheme. Refuses a non-canonical physical schema.
    pub fn from_table(t: &Table) -> Result<MpTable> {
        if t.fields.len() != MP_FIELD_COUNT {
            return Err(CoreError::Format(format!(
                "MP has {} fields, expected {MP_FIELD_COUNT}",
                t.fields.len()
            )));
        }
        let drift = mp_schema_drift(t);
        if let Some(first) = drift.first() {
            return Err(CoreError::Format(format!(
                "MP schema drift at {} ({}): expected {:?}, actual {:?}",
                first.column, first.kind, first.expected, first.actual
            )));
        }
        let mut buckets: BTreeMap<Scheme, Vec<MapNode>> = BTreeMap::new();
        let mut unassigned = Vec::new();
        let mut deleted = 0usize;
        for r in 0..t.records.len() {
            if t.is_deleted(r) {
                deleted += 1;
                continue;
            }
            let node = MapNode::from_table(t, r)?;
            match node.scheme {
                Scheme::Unassigned => unassigned.push(node),
                s => buckets.entry(s).or_default().push(node),
            }
        }
        let schemes = buckets
            .into_iter()
            .map(|(s, nodes)| (s, MpScheme::from_nodes(s, nodes)))
            .collect();
        Ok(MpTable {
            schemes,
            deleted,
            unassigned,
        })
    }

    /// Tombstoned rows skipped at decode.
    pub fn deleted(&self) -> usize {
        self.deleted
    }

    pub fn scheme(&self, s: Scheme) -> Option<&MpScheme> {
        self.schemes.get(&s)
    }

    /// Digit scheme (`GROUPn` target).
    pub fn digit(&self, d: u8) -> Option<&MpScheme> {
        self.scheme(Scheme::Digit(d))
    }

    /// The master statement tree (`MP.ID='M'`).
    pub fn mapping(&self) -> Option<&MpScheme> {
        self.scheme(Scheme::Mapping)
    }

    pub fn schemes(&self) -> impl Iterator<Item = &MpScheme> {
        self.schemes.values()
    }

    pub fn scheme_keys(&self) -> Vec<Scheme> {
        self.schemes.keys().copied().collect()
    }

    /// Rows with blank `MP.ID`.
    pub fn unassigned(&self) -> &[MapNode] {
        &self.unassigned
    }

    /// First node with `code` in `scheme`.
    pub fn node(&self, scheme: Scheme, code: &str) -> Option<&MapNode> {
        self.scheme(scheme).and_then(|s| s.get(code))
    }

    /// First schemes containing the code, in scheme order.
    pub fn schemes_with(&self, code: &str) -> Vec<Scheme> {
        self.schemes
            .iter()
            .filter(|(_, s)| s.contains(code))
            .map(|(k, _)| *k)
            .collect()
    }

    /// Lead-schedule resolution (spec: `SCHEDULE` is MP-resolvable): the first
    /// node whose `MP.GROUP1` equals `code`.
    pub fn lead_schedule_nodes(&self, code: &str) -> Vec<&MapNode> {
        self.schemes
            .values()
            .flat_map(|s| s.nodes.iter())
            .filter(|n| n.lead_schedule.trim() == code)
            .collect()
    }

    /// Structural issues across every scheme.
    pub fn issues(&self) -> Vec<MpIssue> {
        let mut out = Vec::new();
        for s in self.schemes.values() {
            out.extend(s.issues());
        }
        out
    }

    fn target(&self, digit: u8, code: &str) -> Option<&MapNode> {
        self.digit(digit).and_then(|s| s.get(code))
    }

    /// F37 check: every `AM.GROUPn` value must be a verbatim `MAP_NO` of an
    /// assignable node in scheme `n`.
    pub fn assignment_issues(&self, am: &Table) -> Vec<AssignmentIssue> {
        let mut out = Vec::new();
        let ac_no_col = if am.field_index("AC_NO").is_some() {
            "AC_NO"
        } else {
            ""
        };
        for d in ASSIGNABLE_DIGITS {
            let Some(col) = Scheme::Digit(d).am_column() else {
                continue;
            };
            for r in 0..am.records.len() {
                if am.is_deleted(r) {
                    continue;
                }
                let Ok(raw) = text(am, r, col) else { continue };
                if raw.is_empty() {
                    continue;
                }
                let ac_no = if ac_no_col.is_empty() {
                    String::new()
                } else {
                    text(am, r, ac_no_col).unwrap_or_default()
                };
                let kind = match self.target(d, &raw) {
                    None => Some(AssignmentIssueKind::UnknownTarget),
                    Some(n) if !n.kind.is_assignable() => {
                        Some(AssignmentIssueKind::NotAssignable)
                    }
                    Some(n) if n.map_no != raw => Some(AssignmentIssueKind::NotVerbatim),
                    Some(_) => None,
                };
                if let Some(kind) = kind {
                    out.push(AssignmentIssue {
                        row: r,
                        ac_no,
                        digit: d,
                        code: raw,
                        kind,
                    });
                }
            }
        }
        out
    }
}

/// Column that stores a scheme's account assignments (`None` for scheme 1).
pub fn assignment_column(digit: u8) -> Option<&'static str> {
    Scheme::Digit(digit).am_column()
}

/// Read one assignment slot from an `AM` row.
pub fn account_assignment(am: &Table, row: usize, digit: u8) -> Result<String> {
    let col = assignment_column(digit).ok_or_else(|| {
        CoreError::Format(format!(
            "grouping slot {digit} has no AM column (AM carries slots 0 and 2..=9)"
        ))
    })?;
    text(am, row, col)
}

/// Assign a chart account to a grouping node: `AM.GROUPn := node.MAP_NO`.
///
/// Refuses (without writing) when:
/// * the slot digit has no `AM` column (`1`),
/// * the node is absent from scheme `n`,
/// * the node is a title / calculated / filtered / unassignable node.
///
/// The write itself is atomic (`write_cell` validates width and type).
pub fn assign_account(am: &mut Table, row: usize, mp: &MpTable, digit: u8, code: &str) -> Result<()> {
    let col = assignment_column(digit).ok_or_else(|| {
        CoreError::Format(format!(
            "grouping slot {digit} has no AM column (AM carries slots 0 and 2..=9)"
        ))
    })?;
    let node = mp.target(digit, code).ok_or_else(|| {
        CoreError::Format(format!(
            "no MP node {code:?} in scheme {digit} (assignment target must exist)"
        ))
    })?;
    if !node.kind.is_assignable() {
        return Err(CoreError::Format(format!(
            "MP node {code:?} is a {} node and cannot receive an account",
            node.kind
        )));
    }
    write_cell(am, row, col, &node.map_no)
}

/// Clear one assignment slot (blank write, keeps width).
pub fn clear_assignment(am: &mut Table, row: usize, digit: u8) -> Result<()> {
    let col = assignment_column(digit).ok_or_else(|| {
        CoreError::Format(format!("grouping slot {digit} has no AM column"))
    })?;
    write_cell(am, row, col, "")
}

/// All non-empty assignments in one slot: `(row, code)`.
pub fn assignments(am: &Table, digit: u8) -> Vec<(usize, String)> {
    let Some(col) = assignment_column(digit) else {
        return Vec::new();
    };
    (0..am.records.len())
        .filter(|&r| !am.is_deleted(r))
        .filter_map(|r| {
            let v = text(am, r, col).ok()?;
            (!v.is_empty()).then_some((r, v))
        })
        .collect()
}

/// Rows still unassigned in a slot.
pub fn unassigned_rows(am: &Table, digit: u8) -> Vec<usize> {
    let Some(col) = assignment_column(digit) else {
        return Vec::new();
    };
    (0..am.records.len())
        .filter(|&r| !am.is_deleted(r))
        .filter(|&r| text(am, r, col).map(|v| v.is_empty()).unwrap_or(false))
        .collect()
}

/// Accounts pointing at one node: `AM.GROUPn == node.MAP_NO`.
pub fn accounts_at(am: &Table, mp: &MpTable, scheme: Scheme, code: &str) -> Vec<(usize, String)> {
    let Some(col) = scheme.am_column() else {
        return Vec::new();
    };
    let Some(node) = mp.node(scheme, code) else {
        return Vec::new();
    };
    (0..am.records.len())
        .filter(|&r| !am.is_deleted(r))
        .filter_map(|r| {
            let v = text(am, r, col).ok()?;
            (v == node.map_no).then(|| {
                let ac = am
                    .cell_str(r, "AC_NO")
                    .map(|s| s.trim_end().to_string())
                    .unwrap_or_default();
                (r, ac)
            })
        })
        .collect()
}

/// Lead-schedule codes carried by `MP` nodes (the `AM.SCHEDULE` domain).
pub fn lead_schedule_codes(mp: &MpTable) -> Vec<String> {
    let mut out: Vec<String> = mp
        .schemes()
        .flat_map(|s| s.nodes.iter())
        .map(|n| n.lead_schedule.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Write `AM.SCHEDULE` for a row after checking the code resolves in `MP`.
///
/// `AM` stores lead-schedule codes verbatim (hierarchical `5000.100` is legal);
/// the MP side is `MP.GROUP1`.
pub fn set_lead_schedule(am: &mut Table, row: usize, mp: &MpTable, code: &str) -> Result<()> {
    let c = code.trim();
    if !c.is_empty() && mp.lead_schedule_nodes(c).is_empty() {
        return Err(CoreError::Format(format!(
            "lead schedule {c:?} is not referenced by any MP node"
        )));
    }
    write_cell(am, row, "SCHEDULE", c)
}

/// Read `AM.SCHEDULE` for a row.
pub fn lead_schedule(am: &Table, row: usize) -> Result<String> {
    text(am, row, "SCHEDULE")
}

/// Chart rows whose `SCHEDULE` is not referenced by any MP node.
///
/// This is the cross-table consistency hook for the P5.C `BL.GROUP` lane.
pub fn dangling_lead_schedules(am: &Table, mp: &MpTable) -> Vec<(usize, String, String)> {
    let codes: std::collections::BTreeSet<String> = lead_schedule_codes(mp).into_iter().collect();
    let mut out = Vec::new();
    for r in 0..am.records.len() {
        if am.is_deleted(r) {
            continue;
        }
        let Ok(code) = text(am, r, "SCHEDULE") else {
            continue;
        };
        if code.is_empty() || codes.contains(&code) {
            continue;
        }
        let ac = am
            .cell_str(r, "AC_NO")
            .map(|s| s.trim_end().to_string())
            .unwrap_or_default();
        out.push((r, ac, code));
    }
    out
}

/// Re-encode every decoded node column through `write_cell`; the corpus must
/// come back byte-identical.
pub fn rewrite_nodes(t: &mut Table) -> Result<()> {
    for r in 0..t.records.len() {
        let n = MapNode::from_table(t, r)?;
        write_cell(t, r, "MAP_NO", &n.map_no)?;
        write_cell(t, r, "MAP_NOCR", &n.map_no_cr)?;
        write_cell(t, r, "AC_DESC", &n.desc)?;
        write_cell(t, r, "TITLE", &n.title_raw)?;
        write_cell(t, r, "MAPTYPE", &n.map_type_raw)?;
        write_cell(t, r, "CLASS", &n.class)?;
        write_cell(t, r, "GROUP1", &n.lead_schedule)?;
        write_cell(t, r, "GROUP1CR", &n.lead_schedule_cr)?;
        n.props.write_to(t, r)?;
    }
    Ok(())
}

/// `AM` slot columns used by the account side, for gate cross-checks.
pub fn am_assignment_columns() -> Vec<&'static str> {
    ASSIGNABLE_DIGITS
        .iter()
        .filter_map(|&d| assignment_column(d))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_67_columns_and_record_1332() {
        assert_eq!(MP_SCHEMA.len(), MP_FIELD_COUNT);
        let mut seen = std::collections::BTreeSet::new();
        for f in MP_SCHEMA.iter() {
            assert!(seen.insert(f.name), "duplicate column {}", f.name);
        }
        assert_eq!(
            MP_SCHEMA.iter().map(|f| f.len as usize).sum::<usize>() + 1,
            MP_RECORD_LEN
        );
        assert!(MP_SCHEMA.iter().all(|f| matches!(f.kind, b'C' | b'N' | b'M' | b'L')));
    }

    #[test]
    fn schemes_and_kinds_parse() {
        assert_eq!(Scheme::parse("8").unwrap(), Scheme::Digit(8));
        assert_eq!(Scheme::parse("m").unwrap(), Scheme::Mapping);
        assert_eq!(Scheme::parse(" w ").unwrap(), Scheme::Other(b'W'));
        assert_eq!(Scheme::parse("").unwrap(), Scheme::Unassigned);
        assert!(Scheme::parse("MM").is_err());
        assert_eq!(Scheme::Digit(2).am_column(), Some("GROUP2"));
        assert_eq!(Scheme::Digit(9).am_column(), Some("GROUP9"));
        assert_eq!(Scheme::Digit(0).am_column(), Some("GROUP0"));
        assert_eq!(Scheme::Digit(1).am_column(), None);

        assert_eq!(NodeKind::parse("Y", ""), NodeKind::Title);
        assert_eq!(NodeKind::parse("N", ""), NodeKind::Assignable);
        assert_eq!(NodeKind::parse("", "C"), NodeKind::Calculated);
        assert_eq!(NodeKind::parse("", "U"), NodeKind::Unassignable);
        // TITLE wins over MAPTYPE (hull 173.100 is TITLE='F' MAPTYPE='C')
        assert_eq!(NodeKind::parse("F", "C"), NodeKind::Filtered);
        assert!(!NodeKind::parse("Y", "").is_assignable());
        assert!(NodeKind::parse("N", "").is_assignable());
    }
}

