//! cw-app 8-level document signoff - `SH.A1..A8` + the WPG response documents
//! (P5.G G2).
//!
//! Contract: `docs\specs\signoff.md` v1.0. Two established sources:
//!
//! 1. **COM surface** (`inventory\com-model`, F47): `doc.SignoffInitials(r)` /
//!    `doc.SignoffDate(r)` are indexed get+PUT for `r = 1..=8`; a put lands in
//!    `SH.A{r}` / `SH.AD{r}` (fixture `sec1`: A1=TS, A2=QB, AD=20261022).
//!    Role names come from `ClientOptions.Role(1..8)` = **Prepared by 1..3 /
//!    Reviewed by 1..5** (F47), i.e. levels 1-3 are the preparer ladder and
//!    4-8 the reviewer ladder. `doc.Sign` sets `SH.SIGN='Y'`;
//!    `doc.Lockdown` sets `SH.LOCKDOWN` (F47/F68).
//! 2. **Physical columns** (`inventory\dbf-schema.tsv`, 124 SH fields in every
//!    corpus): `A{n}` C8 initials, `AD{n}` D8 date, `AC{n}` C22 and `AU{n}`
//!    C22 are the two GUID-width companion slots. **AC/AU semantics are not
//!    established**: the only COM put->diff evidence covers `A{n}`/`AD{n}` (F47).
//!    In corp2026 the 52 populated level-1 rows carry `AU='E120235'` (a user
//!    id) and `AC` either blank or a 22-char CWGUID-shaped token, so the
//!    reader exposes both verbatim and the writer only touches them when the
//!    caller supplies a value (recorded residual, F65 cleared `A/AD/AC/AU`
//!    together at year-end).
//!
//! Audit law (F47/F67/F68): every signoff put auto-logs one `HI` row
//! `CATEGORY='DOCS'`, `TYPE='DOCSIGN{n}'`, `OBJECTTYPE='SH'`,
//! `OBJECTID=<doc GUID>`. [`SignoffBook::sign_off`] writes `SH` and appends
//! that row through [`HistoryLog::record_document_event`] in one call.
//!
//! WPG response documents
//! ----------------------
//! The signoff carriers are the CaseView **form** documents (`SH.TYPE='F'`) -
//! the same class [`crate::program::ResponseDocs`] projects (F30: "signoff
//! evidence lives in WPG response docs"). corp2026 has exactly 7 live
//! `TYPE='F'` rows under `PRG='ac'`; **4 are signed at level 1 only**
//! (`0300`, `0315.10`, `0325`, `0400`; initials `N`, user `E120235`) and
//! `0120`/`0126`/`0272` are unsigned. corp2026 is level-1-only, but the **other
//! two frozen corpora carry real multi-level ladders**, so the 8-level model is
//! no longer established only structurally:
//!
//! | corpus | live docs | signed | per-level census | deepest |
//! |---|---|---|---|---|
//! | corp2026 | 93 | 52 | `[52,0,0,0,0,0,0,0]` | L1 |
//! | hull2024 | 103 | 72 | `[3,63,69,47,3,0,0,0]` | L5 |
//! | mwgh2025 | 94 | 76 | `[76,58,76,52,0,0,66,0]` | L7 |
//!
//! (censuses over rows with well-formed ladder cells; `hull2024` has two rows in
//! its readable prefix whose ladder payload is fabricated, `tests/signoff_gate.rs`
//! names and excludes them). `AC`/`AU` are populated on real signed rows, not just
//! established by column width: `AU` is a 7-char user id on **every** signed level of
//! every signed document, and `AC` is a 22-char GUID-width token populated
//! all-or-nothing per document.
//!
//! Out of scope: `SH.SD1..SD8` (empty in every corpus, a *separate*
//! signoff-required/schedule family, F47 residual) and [`SignoffBook`] does not
//! project `SH.S1..S8` — that family is an **offset mirror of `AU`**
//! (`S{n} == AU{n+1}` on 45 of hull2024's 58 authentic cells, never the same level,
//! 13 free-text annotations), not a parallel ladder; see the gate for the census.
//! Also out of scope: the `HI.MILESTONE` blob and any password/identity
//! enforcement (security lane).

use std::path::Path;

use cw_core::dbf::Table;
use cw_core::guid::Rng;
use cw_core::{CoreError, Result};

use crate::history::{HistoryKind, HistoryLog};

/// Signoff levels carried by one `SH` row (COM `SignoffInitials(1..8)`).
pub const SIGNOFF_LEVELS: u8 = 8;
/// Last preparer level (COM `Role(1..3)` = "Prepared by 1..3", F47).
pub const PREPARED_LAST: u8 = 3;
/// First reviewer level (COM `Role(4..8)` = "Reviewed by 1..5", F47).
pub const REVIEWED_FIRST: u8 = 4;

/// COM `Role(r)` names, indexed by `level - 1` (F47).
pub const ROLE_NAMES: [&str; SIGNOFF_LEVELS as usize] = [
    "Prepared by 1",
    "Prepared by 2",
    "Prepared by 3",
    "Reviewed by 1",
    "Reviewed by 2",
    "Reviewed by 3",
    "Reviewed by 4",
    "Reviewed by 5",
];

/// Which ladder a level belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignoffRole {
    /// Levels 1..=3.
    Prepared(u8),
    /// Levels 4..=8, carrying the `Reviewed by n` ordinal (1..=5).
    Reviewed(u8),
}

/// Validate `level` (1..=8) and classify it.
pub fn role(level: u8) -> Result<SignoffRole> {
    match level {
        1..=PREPARED_LAST => Ok(SignoffRole::Prepared(level)),
        REVIEWED_FIRST..=SIGNOFF_LEVELS => Ok(SignoffRole::Reviewed(level - PREPARED_LAST)),
        _ => Err(CoreError::Format(format!(
            "signoff level {level} outside 1..={SIGNOFF_LEVELS}"
        ))),
    }
}

/// COM `Role(r)` display name (`"Prepared by 1"` .. `"Reviewed by 5"`).
pub fn role_name(level: u8) -> Result<&'static str> {
    role(level)?;
    Ok(ROLE_NAMES[(level - 1) as usize])
}

/// Overall position of one document on the ladder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignoffStage {
    /// No level signed.
    Unsigned,
    /// At least one preparer level (1..=3) signed, no reviewer level.
    Prepared,
    /// At least one reviewer level (4..=8) signed.
    Reviewed,
}

fn cell(table: &Table, row: usize, name: &str) -> String {
    table
        .cell_str(row, name)
        .unwrap_or_default()
        .trim_end_matches([' ', '\u{0}'])
        .to_string()
}

fn trimmed(table: &Table, row: usize, name: &str) -> String {
    cell(table, row, name).trim().to_string()
}

fn flag(table: &Table, row: usize, name: &str) -> bool {
    trimmed(table, row, name).eq_ignore_ascii_case("Y")
}

/// One level of the eight-level ladder, decoded from `A{n}`/`AD{n}`/`AC{n}`/
/// `AU{n}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignoffLevel {
    /// 1..=8 (COM `SignoffInitials` index; see [`role_name`]).
    pub level: u8,
    /// `A{n}` - initials, stored verbatim (corpus rows right-justify to 8).
    pub initials: String,
    /// `AD{n}` - `yyyymmdd`, blank when unsigned.
    pub date: String,
    /// `AC{n}` - 22-char companion slot (GUID-shaped in corp2026); semantics
    /// unverified, carried verbatim.
    pub ac: String,
    /// `AU{n}` - 22-char companion slot (`'E120235'` user id in corp2026);
    /// semantics unverified, carried verbatim.
    pub au: String,
}

impl SignoffLevel {
    pub fn new(level: u8) -> Result<SignoffLevel> {
        role(level)?;
        Ok(SignoffLevel {
            level,
            initials: String::new(),
            date: String::new(),
            ac: String::new(),
            au: String::new(),
        })
    }

    /// Signed when either the initials or the date cell is non-empty.
    pub fn is_signed(&self) -> bool {
        !self.initials.trim().is_empty() || !self.date.trim().is_empty()
    }

    /// Trimmed initials (corpus stores `"       N"`).
    pub fn initials_text(&self) -> &str {
        self.initials.trim()
    }

    /// `AU{n}` trimmed - the observed user id (`'E120235'`).
    pub fn user(&self) -> &str {
        self.au.trim()
    }

    /// `AC{n}` trimmed - the observed 22-char companion token.
    pub fn signature_guid(&self) -> &str {
        self.ac.trim()
    }

    pub fn role(&self) -> SignoffRole {
        role(self.level).expect("decoded level is in range")
    }

    pub fn role_name(&self) -> &'static str {
        ROLE_NAMES[(self.level - 1) as usize]
    }
}

/// One `SH` row's eight-level signoff state plus its document identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignoffSheet {
    /// `SH.CWGUID` (document id; COM `ItemIdentifier`).
    pub guid: String,
    /// `SH.PRG` (`"ac"` for the engagement program family).
    pub prg: String,
    /// `SH.SCHEDULE` (`"0400 (M1)"`, `"0272"`, empty on headings).
    pub schedule: String,
    /// `SH.NAME` (max 2 chars).
    pub name: String,
    /// `SH.SCH_DESC` (document title).
    pub title: String,
    /// `SH.TYPE` (`*`/`H`/`A`/`F`/`M`; only `F` is a response form).
    pub doc_type: String,
    /// `SH.SIGN='Y'` (COM `doc.Sign`, F47).
    pub sign: bool,
    /// `SH.LOCKDOWN='Y'` (COM `doc.Lockdown`, F47/F68).
    pub locked_down: bool,
    /// Levels 1..=8 in order.
    pub levels: [SignoffLevel; SIGNOFF_LEVELS as usize],
}

impl SignoffSheet {
    /// Decode levels 1..=8 from one physical `SH` row.
    pub fn from_sh_row(table: &Table, row: usize) -> Result<SignoffSheet> {
        let mut levels: [SignoffLevel; SIGNOFF_LEVELS as usize] =
            std::array::from_fn(|i| SignoffLevel {
                level: (i + 1) as u8,
                initials: String::new(),
                date: String::new(),
                ac: String::new(),
                au: String::new(),
            });
        for lvl in levels.iter_mut() {
            let n = lvl.level;
            lvl.initials = cell(table, row, &format!("A{n}"));
            lvl.date = trimmed(table, row, &format!("AD{n}"));
            lvl.ac = trimmed(table, row, &format!("AC{n}"));
            lvl.au = trimmed(table, row, &format!("AU{n}"));
        }
        Ok(SignoffSheet {
            guid: trimmed(table, row, "CWGUID"),
            prg: trimmed(table, row, "PRG"),
            schedule: trimmed(table, row, "SCHEDULE"),
            name: trimmed(table, row, "NAME"),
            title: trimmed(table, row, "SCH_DESC"),
            doc_type: trimmed(table, row, "TYPE"),
            sign: flag(table, row, "SIGN"),
            locked_down: flag(table, row, "LOCKDOWN"),
            levels,
        })
    }

    /// True when this row is a CaseView form (the WPG response-document class).
    pub fn is_response_document(&self) -> bool {
        self.doc_type == "F"
    }

    pub fn level(&self, level: u8) -> Result<&SignoffLevel> {
        role(level)?;
        Ok(&self.levels[(level - 1) as usize])
    }

    /// Signed levels, ascending.
    pub fn signed_levels(&self) -> Vec<u8> {
        self.levels
            .iter()
            .filter(|l| l.is_signed())
            .map(|l| l.level)
            .collect()
    }

    /// Number of signed levels (0..=8).
    pub fn signed_count(&self) -> usize {
        self.levels.iter().filter(|l| l.is_signed()).count()
    }

    pub fn is_signed(&self, level: u8) -> Result<bool> {
        Ok(self.level(level)?.is_signed())
    }

    /// Highest signed level, if any.
    pub fn highest_signed(&self) -> Option<u8> {
        self.levels
            .iter()
            .rev()
            .find(|l| l.is_signed())
            .map(|l| l.level)
    }

    /// First unsigned level in document order (the next required signoff).
    pub fn next_level(&self) -> Option<u8> {
        self.levels.iter().find(|l| !l.is_signed()).map(|l| l.level)
    }

    /// All eight levels signed.
    pub fn is_complete(&self) -> bool {
        self.signed_count() == SIGNOFF_LEVELS as usize
    }

    pub fn stage(&self) -> SignoffStage {
        let reviewed = self.levels[PREPARED_LAST as usize..]
            .iter()
            .any(|l| l.is_signed());
        if reviewed {
            return SignoffStage::Reviewed;
        }
        let prepared = self.levels[..PREPARED_LAST as usize]
            .iter()
            .any(|l| l.is_signed());
        if prepared {
            SignoffStage::Prepared
        } else {
            SignoffStage::Unsigned
        }
    }

    /// Reviewers still missing after the preparer ladder is complete (the
    /// `ICWSignoffRequirements.Role{1..8}Required` surface is caller-side; this
    /// is the observed state only).
    pub fn open_review_levels(&self) -> Vec<u8> {
        self.levels[PREPARED_LAST as usize..]
            .iter()
            .filter(|l| !l.is_signed())
            .map(|l| l.level)
            .collect()
    }
}

/// Widths of the four per-level column families (inventory `dbf-schema.tsv`).
const LEVEL_WIDTHS: [(&str, u8, u8); 4] = [("A", b'C', 8), ("AD", b'D', 8), ("AC", b'C', 22), ("AU", b'C', 22)];

fn check_schema(table: &Table) -> Result<()> {
    for required in ["CWGUID", "PRG", "SCHEDULE", "NAME", "TYPE", "SCH_DESC", "SIGN", "LOCKDOWN"] {
        if table.field_index(required).is_none() {
            return Err(CoreError::Format(format!(
                "SH signoff: missing column {required}"
            )));
        }
    }
    for n in 1..=SIGNOFF_LEVELS {
        for (prefix, kind, len) in LEVEL_WIDTHS {
            let name = format!("{prefix}{n}");
            let idx = table.field_index(&name).ok_or_else(|| {
                CoreError::Format(format!("SH signoff: missing column {name}"))
            })?;
            let f = &table.fields[idx];
            if f.kind != kind || f.len != len || f.dec != 0 {
                return Err(CoreError::Format(format!(
                    "SH signoff: schema drift at {name}: got {} {}{}.{}, want {} {}{}.0",
                    f.name,
                    f.kind as char,
                    f.len,
                    f.dec,
                    name,
                    kind as char,
                    len
                )));
            }
        }
    }
    Ok(())
}

/// Write one cell into a record, width- and type-checked (`N`/`D` right-align,
/// the shape used by `program.rs` and the CW writers).
fn put(rec: &mut [u8], fields: &[cw_core::dbf::Field], name: &str, value: &str) -> Result<()> {
    let (idx, field) = fields
        .iter()
        .enumerate()
        .find(|(_, f)| f.name == name)
        .ok_or_else(|| CoreError::Format(format!("SH signoff: no column {name}")))?;
    if value.len() > field.len as usize {
        return Err(CoreError::Format(format!(
            "SH signoff: {name} value {} chars > width {}",
            value.len(),
            field.len
        )));
    }
    let mut start = 1usize;
    for f in &fields[..idx] {
        start += f.len as usize;
    }
    let width = field.len as usize;
    let cell = &mut rec[start..start + width];
    for b in cell.iter_mut() {
        *b = b' ';
    }
    let bytes = value.as_bytes();
    if field.kind == b'N' || field.kind == b'D' {
        let padded = width.saturating_sub(bytes.len());
        cell[padded..].copy_from_slice(bytes);
    } else {
        cell[..bytes.len()].copy_from_slice(bytes);
    }
    Ok(())
}

fn check_date(date: &str) -> Result<()> {
    if date.is_empty() {
        return Ok(());
    }
    if date.len() != 8 || !date.bytes().all(|b| b.is_ascii_digit()) {
        return Err(CoreError::Format(format!(
            "signoff date {date:?} is not yyyymmdd"
        )));
    }
    Ok(())
}

/// Per-level census over a whole book.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignoffCensus {
    /// Live `SH` rows carrying a signoff block.
    pub documents: usize,
    /// Documents with at least one signed level.
    pub signed_documents: usize,
    /// Documents signed at all eight levels.
    pub complete_documents: usize,
    /// Signed-document count per level (index 0 = level 1).
    pub level_counts: [usize; SIGNOFF_LEVELS as usize],
}

/// The eight-level signoff layer over one `SH` table.
#[derive(Clone, Debug)]
pub struct SignoffBook {
    sh: Table,
    sheets: Vec<SignoffSheet>,
    /// Physical row index for each entry in `sheets` (1:1).
    rows: Vec<usize>,
}

impl SignoffBook {
    pub fn from_bytes(bytes: &[u8]) -> Result<SignoffBook> {
        Self::from_table(Table::parse(bytes)?)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<SignoffBook> {
        Self::from_bytes(&std::fs::read(path)?)
    }

    pub fn from_table(table: Table) -> Result<SignoffBook> {
        check_schema(&table)?;
        let mut sheets = Vec::new();
        let mut rows = Vec::new();
        for r in 0..table.count() {
            if table.is_deleted(r) {
                continue;
            }
            sheets.push(SignoffSheet::from_sh_row(&table, r)?);
            rows.push(r);
        }
        Ok(SignoffBook { sh: table, sheets, rows })
    }

    /// Live documents in physical `SH` order (CW document-tree order).
    pub fn sheets(&self) -> &[SignoffSheet] {
        &self.sheets
    }

    pub fn count(&self) -> usize {
        self.sheets.len()
    }

    pub fn index_of(&self, guid: &str) -> Option<usize> {
        self.sheets
            .iter()
            .position(|s| s.guid.trim() == guid.trim())
    }

    pub fn sheet(&self, guid: &str) -> Option<&SignoffSheet> {
        self.index_of(guid).map(|i| &self.sheets[i])
    }

    /// The WPG response documents: live `SH.TYPE='F'` CaseView forms, which
    /// are the signoff carriers (F30). Ordered by `(SCHEDULE, title)` like
    /// [`crate::program::ResponseDocs`].
    pub fn wpg_documents(&self) -> Vec<&SignoffSheet> {
        let mut out: Vec<&SignoffSheet> = self
            .sheets
            .iter()
            .filter(|s| s.is_response_document())
            .collect();
        out.sort_by(|a, b| {
            (
                a.schedule.split_whitespace().next().unwrap_or(""),
                &a.title,
                &a.guid,
            )
                .cmp(&(
                    b.schedule.split_whitespace().next().unwrap_or(""),
                    &b.title,
                    &b.guid,
                ))
        });
        out
    }

    /// Census over every live `SH` row.
    pub fn census(&self) -> SignoffCensus {
        let mut c = SignoffCensus {
            documents: self.sheets.len(),
            ..Default::default()
        };
        for s in &self.sheets {
            let signed = s.signed_levels();
            if signed.is_empty() {
                continue;
            }
            c.signed_documents += 1;
            if s.is_complete() {
                c.complete_documents += 1;
            }
            for l in signed {
                c.level_counts[(l - 1) as usize] += 1;
            }
        }
        c
    }

    /// Census restricted to the WPG response-document class.
    pub fn wpg_census(&self) -> SignoffCensus {
        let mut c = SignoffCensus::default();
        for s in self.wpg_documents() {
            c.documents += 1;
            let signed = s.signed_levels();
            if signed.is_empty() {
                continue;
            }
            c.signed_documents += 1;
            if s.is_complete() {
                c.complete_documents += 1;
            }
            for l in signed {
                c.level_counts[(l - 1) as usize] += 1;
            }
        }
        c
    }

    /// `doc.SignoffInitials(r)=..` + `SignoffDate(r)=..` put (F47): write
    /// `A{r}`/`AD{r}` and, when supplied, the companion `AC{r}`/`AU{r}` slots.
    /// Does **not** append a `HI` row - use [`SignoffBook::sign_off`] for the
    /// audited path.
    pub fn set_signoff(
        &mut self,
        guid: &str,
        level: u8,
        initials: &str,
        date: &str,
        ac: Option<&str>,
        au: Option<&str>,
    ) -> Result<()> {
        role(level)?;
        check_date(date)?;
        let sheet = self
            .index_of(guid)
            .ok_or_else(|| CoreError::Format(format!("SH signoff: no document {guid}")))?;
        let row = self.rows[sheet];
        let fields = self.sh.fields.clone();
        {
            let rec = self
                .sh
                .records
                .get_mut(row)
                .ok_or_else(|| CoreError::Format(format!("SH signoff: row {row} missing")))?;
            put(rec, &fields, &format!("A{level}"), initials)?;
            put(rec, &fields, &format!("AD{level}"), date)?;
            if let Some(v) = ac {
                put(rec, &fields, &format!("AC{level}"), v)?;
            }
            if let Some(v) = au {
                put(rec, &fields, &format!("AU{level}"), v)?;
            }
        }
        self.sheets[sheet] = SignoffSheet::from_sh_row(&self.sh, row)?;
        Ok(())
    }

    /// Clear levels 1..=8 (`A/AD/AC/AU`), the year-end reset law (F65).
    pub fn clear_signoffs(&mut self, guid: &str) -> Result<()> {
        let sheet = self
            .index_of(guid)
            .ok_or_else(|| CoreError::Format(format!("SH signoff: no document {guid}")))?;
        let row = self.rows[sheet];
        let fields = self.sh.fields.clone();
        {
            let rec = self
                .sh
                .records
                .get_mut(row)
                .ok_or_else(|| CoreError::Format(format!("SH signoff: row {row} missing")))?;
            for level in 1..=SIGNOFF_LEVELS {
                for prefix in ["A", "AD", "AC", "AU"] {
                    put(rec, &fields, &format!("{prefix}{level}"), "")?;
                }
            }
        }
        self.sheets[sheet] = SignoffSheet::from_sh_row(&self.sh, row)?;
        Ok(())
    }

    /// `doc.Sign` (F47): `SH.SIGN='Y'`, `false` writes `'N'`.
    pub fn set_sign(&mut self, guid: &str, sign: bool) -> Result<()> {
        self.set_flag(guid, "SIGN", sign)
    }

    /// `doc.Lockdown` (F47/F68): `SH.LOCKDOWN='Y'`, `false` writes `'N'`.
    pub fn set_lockdown(&mut self, guid: &str, locked: bool) -> Result<()> {
        self.set_flag(guid, "LOCKDOWN", locked)
    }

    fn set_flag(&mut self, guid: &str, column: &str, on: bool) -> Result<()> {
        let sheet = self
            .index_of(guid)
            .ok_or_else(|| CoreError::Format(format!("SH signoff: no document {guid}")))?;
        let row = self.rows[sheet];
        let fields = self.sh.fields.clone();
        let value = if on { "Y" } else { "N" };
        {
            let rec = self
                .sh
                .records
                .get_mut(row)
                .ok_or_else(|| CoreError::Format(format!("SH signoff: row {row} missing")))?;
            put(rec, &fields, column, value)?;
        }
        self.sheets[sheet] = SignoffSheet::from_sh_row(&self.sh, row)?;
        Ok(())
    }

    /// Audited signoff put: writes `SH.A{r}`/`AD{r}` (and optional companion
    /// slots) and appends the `HI` `DOCS/DOCSIGN{r}` row CW writes on every
    /// signoff put (F47/F67/F68).
    ///
    /// `version` is the `version` extra CW carries on signoff rows (F67); its
    /// value semantics are unobserved, so `None` omits it.
    #[allow(clippy::too_many_arguments)]
    pub fn sign_off(
        &mut self,
        history: &mut HistoryLog,
        guid: &str,
        level: u8,
        initials: &str,
        date: &str,
        user: &str,
        version: Option<&str>,
        checksum: &str,
        stamp: &str,
        rng: &mut Rng,
    ) -> Result<()> {
        self.set_signoff(guid, level, initials, date, None, None)?;
        let extra: Vec<(&str, &str)> = version.map(|v| ("version", v)).into_iter().collect();
        history.record_document_event(
            HistoryKind::Signoff(level),
            guid,
            user,
            initials.trim(),
            &extra,
            checksum,
            stamp,
            rng,
        )?;
        Ok(())
    }

    /// In-memory save image (`SH.dbf` bytes). A no-op save is byte-identical.
    pub fn encode(&self) -> Vec<u8> {
        self.sh.encode()
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        self.sh.write_path(path)
    }

    pub fn table(&self) -> &Table {
        &self.sh
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_split_three_and_five() {
        assert_eq!(role_name(1).unwrap(), "Prepared by 1");
        assert_eq!(role_name(3).unwrap(), "Prepared by 3");
        assert_eq!(role_name(4).unwrap(), "Reviewed by 1");
        assert_eq!(role_name(8).unwrap(), "Reviewed by 5");
        assert_eq!(role(4).unwrap(), SignoffRole::Reviewed(1));
        assert!(role_name(0).is_err());
        assert!(role_name(9).is_err());
        assert!(SignoffLevel::new(9).is_err());
    }

    #[test]
    fn date_shape_is_checked() {
        assert!(check_date("20261023").is_ok());
        assert!(check_date("").is_ok());
        assert!(check_date("2026-10-23").is_err());
        assert!(check_date("2026102").is_err());
    }

    #[test]
    fn stage_follows_ladder() {
        let mut sheet = SignoffSheet {
            guid: "g".into(),
            prg: "ac".into(),
            schedule: String::new(),
            name: String::new(),
            title: String::new(),
            doc_type: "F".into(),
            sign: false,
            locked_down: false,
            levels: std::array::from_fn(|i| SignoffLevel {
                level: (i + 1) as u8,
                initials: String::new(),
                date: String::new(),
                ac: String::new(),
                au: String::new(),
            }),
        };
        assert_eq!(sheet.stage(), SignoffStage::Unsigned);
        assert_eq!(sheet.next_level(), Some(1));
        sheet.levels[0].initials = "N".into();
        assert_eq!(sheet.stage(), SignoffStage::Prepared);
        assert_eq!(sheet.next_level(), Some(2));
        assert_eq!(sheet.open_review_levels(), vec![4, 5, 6, 7, 8]);
        sheet.levels[3].date = "20261023".into();
        assert_eq!(sheet.stage(), SignoffStage::Reviewed);
        assert_eq!(sheet.highest_signed(), Some(4));
        assert!(!sheet.is_complete());
    }
}

