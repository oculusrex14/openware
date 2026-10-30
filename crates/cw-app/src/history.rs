//! cw-app history / audit trail (P5.D4a).
//!
//! Behavior contract: `docs\specs\history.md` (F47 signoff audit + the corpus
//! notes below), fixture `a sample copy` (approved copy, no COM).
//!
//! The history log is the `HI` table: 14 fields, record length 201, one row
//! per audited operation. `VALUES` and `MILESTONE` are FoxPro memo (M) cells
//! referencing `HI.FPT`; `VALUES` is a small `<values>` XML document carrying
//! the display user and operation-specific attributes, `MILESTONE` is the
//! (usually empty) binary milestone blob.
//!
//! Observed corpus law (corp2026 / USI, 1710 live rows, 2021-11-10 .. 2026-10-21):
//!
//! | CATEGORY | TYPE     | meaning                              | OBJECTID        |
//! |----------|----------|--------------------------------------|-----------------|
//! | `FILA` / `` | `FACCUSR` | application sign-in / user access | empty           |
//! | `DOCA`   | `DOCAUSR`| document open / access              | `SH` CWGUID     |
//! | `DOC-`   | `DOCDEL` | document delete (name in VALUES)     | empty           |
//! | `DOC*`   | `DOCPROP`| document property change             | `SH` CWGUID     |
//! | `DOCS`   | `DOCSIGNn`| signoff role n put (F47)            | `SH` CWGUID     |
//!
//! Reader laws:
//! - every cell is decoded by field name, never by hard-coded offset;
//! - C cells are latin-1, right-stripped of spaces (same canon as C1);
//! - M cells resolve through [`Fpt::memo`], so an absent/zero block is empty;
//! - `CHECKSUM` is a 22-char CWGUID-shaped token, empty on `FACCUSR`, absent
//!   on some older deletes/property rows (94 + 180 + 42 in corp2026);
//! - `CWGUID` is the row id (family B for HI); one legacy 8-char factory row
//!   is carried verbatim by the reader (F59).
//!
//! Writer laws (no-op save is byte-stable):
//! - append a 201-byte record, bump the DBF record count through
//!   [`Table::encode`], append the `VALUES` payload as one or more FPT blocks
//!   and set the 10-digit block reference;
//! - allocate `CWGUID` from the HI family when the event leaves it blank;
//! - emit the corpus `VALUES` shape
//!   `<values><value name="username">..</value>...<value name="checksumversion"
//!   type="integer">4097</value></values>\r\n`;
//! - the `CWDATE`/`STARTDATE`/`ENDDATE`/`CREATEDON` stamps are caller-supplied
//!   (`YYYYMMDDhhmmss`) so golden runs stay deterministic.
//!
//! Documented deviation: `MILESTONE` is carried as an opaque memo payload;
//! the corpus contains no non-empty milestone rows, so its internal binary
//! layout is not inferred here (P5.G/lockout lane).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use cw_core::canon::latin1;
use cw_core::dbf::{RawRecord, Table};
use cw_core::fpt::Fpt;
use cw_core::guid::{CwGuid, Rng};
use cw_core::{CoreError, Result};

/// Live-row deletion flag.
pub const LIVE: u8 = b' ';
/// Tombstone deletion flag.
pub const DELETED: u8 = b'*';

/// Table suffix owned by this module.
pub const SUFFIX: &str = "HI";

/// Default `checksumversion` written into `VALUES` (corpus mode 4097).
pub const CHECKSUM_VERSION: u32 = 4097;

/// One `<value>` child of the history `VALUES` document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueItem {
    /// `name` attribute (`username`, `deletedid`, `DocPropChange`, ...).
    pub name: String,
    /// Element text with XML entities decoded, whitespace preserved.
    pub text: String,
    /// `type` attribute when present (`integer` in the corpus).
    pub kind: Option<String>,
}

/// Decoded `HI` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEvent {
    pub category: String,
    pub event_type: String,
    pub object_id: String,
    pub object_type: String,
    pub user: String,
    /// Raw `VALUES` memo text (empty when the memo block is absent).
    pub values: String,
    pub checksum: String,
    /// Raw `MILESTONE` memo payload (opaque; empty in the corpus).
    pub milestone: Vec<u8>,
    pub milestone_rec: String,
    pub start_date: String,
    pub end_date: String,
    pub cwguid: String,
    pub cw_date: String,
    pub created_on: String,
    /// Physical deletion flag (`*` tombstones stay readable).
    pub deleted: bool,
}

/// Operation class decoded from (`CATEGORY`, `TYPE`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryKind {
    /// Application sign-in / user access (`FACCUSR`).
    UserAccess,
    /// Document open / access (`DOCAUSR`).
    DocumentAccess,
    /// Document delete (`DOCDEL`; the deleted name lives in `VALUES`).
    DocumentDelete,
    /// Document property change (`DOCPROP`).
    DocumentProperty,
    /// Signoff role `1..=8` put (`DOCSIGNn`, F47).
    Signoff(u8),
    /// Anything else (kept lossless by the reader).
    Other,
}

impl HistoryEvent {
    /// Parsed `VALUES` children in document order.
    pub fn values_items(&self) -> Vec<ValueItem> {
        parse_values(&self.values)
    }

    /// First `VALUES` child named `name`.
    pub fn value(&self, name: &str) -> Option<String> {
        self.values_items()
            .into_iter()
            .find(|v| v.name == name)
            .map(|v| v.text)
    }

    /// Display user (`username` in `VALUES`).
    pub fn username(&self) -> Option<String> {
        self.value("username")
    }

    /// Initials as stored (leading spaces preserved, as CW writes them).
    pub fn user_initials(&self) -> Option<String> {
        self.value("userinitials")
    }

    /// `checksumversion` as an integer.
    pub fn checksum_version(&self) -> Option<u32> {
        self.value("checksumversion")?.trim().parse().ok()
    }

    /// Operation class.
    pub fn kind(&self) -> HistoryKind {
        match self.event_type.as_str() {
            "FACCUSR" => HistoryKind::UserAccess,
            "DOCAUSR" => HistoryKind::DocumentAccess,
            "DOCDEL" => HistoryKind::DocumentDelete,
            "DOCPROP" => HistoryKind::DocumentProperty,
            other => {
                if let Some(n) = other.strip_prefix("DOCSIGN").and_then(|s| s.parse::<u8>().ok()) {
                    HistoryKind::Signoff(n)
                } else {
                    HistoryKind::Other
                }
            }
        }
    }

    /// Signoff role for `DOCSIGNn` rows.
    pub fn signoff_role(&self) -> Option<u8> {
        match self.kind() {
            HistoryKind::Signoff(n) => Some(n),
            _ => None,
        }
    }

    /// `true` when the event is anchored to a document (`OBJECTTYPE='SH'`).
    pub fn is_document_event(&self) -> bool {
        self.object_type.eq_ignore_ascii_case("SH") && !self.object_id.is_empty()
    }

    /// Empty event with all timestamps set to `stamp`.
    pub fn at(stamp: impl Into<String>) -> HistoryEvent {
        let stamp = stamp.into();
        HistoryEvent {
            category: String::new(),
            event_type: String::new(),
            object_id: String::new(),
            object_type: String::new(),
            user: String::new(),
            values: String::new(),
            checksum: String::new(),
            milestone: Vec::new(),
            milestone_rec: String::new(),
            start_date: stamp.clone(),
            end_date: stamp.clone(),
            cwguid: String::new(),
            cw_date: stamp.clone(),
            created_on: stamp,
            deleted: false,
        }
    }
}

/// Census over a history log (the golden-test surface).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryCensus {
    pub records: usize,
    pub live: usize,
    pub deleted: usize,
    pub users: usize,
    pub categories: BTreeMap<String, usize>,
    pub types: BTreeMap<String, usize>,
    pub object_types: BTreeMap<String, usize>,
    pub first_date: String,
    pub last_date: String,
}

/// `HI` reader / writer bound to one engagement.
#[derive(Clone, Debug)]
pub struct HistoryLog {
    pub table: Table,
    pub fpt: Fpt,
}

impl HistoryLog {
    /// Open `<dir>\<stem>HI.dbf` + `.fpt` (case-insensitive, sidecar optional).
    pub fn open(dir: impl AsRef<Path>, stem: &str) -> Result<HistoryLog> {
        let dir = dir.as_ref();
        let dbf_path = find_sidecar(dir, stem, "HI", "dbf")?;
        let table = Table::parse_path(&dbf_path)?;
        let fpt = match find_sidecar(dir, stem, "HI", "fpt") {
            Ok(p) => Fpt::parse_path(p)?,
            Err(_) => empty_fpt(),
        };
        Ok(HistoryLog { table, fpt })
    }

    /// Parse in-memory bytes (DBF first, then FPT).
    pub fn from_bytes(dbf: &[u8], fpt: &[u8]) -> Result<HistoryLog> {
        let table = Table::parse(dbf)?;
        let fpt = if fpt.is_empty() { empty_fpt() } else { Fpt::parse(fpt)? };
        Ok(HistoryLog { table, fpt })
    }

    /// Decode every physical row, tombstones included.
    pub fn all(&self) -> Result<Vec<HistoryEvent>> {
        let layout = Layout::of(&self.table);
        let mut out = Vec::with_capacity(self.table.count());
        for r in 0..self.table.count() {
            out.push(self.decode(&layout, r)?);
        }
        Ok(out)
    }

    /// Decode live rows only.
    pub fn live(&self) -> Result<Vec<HistoryEvent>> {
        Ok(self.all()?.into_iter().filter(|e| !e.deleted).collect())
    }

    /// Decode one physical row.
    pub fn event(&self, r: usize) -> Result<HistoryEvent> {
        self.decode(&Layout::of(&self.table), r)
    }

    fn decode(&self, layout: &Layout, r: usize) -> Result<HistoryEvent> {
        let rec = self
            .table
            .records
            .get(r)
            .ok_or_else(|| CoreError::Format(format!("record {r} out of range")))?;
        let c = |name: &str| -> Result<String> { Ok(layout.get(rec, name)?.trim_end_matches(' ').to_string()) };
        let m = |name: &str| -> Result<Vec<u8>> {
            let raw = layout.raw(rec, name)?;
            Ok(self.fpt.memo(raw).map(|p| p.to_vec()).unwrap_or_default())
        };
        Ok(HistoryEvent {
            category: c("CATEGORY")?,
            event_type: c("TYPE")?,
            object_id: c("OBJECTID")?,
            object_type: c("OBJECTTYPE")?,
            user: c("USER")?,
            values: latin1(&m("VALUES")?),
            checksum: c("CHECKSUM")?,
            milestone: m("MILESTONE")?,
            milestone_rec: c("MILESTNREC")?,
            start_date: c("STARTDATE")?,
            end_date: c("ENDDATE")?,
            cwguid: c("CWGUID")?,
            cw_date: c("CWDATE")?,
            created_on: c("CREATEDON")?,
            deleted: rec[0] != LIVE,
        })
    }

    /// Category/type/user/date census (golden gate).
    pub fn census(&self) -> Result<HistoryCensus> {
        let events = self.all()?;
        let mut out = HistoryCensus {
            records: events.len(),
            ..Default::default()
        };
        let mut users = BTreeSet::new();
        for e in &events {
            if e.deleted {
                out.deleted += 1;
                continue;
            }
            out.live += 1;
            *out.categories.entry(e.category.clone()).or_insert(0) += 1;
            *out.types.entry(e.event_type.clone()).or_insert(0) += 1;
            *out.object_types.entry(e.object_type.clone()).or_insert(0) += 1;
            if !e.user.is_empty() {
                users.insert(e.user.to_ascii_lowercase());
            }
            if !e.created_on.is_empty() {
                if out.first_date.is_empty() || e.created_on < out.first_date {
                    out.first_date = e.created_on.clone();
                }
                if e.created_on > out.last_date {
                    out.last_date = e.created_on.clone();
                }
            }
        }
        out.users = users.len();
        Ok(out)
    }

    /// Append one event, allocating the row GUID when blank. Returns its index.
    pub fn append(&mut self, event: &HistoryEvent, rng: &mut Rng) -> Result<usize> {
        let layout = Layout::of(&self.table);
        let mut rec: RawRecord = vec![b' '; self.table.record_len as usize];
        rec[0] = if event.deleted { DELETED } else { LIVE };

        let mut ev = event.clone();
        if ev.cwguid.trim().is_empty() {
            ev.cwguid = CwGuid::allocate_for_table(SUFFIX, rng).encode();
        }
        if ev.created_on.is_empty() {
            ev.created_on = ev.cw_date.clone();
        }
        if ev.cw_date.is_empty() {
            ev.cw_date = ev.created_on.clone();
        }
        if ev.start_date.is_empty() {
            ev.start_date = ev.created_on.clone();
        }
        if ev.end_date.is_empty() {
            ev.end_date = ev.start_date.clone();
        }

        layout.set(&mut rec, "CATEGORY", &ev.category, true)?;
        layout.set(&mut rec, "TYPE", &ev.event_type, true)?;
        layout.set(&mut rec, "OBJECTID", &ev.object_id, true)?;
        layout.set(&mut rec, "OBJECTTYPE", &ev.object_type, true)?;
        layout.set(&mut rec, "USER", &ev.user, true)?;
        layout.set(&mut rec, "CHECKSUM", &ev.checksum, true)?;
        layout.set(&mut rec, "MILESTNREC", &ev.milestone_rec, true)?;
        layout.set(&mut rec, "STARTDATE", &ev.start_date, true)?;
        layout.set(&mut rec, "ENDDATE", &ev.end_date, true)?;
        layout.set(&mut rec, "CWGUID", &ev.cwguid, true)?;
        layout.set(&mut rec, "CWDATE", &ev.cw_date, true)?;
        layout.set(&mut rec, "CREATEDON", &ev.created_on, true)?;

        let values_block = self.put_memo(ev.values.as_bytes());
        layout.set(&mut rec, "VALUES", &memo_ref(values_block), true)?;
        let milestone_block = self.put_memo(&ev.milestone);
        layout.set(&mut rec, "MILESTONE", &memo_ref(milestone_block), true)?;

        let idx = self.table.records.len();
        self.table.records.push(rec);
        Ok(idx)
    }

    /// Append the CW-shaped audit row for one document/user operation.
    ///
    /// `extra` fields land between `userinitials` and `checksumversion`
    /// (`deletedid`/`deletedname` for deletes, `DocPropChange` for properties,
    /// `version` for signoffs). `checksum` is the target document checksum.
    pub fn record_document_event(
        &mut self,
        kind: HistoryKind,
        document_id: &str,
        user: &str,
        initials: &str,
        extra: &[(&str, &str)],
        checksum: &str,
        stamp: &str,
        rng: &mut Rng,
    ) -> Result<usize> {
        let (category, event_type) = kind_cells(&kind)?;
        // Blank identity => CW writes neither username nor userinitials and
        // leaves `HI.USER` empty (fixture d4-signoff-s1, F68).
        let mut fields = Vec::new();
        if !user.is_empty() || !initials.is_empty() {
            fields.push(("username".to_string(), user.to_string()));
            fields.push(("userinitials".to_string(), initials.to_string()));
        }
        for (k, v) in extra {
            fields.push(((*k).to_string(), (*v).to_string()));
        }
        // Corpus law: only open/property/signoff rows are SH-anchored.
        // Application access and deletes carry empty OBJECTID/OBJECTTYPE.
        let anchored = !matches!(kind, HistoryKind::UserAccess | HistoryKind::DocumentDelete);
        let mut ev = HistoryEvent::at(stamp);
        ev.category = category.into();
        ev.event_type = event_type;
        ev.object_id = if anchored { document_id.to_string() } else { String::new() };
        ev.object_type = if anchored { "SH".into() } else { String::new() };
        ev.user = user.to_string();
        ev.checksum = checksum.to_string();
        ev.values = values_xml(&fields, CHECKSUM_VERSION);
        self.append(&ev, rng)
    }

    /// In-memory save image: `(HI.dbf bytes, HI.fpt bytes)`.
    pub fn encode(&self) -> (Vec<u8>, Vec<u8>) {
        (self.table.encode(), self.fpt.encode())
    }

    /// Write `<dir>\<stem>HI.dbf` and `<stem>HI.fpt`.
    pub fn save(&self, dir: impl AsRef<Path>, stem: &str) -> Result<()> {
        let dir = dir.as_ref();
        fs::create_dir_all(dir).map_err(CoreError::from)?;
        self.table.write_path(dir.join(format!("{stem}HI.dbf")))?;
        self.fpt.write_path(dir.join(format!("{stem}HI.fpt")))?;
        Ok(())
    }

    /// Append a memo payload at `next_free`; returns the block reference.
    ///
    /// CW shape (F68, COM fixture `d4-signoff-s1`): the block header + payload
    /// are appended and the file ends at `base + 8 + len` — the last block is
    /// **not** padded to a block boundary (`875604 -> 876116` for a 76-byte
    /// payload at block 1711). Only a gap up to `base` is zero-filled when the
    /// previous block ended mid-block.
    fn put_memo(&mut self, payload: &[u8]) -> u32 {
        if payload.is_empty() {
            return 0;
        }
        let bs = self.fpt.block_size.max(1) as usize;
        let block = self.fpt.next_free.max(1);
        let base = block as usize * bs;
        if self.fpt.data.len() < base {
            self.fpt.data.resize(base, 0);
        }
        let need = 8 + payload.len();
        let blocks = (need + bs - 1) / bs;
        let end = base + need;
        if self.fpt.data.len() < end {
            self.fpt.data.resize(end, 0);
        }
        self.fpt.data[base..base + 4].copy_from_slice(&1u32.to_be_bytes()); // 1 = text
        self.fpt.data[base + 4..base + 8].copy_from_slice(&(payload.len() as u32).to_be_bytes());
        self.fpt.data[base + 8..base + 8 + payload.len()].copy_from_slice(payload);
        self.fpt.next_free = block + blocks as u32;
        self.fpt.data[0..4].copy_from_slice(&self.fpt.next_free.to_be_bytes());
        block
    }
}

/// Map an operation class to its observed (`CATEGORY`, `TYPE`) cells.
pub fn kind_cells(kind: &HistoryKind) -> Result<(String, String)> {
    Ok(match kind {
        HistoryKind::UserAccess => ("FILA".into(), "FACCUSR".into()),
        HistoryKind::DocumentAccess => ("DOCA".into(), "DOCAUSR".into()),
        HistoryKind::DocumentDelete => ("DOC-".into(), "DOCDEL".into()),
        HistoryKind::DocumentProperty => ("DOC*".into(), "DOCPROP".into()),
        HistoryKind::Signoff(n) if (1..=8).contains(n) => ("DOCS".into(), format!("DOCSIGN{n}")),
        HistoryKind::Signoff(n) => {
            return Err(CoreError::Format(format!("signoff role {n} outside 1..=8")))
        }
        HistoryKind::Other => {
            return Err(CoreError::Format("HistoryKind::Other has no canonical cells".into()))
        }
    })
}

/// Parse a `<values>` XML document into ordered items.
pub fn parse_values(text: &str) -> Vec<ValueItem> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(rel) = text[i..].find("<value") {
        let start = i + rel;
        let Some(gt_rel) = text[start..].find('>') else { break };
        let gt = start + gt_rel;
        if text[start..gt].starts_with("<values") {
            i = gt + 1;
            continue;
        }
        let head = &text[start..gt];
        let name = attr(head, "name").unwrap_or_default();
        let kind = attr(head, "type");
        let body_start = gt + 1;
        let Some(end_rel) = text[body_start..].find("</value>") else { break };
        let end = body_start + end_rel;
        out.push(ValueItem {
            name,
            text: xml_unescape(&text[body_start..end]),
            kind,
        });
        i = end + "</value>".len();
    }
    out
}

/// Serialize `VALUES` in the corpus shape (CRLF terminated).
pub fn values_xml(fields: &[(String, String)], checksum_version: u32) -> String {
    let mut out = String::from("<values>");
    for (name, value) in fields {
        if name == "checksumversion" {
            continue;
        }
        out.push_str(&format!("<value name=\"{}\"", xml_escape(name)));
        if value.starts_with(' ') || value.ends_with(' ') {
            out.push_str(" xml:space=\"preserve\"");
        }
        out.push('>');
        out.push_str(&xml_escape(value));
        out.push_str("</value>");
    }
    out.push_str(&format!(
        "<value name=\"checksumversion\" type=\"integer\">{checksum_version}</value></values>\r\n"
    ));
    out
}

fn attr(head: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let pos = head.find(&needle)? + needle.len();
    let rest = &head[pos..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

fn xml_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        let Some(end) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let entity = &tail[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse::<u32>().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(ch) => out.push(ch),
            None => out.push_str(&tail[..=end]),
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

fn memo_ref(block: u32) -> String {
    // CW writes new references right-aligned with spaces ("      1711"),
    // legacy rows carry zero padding ("0000000001"); readers trim (F68).
    format!("{block:>10}")
}

fn empty_fpt() -> Fpt {
    let mut data = vec![0u8; 512];
    data[6..8].copy_from_slice(&512u16.to_be_bytes());
    data[0..4].copy_from_slice(&1u32.to_be_bytes());
    Fpt { data, block_size: 512, next_free: 1 }
}

fn find_sidecar(dir: &Path, stem: &str, suffix: &str, ext: &str) -> Result<PathBuf> {
    let want = format!("{stem}{suffix}.{ext}").to_ascii_lowercase();
    if let Ok(rd) = fs::read_dir(dir) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if name == want {
                return Ok(entry.path());
            }
        }
    }
    Err(CoreError::Format(format!(
        "missing {stem}{suffix}.{ext} in {}",
        dir.display()
    )))
}

struct Layout {
    cells: Vec<(String, usize, usize)>,
}

impl Layout {
    fn of(t: &Table) -> Layout {
        let mut cells = Vec::with_capacity(t.fields.len());
        let mut off = 1usize;
        for f in &t.fields {
            cells.push((f.name.to_ascii_uppercase(), off, f.len as usize));
            off += f.len as usize;
        }
        Layout { cells }
    }

    fn span(&self, name: &str) -> Result<(usize, usize)> {
        let key = name.to_ascii_uppercase();
        self.cells
            .iter()
            .find(|(n, _, _)| *n == key)
            .map(|(_, o, w)| (*o, *w))
            .ok_or_else(|| CoreError::Format(format!("HI missing field {name}")))
    }

    fn raw<'a>(&self, rec: &'a [u8], name: &str) -> Result<&'a [u8]> {
        let (o, w) = self.span(name)?;
        Ok(&rec[o..o + w])
    }

    fn get(&self, rec: &[u8], name: &str) -> Result<String> {
        Ok(latin1(self.raw(rec, name)?))
    }

    fn set(&self, rec: &mut [u8], name: &str, val: &str, left: bool) -> Result<()> {
        let (o, w) = self.span(name)?;
        let bytes = val.as_bytes();
        if bytes.len() > w {
            return Err(CoreError::Format(format!(
                "value {val:?} wider than HI.{name} ({w})"
            )));
        }
        let cell = &mut rec[o..o + w];
        for b in cell.iter_mut() {
            *b = b' ';
        }
        if left {
            cell[..bytes.len()].copy_from_slice(bytes);
        } else {
            cell[w - bytes.len()..].copy_from_slice(bytes);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_roundtrip_and_entities() {
        let fields = vec![
            ("username".to_string(), "A & B <ops>".to_string()),
            ("userinitials".to_string(), "   AB".to_string()),
        ];
        let xml = values_xml(&fields, CHECKSUM_VERSION);
        let items = parse_values(&xml);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].name, "username");
        assert_eq!(items[0].text, "A & B <ops>");
        assert_eq!(items[1].text, "   AB");
        assert_eq!(items[2].name, "checksumversion");
        assert_eq!(items[2].kind.as_deref(), Some("integer"));
        assert_eq!(items[2].text, "4097");
        assert!(xml.ends_with("</values>\r\n"));
    }

    #[test]
    fn signoff_decode() {
        let mut e = HistoryEvent::at("20260101000000");
        e.event_type = "DOCSIGN3".into();
        e.category = "DOCS".into();
        assert_eq!(e.kind(), HistoryKind::Signoff(3));
        assert_eq!(e.signoff_role(), Some(3));
        assert_eq!(kind_cells(&e.kind()).unwrap(), ("DOCS".into(), "DOCSIGN3".into()));
        assert!(kind_cells(&HistoryKind::Signoff(9)).is_err());
    }

    #[test]
    fn census_and_kinds_mapping() {
        assert_eq!(
            kind_cells(&HistoryKind::DocumentDelete).unwrap(),
            ("DOC-".into(), "DOCDEL".into())
        );
        assert_eq!(
            kind_cells(&HistoryKind::DocumentProperty).unwrap(),
            ("DOC*".into(), "DOCPROP".into())
        );
        assert_eq!(
            kind_cells(&HistoryKind::UserAccess).unwrap(),
            ("FILA".into(), "FACCUSR".into())
        );
    }
}

