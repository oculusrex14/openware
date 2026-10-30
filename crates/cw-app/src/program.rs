//! cw-app review programs & checklists - the `PR` table and response documents
//! (P5.G G1).
//!
//! Contract: `docs\specs\program.md` v1.0. Ground truth for the physical layout
//! is `inventory\dbf-schema.tsv` (hull2024 + corp2026 + mwgh2025 all carry the
//! same 33-field / 297-byte `PR` table, 0 records in every frozen corpus).
//! The COM surface is `ICWProgramChecklistItems` / `ICWProgramChecklistItem`
//! (`inventory\com-model\tlb-01-cw42.dll.md` §ICWProgramChecklistItem):
//!
//! | COM member | `PR` column | note |
//! |---|---|---|
//! | `Procedures` | `TEXT` | the procedure / checklist question |
//! | `Response` (VT_UI4) | `CHECK` | response code, rendered as text |
//! | `Explanation` | `EXPL` | explanation for the response |
//! | `Initials` | `A1` | first signoff/initial cell (role column family A1..A7) |
//! | `Reference` (variant) | `WP_REF` / `AUTH_REF` | workpaper / program reference |
//! | `Staff` | `STAFF` | assigned staff id |
//! | `Time` / `EstTime` | `ACTUAL` / `BUDGET` | decimal hours |
//! | `Completeness`..`Presentation` | `M1..M4` + assertion initials | assertion flags |
//! | `ItemIdentifier` | `CWGUID` | row id |
//! | `ItemLastChange` | `CWDATE` | audit stamp |
//!
//! The `A1..A7` / `M1..M4` split is carried structurally: `PR` has no field
//! names beyond those cells, and the corpus has no populated row that would
//! let the mapping to individual assertion properties be confirmed. The reader
//! therefore exposes both families verbatim plus the COM-shaped accessors that
//! are established by the typelib (`response`, `explanation`, `initials`, `staff`,
//! `time`, `est_time`). `PROCEDURES` text and all other cells round-trip
//! byte-identically.
//!
//! Response documents
//! ------------------
//! In the frozen corpora every `PR` table is empty; program/checklist content
//! lives in CaseView **form** documents (`SH.TYPE='F'`). The program document
//! in the review corpus is `0400 Program for Review Engagements`
//! (`CVGROUP=''`, `CVID='04000RG2'`), and `0272 Checklist for Disclosures`,
//! `0120`/`0126`/`0300` questionnaires are the same class. [`ResponseDocs`] is
//! the `SH` projection of exactly that class (live `TYPE='F'` rows), and
//! [`ResponseDocs::resolve`] links a `PR` row to its response document by
//! `DOCGUID` first, then by `WP_REF`/`AUTH_REF` against the document schedule.
//!
//! Out of scope (documented in the spec): decoding the CaseView response
//! **cells** (cw-cv `Strings` codec is still open - F79 residual), `PR.MEMO` /
//! `MEMO_NO` FPT payloads, and `Annotations` (AN lane). The writer never
//! invents a response; it only stores an explicitly supplied code.

use std::collections::BTreeMap;

use cw_core::dbf::Table;
use cw_core::guid::{CwGuid, Rng};
use cw_core::{CoreError, Result};

/// Physical `PR` field count (all three corpus schemas).
pub const PR_FIELD_COUNT: usize = 33;
/// Physical `PR` record length, deletion flag included.
pub const PR_RECORD_LEN: usize = 297;
/// dBase live/deleted record flags.
const LIVE: u8 = b' ';
const DELETED: u8 = b'*';

/// One field descriptor of the canonical `PR` layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrField {
    pub name: &'static str,
    /// dBase type letter (`C`/`N`/`D`/`M`).
    pub kind: u8,
    pub len: u8,
    pub dec: u8,
}

const fn f(name: &'static str, kind: u8, len: u8, dec: u8) -> PrField {
    PrField { name, kind, len, dec }
}

/// Canonical `PR` schema in physical order (33 columns, inventory F25).
pub static PR_SCHEMA: [PrField; PR_FIELD_COUNT] = [
    f("LINENO", b'N', 8, 0),
    f("DOCGUID", b'C', 22, 0),
    f("NO", b'N', 3, 0),
    f("TYPE", b'C', 2, 0),
    f("TEXT", b'C', 60, 0),
    f("DONE_BY", b'C', 4, 0),
    f("DATE", b'D', 8, 0),
    f("AUTH_PRG", b'C', 2, 0),
    f("AUTH_REF", b'C', 10, 0),
    f("WP_REF", b'C', 40, 0),
    f("CHECK", b'C', 2, 0),
    f("EXPL", b'C', 30, 0),
    f("MEMO_NO", b'C', 2, 0),
    f("A1", b'C', 1, 0),
    f("A2", b'C', 1, 0),
    f("A3", b'C', 1, 0),
    f("A4", b'C', 1, 0),
    f("A5", b'C', 1, 0),
    f("A6", b'C', 1, 0),
    f("A7", b'C', 1, 0),
    f("STAFF", b'C', 4, 0),
    f("BUDGET", b'N', 6, 2),
    f("ACTUAL", b'N', 6, 2),
    f("PRIOR", b'N', 6, 2),
    f("SCORE", b'N', 6, 2),
    f("M1", b'C', 2, 0),
    f("M2", b'C', 2, 0),
    f("M3", b'C', 2, 0),
    f("M4", b'C', 2, 0),
    f("MEMO", b'M', 10, 0),
    f("CWGUID", b'C', 22, 0),
    f("CWDATE", b'C', 14, 0),
    f("CREATEDON", b'C', 14, 0),
];

fn number(cell: &[u8]) -> Option<f64> {
    let s = String::from_utf8_lossy(cell);
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        s.parse::<f64>().ok()
    }
}

fn int(cell: &[u8]) -> i64 {
    let s = String::from_utf8_lossy(cell);
    s.trim().parse::<i64>().unwrap_or(0)
}

/// One `PR` row decoded into the COM `ICWProgramChecklistItem` shape.
#[derive(Clone, Debug, PartialEq)]
pub struct ChecklistItem {
    pub lineno: i64,
    /// Owning program/checklist document (`SH.CWGUID`).
    pub doc_guid: String,
    /// 1-based item number inside the owning document.
    pub no: i32,
    /// `PR.TYPE` (2 chars; item class, carried verbatim).
    pub item_type: String,
    /// COM `Procedures`.
    pub text: String,
    pub done_by: String,
    /// `yyyymmdd`, blank when unanswered.
    pub date: String,
    pub auth_program: String,
    pub auth_ref: String,
    /// COM `Reference` target (workpaper / program reference).
    pub wp_ref: String,
    /// COM `Response` code as stored text (empty = unanswered).
    pub check: String,
    /// COM `Explanation`.
    pub explanation: String,
    pub memo_no: String,
    /// `A1..A7` initials family, verbatim.
    pub assertion_initials: [String; 7],
    /// COM `Staff`.
    pub staff: String,
    /// COM `EstTime` (hours).
    pub budget: Option<f64>,
    /// COM `Time` (hours).
    pub actual: Option<f64>,
    pub prior: Option<f64>,
    pub score: Option<f64>,
    /// `M1..M4` assertion/response cells, verbatim.
    pub milestones: [String; 4],
    /// Raw `MEMO` pointer cell (FPT payload is out of scope v1).
    pub memo: String,
    /// COM `ItemIdentifier`.
    pub cwguid: String,
    /// COM `ItemLastChange`.
    pub cw_date: String,
    pub created_on: String,
    /// Physical tombstone (`*`) flag; `items()` skips these.
    pub deleted: bool,
}

impl ChecklistItem {
    /// Decode one row. Missing columns are a format error (schema drift is
    /// rejected up front by [`ProgramChecklist::from_table`]).
    pub fn decode(table: &Table, row: usize) -> Result<ChecklistItem> {
        let c = |name: &str| -> Result<String> {
            table
                .cell_str(row, name)
                .map(|s| s.trim_end_matches([' ', '\u{0}']).to_string())
                .ok_or_else(|| CoreError::Format(format!("PR: missing column {name}")))
        };
        let opt = |name: &str| -> Result<Option<f64>> {
            let cell = table
                .cell(row, name)
                .ok_or_else(|| CoreError::Format(format!("PR: missing column {name}")))?;
            Ok(number(cell))
        };
        let arr7 = |prefix: &str, n: usize| -> Result<Vec<String>> {
            (1..=n)
                .map(|i| c(&format!("{prefix}{i}")))
                .collect::<Result<Vec<_>>>()
        };
        let a: Vec<String> = arr7("A", 7)?;
        let m: Vec<String> = arr7("M", 4)?;
        let mut assertion_initials: [String; 7] = std::array::from_fn(|_| String::new());
        for (dst, src) in assertion_initials.iter_mut().zip(a) {
            *dst = src;
        }
        let mut milestones: [String; 4] = std::array::from_fn(|_| String::new());
        for (dst, src) in milestones.iter_mut().zip(m) {
            *dst = src;
        }
        Ok(ChecklistItem {
            lineno: int(table.cell(row, "LINENO").unwrap_or(b"")),
            doc_guid: c("DOCGUID")?,
            no: int(table.cell(row, "NO").unwrap_or(b"")) as i32,
            item_type: c("TYPE")?,
            text: c("TEXT")?,
            done_by: c("DONE_BY")?,
            date: c("DATE")?,
            auth_program: c("AUTH_PRG")?,
            auth_ref: c("AUTH_REF")?,
            wp_ref: c("WP_REF")?,
            check: c("CHECK")?,
            explanation: c("EXPL")?,
            memo_no: c("MEMO_NO")?,
            assertion_initials,
            staff: c("STAFF")?,
            budget: opt("BUDGET")?,
            actual: opt("ACTUAL")?,
            prior: opt("PRIOR")?,
            score: opt("SCORE")?,
            milestones,
            memo: c("MEMO")?,
            cwguid: c("CWGUID")?,
            cw_date: c("CWDATE")?,
            created_on: c("CREATEDON")?,
            deleted: table.is_deleted(row),
        })
    }

    /// COM `Response` as a numeric code when the stored `CHECK` is numeric.
    pub fn response_code(&self) -> Option<u32> {
        self.check.trim().parse::<u32>().ok()
    }

    /// True when a response was recorded (`CHECK` non-empty).
    pub fn is_answered(&self) -> bool {
        !self.check.trim().is_empty()
    }

    /// First signoff initial (COM `Initials` mapping candidate, see module docs).
    pub fn initials(&self) -> &str {
        self.assertion_initials[0].trim()
    }
}

fn check_schema(table: &Table) -> Result<()> {
    if table.fields.len() != PR_FIELD_COUNT {
        return Err(CoreError::Format(format!(
            "PR: {} fields, expected {PR_FIELD_COUNT}",
            table.fields.len()
        )));
    }
    if table.record_len as usize != PR_RECORD_LEN {
        return Err(CoreError::Format(format!(
            "PR: record length {}, expected {PR_RECORD_LEN}",
            table.record_len
        )));
    }
    for (i, want) in PR_SCHEMA.iter().enumerate() {
        let got = &table.fields[i];
        if got.name != want.name || got.kind != want.kind || got.len != want.len || got.dec != want.dec
        {
            return Err(CoreError::Format(format!(
                "PR: schema drift at {i}: got {} {}{}.{}, want {} {}{}.{}",
                got.name, got.kind as char, got.len, got.dec, want.name, want.kind as char, want.len,
                want.dec
            )));
        }
    }
    Ok(())
}

fn put(rec: &mut [u8], fields: &[cw_core::dbf::Field], name: &str, value: &str) -> Result<()> {
    let (idx, field) = fields
        .iter()
        .enumerate()
        .find(|(_, f)| f.name == name)
        .ok_or_else(|| CoreError::Format(format!("PR: no column {name}")))?;
    if value.len() > field.len as usize {
        return Err(CoreError::Format(format!(
            "PR: {name} value {} chars > width {}",
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

/// The `doc.ProgramChecklist` collection over one `PR` table.
#[derive(Clone, Debug)]
pub struct ProgramChecklist {
    table: Table,
    items: Vec<ChecklistItem>,
    /// Physical row index for each entry in `items` (1:1).
    rows: Vec<usize>,
}

impl ProgramChecklist {
    /// Parse `PR.dbf` bytes and decode every live item.
    pub fn from_bytes(bytes: &[u8]) -> Result<ProgramChecklist> {
        Self::from_table(Table::parse(bytes)?)
    }

    pub fn open(path: impl AsRef<std::path::Path>) -> Result<ProgramChecklist> {
        Self::from_bytes(&std::fs::read(path)?)
    }

    pub fn from_table(table: Table) -> Result<ProgramChecklist> {
        check_schema(&table)?;
        let mut items = Vec::new();
        let mut rows = Vec::new();
        for r in 0..table.count() {
            if table.is_deleted(r) {
                continue;
            }
            items.push(ChecklistItem::decode(&table, r)?);
            rows.push(r);
        }
        Ok(ProgramChecklist { table, items, rows })
    }

    /// Live items in physical row order (CW stores them document-ordered).
    pub fn items(&self) -> &[ChecklistItem] {
        &self.items
    }

    /// `ICWProgramChecklistItems.Count`.
    pub fn count(&self) -> usize {
        self.items.len()
    }

    /// Items of one owning document, ordered by `NO` (COM `Item(i)` order).
    pub fn items_for(&self, doc_guid: &str) -> Vec<&ChecklistItem> {
        let mut v: Vec<&ChecklistItem> = self
            .items
            .iter()
            .filter(|i| i.doc_guid.trim() == doc_guid.trim())
            .collect();
        v.sort_by_key(|i| (i.no, i.lineno));
        v
    }

    /// Grouped view by owning document.
    pub fn grouped(&self) -> BTreeMap<&str, Vec<&ChecklistItem>> {
        let mut out: BTreeMap<&str, Vec<&ChecklistItem>> = BTreeMap::new();
        for item in &self.items {
            out.entry(item.doc_guid.as_str()).or_default().push(item);
        }
        for v in out.values_mut() {
            v.sort_by_key(|i| (i.no, i.lineno));
        }
        out
    }

    /// `Add(text)`: append one item to `doc_guid`, allocating `NO`, `LINENO`
    /// and the row `CWGUID` (PR has no observed family tag -> allocator base A,
    /// per `cw_core::guid::Family::for_table`).
    ///
    /// `TYPE` is written as `"1"` (checklist line) and `CHECK` stays empty
    /// (unanswered); both are structural defaults, not COM-verified values.
    pub fn add_item(
        &mut self,
        doc_guid: &str,
        text: &str,
        rng: &mut Rng,
        stamp: &str,
    ) -> Result<usize> {
        if doc_guid.trim().is_empty() {
            return Err(CoreError::Format("PR: DOCGUID is mandatory".into()));
        }
        let no = self
            .items
            .iter()
            .filter(|i| i.doc_guid.trim() == doc_guid.trim())
            .map(|i| i.no)
            .max()
            .unwrap_or(0)
            + 1;
        let lineno = self.items.iter().map(|i| i.lineno).max().unwrap_or(0) + 1;
        let guid = CwGuid::allocate_for_table("PR", rng).encode();
        let mut rec = vec![LIVE; PR_RECORD_LEN];
        let fields = self.table.fields.clone();
        put(&mut rec, &fields, "LINENO", &lineno.to_string())?;
        put(&mut rec, &fields, "DOCGUID", doc_guid.trim())?;
        put(&mut rec, &fields, "NO", &no.to_string())?;
        put(&mut rec, &fields, "TYPE", "1")?;
        put(&mut rec, &fields, "TEXT", text)?;
        put(&mut rec, &fields, "CWGUID", &guid)?;
        put(&mut rec, &fields, "CWDATE", stamp)?;
        put(&mut rec, &fields, "CREATEDON", stamp)?;
        let row = self.table.records.len();
        self.table.records.push(rec);
        self.items.push(ChecklistItem::decode(&self.table, row)?);
        self.rows.push(row);
        Ok(self.items.len() - 1)
    }

    /// `RemoveAt(i)` (0-based). Physical tombstone, like CW's table delete.
    pub fn remove_at(&mut self, index: usize) -> Result<()> {
        let row = *self
            .rows
            .get(index)
            .ok_or_else(|| CoreError::Format(format!("PR: RemoveAt index {index} out of range")))?;
        if let Some(rec) = self.table.records.get_mut(row) {
            rec[0] = DELETED;
        }
        if let Some(item) = self.items.get_mut(index) {
            item.deleted = true;
        }
        Ok(())
    }

    /// `RemoveAll()`: tombstone every live item.
    pub fn remove_all(&mut self) {
        for row in self.rows.iter().copied() {
            if let Some(rec) = self.table.records.get_mut(row) {
                rec[0] = DELETED;
            }
        }
        for item in &mut self.items {
            item.deleted = true;
        }
    }

    /// Update the response code (`CHECK`) and/or explanation (`EXPL`) on one
    /// item; both cells are width-validated. `check` is written verbatim (the
    /// COM `Response` enum is a `user` code, so callers pass the numeric text).
    pub fn set_response(
        &mut self,
        index: usize,
        check: Option<&str>,
        explanation: Option<&str>,
    ) -> Result<()> {
        let row = *self
            .rows
            .get(index)
            .ok_or_else(|| CoreError::Format(format!("PR: item {index} out of range")))?;
        let fields = self.table.fields.clone();
        {
            let rec = self
                .table
                .records
                .get_mut(row)
                .ok_or_else(|| CoreError::Format(format!("PR: row {row} missing")))?;
            if let Some(v) = check {
                put(rec, &fields, "CHECK", v)?;
            }
            if let Some(v) = explanation {
                put(rec, &fields, "EXPL", v)?;
            }
        }
        self.items[index] = ChecklistItem::decode(&self.table, row)?;
        Ok(())
    }

    /// In-memory save image (`PR.dbf` bytes). A no-op save is byte-identical.
    pub fn encode(&self) -> Vec<u8> {
        self.table.encode()
    }

    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<()> {
        self.table.write_path(path)
    }

    pub fn table(&self) -> &Table {
        &self.table
    }
}

/// A live CaseView form document from `SH`: the response-document carrier.
#[derive(Clone, Debug, PartialEq)]
pub struct ProgramDocument {
    pub guid: String,
    /// `SH.SCHEDULE` (`"0400 (M1)"`, `"0272"`, empty on headings).
    pub schedule: String,
    /// `SH.NAME` (max 2 chars in the corpus).
    pub name: String,
    /// `SH.SCH_DESC` (document title).
    pub title: String,
    /// `SH.CVGROUP` + `SH.CVID`: the CaseView form identity.
    pub cv_group: String,
    pub cv_id: String,
    pub doc_type: String,
}

impl ProgramDocument {
    /// Schedule token without the milestone suffix / spaces: `"0400 (M1)"` ->
    /// `"0400"`, `"0315.10 (M1)"` -> `"0315.10"`.
    pub fn schedule_key(&self) -> String {
        let s = self.schedule.trim();
        let head = s.split_whitespace().next().unwrap_or("");
        head.to_ascii_uppercase()
    }
}

/// `SH` projection of live CaseView **form** documents (`TYPE='F'`) - programs,
/// checklists and questionnaires. These are the documents a `PR` item's
/// `DOCGUID` / `WP_REF` points at.
#[derive(Clone, Debug, Default)]
pub struct ResponseDocs {
    docs: Vec<ProgramDocument>,
}

impl ResponseDocs {
    pub fn from_sh(table: &Table) -> Result<ResponseDocs> {
        if table.field_index("CWGUID").is_none()
            || table.field_index("TYPE").is_none()
            || table.field_index("SCHEDULE").is_none()
            || table.field_index("CVGROUP").is_none()
            || table.field_index("CVID").is_none()
        {
            return Err(CoreError::Format("SH: response-doc columns missing".into()));
        }
        let get = |r: usize, n: &str| table.cell_str(r, n).unwrap_or_default();
        let mut docs = Vec::new();
        for r in 0..table.count() {
            if table.is_deleted(r) {
                continue;
            }
            if get(r, "TYPE").trim() != "F" {
                continue;
            }
            let doc = ProgramDocument {
                guid: get(r, "CWGUID").trim().to_string(),
                schedule: get(r, "SCHEDULE").trim().to_string(),
                name: get(r, "NAME").trim().to_string(),
                title: get(r, "SCH_DESC").trim().to_string(),
                cv_group: get(r, "CVGROUP").trim().to_string(),
                cv_id: get(r, "CVID").trim().to_string(),
                doc_type: get(r, "TYPE").trim().to_string(),
            };
            if !doc.guid.is_empty() {
                docs.push(doc);
            }
        }
        docs.sort_by(|a, b| {
            (a.schedule_key(), &a.title, &a.guid).cmp(&(b.schedule_key(), &b.title, &b.guid))
        });
        Ok(ResponseDocs { docs })
    }

    pub fn documents(&self) -> &[ProgramDocument] {
        &self.docs
    }

    pub fn by_guid(&self, guid: &str) -> Option<&ProgramDocument> {
        self.docs.iter().find(|d| d.guid.trim() == guid.trim())
    }

    /// Resolve a checklist row to its response document: exact `DOCGUID`
    /// first, then `WP_REF`/`AUTH_REF` against the schedule token.
    pub fn resolve(&self, item: &ChecklistItem) -> Option<&ProgramDocument> {
        if let Some(d) = self.by_guid(&item.doc_guid) {
            return Some(d);
        }
        for r in [item.wp_ref.trim(), item.auth_ref.trim()] {
            if r.is_empty() {
                continue;
            }
            let key = r
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_uppercase();
            if let Some(d) = self
                .docs
                .iter()
                .find(|d| !d.schedule_key().is_empty() && d.schedule_key() == key)
            {
                return Some(d);
            }
        }
        None
    }
}

/// One response document plus its checklist progress.
#[derive(Clone, Debug, PartialEq)]
pub struct ResponseView {
    /// `PR.DOCGUID` (always present, even when `SH` has no matching row).
    pub doc_guid: String,
    /// Resolved `SH` form document, when the register still carries it.
    pub document: Option<ProgramDocument>,
    pub total: usize,
    pub answered: usize,
}

impl ProgramChecklist {
    /// One [`ResponseView`] per owning document, ordered by document schedule.
    pub fn response_views(&self, docs: &ResponseDocs) -> Vec<ResponseView> {
        let mut views: Vec<ResponseView> = Vec::new();
        for (guid, items) in self.grouped() {
            views.push(ResponseView {
                doc_guid: guid.to_string(),
                document: docs.by_guid(guid).cloned(),
                total: items.len(),
                answered: items.iter().filter(|i| i.is_answered()).count(),
            });
        }
        views.sort_by(|a, b| {
            let ka = a
                .document
                .as_ref()
                .map(|d| (d.schedule_key(), d.title.clone()))
                .unwrap_or_default();
            let kb = b
                .document
                .as_ref()
                .map(|d| (d.schedule_key(), d.title.clone()))
                .unwrap_or_default();
            (ka, &a.doc_guid).cmp(&(kb, &b.doc_guid))
        });
        views
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_33_columns() {
        assert_eq!(PR_SCHEMA.len(), PR_FIELD_COUNT);
        let sum: usize = PR_SCHEMA.iter().map(|f| f.len as usize).sum();
        assert_eq!(sum + 1, PR_RECORD_LEN);
    }

    #[test]
    fn schedule_key_trims_milestone() {
        let d = ProgramDocument {
            guid: "x".into(),
            schedule: "0315.10 (M1)".into(),
            name: String::new(),
            title: String::new(),
            cv_group: String::new(),
            cv_id: String::new(),
            doc_type: "F".into(),
        };
        assert_eq!(d.schedule_key(), "0315.10");
    }
}

