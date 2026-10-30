//! cw-app review layer - `AN` annotations/tickmarks/references and `TS`
//! issues/review notes (P5.G G3).
//!
//! Contracts:
//! - `docs\specs\annotations.md` v1.0 (fixture-confirmed COM create path + the
//!   MWGH 242-row census, F40/F43).
//! - `inventory\dbf-schema.tsv` for the physical layouts: `AN` = 21 columns /
//!   324-byte records, `TS` = 34 columns / 839-byte records, identical in all
//!   three frozen corpora.
//!
//! ## AN (annotations, tickmarks, references)
//!
//! One row = one review anchor. The anchor is `OBJECTTYPE`+`OBJECTID` (the
//! annotated account/map/tax-code) and/or `DOCUMENTID` (the owning CaseView
//! document). `**` + blank `OBJECTID` is CW's document/global anchor. The link
//! slot `ARGUMENT` is typed:
//!
//! | TYPE | meaning | ARGUMENT | CONTENT |
//! |---|---|---|---|
//! | `A` | highlight annotation | optional 22c token | text |
//! | `N` | note | - | memo block (AN.fpt) |
//! | `T` | tickmark instance | tickmark definition id | - |
//! | `R` | reference | target CWGUID | - |
//! | `I` | issue link | `TS.CWGUID` | memo block |
//! | `U` | user text (manual reference / hyperlink label) | the literal text | memo for hyperlink |
//!
//! Census laws (live rows): mwgh2025 has 242 (A 159 / R 47 / N 19 / T 17),
//! hull2024 373 (A 204 / T 73 / N 47 / R 31 / U 18), corp2026 0 (all 8 rows
//! tombstoned). Tickmark definitions are shared strings: mwgh2025 has two,
//! `OKkh..` x16 and `Hfr..` x1. No live `I` row exists in the frozen corpora
//! (TS is empty in hull/mwgh), so the issue link is exercised on synthetic
//! writes only - residual recorded in `docs\specs\annotations.md` §5.
//!
//! ## TS (issues / review notes)
//!
//! corp2026 is the only corpus with live `TS` rows: 50 physical, 3 deleted,
//! 47 live. Observed domain: `STATUS` `C` x43 (complete, `PERCENT=100`,
//! `COMPLETEON`/`COMPLETEBY` set) and `N` x4 (new, `PERCENT=0`); `PRIORITY=1`
//! throughout; `ROLL`/`ARCHIVE`/`CLEARED` are CHAR `F` (false); `TYPE`,
//! `OBJECTTYPE`, `FILEID` blank; `DOCID` populated on 45/47 and 45/45 resolve
//! to a live `SH.CWGUID` (F60); `ID` is a per-engagement sequence with
//! different prefixes (`VF-2..VF-28`, `MC203-1..MC203-22`) and is not a
//! documented invariant, so the writer only continues the observed prefix.
//!
//! COM surface (F40/F43): writes go through `document.Annotations` factories
//! (`AddNote(text, [pt])`, `AddTickmark(tickmark, [pt])`, `AddReference(...)`,
//! `AddIssue(issue, [pt])`, `AddManualReference(text, [pt])`,
//! `AddHyperlink(label, url, external, [pt])`); `client.Annotations` is a
//! read aggregate and rejects every `Add*` shape. [`AnnotationBook`] mirrors
//! those factories as one [`AnnotationDraft`] writer. `Remove` tombstones the
//! row (unexercised in CW - residual).
//!
//! Audit: `AN` carries `CWDATE`/`CREATEDON`/`ANDATE`/`ANCREADATE` plus the
//! `CWCHECKSUM`/`CONSCHKSUM`/`EXTCHKSUM` triplet (F25); `TS` carries
//! `CWDATED`/`CWDATER` + `HISTORYCRE`/`HISTORYCOM` + the same checksum-less
//! audit shape. Stamps are caller-supplied so golden runs stay deterministic.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use cw_core::dbf::Table;
use cw_core::fpt::Fpt;
use cw_core::guid::{CwGuid, Rng};
use cw_core::{CoreError, Result};

/// Physical `AN` field count / record length (all three corpora).
pub const AN_FIELD_COUNT: usize = 21;
pub const AN_RECORD_LEN: usize = 324;
/// Physical `TS` field count / record length (all three corpora).
pub const TS_FIELD_COUNT: usize = 34;
pub const TS_RECORD_LEN: usize = 839;

const LIVE: u8 = b' ';
const DELETED: u8 = b'*';

/// `AN` schema in physical order.
pub static AN_SCHEMA: [(&str, u8, u8, u8); AN_FIELD_COUNT] = [
    ("TYPE", b'C', 1, 0),
    ("ARGUMENT", b'C', 22, 0),
    ("DOCUMENTID", b'C', 22, 0),
    ("OBJECTTYPE", b'C', 2, 0),
    ("OBJECTID", b'C', 22, 0),
    ("FIELD", b'C', 64, 0),
    ("BEFORE", b'L', 1, 0),
    ("USER", b'C', 22, 0),
    ("LASTUSER", b'C', 22, 0),
    ("ROLE", b'N', 1, 0),
    ("CONTENT", b'M', 10, 0),
    ("CONTEXT", b'M', 10, 0),
    ("CHECKSUM", b'C', 22, 0),
    ("CWGUID", b'C', 22, 0),
    ("CWDATE", b'C', 14, 0),
    ("CREATEDON", b'C', 14, 0),
    ("ANDATE", b'C', 14, 0),
    ("ANCREADATE", b'C', 14, 0),
    ("CWCHECKSUM", b'C', 8, 0),
    ("CONSCHKSUM", b'C', 8, 0),
    ("EXTCHKSUM", b'C', 8, 0),
];

/// `TS` schema in physical order.
pub static TS_SCHEMA: [(&str, u8, u8, u8); TS_FIELD_COUNT] = [
    ("CWGUID", b'C', 22, 0),
    ("FILEID", b'C', 22, 0),
    ("DOCID", b'C', 22, 0),
    ("OBJECTID", b'C', 128, 0),
    ("OBJECTTYPE", b'C', 2, 0),
    ("FIELD", b'C', 64, 0),
    ("TYPE", b'C', 2, 0),
    ("SUBJECT", b'C', 254, 0),
    ("DESC", b'M', 10, 0),
    ("RESOLUTION", b'M', 10, 0),
    ("PRIORITY", b'C', 1, 0),
    ("CREATEDBY", b'C', 22, 0),
    ("ASSIGNEDTO", b'C', 22, 0),
    ("CREATEDON", b'C', 14, 0),
    ("MODIFIEDON", b'C', 14, 0),
    ("STARTON", b'C', 14, 0),
    ("DUEON", b'C', 14, 0),
    ("REMINDER", b'C', 14, 0),
    ("REMINDED", b'C', 14, 0),
    ("COMPLETEON", b'C', 14, 0),
    ("CLEAREDON", b'C', 14, 0),
    ("ROLL", b'L', 1, 0),
    ("ARCHIVE", b'L', 1, 0),
    ("STATUS", b'C', 1, 0),
    ("PERCENT", b'N', 3, 0),
    ("CLEARED", b'L', 1, 0),
    ("HISTORYCRE", b'C', 22, 0),
    ("HISTORYCOM", b'C', 22, 0),
    ("CWDATED", b'C', 14, 0),
    ("CWDATER", b'C', 14, 0),
    ("CLEAREDBY", b'C', 22, 0),
    ("COMPLETEBY", b'C', 22, 0),
    ("ID", b'C', 12, 0),
    ("EXINFO", b'M', 10, 0),
];

fn cell(t: &Table, r: usize, name: &str) -> String {
    t.cell_str(r, name)
        .unwrap_or_default()
        .trim_end_matches([' ', '\u{0}'])
        .to_string()
}

fn trimmed(t: &Table, r: usize, name: &str) -> String {
    cell(t, r, name).trim().to_string()
}

fn flag(t: &Table, r: usize, name: &str) -> bool {
    matches!(trimmed(t, r, name).to_ascii_uppercase().as_str(), "Y" | "T")
}

fn number(t: &Table, r: usize, name: &str) -> Option<i64> {
    trimmed(t, r, name).parse().ok()
}

fn memo_text(fpt: &Fpt, raw: &[u8]) -> String {
    fpt.memo(raw)
        .map(|b| String::from_utf8_lossy(b).to_string())
        .unwrap_or_default()
}

fn check_schema(table: &Table, name: &str, want: &[(&str, u8, u8, u8)]) -> Result<()> {
    if table.fields.len() != want.len() {
        return Err(CoreError::Format(format!(
            "{name}: {} columns, want {}",
            table.fields.len(),
            want.len()
        )));
    }
    for (got, (w_name, w_kind, w_len, w_dec)) in table.fields.iter().zip(want.iter()) {
        if got.name != *w_name || got.kind != *w_kind || got.len != *w_len || got.dec != *w_dec {
            return Err(CoreError::Format(format!(
                "{name}: schema drift at {}: got {} {}{}.{}, want {w_name} {}{w_len}.{w_dec}",
                got.name,
                got.name,
                got.kind as char,
                got.len,
                got.dec,
                *w_kind as char
            )));
        }
    }
    Ok(())
}

/// Width- and type-checked cell write (same shape as the CW writers).
fn put(rec: &mut [u8], fields: &[cw_core::dbf::Field], name: &str, value: &str) -> Result<()> {
    let (idx, field) = fields
        .iter()
        .enumerate()
        .find(|(_, f)| f.name == name)
        .ok_or_else(|| CoreError::Format(format!("no column {name}")))?;
    if value.len() > field.len as usize {
        return Err(CoreError::Format(format!(
            "{name} value {} chars > width {}",
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

/// Append one text payload to an FPT image, returning its block reference
/// (0 = empty payload). Same block layout as `cw-app::history`.
fn put_memo(fpt: &mut Fpt, payload: &[u8]) -> u32 {
    if payload.is_empty() {
        return 0;
    }
    let bs = fpt.block_size.max(1) as usize;
    let block = fpt.next_free.max(1);
    let base = block as usize * bs;
    if fpt.data.len() < base {
        fpt.data.resize(base, 0);
    }
    let need = 8 + payload.len();
    let blocks = (need + bs - 1) / bs;
    let end = base + need;
    if fpt.data.len() < end {
        fpt.data.resize(end, 0);
    }
    fpt.data[base..base + 4].copy_from_slice(&1u32.to_be_bytes());
    fpt.data[base + 4..base + 8].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    fpt.data[base + 8..base + 8 + payload.len()].copy_from_slice(payload);
    fpt.next_free = block + blocks as u32;
    fpt.data[0..4].copy_from_slice(&fpt.next_free.to_be_bytes());
    block
}

fn memo_ref(block: u32) -> String {
    format!("{block:>10}")
}

fn empty_fpt() -> Fpt {
    let mut data = vec![0u8; 512];
    data[6..8].copy_from_slice(&512u16.to_be_bytes());
    data[0..4].copy_from_slice(&1u32.to_be_bytes());
    Fpt { data, block_size: 512, next_free: 1 }
}

/// Case-insensitive `<dir>\<stem><suffix>.<ext>` lookup (AN.fpt vs AN.FPT).
fn sidecar(dir: &Path, stem: &str, suffix: &str, ext: &str) -> Result<PathBuf> {
    let want = format!("{stem}{suffix}.{ext}").to_ascii_lowercase();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if name == want {
                return Ok(entry.path());
            }
        }
    }
    Err(CoreError::Format(format!("missing {stem}{suffix}.{ext}")))
}

// ---------------------------------------------------------------------------
// AN - annotations / tickmarks / references
// ---------------------------------------------------------------------------

/// The six observed `AN.TYPE` classes (spec §1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnotationType {
    Annotation,
    Note,
    Tickmark,
    Reference,
    Issue,
    UserText,
}

impl AnnotationType {
    pub fn letter(self) -> char {
        match self {
            AnnotationType::Annotation => 'A',
            AnnotationType::Note => 'N',
            AnnotationType::Tickmark => 'T',
            AnnotationType::Reference => 'R',
            AnnotationType::Issue => 'I',
            AnnotationType::UserText => 'U',
        }
    }

    pub fn parse(s: &str) -> Result<AnnotationType> {
        match s.trim() {
            "A" => Ok(AnnotationType::Annotation),
            "N" => Ok(AnnotationType::Note),
            "T" => Ok(AnnotationType::Tickmark),
            "R" => Ok(AnnotationType::Reference),
            "I" => Ok(AnnotationType::Issue),
            "U" => Ok(AnnotationType::UserText),
            other => Err(CoreError::Format(format!(
                "AN type {other:?} outside A/N/T/R/I/U"
            ))),
        }
    }

    /// `T`/`R`/`I` carry a 22-char CWGUID-shaped `ARGUMENT`; `U` carries
    /// literal text (spec §1, F60 shape test).
    pub fn argument_is_id(self) -> bool {
        matches!(
            self,
            AnnotationType::Annotation | AnnotationType::Tickmark | AnnotationType::Reference | AnnotationType::Issue
        )
    }
}

/// Decoded anchor: `OBJECTTYPE` (`AM`/`MP`/`TP`/`**`) + `OBJECTID`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Anchor {
    pub object_type: String,
    pub object_id: String,
}

impl Anchor {
    pub fn account(guid: &str) -> Anchor {
        Anchor { object_type: "AM".into(), object_id: guid.trim().to_string() }
    }
    pub fn mapping(guid: &str) -> Anchor {
        Anchor { object_type: "MP".into(), object_id: guid.trim().to_string() }
    }
    pub fn tax_code(guid: &str) -> Anchor {
        Anchor { object_type: "TP".into(), object_id: guid.trim().to_string() }
    }
    /// `**` anchor used for document/global annotations (spec §1).
    pub fn global() -> Anchor {
        Anchor { object_type: "**".into(), object_id: String::new() }
    }
    pub fn is_global(&self) -> bool {
        self.object_type == "**" && self.object_id.is_empty()
    }
    pub fn is_empty(&self) -> bool {
        self.object_type.is_empty() && self.object_id.is_empty()
    }
}

/// One decoded `AN` row (memo cells resolved through `AN.fpt`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Annotation {
    pub kind: AnnotationType,
    /// `ARGUMENT` (link slot; meaning follows `kind`).
    pub argument: String,
    /// `DOCUMENTID` - owning CaseView document CWGUID (blank = doc-agnostic).
    pub document_id: String,
    pub object_type: String,
    pub object_id: String,
    /// `FIELD` - optional field/cell identity inside the anchored object.
    pub field: String,
    pub before: bool,
    pub user: String,
    pub last_user: String,
    pub role: Option<i64>,
    /// `CONTENT` memo payload (resolved text).
    pub content: String,
    /// Raw `CONTENT` block reference cell (trimmed).
    pub content_ref: String,
    /// `CONTEXT` memo payload (resolved text).
    pub context: String,
    pub checksum: String,
    pub cwguid: String,
    pub cw_date: String,
    pub created_on: String,
    pub an_date: String,
    pub an_created_date: String,
    pub cw_checksum: String,
    pub cons_chksum: String,
    pub ext_chksum: String,
    pub deleted: bool,
}

impl Annotation {
    pub fn anchor(&self) -> Anchor {
        Anchor { object_type: self.object_type.clone(), object_id: self.object_id.clone() }
    }

    /// True when the annotation is document/global rather than object-anchored.
    pub fn is_document_level(&self) -> bool {
        self.object_id.trim().is_empty() || self.object_type.trim() == "**"
    }

    pub fn has_text(&self) -> bool {
        !self.content.is_empty()
    }

    fn decode(t: &Table, r: usize, fpt: &Fpt) -> Result<Annotation> {
        let content_raw = t.cell(r, "CONTENT").unwrap_or(&[]);
        let context_raw = t.cell(r, "CONTEXT").unwrap_or(&[]);
        Ok(Annotation {
            kind: AnnotationType::parse(&trimmed(t, r, "TYPE"))?,
            argument: trimmed(t, r, "ARGUMENT"),
            document_id: trimmed(t, r, "DOCUMENTID"),
            object_type: trimmed(t, r, "OBJECTTYPE"),
            object_id: trimmed(t, r, "OBJECTID"),
            field: trimmed(t, r, "FIELD"),
            before: flag(t, r, "BEFORE"),
            user: trimmed(t, r, "USER"),
            last_user: trimmed(t, r, "LASTUSER"),
            role: number(t, r, "ROLE"),
            content: memo_text(fpt, content_raw),
            content_ref: trimmed(t, r, "CONTENT"),
            context: memo_text(fpt, context_raw),
            checksum: trimmed(t, r, "CHECKSUM"),
            cwguid: trimmed(t, r, "CWGUID"),
            cw_date: trimmed(t, r, "CWDATE"),
            created_on: trimmed(t, r, "CREATEDON"),
            an_date: trimmed(t, r, "ANDATE"),
            an_created_date: trimmed(t, r, "ANCREADATE"),
            cw_checksum: trimmed(t, r, "CWCHECKSUM"),
            cons_chksum: trimmed(t, r, "CONSCHKSUM"),
            ext_chksum: trimmed(t, r, "EXTCHKSUM"),
            deleted: t.is_deleted(r),
        })
    }
}

/// COM-factory-shaped write request. Build with a constructor, fill the
/// per-kind slots, then [`AnnotationBook::add`].
#[derive(Clone, Debug)]
pub struct AnnotationDraft {
    pub kind: AnnotationType,
    pub argument: String,
    pub document_id: String,
    pub object_type: String,
    pub object_id: String,
    pub field: String,
    pub content: String,
    pub context: String,
    pub user: String,
    pub role: Option<u8>,
}

impl AnnotationDraft {
    fn base(kind: AnnotationType, user: &str) -> AnnotationDraft {
        AnnotationDraft {
            kind,
            argument: String::new(),
            document_id: String::new(),
            object_type: String::new(),
            object_id: String::new(),
            field: String::new(),
            content: String::new(),
            context: String::new(),
            user: user.trim().to_string(),
            role: None,
        }
    }

    /// COM `AddNote(text, [pt])`.
    pub fn note(text: &str, user: &str) -> AnnotationDraft {
        let mut d = AnnotationDraft::base(AnnotationType::Note, user);
        d.content = text.to_string();
        d
    }

    /// COM `AddTickmark(tickmark, [pt])` - `definition` is the tickmark
    /// registry id; a plain string is rejected by COM, so this is the
    /// pre-looked-up id (spec §5).
    pub fn tickmark(definition: &str, user: &str) -> AnnotationDraft {
        let mut d = AnnotationDraft::base(AnnotationType::Tickmark, user);
        d.argument = definition.trim().to_string();
        d
    }

    /// COM `AddReference(target, [pt])` - `target` is a CWGUID.
    pub fn reference(target: &str, user: &str) -> AnnotationDraft {
        let mut d = AnnotationDraft::base(AnnotationType::Reference, user);
        d.argument = target.trim().to_string();
        d
    }

    /// COM `AddIssue(issue, [pt])` - `issue_guid` is the `TS.CWGUID`.
    pub fn issue(issue_guid: &str, user: &str) -> AnnotationDraft {
        let mut d = AnnotationDraft::base(AnnotationType::Issue, user);
        d.argument = issue_guid.trim().to_string();
        d
    }

    /// COM `AddManualReference(text, [pt])`.
    pub fn manual_reference(text: &str, user: &str) -> AnnotationDraft {
        let mut d = AnnotationDraft::base(AnnotationType::UserText, user);
        d.argument = text.to_string();
        d
    }

    /// COM `AddHyperlink(label, url, external, [pt])` - label lands in
    /// `ARGUMENT`, the URL/context in the `CONTEXT` memo.
    pub fn hyperlink(label: &str, url: &str, user: &str) -> AnnotationDraft {
        let mut d = AnnotationDraft::base(AnnotationType::UserText, user);
        d.argument = label.to_string();
        d.context = format!("url:{url}");
        d
    }

    /// Attach an anchor (`AM`/`MP`/`TP` or `**`).
    pub fn anchored(mut self, anchor: &Anchor) -> AnnotationDraft {
        self.object_type = anchor.object_type.clone();
        self.object_id = anchor.object_id.clone();
        self
    }

    pub fn on_document(mut self, doc_guid: &str) -> AnnotationDraft {
        self.document_id = doc_guid.trim().to_string();
        self
    }

    pub fn on_field(mut self, field: &str) -> AnnotationDraft {
        self.field = field.to_string();
        self
    }

    pub fn with_content(mut self, text: &str) -> AnnotationDraft {
        self.content = text.to_string();
        self
    }
}

/// Per-book counts (live rows only).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnnotationCensus {
    pub live: usize,
    pub deleted: usize,
    pub by_type: BTreeMap<char, usize>,
    pub anchored: usize,
    pub document_global: usize,
    pub with_text: usize,
    pub tickmark_definitions: usize,
    pub users: usize,
    pub documents: usize,
}

/// Tickmark definition usage across one `AN` book.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TickmarkUsage {
    pub definition: String,
    pub count: usize,
    pub documents: BTreeSet<String>,
}

/// The `AN` table plus its `.fpt` memo sidecar.
#[derive(Clone, Debug)]
pub struct AnnotationBook {
    table: Table,
    fpt: Fpt,
    rows: Vec<usize>,
    annotations: Vec<Annotation>,
}

impl AnnotationBook {
    /// Open `<dir>\<stem>AN.dbf` + `AN.fpt` (FPT optional).
    pub fn open(dir: impl AsRef<Path>, stem: &str) -> Result<AnnotationBook> {
        let dir = dir.as_ref();
        let dbf = sidecar(dir, stem, "AN", "dbf")?;
        let table = Table::parse_path(dbf)?;
        let fpt = match sidecar(dir, stem, "AN", "fpt") {
            Ok(p) => Fpt::parse_path(p)?,
            Err(_) => empty_fpt(),
        };
        Self::from_parts(table, fpt)
    }

    pub fn from_bytes(dbf: &[u8], fpt: &[u8]) -> Result<AnnotationBook> {
        let table = Table::parse(dbf)?;
        let fpt = if fpt.is_empty() { empty_fpt() } else { Fpt::parse(fpt)? };
        Self::from_parts(table, fpt)
    }

    pub fn from_table(table: Table, fpt: Fpt) -> Result<AnnotationBook> {
        Self::from_parts(table, fpt)
    }

    fn from_parts(table: Table, fpt: Fpt) -> Result<AnnotationBook> {
        check_schema(&table, "AN", &AN_SCHEMA)?;
        let mut rows = Vec::new();
        let mut annotations = Vec::new();
        for r in 0..table.count() {
            if table.is_deleted(r) {
                continue;
            }
            annotations.push(Annotation::decode(&table, r, &fpt)?);
            rows.push(r);
        }
        Ok(AnnotationBook { table, fpt, rows, annotations })
    }

    /// Live annotations in physical row order.
    pub fn annotations(&self) -> &[Annotation] {
        &self.annotations
    }

    pub fn count(&self) -> usize {
        self.annotations.len()
    }

    pub fn index_of(&self, cwguid: &str) -> Option<usize> {
        self.annotations.iter().position(|a| a.cwguid.trim() == cwguid.trim())
    }

    pub fn by_type(&self, kind: AnnotationType) -> Vec<&Annotation> {
        self.annotations.iter().filter(|a| a.kind == kind).collect()
    }

    pub fn notes(&self) -> Vec<&Annotation> {
        self.by_type(AnnotationType::Note)
    }

    pub fn references(&self) -> Vec<&Annotation> {
        self.by_type(AnnotationType::Reference)
    }

    pub fn issues(&self) -> Vec<&Annotation> {
        self.by_type(AnnotationType::Issue)
    }

    pub fn tickmarks(&self) -> Vec<&Annotation> {
        self.by_type(AnnotationType::Tickmark)
    }

    /// Annotations owned by one CaseView document.
    pub fn by_document(&self, doc_guid: &str) -> Vec<&Annotation> {
        self.annotations
            .iter()
            .filter(|a| a.document_id.trim() == doc_guid.trim())
            .collect()
    }

    /// Annotations anchored to one object (`AM`/`MP`/`TP` CWGUID).
    pub fn by_object(&self, object_id: &str) -> Vec<&Annotation> {
        self.annotations
            .iter()
            .filter(|a| a.object_id.trim() == object_id.trim())
            .collect()
    }

    /// Tickmark definition usage, most-used first (definition then count).
    pub fn tickmark_usage(&self) -> Vec<TickmarkUsage> {
        let mut map: BTreeMap<String, TickmarkUsage> = BTreeMap::new();
        for a in self.tickmarks() {
            let e = map.entry(a.argument.clone()).or_insert_with(|| TickmarkUsage {
                definition: a.argument.clone(),
                count: 0,
                documents: BTreeSet::new(),
            });
            e.count += 1;
            if !a.document_id.is_empty() {
                e.documents.insert(a.document_id.clone());
            }
        }
        let mut out: Vec<TickmarkUsage> = map.into_values().collect();
        out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.definition.cmp(&b.definition)));
        out
    }

    /// Distinct tickmark definitions (the pre-step COM requires, spec §5).
    pub fn tickmark_definitions(&self) -> Vec<String> {
        self.tickmark_usage().into_iter().map(|t| t.definition).collect()
    }

    pub fn census(&self) -> AnnotationCensus {
        let mut c = AnnotationCensus {
            live: self.annotations.len(),
            deleted: self.table.count().saturating_sub(self.annotations.len()),
            ..Default::default()
        };
        let mut users = BTreeSet::new();
        let mut documents = BTreeSet::new();
        for a in &self.annotations {
            *c.by_type.entry(a.kind.letter()).or_insert(0) += 1;
            if !a.object_id.is_empty() {
                c.anchored += 1;
            } else {
                c.document_global += 1;
            }
            if a.has_text() {
                c.with_text += 1;
            }
            if !a.user.is_empty() {
                users.insert(a.user.clone());
            }
            if !a.document_id.is_empty() {
                documents.insert(a.document_id.clone());
            }
        }
        c.tickmark_definitions = self.tickmark_definitions().len();
        c.users = users.len();
        c.documents = documents.len();
        c
    }

    /// COM-factory writer: append one row and (when `content`/`context` are
    /// set) memo blocks. Returns the live index.
    pub fn add(&mut self, draft: AnnotationDraft, rng: &mut Rng, stamp: &str) -> Result<usize> {
        if draft.object_type.len() > 2 {
            return Err(CoreError::Format(format!(
                "AN OBJECTTYPE {:?} > 2 chars",
                draft.object_type
            )));
        }
        if !draft.object_id.is_empty() && draft.object_type.trim().is_empty() {
            return Err(CoreError::Format(
                "AN: OBJECTID requires OBJECTTYPE".into(),
            ));
        }
        if draft.kind.argument_is_id() {
            if draft.argument.trim().is_empty() {
                return Err(CoreError::Format(format!(
                    "AN {} requires an ARGUMENT id",
                    draft.kind.letter()
                )));
            }
            if draft.argument.len() > 22 {
                return Err(CoreError::Format(format!(
                    "AN ARGUMENT {} chars > 22",
                    draft.argument.len()
                )));
            }
        }
        if draft.kind == AnnotationType::Tickmark && draft.argument.trim().is_empty() {
            return Err(CoreError::Format("tickmark definition id is mandatory".into()));
        }
        let guid = CwGuid::allocate_for_table("AN", rng).encode();
        let mut rec = vec![LIVE; AN_RECORD_LEN];
        let fields = self.table.fields.clone();
        put(&mut rec, &fields, "TYPE", &draft.kind.letter().to_string())?;
        put(&mut rec, &fields, "ARGUMENT", draft.argument.trim())?;
        put(&mut rec, &fields, "DOCUMENTID", draft.document_id.trim())?;
        put(&mut rec, &fields, "OBJECTTYPE", draft.object_type.trim())?;
        put(&mut rec, &fields, "OBJECTID", draft.object_id.trim())?;
        put(&mut rec, &fields, "FIELD", draft.field.trim())?;
        put(&mut rec, &fields, "BEFORE", "F")?;
        put(&mut rec, &fields, "USER", draft.user.trim())?;
        put(&mut rec, &fields, "LASTUSER", draft.user.trim())?;
        if let Some(role) = draft.role {
            put(&mut rec, &fields, "ROLE", &role.to_string())?;
        }
        let content_block = put_memo(&mut self.fpt, draft.content.as_bytes());
        let context_block = put_memo(&mut self.fpt, draft.context.as_bytes());
        put(&mut rec, &fields, "CONTENT", &memo_ref(content_block))?;
        put(&mut rec, &fields, "CONTEXT", &memo_ref(context_block))?;
        put(&mut rec, &fields, "CWGUID", &guid)?;
        put(&mut rec, &fields, "CWDATE", stamp)?;
        put(&mut rec, &fields, "CREATEDON", stamp)?;
        put(&mut rec, &fields, "ANDATE", stamp)?;
        put(&mut rec, &fields, "ANCREADATE", stamp)?;
        let row = self.table.records.len();
        self.table.records.push(rec);
        self.annotations.push(Annotation::decode(&self.table, row, &self.fpt)?);
        self.rows.push(row);
        Ok(self.annotations.len() - 1)
    }

    /// COM `Remove(annotation)` - physical tombstone (CW semantics
    /// unexercised; recorded residual).
    pub fn remove_at(&mut self, index: usize) -> Result<()> {
        let row = *self
            .rows
            .get(index)
            .ok_or_else(|| CoreError::Format(format!("AN remove {index} out of range")))?;
        if let Some(rec) = self.table.records.get_mut(row) {
            rec[0] = DELETED;
        }
        if let Some(a) = self.annotations.get_mut(index) {
            a.deleted = true;
        }
        Ok(())
    }

    /// Replace one annotation's memo `CONTENT` (edit path; appends a new block).
    pub fn set_content(&mut self, index: usize, text: &str, user: &str, stamp: &str) -> Result<()> {
        let row = *self
            .rows
            .get(index)
            .ok_or_else(|| CoreError::Format(format!("AN edit {index} out of range")))?;
        let block = put_memo(&mut self.fpt, text.as_bytes());
        let fields = self.table.fields.clone();
        {
            let rec = self
                .table
                .records
                .get_mut(row)
                .ok_or_else(|| CoreError::Format(format!("AN row {row} missing")))?;
            put(rec, &fields, "CONTENT", &memo_ref(block))?;
            put(rec, &fields, "LASTUSER", user.trim())?;
            put(rec, &fields, "CWDATE", stamp)?;
        }
        self.annotations[index] = Annotation::decode(&self.table, row, &self.fpt)?;
        Ok(())
    }

    /// In-memory save image (`AN.dbf` bytes). A no-op save is byte-identical.
    pub fn encode(&self) -> Vec<u8> {
        self.table.encode()
    }

    pub fn encode_fpt(&self) -> Vec<u8> {
        self.fpt.encode()
    }

    pub fn save(&self, dbf: impl AsRef<Path>, fpt: impl AsRef<Path>) -> Result<()> {
        self.table.write_path(dbf)?;
        self.fpt.write_path(fpt)
    }

    pub fn table(&self) -> &Table {
        &self.table
    }

    pub fn fpt(&self) -> &Fpt {
        &self.fpt
    }
}

// ---------------------------------------------------------------------------
// TS - issues / review notes
// ---------------------------------------------------------------------------

/// `TS.STATUS` classes observed in corp2026 (`N` x4, `C` x43).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueStatus {
    /// `N` - new/open.
    New,
    /// `C` - complete (`PERCENT=100`).
    Complete,
}

impl IssueStatus {
    pub fn letter(self) -> char {
        match self {
            IssueStatus::New => 'N',
            IssueStatus::Complete => 'C',
        }
    }

    pub fn parse(s: &str) -> Result<IssueStatus> {
        match s.trim() {
            "N" => Ok(IssueStatus::New),
            "C" => Ok(IssueStatus::Complete),
            other => Err(CoreError::Format(format!(
                "TS status {other:?} outside N/C (observed domain)"
            ))),
        }
    }
}

/// One decoded `TS` row (memo cells resolved through `TS.fpt`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub cwguid: String,
    pub file_id: String,
    pub doc_id: String,
    pub object_id: String,
    pub object_type: String,
    pub field: String,
    pub issue_type: String,
    pub subject: String,
    pub description: String,
    pub desc_ref: String,
    pub resolution: String,
    pub resolution_ref: String,
    pub priority: String,
    pub created_by: String,
    pub assigned_to: String,
    pub created_on: String,
    pub modified_on: String,
    pub start_on: String,
    pub due_on: String,
    pub reminder: String,
    pub reminded: String,
    pub complete_on: String,
    pub cleared_on: String,
    pub roll: bool,
    pub archive: bool,
    pub status: String,
    pub percent: Option<i64>,
    pub cleared: bool,
    pub history_created: String,
    pub history_comment: String,
    pub cw_date_d: String,
    pub cw_date_r: String,
    pub cleared_by: String,
    pub complete_by: String,
    pub id: String,
    pub ex_info: String,
    pub deleted: bool,
}

impl Issue {
    pub fn status_kind(&self) -> Result<IssueStatus> {
        IssueStatus::parse(&self.status)
    }

    pub fn is_open(&self) -> bool {
        self.status.trim() != "C"
    }

    pub fn is_complete(&self) -> bool {
        self.status.trim() == "C"
    }

    pub fn anchor(&self) -> Anchor {
        Anchor { object_type: self.object_type.clone(), object_id: self.object_id.clone() }
    }

    fn decode(t: &Table, r: usize, fpt: &Fpt) -> Result<Issue> {
        Ok(Issue {
            cwguid: trimmed(t, r, "CWGUID"),
            file_id: trimmed(t, r, "FILEID"),
            doc_id: trimmed(t, r, "DOCID"),
            object_id: trimmed(t, r, "OBJECTID"),
            object_type: trimmed(t, r, "OBJECTTYPE"),
            field: trimmed(t, r, "FIELD"),
            issue_type: trimmed(t, r, "TYPE"),
            subject: trimmed(t, r, "SUBJECT"),
            description: memo_text(fpt, t.cell(r, "DESC").unwrap_or(&[])),
            desc_ref: trimmed(t, r, "DESC"),
            resolution: memo_text(fpt, t.cell(r, "RESOLUTION").unwrap_or(&[])),
            resolution_ref: trimmed(t, r, "RESOLUTION"),
            priority: trimmed(t, r, "PRIORITY"),
            created_by: trimmed(t, r, "CREATEDBY"),
            assigned_to: trimmed(t, r, "ASSIGNEDTO"),
            created_on: trimmed(t, r, "CREATEDON"),
            modified_on: trimmed(t, r, "MODIFIEDON"),
            start_on: trimmed(t, r, "STARTON"),
            due_on: trimmed(t, r, "DUEON"),
            reminder: trimmed(t, r, "REMINDER"),
            reminded: trimmed(t, r, "REMINDED"),
            complete_on: trimmed(t, r, "COMPLETEON"),
            cleared_on: trimmed(t, r, "CLEAREDON"),
            roll: flag(t, r, "ROLL"),
            archive: flag(t, r, "ARCHIVE"),
            status: trimmed(t, r, "STATUS"),
            percent: number(t, r, "PERCENT"),
            cleared: flag(t, r, "CLEARED"),
            history_created: trimmed(t, r, "HISTORYCRE"),
            history_comment: trimmed(t, r, "HISTORYCOM"),
            cw_date_d: trimmed(t, r, "CWDATED"),
            cw_date_r: trimmed(t, r, "CWDATER"),
            cleared_by: trimmed(t, r, "CLEAREDBY"),
            complete_by: trimmed(t, r, "COMPLETEBY"),
            id: trimmed(t, r, "ID"),
            ex_info: memo_text(fpt, t.cell(r, "EXINFO").unwrap_or(&[])),
            deleted: t.is_deleted(r),
        })
    }
}

/// Write request for [`IssueBook::add`].
#[derive(Clone, Debug, Default)]
pub struct IssueDraft {
    /// `SUBJECT` (<=254).
    pub subject: String,
    /// `DESC` memo payload.
    pub description: String,
    /// `DOCID` - owning CaseView document.
    pub doc_id: String,
    pub object_type: String,
    pub object_id: String,
    pub field: String,
    /// `TYPE` (blank in every corpus row).
    pub issue_type: String,
    /// `PRIORITY` (`"1"` in every corpus row).
    pub priority: String,
    pub created_by: String,
    pub assigned_to: String,
    /// `ID` - when blank the writer continues the observed prefix.
    pub id: String,
}

impl IssueDraft {
    pub fn new(subject: &str, created_by: &str) -> IssueDraft {
        IssueDraft {
            subject: subject.to_string(),
            priority: "1".into(),
            created_by: created_by.trim().to_string(),
            ..Default::default()
        }
    }

    pub fn on_document(mut self, doc_guid: &str) -> IssueDraft {
        self.doc_id = doc_guid.trim().to_string();
        self
    }

    pub fn anchored(mut self, anchor: &Anchor) -> IssueDraft {
        self.object_type = anchor.object_type.clone();
        self.object_id = anchor.object_id.clone();
        self
    }

    pub fn described(mut self, text: &str) -> IssueDraft {
        self.description = text.to_string();
        self
    }

    pub fn assigned_to(mut self, user: &str) -> IssueDraft {
        self.assigned_to = user.trim().to_string();
        self
    }
}

/// Per-book issue counts (live rows only).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IssueCensus {
    pub live: usize,
    pub deleted: usize,
    pub open: usize,
    pub complete: usize,
    pub by_status: BTreeMap<String, usize>,
    pub by_priority: BTreeMap<String, usize>,
    pub documents: usize,
    pub anchored: usize,
    pub unassigned: usize,
}

/// The `TS` table plus its `.fpt` memo sidecar.
#[derive(Clone, Debug)]
pub struct IssueBook {
    table: Table,
    fpt: Fpt,
    rows: Vec<usize>,
    issues: Vec<Issue>,
}

impl IssueBook {
    pub fn open(dir: impl AsRef<Path>, stem: &str) -> Result<IssueBook> {
        let dir = dir.as_ref();
        let dbf = sidecar(dir, stem, "TS", "dbf")?;
        let table = Table::parse_path(dbf)?;
        let fpt = match sidecar(dir, stem, "TS", "fpt") {
            Ok(p) => Fpt::parse_path(p)?,
            Err(_) => empty_fpt(),
        };
        Self::from_parts(table, fpt)
    }

    pub fn from_bytes(dbf: &[u8], fpt: &[u8]) -> Result<IssueBook> {
        let table = Table::parse(dbf)?;
        let fpt = if fpt.is_empty() { empty_fpt() } else { Fpt::parse(fpt)? };
        Self::from_parts(table, fpt)
    }

    pub fn from_table(table: Table, fpt: Fpt) -> Result<IssueBook> {
        Self::from_parts(table, fpt)
    }

    fn from_parts(table: Table, fpt: Fpt) -> Result<IssueBook> {
        check_schema(&table, "TS", &TS_SCHEMA)?;
        let mut rows = Vec::new();
        let mut issues = Vec::new();
        for r in 0..table.count() {
            if table.is_deleted(r) {
                continue;
            }
            issues.push(Issue::decode(&table, r, &fpt)?);
            rows.push(r);
        }
        Ok(IssueBook { table, fpt, rows, issues })
    }

    pub fn issues(&self) -> &[Issue] {
        &self.issues
    }

    pub fn count(&self) -> usize {
        self.issues.len()
    }

    pub fn index_of(&self, cwguid: &str) -> Option<usize> {
        self.issues.iter().position(|i| i.cwguid.trim() == cwguid.trim())
    }

    pub fn by_id(&self, id: &str) -> Option<&Issue> {
        self.issues.iter().find(|i| i.id.trim() == id.trim())
    }

    pub fn open_issues(&self) -> Vec<&Issue> {
        self.issues.iter().filter(|i| i.is_open()).collect()
    }

    pub fn complete_issues(&self) -> Vec<&Issue> {
        self.issues.iter().filter(|i| i.is_complete()).collect()
    }

    pub fn issues_for_document(&self, doc_guid: &str) -> Vec<&Issue> {
        self.issues
            .iter()
            .filter(|i| i.doc_id.trim() == doc_guid.trim())
            .collect()
    }

    pub fn issues_for_object(&self, object_id: &str) -> Vec<&Issue> {
        self.issues
            .iter()
            .filter(|i| i.object_id.trim() == object_id.trim())
            .collect()
    }

    /// Issue links stored in an `AN` book (`TYPE='I'`), resolved to issues by
    /// `ARGUMENT`. Danglers (annotation to a missing issue) are reported.
    pub fn linked_annotations(&self, annotations: &AnnotationBook) -> (Vec<String>, Vec<String>) {
        let mut linked = Vec::new();
        let mut danglers = Vec::new();
        for a in annotations.issues() {
            if self.index_of(&a.argument).is_some() {
                linked.push(a.argument.clone());
            } else {
                danglers.push(a.argument.clone());
            }
        }
        (linked, danglers)
    }

    pub fn census(&self) -> IssueCensus {
        let mut c = IssueCensus {
            live: self.issues.len(),
            deleted: self.table.count().saturating_sub(self.issues.len()),
            ..Default::default()
        };
        let mut documents = BTreeSet::new();
        for i in &self.issues {
            *c.by_status.entry(i.status.clone()).or_insert(0) += 1;
            *c.by_priority.entry(i.priority.clone()).or_insert(0) += 1;
            if i.is_open() {
                c.open += 1;
            } else {
                c.complete += 1;
            }
            if !i.doc_id.is_empty() {
                documents.insert(i.doc_id.clone());
            }
            if !i.object_id.is_empty() {
                c.anchored += 1;
            }
            if i.assigned_to.is_empty() {
                c.unassigned += 1;
            }
        }
        c.documents = documents.len();
        c
    }

    /// Continue the observed `ID` sequence (`<prefix>-<n>`), keeping the
    /// prefix of the last live row and bumping the highest matching number.
    pub fn suggest_id(&self) -> String {
        let last = self.issues.last().map(|i| i.id.clone()).unwrap_or_default();
        let prefix = last
            .rsplit_once('-')
            .map(|(p, _)| p.to_string())
            .unwrap_or_else(|| "VF".to_string());
        let mut max = 0u32;
        for i in &self.issues {
            if let Some((p, n)) = i.id.rsplit_once('-') {
                if p == prefix {
                    if let Ok(v) = n.parse::<u32>() {
                        max = max.max(v);
                    }
                }
            }
        }
        format!("{prefix}-{}", max + 1)
    }

    /// Append one issue: `STATUS='N'`, `PERCENT=0`, audit stamps = `stamp`,
    /// booleans `F`. Returns the live index.
    pub fn add(&mut self, draft: IssueDraft, rng: &mut Rng, stamp: &str) -> Result<usize> {
        if draft.subject.trim().is_empty() {
            return Err(CoreError::Format("TS: SUBJECT is mandatory".into()));
        }
        if draft.object_type.len() > 2 {
            return Err(CoreError::Format(format!(
                "TS OBJECTTYPE {:?} > 2 chars",
                draft.object_type
            )));
        }
        let guid = CwGuid::allocate_for_table("TS", rng).encode();
        let id = if draft.id.trim().is_empty() {
            self.suggest_id()
        } else {
            draft.id.trim().to_string()
        };
        if self.issues.iter().any(|i| i.id == id) {
            return Err(CoreError::Format(format!("TS: duplicate ID {id}")));
        }
        let mut rec = vec![LIVE; TS_RECORD_LEN];
        let fields = self.table.fields.clone();
        put(&mut rec, &fields, "CWGUID", &guid)?;
        put(&mut rec, &fields, "DOCID", draft.doc_id.trim())?;
        put(&mut rec, &fields, "OBJECTTYPE", draft.object_type.trim())?;
        put(&mut rec, &fields, "OBJECTID", draft.object_id.trim())?;
        put(&mut rec, &fields, "FIELD", draft.field.trim())?;
        put(&mut rec, &fields, "TYPE", draft.issue_type.trim())?;
        put(&mut rec, &fields, "SUBJECT", draft.subject.trim())?;
        let desc = put_memo(&mut self.fpt, draft.description.as_bytes());
        put(&mut rec, &fields, "DESC", &memo_ref(desc))?;
        put(&mut rec, &fields, "PRIORITY", draft.priority.trim())?;
        put(&mut rec, &fields, "CREATEDBY", draft.created_by.trim())?;
        put(&mut rec, &fields, "ASSIGNEDTO", draft.assigned_to.trim())?;
        put(&mut rec, &fields, "CREATEDON", stamp)?;
        put(&mut rec, &fields, "MODIFIEDON", stamp)?;
        put(&mut rec, &fields, "ROLL", "F")?;
        put(&mut rec, &fields, "ARCHIVE", "F")?;
        put(&mut rec, &fields, "STATUS", &IssueStatus::New.letter().to_string())?;
        put(&mut rec, &fields, "PERCENT", "0")?;
        put(&mut rec, &fields, "CLEARED", "F")?;
        put(&mut rec, &fields, "CWDATED", stamp)?;
        put(&mut rec, &fields, "CWDATER", stamp)?;
        put(&mut rec, &fields, "ID", &id)?;
        let row = self.table.records.len();
        self.table.records.push(rec);
        self.issues.push(Issue::decode(&self.table, row, &self.fpt)?);
        self.rows.push(row);
        Ok(self.issues.len() - 1)
    }

    /// Set the status/percent pair. `C` stamps `COMPLETEON`/`COMPLETEBY`;
    /// `N` clears them and forces percent 0.
    pub fn set_status(
        &mut self,
        index: usize,
        status: IssueStatus,
        percent: Option<i64>,
        user: &str,
        stamp: &str,
    ) -> Result<()> {
        if let Some(p) = percent {
            if !(0..=100).contains(&p) {
                return Err(CoreError::Format(format!("TS percent {p} outside 0..=100")));
            }
        }
        let row = *self
            .rows
            .get(index)
            .ok_or_else(|| CoreError::Format(format!("TS status index {index} out of range")))?;
        let percent = percent.unwrap_or(match status {
            IssueStatus::Complete => 100,
            IssueStatus::New => 0,
        });
        let fields = self.table.fields.clone();
        {
            let rec = self
                .table
                .records
                .get_mut(row)
                .ok_or_else(|| CoreError::Format(format!("TS row {row} missing")))?;
            put(rec, &fields, "STATUS", &status.letter().to_string())?;
            put(rec, &fields, "PERCENT", &percent.to_string())?;
            put(rec, &fields, "MODIFIEDON", stamp)?;
            put(rec, &fields, "CWDATER", stamp)?;
            match status {
                IssueStatus::Complete => {
                    put(rec, &fields, "COMPLETEON", stamp)?;
                    put(rec, &fields, "COMPLETEBY", user.trim())?;
                }
                IssueStatus::New => {
                    put(rec, &fields, "COMPLETEON", "")?;
                    put(rec, &fields, "COMPLETEBY", "")?;
                }
            }
        }
        self.issues[index] = Issue::decode(&self.table, row, &self.fpt)?;
        Ok(())
    }

    /// Complete an issue with a resolution memo (`RESOLUTION`).
    pub fn resolve(&mut self, index: usize, resolution: &str, user: &str, stamp: &str) -> Result<()> {
        let row = *self
            .rows
            .get(index)
            .ok_or_else(|| CoreError::Format(format!("TS resolve index {index} out of range")))?;
        let block = put_memo(&mut self.fpt, resolution.as_bytes());
        let fields = self.table.fields.clone();
        {
            let rec = self
                .table
                .records
                .get_mut(row)
                .ok_or_else(|| CoreError::Format(format!("TS row {row} missing")))?;
            put(rec, &fields, "RESOLUTION", &memo_ref(block))?;
        }
        self.set_status(index, IssueStatus::Complete, Some(100), user, stamp)
    }

    /// Reopen a completed issue (`STATUS='N'`, percent 0, resolution kept).
    pub fn reopen(&mut self, index: usize, user: &str, stamp: &str) -> Result<()> {
        self.set_status(index, IssueStatus::New, Some(0), user, stamp)
    }

    /// Tombstone one issue (`Remove`).
    pub fn remove_at(&mut self, index: usize) -> Result<()> {
        let row = *self
            .rows
            .get(index)
            .ok_or_else(|| CoreError::Format(format!("TS remove {index} out of range")))?;
        if let Some(rec) = self.table.records.get_mut(row) {
            rec[0] = DELETED;
        }
        if let Some(i) = self.issues.get_mut(index) {
            i.deleted = true;
        }
        Ok(())
    }

    pub fn encode(&self) -> Vec<u8> {
        self.table.encode()
    }

    pub fn encode_fpt(&self) -> Vec<u8> {
        self.fpt.encode()
    }

    pub fn save(&self, dbf: impl AsRef<Path>, fpt: impl AsRef<Path>) -> Result<()> {
        self.table.write_path(dbf)?;
        self.fpt.write_path(fpt)
    }

    pub fn table(&self) -> &Table {
        &self.table
    }

    pub fn fpt(&self) -> &Fpt {
        &self.fpt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotation_types_round_trip() {
        for t in [
            AnnotationType::Annotation,
            AnnotationType::Note,
            AnnotationType::Tickmark,
            AnnotationType::Reference,
            AnnotationType::Issue,
            AnnotationType::UserText,
        ] {
            assert_eq!(AnnotationType::parse(&t.letter().to_string()).unwrap(), t);
        }
        assert!(AnnotationType::parse("X").is_err());
        assert!(AnnotationType::parse("").is_err());
        assert!(AnnotationType::Tickmark.argument_is_id());
        assert!(!AnnotationType::UserText.argument_is_id());
    }

    #[test]
    fn anchors_classify() {
        assert!(Anchor::global().is_global());
        assert!(!Anchor::account("AM").is_global());
        assert_eq!(Anchor::mapping("MP").object_type, "MP");
        assert!(Anchor::default().is_empty());
    }

    #[test]
    fn issue_status_domain() {
        assert_eq!(IssueStatus::parse("N").unwrap(), IssueStatus::New);
        assert_eq!(IssueStatus::parse("C").unwrap(), IssueStatus::Complete);
        assert!(IssueStatus::parse("X").is_err());
        assert_eq!(IssueStatus::Complete.letter(), 'C');
    }

    #[test]
    fn memo_ref_is_right_aligned() {
        assert_eq!(memo_ref(7), "         7");
        let mut f = empty_fpt();
        assert_eq!(put_memo(&mut f, b""), 0);
        let b = put_memo(&mut f, b"hello");
        assert_eq!(b, 1);
        assert_eq!(f.memo(&memo_ref(b).into_bytes()).unwrap(), b"hello");
        assert_eq!(f.next_free, 2);
    }
}

