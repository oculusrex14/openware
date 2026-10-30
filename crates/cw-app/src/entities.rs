//! cw-app entities / organisation setup — the `CE` table model (P5.H H1).
//!
//! Contract: `docs\specs\entities.md` v0.9. Physical ground truth is
//! `inventory\dbf-schema.tsv`: all three frozen corpora carry the **same**
//! 33-field / 1089-byte-header / 448-byte-record `CE` layout and **0 live
//! records** (single-blank-entity normal form, `docs\interlink-model.md` §3).
//! The COM surface is `ICWEntity` / `ICWEntities` (`inventory\com-model\
//! tlb-01-cw42.dll.md`), which names the writable properties this model
//! decodes.
//!
//! COM — `CE` map (evidence: typelib names + column widths; corpus CE empty):
//!
//! | `ICWEntity` | `CE` column | note |
//! |---|---|---|
//! | `RawEntityCode` | `ENTITY` | consolidation entity code |
//! | `Name` | `NAME` | display name |
//! | `Abbreviation` | `ABBR` | short name |
//! | `PercentCurrentYear` | `PER` | ownership %, current year |
//! | `PercentPrior1..4` | `PER_P1..PER_P4` | ownership %, prior 1..4 |
//! | `PercentBudget` | `PER_BUD` | budget ownership % |
//! | `PercentForecast` | `PER_FCAST` | forecast ownership % |
//! | `ExternalIncludePrior` | `PRIOR` | 0/1 flag |
//! | `ExternalIncludeBudget` | `BUDGET` | 0/1 |
//! | `ExternalExcludeZero` | `EXCLUDE` | 0/1 |
//! | `ExternalApplyForeignExchange` | `FX` | 0/1 (FX lane = H3) |
//! | `ExternalIncludeForecast` | `FCAST` | 0/1 |
//! | `ExternalSync` | `SYNC` | 0/1 |
//! | `ExternalUnmapNewAccounts` | `UNMAP` | 0/1 |
//! | `ExternalIncludeCustom` | `CUSTOMBAL` | 0/1 |
//! | `IsUsingExternalSource` | `SOURCE` | 1-char source code |
//! | `ItemIdentifier` | `CWGUID` | row id |
//! | (file identity) | `FILEGUID` | linked-file id |
//! | `ItemLastChange` | `CWDATE` | 14-char timestamp |
//! | `LastSyncDate` | `LASTSYNC` | 14-char timestamp |
//! | `ExDescription` | `DESC` | memo pointer (payload not decoded) |
//! | `Properties` | `CLP` / `EXINFO` | memo pointers to extended props |
//!
//! Confidence markers: the column *widths* and the **7 percentage columns**
//! are [O] (schema census). The property?”column assignment is [I] —CE is
//! empty in every corpus, so no populated row can confirm it. `PER*` is read
//! as the ownership-percent family because the count (7) and COM names
//! (`PercentCurrentYear/Prior1..4/Budget/Forecast`) match exactly; the
//! alternative (period-end dates) has no COM counterpart. A future COM probe
//! that writes one entity settles it.
//!
//! Scope (H1): entity row model, `CE` read/write, org setup validation, and
//! multi-year ownership access. Consolidation maths (`CONSCHKSUM`, units) is
//! H2; FX conversion (`FXCYTD`/`C_RATE`/`FXTYPE`) and fund accounting is H3.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use cw_core::dbf::Table;
use cw_core::guid::{CwGuid, Rng};
use cw_core::{CoreError, Result};

use crate::accounts::{write_cell, DriftKind, SchemaDrift};

/// Physical `CE` field count (all three corpus schemas).
pub const CE_FIELD_COUNT: usize = 33;
/// Physical `CE` record length, deletion flag included.
pub const CE_RECORD_LEN: usize = 448;
/// Physical `CE` header length (32 + 33*32 + 0x0D), all corpora.
pub const CE_HEADER_LEN: usize = 1089;
/// Entity code width (`CE.ENTITY`).
pub const ENTITY_CODE_LEN: usize = 40;
/// Lower/upper bound for a consolidation percentage [I].
pub const PERCENT_MIN: f64 = 0.0;
/// Upper bound for a consolidation percentage [I].
pub const PERCENT_MAX: f64 = 100.0;

const LIVE: u8 = b' ';
const DELETED: u8 = b'*';

/// One field descriptor of the canonical `CE` layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CeField {
    pub name: &'static str,
    /// dBase type letter (`C`/`N`/`M`).
    pub kind: u8,
    pub len: u8,
    pub dec: u8,
}

const fn f(name: &'static str, kind: u8, len: u8, dec: u8) -> CeField {
    CeField { name, kind, len, dec }
}

/// Canonical `CE` schema in physical order (33 columns).
pub static CE_SCHEMA: [CeField; CE_FIELD_COUNT] = [
    f("ENTITY", b'C', 40, 0),
    f("TEMPENT", b'C', 40, 0),
    f("TYPE", b'C', 1, 0),
    f("NAME", b'C', 100, 0),
    f("DIV", b'C', 6, 0),
    f("PER", b'C', 10, 0),
    f("PER_P1", b'C', 10, 0),
    f("PER_P2", b'C', 10, 0),
    f("PER_P3", b'C', 10, 0),
    f("PER_P4", b'C', 10, 0),
    f("PER_BUD", b'C', 10, 0),
    f("LOCATION", b'M', 10, 0),
    f("ABBR", b'C', 40, 0),
    f("ENT_MOD", b'C', 14, 0),
    f("PRIOR", b'N', 1, 0),
    f("BUDGET", b'N', 1, 0),
    f("EXCLUDE", b'N', 1, 0),
    f("DESC", b'M', 10, 0),
    f("SOURCE", b'C', 1, 0),
    f("STATUS", b'C', 1, 0),
    f("FX", b'N', 1, 0),
    f("CWGUID", b'C', 22, 0),
    f("PER_FCAST", b'C', 10, 0),
    f("FCAST", b'N', 1, 0),
    f("SYNC", b'N', 1, 0),
    f("CWDATE", b'C', 14, 0),
    f("FILEGUID", b'C', 22, 0),
    f("CREATEDON", b'C', 14, 0),
    f("LASTSYNC", b'C', 14, 0),
    f("UNMAP", b'N', 1, 0),
    f("CUSTOMBAL", b'N', 1, 0),
    f("CLP", b'M', 10, 0),
    f("EXINFO", b'M', 10, 0),
];

/// Look up a canonical `CE` field by name.
pub fn ce_schema_field(name: &str) -> Option<&'static CeField> {
    let n = name.trim().to_ascii_uppercase();
    CE_SCHEMA.iter().find(|f| f.name == n)
}

/// Full physical-schema comparison against [`CE_SCHEMA`] (order, names,
/// types, widths, decimals). Shares [`SchemaDrift`] with the `AM`/`MP` models.
pub fn ce_schema_drift(t: &Table) -> Vec<SchemaDrift> {
    let mut out = Vec::new();
    let mut actual_names: Vec<String> = Vec::with_capacity(t.fields.len());
    for fd in &t.fields {
        actual_names.push(fd.name.to_ascii_uppercase());
    }
    for e in CE_SCHEMA.iter() {
        if !actual_names.iter().any(|n| n == e.name) {
            out.push(SchemaDrift {
                column: e.name.to_string(),
                kind: DriftKind::Missing,
                expected: format!("{}{}.{}", e.kind as char, e.len, e.dec),
                actual: String::new(),
            });
        }
    }
    for a in actual_names.iter() {
        if !CE_SCHEMA.iter().any(|e| e.name == a) {
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
        let Some(e) = CE_SCHEMA.iter().find(|e| e.name == name) else {
            continue;
        };
        let expected_pos = CE_SCHEMA.iter().position(|x| x.name == name).unwrap();
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

/// True when the table is the canonical 33-column `CE` layout.
pub fn ce_schema_matches(t: &Table) -> bool {
    ce_schema_drift(t).is_empty()
}

/// Build an empty dBase III table from `(name, kind, len, dec)` descriptors.
///
/// Used by org setup to materialise a `CE` file in an engagement that has none
/// (the corpus normal form) and by the gate test for synthetic projections.
pub fn blank_table(columns: &[(&str, u8, u8, u8)]) -> Table {
    let header_len = 32 + 32 * columns.len() + 1;
    let record_len = 1 + columns.iter().map(|c| c.2 as usize).sum::<usize>();
    let mut raw = vec![0u8; header_len];
    raw[0] = 0x03; // dBase III
    raw[8..10].copy_from_slice(&(header_len as u16).to_le_bytes());
    raw[10..12].copy_from_slice(&(record_len as u16).to_le_bytes());
    let mut fields = Vec::with_capacity(columns.len());
    for (i, (name, kind, len, dec)) in columns.iter().enumerate() {
        let off = 32 + i * 32;
        let nb = name.to_ascii_uppercase().into_bytes();
        raw[off..off + nb.len()].copy_from_slice(&nb);
        raw[off + 11] = *kind;
        raw[off + 16] = *len;
        raw[off + 17] = *dec;
        fields.push(cw_core::dbf::Field {
            name: name.to_ascii_uppercase(),
            kind: *kind,
            len: *len,
            dec: *dec,
        });
    }
    raw[header_len - 1] = 0x0D;
    Table {
        raw_header: raw,
        version: 0x03,
        header_len: header_len as u16,
        record_len: record_len as u16,
        fields,
        records: Vec::new(),
    }
}

/// Empty canonical `CE` table (1089-byte header, 448-byte records).
pub fn blank_ce_table() -> Table {
    let cols: Vec<(&str, u8, u8, u8)> =
        CE_SCHEMA.iter().map(|f| (f.name, f.kind, f.len, f.dec)).collect();
    blank_table(&cols)
}

fn cell(t: &Table, r: usize, name: &str) -> String {
    match t.cell(r, name) {
        Some(raw) => raw
            .iter()
            .map(|&b| b as char)
            .collect::<String>()
            .trim()
            .to_string(),
        None => String::new(),
    }
}

fn put(t: &mut Table, r: usize, name: &str, value: &str) -> Result<()> {
    write_cell(t, r, name, value)
}

/// CaseWare `N`/`C` boolean cell: `""` (unset), `0`, `1`, `Y`, `N`, `T`, `F`.
///
/// The raw text is retained so parse/write is byte-faithful (the corpus stores
/// these columns as `N 1`; COM presents them as `VT_BOOL`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CeBool(String);

impl CeBool {
    pub fn parse(raw: &str) -> Result<CeBool> {
        let s = raw.trim().to_ascii_uppercase();
        match s.as_str() {
            "" | "0" | "1" | "Y" | "N" | "T" | "F" => Ok(CeBool(s)),
            other => Err(CoreError::Format(format!(
                "invalid CE boolean {other:?} (expected blank, 0/1, Y/N or T/F)"
            ))),
        }
    }

    pub fn yes() -> CeBool {
        CeBool("1".into())
    }

    pub fn no() -> CeBool {
        CeBool("0".into())
    }

    pub fn unset() -> CeBool {
        CeBool(String::new())
    }

    pub fn is_true(&self) -> bool {
        matches!(self.0.as_str(), "1" | "Y" | "T")
    }

    pub fn is_set(&self) -> bool {
        !self.0.is_empty()
    }

    pub fn raw(&self) -> &str {
        &self.0
    }
}

/// A consolidation percentage cell (`CE.PER*`, `C 10`).
///
/// The raw text is preserved verbatim; `value` is the COM-facing double.
/// Range validation is deferred to [`Org::validate`] so a malformed file can
/// still be read and reported instead of failing the whole load.
#[derive(Clone, Debug, PartialEq)]
pub struct Percent {
    value: f64,
    raw: String,
}

impl Percent {
    pub fn parse(raw: &str) -> Result<Option<Percent>> {
        let s = raw.trim();
        if s.is_empty() || s == "." {
            return Ok(None);
        }
        let value = s.parse::<f64>().map_err(|_| {
            CoreError::Format(format!("invalid CE percentage {s:?}"))
        })?;
        if !value.is_finite() {
            return Err(CoreError::Format(format!(
                "non-finite CE percentage {s:?}"
            )));
        }
        Ok(Some(Percent { value, raw: s.to_string() }))
    }

    /// Build a validated percentage (0..=100, [I]).
    pub fn new(value: f64) -> Result<Percent> {
        if !value.is_finite() || !(PERCENT_MIN..=PERCENT_MAX).contains(&value) {
            return Err(CoreError::Format(format!(
                "CE percentage {value} outside [{PERCENT_MIN}, {PERCENT_MAX}]"
            )));
        }
        Ok(Percent { value, raw: format_percent(value) })
    }

    pub fn value(&self) -> f64 {
        self.value
    }

    pub fn raw(&self) -> &str {
        &self.raw
    }

    pub fn in_range(&self) -> bool {
        (PERCENT_MIN..=PERCENT_MAX).contains(&self.value)
    }
}

fn format_percent(v: f64) -> String {
    let mut s = format!("{v:.5}");
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    s
}

/// One column of the multi-year ownership grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Year {
    Current,
    Prior1,
    Prior2,
    Prior3,
    Prior4,
    Budget,
    Forecast,
}

impl Year {
    /// All seven COM ownership columns, current year first.
    pub const ALL: [Year; 7] = [
        Year::Current,
        Year::Prior1,
        Year::Prior2,
        Year::Prior3,
        Year::Prior4,
        Year::Budget,
        Year::Forecast,
    ];

    pub fn index(self) -> usize {
        match self {
            Year::Current => 0,
            Year::Prior1 => 1,
            Year::Prior2 => 2,
            Year::Prior3 => 3,
            Year::Prior4 => 4,
            Year::Budget => 5,
            Year::Forecast => 6,
        }
    }

    pub fn ce_column(self) -> &'static str {
        match self {
            Year::Current => "PER",
            Year::Prior1 => "PER_P1",
            Year::Prior2 => "PER_P2",
            Year::Prior3 => "PER_P3",
            Year::Prior4 => "PER_P4",
            Year::Budget => "PER_BUD",
            Year::Forecast => "PER_FCAST",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Year::Current => "current",
            Year::Prior1 => "prior 1",
            Year::Prior2 => "prior 2",
            Year::Prior3 => "prior 3",
            Year::Prior4 => "prior 4",
            Year::Budget => "budget",
            Year::Forecast => "forecast",
        }
    }

    /// Prior-year offset (`Prior1` —`Some(1)`); `None` for the rest.
    pub fn prior_offset(self) -> Option<i32> {
        match self {
            Year::Prior1 => Some(1),
            Year::Prior2 => Some(2),
            Year::Prior3 => Some(3),
            Year::Prior4 => Some(4),
            _ => None,
        }
    }
}

/// The ownership-percent grid: current + prior 1..4 + budget + forecast.
#[derive(Clone, Debug, PartialEq)]
pub struct Ownership {
    values: [Option<Percent>; 7],
}

impl Default for Ownership {
    fn default() -> Self {
        Ownership { values: std::array::from_fn(|_| None) }
    }
}

impl Ownership {
    pub fn get(&self, year: Year) -> Option<&Percent> {
        self.values[year.index()].as_ref()
    }

    pub fn value(&self, year: Year) -> Option<f64> {
        self.get(year).map(|p| p.value())
    }

    pub fn current(&self) -> Option<f64> {
        self.value(Year::Current)
    }

    pub fn set(&mut self, year: Year, percent: Option<Percent>) {
        self.values[year.index()] = percent;
    }

    /// 100% in the current year (a wholly owned unit) —convenience only.
    pub fn full() -> Ownership {
        let mut o = Ownership::default();
        o.set(Year::Current, Percent::new(100.0).ok());
        o
    }

    /// True when every set value is inside `[PERCENT_MIN, PERCENT_MAX]`.
    pub fn in_range(&self) -> bool {
        self.values.iter().flatten().all(|p| p.in_range())
    }

    /// True when at least one year carries a value.
    pub fn is_set(&self) -> bool {
        self.values.iter().any(|v| v.is_some())
    }

    fn from_row(t: &Table, r: usize) -> Result<Ownership> {
        let mut o = Ownership::default();
        for y in Year::ALL {
            o.set(y, Percent::parse(&cell(t, r, y.ce_column()))?);
        }
        Ok(o)
    }

    fn write_into(&self, t: &mut Table, r: usize) -> Result<()> {
        for y in Year::ALL {
            let raw = self.get(y).map(|p| p.raw()).unwrap_or("");
            put(t, r, y.ce_column(), raw)?;
        }
        Ok(())
    }
}

/// One decoded `CE` row (a consolidation entity).
#[derive(Clone, Debug, PartialEq)]
pub struct Entity {
    /// `ENTITY` —consolidation code; blank is the single-entity normal form.
    pub code: String,
    /// `TEMPENT` —temporary/import staging code.
    pub temp_code: String,
    /// `TYPE` —1-char entity type.
    pub kind: String,
    /// `NAME` —display name.
    pub name: String,
    /// `DIV` —division code.
    pub division: String,
    /// `PER*` —ownership percentages across the multi-year grid.
    pub ownership: Ownership,
    /// `ABBR` —abbreviation.
    pub abbr: String,
    /// `ENT_MOD` —entity-modified timestamp (`C 14`).
    pub modified: String,
    /// `PRIOR` —include prior-year data.
    pub include_prior: CeBool,
    /// `BUDGET` —include budget data.
    pub include_budget: CeBool,
    /// `EXCLUDE` —exclude from the consolidation.
    pub exclude: CeBool,
    /// `SOURCE` —external-source marker.
    pub source: String,
    /// `STATUS` —status marker.
    pub status: String,
    /// `FX` —apply foreign exchange (conversion itself is H3).
    pub apply_fx: CeBool,
    /// `PER_FCAST` companion flag `FCAST` —include forecast.
    pub include_forecast: CeBool,
    /// `SYNC` —external sync marker.
    pub sync: CeBool,
    /// `UNMAP` —unmap new accounts (consolidation-ready switch).
    pub unmap_new_accounts: CeBool,
    /// `CUSTOMBAL` —include custom balances.
    pub include_custom: CeBool,
    /// `CWGUID` —row id.
    pub cwguid: String,
    /// `FILEGUID` —linked-file id.
    pub fileguid: String,
    /// `CWDATE` / `CREATEDON` / `LASTSYNC` —`C 14` audit stamps.
    pub cwdate: String,
    pub createdon: String,
    pub lastsync: String,
    /// Memo cells (`LOCATION`, `DESC`, `CLP`, `EXINFO`) —raw FPT pointers.
    pub location_ptr: String,
    pub description_ptr: String,
    pub clp_ptr: String,
    pub exinfo_ptr: String,
}

impl Entity {
    /// New entity row (no GUID, no ownership set). `code` may be blank only
    /// when the caller is building the explicit single-entity form.
    pub fn new(code: &str, name: &str) -> Result<Entity> {
        let code = code.trim();
        let name = name.trim();
        if code.chars().count() > ENTITY_CODE_LEN {
            return Err(CoreError::Format(format!(
                "entity code {code:?} exceeds {ENTITY_CODE_LEN} chars"
            )));
        }
        if name.chars().count() > 100 {
            return Err(CoreError::Format(format!(
                "entity name {name:?} exceeds 100 chars"
            )));
        }
        Ok(Entity {
            code: code.to_string(),
            temp_code: String::new(),
            kind: String::new(),
            name: name.to_string(),
            division: String::new(),
            ownership: Ownership::default(),
            abbr: String::new(),
            modified: String::new(),
            include_prior: CeBool::unset(),
            include_budget: CeBool::unset(),
            exclude: CeBool::unset(),
            source: String::new(),
            status: String::new(),
            apply_fx: CeBool::unset(),
            include_forecast: CeBool::unset(),
            sync: CeBool::unset(),
            unmap_new_accounts: CeBool::unset(),
            include_custom: CeBool::unset(),
            cwguid: String::new(),
            fileguid: String::new(),
            cwdate: String::new(),
            createdon: String::new(),
            lastsync: String::new(),
            location_ptr: String::new(),
            description_ptr: String::new(),
            clp_ptr: String::new(),
            exinfo_ptr: String::new(),
        })
    }

    /// New entity with a fresh `CWGUID` (CE has no corpus rows —family A).
    pub fn new_with_guid(code: &str, name: &str, rng: &mut Rng) -> Result<Entity> {
        let mut e = Entity::new(code, name)?;
        e.cwguid = CwGuid::allocate_for_table("CE", rng).encode();
        Ok(e)
    }

    pub fn is_blank_code(&self) -> bool {
        self.code.trim().is_empty()
    }

    /// Included in an actual consolidation (`EXCLUDE` unset/false).
    pub fn is_included(&self) -> bool {
        !self.exclude.is_true()
    }

    pub fn year_percent(&self, year: Year) -> Option<f64> {
        self.ownership.value(year)
    }

    pub fn set_year_percent(&mut self, year: Year, percent: Option<Percent>) {
        self.ownership.set(year, percent);
    }

    /// Decode one live `CE` record.
    pub fn from_row(t: &Table, r: usize) -> Result<Entity> {
        if t.records.get(r).map(|rec| rec[0] != LIVE).unwrap_or(true) {
            return Err(CoreError::Format(format!("CE row {r} is not live")));
        }
        Ok(Entity {
            code: cell(t, r, "ENTITY"),
            temp_code: cell(t, r, "TEMPENT"),
            kind: cell(t, r, "TYPE"),
            name: cell(t, r, "NAME"),
            division: cell(t, r, "DIV"),
            ownership: Ownership::from_row(t, r)?,
            abbr: cell(t, r, "ABBR"),
            modified: cell(t, r, "ENT_MOD"),
            include_prior: CeBool::parse(&cell(t, r, "PRIOR"))?,
            include_budget: CeBool::parse(&cell(t, r, "BUDGET"))?,
            exclude: CeBool::parse(&cell(t, r, "EXCLUDE"))?,
            source: cell(t, r, "SOURCE"),
            status: cell(t, r, "STATUS"),
            apply_fx: CeBool::parse(&cell(t, r, "FX"))?,
            include_forecast: CeBool::parse(&cell(t, r, "FCAST"))?,
            sync: CeBool::parse(&cell(t, r, "SYNC"))?,
            unmap_new_accounts: CeBool::parse(&cell(t, r, "UNMAP"))?,
            include_custom: CeBool::parse(&cell(t, r, "CUSTOMBAL"))?,
            cwguid: cell(t, r, "CWGUID"),
            fileguid: cell(t, r, "FILEGUID"),
            cwdate: cell(t, r, "CWDATE"),
            createdon: cell(t, r, "CREATEDON"),
            lastsync: cell(t, r, "LASTSYNC"),
            location_ptr: cell(t, r, "LOCATION"),
            description_ptr: cell(t, r, "DESC"),
            clp_ptr: cell(t, r, "CLP"),
            exinfo_ptr: cell(t, r, "EXINFO"),
        })
    }

    /// Append this entity to `t` (which must be a canonical empty/`CE` table).
    ///
    /// Memo cells are written blank: FPT payloads are not decoded by H1
    /// (`cw-core::fpt` only carries pointers), so re-emitting a stale pointer
    /// would be worse than dropping it. Everything else round-trips.
    pub fn write_into(&self, t: &mut Table, r: usize) -> Result<()> {
        put(t, r, "ENTITY", &self.code)?;
        put(t, r, "TEMPENT", &self.temp_code)?;
        put(t, r, "TYPE", &self.kind)?;
        put(t, r, "NAME", &self.name)?;
        put(t, r, "DIV", &self.division)?;
        self.ownership.write_into(t, r)?;
        put(t, r, "ABBR", &self.abbr)?;
        put(t, r, "ENT_MOD", &self.modified)?;
        put(t, r, "PRIOR", self.include_prior.raw())?;
        put(t, r, "BUDGET", self.include_budget.raw())?;
        put(t, r, "EXCLUDE", self.exclude.raw())?;
        put(t, r, "SOURCE", &self.source)?;
        put(t, r, "STATUS", &self.status)?;
        put(t, r, "FX", self.apply_fx.raw())?;
        put(t, r, "FCAST", self.include_forecast.raw())?;
        put(t, r, "SYNC", self.sync.raw())?;
        put(t, r, "UNMAP", self.unmap_new_accounts.raw())?;
        put(t, r, "CUSTOMBAL", self.include_custom.raw())?;
        put(t, r, "CWGUID", &self.cwguid)?;
        put(t, r, "FILEGUID", &self.fileguid)?;
        put(t, r, "CWDATE", &self.cwdate)?;
        put(t, r, "CREATEDON", &self.createdon)?;
        put(t, r, "LASTSYNC", &self.lastsync)?;
        Ok(())
    }
}

/// Severity-free org problem record (validation never stops at the first).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrgIssue {
    pub kind: OrgIssueKind,
    pub entity: String,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrgIssueKind {
    DuplicateEntityCode,
    BlankEntityCode,
    MissingName,
    PercentOutOfRange,
    UnknownEntity,
    NoEntities,
}

impl fmt::Display for OrgIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} [{}]: {}",
            self.kind,
            if self.entity.is_empty() { "<blank>" } else { &self.entity },
            self.detail
        )
    }
}

/// The engagement's organisation: the live `CE` rows.
///
/// `Org::default()` (no rows) is the **single-blank-entity normal form** the
/// corpora prove: every `AM`/`BL`/`GL` row carries `ENTITY=''`, and `CE.dbf`
/// is empty. [`Org::effective_entities`] materialises the implicit blank
/// entity when the table is empty.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Org {
    pub entities: Vec<Entity>,
}

impl Org {
    /// Parse a `CE` table, rejecting physical-schema drift.
    pub fn from_table(t: &Table) -> Result<Org> {
        let drift = ce_schema_drift(t);
        if !drift.is_empty() {
            return Err(CoreError::Format(format!(
                "CE schema drift: {} issue(s), first {:?}",
                drift.len(),
                drift[0]
            )));
        }
        let mut entities = Vec::new();
        for r in 0..t.count() {
            match t.records[r][0] {
                LIVE => entities.push(Entity::from_row(t, r)?),
                DELETED => {}
                b => {
                    return Err(CoreError::Format(format!(
                        "CE row {r}: invalid deletion flag {b:#x}"
                    )))
                }
            }
        }
        Ok(Org { entities })
    }

    pub fn read_path(path: impl AsRef<Path>) -> Result<Org> {
        Org::from_table(&Table::parse_path(path)?)
    }

    /// Materialise a canonical `CE` table (all rows live, in order).
    pub fn to_table(&self) -> Result<Table> {
        let mut t = blank_ce_table();
        for e in &self.entities {
            let r = t.records.len();
            t.records.push(vec![LIVE; CE_RECORD_LEN]);
            e.write_into(&mut t, r)?;
        }
        Ok(t)
    }

    pub fn write_path(&self, path: impl AsRef<Path>) -> Result<()> {
        self.to_table()?.write_path(path)
    }

    /// No `CE` rows: the corpus normal form.
    pub fn single_blank() -> Org {
        Org::default()
    }

    pub fn from_entities(entities: Vec<Entity>) -> Org {
        Org { entities }
    }

    /// True when the org is the corpus normal form (no rows, or one blank row).
    pub fn is_single_blank(&self) -> bool {
        self.entities.is_empty()
            || (self.entities.len() == 1 && self.entities[0].is_blank_code())
    }

    /// Rows plus the implicit blank entity when the table is empty.
    pub fn effective_entities(&self) -> Vec<Entity> {
        if self.entities.is_empty() {
            vec![Entity::new("", "").expect("blank entity is always valid")]
        } else {
            self.entities.clone()
        }
    }

    pub fn by_code(&self, code: &str) -> Option<&Entity> {
        let code = code.trim();
        self.entities.iter().find(|e| e.code.trim() == code)
    }

    /// Rows that participate in a consolidation (`EXCLUDE` not true).
    pub fn included(&self) -> Vec<&Entity> {
        self.entities.iter().filter(|e| e.is_included()).collect()
    }

    /// Rows with a positive current-year ownership percent and not excluded.
    pub fn consolidation_units(&self) -> Vec<&Entity> {
        self.entities
            .iter()
            .filter(|e| e.is_included() && e.year_percent(Year::Current).unwrap_or(0.0) > 0.0)
            .collect()
    }

    pub fn year_percent(&self, code: &str, year: Year) -> Option<f64> {
        self.by_code(code).and_then(|e| e.year_percent(year))
    }

    /// Validation pass: duplicate/blank codes, names, percentage ranges.
    pub fn validate(&self) -> Vec<OrgIssue> {
        let mut out = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for e in &self.entities {
            let code = e.code.trim();
            if code.is_empty() {
                out.push(OrgIssue {
                    kind: OrgIssueKind::BlankEntityCode,
                    entity: String::new(),
                    detail: "entity code is blank".into(),
                });
            } else if !seen.insert(code.to_ascii_uppercase()) {
                out.push(OrgIssue {
                    kind: OrgIssueKind::DuplicateEntityCode,
                    entity: code.into(),
                    detail: "entity code appears more than once".into(),
                });
            }
            if e.name.trim().is_empty() {
                out.push(OrgIssue {
                    kind: OrgIssueKind::MissingName,
                    entity: code.into(),
                    detail: "entity name is blank".into(),
                });
            }
            for y in Year::ALL {
                if let Some(p) = e.ownership.get(y) {
                    if !p.in_range() {
                        out.push(OrgIssue {
                            kind: OrgIssueKind::PercentOutOfRange,
                            entity: code.into(),
                            detail: format!(
                                "{} ownership {} outside [{PERCENT_MIN}, {PERCENT_MAX}]",
                                y.label(),
                                p.value()
                            ),
                        });
                    }
                }
            }
        }
        if self.entities.len() > 1 && self.entities.iter().any(|e| e.is_blank_code()) {
            out.push(OrgIssue {
                kind: OrgIssueKind::BlankEntityCode,
                entity: String::new(),
                detail: "multi-entity org contains a blank entity code".into(),
            });
        }
        out
    }
}

/// Unique `ENTITY` values across an engagement table (`AM`/`BL`/`GL`).
///
/// Implements the interlink law: every numeric row carries `ENTITY`, and the
/// corpus normal form is `''` everywhere while `CE` is empty.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EntityDimension {
    /// entity code —live row count (blank key = `""`).
    pub counts: BTreeMap<String, usize>,
}

impl EntityDimension {
    pub fn from_table(t: &Table) -> Result<EntityDimension> {
        if t.field_index("ENTITY").is_none() {
            return Err(CoreError::Format("table has no ENTITY column".into()));
        }
        let mut counts = BTreeMap::new();
        for r in 0..t.count() {
            if t.records[r][0] != LIVE {
                continue;
            }
            *counts.entry(cell(t, r, "ENTITY")).or_insert(0) += 1;
        }
        Ok(EntityDimension { counts })
    }

    pub fn blank_rows(&self) -> usize {
        self.counts.get("").copied().unwrap_or(0)
    }

    /// True when every row is the blank entity.
    pub fn is_single_blank(&self) -> bool {
        self.counts.keys().all(|k| k.is_empty())
    }

    pub fn codes(&self) -> impl Iterator<Item = &String> {
        self.counts.keys().filter(|k| !k.is_empty())
    }

    /// Check the dimension against an org roster.
    pub fn check(&self, org: &Org) -> Vec<OrgIssue> {
        let mut out = Vec::new();
        if org.is_single_blank() {
            if !self.is_single_blank() {
                for code in self.codes() {
                    out.push(OrgIssue {
                        kind: OrgIssueKind::UnknownEntity,
                        entity: code.clone(),
                        detail: "non-blank ENTITY in a single-blank engagement".into(),
                    });
                }
            }
            return out;
        }
        let roster: BTreeSet<&str> =
            org.entities.iter().map(|e| e.code.trim()).collect();
        for code in self.codes() {
            if !roster.contains(code.as_str()) {
                out.push(OrgIssue {
                    kind: OrgIssueKind::UnknownEntity,
                    entity: code.clone(),
                    detail: "ENTITY value has no CE row".into(),
                });
            }
        }
        if self.blank_rows() > 0 && !roster.contains("") {
            out.push(OrgIssue {
                kind: OrgIssueKind::UnknownEntity,
                entity: String::new(),
                detail: "blank ENTITY rows but no blank CE row".into(),
            });
        }
        out
    }
}

/// Incremental org-setup builder (new engagement / add subsidiary).
#[derive(Clone, Debug, Default)]
pub struct OrgSetup {
    org: Org,
}

impl OrgSetup {
    pub fn new() -> OrgSetup {
        OrgSetup::default()
    }

    /// Start from the corpus normal form (empty `CE`).
    pub fn single_blank() -> OrgSetup {
        OrgSetup::default()
    }

    pub fn org(&self) -> &Org {
        &self.org
    }

    /// Add one entity. Codes are unique case-insensitively and non-blank;
    /// the first added entity makes the org multi-entity (single-blank form
    /// is then no longer valid).
    pub fn add_entity(&mut self, code: &str, name: &str) -> Result<&mut Entity> {
        let code = code.trim();
        if code.is_empty() {
            return Err(CoreError::Format(
                "org setup requires a non-blank entity code".into(),
            ));
        }
        if self
            .org
            .entities
            .iter()
            .any(|e| e.code.eq_ignore_ascii_case(code))
        {
            return Err(CoreError::Format(format!(
                "duplicate entity code {code:?}"
            )));
        }
        self.org.entities.push(Entity::new(code, name)?);
        Ok(self.org.entities.last_mut().expect("just pushed"))
    }

    /// Finish, returning the org plus any remaining validation issues.
    pub fn finish(self) -> (Org, Vec<OrgIssue>) {
        let issues = self.org.validate();
        (self.org, issues)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_ce_table_matches_corpus_geometry() {
        let t = blank_ce_table();
        assert_eq!(t.header_len as usize, CE_HEADER_LEN);
        assert_eq!(t.record_len as usize, CE_RECORD_LEN);
        assert!(ce_schema_matches(&t));
        let bytes = t.encode();
        let re = Table::parse(&bytes).unwrap();
        assert_eq!(re.count(), 0);
        assert!(ce_schema_matches(&re));
    }

    #[test]
    fn blank_table_roundtrip() {
        let t = blank_table(&[("ENTITY", b'C', 40, 0)]);
        assert_eq!(t.header_len as usize, 32 + 32 + 1);
        assert_eq!(t.record_len as usize, 41);
        assert_eq!(Table::parse(&t.encode()).unwrap().count(), 0);
    }

    #[test]
    fn entity_roundtrip() {
        let mut org = Org::single_blank();
        let mut e = Entity::new("SUB1", "Subsidiary One").unwrap();
        e.ownership.set(Year::Current, Percent::new(80.0).ok());
        e.ownership.set(Year::Prior1, Percent::new(75.5).ok());
        e.exclude = CeBool::yes();
        e.apply_fx = CeBool::yes();
        org.entities.push(e);
        let t = org.to_table().unwrap();
        let re = Org::from_table(&Table::parse(&t.encode()).unwrap()).unwrap();
        assert_eq!(re, org);
        assert_eq!(re.entities[0].year_percent(Year::Current), Some(80.0));
        assert_eq!(re.entities[0].year_percent(Year::Prior1), Some(75.5));
        assert!(re.entities[0].exclude.is_true());
        assert!(re.validate().is_empty());
    }

    #[test]
    fn validation_catches_setup_errors() {
        let mut e1 = Entity::new("A", "Alpha").unwrap();
        e1.ownership.set(Year::Current, Percent::parse("140").unwrap());
        let mut e2 = Entity::new("a", "").unwrap();
        e2.exclude = CeBool::yes();
        let org = Org::from_entities(vec![e1, e2, Entity::new("", "Blank").unwrap()]);
        let kinds: Vec<OrgIssueKind> = org.validate().iter().map(|i| i.kind).collect();
        assert!(kinds.contains(&OrgIssueKind::DuplicateEntityCode));
        assert!(kinds.contains(&OrgIssueKind::MissingName));
        assert!(kinds.contains(&OrgIssueKind::PercentOutOfRange));
        assert!(kinds.contains(&OrgIssueKind::BlankEntityCode));
    }

    #[test]
    fn single_blank_dimension_law() {
        let org = Org::single_blank();
        assert!(org.is_single_blank());
        assert_eq!(org.effective_entities().len(), 1);

        let blank = blank_table(&[("ENTITY", b'C', 40, 0)]);
        let mut t = blank;
        t.records.push(vec![LIVE; 41]);
        let dim = EntityDimension::from_table(&t).unwrap();
        assert!(dim.is_single_blank());
        assert!(dim.check(&org).is_empty());

        let mut t2 = blank_table(&[("ENTITY", b'C', 40, 0)]);
        let mut row = vec![b' '; 41];
        row[1] = b'X'; // ENTITY='X', live flag stays blank
        t2.records.push(row);
        let dim2 = EntityDimension::from_table(&t2).unwrap();
        assert_eq!(dim2.check(&org).len(), 1);
    }

    #[test]
    fn setup_builder_enforces_unique_codes() {
        let mut s = OrgSetup::single_blank();
        s.add_entity("PARENT", "Parent Co").unwrap();
        assert!(s.add_entity("parent", "Dup").is_err());
        let (org, issues) = s.finish();
        assert!(issues.is_empty());
        assert_eq!(org.entities.len(), 1);
        assert_eq!(org.entities[0].name, "Parent Co");
    }
}

