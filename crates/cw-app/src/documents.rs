//! cw-app document layer - the `SH` Document Manager register, `WR` reference
//! links and `WT` note rows (P5.G G4).
//!
//! Contract: `docs\specs\documents.md` v1.0, grounded in
//! `inventory\dbf-schema.tsv` and the frozen corpora under `tests\golden\dbf`.
//!
//! ## SH - document register
//!
//! `SH` is the Document Manager: 124 fields / 1737-byte records in every
//! corpus. `LINENO` (N9) is the display order, `TYPE` the node class,
//! `SCH_DESC` the title, `SCHEDULE` the reference token, `PRG` the owning
//! program (`ac` = engagement, `IR` = index/report rows, `EX` = exported).
//! `DOCUMENTID` is the tree parent (`F60`: carried but empty in all three
//! corpora) and `SRCGUID` the cross-engagement copy source. Live-row census:
//! hull2024 118, mwgh2025 94, corp2026 93 (with 546/164/107 tombstones).
//!
//! Observed `TYPE` classes (live rows, corp2026):
//!
//! | TYPE | rows | meaning | NAME |
//! |---|---|---|---|
//! | `*` | 48 | link/analysis document | `*` |
//! | `H` | 22 | heading (folder) | blank |
//! | `F` | 7 | CaseView form (WPG response doc) | blank |
//! | `M` | 4 | misc/letter document | blank |
//! | `A` | 6 | analysis (`LS`/`TB`/`AJ`/`AN`) | tag |
//! | blank | 6 | index/report row | blank |
//!
//! `LINENO` lattice: app rows start at 1,000,000 and step 32,000
//! (`1000000, 1032000, 1064000, ...`); inserted rows halve a gap
//! (`1550399`, `1556798`, `1783999`, `3191999`), so values are unique among
//! app rows but not across `PRG='IR'` index rows (which share `0`). Blank
//! `LINENO` exists (corp2026 `EX` rows). Every live `ac` row carries
//! `ARCHIVE='Y'` and `ROLL='Y'`; `LOCKDOWN='Y'` marks the locked set (F68).
//!
//! Deletion law (measured): CW tombstones the record (`0x2A`) and does **not**
//! set `DELETED`/`HIDDEN`/`DELETEDON`; corp2026 tombstones carry
//! `RECYCLEDAT='0000000000'` (a zero memo pointer, F64). Deleting to the
//! Recycle Bin is therefore a tombstone + recycle stamp, while the "Recycle
//! bin" heading row is an ordinary `H` node. `restore` clears the tombstone.
//!
//! ## WR - references
//!
//! 9 fields / 133 bytes. One row = one reference memo attached to a document
//! (`DOCGUID` -> live `SH.CWGUID`; `FROM_REF` target ref text, `REV_CODE`
//! revision code, `MEMO_NO` slot, `LINENO` constant sentinel `111111` in
//! every corpus row). Live counts hull 3 / mwgh 2 / corp 1; **every live
//! `DOCGUID` resolves**, the deleted rows are the danglers (F60). `MEMO` is
//! the payload (memo block in `WR.fpt`/`WR.FPT`).
//!
//! ## WT - note text/value rows
//!
//! 60 fields / 571 bytes, **0 rows in all three corpora**: the layout is
//! `LINENO`+`DOCGUID`+`TEXT`+`TYPE`, four parallel 13-slot blocks
//! (`A..M` N19.2 values, `AF..MF` C12 formats, `AN..MN` C2 codes,
//! `AC..MC` C2 codes), then `PERCENT`/`CWGUID`/audit stamps. Reader + writer
//! are established synthetically; no corpus value law exists (residual).
//!
//! ## What is deliberately not here
//!
//! Signoff columns (`A1..A8`/`AD`/`AC`/`AU`), `SIGN`, and `HI` audit rows
//! belong to [`crate::signoff`]; CaseView cell text to `cw-cv`; document file
//! bodies are opaque (`.cvw`/`.docx` payloads are copied, never parsed).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use cw_core::dbf::Table;
use cw_core::fpt::Fpt;
use cw_core::guid::{CwGuid, Rng};
use cw_core::{CoreError, Result};

/// Physical `SH` geometry (identical in all corpora).
pub const SH_FIELD_COUNT: usize = 124;
pub const SH_RECORD_LEN: usize = 1737;
/// Physical `WR` geometry.
pub const WR_FIELD_COUNT: usize = 9;
pub const WR_RECORD_LEN: usize = 133;
/// Physical `WT` geometry.
pub const WT_FIELD_COUNT: usize = 60;
pub const WT_RECORD_LEN: usize = 571;

/// Primary `LINENO` step between adjacent app rows (corpus lattice).
pub const LINENO_STEP: i64 = 32_000;
/// First app-row `LINENO` in every corpus.
pub const LINENO_BASE: i64 = 1_000_000;
/// `WR.LINENO` sentinel written by CW in every row.
pub const WR_LINENO: &str = "111111";

const LIVE: u8 = b' ';
const DELETED: u8 = b'*';

/// `SH` column names in physical order (schema-drift guard).
pub static SH_COLUMNS: [&str; SH_FIELD_COUNT] = [
    "LINENO", "PRG", "SCHEDULE", "NAME", "TYPE", "SCH_DESC", "ACCESS", "S1", "SD1", "S2", "SD2",
    "S3", "SD3", "S4", "SD4", "S5", "SD5", "S6", "SD6", "S7", "SD7", "S8", "SD8", "A1", "AD1",
    "AC1", "AU1", "A2", "AD2", "AC2", "AU2", "A3", "AD3", "AC3", "AU3", "A4", "AD4", "AC4", "AU4",
    "A5", "AD5", "AC5", "AU5", "A6", "AD6", "AC6", "AU6", "A7", "AD7", "AC7", "AU7", "A8", "AD8",
    "AC8", "AU8", "AC_FROM", "AC_TO", "DATE_FROM", "DATE_TO", "AMT_FROM", "AMT_TO", "OTHER", "VIEW",
    "H1", "H2", "RND", "B_PRT", "A_CODE", "UPD", "DGN", "MEMO", "SIGN", "MASK", "GROUP", "SORT",
    "SKIPPRINT", "CONS_VIEW", "FE", "INITIALS", "ORIENT", "ADJ", "ARRANGE", "ANFLG", "AMTCHG",
    "ZBAL", "ENTITY", "ACT_TYPE", "CVGROUP", "PERIOD", "CVID", "CVDIST", "BALTYPE", "DISCRETE",
    "PARENTONLY", "ARCHIVE", "ROLL", "LOCKDOWN", "TASKUSER", "CWGUID", "EXINFO", "CWDATE",
    "DELETED", "CREATEDON", "VIEWER", "BINDATA", "LAYOUT", "SRCGUID", "CUSTOM", "DELETEDON",
    "DELETEDBY", "HIDDEN", "RECYCLEDAT", "LANGUAGE", "ROLESET", "DOCUMENTID", "MTYPE",
    "PLHOLDTYPE", "INCLUDEAJ2", "INCLUDEAJ3", "DELIV", "DUEDATE", "MATERIAL", "INPROGRESS",
    "DELIV_DATE",
];

/// `WR` column names in physical order.
pub static WR_COLUMNS: [&str; WR_FIELD_COUNT] = [
    "DOCGUID", "FROM_REF", "REV_CODE", "MEMO_NO", "MEMO", "LINENO", "CWGUID", "CWDATE",
    "CREATEDON",
];

/// `WT` column names in physical order.
pub static WT_COLUMNS: [&str; WT_FIELD_COUNT] = [
    "LINENO", "DOCGUID", "TEXT", "TYPE", "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K",
    "L", "M", "AF", "BF", "CF", "DF", "EF", "FF", "GF", "HF", "IF", "JF", "KF", "LF", "MF", "AN",
    "BN", "CN", "DN", "EN", "FN", "GN", "HN", "IN", "JN", "KN", "LN", "MN", "AC", "BC", "CC", "DC",
    "EC", "FC", "GC", "HC", "IC", "JC", "KC", "LC", "MC", "PERCENT", "CWGUID", "CWDATE",
    "CREATEDON",
];

/// The four parallel `WT` value families (suffix `n` = `A..M`).
pub const WT_SLOTS: [char; 13] = [
    'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M',
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

fn check_columns(table: &Table, name: &str, want: &[&str], record_len: usize) -> Result<()> {
    if table.fields.len() != want.len() {
        return Err(CoreError::Format(format!(
            "{name}: {} columns, want {}",
            table.fields.len(),
            want.len()
        )));
    }
    if table.record_len as usize != record_len {
        return Err(CoreError::Format(format!(
            "{name}: record len {}, want {record_len}",
            table.record_len
        )));
    }
    for (got, want) in table.fields.iter().zip(want.iter()) {
        if got.name != *want {
            return Err(CoreError::Format(format!(
                "{name}: column drift: got {}, want {want}",
                got.name
            )));
        }
    }
    Ok(())
}

/// Width- and type-checked cell write (same shape as the other CW writers).
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

/// Append a memo payload to an FPT image, returning the block reference.
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

/// Case-insensitive `<dir>\<stem><suffix>.<ext>` lookup.
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
// SH - document register / tree
// ---------------------------------------------------------------------------

/// `SH.TYPE` classes observed live in the corpora.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentKind {
    /// Blank `TYPE` - `PRG='IR'` index/report row.
    Index,
    /// `H` - heading / folder node.
    Heading,
    /// `A` - analysis document (`NAME` carries `LS`/`TB`/`AJ`/`AN`/...).
    Analysis,
    /// `F` - CaseView form (WPG response document).
    Form,
    /// `M` - miscellaneous/letter document.
    Misc,
    /// `*` - link/analysis star document (`NAME='*'`).
    Star,
    /// A lettered class outside the corpus-derived enum (observed: S/J/B/R/r/t/Q
    /// and non-ASCII on hull2024 rows past the readable frontier). Preserved
    /// verbatim; semantics documented as a census, not inferred.
    Other(char),
}

impl DocumentKind {
    pub fn letter(self) -> char {
        match self {
            DocumentKind::Index => ' ',
            DocumentKind::Heading => 'H',
            DocumentKind::Analysis => 'A',
            DocumentKind::Form => 'F',
            DocumentKind::Misc => 'M',
            DocumentKind::Star => '*',
            DocumentKind::Other(c) => c,
        }
    }

    pub fn parse(s: &str) -> Result<DocumentKind> {
        match s.trim() {
            "" => Ok(DocumentKind::Index),
            "H" => Ok(DocumentKind::Heading),
            "A" => Ok(DocumentKind::Analysis),
            "F" => Ok(DocumentKind::Form),
            "M" => Ok(DocumentKind::Misc),
            "*" => Ok(DocumentKind::Star),
            other if other.chars().count() == 1 => {
                let c = other.chars().next().unwrap();
                Ok(DocumentKind::Other(c))
            }
            other => Err(CoreError::Format(format!(
                "SH type {other:?} outside blank/H/A/F/M/*"
            ))),
        }
    }

    /// Heading nodes group other documents in the manager view.
    pub fn is_heading(self) -> bool {
        self == DocumentKind::Heading
    }

    /// Forms are the CaseView response documents (F80/F81).
    pub fn is_form(self) -> bool {
        self == DocumentKind::Form
    }
}

/// One decoded `SH` row (live rows only; tombstones are counted, not decoded).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShDocument {
    /// `LINENO` (None when blank - corp2026 `EX` rows).
    pub lineno: Option<i64>,
    /// `PRG` - `ac` (engagement), `IR` (index), `EX` (exported).
    pub prg: String,
    /// `SCHEDULE` reference token.
    pub schedule: String,
    /// `NAME` - two-char tag (`LS`/`TB`/`AJ`/`AN`/`*`/blank).
    pub name: String,
    pub kind: DocumentKind,
    /// `SCH_DESC` - display title.
    pub title: String,
    /// `ACCESS` access mask.
    pub access: String,
    /// `CVID` - CaseView document id (`04000RG2`, blank for non-forms).
    pub cvid: String,
    /// `VIEWER` - blank, or `N` on the 14 hull LS analyses.
    pub viewer: String,
    /// `DOCUMENTID` - tree parent CWGUID (empty in every corpus).
    pub document_id: String,
    /// `SRCGUID` - cross-engagement copy source (empty in every corpus).
    pub src_guid: String,
    pub entity: String,
    pub archive: bool,
    pub roll: bool,
    pub lockdown: bool,
    pub hidden: bool,
    pub delivered: bool,
    pub sign: bool,
    pub cwguid: String,
    pub cw_date: String,
    pub created_on: String,
    /// Physical record index in the table.
    pub row: usize,
}

impl ShDocument {
    pub fn is_index(&self) -> bool {
        self.kind == DocumentKind::Index
    }

    pub fn is_app(&self) -> bool {
        self.prg != "IR" && self.prg != "EX"
    }

    /// Title, falling back to the schedule token.
    pub fn label(&self) -> &str {
        if self.title.is_empty() {
            &self.schedule
        } else {
            &self.title
        }
    }

    pub fn has_parent(&self) -> bool {
        !self.document_id.is_empty()
    }

    fn decode(t: &Table, r: usize) -> Result<ShDocument> {
        Ok(ShDocument {
            lineno: number(t, r, "LINENO"),
            prg: trimmed(t, r, "PRG"),
            schedule: cell(t, r, "SCHEDULE").trim().to_string(),
            name: cell(t, r, "NAME").trim().to_string(),
            kind: DocumentKind::parse(&trimmed(t, r, "TYPE"))?,
            title: cell(t, r, "SCH_DESC").trim_end().to_string(),
            access: trimmed(t, r, "ACCESS"),
            cvid: trimmed(t, r, "CVID"),
            viewer: trimmed(t, r, "VIEWER"),
            document_id: trimmed(t, r, "DOCUMENTID"),
            src_guid: trimmed(t, r, "SRCGUID"),
            entity: trimmed(t, r, "ENTITY"),
            archive: flag(t, r, "ARCHIVE"),
            roll: flag(t, r, "ROLL"),
            lockdown: flag(t, r, "LOCKDOWN"),
            hidden: flag(t, r, "HIDDEN"),
            delivered: flag(t, r, "DELIV"),
            sign: flag(t, r, "SIGN"),
            cwguid: trimmed(t, r, "CWGUID"),
            cw_date: trimmed(t, r, "CWDATE"),
            created_on: trimmed(t, r, "CREATEDON"),
            row: r,
        })
    }
}

/// Per-book document counts (live rows only).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocumentCensus {
    pub live: usize,
    pub deleted: usize,
    pub by_type: BTreeMap<char, usize>,
    pub by_prg: BTreeMap<String, usize>,
    pub headings: usize,
    pub forms: usize,
    pub analyses: usize,
    pub index_rows: usize,
    pub with_parent: usize,
    pub roots: usize,
    pub archived: usize,
    pub rolled: usize,
    pub locked: usize,
    pub blank_lineno: usize,
    pub cycles: usize,
}

/// Tree node for the Document Manager projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentNode {
    pub guid: String,
    pub title: String,
    pub kind: DocumentKind,
    pub schedule: String,
    pub lineno: Option<i64>,
    pub depth: usize,
    pub children: Vec<DocumentNode>,
}

/// Write request for [`DocumentBook::add`] / `insert_after`.
#[derive(Clone, Debug)]
pub struct DocumentDraft {
    pub kind: DocumentKind,
    pub prg: String,
    pub schedule: String,
    pub name: String,
    pub title: String,
    pub cvid: String,
    pub entity: String,
    pub parent: String,
    /// Explicit `LINENO`; None = append on the 32,000 lattice.
    pub lineno: Option<i64>,
    /// `ARCHIVE`/`ROLL` (corpus: `Y` on every live `ac` row).
    pub archived: bool,
    pub locked: bool,
}

impl DocumentDraft {
    fn base(kind: DocumentKind) -> DocumentDraft {
        DocumentDraft {
            kind,
            prg: "ac".into(),
            schedule: String::new(),
            name: String::new(),
            title: String::new(),
            cvid: String::new(),
            entity: String::new(),
            parent: String::new(),
            lineno: None,
            archived: kind != DocumentKind::Index,
            locked: false,
        }
    }

    /// New heading/folder node (`TYPE='H'`, title in `SCH_DESC`).
    pub fn heading(title: &str) -> DocumentDraft {
        let mut d = DocumentDraft::base(DocumentKind::Heading);
        d.title = title.to_string();
        d
    }

    /// New CaseView form (`TYPE='F'` with `CVID`).
    pub fn form(schedule: &str, title: &str, cvid: &str) -> DocumentDraft {
        let mut d = DocumentDraft::base(DocumentKind::Form);
        d.schedule = schedule.trim().to_string();
        d.title = title.to_string();
        d.cvid = cvid.trim().to_string();
        d
    }

    /// New analysis document (`TYPE='A'`, `NAME` tag).
    pub fn analysis(name: &str, schedule: &str, title: &str) -> DocumentDraft {
        let mut d = DocumentDraft::base(DocumentKind::Analysis);
        d.name = name.trim().to_string();
        d.schedule = schedule.trim().to_string();
        d.title = title.to_string();
        d
    }

    /// New star/link document (`TYPE='*'`, `NAME='*'`).
    pub fn star(title: &str) -> DocumentDraft {
        let mut d = DocumentDraft::base(DocumentKind::Star);
        d.name = "*".into();
        d.title = title.to_string();
        d
    }

    /// Index/report row (`TYPE` blank, `PRG='IR'`).
    pub fn index(schedule: &str) -> DocumentDraft {
        let mut d = DocumentDraft::base(DocumentKind::Index);
        d.prg = "IR".into();
        d.schedule = schedule.trim().to_string();
        d
    }

    pub fn parent(mut self, guid: &str) -> DocumentDraft {
        self.parent = guid.trim().to_string();
        self
    }

    pub fn at(mut self, lineno: i64) -> DocumentDraft {
        self.lineno = Some(lineno);
        self
    }

    pub fn locked(mut self) -> DocumentDraft {
        self.locked = true;
        self
    }
}

/// The `SH` table (no memo sidecar is read: document bodies are opaque).
#[derive(Clone, Debug)]
pub struct DocumentBook {
    table: Table,
    rows: Vec<usize>,
    docs: Vec<ShDocument>,
}

impl DocumentBook {
    /// Open `<dir>\<stem>SH.dbf` (sidecar not required for tree ops).
    pub fn open(dir: impl AsRef<Path>, stem: &str) -> Result<DocumentBook> {
        let dir = dir.as_ref();
        let dbf = sidecar(dir, stem, "SH", "dbf")?;
        Self::from_table(Table::parse_path(dbf)?)
    }

    pub fn from_bytes(dbf: &[u8]) -> Result<DocumentBook> {
        Self::from_table(Table::parse(dbf)?)
    }

    pub fn from_table(table: Table) -> Result<DocumentBook> {
        check_columns(&table, "SH", &SH_COLUMNS, SH_RECORD_LEN)?;
        let mut rows = Vec::new();
        let mut docs = Vec::new();
        for r in 0..table.count() {
            if table.is_deleted(r) {
                continue;
            }
            docs.push(ShDocument::decode(&table, r)?);
            rows.push(r);
        }
        Ok(DocumentBook { table, rows, docs })
    }

    /// Live documents in physical row order.
    pub fn documents(&self) -> &[ShDocument] {
        &self.docs
    }

    pub fn count(&self) -> usize {
        self.docs.len()
    }

    pub fn index_of(&self, guid: &str) -> Option<usize> {
        self.docs.iter().position(|d| d.cwguid == guid.trim())
    }

    pub fn resolve(&self, guid: &str) -> Option<&ShDocument> {
        self.index_of(guid).map(|i| &self.docs[i])
    }

    /// Exact `SCHEDULE` lookup (the token as stored, trimmed).
    pub fn by_schedule(&self, schedule: &str) -> Vec<&ShDocument> {
        self.docs
            .iter()
            .filter(|d| d.schedule == schedule.trim())
            .collect()
    }

    pub fn headings(&self) -> Vec<&ShDocument> {
        self.docs.iter().filter(|d| d.kind.is_heading()).collect()
    }

    pub fn forms(&self) -> Vec<&ShDocument> {
        self.docs.iter().filter(|d| d.kind.is_form()).collect()
    }

    pub fn analyses(&self) -> Vec<&ShDocument> {
        self.docs.iter().filter(|d| d.kind == DocumentKind::Analysis).collect()
    }

    /// Live rows ordered by `(LINENO, physical row)` - the register order.
    pub fn ordered(&self) -> Vec<&ShDocument> {
        let mut out: Vec<&ShDocument> = self.docs.iter().collect();
        out.sort_by(|a, b| {
            a.lineno
                .unwrap_or(i64::MAX)
                .cmp(&b.lineno.unwrap_or(i64::MAX))
                .then_with(|| a.row.cmp(&b.row))
        });
        out
    }

    /// The `Recycle bin` heading, when present (corp2026/hull2024).
    pub fn recycle_bin(&self) -> Option<&ShDocument> {
        self.headings()
            .into_iter()
            .find(|h| h.title.eq_ignore_ascii_case("Recycle bin"))
    }

    pub fn census(&self) -> DocumentCensus {
        let mut c = DocumentCensus {
            live: self.docs.len(),
            deleted: self.table.count().saturating_sub(self.docs.len()),
            ..Default::default()
        };
        for d in &self.docs {
            *c.by_type.entry(d.kind.letter()).or_insert(0) += 1;
            *c.by_prg.entry(d.prg.clone()).or_insert(0) += 1;
            match d.kind {
                DocumentKind::Heading => c.headings += 1,
                DocumentKind::Form => c.forms += 1,
                DocumentKind::Analysis => c.analyses += 1,
                DocumentKind::Index => c.index_rows += 1,
                _ => {}
            }
            if d.has_parent() {
                c.with_parent += 1;
            } else {
                c.roots += 1;
            }
            if d.archive {
                c.archived += 1;
            }
            if d.roll {
                c.rolled += 1;
            }
            if d.lockdown {
                c.locked += 1;
            }
            if d.lineno.is_none() {
                c.blank_lineno += 1;
            }
        }
        c.cycles = self.cycle_nodes().len();
        c
    }

    /// Parent map (child guid -> parent guid) over live rows.
    fn parent_map(&self) -> BTreeMap<&str, &str> {
        self.docs
            .iter()
            .filter(|d| d.has_parent())
            .map(|d| (d.cwguid.as_str(), d.document_id.as_str()))
            .collect()
    }

    /// True when adding `child` under `parent` would create a cycle.
    pub fn would_cycle(&self, child: &str, parent: &str) -> bool {
        let map = self.parent_map();
        let child = child.trim();
        let mut cur = parent.trim();
        let mut seen = BTreeSet::new();
        loop {
            if cur == child {
                return true;
            }
            if !seen.insert(cur) {
                return true;
            }
            match map.get(cur) {
                Some(p) if !p.is_empty() => cur = p,
                _ => return false,
            }
        }
    }

    /// Nodes whose parent chain loops (should be empty; exposed for repair).
    pub fn cycle_nodes(&self) -> Vec<String> {
        let map = self.parent_map();
        let mut out = Vec::new();
        for d in &self.docs {
            let mut seen = BTreeSet::new();
            let mut cur = d.cwguid.as_str();
            loop {
                if !seen.insert(cur) {
                    out.push(d.cwguid.clone());
                    break;
                }
                match map.get(cur) {
                    Some(p) if !p.is_empty() => cur = p,
                    _ => break,
                }
            }
        }
        out
    }

    /// Document Manager tree, roots in register order.
    pub fn tree(&self) -> Vec<DocumentNode> {
        let mut children: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        let mut roots: Vec<usize> = Vec::new();
        for (i, d) in self.docs.iter().enumerate() {
            if d.has_parent() && self.resolve(&d.document_id).is_some() && !self.would_cycle(&d.cwguid, &d.document_id) {
                children.entry(d.document_id.as_str()).or_default().push(i);
            } else {
                roots.push(i);
            }
        }
        let key = |i: usize| {
            let d = &self.docs[i];
            (d.lineno.unwrap_or(i64::MAX), d.row)
        };
        roots.sort_by_key(|i| key(*i));
        fn build(book: &DocumentBook, idx: usize, depth: usize, children: &BTreeMap<&str, Vec<usize>>) -> DocumentNode {
            let d = &book.docs[idx];
            let mut kids: Vec<usize> = children.get(d.cwguid.as_str()).cloned().unwrap_or_default();
            kids.sort_by_key(|i| {
                let x = &book.docs[*i];
                (x.lineno.unwrap_or(i64::MAX), x.row)
            });
            DocumentNode {
                guid: d.cwguid.clone(),
                title: d.label().to_string(),
                kind: d.kind,
                schedule: d.schedule.clone(),
                lineno: d.lineno,
                depth,
                children: kids
                    .into_iter()
                    .map(|i| build(book, i, depth + 1, children))
                    .collect(),
            }
        }
        roots.into_iter().map(|i| build(self, i, 0, &children)).collect()
    }

    /// Depth-first `(depth, document)` projection of [`Self::tree`].
    pub fn preorder(&self) -> Vec<(usize, &ShDocument)> {
        fn walk<'a>(book: &'a DocumentBook, nodes: &[DocumentNode], out: &mut Vec<(usize, &'a ShDocument)>) {
            for n in nodes {
                if let Some(i) = book.index_of(&n.guid) {
                    out.push((n.depth, &book.docs[i]));
                }
                walk(book, &n.children, out);
            }
        }
        let mut out = Vec::new();
        walk(self, &self.tree(), &mut out);
        out
    }

    /// `/`-joined heading titles above `guid` (folder path).
    pub fn path_of(&self, guid: &str) -> Option<String> {
        let mut d = self.resolve(guid)?;
        let mut parts = vec![d.label().to_string()];
        let mut seen = BTreeSet::new();
        while d.has_parent() {
            if !seen.insert(d.cwguid.as_str()) {
                break;
            }
            match self.resolve(&d.document_id) {
                Some(p) => {
                    parts.push(p.label().to_string());
                    d = p;
                }
                None => break,
            }
        }
        parts.reverse();
        Some(parts.join("/"))
    }

    /// Next app-row `LINENO` (max + 32,000; 1,000,000 when empty).
    pub fn next_lineno(&self) -> i64 {
        self.docs
            .iter()
            .filter(|d| !d.is_index())
            .filter_map(|d| d.lineno)
            .max()
            .map(|m| m + LINENO_STEP)
            .unwrap_or(LINENO_BASE)
    }

    /// `LINENO` strictly between `after` (index of an existing doc) and the
    /// next doc in register order, or None when the gap is exhausted.
    pub fn lineno_after(&self, index: usize) -> Option<i64> {
        let d = self.docs.get(index)?;
        let lo = d.lineno?;
        let hi = self
            .docs
            .iter()
            .filter_map(|x| x.lineno)
            .filter(|v| *v > lo)
            .min();
        match hi {
            Some(h) if h - lo > 1 => Some(lo + (h - lo) / 2),
            None => Some(lo + LINENO_STEP),
            _ => None,
        }
    }

    fn write_cell(&mut self, index: usize, name: &str, value: &str) -> Result<()> {
        let row = *self
            .rows
            .get(index)
            .ok_or_else(|| CoreError::Format(format!("SH index {index} out of range")))?;
        let fields = self.table.fields.clone();
        let rec = self
            .table
            .records
            .get_mut(row)
            .ok_or_else(|| CoreError::Format(format!("SH row {row} missing")))?;
        put(rec, &fields, name, value)
    }

    fn reload(&mut self, index: usize) -> Result<()> {
        let row = self.rows[index];
        self.docs[index] = ShDocument::decode(&self.table, row)?;
        Ok(())
    }

    /// Append one document. `LINENO` defaults to the 32,000 lattice.
    pub fn add(&mut self, draft: DocumentDraft, rng: &mut Rng, stamp: &str) -> Result<usize> {
        if draft.schedule.len() > 40 {
            return Err(CoreError::Format("SH SCHEDULE > 40 chars".into()));
        }
        if draft.title.len() > 150 {
            return Err(CoreError::Format("SH SCH_DESC > 150 chars".into()));
        }
        if draft.kind.is_form() && draft.cvid.is_empty() {
            return Err(CoreError::Format("SH form requires a CVID".into()));
        }
        if !draft.parent.is_empty() {
            if self.resolve(&draft.parent).is_none() {
                return Err(CoreError::Format(format!(
                    "SH parent {} is not a live document",
                    draft.parent
                )));
            }
        }
        let guid = CwGuid::allocate_for_table("SH", rng).encode();
        let lineno = draft.lineno.unwrap_or_else(|| self.next_lineno());
        let mut rec = vec![LIVE; SH_RECORD_LEN];
        let fields = self.table.fields.clone();
        put(&mut rec, &fields, "LINENO", &lineno.to_string())?;
        put(&mut rec, &fields, "PRG", draft.prg.trim())?;
        put(&mut rec, &fields, "SCHEDULE", draft.schedule.trim())?;
        put(&mut rec, &fields, "NAME", draft.name.trim())?;
        put(&mut rec, &fields, "TYPE", &draft.kind.letter().to_string())?;
        put(&mut rec, &fields, "SCH_DESC", draft.title.trim_end())?;
        put(&mut rec, &fields, "CVID", draft.cvid.trim())?;
        put(&mut rec, &fields, "ENTITY", draft.entity.trim())?;
        put(&mut rec, &fields, "DOCUMENTID", draft.parent.trim())?;
        put(&mut rec, &fields, "ARCHIVE", if draft.archived { "Y" } else { "F" })?;
        put(&mut rec, &fields, "ROLL", if draft.archived { "Y" } else { "F" })?;
        put(&mut rec, &fields, "LOCKDOWN", if draft.locked { "Y" } else { "F" })?;
        put(&mut rec, &fields, "CWGUID", &guid)?;
        put(&mut rec, &fields, "CWDATE", stamp)?;
        put(&mut rec, &fields, "CREATEDON", stamp)?;
        let row = self.table.records.len();
        self.table.records.push(rec);
        self.docs.push(ShDocument::decode(&self.table, row)?);
        self.rows.push(row);
        Ok(self.docs.len() - 1)
    }

    /// Insert a document directly after `index` (midpoint `LINENO`). When the
    /// gap is exhausted the register is re-spaced and the add retried.
    pub fn insert_after(&mut self, index: usize, draft: DocumentDraft, rng: &mut Rng, stamp: &str) -> Result<usize> {
        let mut draft = draft;
        match self.lineno_after(index) {
            Some(v) => draft.lineno = Some(v),
            None => {
                self.renumber()?;
                if let Some(v) = self.lineno_after(index) {
                    draft.lineno = Some(v);
                }
            }
        }
        self.add(draft, rng, stamp)
    }

    /// Re-space every live non-index row on the 32,000 lattice, preserving
    /// register order (`PRG='IR'` rows keep their duplicate `0`).
    pub fn renumber(&mut self) -> Result<()> {
        let mut order: Vec<usize> = self
            .docs
            .iter()
            .enumerate()
            .filter(|(_, d)| !d.is_index())
            .map(|(i, _)| i)
            .collect();
        order.sort_by_key(|i| {
            let d = &self.docs[*i];
            (d.lineno.unwrap_or(i64::MAX), d.row)
        });
        for (k, i) in order.iter().enumerate() {
            let v = LINENO_BASE + LINENO_STEP * k as i64;
            self.write_cell(*i, "LINENO", &v.to_string())?;
            self.reload(*i)?;
        }
        Ok(())
    }

    /// Rename a document (`SCH_DESC`).
    pub fn rename(&mut self, index: usize, title: &str, stamp: &str) -> Result<()> {
        if title.len() > 150 {
            return Err(CoreError::Format("SH SCH_DESC > 150 chars".into()));
        }
        self.write_cell(index, "SCH_DESC", title.trim_end())?;
        self.write_cell(index, "CWDATE", stamp)?;
        self.reload(index)
    }

    /// Re-schedule a document (`SCHEDULE`).
    pub fn set_schedule(&mut self, index: usize, schedule: &str) -> Result<()> {
        self.write_cell(index, "SCHEDULE", schedule.trim())?;
        self.reload(index)
    }

    /// Move a document under `parent` (None = root). Cycles are rejected.
    pub fn move_to(&mut self, index: usize, parent: Option<&str>, stamp: &str) -> Result<()> {
        let child = self.docs[index].cwguid.clone();
        let parent = parent.map(|p| p.trim()).unwrap_or("");
        if !parent.is_empty() {
            if self.resolve(parent).is_none() {
                return Err(CoreError::Format(format!("SH move: parent {parent} not live")));
            }
            if self.would_cycle(&child, parent) {
                return Err(CoreError::Format(format!(
                    "SH move: {child} -> {parent} would create a cycle"
                )));
            }
        }
        self.write_cell(index, "DOCUMENTID", parent)?;
        self.write_cell(index, "CWDATE", stamp)?;
        self.reload(index)
    }

    /// Set the `ARCHIVE`/`ROLL` pair (corpus: `Y` on every live `ac` row).
    pub fn set_archived(&mut self, index: usize, archived: bool) -> Result<()> {
        self.write_cell(index, "ARCHIVE", if archived { "Y" } else { "F" })?;
        self.write_cell(index, "ROLL", if archived { "Y" } else { "F" })?;
        self.reload(index)
    }

    /// Delete to the Recycle Bin = physical tombstone (measured CW law).
    pub fn delete_at(&mut self, index: usize) -> Result<()> {
        let row = *self
            .rows
            .get(index)
            .ok_or_else(|| CoreError::Format(format!("SH delete {index} out of range")))?;
        if let Some(rec) = self.table.records.get_mut(row) {
            rec[0] = DELETED;
        }
        self.docs.remove(index);
        self.rows.remove(index);
        // physical row indices after the removed one are unchanged (we only
        // drop the live mapping, never the record).
        Ok(())
    }

    /// Restore a tombstoned row by physical record index.
    pub fn restore_row(&mut self, row: usize) -> Result<usize> {
        if row >= self.table.count() {
            return Err(CoreError::Format(format!("SH row {row} out of range")));
        }
        if self.rows.contains(&row) {
            return Err(CoreError::Format(format!("SH row {row} is already live")));
        }
        if let Some(rec) = self.table.records.get_mut(row) {
            rec[0] = LIVE;
        }
        let doc = ShDocument::decode(&self.table, row)?;
        let guid = doc.cwguid.clone();
        self.docs.push(doc);
        self.rows.push(row);
        // keep the live view in physical order, like `from_table`
        let mut order: Vec<usize> = (0..self.docs.len()).collect();
        order.sort_by_key(|i| self.rows[*i]);
        self.docs = order.iter().map(|i| self.docs[*i].clone()).collect();
        self.rows = order.iter().map(|i| self.rows[*i]).collect();
        self.index_of(&guid)
            .ok_or_else(|| CoreError::Format("SH restore lost the row".into()))
    }

    /// Physical tombstone count (deleted rows kept in the table).
    pub fn tombstone_count(&self) -> usize {
        self.table.count().saturating_sub(self.docs.len())
    }

    /// In-memory save image (`SH.dbf`). A no-op save is byte-identical.
    pub fn encode(&self) -> Vec<u8> {
        self.table.encode()
    }

    pub fn save(&self, dbf: impl AsRef<Path>) -> Result<()> {
        self.table.write_path(dbf)
    }

    pub fn table(&self) -> &Table {
        &self.table
    }
}

// ---------------------------------------------------------------------------
// WR - reference links
// ---------------------------------------------------------------------------

/// One decoded `WR` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    pub doc_guid: String,
    pub from_ref: String,
    pub rev_code: String,
    pub memo_no: String,
    pub memo: String,
    pub lineno: String,
    pub cwguid: String,
    pub cw_date: String,
    pub created_on: String,
    pub deleted: bool,
}

/// Write request for [`ReferenceBook::add`].
#[derive(Clone, Debug, Default)]
pub struct ReferenceDraft {
    pub doc_guid: String,
    pub from_ref: String,
    pub rev_code: String,
    pub memo_no: String,
    pub memo: String,
}

impl ReferenceDraft {
    /// Reference memo on a document (`DOCGUID` must be a live `SH` row).
    pub fn new(doc_guid: &str, memo: &str) -> ReferenceDraft {
        ReferenceDraft {
            doc_guid: doc_guid.trim().to_string(),
            memo: memo.to_string(),
            ..Default::default()
        }
    }

    pub fn from_ref(mut self, reference: &str) -> ReferenceDraft {
        self.from_ref = reference.trim().to_string();
        self
    }

    pub fn revision(mut self, code: &str) -> ReferenceDraft {
        self.rev_code = code.trim().to_string();
        self
    }
}

/// The `WR` table plus its memo sidecar.
#[derive(Clone, Debug)]
pub struct ReferenceBook {
    table: Table,
    fpt: Fpt,
    rows: Vec<usize>,
    refs: Vec<Reference>,
}

impl ReferenceBook {
    pub fn open(dir: impl AsRef<Path>, stem: &str) -> Result<ReferenceBook> {
        let dir = dir.as_ref();
        let dbf = sidecar(dir, stem, "WR", "dbf")?;
        let table = Table::parse_path(dbf)?;
        let fpt = match sidecar(dir, stem, "WR", "fpt") {
            Ok(p) => Fpt::parse_path(p)?,
            Err(_) => empty_fpt(),
        };
        Self::from_parts(table, fpt)
    }

    pub fn from_bytes(dbf: &[u8], fpt: &[u8]) -> Result<ReferenceBook> {
        let table = Table::parse(dbf)?;
        let fpt = if fpt.is_empty() { empty_fpt() } else { Fpt::parse(fpt)? };
        Self::from_parts(table, fpt)
    }

    pub fn from_table(table: Table, fpt: Fpt) -> Result<ReferenceBook> {
        Self::from_parts(table, fpt)
    }

    fn from_parts(table: Table, fpt: Fpt) -> Result<ReferenceBook> {
        check_columns(&table, "WR", &WR_COLUMNS, WR_RECORD_LEN)?;
        let mut rows = Vec::new();
        let mut refs = Vec::new();
        for r in 0..table.count() {
            if table.is_deleted(r) {
                continue;
            }
            refs.push(decode_reference(&table, r, &fpt));
            rows.push(r);
        }
        Ok(ReferenceBook { table, fpt, rows, refs })
    }

    pub fn references(&self) -> &[Reference] {
        &self.refs
    }

    pub fn count(&self) -> usize {
        self.refs.len()
    }

    pub fn by_document(&self, doc_guid: &str) -> Vec<&Reference> {
        self.refs
            .iter()
            .filter(|r| r.doc_guid == doc_guid.trim())
            .collect()
    }

    /// Live rows whose `DOCGUID` no longer resolves into `SH` (F60 law:
    /// live WR rows resolve; deleted documents leave dangling memos).
    pub fn dangling(&self, book: &DocumentBook) -> Vec<String> {
        let mut out = Vec::new();
        for r in &self.refs {
            if book.resolve(&r.doc_guid).is_none() {
                out.push(r.doc_guid.clone());
            }
        }
        out
    }

    /// Append a reference row with `LINENO=111111` (the CW sentinel).
    pub fn add(&mut self, draft: ReferenceDraft, rng: &mut Rng, stamp: &str) -> Result<usize> {
        if draft.doc_guid.is_empty() {
            return Err(CoreError::Format("WR: DOCGUID is mandatory".into()));
        }
        if draft.doc_guid.len() != 22 {
            return Err(CoreError::Format(format!(
                "WR DOCGUID {} chars, want 22",
                draft.doc_guid.len()
            )));
        }
        if draft.from_ref.len() > 40 {
            return Err(CoreError::Format("WR FROM_REF > 40 chars".into()));
        }
        let guid = CwGuid::allocate_for_table("WR", rng).encode();
        let block = put_memo(&mut self.fpt, draft.memo.as_bytes());
        let mut rec = vec![LIVE; WR_RECORD_LEN];
        let fields = self.table.fields.clone();
        put(&mut rec, &fields, "DOCGUID", draft.doc_guid.trim())?;
        put(&mut rec, &fields, "FROM_REF", draft.from_ref.trim())?;
        put(&mut rec, &fields, "REV_CODE", draft.rev_code.trim())?;
        put(&mut rec, &fields, "MEMO_NO", draft.memo_no.trim())?;
        put(&mut rec, &fields, "MEMO", &memo_ref(block))?;
        put(&mut rec, &fields, "LINENO", WR_LINENO)?;
        put(&mut rec, &fields, "CWGUID", &guid)?;
        put(&mut rec, &fields, "CWDATE", stamp)?;
        put(&mut rec, &fields, "CREATEDON", stamp)?;
        let row = self.table.records.len();
        self.table.records.push(rec);
        self.refs.push(decode_reference(&self.table, row, &self.fpt));
        self.rows.push(row);
        Ok(self.refs.len() - 1)
    }

    pub fn remove_at(&mut self, index: usize) -> Result<()> {
        let row = *self
            .rows
            .get(index)
            .ok_or_else(|| CoreError::Format(format!("WR remove {index} out of range")))?;
        if let Some(rec) = self.table.records.get_mut(row) {
            rec[0] = DELETED;
        }
        if let Some(r) = self.refs.get_mut(index) {
            r.deleted = true;
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
}

fn decode_reference(t: &Table, r: usize, fpt: &Fpt) -> Reference {
    Reference {
        doc_guid: trimmed(t, r, "DOCGUID"),
        from_ref: trimmed(t, r, "FROM_REF"),
        rev_code: trimmed(t, r, "REV_CODE"),
        memo_no: trimmed(t, r, "MEMO_NO"),
        memo: memo_text(fpt, t.cell(r, "MEMO").unwrap_or(&[])),
        lineno: trimmed(t, r, "LINENO"),
        cwguid: trimmed(t, r, "CWGUID"),
        cw_date: trimmed(t, r, "CWDATE"),
        created_on: trimmed(t, r, "CREATEDON"),
        deleted: t.is_deleted(r),
    }
}

// ---------------------------------------------------------------------------
// WT - note text/value rows
// ---------------------------------------------------------------------------

/// One decoded `WT` note row.
#[derive(Clone, Debug, PartialEq)]
pub struct WtNote {
    pub lineno: Option<i64>,
    pub doc_guid: String,
    pub text: String,
    /// `TYPE` (single char; no corpus row proves the domain).
    pub kind: String,
    /// `A..M` numeric slots.
    pub numbers: BTreeMap<char, f64>,
    /// `AF..MF` format strings.
    pub formats: BTreeMap<char, String>,
    /// `AN..MN` codes.
    pub codes: BTreeMap<char, String>,
    /// `AC..MC` codes.
    pub ac_codes: BTreeMap<char, String>,
    pub percent: Option<f64>,
    pub cwguid: String,
    pub cw_date: String,
    pub created_on: String,
}

/// Write request for [`NoteBook::add`].
#[derive(Clone, Debug, Default)]
pub struct NoteDraft {
    pub doc_guid: String,
    pub text: String,
    pub kind: String,
    pub numbers: Vec<(char, f64)>,
    pub formats: Vec<(char, String)>,
    pub codes: Vec<(char, String)>,
    pub percent: Option<f64>,
}

impl NoteDraft {
    pub fn new(doc_guid: &str, text: &str) -> NoteDraft {
        NoteDraft {
            doc_guid: doc_guid.trim().to_string(),
            text: text.to_string(),
            kind: "T".into(),
            ..Default::default()
        }
    }

    pub fn number(mut self, slot: char, value: f64) -> NoteDraft {
        self.numbers.push((slot, value));
        self
    }

    pub fn format(mut self, slot: char, format: &str) -> NoteDraft {
        self.formats.push((slot, format.to_string()));
        self
    }

    pub fn percent(mut self, p: f64) -> NoteDraft {
        self.percent = Some(p);
        self
    }

    /// Reject slots outside `A..M` (the four 13-slot families).
    pub fn validate(&self) -> Result<()> {
        for (slot, _) in &self.numbers {
            if !WT_SLOTS.contains(slot) {
                return Err(CoreError::Format(format!("WT numeric slot {slot} outside A..M")));
            }
        }
        for (slot, _) in &self.formats {
            if !WT_SLOTS.contains(slot) {
                return Err(CoreError::Format(format!("WT format slot {slot} outside A..M")));
            }
        }
        Ok(())
    }
}

/// The `WT` table (no memo columns; 0 rows in every corpus).
#[derive(Clone, Debug)]
pub struct NoteBook {
    table: Table,
    rows: Vec<usize>,
    notes: Vec<WtNote>,
}

impl NoteBook {
    pub fn open(dir: impl AsRef<Path>, stem: &str) -> Result<NoteBook> {
        let dir = dir.as_ref();
        let dbf = sidecar(dir, stem, "WT", "dbf")?;
        Self::from_table(Table::parse_path(dbf)?)
    }

    pub fn from_bytes(dbf: &[u8]) -> Result<NoteBook> {
        Self::from_table(Table::parse(dbf)?)
    }

    pub fn from_table(table: Table) -> Result<NoteBook> {
        check_columns(&table, "WT", &WT_COLUMNS, WT_RECORD_LEN)?;
        let mut rows = Vec::new();
        let mut notes = Vec::new();
        for r in 0..table.count() {
            if table.is_deleted(r) {
                continue;
            }
            notes.push(decode_note(&table, r));
            rows.push(r);
        }
        Ok(NoteBook { table, rows, notes })
    }

    pub fn notes(&self) -> &[WtNote] {
        &self.notes
    }

    pub fn count(&self) -> usize {
        self.notes.len()
    }

    pub fn by_document(&self, doc_guid: &str) -> Vec<&WtNote> {
        self.notes
            .iter()
            .filter(|n| n.doc_guid == doc_guid.trim())
            .collect()
    }

    pub fn next_lineno(&self) -> i64 {
        self.notes
            .iter()
            .filter_map(|n| n.lineno)
            .max()
            .map(|m| m + 1)
            .unwrap_or(1)
    }

    /// Append one note row (`LINENO` = max + 1).
    pub fn add(&mut self, draft: NoteDraft, rng: &mut Rng, stamp: &str) -> Result<usize> {
        draft.validate()?;
        if draft.doc_guid.is_empty() {
            return Err(CoreError::Format("WT: DOCGUID is mandatory".into()));
        }
        let guid = CwGuid::allocate_for_table("WT", rng).encode();
        let lineno = self.next_lineno();
        let mut rec = vec![LIVE; WT_RECORD_LEN];
        let fields = self.table.fields.clone();
        put(&mut rec, &fields, "LINENO", &lineno.to_string())?;
        put(&mut rec, &fields, "DOCGUID", draft.doc_guid.trim())?;
        put(&mut rec, &fields, "TEXT", draft.text.trim_end())?;
        put(&mut rec, &fields, "TYPE", draft.kind.trim())?;
        for (slot, value) in &draft.numbers {
            put(&mut rec, &fields, &slot.to_string(), &format!("{value:.2}"))?;
        }
        for (slot, format) in &draft.formats {
            put(&mut rec, &fields, &format!("{slot}F"), format.trim())?;
        }
        for (slot, code) in &draft.codes {
            put(&mut rec, &fields, &format!("{slot}N"), code.trim())?;
        }
        if let Some(p) = draft.percent {
            put(&mut rec, &fields, "PERCENT", &format!("{p:.2}"))?;
        }
        put(&mut rec, &fields, "CWGUID", &guid)?;
        put(&mut rec, &fields, "CWDATE", stamp)?;
        put(&mut rec, &fields, "CREATEDON", stamp)?;
        let row = self.table.records.len();
        self.table.records.push(rec);
        self.notes.push(decode_note(&self.table, row));
        self.rows.push(row);
        Ok(self.notes.len() - 1)
    }

    pub fn remove_at(&mut self, index: usize) -> Result<()> {
        let row = *self
            .rows
            .get(index)
            .ok_or_else(|| CoreError::Format(format!("WT remove {index} out of range")))?;
        if let Some(rec) = self.table.records.get_mut(row) {
            rec[0] = DELETED;
        }
        self.rows.remove(index);
        self.notes.remove(index);
        Ok(())
    }

    pub fn encode(&self) -> Vec<u8> {
        self.table.encode()
    }

    pub fn save(&self, dbf: impl AsRef<Path>) -> Result<()> {
        self.table.write_path(dbf)
    }

    pub fn table(&self) -> &Table {
        &self.table
    }
}

fn decode_note(t: &Table, r: usize) -> WtNote {
    let dec = |name: &str| -> Option<f64> { trimmed(t, r, name).parse().ok() };
    let mut numbers = BTreeMap::new();
    let mut formats = BTreeMap::new();
    let mut codes = BTreeMap::new();
    let mut ac_codes = BTreeMap::new();
    for slot in WT_SLOTS {
        if let Some(v) = dec(&slot.to_string()) {
            numbers.insert(slot, v);
        }
        let f = trimmed(t, r, &format!("{slot}F"));
        if !f.is_empty() {
            formats.insert(slot, f);
        }
        let c = trimmed(t, r, &format!("{slot}N"));
        if !c.is_empty() {
            codes.insert(slot, c);
        }
        let a = trimmed(t, r, &format!("{slot}C"));
        if !a.is_empty() {
            ac_codes.insert(slot, a);
        }
    }
    WtNote {
        lineno: number(t, r, "LINENO"),
        doc_guid: trimmed(t, r, "DOCGUID"),
        text: cell(t, r, "TEXT").trim_end().to_string(),
        kind: trimmed(t, r, "TYPE"),
        numbers,
        formats,
        codes,
        ac_codes,
        percent: dec("PERCENT"),
        cwguid: trimmed(t, r, "CWGUID"),
        cw_date: trimmed(t, r, "CWDATE"),
        created_on: trimmed(t, r, "CREATEDON"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_kinds_round_trip() {
        for k in [
            DocumentKind::Index,
            DocumentKind::Heading,
            DocumentKind::Analysis,
            DocumentKind::Form,
            DocumentKind::Misc,
            DocumentKind::Star,
        ] {
            assert_eq!(DocumentKind::parse(&k.letter().to_string()).unwrap(), k);
        }
        // hull2024 carries letters beyond the corpus enum (S/J/B/R/t/...):
        // any single printable char round-trips as Other; only junk errors.
        assert_eq!(
            DocumentKind::parse("X").unwrap(),
            DocumentKind::Other('X')
        );
        assert!(DocumentKind::parse("t").unwrap().is_heading() == false);
        assert!(DocumentKind::parse("AB").is_err());
        assert!(DocumentKind::Heading.is_heading());
        assert!(DocumentKind::Form.is_form());
    }

    #[test]
    fn column_arrays_match_geometry() {
        assert_eq!(SH_COLUMNS.len(), SH_FIELD_COUNT);
        assert_eq!(WR_COLUMNS.len(), WR_FIELD_COUNT);
        assert_eq!(WT_COLUMNS.len(), WT_FIELD_COUNT);
        assert_eq!(WT_COLUMNS.len(), 4 + WT_SLOTS.len() * 4 + 4);
    }

    #[test]
    fn memo_ref_is_right_aligned() {
        assert_eq!(memo_ref(7), "         7");
        let mut f = empty_fpt();
        assert_eq!(put_memo(&mut f, b""), 0);
        let b = put_memo(&mut f, b"ref");
        assert_eq!(f.memo(&memo_ref(b).into_bytes()).unwrap(), b"ref");
    }
}

