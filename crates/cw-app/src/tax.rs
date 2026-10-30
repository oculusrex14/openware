//! cw-app tax export codes (`TP`) + GIFI assignment (P5.E5).
//!
//! Contract: `docs/specs/tax-export.md` (F53 COM map) and
//! `docs/specs/export.md` §3/§6 (GIFI row gate). Three layers:
//!
//! 1. [`TP_SCHEMA`] - the 18-column physical layout (609-byte header,
//!    174-byte records), frozen from `inventory/dbf-schema.tsv` and re-checked
//!    against the three golden `*TP.dbf` files.
//! 2. [`TaxExportCode`] / [`TaxExportTable`] - the code table itself. `TCODE`
//!    is the COM `Id`, `CWGUID` is `ItemIdentifier`; the COM `Get` accepts
//!    either. Rows are scoped by `(FORM, ENTITY, YEAR)`: a blank `YEAR` means
//!    "all years" (the observed user-defined rows), and user-defined rows are
//!    the only mutable ones (`UDEF` `U`/`X`; standard rows refuse writes with
//!    the COM message `Cannot modify non-user-defined`).
//! 3. Assignment - `AM.TAC_NO` / `MP.TAC_NO` plus the per-entity mapping slots
//!    `MP.TAX1..TAX4`, and `AM.GIFI` / `MP.GIFI`. Every write goes through
//!    [`crate::accounts::write_cell`] (width/type validation, atomic refusal),
//!    and every code reference is validated against the decoded table first.
//!
//! The active-set filter (`client.TaxExportCodes` = 741 rows on USI out of the
//! full `TP` store) is still an open RE question (spec §6.4), so lookup here is
//! over the full decoded table; [`TaxExportTable::for_scope`] exposes the
//! `(form, entity, year)` projection the export lane needs.

use std::collections::BTreeMap;

use cw_core::dbf::Table;
use cw_core::{CoreError, Result};

use crate::accounts::{write_cell, DriftKind, SchemaDrift};

/// Physical `TP` field count (all three corpus schemas).
pub const TP_FIELD_COUNT: usize = 18;
/// Physical `TP` record length, deletion flag included.
pub const TP_RECORD_LEN: usize = 174;

/// One field descriptor of the canonical `TP` layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TpField {
    pub name: &'static str,
    /// dBase type letter (`C`/`N` in the corpus; all 18 columns).
    pub kind: u8,
    pub len: u8,
    pub dec: u8,
}

const fn f(name: &'static str, kind: u8, len: u8, dec: u8) -> TpField {
    TpField { name, kind, len, dec }
}

/// Canonical `TP` schema in physical order (18 columns).
pub static TP_SCHEMA: [TpField; TP_FIELD_COUNT] = [
    f("TYPE", b'C', 1, 0),
    f("TCODE", b'C', 16, 0),
    f("DESCRIPT", b'C', 50, 0),
    f("SUB", b'C', 1, 0),
    f("CUCODE", b'C', 10, 0),
    f("PRCODE", b'C', 10, 0),
    f("ORDER", b'C', 1, 0),
    f("SIGN", b'C', 1, 0),
    f("FORM", b'C', 16, 0),
    f("ENTITY", b'C', 1, 0),
    f("DETAIL", b'C', 1, 0),
    f("YEAR", b'C', 2, 0),
    f("PER", b'N', 6, 2),
    f("UDEF", b'C', 1, 0),
    f("REF", b'C', 6, 0),
    f("CWGUID", b'C', 22, 0),
    f("CWDATE", b'C', 14, 0),
    f("CREATEDON", b'C', 14, 0),
];

/// Look up a canonical `TP` field by name.
pub fn tp_schema_field(name: &str) -> Option<&'static TpField> {
    let n = name.trim().to_ascii_uppercase();
    TP_SCHEMA.iter().find(|f| f.name == n)
}

/// Full physical-schema comparison against [`TP_SCHEMA`] (order, names, types,
/// widths, decimals). Shares [`SchemaDrift`] with the `AM`/`MP` models.
pub fn tp_schema_drift(t: &Table) -> Vec<SchemaDrift> {
    let mut out = Vec::new();
    let actual: Vec<String> = t
        .fields
        .iter()
        .map(|fd| fd.name.to_ascii_uppercase())
        .collect();

    for e in TP_SCHEMA.iter() {
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
        if !TP_SCHEMA.iter().any(|e| e.name == a) {
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
        let Some(e) = TP_SCHEMA.iter().find(|e| e.name == name) else {
            continue;
        };
        let expected_pos = TP_SCHEMA.iter().position(|x| x.name == name).unwrap();
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

/// True when the table is the canonical 18-column `TP` layout.
pub fn tp_schema_matches(t: &Table) -> bool {
    tp_schema_drift(t).is_empty()
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

/// `TP.UDEF` user-defined marker.
///
/// The corpus uses `U` for the bulk of user codes and `X` for the 27
/// `FORM='Unknown'` extension rows; both mean "not shipped", i.e. mutable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserDefinedKind {
    /// Blank `UDEF` - a shipped/standard code (immutable over COM).
    Standard,
    /// `U` - user-defined code.
    User,
    /// `X` - user extension row (`FORM='Unknown'` in the corpus).
    Extension,
    /// Any other letter; kept so decode never panics.
    Other(u8),
}

impl UserDefinedKind {
    pub fn parse(raw: &str) -> UserDefinedKind {
        match raw.trim().as_bytes() {
            [] => UserDefinedKind::Standard,
            [b'u' | b'U'] => UserDefinedKind::User,
            [b'x' | b'X'] => UserDefinedKind::Extension,
            [c] => UserDefinedKind::Other(*c),
            _ => UserDefinedKind::Other(0),
        }
    }

    /// Standard rows refuse mutation ("Cannot modify non-user-defined").
    pub fn is_user_defined(self) -> bool {
        matches!(self, UserDefinedKind::User | UserDefinedKind::Extension)
    }

    pub fn raw(self) -> &'static str {
        match self {
            UserDefinedKind::Standard => "",
            UserDefinedKind::User => "U",
            UserDefinedKind::Extension => "X",
            UserDefinedKind::Other(_) => "",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            UserDefinedKind::Standard => "standard",
            UserDefinedKind::User => "user",
            UserDefinedKind::Extension => "extension",
            UserDefinedKind::Other(_) => "unknown",
        }
    }
}

/// One decoded `TP` tax-export code row.
#[derive(Clone, Debug, PartialEq)]
pub struct TaxExportCode {
    pub row: usize,
    /// `TYPE` - `W` on every live corpus row.
    pub row_type: String,
    /// `TCODE` - the COM `Id` used by `AM.TAC_NO` / `MP.TAC_NO`.
    pub code: String,
    pub description: String,
    pub sub: String,
    pub cu_code: String,
    pub pr_code: String,
    pub order: String,
    /// `SIGN` - the COM `SwitchSign` flag (`''` or `P` in the corpus).
    pub sign: String,
    pub form: String,
    pub entity: String,
    pub detail: String,
    /// `YEAR` as stored: two digits, or blank for "all years".
    pub year: String,
    /// `PER` - COM `Percent`, stored numeric (6,2).
    pub percent: f64,
    pub user_defined: UserDefinedKind,
    pub ref_code: String,
    /// `CWGUID` - the COM `ItemIdentifier`.
    pub cwguid: String,
    pub cwdate: String,
    pub createdon: String,
}

impl TaxExportCode {
    /// Decode one physical `TP` row.
    pub fn from_table(t: &Table, r: usize) -> Result<TaxExportCode> {
        Ok(TaxExportCode {
            row: r,
            row_type: text(t, r, "TYPE")?,
            code: text(t, r, "TCODE")?,
            description: text(t, r, "DESCRIPT")?,
            sub: text(t, r, "SUB")?,
            cu_code: text(t, r, "CUCODE")?,
            pr_code: text(t, r, "PRCODE")?,
            order: text(t, r, "ORDER")?,
            sign: text(t, r, "SIGN")?,
            form: text(t, r, "FORM")?,
            entity: text(t, r, "ENTITY")?,
            detail: text(t, r, "DETAIL")?,
            year: text(t, r, "YEAR")?,
            percent: num(t, r, "PER")?,
            user_defined: UserDefinedKind::parse(&text(t, r, "UDEF")?),
            ref_code: text(t, r, "REF")?,
            cwguid: text(t, r, "CWGUID")?,
            cwdate: text(t, r, "CWDATE")?,
            createdon: text(t, r, "CREATEDON")?,
        })
    }

    pub fn is_user_defined(&self) -> bool {
        self.user_defined.is_user_defined()
    }

    /// True when this row is presented for `(form, entity, year)`.
    ///
    /// Blank scope fields are wildcards (the observed user rows carry a blank
    /// `YEAR`); comparison is case-insensitive but exact otherwise.
    pub fn applies_to(&self, form: &str, entity: &str, year: &str) -> bool {
        let eq = |stored: &str, wanted: &str| {
            stored.trim().is_empty() || stored.trim().eq_ignore_ascii_case(wanted.trim())
        };
        eq(&self.form, form) && eq(&self.entity, entity) && eq(&self.year, year)
    }

    /// Short display label (`"01A - 1120/C"`).
    pub fn label(&self) -> String {
        if self.form.trim().is_empty() {
            self.code.clone()
        } else {
            format!("{} - {}/{}", self.code, self.form, self.entity)
        }
    }
}

/// All live `TP` rows plus code/GUID indexes.
#[derive(Clone, Debug)]
pub struct TaxExportTable {
    rows: Vec<TaxExportCode>,
    by_code: BTreeMap<String, Vec<usize>>,
    by_guid: BTreeMap<String, usize>,
    deleted: usize,
}

impl TaxExportTable {
    /// Build the table. Refuses a non-canonical physical schema.
    pub fn from_table(t: &Table) -> Result<TaxExportTable> {
        if t.fields.len() != TP_FIELD_COUNT {
            return Err(CoreError::Format(format!(
                "TP has {} fields, expected {TP_FIELD_COUNT}",
                t.fields.len()
            )));
        }
        let drift = tp_schema_drift(t);
        if let Some(first) = drift.first() {
            return Err(CoreError::Format(format!(
                "TP schema drift at {} ({}): expected {:?}, actual {:?}",
                first.column, first.kind, first.expected, first.actual
            )));
        }
        let mut rows = Vec::new();
        let mut by_code: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut by_guid: BTreeMap<String, usize> = BTreeMap::new();
        let mut deleted = 0usize;
        for r in 0..t.records.len() {
            if t.is_deleted(r) {
                deleted += 1;
                continue;
            }
            let code = TaxExportCode::from_table(t, r)?;
            let idx = rows.len();
            if !code.code.is_empty() {
                by_code.entry(code.code.clone()).or_default().push(idx);
            }
            if !code.cwguid.is_empty() {
                by_guid.entry(code.cwguid.clone()).or_insert(idx);
            }
            rows.push(code);
        }
        Ok(TaxExportTable {
            rows,
            by_code,
            by_guid,
            deleted,
        })
    }

    /// Tombstoned rows skipped at decode.
    pub fn deleted(&self) -> usize {
        self.deleted
    }

    /// Every live row in physical order.
    pub fn rows(&self) -> &[TaxExportCode] {
        &self.rows
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Row by its physical `TP` row index (`TaxExportCode::row`).
    pub fn get_row(&self, row: usize) -> Option<&TaxExportCode> {
        self.rows.iter().find(|r| r.row == row)
    }

    /// COM `Get(key)`: match an `Id` (`TCODE`) first, then an
    /// `ItemIdentifier` (`CWGUID`). Duplicate `TCODE` rows are legal (one per
    /// form/entity/year); the first physical match is returned.
    pub fn get(&self, key: &str) -> Option<&TaxExportCode> {
        let k = key.trim();
        if let Some(idx) = self.by_code.get(k).and_then(|v| v.first()) {
            return self.rows.get(*idx);
        }
        self.by_guid.get(k).and_then(|&i| self.rows.get(i))
    }

    /// Every row carrying `code` (all form/entity/year scopes).
    pub fn all_by_code(&self, code: &str) -> Vec<&TaxExportCode> {
        self.by_code
            .get(code.trim())
            .map(|v| v.iter().filter_map(|&i| self.rows.get(i)).collect())
            .unwrap_or_default()
    }

    /// True when the key resolves as an `Id` or `ItemIdentifier`.
    pub fn contains(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Distinct `TCODE` values in sorted order.
    pub fn codes(&self) -> Vec<String> {
        self.by_code.keys().cloned().collect()
    }

    /// Distinct user-defined rows.
    pub fn user_defined(&self) -> Vec<&TaxExportCode> {
        self.rows.iter().filter(|r| r.is_user_defined()).collect()
    }

    /// Rows presented for one export scope (blank stored scope = wildcard).
    pub fn for_scope(&self, form: &str, entity: &str, year: &str) -> Vec<&TaxExportCode> {
        self.rows
            .iter()
            .filter(|r| r.applies_to(form, entity, year))
            .collect()
    }

    /// Resolve a code for one scope; `None` when absent.
    pub fn resolve(&self, code: &str, form: &str, entity: &str, year: &str) -> Option<&TaxExportCode> {
        self.all_by_code(code)
            .into_iter()
            .find(|r| r.applies_to(form, entity, year))
    }

    /// COM validation law: an unknown code is rejected with
    /// `Tax Export Code is not valid`.
    pub fn validate(&self, code: &str) -> Result<&TaxExportCode> {
        self.get(code).ok_or_else(|| {
            CoreError::Format(format!(
                "Tax Export Code {:?} is not valid (not present in TP)",
                code.trim()
            ))
        })
    }

    /// Validate a code against a concrete `(form, entity, year)` scope.
    pub fn validate_for_scope(
        &self,
        code: &str,
        form: &str,
        entity: &str,
        year: &str,
    ) -> Result<&TaxExportCode> {
        self.resolve(code, form, entity, year).ok_or_else(|| {
            CoreError::Format(format!(
                "Tax Export Code {:?} is not valid for {form}/{entity}/Y{year}",
                code.trim()
            ))
        })
    }

    /// A row is mutable only when `UDEF` marks it user-defined.
    pub fn row_is_mutable(&self, row: usize) -> bool {
        self.get_row(row).map(|r| r.is_user_defined()).unwrap_or(false)
    }
}

fn ensure_user_row(t: &Table, row: usize) -> Result<()> {
    let flag = UserDefinedKind::parse(&text(t, row, "UDEF")?);
    if !flag.is_user_defined() {
        return Err(CoreError::Format(format!(
            "Cannot modify non-user-defined tax export code at TP row {row}"
        )));
    }
    Ok(())
}

/// Update `TP.DESCRIPT` on a user-defined row (standard rows refuse).
pub fn set_code_description(t: &mut Table, row: usize, description: &str) -> Result<()> {
    ensure_user_row(t, row)?;
    write_cell(t, row, "DESCRIPT", description.trim())
}

/// Update `TP.PER` (COM `Percent`) on a user-defined row.
pub fn set_code_percent(t: &mut Table, row: usize, percent: f64) -> Result<()> {
    ensure_user_row(t, row)?;
    if !percent.is_finite() || percent < 0.0 {
        return Err(CoreError::Format(format!(
            "TP PER {percent} must be a non-negative finite number"
        )));
    }
    write_cell(t, row, "PER", &format!("{percent:.2}"))
}

/// Update `TP.SIGN` (COM `SwitchSign`) on a user-defined row. `true` writes
/// the observed `P` flag; `false` clears the cell.
pub fn set_code_switch_sign(t: &mut Table, row: usize, on: bool) -> Result<()> {
    ensure_user_row(t, row)?;
    write_cell(t, row, "SIGN", if on { "P" } else { "" })
}

/// Re-encode every decoded `TP` column through `write_cell`; the corpus must
/// come back byte-identical.
pub fn rewrite_codes(t: &mut Table) -> Result<()> {
    for r in 0..t.records.len() {
        let c = TaxExportCode::from_table(t, r)?;
        write_cell(t, r, "TYPE", &c.row_type)?;
        write_cell(t, r, "TCODE", &c.code)?;
        write_cell(t, r, "DESCRIPT", &c.description)?;
        write_cell(t, r, "SUB", &c.sub)?;
        write_cell(t, r, "CUCODE", &c.cu_code)?;
        write_cell(t, r, "PRCODE", &c.pr_code)?;
        write_cell(t, r, "ORDER", &c.order)?;
        write_cell(t, r, "SIGN", &c.sign)?;
        write_cell(t, r, "FORM", &c.form)?;
        write_cell(t, r, "ENTITY", &c.entity)?;
        write_cell(t, r, "DETAIL", &c.detail)?;
        write_cell(t, r, "YEAR", &c.year)?;
        write_cell(t, r, "PER", &text(t, r, "PER")?)?;
        write_cell(t, r, "UDEF", &text(t, r, "UDEF")?)?;
        write_cell(t, r, "REF", &c.ref_code)?;
        write_cell(t, r, "CWGUID", &c.cwguid)?;
        write_cell(t, r, "CWDATE", &c.cwdate)?;
        write_cell(t, r, "CREATEDON", &c.createdon)?;
    }
    Ok(())
}

/// `AM.TAC_NO` - the account's tax-export code.
pub fn account_tax_code(am: &Table, row: usize) -> Result<String> {
    text(am, row, "TAC_NO")
}

/// Assign `AM.TAC_NO` after validating the code against `TP` (`""` clears).
pub fn assign_account_tax_code(
    am: &mut Table,
    row: usize,
    tp: &TaxExportTable,
    code: &str,
) -> Result<()> {
    let c = code.trim();
    if !c.is_empty() {
        tp.validate(c)?;
    }
    write_cell(am, row, "TAC_NO", c)
}

/// Clear `AM.TAC_NO`.
pub fn clear_account_tax_code(am: &mut Table, row: usize) -> Result<()> {
    write_cell(am, row, "TAC_NO", "")
}

/// `MP` per-entity tax slots in COM probe order: slot 0 = `TAC_NO`,
/// slots 1..=3 = `TAX1..TAX3`. `TAX4` is present physically and exposed as
/// slot 4; COM only proved `0..=3` (spec §2/§6.1).
pub const MAPPING_TAX_COLUMNS: [&str; 5] = ["TAC_NO", "TAX1", "TAX2", "TAX3", "TAX4"];

pub fn mapping_tax_column(slot: usize) -> Option<&'static str> {
    MAPPING_TAX_COLUMNS.get(slot).copied()
}

/// Read one `MP` tax slot (`0..=4`).
pub fn mapping_tax_code(mp: &Table, row: usize, slot: usize) -> Result<String> {
    let col = mapping_tax_column(slot).ok_or_else(|| {
        CoreError::Format(format!(
            "mapping tax slot {slot} outside 0..={} (TAC_NO, TAX1..TAX4)",
            MAPPING_TAX_COLUMNS.len() - 1
        ))
    })?;
    text(mp, row, col)
}

/// Assign one `MP` tax slot after validating the code (`""` clears).
pub fn assign_mapping_tax_code(
    mp: &mut Table,
    row: usize,
    slot: usize,
    tp: &TaxExportTable,
    code: &str,
) -> Result<()> {
    let col = mapping_tax_column(slot).ok_or_else(|| {
        CoreError::Format(format!("mapping tax slot {slot} has no MP column"))
    })?;
    let c = code.trim();
    if !c.is_empty() {
        tp.validate(c)?;
    }
    write_cell(mp, row, col, c)
}

/// Clear one `MP` tax slot.
pub fn clear_mapping_tax_code(mp: &mut Table, row: usize, slot: usize) -> Result<()> {
    let col = mapping_tax_column(slot)
        .ok_or_else(|| CoreError::Format(format!("mapping tax slot {slot} has no MP column")))?;
    write_cell(mp, row, col, "")
}

/// Length of a Canadian GIFI tag (`AM.GIFI`/`MP.GIFI`).
pub const GIFI_CODE_LEN: usize = 4;

/// Validate a GIFI tag: exactly four ASCII digits (`1000`, `3620`, ...).
pub fn gifi_code(raw: &str) -> Result<String> {
    let s = raw.trim();
    if s.len() != GIFI_CODE_LEN || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(CoreError::Format(format!(
            "GIFI code {s:?} must be exactly {GIFI_CODE_LEN} ASCII digits"
        )));
    }
    Ok(s.to_string())
}

/// `AM.GIFI` - the account's GIFI tag (blank when untagged).
pub fn account_gifi(am: &Table, row: usize) -> Result<String> {
    text(am, row, "GIFI")
}

/// Assign `AM.GIFI` (`""` clears).
pub fn assign_account_gifi(am: &mut Table, row: usize, code: &str) -> Result<()> {
    let c = if code.trim().is_empty() {
        String::new()
    } else {
        gifi_code(code)?
    };
    write_cell(am, row, "GIFI", &c)
}

/// Clear `AM.GIFI`.
pub fn clear_account_gifi(am: &mut Table, row: usize) -> Result<()> {
    write_cell(am, row, "GIFI", "")
}

/// `MP.GIFI` - the mapping node's GIFI tag.
pub fn mapping_gifi(mp: &Table, row: usize) -> Result<String> {
    text(mp, row, "GIFI")
}

/// Assign `MP.GIFI` (`""` clears).
pub fn assign_mapping_gifi(mp: &mut Table, row: usize, code: &str) -> Result<()> {
    let c = if code.trim().is_empty() {
        String::new()
    } else {
        gifi_code(code)?
    };
    write_cell(mp, row, "GIFI", &c)
}

/// Clear `MP.GIFI`.
pub fn clear_mapping_gifi(mp: &mut Table, row: usize) -> Result<()> {
    write_cell(mp, row, "GIFI", "")
}

/// One GIFI-tagged account as the export lane sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct GifiRow {
    pub row: usize,
    pub ac_no: String,
    pub code: String,
    /// Year-0 `OPEN + CUR_YTD` net supplied by the caller (the F55 export
    /// amount basis). The golden `AM` balance columns are blank, so the
    /// caller must project this from the `BL` cube.
    pub amount: f64,
}

/// GIFI-tagged accounts that pass the F55 row gate: non-blank `GIFI` and a
/// non-zero year-0 net in `amounts` (keyed by `AC_NO`). Order follows the
/// chart. Amounts are rounded to cents.
pub fn gifi_rows(am: &Table, amounts: &BTreeMap<String, f64>) -> Vec<GifiRow> {
    let mut out = Vec::new();
    for r in 0..am.records.len() {
        if am.is_deleted(r) {
            continue;
        }
        let Ok(code) = text(am, r, "GIFI") else { continue };
        let code = code.trim();
        if code.is_empty() {
            continue;
        }
        let ac_no = text(am, r, "AC_NO").unwrap_or_default();
        let amount = money(*amounts.get(&ac_no).unwrap_or(&0.0));
        if amount == 0.0 {
            continue;
        }
        out.push(GifiRow {
            row: r,
            ac_no,
            code: code.to_string(),
            amount,
        });
    }
    out
}

/// A non-fatal GIFI assignment problem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GifiIssue {
    pub row: usize,
    pub ac_no: String,
    pub code: String,
    pub kind: GifiIssueKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GifiIssueKind {
    /// Tag is not exactly four digits.
    BadCode,
    /// Tag present but year-0 `O+Y` net is zero, so F55 will skip the row.
    TaggedWithoutBalance,
}

/// Audit `AM.GIFI` assignments against the F55 emission rules. `amounts` is
/// keyed by `AC_NO` exactly like [`gifi_rows`].
pub fn gifi_issues(am: &Table, amounts: &BTreeMap<String, f64>) -> Vec<GifiIssue> {
    let mut out = Vec::new();
    for r in 0..am.records.len() {
        if am.is_deleted(r) {
            continue;
        }
        let Ok(code) = text(am, r, "GIFI") else { continue };
        let code = code.trim();
        if code.is_empty() {
            continue;
        }
        let ac_no = text(am, r, "AC_NO").unwrap_or_default();
        if gifi_code(code).is_err() {
            out.push(GifiIssue {
                row: r,
                ac_no,
                code: code.to_string(),
                kind: GifiIssueKind::BadCode,
            });
            continue;
        }
        let amount = money(*amounts.get(&ac_no).unwrap_or(&0.0));
        if amount == 0.0 {
            out.push(GifiIssue {
                row: r,
                ac_no,
                code: code.to_string(),
                kind: GifiIssueKind::TaggedWithoutBalance,
            });
        }
    }
    out
}

/// Which table an unresolved tax code came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaxRefSource {
    Account,
    Mapping,
}

impl TaxRefSource {
    pub fn label(self) -> &'static str {
        match self {
            TaxRefSource::Account => "AM.TAC_NO",
            TaxRefSource::Mapping => "MP tax slot",
        }
    }
}

/// A tax-code reference that does not resolve in `TP`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaxReferenceIssue {
    pub source: TaxRefSource,
    /// Physical row in `AM`/`MP`.
    pub row: usize,
    /// `AC_NO` for account rows, `MAP_NO` for mapping rows.
    pub owner: String,
    /// `None` for `AM.TAC_NO`; `Some(0..=4)` for the mapping slots.
    pub slot: Option<usize>,
    pub code: String,
}

/// Unknown `AM.TAC_NO` references (F53 validation hook). The hull corpus has
/// 112 distinct account codes and all resolve, so this is empty on golden data.
pub fn account_tax_issues(am: &Table, tp: &TaxExportTable) -> Vec<TaxReferenceIssue> {
    let mut out = Vec::new();
    for r in 0..am.records.len() {
        if am.is_deleted(r) {
            continue;
        }
        let Ok(code) = text(am, r, "TAC_NO") else { continue };
        let code = code.trim();
        if code.is_empty() || tp.contains(code) {
            continue;
        }
        out.push(TaxReferenceIssue {
            source: TaxRefSource::Account,
            row: r,
            owner: text(am, r, "AC_NO").unwrap_or_default(),
            slot: None,
            code: code.to_string(),
        });
    }
    out
}

/// Unknown mapping tax references across `TAC_NO` + `TAX1..TAX4`.
pub fn mapping_tax_issues(mp: &Table, tp: &TaxExportTable) -> Vec<TaxReferenceIssue> {
    let mut out = Vec::new();
    for r in 0..mp.records.len() {
        if mp.is_deleted(r) {
            continue;
        }
        for (slot, col) in MAPPING_TAX_COLUMNS.iter().enumerate() {
            let Ok(code) = text(mp, r, col) else { continue };
            let code = code.trim();
            if code.is_empty() || tp.contains(code) {
                continue;
            }
            out.push(TaxReferenceIssue {
                source: TaxRefSource::Mapping,
                row: r,
                owner: text(mp, r, "MAP_NO").unwrap_or_default(),
                slot: Some(slot),
                code: code.to_string(),
            });
        }
    }
    out
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
    fn schema_is_18_columns_and_record_174() {
        assert_eq!(TP_SCHEMA.len(), TP_FIELD_COUNT);
        let mut seen = std::collections::BTreeSet::new();
        for f in TP_SCHEMA.iter() {
            assert!(seen.insert(f.name), "duplicate column {}", f.name);
        }
        assert_eq!(
            TP_SCHEMA.iter().map(|f| f.len as usize).sum::<usize>() + 1,
            TP_RECORD_LEN
        );
        assert!(TP_SCHEMA.iter().all(|f| matches!(f.kind, b'C' | b'N')));
    }

    #[test]
    fn user_defined_and_scope_laws() {
        assert_eq!(UserDefinedKind::parse(""), UserDefinedKind::Standard);
        assert_eq!(UserDefinedKind::parse("u"), UserDefinedKind::User);
        assert_eq!(UserDefinedKind::parse("X"), UserDefinedKind::Extension);
        assert!(!UserDefinedKind::Standard.is_user_defined());
        assert!(UserDefinedKind::Extension.is_user_defined());

        let c = TaxExportCode {
            row: 0,
            row_type: "W".into(),
            code: "01A".into(),
            description: "test".into(),
            sub: String::new(),
            cu_code: String::new(),
            pr_code: String::new(),
            order: String::new(),
            sign: String::new(),
            form: "1120".into(),
            entity: "C".into(),
            detail: "*".into(),
            year: "14".into(),
            percent: 100.0,
            user_defined: UserDefinedKind::Standard,
            ref_code: "O".into(),
            cwguid: "guid".into(),
            cwdate: String::new(),
            createdon: String::new(),
        };
        assert!(c.applies_to("1120", "C", "14"));
        assert!(!c.applies_to("1120S", "C", "14"));
        assert_eq!(c.label(), "01A - 1120/C");
    }

    #[test]
    fn gifi_code_is_four_digits() {
        assert_eq!(gifi_code(" 3620 ").unwrap(), "3620");
        assert!(gifi_code("362").is_err());
        assert!(gifi_code("36200").is_err());
        assert!(gifi_code("36a0").is_err());
        assert!(gifi_code("").is_err());
    }
}

