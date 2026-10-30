//! cw-app adjusting entries — the `GL` row store, entry taxonomy and the
//! line/balance model (P5.E3a).
//!
//! Contract: `docs\specs\entries.md` §2–§5 and `docs\specs\journals.md` §2;
//! evidence F34 (AJE write path), F44 (journal rows), F45 (legacy ids).
//!
//! Three layers, mirroring [`crate::accounts`]:
//!
//! 1. [`GL_SCHEMA`] — the 51-column physical `GL` layout (668-byte records),
//!    frozen from `inventory\dbf-schema.tsv` and verified against the three
//!    golden corpora.
//! 2. [`GlRow`] — one physical row as typed cells. `parse_record`/`encode`
//!    are a standalone codec (`encode(parse(bytes)) == bytes`), and
//!    `from_table`/`write_to` bridge the kernel [`Table`] byte store.
//! 3. [`AdjustmentEntry`] / [`Adjustments`] — an entry is **1 header row**
//!    (`AC_NO` = 40 × `Z`, no amount) plus one row per line, keyed by
//!    `REFNO`; the header carries the class (`STATUS` → [`EntryType`]),
//!    user reference (`FROM_REF`), description, date and period.
//!
//! Write law: the module never invents column formatting. Typed writes go
//! through [`GlRow::set`] (width + dBase-type validation, CW padding) and
//! [`GlRow::write_to`], so decoding and re-encoding every golden row is
//! byte-identical. Memo (`M`) cells are carried as raw FPT pointers, never
//! edited (the FPT writer owns payloads).
//!
//! Out of scope (documented in the specs): BL propagation after
//! Recalc/Repost (cube lane), the booked-in-GL flag (F45: storage unlocated,
//! UI lane) and `SaveChanges` (.csl change files).

use std::collections::HashMap;
use std::fmt;

use cw_core::canon::latin1;
use cw_core::dbf::Table;
use cw_core::{CoreError, Result};

/// Physical `GL` field count (all three corpus schemas).
pub const GL_FIELD_COUNT: usize = 51;
/// Physical `GL` record length, deletion flag included.
pub const GL_RECORD_LEN: usize = 668;
/// The synthetic header account: 40 × `Z` (F34).
pub const GL_HEADER_ACCOUNT: &str = "ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ";
/// Adjusting-journal source on every AJE row (F34).
pub const AJE_JOURNAL_SOURCE: &str = "AJ";
/// AJE balance bucket (F34/F44; journal rows have none).
pub const AJE_BUCKET: &str = "Y";
/// Tolerance for cent-level balance comparisons.
pub const BALANCE_EPSILON: f64 = 0.005;

/// One field descriptor of the canonical `GL` layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlField {
    pub name: &'static str,
    /// dBase type letter (`C`/`N`/`D`/`L`/`M` in the corpus).
    pub kind: u8,
    pub len: u8,
    pub dec: u8,
}

const fn f(name: &'static str, kind: u8, len: u8, dec: u8) -> GlField {
    GlField { name, kind, len, dec }
}

/// Canonical `GL` schema in physical order (51 columns, 668-byte records).
pub static GL_SCHEMA: [GlField; GL_FIELD_COUNT] = [
    f("AC_NO", b'C', 40, 0),
    f("ENTITY", b'C', 40, 0),
    f("DIV", b'C', 6, 0),
    f("SUB", b'C', 6, 0),
    f("JE_DESC", b'C', 35, 0),
    f("DESC_NOTE", b'M', 10, 0),
    f("REFNO", b'C', 20, 0),
    f("B_ACT", b'C', 40, 0),
    f("B_ENT", b'C', 40, 0),
    f("B_DIV", b'C', 6, 0),
    f("B_SUB", b'C', 6, 0),
    f("OPT", b'C', 80, 0),
    f("DTYPE", b'C', 1, 0),
    f("RF", b'C', 3, 0),
    f("JOURSOURCE", b'C', 2, 0),
    f("STATUS", b'C', 1, 0),
    f("DATE", b'D', 8, 0),
    f("NET", b'N', 19, 2),
    f("TAX", b'N', 19, 2),
    f("CUR_MTH", b'N', 19, 2),
    f("REC", b'C', 1, 0),
    f("FROM_REF", b'C', 40, 0),
    f("REV_CODE", b'C', 2, 0),
    f("MEMO_NO", b'C', 2, 0),
    f("ACT_TYPE", b'C', 1, 0),
    f("TAX_ID", b'C', 4, 0),
    f("PER", b'C', 2, 0),
    f("KEY", b'C', 10, 0),
    f("RECURRING", b'C', 1, 0),
    f("ERROR", b'C', 2, 0),
    f("ACCOUNTS", b'C', 1, 0),
    f("RECURKEY", b'C', 10, 0),
    f("BUCKET", b'C', 1, 0),
    f("CWGUID", b'C', 22, 0),
    f("SPLIT", b'C', 1, 0),
    f("EXINFO", b'M', 10, 0),
    f("LINKGUID", b'C', 22, 0),
    f("LINK2GUID", b'C', 22, 0),
    f("ITEMGUID", b'C', 22, 0),
    f("PAY_TAX", b'C', 22, 0),
    f("OVERRIDE", b'L', 1, 0),
    f("CWDATE", b'C', 14, 0),
    f("CREATEDON", b'C', 14, 0),
    f("USER", b'C', 22, 0),
    f("ADJ_PERM", b'C', 1, 0),
    f("MASTER", b'C', 1, 0),
    f("CUSTOM", b'C', 2, 0),
    f("APPLY", b'C', 1, 0),
    f("DYNAMIC", b'C', 1, 0),
    f("FORMULA", b'M', 10, 0),
    f("MTYPE", b'C', 1, 0),
];

/// Index of `name` in [`GL_SCHEMA`].
pub fn gl_schema_index(name: &str) -> Option<usize> {
    GL_SCHEMA
        .iter()
        .position(|f| f.name.eq_ignore_ascii_case(name))
}

/// True when `t` carries the canonical 51-column `GL` layout.
pub fn gl_schema_matches(t: &Table) -> bool {
    t.fields.len() == GL_FIELD_COUNT
        && t.record_len as usize == GL_RECORD_LEN
        && t.fields.iter().zip(GL_SCHEMA.iter()).all(|(got, want)| {
            got.name.eq_ignore_ascii_case(want.name)
                && got.kind == want.kind
                && got.len == want.len
                && got.dec == want.dec
        })
}

/// Structural schema drift (name/type/length/decimals), for gate reporting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlSchemaDrift {
    pub column: String,
    pub field: String,
    pub expected: String,
    pub actual: String,
}

/// Compare `t` against [`GL_SCHEMA`]; empty when canonical.
pub fn gl_schema_drift(t: &Table) -> Vec<GlSchemaDrift> {
    let mut out = Vec::new();
    for (i, want) in GL_SCHEMA.iter().enumerate() {
        match t.fields.get(i) {
            None => out.push(GlSchemaDrift {
                column: want.name.to_string(),
                field: "missing".into(),
                expected: format!("{} {}{}.{}", want.name, want.kind as char, want.len, want.dec),
                actual: String::new(),
            }),
            Some(got) => {
                if !got.name.eq_ignore_ascii_case(want.name) {
                    out.push(GlSchemaDrift {
                        column: want.name.into(),
                        field: "name".into(),
                        expected: want.name.into(),
                        actual: got.name.clone(),
                    });
                }
                if got.kind != want.kind || got.len != want.len || got.dec != want.dec {
                    out.push(GlSchemaDrift {
                        column: want.name.into(),
                        field: "descriptor".into(),
                        expected: format!("{}{}.{}", want.kind as char, want.len, want.dec),
                        actual: format!("{}{}.{}", got.kind as char, got.len, got.dec),
                    });
                }
            }
        }
    }
    for (i, got) in t.fields.iter().enumerate().skip(GL_FIELD_COUNT) {
        out.push(GlSchemaDrift {
            column: got.name.clone(),
            field: format!("extra[{i}]"),
            expected: String::new(),
            actual: format!("{}{}.{}", got.kind as char, got.len, got.dec),
        });
    }
    out
}

/// `CWAdjustingEntryType` — the 13 entry classes, mapped to `GL.STATUS`
/// empirically (F34). `PriorPeriod` has no storage code: the COM put is
/// rejected without prior-year context (F34 note 7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntryType {
    /// `aetNormal` — `STATUS='N'`.
    Normal,
    /// `aetReclassifying` — `STATUS='R'`.
    Reclassifying,
    /// `aetUnrecordedProposed` — `STATUS='U'`.
    UnrecordedProposed,
    /// `aetEliminating` — `STATUS='E'`.
    Eliminating,
    /// `aetTaxFederal` — `STATUS='T'`.
    TaxFederal,
    /// `aetTaxState` — `STATUS='V'`.
    TaxState,
    /// `aetTaxCity` — `STATUS='C'`.
    TaxCity,
    /// `aetPriorPeriod` — no storage code observed.
    PriorPeriod,
    /// `aetOtherBasis` — `STATUS='B'`.
    OtherBasis,
    /// `aetUnrecordedProjected` — `STATUS='L'`.
    UnrecordedProjected,
    /// `aetUnrecordedJudgmental` — `STATUS='G'`.
    UnrecordedJudgmental,
    /// `aetOther2` — `STATUS='2'`.
    Other2,
    /// `aetOther3` — `STATUS='3'`.
    Other3,
}

impl EntryType {
    /// All 13 classes in COM enum order.
    pub const ALL: [EntryType; 13] = [
        EntryType::Normal,
        EntryType::Reclassifying,
        EntryType::UnrecordedProposed,
        EntryType::Eliminating,
        EntryType::TaxFederal,
        EntryType::TaxState,
        EntryType::TaxCity,
        EntryType::PriorPeriod,
        EntryType::OtherBasis,
        EntryType::UnrecordedProjected,
        EntryType::UnrecordedJudgmental,
        EntryType::Other2,
        EntryType::Other3,
    ];

    /// COM enum value 0..=12.
    pub fn code(self) -> u8 {
        match self {
            EntryType::Normal => 0,
            EntryType::Reclassifying => 1,
            EntryType::UnrecordedProposed => 2,
            EntryType::Eliminating => 3,
            EntryType::TaxFederal => 4,
            EntryType::TaxState => 5,
            EntryType::TaxCity => 6,
            EntryType::PriorPeriod => 7,
            EntryType::OtherBasis => 8,
            EntryType::UnrecordedProjected => 9,
            EntryType::UnrecordedJudgmental => 10,
            EntryType::Other2 => 11,
            EntryType::Other3 => 12,
        }
    }

    /// Inverse of [`EntryType::code`]; `None` outside 0..=12.
    pub fn from_code(code: u8) -> Option<EntryType> {
        EntryType::ALL.iter().copied().find(|t| t.code() == code)
    }

    /// `GL.STATUS` letter, or `None` for the unstoreable prior-period class.
    pub fn status(self) -> Option<&'static str> {
        match self {
            EntryType::Normal => Some("N"),
            EntryType::Reclassifying => Some("R"),
            EntryType::UnrecordedProposed => Some("U"),
            EntryType::Eliminating => Some("E"),
            EntryType::TaxFederal => Some("T"),
            EntryType::TaxState => Some("V"),
            EntryType::TaxCity => Some("C"),
            EntryType::PriorPeriod => None,
            EntryType::OtherBasis => Some("B"),
            EntryType::UnrecordedProjected => Some("L"),
            EntryType::UnrecordedJudgmental => Some("G"),
            EntryType::Other2 => Some("2"),
            EntryType::Other3 => Some("3"),
        }
    }

    /// Strict `GL.STATUS` decode. Blank/unknown is a data error.
    pub fn from_status(raw: &str) -> Result<EntryType> {
        let s = raw.trim();
        EntryType::ALL
            .iter()
            .copied()
            .find(|t| t.status() == Some(s))
            .ok_or_else(|| {
                CoreError::Format(format!(
                    "unknown adjusting-entry STATUS {s:?} (expected N/R/U/E/T/V/C/B/L/G/2/3)"
                ))
            })
    }

    /// Stored code used by the gate: `STATUS` for all but prior-period.
    pub fn storage(self) -> Option<&'static str> {
        self.status()
    }

    /// COM enum member name (`aet…`).
    pub fn label(self) -> &'static str {
        match self {
            EntryType::Normal => "aetNormal",
            EntryType::Reclassifying => "aetReclassifying",
            EntryType::UnrecordedProposed => "aetUnrecordedProposed",
            EntryType::Eliminating => "aetEliminating",
            EntryType::TaxFederal => "aetTaxFederal",
            EntryType::TaxState => "aetTaxState",
            EntryType::TaxCity => "aetTaxCity",
            EntryType::PriorPeriod => "aetPriorPeriod",
            EntryType::OtherBasis => "aetOtherBasis",
            EntryType::UnrecordedProjected => "aetUnrecordedProjected",
            EntryType::UnrecordedJudgmental => "aetUnrecordedJudgmental",
            EntryType::Other2 => "aetOther2",
            EntryType::Other3 => "aetOther3",
        }
    }

    /// User-facing class name (Entries dialog).
    pub fn display_name(self) -> &'static str {
        match self {
            EntryType::Normal => "Adjusting entry",
            EntryType::Reclassifying => "Reclassifying entry",
            EntryType::UnrecordedProposed => "Unrecorded proposed entry",
            EntryType::Eliminating => "Eliminating entry",
            EntryType::TaxFederal => "Tax - federal",
            EntryType::TaxState => "Tax - state",
            EntryType::TaxCity => "Tax - city",
            EntryType::PriorPeriod => "Prior period",
            EntryType::OtherBasis => "Other basis",
            EntryType::UnrecordedProjected => "Unrecorded projected entry",
            EntryType::UnrecordedJudgmental => "Unrecorded judgmental entry",
            EntryType::Other2 => "Other 2",
            EntryType::Other3 => "Other 3",
        }
    }

    pub fn is_tax(self) -> bool {
        matches!(
            self,
            EntryType::TaxFederal | EntryType::TaxState | EntryType::TaxCity
        )
    }

    pub fn is_unrecorded(self) -> bool {
        matches!(
            self,
            EntryType::UnrecordedProposed
                | EntryType::UnrecordedProjected
                | EntryType::UnrecordedJudgmental
        )
    }

    pub fn has_storage_code(self) -> bool {
        self.status().is_some()
    }
}

impl fmt::Display for EntryType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

/// One physical `GL` record as validated, padding-normalized cells.
///
/// `cells` are in [`GL_SCHEMA`] order. Character/date/logical cells are
/// left-justified on encode; numeric cells right-justified (CW/dBase style).
/// Memo cells keep the FPT pointer text and are never edited here.
#[derive(Clone, Debug, PartialEq)]
pub struct GlRow {
    /// Record deletion flag (`0x20` live, `0x2A` deleted).
    pub flag: u8,
    pub cells: Vec<String>,
}

impl GlRow {
    /// An all-blank row (space flag, empty cells) ready for [`GlRow::set`].
    pub fn blank() -> GlRow {
        GlRow {
            flag: b' ',
            cells: vec![String::new(); GL_FIELD_COUNT],
        }
    }

    /// Decode one 668-byte physical record (flag included).
    pub fn parse_record(bytes: &[u8]) -> Result<GlRow> {
        if bytes.len() != GL_RECORD_LEN {
            return Err(CoreError::Format(format!(
                "GL record is {} bytes, expected {GL_RECORD_LEN}",
                bytes.len()
            )));
        }
        let mut cells = Vec::with_capacity(GL_FIELD_COUNT);
        let mut off = 1usize;
        for field in GL_SCHEMA.iter() {
            let width = field.len as usize;
            let raw = &bytes[off..off + width];
            off += width;
            let mut s = latin1(raw);
            match field.kind {
                b'N' | b'D' => s = s.trim().to_string(),
                _ => s = s.trim_end_matches(' ').to_string(),
            }
            cells.push(s);
        }
        Ok(GlRow {
            flag: bytes[0],
            cells,
        })
    }

    /// Encode the row back to 668 bytes.
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.cells.len() != GL_FIELD_COUNT {
            return Err(CoreError::Format(format!(
                "GL row has {} cells, expected {GL_FIELD_COUNT}",
                self.cells.len()
            )));
        }
        let mut out = Vec::with_capacity(GL_RECORD_LEN);
        out.push(self.flag);
        for (field, cell) in GL_SCHEMA.iter().zip(self.cells.iter()) {
            let width = field.len as usize;
            let raw: Vec<u8> = cell
                .chars()
                .map(|c| {
                    let n = c as u32;
                    if n <= 0xFF {
                        Ok(n as u8)
                    } else {
                        Err(CoreError::Format(format!(
                            "{} contains non-Latin-1 char {c:?}",
                            field.name
                        )))
                    }
                })
                .collect::<Result<Vec<u8>>>()?;
            if raw.len() > width {
                return Err(CoreError::Format(format!(
                    "{} cell {:?} is {} bytes, field width {width}",
                    field.name,
                    cell,
                    raw.len()
                )));
            }
            let pad = width - raw.len();
            if field.kind == b'N' {
                out.extend(std::iter::repeat(b' ').take(pad));
                out.extend_from_slice(&raw);
            } else {
                out.extend_from_slice(&raw);
                out.extend(std::iter::repeat(b' ').take(pad));
            }
        }
        Ok(out)
    }

    /// Decode row `r` of a canonical `GL` table.
    pub fn from_table(t: &Table, r: usize) -> Result<GlRow> {
        if !gl_schema_matches(t) {
            return Err(CoreError::Format(format!(
                "GL schema drift: {:?}",
                gl_schema_drift(t)
            )));
        }
        let rec = t
            .records
            .get(r)
            .ok_or_else(|| CoreError::Format(format!("row {r} out of range")))?;
        GlRow::parse_record(rec)
    }

    /// Write back into an existing row (record bytes are replaced atomically).
    pub fn write_to(&self, t: &mut Table, r: usize) -> Result<()> {
        if !gl_schema_matches(t) {
            return Err(CoreError::Format(format!(
                "GL schema drift: {:?}",
                gl_schema_drift(t)
            )));
        }
        let rec = t
            .records
            .get_mut(r)
            .ok_or_else(|| CoreError::Format(format!("row {r} out of range")))?;
        let bytes = self.encode()?;
        rec.copy_from_slice(&bytes);
        Ok(())
    }

    /// Raw cell text, trimmed at both ends (leading spaces are CW's
    /// right-justification padding; the byte codec keeps them in `cells`).
    pub fn get(&self, column: &str) -> Result<&str> {
        let i = gl_schema_index(column)
            .ok_or_else(|| CoreError::Format(format!("GL has no column {column}")))?;
        Ok(self.cells[i].trim())
    }

    /// Validated write of one cell (width + dBase type, CW padding policy).
    pub fn set(&mut self, column: &str, value: &str) -> Result<()> {
        let i = gl_schema_index(column)
            .ok_or_else(|| CoreError::Format(format!("GL has no column {column}")))?;
        let field = GL_SCHEMA[i];
        let width = field.len as usize;
        if value.chars().any(|c| c as u32 > 0xFF) {
            return Err(CoreError::Format(format!(
                "{column} value contains a non-Latin-1 char: {value:?}"
            )));
        }
        if value.chars().count() > width {
            return Err(CoreError::Format(format!(
                "{column} value {value:?} is {} chars, field width {width}",
                value.chars().count()
            )));
        }
        let normalized = match field.kind {
            b'C' | b'L' => value.trim_end_matches(' ').to_string(),
            b'N' => {
                let s = value.trim();
                if !s.is_empty() {
                    s.parse::<f64>().map_err(|_| {
                        CoreError::Format(format!("{column}={s:?} is not numeric"))
                    })?;
                }
                s.to_string()
            }
            b'D' => {
                let s = value.trim();
                if !s.is_empty() && !is_date_token(s) {
                    return Err(CoreError::Format(format!(
                        "{column}={s:?} is not a YYYYMMDD date"
                    )));
                }
                s.to_string()
            }
            b'M' => {
                return Err(CoreError::Format(format!(
                    "{column} is a memo (FPT) cell; use the FPT writer"
                )))
            }
            other => {
                return Err(CoreError::Format(format!(
                    "unsupported dBase type {}",
                    other as char
                )))
            }
        };
        self.cells[i] = normalized;
        Ok(())
    }

    pub fn is_deleted(&self) -> bool {
        self.flag == b'*'
    }

    /// Header row test: account is the 40 × `Z` sentinel.
    pub fn is_header(&self) -> bool {
        self.get("AC_NO")
            .map(|v| v.trim_end_matches(' ') == GL_HEADER_ACCOUNT)
            .unwrap_or(false)
    }

    /// `STATUS` decoded to the entry taxonomy.
    pub fn entry_type(&self) -> Result<EntryType> {
        EntryType::from_status(self.get("STATUS")?)
    }

    /// `NET` as a number (blank = 0.0).
    pub fn net(&self) -> Result<f64> {
        parse_amount(self.get("NET")?)
    }

    /// `PER` as an optional period index (blank = None).
    pub fn period(&self) -> Result<Option<u8>> {
        parse_period(self.get("PER")?)
    }

    /// Account number as stored (right-trimmed; may be blank).
    pub fn account(&self) -> &str {
        self.cells[gl_schema_index("AC_NO").expect("AC_NO in GL_SCHEMA")]
            .trim_end_matches(' ')
    }
}

fn is_date_token(s: &str) -> bool {
    if s.len() != 8 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let month: u32 = s[4..6].parse().unwrap_or(0);
    let day: u32 = s[6..8].parse().unwrap_or(0);
    (1..=12).contains(&month) && (1..=31).contains(&day)
}

fn parse_amount(raw: &str) -> Result<f64> {
    let s = raw.trim();
    if s.is_empty() {
        return Ok(0.0);
    }
    s.parse::<f64>()
        .map_err(|_| CoreError::Format(format!("NET={s:?} is not numeric")))
}

fn parse_period(raw: &str) -> Result<Option<u8>> {
    let s = raw.trim();
    if s.is_empty() {
        return Ok(None);
    }
    s.parse::<u8>()
        .map(Some)
        .map_err(|_| CoreError::Format(format!("PER={s:?} is not a period index")))
}

/// One decoded entry line (`AdjustmentEntryItem` → one `GL` row).
#[derive(Clone, Debug, PartialEq)]
pub struct AdjustmentLine {
    /// Physical row in the source table.
    pub row: usize,
    pub account: String,
    pub amount: f64,
    /// `PER` (line rows); `None` when blank.
    pub period: Option<u8>,
    pub entity: String,
    pub div: String,
    pub sub: String,
    pub tax: f64,
    pub bucket: String,
}

impl AdjustmentLine {
    pub fn new(account: impl Into<String>, amount: f64) -> AdjustmentLine {
        AdjustmentLine {
            row: 0,
            account: account.into(),
            amount,
            period: None,
            entity: String::new(),
            div: String::new(),
            sub: String::new(),
            tax: 0.0,
            bucket: AJE_BUCKET.to_string(),
        }
    }

    /// Decode a line from its `GL` row.
    pub fn from_row(row: usize, gl: &GlRow) -> Result<AdjustmentLine> {
        Ok(AdjustmentLine {
            row,
            account: gl.account().to_string(),
            amount: gl.net()?,
            period: gl.period()?,
            entity: gl.get("ENTITY")?.trim_end().to_string(),
            div: gl.get("DIV")?.trim_end().to_string(),
            sub: gl.get("SUB")?.trim_end().to_string(),
            tax: parse_amount(gl.get("TAX")?)?,
            bucket: gl.get("BUCKET")?.trim_end().to_string(),
        })
    }
}

/// Debits/credits for one entry or a whole journal.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Balance {
    pub debits: f64,
    pub credits: f64,
}

impl Balance {
    pub fn net(self) -> f64 {
        self.debits - self.credits
    }

    /// True when the two sides agree within [`BALANCE_EPSILON`].
    pub fn is_balanced(self) -> bool {
        self.net().abs() < BALANCE_EPSILON
    }

    pub fn add_amount(&mut self, amount: f64) {
        if amount >= 0.0 {
            self.debits += amount;
        } else {
            self.credits += -amount;
        }
    }
}

/// One adjusting entry: header + lines.
#[derive(Clone, Debug, PartialEq)]
pub struct AdjustmentEntry {
    /// `REFNO` (system Id: `AJE #n`, a legacy id, or a journal ref).
    pub system_id: String,
    /// `FROM_REF` on the header (user reference).
    pub user_reference: String,
    /// `JE_DESC` on the header.
    pub description: String,
    pub entry_type: EntryType,
    /// `DATE` on the header (`YYYYMMDD`).
    pub date: String,
    /// `PER` on the header, when present.
    pub period: Option<u8>,
    /// `JOURSOURCE` (`AJ` for the adjusting family).
    pub journal_source: String,
    /// Physical header row; `None` when the group had no `Z…Z` row.
    pub header_row: Option<usize>,
    pub lines: Vec<AdjustmentLine>,
    /// The exact decoded rows (header first) for byte-stable re-emission.
    pub gl_rows: Vec<GlRow>,
}

impl AdjustmentEntry {
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn balance(&self) -> Balance {
        let mut b = Balance::default();
        for line in &self.lines {
            b.add_amount(line.amount);
        }
        b
    }

    /// Σ line `NET` (F34 T1: zero for a balanced entry).
    pub fn difference(&self) -> f64 {
        self.lines.iter().map(|l| l.amount).sum()
    }

    pub fn is_balanced(&self) -> bool {
        self.difference().abs() < BALANCE_EPSILON
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Physical row count: header + lines (F34 T5).
    pub fn row_count(&self) -> usize {
        self.gl_rows.len()
    }

    pub fn accounts(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(|l| l.account.as_str())
    }

    /// Entry date must fall inside the engagement fiscal year (F34 T3).
    pub fn date_in_range(&self, start: &str, end: &str) -> Result<bool> {
        if !is_date_token(&self.date) {
            return Err(CoreError::Format(format!(
                "entry {} date {:?} is not YYYYMMDD",
                self.system_id, self.date
            )));
        }
        if !is_date_token(start) || !is_date_token(end) {
            return Err(CoreError::Format(format!(
                "fiscal-year bounds {start:?}..{end:?} are not YYYYMMDD"
            )));
        }
        Ok(self.date.as_str() >= start && self.date.as_str() <= end)
    }

    /// Re-encode the entry's original rows (F34 write-back path).
    pub fn encode_rows(&self) -> Result<Vec<Vec<u8>>> {
        self.gl_rows.iter().map(GlRow::encode).collect()
    }

    /// Build the canonical 1 + n rows for a new AJE (F34 row law).
    ///
    /// All common columns are stamped: `JOURSOURCE='AJ'`, `BUCKET='Y'`,
    /// `DTYPE='G'`, `CUR_MTH = NET`, `B_ACT = account`. `NET` is formatted
    /// with two decimals (CW numeric convention).
    pub fn render(
        system_id: &str,
        user_reference: &str,
        description: &str,
        entry_type: EntryType,
        date: &str,
        period: Option<u8>,
        lines: &[AdjustmentLine],
    ) -> Result<AdjustmentEntry> {
        let status = entry_type.status().ok_or_else(|| {
            CoreError::Format(format!(
                "{} has no GL.STATUS code; prior-period entries need prior-year context",
                entry_type.label()
            ))
        })?;

        let mut rows = Vec::with_capacity(lines.len() + 1);
        let mut header = GlRow::blank();
        header.set("AC_NO", GL_HEADER_ACCOUNT)?;
        header.set("B_ACT", GL_HEADER_ACCOUNT)?;
        header.set("REFNO", system_id)?;
        header.set("FROM_REF", user_reference)?;
        header.set("JE_DESC", description)?;
        header.set("JOURSOURCE", AJE_JOURNAL_SOURCE)?;
        header.set("STATUS", status)?;
        header.set("DTYPE", "G")?;
        header.set("BUCKET", AJE_BUCKET)?;
        header.set("DATE", date)?;
        if let Some(p) = period {
            header.set("PER", &p.to_string())?;
        }
        rows.push(header);

        for line in lines {
            if line.account.trim().is_empty() {
                return Err(CoreError::Format("adjustment line has no account".into()));
            }
            let mut row = GlRow::blank();
            row.set("AC_NO", &line.account)?;
            row.set("B_ACT", &line.account)?;
            row.set("REFNO", system_id)?;
            row.set("FROM_REF", user_reference)?;
            row.set("JE_DESC", description)?;
            row.set("JOURSOURCE", AJE_JOURNAL_SOURCE)?;
            row.set("STATUS", status)?;
            row.set("DTYPE", "G")?;
            row.set("BUCKET", AJE_BUCKET)?;
            row.set("DATE", date)?;
            let amount = format!("{:.2}", line.amount);
            row.set("NET", &amount)?;
            row.set("CUR_MTH", &amount)?;
            let per = line.period.or(period);
            if let Some(p) = per {
                row.set("PER", &p.to_string())?;
            }
            rows.push(row);
        }

        let balance = lines.iter().fold(Balance::default(), |mut b, l| {
            b.add_amount(l.amount);
            b
        });
        if !balance.is_balanced() {
            return Err(CoreError::Format(format!(
                "entry {system_id:?} is out of balance by {:.2}",
                balance.net()
            )));
        }

        let header_row = 0usize;
        Ok(AdjustmentEntry {
            system_id: system_id.trim().to_string(),
            user_reference: user_reference.trim().to_string(),
            description: description.trim_end().to_string(),
            entry_type,
            date: date.to_string(),
            period,
            journal_source: AJE_JOURNAL_SOURCE.to_string(),
            header_row: Some(header_row),
            lines: lines
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    let mut l = l.clone();
                    l.row = i + 1;
                    l
                })
                .collect(),
            gl_rows: rows,
        })
    }
}

/// A data-level problem found while decoding a `GL` journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdjustmentIssue {
    pub system_id: String,
    pub kind: AdjustmentIssueKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdjustmentIssueKind {
    /// The group has no `Z…Z` header row.
    MissingHeader,
    /// More than one `Z…Z` row in the group.
    DuplicateHeader,
    /// Σ line `NET` is not zero.
    Unbalanced,
    /// Header only, no entry lines.
    EmptyEntry,
    /// `STATUS` is not one of the 13 storage codes.
    UnknownStatus,
    /// Rows in the group disagree on `STATUS`.
    MixedStatus,
    /// Rows in the group disagree on `JOURSOURCE`.
    MixedSource,
}

impl fmt::Display for AdjustmentIssueKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            AdjustmentIssueKind::MissingHeader => "missing header row",
            AdjustmentIssueKind::DuplicateHeader => "duplicate header row",
            AdjustmentIssueKind::Unbalanced => "entry does not balance",
            AdjustmentIssueKind::EmptyEntry => "entry has no lines",
            AdjustmentIssueKind::UnknownStatus => "unknown STATUS code",
            AdjustmentIssueKind::MixedStatus => "mixed STATUS codes",
            AdjustmentIssueKind::MixedSource => "mixed JOURSOURCE values",
        };
        f.write_str(s)
    }
}

impl fmt::Display for AdjustmentIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.system_id, self.kind)
    }
}

/// The decoded `GL` journal (adjusting-entry family).
#[derive(Clone, Debug, Default)]
pub struct Adjustments {
    entries: Vec<AdjustmentEntry>,
    issues: Vec<AdjustmentIssue>,
    /// Every live row in physical file order (entry rows are not always
    /// contiguous - hull/mwgh interleave entries), for byte-stable re-emission.
    source_rows: Vec<GlRow>,
}

impl Adjustments {
    /// Decode every live row, grouping by (`JOURSOURCE`, `REFNO`) in file order.
    ///
    /// Refuses a non-canonical physical schema. Structural data problems are
    /// collected in [`Adjustments::issues`] so a gate can assert cleanliness
    /// without losing the rest of the journal.
    pub fn from_table(t: &Table) -> Result<Adjustments> {
        if !gl_schema_matches(t) {
            return Err(CoreError::Format(format!(
                "GL has {} fields / record {}; expected {GL_FIELD_COUNT} / {GL_RECORD_LEN}: {:?}",
                t.fields.len(),
                t.record_len,
                gl_schema_drift(t)
            )));
        }
        let mut order: Vec<(String, String)> = Vec::new();
        let mut groups: HashMap<(String, String), Vec<(usize, GlRow)>> = HashMap::new();
        let mut source_rows: Vec<GlRow> = Vec::new();
        for r in 0..t.count() {
            if t.is_deleted(r) {
                continue;
            }
            let row = GlRow::from_table(t, r)?;
            source_rows.push(row.clone());
            // F44 T5: JOURSOURCE partitions the store ('AJ' vs journal ids);
            // the same REFNO text may exist in two journals.
            let source = row.get("JOURSOURCE")?.trim().to_string();
            let key = row.get("REFNO")?.trim().to_string();
            let group_key = (source, key);
            if !groups.contains_key(&group_key) {
                order.push(group_key.clone());
            }
            groups.entry(group_key).or_default().push((r, row));
        }

        let mut entries = Vec::with_capacity(order.len());
        let mut issues = Vec::new();
        for (journal_source, id) in order {
            let rows = groups.remove(&(journal_source, id.clone())).unwrap_or_default();
            let header_positions: Vec<usize> = rows
                .iter()
                .enumerate()
                .filter(|(_, (_, row))| row.is_header())
                .map(|(i, _)| i)
                .collect();
            if header_positions.is_empty() {
                issues.push(AdjustmentIssue {
                    system_id: id.clone(),
                    kind: AdjustmentIssueKind::MissingHeader,
                });
            } else if header_positions.len() > 1 {
                issues.push(AdjustmentIssue {
                    system_id: id.clone(),
                    kind: AdjustmentIssueKind::DuplicateHeader,
                });
            }
            let reference_idx = header_positions.first().copied().unwrap_or(0);
            let header = &rows[reference_idx].1;

            let entry_type = match header.entry_type() {
                Ok(t) => t,
                Err(_) => {
                    issues.push(AdjustmentIssue {
                        system_id: id.clone(),
                        kind: AdjustmentIssueKind::UnknownStatus,
                    });
                    EntryType::Normal
                }
            };
            let statuses: Vec<&str> = rows
                .iter()
                .map(|(_, row)| row.get("STATUS").unwrap_or("").trim())
                .collect();
            if statuses.iter().any(|s| *s != statuses[0]) {
                issues.push(AdjustmentIssue {
                    system_id: id.clone(),
                    kind: AdjustmentIssueKind::MixedStatus,
                });
            }
            let sources: Vec<&str> = rows
                .iter()
                .map(|(_, row)| row.get("JOURSOURCE").unwrap_or("").trim())
                .collect();
            if sources.iter().any(|s| *s != sources[0]) {
                issues.push(AdjustmentIssue {
                    system_id: id.clone(),
                    kind: AdjustmentIssueKind::MixedSource,
                });
            }

            let mut lines = Vec::new();
            for (row_no, row) in rows.iter() {
                if row.is_header() {
                    continue;
                }
                lines.push(AdjustmentLine::from_row(*row_no, row)?);
            }

            let entry = AdjustmentEntry {
                system_id: id.clone(),
                user_reference: header.get("FROM_REF")?.trim_end().to_string(),
                description: header.get("JE_DESC")?.trim_end().to_string(),
                entry_type,
                date: header.get("DATE")?.trim().to_string(),
                period: match header.period() {
                    Ok(p) => p,
                    Err(_) => None,
                },
                journal_source: header.get("JOURSOURCE")?.trim_end().to_string(),
                header_row: header_positions.first().map(|i| rows[*i].0),
                lines,
                gl_rows: rows.iter().map(|(_, row)| row.clone()).collect(),
            };

            if entry.lines.is_empty() {
                issues.push(AdjustmentIssue {
                    system_id: id.clone(),
                    kind: AdjustmentIssueKind::EmptyEntry,
                });
            }
            if !entry.is_balanced() {
                issues.push(AdjustmentIssue {
                    system_id: id.clone(),
                    kind: AdjustmentIssueKind::Unbalanced,
                });
            }
            entries.push(entry);
        }

        Ok(Adjustments {
            entries,
            issues,
            source_rows,
        })
    }

    pub fn entries(&self) -> &[AdjustmentEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn issues(&self) -> &[AdjustmentIssue] {
        &self.issues
    }

    /// Lookup by system Id (`REFNO`), e.g. `"AJE #01"` (first match).
    pub fn get(&self, system_id: &str) -> Option<&AdjustmentEntry> {
        self.entries
            .iter()
            .find(|e| e.system_id == system_id.trim())
    }

    /// Lookup within one journal partition (`JOURSOURCE` + `REFNO`).
    pub fn get_in(&self, journal_source: &str, system_id: &str) -> Option<&AdjustmentEntry> {
        self.entries.iter().find(|e| {
            e.system_id == system_id.trim() && e.journal_source == journal_source.trim()
        })
    }

    pub fn line_count(&self) -> usize {
        self.entries.iter().map(|e| e.lines.len()).sum()
    }

    /// Journal-wide debit/credit totals.
    pub fn balance(&self) -> Balance {
        let mut b = Balance::default();
        for entry in &self.entries {
            for line in &entry.lines {
                b.add_amount(line.amount);
            }
        }
        b
    }

    /// Entries that fail the Σ line = 0 invariant.
    pub fn unbalanced(&self) -> impl Iterator<Item = &AdjustmentEntry> {
        self.entries.iter().filter(|e| !e.is_balanced())
    }

    /// Count per class (stable enum order).
    pub fn type_counts(&self) -> Vec<(EntryType, usize)> {
        EntryType::ALL
            .iter()
            .map(|t| {
                (
                    *t,
                    self.entries.iter().filter(|e| e.entry_type == *t).count(),
                )
            })
            .filter(|(_, n)| *n > 0)
            .collect()
    }

    /// Exact row bytes of the whole journal, in physical file order.
    pub fn encode_rows(&self) -> Result<Vec<Vec<u8>>> {
        self.source_rows.iter().map(GlRow::encode).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_type_taxonomy_is_complete() {
        assert_eq!(EntryType::ALL.len(), 13);
        for t in EntryType::ALL {
            assert_eq!(EntryType::from_code(t.code()), Some(t));
            match t.status() {
                Some(s) => assert_eq!(EntryType::from_status(s).unwrap(), t),
                None => {
                    assert_eq!(t, EntryType::PriorPeriod);
                    assert!(!t.has_storage_code());
                }
            }
        }
        assert!(EntryType::from_status("7").is_err());
        assert!(EntryType::from_status("X").is_err());
        assert_eq!(EntryType::TaxState.status(), Some("V"));
    }

    #[test]
    fn blank_row_encodes_to_spaces() {
        let row = GlRow::blank();
        let bytes = row.encode().unwrap();
        assert_eq!(bytes.len(), GL_RECORD_LEN);
        assert_eq!(bytes[0], b' ');
        assert!(bytes[1..].iter().all(|&b| b == b' '));
        let back = GlRow::parse_record(&bytes).unwrap();
        assert_eq!(back, row);
    }
}

