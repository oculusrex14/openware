//! cw-app lockdown / lockout (P5.D4b).
//!
//! Behavior contract: `docs\specs\lockdown.md` v1.0, evidence F68 (COM fixture
//! `a sample copy`, USI copy, 2026-10-23).
//!
//! `client.Lockdown()` is a one-way engagement lock. Its observable footprint:
//!
//! 1. `.ac` hub ??stream `\x05DocumentSummaryInformation` (OLE property set,
//!    section 1 user-defined props): pid 273 `VT_LPSTR` `""` -> `"L"`,
//!    pid 267 `VT_FILETIME` -> next midnight (the lockdown date), pid 277
//!    `VT_FILETIME` -> the lock stamp, pid 321 (last change) -> the same
//!    stamp. Nothing else in the hub changes (all other streams byte-equal).
//! 2. `FP` (CLP workpaper cells): cell 275 seq 1 `TEXT='L'`, cell 278 seq 1
//!    `TEXT=<Excel serial of the lockdown date>` (46258 = 2026-10-24), cell
//!    287 seq 1 `TEXT='N'`, cell 288 seq 1 `TEXT=<stamp>`, cell 288 seq 2
//!    TEXT unchanged; every touched row also gets `CWDATE=<stamp>`.
//! 3. `SH`: one row per locked document ??`PERIOD='Y1'`, `CWDATE=<stamp>`,
//!    `LOCKDOWN` unchanged (already `Y` on the review template).
//! 4. `HI`: one `LOCK/FILELOCK` row (unanchored) + one `LOCK/DOCLOCK` row per
//!    locked document (`OBJECTID` = SH CWGUID, `OBJECTTYPE='SH'`), both with
//!    `USER=''` and `VALUES` = `<values>..checksumversion 4097..</values>\\r\\n`.
//!    CW additionally writes `MSD` milestone snapshot rows (47 here, 6.2 MB of
//!    FPT blobs) ??see [`MILESTONE_SNAPSHOTS`] and spec residual 7.1.
//!
//! Reader side: [`LockdownState::read`] is the app's `IsLockedDown` /
//! `LockdownDate` equivalent; [`document_locked`] is the per-document view.
//!
//! Residual v1 (documented, not silently wrong): CW additionally writes `MSD`
//! milestone snapshot rows (47 here, 6.2 MB of FPT blobs) which v1 skips ??the
//! app keeps the audit trail but not the document snapshots (spec 7.1). The
//! document selection itself is decoded: every live `SH` row with
//! `PRG='ac'` + `LOCKDOWN='Y'` (84 of 93 live; the other 9 are program/library
//! rows) is locked, and that set equals CW's `DOCLOCK` set exactly.

use std::fs;
use std::path::{Path, PathBuf};

use cw_core::cfb::Cfb;
use cw_core::dbf::Table;
use cw_core::guid::Rng;
use cw_core::{CoreError, Result};

use crate::history::{values_xml, HistoryEvent, HistoryLog, CHECKSUM_VERSION};

/// Hub stream carrying the engagement document-summary properties.
pub const SUMMARY_STREAM: &str = "\u{5}DocumentSummaryInformation";

/// Property id: lock stamp (operation wall clock, FILETIME).
pub const PID_LOCKDOWN_STAMP: u32 = 267;
/// Property id: lock state (`""` = open, `"L"` = locked).
pub const PID_LOCK_STATE: u32 = 273;
/// Property id: lockdown date marker (day end 23:59:59, FILETIME; the COM
/// `LockdownDate` renders it as the following local midnight).
pub const PID_LOCKDOWN_DATE: u32 = 277;
/// Property id: last-change stamp (FILETIME, cosmetic).
pub const PID_LAST_CHANGE: u32 = 321;

/// `SH.PERIOD` marker CW writes on every locked document.
pub const LOCKED_PERIOD: &str = "Y1";

/// Number of CW `MSD` milestone rows observed on the fixture lockdown (v1 does
/// not emit them; spec residual 7.1).
pub const MILESTONE_SNAPSHOTS: usize = 47;

const VT_I2: u32 = 2;
const VT_I4: u32 = 3;
const VT_BOOL: u32 = 11;
const VT_LPSTR: u32 = 30;
const VT_LPWSTR: u32 = 31;
const VT_FILETIME: u32 = 64;

/// One property-set value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PropValue {
    Empty,
    I2(i16),
    I4(i32),
    Bool(bool),
    LpStr(String),
    LpWStr(String),
    FileTime(u64),
    /// Any other VT (carried raw, length from the type table).
    Other(u32),
}

/// Minimal OLE property-set stream (MS-OLEPS) with in-place patching.
///
/// The hub's summary stream is fixed-size in the corpus (5204 bytes, section 1
/// at 108): values are patched inside their existing slots so the container
/// needs no sector surgery.
#[derive(Clone, Debug)]
pub struct PropertySet {
    bytes: Vec<u8>,
    /// (section offset, property count) for every set in the stream.
    sections: Vec<(usize, usize)>,
}

impl PropertySet {
    pub fn parse(bytes: &[u8]) -> Result<PropertySet> {
        if bytes.len() < 28 {
            return Err(CoreError::Format("propset: too small".into()));
        }
        let numsets = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]) as usize;
        if 28 + numsets * 20 > bytes.len() || numsets == 0 {
            return Err(CoreError::Format("propset: bad header".into()));
        }
        let mut sections = Vec::with_capacity(numsets);
        for i in 0..numsets {
            let base = 28 + i * 20;
            let off = u32::from_le_bytes([
                bytes[base + 16],
                bytes[base + 17],
                bytes[base + 18],
                bytes[base + 19],
            ]) as usize;
            if off + 8 > bytes.len() {
                return Err(CoreError::Format("propset: section out of range".into()));
            }
            let n = u32::from_le_bytes([bytes[off + 4], bytes[off + 5], bytes[off + 6], bytes[off + 7]])
                as usize;
            if off + 8 + n * 8 > bytes.len() {
                return Err(CoreError::Format("propset: prop table out of range".into()));
            }
            sections.push((off, n));
        }
        Ok(PropertySet { bytes: bytes.to_vec(), sections })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Property ids present in the user-defined section.
    pub fn pids(&self) -> Vec<u32> {
        let Some((off, n)) = self.sections.get(self.user_section()) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(*n);
        for i in 0..*n {
            let p = off + 8 + i * 8;
            out.push(u32::from_le_bytes([
                self.bytes[p],
                self.bytes[p + 1],
                self.bytes[p + 2],
                self.bytes[p + 3],
            ]));
        }
        out
    }

    /// Section 1 = the user-defined (custom) set CW uses for file state.
    pub fn user_section(&self) -> usize {
        if self.sections.len() > 1 { 1 } else { 0 }
    }

    fn prop_offset(&self, pid: u32) -> Option<usize> {
        let (off, n) = *self.sections.get(self.user_section())?;
        for i in 0..n {
            let p = off + 8 + i * 8;
            let id = u32::from_le_bytes([self.bytes[p], self.bytes[p + 1], self.bytes[p + 2], self.bytes[p + 3]]);
            if id == pid {
                let po = u32::from_le_bytes([
                    self.bytes[p + 4],
                    self.bytes[p + 5],
                    self.bytes[p + 6],
                    self.bytes[p + 7],
                ]) as usize;
                return Some(off + po);
            }
        }
        None
    }

    /// Read a property by id from the user-defined section.
    pub fn get(&self, pid: u32) -> Option<PropValue> {
        let v = self.prop_offset(pid)?;
        let vt = self.u32(v);
        Some(match vt {
            VT_I2 => PropValue::I2(i16::from_le_bytes([self.bytes[v + 4], self.bytes[v + 5]])),
            VT_I4 => PropValue::I4(self.u32(v + 4) as i32),
            VT_BOOL => PropValue::Bool(
                i16::from_le_bytes([self.bytes[v + 4], self.bytes[v + 5]]) != 0,
            ),
            VT_LPSTR => {
                let n = self.u32(v + 4) as usize;
                let s = &self.bytes[v + 8..v + 8 + n];
                PropValue::LpStr(String::from_utf8_lossy(trim_nul(s)).into_owned())
            }
            VT_LPWSTR => {
                let n = self.u32(v + 4) as usize;
                PropValue::LpWStr(decode_utf16(&self.bytes[v + 8..v + 8 + n * 2]))
            }
            VT_FILETIME => {
                let mut b = [0u8; 8];
                b.copy_from_slice(&self.bytes[v + 4..v + 12]);
                PropValue::FileTime(u64::from_le_bytes(b))
            }
            other => PropValue::Other(other),
        })
    }

    pub fn get_filetime(&self, pid: u32) -> Option<u64> {
        match self.get(pid)? {
            PropValue::FileTime(v) => Some(v),
            _ => None,
        }
    }

    pub fn get_string(&self, pid: u32) -> Option<String> {
        match self.get(pid)? {
            PropValue::LpStr(s) | PropValue::LpWStr(s) => Some(s),
            _ => None,
        }
    }

    /// Byte capacity of a value slot (from the value start to the next value
    /// or the end of the section).
    fn slot_end(&self, v: usize) -> usize {
        let Some((off, n)) = self.sections.get(self.user_section()) else {
            return v;
        };
        let mut end = off + self.u32(*off) as usize;
        if end <= *off || end > self.bytes.len() {
            end = self.bytes.len();
        }
        for i in 0..*n {
            let p = off + 8 + i * 8;
            let po = u32::from_le_bytes([
                self.bytes[p + 4],
                self.bytes[p + 5],
                self.bytes[p + 6],
                self.bytes[p + 7],
            ]) as usize;
            let a = off + po;
            if a > v && a < end {
                end = a;
            }
        }
        end
    }

    /// Overwrite an existing string slot (`VT_LPSTR` or `VT_LPWSTR`).
    pub fn set_string(&mut self, pid: u32, value: &str) -> Result<()> {
        let v = self
            .prop_offset(pid)
            .ok_or_else(|| CoreError::Format(format!("propset: pid {pid} missing")))?;
        let cap_bytes = self.slot_end(v).saturating_sub(v + 8);
        match self.u32(v) {
            VT_LPSTR => {
                let need = value.len() + 1;
                if need > cap_bytes {
                    return Err(CoreError::Format(format!(
                        "propset: pid {pid} slot {cap_bytes}B too small for {need}B"
                    )));
                }
                for b in &mut self.bytes[v + 8..v + 8 + cap_bytes] {
                    *b = 0;
                }
                self.bytes[v + 8..v + 8 + value.len()].copy_from_slice(value.as_bytes());
                self.set_u32(v + 4, need as u32);
            }
            VT_LPWSTR => {
                let cap_chars = cap_bytes / 2;
                let need = value.chars().count() + 1;
                if need > cap_chars {
                    return Err(CoreError::Format(format!(
                        "propset: pid {pid} wide slot {cap_chars} too small for {need}"
                    )));
                }
                for b in &mut self.bytes[v + 8..v + 8 + cap_bytes] {
                    *b = 0;
                }
                for (i, u) in value.encode_utf16().enumerate() {
                    let o = v + 8 + i * 2;
                    self.bytes[o..o + 2].copy_from_slice(&u.to_le_bytes());
                }
                self.set_u32(v + 4, need as u32);
            }
            other => {
                return Err(CoreError::Format(format!(
                    "propset: pid {pid} is not a string (vt {other})"
                )))
            }
        }
        Ok(())
    }

    /// Overwrite an existing `VT_FILETIME` slot.
    pub fn set_filetime(&mut self, pid: u32, value: u64) -> Result<()> {
        let v = self
            .prop_offset(pid)
            .ok_or_else(|| CoreError::Format(format!("propset: pid {pid} missing")))?;
        if self.u32(v) != VT_FILETIME {
            return Err(CoreError::Format(format!("propset: pid {pid} is not VT_FILETIME")));
        }
        self.bytes[v + 4..v + 12].copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    fn u32(&self, off: usize) -> u32 {
        u32::from_le_bytes([
            self.bytes[off],
            self.bytes[off + 1],
            self.bytes[off + 2],
            self.bytes[off + 3],
        ])
    }

    fn set_u32(&mut self, off: usize, val: u32) {
        self.bytes[off..off + 4].copy_from_slice(&val.to_le_bytes());
    }
}

fn trim_nul(s: &[u8]) -> &[u8] {
    match s.iter().position(|&b| b == 0) {
        Some(p) => &s[..p],
        None => s,
    }
}

fn decode_utf16(bytes: &[u8]) -> String {
    let ucs: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let s = String::from_utf16_lossy(&ucs);
    s.trim_end_matches('\u{0}').to_string()
}

/// Engagement lock state (`IsLockedDown` / `LockdownDate` equivalents).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LockdownState {
    pub locked: bool,
    /// Next-midnight lockdown date (FILETIME ticks), when locked.
    pub date: Option<u64>,
    /// Operation stamp (FILETIME ticks), when locked.
    pub stamp: Option<u64>,
}

impl LockdownState {
    /// Read the state from the hub `.ac` bytes (the authoritative mirror).
    pub fn from_ac(ac: &[u8]) -> Result<LockdownState> {
        let cfb = Cfb::parse(ac)?;
        let stream = cfb.stream(SUMMARY_STREAM)?;
        let set = PropertySet::parse(&stream)?;
        let locked = set.get_string(PID_LOCK_STATE).map(|s| s.starts_with('L')).unwrap_or(false);
        Ok(LockdownState {
            locked,
            date: set.get_filetime(PID_LOCKDOWN_DATE),
            stamp: set.get_filetime(PID_LOCKDOWN_STAMP),
        })
    }

    /// Read the state from `<dir>\<stem>.ac`.
    pub fn read(dir: impl AsRef<Path>, stem: &str) -> Result<LockdownState> {
        let ac = read_hub(dir.as_ref(), stem)?;
        LockdownState::from_ac(&ac)
    }
}

/// Apply the lock properties inside a hub image (in place, same length).
pub fn mark_lockdown(ac: &mut [u8], stamp: u64, date: u64) -> Result<LockdownState> {
    let cfb = Cfb::parse(ac)?;
    let mut set = PropertySet::parse(&cfb.stream(SUMMARY_STREAM)?)?;
    set.set_string(PID_LOCK_STATE, "L")?;
    set.set_filetime(PID_LOCKDOWN_DATE, date)?;
    set.set_filetime(PID_LOCKDOWN_STAMP, stamp)?;
    if set.get_filetime(PID_LAST_CHANGE).is_some() {
        set.set_filetime(PID_LAST_CHANGE, stamp)?;
    }
    cfb.patch_stream_in_place(ac, SUMMARY_STREAM, set.bytes())?;
    LockdownState::from_ac(ac)
}

/// Per-document lock view (`doc.Lockdown` / locked-period marker).
pub fn document_locked(sh: &Table, document_id: &str) -> Result<bool> {
    for r in 0..sh.count() {
        if sh.cell_str(r, "CWGUID").as_deref().map(str::trim) == Some(document_id) {
            let period = sh.cell_str(r, "PERIOD").unwrap_or_default();
            return Ok(period.trim_start().starts_with('Y'));
        }
    }
    Err(CoreError::Format(format!(
        "SH has no document {document_id}"
    )))
}

/// Default document selection ??exact CW law (F68): every live `SH` row with
/// `PRG='ac'` and `LOCKDOWN='Y'`. On the corpus this is the same 84 documents
/// CW writes `DOCLOCK` rows for (93 live rows total; the other 9 are program
/// and library nodes).
pub fn select_documents(sh: &Table) -> Vec<String> {
    let mut out = Vec::new();
    for r in 0..sh.count() {
        if sh.is_deleted(r) {
            continue;
        }
        if sh.cell_str(r, "PRG").unwrap_or_default().trim() != "ac" {
            continue;
        }
        if sh.cell_str(r, "LOCKDOWN").unwrap_or_default().trim() != "Y" {
            continue;
        }
        let id = sh.cell_str(r, "CWGUID").unwrap_or_default();
        let id = id.trim();
        if !id.is_empty() {
            out.push(id.to_string());
        }
    }
    out
}

/// Excel 1900 date serial (the `FP/CLP` date encoding; F44/F65).
pub fn excel_serial(year: i32, month: u32, day: u32) -> u32 {
    // days since 1899-12-30 (the Excel epoch with the leap-year quirk folded in)
    let mut days = 0i64;
    for y in 1900..year {
        days += if is_leap(y) { 366 } else { 365 };
    }
    let mdays = [31u32, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for m in 1..month {
        days += mdays[(m - 1) as usize] as i64;
        if m == 2 && is_leap(year) {
            days += 1;
        }
    }
    days += (day - 1) as i64;
    // days = whole days since 1900-01-01. Excel serial 1 = 1900-01-01, and the
    // phantom 1900-02-29 (serial 60) shifts every later date by one more.
    let shift = if (year, month, day) >= (1900, 3, 1) { 2 } else { 1 };
    (days + shift) as u32
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Request for one engagement lockdown.
#[derive(Clone, Debug)]
pub struct LockdownRequest {
    /// `YYYYMMDDhhmmss` operation stamp (deterministic for golden runs).
    pub stamp: String,
    /// Operation stamp as FILETIME ticks (UTC).
    pub stamp_filetime: u64,
    /// Next-midnight lockdown date as FILETIME ticks (UTC).
    pub date_filetime: u64,
    /// Excel serial of the lockdown date (FP/CLP cell 278).
    pub fp_date_value: String,
    /// Explicit document set; empty => [`select_documents`].
    pub documents: Vec<String>,
    /// Optional acting identity (CW wrote blanks with no signed-in user).
    pub user: String,
    pub initials: String,
}

/// What a lockdown wrote (the gate surface).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LockdownReport {
    pub documents: usize,
    pub sh_rows: usize,
    pub fp_cells: usize,
    pub lock_events: usize,
    pub milestone_events_skipped: usize,
    pub ac_marked: bool,
    pub already_locked: bool,
}

/// Lock an engagement in place: `SH` document rows, `FP` CLP cells, `HI`
/// audit rows, `.ac` summary properties. One-way: a second call is refused.
pub fn lockdown_engagement(
    dir: impl AsRef<Path>,
    stem: &str,
    req: &LockdownRequest,
) -> Result<LockdownReport> {
    let dir = dir.as_ref();
    let mut report = LockdownReport::default();

    // 1) one-way gate: CW's second Lockdown() is a silent no-op (fixture
    // d4-lockdown-l1: call2 returned normally, date unchanged).
    let ac_path = hub_path(dir, stem)?;
    let mut ac = fs::read(&ac_path)?;
    if LockdownState::from_ac(&ac)?.locked {
        report.already_locked = true;
        return Ok(report);
    }

    // 2) documents
    let sh_path = table_path(dir, stem, "SH")?;
    let mut sh = Table::parse_path(&sh_path)?;
    let docs = if req.documents.is_empty() {
        select_documents(&sh)
    } else {
        req.documents.clone()
    };
    for id in &docs {
        let row = find_row(&sh, "CWGUID", id)?
            .ok_or_else(|| CoreError::Format(format!("SH has no document {id}")))?;
        set_cell(&mut sh, row, "PERIOD", LOCKED_PERIOD)?;
        set_cell(&mut sh, row, "CWDATE", &req.stamp)?;
        report.sh_rows += 1;
    }
    report.documents = docs.len();

    // 3) FP/CLP cells
    let fp_path = table_path(dir, stem, "FP")?;
    let mut fp = Table::parse_path(&fp_path)?;
    let stamp_serial = req.fp_date_value.clone();
    let mut touched = Vec::new();
    for (cell, value) in [
        (275u32, "L".to_string()),
        (278u32, stamp_serial),
        (287u32, "N".to_string()),
        (288u32, req.stamp.clone()),
    ] {
        let row = find_clp_row(&fp, cell, 1)?
            .ok_or_else(|| CoreError::Format(format!("FP/CLP cell {cell} seq 1 missing")))?;
        set_cell(&mut fp, row, "TEXT", &value)?;
        set_cell(&mut fp, row, "CWDATE", &req.stamp)?;
        touched.push(row);
        report.fp_cells += 1;
    }
    if let Some(row) = find_clp_row(&fp, 288, 2)? {
        if !touched.contains(&row) {
            set_cell(&mut fp, row, "CWDATE", &req.stamp)?;
            touched.push(row);
            report.fp_cells += 1;
        }
    }

    // 4) HI audit rows: FILELOCK then DOCLOCK per document
    let mut hi = HistoryLog::open(dir, stem)?;
    let mut rng = Rng::new_seeded(seed_from(&req.stamp, &req.user));
    let mut file = HistoryEvent::at(req.stamp.clone());
    file.category = "LOCK".into();
    file.event_type = "FILELOCK".into();
    file.user = req.user.clone();
    file.values = values_xml(&[], CHECKSUM_VERSION);
    hi.append(&file, &mut rng)?;
    report.lock_events += 1;
    for id in &docs {
        let mut ev = HistoryEvent::at(req.stamp.clone());
        ev.category = "LOCK".into();
        ev.event_type = "DOCLOCK".into();
        ev.object_id = id.clone();
        ev.object_type = "SH".into();
        ev.user = req.user.clone();
        ev.values = values_xml(&[], CHECKSUM_VERSION);
        hi.append(&ev, &mut rng)?;
        report.lock_events += 1;
    }
    report.milestone_events_skipped = MILESTONE_SNAPSHOTS;

    // 5) hub mirror last (after the tables are staged successfully)
    mark_lockdown(&mut ac, req.stamp_filetime, req.date_filetime)?;
    report.ac_marked = true;

    sh.write_path(&sh_path)?;
    fp.write_path(&fp_path)?;
    hi.save(dir, stem)?;
    fs::write(&ac_path, &ac)?;
    let _ = &req.initials;
    Ok(report)
}

fn seed_from(stamp: &str, user: &str) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for b in stamp.bytes().chain(user.bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h | 1
}

fn find_row(t: &Table, field: &str, value: &str) -> Result<Option<usize>> {
    for r in 0..t.count() {
        if t.is_deleted(r) {
            continue;
        }
        if t.cell_str(r, field).map(|s| s.trim() == value).unwrap_or(false) {
            return Ok(Some(r));
        }
    }
    Ok(None)
}

fn find_clp_row(fp: &Table, cellno: u32, seqno: u32) -> Result<Option<usize>> {
    for r in 0..fp.count() {
        if fp.is_deleted(r) {
            continue;
        }
        if fp.cell_str(r, "WKS").map(|s| s.trim() == "CLP").unwrap_or(false)
            && fp
                .cell_str(r, "CELLNO")
                .and_then(|s| s.trim().parse::<u32>().ok())
                == Some(cellno)
            && fp
                .cell_str(r, "SEQNO")
                .and_then(|s| s.trim().parse::<u32>().ok())
                == Some(seqno)
        {
            return Ok(Some(r));
        }
    }
    Ok(None)
}

fn set_cell(t: &mut Table, row: usize, field: &str, value: &str) -> Result<()> {
    let idx = t
        .field_index(field)
        .ok_or_else(|| CoreError::Format(format!("table has no field {field}")))?;
    let width = t.fields[idx].len as usize;
    if value.len() > width {
        return Err(CoreError::Format(format!(
            "value {value:?} wider than {field} ({width})"
        )));
    }
    let offs = t.offsets();
    let start = offs[idx];
    let rec = t
        .records
        .get_mut(row)
        .ok_or_else(|| CoreError::Format(format!("row {row} out of range")))?;
    for b in &mut rec[start..start + width] {
        *b = b' ';
    }
    rec[start..start + value.len()].copy_from_slice(value.as_bytes());
    Ok(())
}

fn hub_path(dir: &Path, stem: &str) -> Result<PathBuf> {
    let want = format!("{stem}.ac").to_ascii_lowercase();
    if let Ok(rd) = fs::read_dir(dir) {
        for entry in rd.flatten() {
            if entry.file_name().to_string_lossy().to_ascii_lowercase() == want {
                return Ok(entry.path());
            }
        }
    }
    Err(CoreError::Format(format!(
        "missing hub {stem}.ac in {}",
        dir.display()
    )))
}

fn table_path(dir: &Path, stem: &str, suffix: &str) -> Result<PathBuf> {
    let want = format!("{stem}{suffix}.dbf").to_ascii_lowercase();
    if let Ok(rd) = fs::read_dir(dir) {
        for entry in rd.flatten() {
            if entry.file_name().to_string_lossy().to_ascii_lowercase() == want {
                return Ok(entry.path());
            }
        }
    }
    Err(CoreError::Format(format!(
        "missing {stem}{suffix}.dbf in {}",
        dir.display()
    )))
}

fn read_hub(dir: &Path, stem: &str) -> Result<Vec<u8>> {
    Ok(fs::read(hub_path(dir, stem)?)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excel_serials() {
        assert_eq!(excel_serial(2026, 8, 24), 46258);
        assert_eq!(excel_serial(2026, 10, 24), 46319);
        assert_eq!(excel_serial(2024, 1, 1), 45292);
    }

    #[test]
    fn property_set_roundtrip() {
        // synthetic 2-section stream with a string + filetime in section 1
        let mut b = vec![0u8; 28 + 2 * 20];
        b[0] = 0xFE;
        b[1] = 0xFF;
        b[24..28].copy_from_slice(&2u32.to_le_bytes());
        let off0 = b.len() as u32;
        b[28 + 16..28 + 20].copy_from_slice(&off0.to_le_bytes());
        let sec0 = vec![0u8; 40];
        b.extend_from_slice(&sec0);
        let off1 = b.len() as u32;
        b[28 + 20 + 16..28 + 20 + 20].copy_from_slice(&off1.to_le_bytes());
        // section: size, n=2, (pid,off)x2, values
        let mut sec = Vec::new();
        sec.extend_from_slice(&0u32.to_le_bytes()); // size (unused by parser)
        sec.extend_from_slice(&2u32.to_le_bytes());
        sec.extend_from_slice(&PID_LOCK_STATE.to_le_bytes());
        sec.extend_from_slice(&24u32.to_le_bytes());
        sec.extend_from_slice(&PID_LOCKDOWN_STAMP.to_le_bytes());
        sec.extend_from_slice(&36u32.to_le_bytes());
        sec.extend_from_slice(&VT_LPSTR.to_le_bytes());
        sec.extend_from_slice(&1u32.to_le_bytes()); // len incl nul
        sec.extend_from_slice(&[0u8, 0, 0, 0]);
        sec.extend_from_slice(&VT_FILETIME.to_le_bytes());
        sec.extend_from_slice(&7u64.to_le_bytes());
        b.extend_from_slice(&sec);
        let mut set = PropertySet::parse(&b).unwrap();
        assert_eq!(set.get_string(PID_LOCK_STATE).as_deref(), Some(""));
        assert_eq!(set.get_filetime(PID_LOCKDOWN_STAMP), Some(7));
        set.set_string(PID_LOCK_STATE, "L").unwrap();
        set.set_filetime(PID_LOCKDOWN_STAMP, 9).unwrap();
        let again = PropertySet::parse(set.bytes()).unwrap();
        assert_eq!(again.get_string(PID_LOCK_STATE).as_deref(), Some("L"));
        assert_eq!(again.get_filetime(PID_LOCKDOWN_STAMP), Some(9));
    }
}

