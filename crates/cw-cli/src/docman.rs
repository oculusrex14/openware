//! Document Manager (DMS): the **file-centric** view of one engagement.
//!
//! CaseWare's Document Manager is not the `SH` table; it is "the dynamic list
//! of documents making up the file". A document is a physical file in the
//! engagement folder (`.docx`, `.xlsx`, `.pdf`, `.cvw`, ...); `SH` supplies the
//! metadata (schedule, title, signoff, flags) for the subset CW indexed. This
//! module unions the two lenses:
//!
//! * **the walk** — every regular file under the engagement directory, with the
//!   same skip rules the package writer uses (`cw_app::backup`): dot-prefixed
//!   names (`.DS_Store`, macOS `._<name>` AppleDouble resource forks) and the
//!   transient `.ac_` / `.~ocw` names are never documents. Nothing else is
//!   filtered: the `dbf`/`cdx`/`fpt` kernel tables are listed and flagged
//!   `internal` rather than hidden, because they are part of the file too.
//! * **the register** — live `SH` rows, read through [`DocumentBook`] with a
//!   readable-prefix repair (F63): hull2024's `SH` carries zero-fill synthesis
//!   past row 603, so decoding stops at the first row whose delete flag is
//!   neither `0x20` nor `0x2A` instead of failing the whole view.
//!
//! ## The join, and where it misses
//!
//! `SH` has no filename column. CW's own identity for a row is `(NAME,
//! SCHEDULE, CVID)`; the file on disk carries a name. The join is therefore a
//! documented best-effort, tested tier by tier (see [`JOIN_TIERS`]):
//!
//! | tier | key | fires when | hull2024 |
//! |---|---|---|---|
//! | `cvid` | `SH.CVID` | `.cvw` named `<stem><CVID>.cvw` | 11 |
//! | `schedule` | `SH.SCHEDULE` | file stem *is* the schedule token (`0160.docx`) | 1 |
//! | `schedule-prefix` | `SH.SCHEDULE` | stem starts with a digit-leading token (`0146_Review.xlsx` → `0146 REV`) | 7 |
//! | `title` | `SH.SCH_DESC` | stem equals the title (`0139 … .docx`) | 8 |
//! | `title-contains` | `SH.SCH_DESC` | stem and title contain each other, both ≥ 8 chars (`Inventory analytics.xls`) | 11 |
//! | `name` | `SH.NAME` | 2-char analysis tag equals the stem (`TS.cdx` → `NAME='TS'`) | 3 |
//!
//! Keys are compared **normalised**: NFKD-folded to ASCII, lower-cased, with
//! every non-alphanumeric character removed. Rows whose `SCHEDULE`/`SCH_DESC`/
//! `CVID` contain control bytes are garbage (the F63 tail) and are never used
//! as join keys; headings are excluded too — their `SCHEDULE` is a `CWGUID`,
//! never a filename.
//!
//! Known misses, stated rather than papered over:
//!
//! * **attachments** — several files can join one row (`0146_Review.xlsx`,
//!   `-2`, `-3` all land on `0146 REV`). The row is reported with
//!   `ambiguous:true` and its `candidates` count.
//! * **no token at all** — `Summary.pdf`, `ss.XLS`, `1COLGR.CGF`, style files
//!   and the engagement machinery (`.ac`, `.TVR`, `.locks`) simply have no SH
//!   row; they are shown as unindexed files.
//! * **wrong-token prefixes** — `0272.01_DisclosureChecklist…docx` joins `0272`
//!   by prefix although CW files it under `0272.01`; the tier is recorded so
//!   the UI can show how the row was matched instead of pretending certainty.
//! * **register-only rows** — analyses, headings, index rows and forms with no
//!   file on disk (`NAME='TB'`, `CVID` blank) are listed with `backing:
//!   "register"`, because CW's list is documents *making up the file*, not
//!   *files found on disk*.
//!
//! ## `.cvw` preview
//!
//! [`preview`] reuses the `cw_cv` public pipeline: [`Cvw::parse`] →
//! `TextTable` built from the `Form/Strings` pool through each block's trailer
//! slot (`slot + POOL_INDEX_BASE`, the F7 law) → [`render::render`] →
//! [`sheet::sheet`]. When the document cannot be parsed, has no cell
//! directory, or the pool is absent (the F63 cut), the preview reports exactly
//! that — metadata plus a raw download — and **never** invents cell text.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cw_app::documents::{DocumentBook, ShDocument};
use cw_app::signoff::{SignoffBook, SignoffStage};
use cw_core::dbf::Table;
use cw_core::sha256::sha256_hex;
use cw_cv::{layout, render, sheet, Cvw, LinkTable, TextTable, WksRegistry};

use crate::json_str;
use crate::web::ApiError;

/// Join tiers in evaluation order, with the one-line law each implements.
pub const JOIN_TIERS: [(&str, &str); 6] = [
    ("cvid", "CVW filename tail equals SH.CVID (01200RG0)"),
    ("schedule", "file stem equals SH.SCHEDULE as stored (0160, 0139)"),
    ("name", "file stem equals the 2-char SH.NAME analysis tag (TS)"),
    ("title", "file stem equals SH.SCH_DESC (0139 Cumulative Effect ...)"),
    ("schedule-prefix", "stem starts with a digit-leading schedule token (0146_Review)"),
    ("title-contains", "stem and title contain each other, both >= 8 chars"),
];

/// How deep the walk goes (same cap as the manifest walker).
const MAX_DEPTH: usize = 32;

// ---------------------------------------------------------------------------
// File classes
// ---------------------------------------------------------------------------

/// Physical-file taxonomy: `(kind, format label, preview mode)`.
///
/// `kind` is the Document Manager class; `internal` marks the engagement's own
/// kernel/machinery files (dbf/cdx/fpt, `.ac`, `.TVR`, `.locks`), which the UI
/// hides behind its "show internals" toggle but always counts.
pub fn file_class(ext: &str) -> (&'static str, &'static str, Option<&'static str>) {
    match ext {
        "cvw" | "cvv" => ("cvw", "CaseView document", Some("cvw")),
        "doc" | "docx" | "docm" | "rtf" | "txt" | "wpd" | "odt" => {
            ("word", "Word document", None)
        }
        "xls" | "xlsx" | "xlsm" | "xlsb" | "csv" | "ods" => ("excel", "Excel workbook", None),
        "pdf" => ("pdf", "PDF document", Some("pdf")),
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "tif" | "tiff" | "webp" | "emf" | "wmf" => {
            ("image", "Image", Some("image"))
        }
        "dbf" | "cdx" | "fpt" => ("internal", "Kernel table", None),
        "ac" | "locks" | "tvr" => ("internal", "Engagement machinery", None),
        // TimeValue amortization worksheets are real documents (one per loan);
        // the `.TVR` list beside them is the machinery.
        "tv" | "tv4" | "tv5" | "tv6" | "tvs" => ("timevalue", "TimeValue worksheet", None),
        "cgf" | "sty" => ("other", "CaseView style", None),
        "" => ("other", "Unknown", None),
        _ => ("other", "Other file", None),
    }
}

/// CW document class for one `SH` row (`TYPE` letter + `NAME`).
pub fn register_class(d: &ShDocument) -> &'static str {
    match d.kind.letter() {
        'H' => "Folder",
        'F' => "CaseView form",
        'A' => "Analysis",
        '*' => "Link",
        ' ' => "Index",
        'M' => "Letter",
        _ => "Other",
    }
}

/// MIME type for the raw-download route (`/api/docman/file`).
pub fn mime_for(ext: &str) -> &'static str {
    match ext {
        "cvw" | "cvv" => "application/vnd.caseware.caseview",
        "doc" => "application/msword",
        "docx" | "docm" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" | "xlsm" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "xlsb" => "application/vnd.ms-excel.sheet.binary.macroEnabled.12",
        "pdf" => "application/pdf",
        "rtf" => "application/rtf",
        "txt" | "csv" => "text/plain; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "webp" => "image/webp",
        "dbf" | "cdx" | "fpt" => "application/octet-stream",
        _ => "application/octet-stream",
    }
}

// ---------------------------------------------------------------------------
// Walk
// ---------------------------------------------------------------------------

/// One physical file in the engagement folder.
#[derive(Clone, Debug)]
pub struct PhysicalFile {
    /// File name (`01200RG0.cvw`).
    pub name: String,
    /// Path relative to the engagement directory, `/`-separated.
    pub rel: String,
    /// Relative parent directory (`""` at the root).
    pub dir: String,
    /// Lower-case extension without the dot.
    pub ext: String,
    pub bytes: u64,
    /// Modification time, seconds since the Unix epoch (0 when unknown).
    pub mtime: i64,
    pub kind: &'static str,
    pub format: &'static str,
    pub preview: Option<&'static str>,
    pub mime: &'static str,
    /// Digest of the bytes; `None` when `sha:false` was requested.
    pub sha: Option<String>,
}

/// The package writer's skip law (`cw_app::backup::is_os_noise`): a name that
/// starts with `.` is filesystem noise — `.DS_Store`, an AppleDouble `._<name>`
/// resource fork, or an editor dropping. The transient names the package path
/// also refuses are skipped the same way.
fn is_skipped(name: &str) -> bool {
    if name.starts_with('.') {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".ac_") || lower.ends_with(".~ocw") || lower.ends_with(".bak")
}

/// Walk `dir` recursively. Symlinks that escape the engagement root are skipped
/// (the raw-file route refuses them anyway; this keeps the listing honest).
pub fn walk(dir: &Path, sha: bool) -> Result<Vec<PhysicalFile>, ApiError> {
    let root = std::fs::canonicalize(dir)
        .map_err(|e| ApiError::not_found(format!("{}: {e}", dir.display())))?;
    let mut out = Vec::new();
    walk_inner(&root, &root, "", 0, sha, &mut out)?;
    out.sort_by(|a, b| {
        a.dir
            .to_ascii_lowercase()
            .cmp(&b.dir.to_ascii_lowercase())
            .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
    });
    Ok(out)
}

fn walk_inner(
    root: &Path,
    dir: &Path,
    prefix: &str,
    depth: usize,
    sha: bool,
    out: &mut Vec<PhysicalFile>,
) -> Result<(), ApiError> {
    if depth > MAX_DEPTH {
        return Err(ApiError::internal(format!(
            "docman: directory nesting exceeds {MAX_DEPTH} at {prefix}"
        )));
    }
    let entries = std::fs::read_dir(dir)
        .map_err(|e| ApiError::internal(format!("cannot list {}: {e}", dir.display())))?;
    for entry in entries {
        let entry =
            entry.map_err(|e| ApiError::internal(format!("cannot list {}: {e}", dir.display())))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if is_skipped(&name) {
            continue;
        }
        let path = entry.path();
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() {
            // A symlinked directory must not walk the host's filesystem.
            if let Ok(canon) = std::fs::canonicalize(&path) {
                if !canon.starts_with(root) {
                    continue;
                }
            }
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            walk_inner(root, &path, &rel, depth + 1, sha, out)?;
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let (kind, format, preview) = file_class(&ext);
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let sha_hex = if sha {
            match std::fs::read(&path) {
                Ok(bytes) => Some(sha256_hex(&bytes)),
                Err(e) => {
                    return Err(ApiError::internal(format!(
                        "cannot read {}: {e}",
                        path.display()
                    )))
                }
            }
        } else {
            None
        };
        out.push(PhysicalFile {
            name: name.clone(),
            rel: rel.clone(),
            dir: prefix.to_string(),
            ext: ext.clone(),
            bytes: meta.len(),
            mtime,
            kind,
            format,
            preview,
            mime: mime_for(&ext),
            sha: sha_hex,
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// SH register (readable-prefix repair)
// ---------------------------------------------------------------------------

/// The `SH` register as the DMS sees it, plus what the repair had to do.
pub struct ShIndex {
    pub book: DocumentBook,
    /// Signoff state per live row, index-aligned with `book.documents()`.
    signoff: Vec<Option<(SignoffStage, usize, bool, Option<u8>)>>,
    /// Records the header declares.
    pub declared: usize,
    /// Records actually decoded (the readable prefix).
    pub readable: usize,
    /// Records dropped because the tail is zero-fill synthesis (F63).
    pub dropped: usize,
    /// True when the file had to be repaired (missing EOF byte / short tail /
    /// garbage records) before it would decode at all.
    pub repaired: bool,
    /// Human-readable note for the API payload (`""` when nothing was wrong).
    pub note: String,
}

impl ShIndex {
    pub fn live(&self) -> usize {
        self.book.count()
    }

    pub fn deleted(&self) -> usize {
        self.declared.saturating_sub(self.live())
    }

    pub fn signoff_of(&self, index: usize) -> Option<(&'static str, usize, bool, Option<u8>)> {
        self.signoff.get(index).and_then(|s| *s).map(|(stage, signed, complete, next)| {
            let label = match stage {
                SignoffStage::Unsigned => "unsigned",
                SignoffStage::Prepared => "prepared",
                SignoffStage::Reviewed => "reviewed",
            };
            (label, signed, complete, next)
        })
    }
}

/// Read `<stem>SH.dbf`, repairing the F63 damaged tail when needed.
///
/// The repair is in memory only; the engagement is never written. Three
/// separate faults are handled, in order:
///
/// 1. **missing EOF** — the file ends on the last record byte instead of
///    `0x1A` (the expanded hull corpus); patch the terminator.
/// 2. **short tail** — the declared record count exceeds the bytes present;
///    lower the count to what fits.
/// 3. **garbage rows** — rows past the readable frontier are zero-fill
///    synthesis whose delete flag is neither `0x20` nor `0x2A`; stop at the
///    first one (this is exactly the `frontier` the documents gate pins).
pub fn read_sh(dir: &Path, stem: &str) -> Result<Option<ShIndex>, ApiError> {
    let Some(path) = find_table(dir, stem, "SH") else {
        return Ok(None);
    };
    let raw = std::fs::read(&path)
        .map_err(|e| ApiError::internal(format!("cannot read {}: {e}", path.display())))?;
    let (bytes, short_tail) = repair_table_bytes(&raw);
    let mut table = Table::parse(&bytes).map_err(|e| {
        ApiError::unprocessable(
            "invalid_data",
            format!("{}: {e}", path.display()),
        )
    })?;
    // The header's declared count is the register's own claim; the repair may
    // have lowered it to what the file actually carries.
    let declared = table.count() + short_tail;
    let frontier = readable_frontier(&table);
    let mut repaired = short_tail > 0 || frontier < table.count();
    let mut note = String::new();
    if short_tail > 0 {
        note.push_str(&format!(
            "file ends {short_tail} record(s) short of its declared count; "
        ));
    }
    if frontier < table.count() {
        let garbage = table.count() - frontier;
        note.push_str(&format!(
            "{garbage} row(s) past the readable frontier (row flag not 0x20/0x2A) dropped; "
        ));
        table.records.truncate(frontier);
        repaired = true;
    }
    let dropped = declared.saturating_sub(table.count());
    let book = DocumentBook::from_table(table.clone()).map_err(|e| {
        ApiError::unprocessable("invalid_data", format!("{}: {e}", path.display()))
    })?;
    let signoff = SignoffBook::from_table(table)
        .map(|b| {
            b.sheets()
                .iter()
                .map(|s| Some((s.stage(), s.signed_count(), s.is_complete(), s.next_level())))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(Some(ShIndex {
        readable: book.count(),
        declared,
        dropped,
        repaired,
        note: note.trim_end_matches("; ").to_string(),
        book,
        signoff,
    }))
}

/// Open `<stem>SH.dbf` as a [`DocumentBook`], repairing the F63 framing faults
/// in memory when the raw file will not decode (missing `0x1A`, short tail,
/// zero-fill rows past the readable frontier). A healthy table takes the plain
/// [`DocumentBook::open`] path and is byte-identical.
pub fn open_book(dir: &Path, stem: &str) -> cw_core::Result<DocumentBook> {
    match DocumentBook::open(dir, stem) {
        Ok(book) => Ok(book),
        Err(original) => {
            let Some(path) = find_table(dir, stem, "SH") else {
                return Err(original);
            };
            let raw = std::fs::read(&path)?;
            let (bytes, _) = repair_table_bytes(&raw);
            let mut table = Table::parse(&bytes)?;
            let frontier = readable_frontier(&table);
            table.records.truncate(frontier);
            DocumentBook::from_table(table)
        }
    }
}

/// Find `<stem><suffix>.dbf` (case-insensitive) in `dir`.
fn find_table(dir: &Path, stem: &str, suffix: &str) -> Option<PathBuf> {
    let want = format!("{stem}{suffix}.dbf").to_ascii_lowercase();
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase() == want)
                .unwrap_or(false)
        })
}

/// Patch a table's framing bytes in memory. Returns the bytes plus how many
/// declared records had to be dropped for the file to be complete.
fn repair_table_bytes(raw: &[u8]) -> (Vec<u8>, usize) {
    if raw.len() < 32 {
        return (raw.to_vec(), 0);
    }
    let count = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]) as usize;
    let header_len = u16::from_le_bytes([raw[8], raw[9]]) as usize;
    let record_len = u16::from_le_bytes([raw[10], raw[11]]) as usize;
    if record_len == 0 || header_len == 0 || header_len > raw.len() {
        return (raw.to_vec(), 0);
    }
    let data = raw.len().saturating_sub(header_len);
    let avail = data / record_len;
    let mut dropped = 0usize;
    let mut out = raw.to_vec();
    let mut count = count;
    if count > avail {
        dropped = count - avail;
        count = avail;
        out[4..8].copy_from_slice(&(count as u32).to_le_bytes());
    }
    let need = header_len + count * record_len + 1;
    out.truncate(need);
    if out.len() < need {
        // The payload ends mid-record: complete the framing with zeros so the
        // terminator lands where the header says it must.
        out.resize(need, 0);
    }
    if out[need - 1] != 0x1A {
        out[need - 1] = 0x1A;
    }
    (out, dropped)
}

/// First record whose delete flag is neither live (`0x20`) nor tombstone
/// (`0x2A`) — the F63 readable frontier.
fn readable_frontier(t: &Table) -> usize {
    t.records
        .iter()
        .position(|r| !matches!(r.first(), Some(0x20) | Some(0x2A)))
        .unwrap_or(t.records.len())
}

// ---------------------------------------------------------------------------
// Join
// ---------------------------------------------------------------------------

/// Normalised comparison key: NFKD-folded to ASCII, lower-cased, alphanumerics
/// only. `0146 REV` / `0146_rev` / `0146Rev` all fold to `0146rev`.
pub fn norm_key(s: &str) -> String {
    // NFKD folding without a Unicode table: drop accents by keeping ASCII and
    // mapping the Latin-1 accented letters the corpus actually carries.
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        let c = match ch {
            'à'..='å' => 'a',
            'À'..='Å' => 'A',
            'ç' => 'c',
            'Ç' => 'C',
            'è'..='ë' => 'e',
            'È'..='Ë' => 'E',
            'ì'..='ï' => 'i',
            'Ì'..='Ï' => 'I',
            'ñ' => 'n',
            'Ñ' => 'N',
            'ò'..='ö' => 'o',
            'Ò'..='Ö' => 'O',
            'ù'..='ü' => 'u',
            'Ù'..='Ü' => 'U',
            'ý' | 'ÿ' => 'y',
            'Ý' => 'Y',
            other => other,
        };
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        }
    }
    out
}

/// True when a cell value is real text: no control bytes (the F63 tail writes
/// zero-fill into `SCHEDULE`/`SCH_DESC`, which must never become a join key).
fn is_clean(s: &str) -> bool {
    !s.chars().any(|c| (c as u32) < 0x20 || c == '\u{7f}')
}

/// A schedule token that can open a filename (`0146`, `0272.01`).
///
/// Heading rows carry a `CWGUID` in `SCHEDULE` (`79B1993C-E48`, `9B70BC01-...`);
/// those start with a hex digit, so a leading digit alone is not enough. A real
/// schedule is short and never dash-delimited.
fn is_schedule_token(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() || s.len() > 16 || s.contains('-') {
        return false;
    }
    s.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false)
}

/// Every live row that may be a join candidate, with normalised keys.
pub struct RegisterIndex<'a> {
    pub rows: Vec<&'a ShDocument>,
    by_cvid: BTreeMap<String, Vec<usize>>,
    by_schedule: BTreeMap<String, Vec<usize>>,
    by_name: BTreeMap<String, Vec<usize>>,
    by_title: BTreeMap<String, Vec<usize>>,
}

impl<'a> RegisterIndex<'a> {
    pub fn build(book: &'a DocumentBook) -> RegisterIndex<'a> {
        let rows: Vec<&ShDocument> = book.documents().iter().collect();
        let mut idx = RegisterIndex {
            rows,
            by_cvid: BTreeMap::new(),
            by_schedule: BTreeMap::new(),
            by_name: BTreeMap::new(),
            by_title: BTreeMap::new(),
        };
        for (i, d) in idx.rows.iter().enumerate() {
            if d.kind.is_heading() {
                // A heading's SCHEDULE is its CWGUID; it never names a file.
                continue;
            }
            if is_clean(&d.cvid) {
                let k = norm_key(&d.cvid);
                if !k.is_empty() {
                    idx.by_cvid.entry(k).or_default().push(i);
                }
            }
            if is_clean(&d.schedule) {
                let k = norm_key(&d.schedule);
                if !k.is_empty() {
                    idx.by_schedule.entry(k).or_default().push(i);
                }
            }
            if is_clean(&d.name) {
                let k = norm_key(&d.name);
                if !k.is_empty() && k != "*" {
                    idx.by_name.entry(k).or_default().push(i);
                }
            }
            if is_clean(&d.title) {
                let k = norm_key(&d.title);
                if !k.is_empty() {
                    idx.by_title.entry(k).or_default().push(i);
                }
            }
        }
        idx
    }

    /// Join one physical file by name. Returns `(row index, tier, candidates)`.
    pub fn join(&self, file_name: &str, stem: &str) -> (Option<usize>, &'static str, usize) {
        let base = file_stem(file_name);
        // A CVW's filename tail is the CVID; every other file keeps its stem.
        let key_full = norm_key(&base);
        let key_stripped = {
            let low = base.to_ascii_lowercase();
            let s = stem.to_ascii_lowercase();
            if low.starts_with(&s) {
                norm_key(&base[s.len()..])
            } else {
                key_full.clone()
            }
        };

        // 1. CVID — the form code inside the filename (`<stem>01200RG0.cvw`).
        for key in [&key_stripped, &key_full] {
            if let Some(hits) = self.by_cvid.get(key.as_str()) {
                return (Some(hits[0]), "cvid", hits.len());
            }
        }
        // 2. SCHEDULE exact.
        for key in [&key_full, &key_stripped] {
            if let Some(hits) = self.by_schedule.get(key.as_str()) {
                return (Some(hits[0]), "schedule", hits.len());
            }
        }
        // 3. NAME (the 2-char analysis tag; fires on engine table names).
        if let Some(hits) = self.by_name.get(key_full.as_str()) {
            return (Some(hits[0]), "name", hits.len());
        }
        // 4. SCH_DESC exact.
        for key in [&key_full, &key_stripped] {
            if let Some(hits) = self.by_title.get(key.as_str()) {
                return (Some(hits[0]), "title", hits.len());
            }
        }
        // 5. Schedule prefix: `0146_Review.xlsx` -> `0146 REV`. Longest token
        //    wins; a token must end at a separator, so `01460.xlsx` cannot
        //    join `0146`.
        let raw = base.as_str();
        let mut best: Option<(usize, usize, usize)> = None; // (token len, row, candidates)
        for (row_idx, d) in self.rows.iter().enumerate() {
            if !is_clean(&d.schedule) || !is_schedule_token(&d.schedule) {
                continue;
            }
            let token = d.schedule.split_whitespace().next().unwrap_or("");
            let token_len = token.chars().count();
            if token_len < 3 || raw.len() < token_len {
                continue;
            }
            if !raw[..token_len].eq_ignore_ascii_case(token) {
                continue;
            }
            let boundary_ok = raw[token_len..]
                .chars()
                .next()
                .map(|c| matches!(c, '_' | '-' | '.' | ' ' | '(' | ')'))
                .unwrap_or(true);
            if !boundary_ok {
                continue;
            }
            let candidates = self.by_schedule.get(&norm_key(&d.schedule)).map(|v| v.len()).unwrap_or(1);
            match best {
                Some((len, _, _)) if len >= token_len => {}
                _ => best = Some((token_len, row_idx, candidates)),
            }
        }
        if let Some((_, row, candidates)) = best {
            return (Some(row), "schedule-prefix", candidates);
        }
        // 6. Title containment (`Inventory analytics.xls` / `... - PBC`).
        let mut best: Option<(usize, usize, usize)> = None; // (diff, row, candidates)
        for (row_idx, d) in self.rows.iter().enumerate() {
            if !is_clean(&d.title) {
                continue;
            }
            let tk = norm_key(&d.title);
            if tk.len() < 8 || key_full.len() < 8 {
                continue;
            }
            if !(tk.contains(&key_full) || key_full.contains(&tk)) {
                continue;
            }
            let diff = tk.len().abs_diff(key_full.len());
            let candidates = self.by_title.get(&tk).map(|v| v.len()).unwrap_or(1);
            match best {
                Some((d0, _, _)) if d0 <= diff => {}
                _ => best = Some((diff, row_idx, candidates)),
            }
        }
        if let Some((_, row, candidates)) = best {
            return (Some(row), "title-contains", candidates);
        }
        (None, "none", 0)
    }
}

/// Filename without its final extension (`2024 … (1148441)01200RG0`).
fn file_stem(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => name[..i].to_string(),
        _ => name.to_string(),
    }
}

// ---------------------------------------------------------------------------
// The collected model
// ---------------------------------------------------------------------------

/// Options for one `/api/docman` collection.
#[derive(Clone, Debug)]
pub struct Options {
    /// Include kernel/machinery files in `rows` (they are always counted).
    pub internals: bool,
    /// Restrict rows to one `kind` (`cvw`, `word`, ...) or register class.
    pub class: Option<String>,
    /// Case-insensitive substring filter over file name / title / schedule.
    pub q: Option<String>,
    /// Compute SHA-256 for every file.
    pub sha: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            internals: false,
            class: None,
            q: None,
            sha: true,
        }
    }
}

/// One row of the Document Manager grid: a file, or a register-only document.
#[derive(Clone, Debug)]
pub struct Row {
    pub id: String,
    pub file: Option<String>,
    pub rel: Option<String>,
    pub dir: String,
    pub ext: String,
    pub kind: &'static str,
    pub format: &'static str,
    pub cw_class: &'static str,
    pub bytes: u64,
    pub mtime: i64,
    pub sha: Option<String>,
    pub mime: &'static str,
    pub preview: Option<&'static str>,
    /// Index into `ShIndex::book.documents()`.
    pub sh: Option<usize>,
    pub join_tier: &'static str,
    pub join_ambiguous: bool,
    pub join_candidates: usize,
}

/// Everything `/api/docman` reports.
pub struct Docman {
    pub path: String,
    pub stem: String,
    pub files: usize,
    pub internals: usize,
    pub documents: usize,
    pub bytes: u64,
    pub sh: Option<ShIndex>,
    pub rows: Vec<Row>,
    /// Register rows with no file on disk (listed with `backing:"register"`).
    pub register_only: usize,
    /// Files that matched no register row.
    pub unindexed: usize,
    /// Files whose join hit more than one candidate row.
    pub ambiguous: usize,
    pub tiers: BTreeMap<&'static str, usize>,
    pub options: Options,
}

impl Docman {
    pub fn joined(&self) -> usize {
        self.rows.iter().filter(|r| r.sh.is_some() && r.file.is_some()).count()
    }
}

/// Collect the union for `dir`, whose engagement stem is `stem`.
pub fn collect(dir: &Path, stem: &str, options: Options) -> Result<Docman, ApiError> {
    let files = walk(dir, options.sha)?;
    let sh = read_sh(dir, stem)?;
    let mut rows: Vec<Row> = Vec::new();
    let mut tiers: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut used_rows: BTreeMap<usize, Vec<usize>> = BTreeMap::new(); // sh row -> row indices

    if let Some(idx) = sh.as_ref() {
        let index = RegisterIndex::build(&idx.book);
        for f in &files {
            let (hit, tier, candidates) = index.join(&f.name, stem);
            *tiers.entry(tier).or_insert(0) += 1;
            let sh_doc = hit.map(|i| index.rows[i]);
            let cw_class = match sh_doc {
                Some(d) => register_class(d),
                None => file_kind_class(f.kind),
            };
            let row = Row {
                id: format!("file:{}", f.rel),
                file: Some(f.name.clone()),
                rel: Some(f.rel.clone()),
                dir: f.dir.clone(),
                ext: f.ext.clone(),
                kind: f.kind,
                format: f.format,
                cw_class,
                bytes: f.bytes,
                mtime: f.mtime,
                sha: f.sha.clone(),
                mime: f.mime,
                preview: f.preview,
                sh: hit,
                join_tier: tier,
                join_ambiguous: candidates > 1,
                join_candidates: candidates.max(if hit.is_some() { 1 } else { 0 }),
            };
            if let Some(h) = hit {
                used_rows.entry(h).or_default().push(rows.len());
            }
            rows.push(row);
        }
        // Register-only rows: SH documents with no file behind them.
        for (i, d) in index.rows.iter().enumerate() {
            if used_rows.contains_key(&i) {
                continue;
            }
            rows.push(Row {
                id: format!("sh:{}", if d.cwguid.is_empty() { format!("row{}", d.row) } else { d.cwguid.clone() }),
                file: None,
                rel: None,
                dir: String::new(),
                ext: String::new(),
                kind: "register",
                format: register_class(d),
                cw_class: register_class(d),
                bytes: 0,
                mtime: 0,
                sha: None,
                mime: "",
                preview: None,
                sh: Some(i),
                join_tier: "register",
                join_ambiguous: false,
                join_candidates: 0,
            });
        }
        // A row joined by several files is ambiguous from the register side
        // too: report the candidate count on every file that joined it.
        for (_hit, indices) in used_rows.iter() {
            if indices.len() > 1 {
                for i in indices {
                    rows[*i].join_ambiguous = true;
                    rows[*i].join_candidates = indices.len();
                }
            }
        }
    } else {
        for f in &files {
            rows.push(Row {
                id: format!("file:{}", f.rel),
                file: Some(f.name.clone()),
                rel: Some(f.rel.clone()),
                dir: f.dir.clone(),
                ext: f.ext.clone(),
                kind: f.kind,
                format: f.format,
                cw_class: file_kind_class(f.kind),
                bytes: f.bytes,
                mtime: f.mtime,
                sha: f.sha.clone(),
                mime: f.mime,
                preview: f.preview,
                sh: None,
                join_tier: "none",
                join_ambiguous: false,
                join_candidates: 0,
            });
        }
    }

    let internals = rows.iter().filter(|r| r.kind == "internal").count();
    // "Documents" is the CaseWare list: physical documents that are not
    // engagement machinery, plus register rows with no file behind them.
    let documents = rows
        .iter()
        .filter(|r| r.kind != "internal")
        .count();
    let total_bytes: u64 = rows.iter().map(|r| r.bytes).sum();
    let register_only = rows.iter().filter(|r| r.file.is_none()).count();
    let unindexed = rows
        .iter()
        .filter(|r| r.file.is_some() && r.sh.is_none() && r.kind != "internal")
        .count();
    let ambiguous = rows.iter().filter(|r| r.join_ambiguous).count();

    // Apply the view filters. The counts above are always the full union.
    let want_class = options.class.as_deref().map(|c| c.trim().to_ascii_lowercase());
    let q = options.q.as_deref().map(|q| q.trim().to_ascii_lowercase());
    let sh_ref = sh.as_ref();
    rows.retain(|r| {
        // The internals toggle hides unindexed kernel/machinery files only. A
        // file that the register itself names (a `TS.dbf` joined by `NAME`) is
        // a document in the list, so it stays.
        if r.kind == "internal" && !options.internals && r.sh.is_none() {
            return false;
        }
        if let Some(class) = want_class.as_deref().filter(|c| !c.is_empty() && *c != "all") {
            let matches = r.kind == class
                || r.cw_class.to_ascii_lowercase() == class
                || r.format.to_ascii_lowercase().contains(class);
            if !matches {
                return false;
            }
        }
        if let Some(q) = q.as_deref().filter(|q| !q.is_empty()) {
            let mut hay = String::new();
            if let Some(f) = &r.file {
                hay.push_str(f);
            }
            if let Some(rel) = &r.rel {
                hay.push_str(rel);
            }
            if let Some(i) = r.sh {
                if let Some(index) = sh_ref {
                    if let Some(d) = index.book.documents().get(i) {
                        hay.push_str(&d.title);
                        hay.push(' ');
                        hay.push_str(&d.schedule);
                        hay.push(' ');
                        hay.push_str(&d.cvid);
                        hay.push(' ');
                        hay.push_str(&d.cwguid);
                    }
                }
            }
            if !hay.to_ascii_lowercase().contains(q) {
                return false;
            }
        }
        true
    });

    Ok(Docman {
        path: dir.display().to_string(),
        stem: stem.to_string(),
        files: files.len(),
        internals,
        documents,
        bytes: total_bytes,
        sh,
        rows,
        register_only,
        unindexed,
        ambiguous,
        tiers,
        options,
    })
}

/// Grid class for a file that has no register row.
fn file_kind_class(kind: &str) -> &'static str {
    match kind {
        "cvw" => "CaseView document",
        "word" => "Word document",
        "excel" => "Excel workbook",
        "pdf" => "PDF document",
        "image" => "Image",
        "timevalue" => "TimeValue worksheet",
        "internal" => "Internal table",
        _ => "File",
    }
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

/// Render the `/api/docman` payload.
pub fn json(d: &Docman) -> String {
    let mut out = format!(
        "{{\"path\":{},\"stem\":{},\"options\":{{\"internals\":{},\"class\":{},\"q\":{},\"sha\":{}}},\"counts\":{{\"files\":{},\"documents\":{},\"internals\":{},\"rows\":{},\"register_only\":{},\"unindexed\":{},\"ambiguous\":{},\"joined\":{},\"bytes\":{}}}",
        json_str(&d.path),
        json_str(&d.stem),
        d.options.internals,
        match &d.options.class {
            Some(c) => json_str(c),
            None => "null".to_string(),
        },
        match &d.options.q {
            Some(q) => json_str(q),
            None => "null".to_string(),
        },
        d.options.sha,
        d.files,
        d.documents,
        d.internals,
        d.rows.len(),
        d.register_only,
        d.unindexed,
        d.ambiguous,
        d.joined(),
        d.bytes
    );
    // Register health.
    out.push_str(",\"sh\":");
    match &d.sh {
        None => out.push_str("null"),
        Some(sh) => out.push_str(&format!(
            "{{\"declared\":{},\"readable\":{},\"live\":{},\"deleted\":{},\"dropped\":{},\"repaired\":{},\"note\":{}}}",
            sh.declared,
            sh.readable,
            sh.live(),
            sh.deleted(),
            sh.dropped,
            sh.repaired,
            json_str(&sh.note)
        )),
    }
    // The join law, so the UI can explain the match instead of guessing.
    out.push_str(",\"join\":{\"tiers\":[");
    for (i, (name, law)) in JOIN_TIERS.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"tier\":{},\"law\":{},\"hits\":{}}}",
            json_str(name),
            json_str(law),
            d.tiers.get(name).copied().unwrap_or(0)
        ));
    }
    out.push_str("],\"counts\":{");
    for (i, (k, v)) in d.tiers.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!("{}:{}", json_str(k), v));
    }
    out.push_str("},\"misses\":[");
    let mut misses: Vec<String> = Vec::new();
    if d.unindexed > 0 {
        misses.push(format!(
            "{} file(s) have no SH row (unindexed: no CVID/SCHEDULE/NAME/title token)",
            d.unindexed
        ));
    }
    if d.ambiguous > 0 {
        misses.push(format!(
            "{} row(s) matched more than one register row or are shared by several files",
            d.ambiguous
        ));
    }
    if d.register_only > 0 {
        misses.push(format!(
            "{} SH row(s) have no file on disk (analyses, headings, index rows)",
            d.register_only
        ));
    }
    for (i, m) in misses.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&json_str(m));
    }
    out.push_str("]}");

    // Rows.
    out.push_str(",\"rows\":[");
    for (i, r) in d.rows.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"id\":");
        out.push_str(&json_str(&r.id));
        out.push_str(",\"backing\":");
        out.push_str(&json_str(if r.file.is_some() { "file" } else { "register" }));
        out.push_str(",\"file\":");
        match &r.file {
            Some(f) => out.push_str(&json_str(f)),
            None => out.push_str("null"),
        }
        out.push_str(",\"rel\":");
        match &r.rel {
            Some(f) => out.push_str(&json_str(f)),
            None => out.push_str("null"),
        }
        out.push_str(&format!(
            ",\"dir\":{},\"ext\":{},\"kind\":{},\"format\":{},\"cw_class\":{},\"bytes\":{},\"mtime\":{},\"mtime_iso\":{},\"mime\":{},\"preview\":",
            json_str(&r.dir),
            json_str(&r.ext),
            json_str(r.kind),
            json_str(r.format),
            json_str(r.cw_class),
            r.bytes,
            r.mtime,
            json_str(&iso_from_epoch(r.mtime)),
            json_str(r.mime)
        ));
        match r.preview {
            Some(p) => out.push_str(&json_str(p)),
            None => out.push_str("null"),
        }
        out.push_str(",\"sha256\":");
        match &r.sha {
            Some(s) => out.push_str(&json_str(s)),
            None => out.push_str("null"),
        }
        out.push_str(",\"open\":");
        match &r.rel {
            Some(rel) => out.push_str(&json_str(&format!(
                "/api/docman/file?path={}",
                pct_encode_path(rel)
            ))),
            None => out.push_str("null"),
        }
        out.push_str(",\"join\":{");
        out.push_str(&format!(
            "\"tier\":{},\"ambiguous\":{},\"candidates\":{}",
            json_str(r.join_tier),
            r.join_ambiguous,
            r.join_candidates
        ));
        out.push('}');
        out.push_str(",\"sh\":");
        match (r.sh, d.sh.as_ref()) {
            (Some(idx), Some(sh)) => push_sh(&mut out, sh, idx),
            _ => out.push_str("null"),
        }
        out.push('}');
    }
    out.push_str("]}");
    out
}

fn push_sh(out: &mut String, sh: &ShIndex, index: usize) {
    let Some(d) = sh.book.documents().get(index) else {
        out.push_str("null");
        return;
    };
    out.push_str(&format!(
        "{{\"row\":{},\"lineno\":{},\"prg\":{},\"schedule\":{},\"name\":{},\"type\":{},\"class\":{},\"title\":{},\"cvid\":{},\"guid\":{},\"created\":{},\"cw_date\":{},\"entity\":{},\"flags\":{{\"archive\":{},\"roll\":{},\"lockdown\":{},\"hidden\":{},\"delivered\":{},\"sign\":{}}},\"signoff\":",
        d.row,
        match d.lineno {
            Some(l) => l.to_string(),
            None => "null".to_string(),
        },
        json_str(&d.prg),
        json_str(&d.schedule),
        json_str(&d.name),
        json_str(&d.kind.letter().to_string()),
        json_str(register_class(d)),
        json_str(&d.title),
        json_str(&d.cvid),
        json_str(&d.cwguid),
        json_str(&cw_stamp(&d.created_on)),
        json_str(&cw_stamp(&d.cw_date)),
        json_str(&d.entity),
        d.archive,
        d.roll,
        d.lockdown,
        d.hidden,
        d.delivered,
        d.sign
    ));
    match sh.signoff_of(index) {
        Some((stage, signed, complete, next)) => out.push_str(&format!(
            "{{\"stage\":{},\"signed\":{},\"complete\":{},\"next_level\":{}}}",
            json_str(stage),
            signed,
            complete,
            match next {
                Some(l) => l.to_string(),
                None => "null".to_string(),
            }
        )),
        None => out.push_str("null"),
    }
    out.push('}');
}

/// `20240517205000` -> `2024-05-17 20:50` (anything else is returned as stored).
pub fn cw_stamp(raw: &str) -> String {
    let s = raw.trim();
    if s.len() != 14 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return s.to_string();
    }
    format!(
        "{}-{}-{} {}:{}",
        &s[0..4],
        &s[4..6],
        &s[6..8],
        &s[8..10],
        &s[10..12]
    )
}

/// Epoch seconds -> `YYYY-MM-DD HH:MM` (UTC). Local time needs a timezone
/// database the workspace does not carry, so the UI labels it UTC.
pub fn iso_from_epoch(secs: i64) -> String {
    if secs <= 0 {
        return String::new();
    }
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60
    )
}

/// Howard Hinnant's civil-from-days (days since 1970-01-01).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Percent-encode a relative path for `?path=` (`/` kept as a separator).
pub fn pct_encode_path(rel: &str) -> String {
    let mut out = String::with_capacity(rel.len());
    for b in rel.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// `.cvw` preview
// ---------------------------------------------------------------------------

/// Result of attempting to render one `.cvw` through the `cw_cv` pipeline.
pub struct CvwPreview {
    /// False when nothing truthful could be laid out (no sheet is fabricated).
    pub rendered: bool,
    /// Why it could not render, or the caveats when it did.
    pub issues: Vec<String>,
    pub form: Option<String>,
    pub kind: &'static str,
    pub blocks: usize,
    pub numbered: usize,
    pub unnumbered: usize,
    pub max_column: String,
    pub max_row: u32,
    pub cells: usize,
    pub text_bound: usize,
    pub substituted: usize,
    /// The rendered structural view, when renderable.
    pub sheet: Option<sheet::SheetDoc>,
    /// (cell name, bound text) for system/unnumbered cells.
    pub system_cells: Vec<(String, String)>,
    pub bytes: u64,
    pub sha: String,
}

/// Render `path` through `Cvw::parse` -> pool-bound `TextTable` -> `render` ->
/// `sheet`. Text comes only from the decoded `Form/Strings` pool; a document
/// with no pool renders its *structure* with empty cells and says so.
pub fn preview(path: &Path) -> Result<CvwPreview, ApiError> {
    let bytes = std::fs::read(path)
        .map_err(|e| ApiError::not_found(format!("{}: {e}", path.display())))?;
    let sha = sha256_hex(&bytes);
    let mut p = CvwPreview {
        rendered: false,
        issues: Vec::new(),
        form: None,
        kind: "unknown",
        blocks: 0,
        numbered: 0,
        unnumbered: 0,
        max_column: String::new(),
        max_row: 0,
        cells: 0,
        text_bound: 0,
        substituted: 0,
        sheet: None,
        system_cells: Vec::new(),
        bytes: bytes.len() as u64,
        sha,
    };
    let doc = match Cvw::parse(&bytes) {
        Ok(d) => d,
        Err(e) => {
            p.issues.push(format!("container: {e}"));
            return Ok(p);
        }
    };
    p.form = doc.form().map(str::to_string);
    p.cells = doc.cells().len();
    if !doc.layout_header.magic_ok {
        p.issues.push("Layout stream: header magic absent".to_string());
    }
    if doc.cells().is_empty() {
        p.issues.push(format!(
            "no cell directory ({} index stream{}); the document carries \
             {} stream(s) and nothing to lay out",
            if doc.cell_dir.header[0] == 0 { "missing" } else { "empty" },
            "",
            doc.streams.len()
        ));
        return Ok(p);
    }
    if !doc.cell_dir.complete {
        p.issues.push(format!(
            "cell directory walk stopped at {} of {} declared cells",
            doc.cells().len(),
            doc.cell_dir.declared_count()
        ));
    }

    // Text binding: each block's trailer slot addresses the Strings pool at
    // `slot + POOL_INDEX_BASE` (F7).
    let mut texts = TextTable::new();
    match doc.strings_pool() {
        Ok(pool) => {
            for (cell, block) in doc.cells().iter().zip(&doc.cell_blocks) {
                if let Some(slot) = block.pool_slot() {
                    let idx = slot as usize + layout::POOL_INDEX_BASE;
                    if let Some(text) = pool.text(idx) {
                        if !text.trim().is_empty() {
                            texts.insert(cell.name.clone(), text.to_string());
                        }
                    }
                }
            }
            p.text_bound = texts.len();
            if !pool.complete {
                p.issues.push(format!(
                    "Strings pool: {} of {} entries decoded",
                    pool.len(),
                    doc.strings.map(|h| h.count as usize).unwrap_or(pool.len())
                ));
            }
        }
        Err(e) => {
            p.issues.push(format!(
                "cell text unavailable: {e} (the form's Strings pool is absent or \
                 cut by the damaged-member frontier); structure is shown, no text is invented"
            ));
        }
    }
    let registry = WksRegistry::from_engagement_dir(
        path.parent().unwrap_or_else(|| Path::new(".")),
    )
    .ok();
    let rendered = render::render(&doc, &texts, &LinkTable::new(), registry.as_ref());
    p.substituted = rendered.substituted();
    for issue in rendered.issues().take(40) {
        p.issues.push(format!("cell {}: {}", issue.name, issue.issue.as_ref().map(|i| i.to_string()).unwrap_or_default()));
    }
    if rendered.issue_count() > 40 {
        p.issues.push(format!(
            "… and {} more unresolved cell(s)",
            rendered.issue_count() - 40
        ));
    }
    let model = sheet::sheet(&rendered);
    p.kind = model.kind.as_str();
    p.blocks = model.blocks.len();
    p.numbered = model.numbered;
    p.unnumbered = model.unnumbered;
    p.max_row = model.max_row;
    p.max_column = if model.max_column > 0 {
        cw_cv::CellNumber {
            column: model.max_column,
            row: 1,
        }
        .column_name()
    } else {
        String::new()
    };
    for cell in &rendered.cells {
        if cell.number.is_none() {
            p.system_cells.push((cell.name.clone(), cell.text.clone()));
        }
    }
    if model.numbered == 0 {
        p.issues.push(
            "no numbered cells: the directory has names but no leadsheet coordinates".to_string(),
        );
        return Ok(p);
    }
    p.rendered = true;
    p.sheet = Some(model);
    Ok(p)
}

/// JSON form of a [`CvwPreview`] (the `?format=json` lens).
pub fn preview_json(p: &CvwPreview, rel: &str) -> String {
    let mut out = format!(
        "{{\"file\":{},\"rendered\":{},\"form\":{},\"kind\":{},\"blocks\":{},\"numbered\":{},\"unnumbered\":{},\"cells\":{},\"text_bound\":{},\"substituted\":{},\"max_column\":{},\"max_row\":{},\"bytes\":{},\"sha256\":{},\"issues\":[",
        json_str(rel),
        p.rendered,
        match &p.form {
            Some(f) => json_str(f),
            None => "null".to_string(),
        },
        json_str(p.kind),
        p.blocks,
        p.numbered,
        p.unnumbered,
        p.cells,
        p.text_bound,
        p.substituted,
        json_str(&p.max_column),
        p.max_row,
        p.bytes,
        json_str(&p.sha)
    );
    for (i, issue) in p.issues.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&json_str(issue));
    }
    out.push_str("],\"blocks_detail\":[");
    if let Some(model) = &p.sheet {
        for (i, b) in model.blocks.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"group\":{},\"first_row\":{},\"last_row\":{},\"first_column\":{},\"last_column\":{},\"cells\":{},\"body\":{}}}",
                match &b.group {
                    Some(g) => json_str(g),
                    None => "null".to_string(),
                },
                b.first_row,
                b.last_row,
                b.first_column,
                b.last_column,
                b.len(),
                b.has_body()
            ));
        }
    }
    out.push_str("]}");
    out
}

/// Inline HTML for the preview route: a self-contained page (no external
/// asset, no script, no font host) that either lays out the sheet or states
/// exactly why it could not.
pub fn preview_html(p: &CvwPreview, rel: &str, raw_url: &str) -> String {
    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<meta name=\"color-scheme\" content=\"light dark\">\n");
    out.push_str(&format!("<title>{}</title>\n", html_escape(rel)));
    out.push_str(STYLE);
    out.push_str("</head><body>\n");
    out.push_str(&format!(
        "<header class=\"bar\"><div><h1>{}</h1><div class=\"sub\">{}</div></div>\
         <div class=\"acts\"><a class=\"btn\" href=\"{}\" download>Download raw .cvw</a></div></header>\n",
        html_escape(rel),
        html_escape(&format!(
            "{}{} · {} block(s) · {} numbered cell(s) · {} text bound · sha256 {}",
            if p.form.is_some() { "CaseView form " } else { "CaseView " },
            p.form.as_deref().unwrap_or("-"),
            p.blocks,
            p.numbered,
            p.text_bound,
            &p.sha[..p.sha.len().min(12)]
        )),
        html_escape(raw_url)
    ));
    out.push_str(&format!(
        "<section class=\"facts\">\
         <span><b>bytes</b> {}</span>\
         <span><b>kind</b> {}</span>\
         <span><b>blocks</b> {}</span>\
         <span><b>unnumbered</b> {}</span>\
         <span><b>extent</b> {}1..{}</span>\
         <span><b>substituted</b> {}</span></section>\n",
        p.bytes,
        html_escape(p.kind),
        p.blocks,
        p.unnumbered,
        if p.max_column.is_empty() { "-" } else { &p.max_column },
        p.max_row,
        p.substituted
    ));
    if !p.issues.is_empty() {
        out.push_str("<section class=\"issues\"><h2>Notes</h2><ul>");
        for issue in &p.issues {
            out.push_str(&format!("<li>{}</li>", html_escape(issue)));
        }
        out.push_str("</ul></section>\n");
    }
    match (&p.sheet, p.rendered) {
        (Some(model), true) => {
            for (bi, block) in model.blocks.iter().enumerate() {
                out.push_str(&format!(
                    "<section class=\"block\"><h2>block {} <span class=\"group\">{}</span> \
                     <span class=\"meta\">rows {}..{} · cols {}..{} · {} cell(s) · {}</span></h2>",
                    bi,
                    html_escape(block.group.as_deref().unwrap_or("(bare names)")),
                    block.first_row,
                    block.last_row,
                    column_name(block.first_column),
                    column_name(block.last_column),
                    block.len(),
                    if block.has_body() { "leadsheet body" } else { "header band" }
                ));
                if !block.duplicates.is_empty() {
                    out.push_str("<p class=\"dup\">duplicate coordinates: ");
                    let dup: Vec<String> = block
                        .duplicates
                        .iter()
                        .map(|(n, names)| {
                            format!("{} ({})", n, names.iter().map(|s| escape_name(s)).collect::<Vec<_>>().join(", "))
                        })
                        .collect();
                    out.push_str(&html_escape(&dup.join("; ")));
                    out.push_str("</p>\n");
                }
                out.push_str("<table class=\"sheet\"><thead><tr><th class=\"corner\"></th>");
                for c in &block.columns {
                    out.push_str(&format!("<th>{}</th>", column_name(*c)));
                }
                out.push_str("</tr></thead><tbody>");
                for row in &block.rows {
                    out.push_str(&format!("<tr><th class=\"rowhead\">{row}</th>"));
                    for c in &block.columns {
                        let number = cw_cv::CellNumber {
                            column: *c,
                            row: *row,
                        };
                        match block.cell(number) {
                            None => out.push_str("<td class=\"gap\"></td>"),
                            Some(cell) => {
                                let mut text = escape_text_visible(&cell.text);
                                if let Some(issue) = &cell.issue {
                                    if !text.is_empty() {
                                        text.push(' ');
                                    }
                                    text.push_str(&format!("!{issue}"));
                                }
                                let tag = match cell.kind {
                                    render::CellKind::Literal => "",
                                    render::CellKind::CellLink => " [cell]",
                                    render::CellKind::LeadsheetLink => " [feed]",
                                };
                                out.push_str(&format!(
                                    "<td data-cell=\"{}\" title=\"{}\">{}{}</td>",
                                    html_escape(&cell.name),
                                    html_escape(&cell.name),
                                    html_escape(&text),
                                    tag
                                ));
                            }
                        }
                    }
                    out.push_str("</tr>");
                }
                out.push_str("</tbody></table></section>\n");
            }
            if !p.system_cells.is_empty() {
                out.push_str("<details class=\"sys\"><summary>System cells (no leadsheet coordinate)</summary><table class=\"syscells\"><tbody>");
                for (name, text) in &p.system_cells {
                    out.push_str(&format!(
                        "<tr><th>{}</th><td>{}</td></tr>",
                        html_escape(&escape_name(name)),
                        html_escape(text)
                    ));
                }
                out.push_str("</tbody></table></details>\n");
            }
        }
        _ => {
            out.push_str(&format!(
                "<section class=\"failed\" data-render=\"failed\"><h2>Nothing laid out</h2>\
                 <p>The sheet is <b>not</b> fabricated: this document has no cell directory \
                 or its coordinates could not be read. The file itself is intact; \
                 <a href=\"{}\" download>download it</a> to open it in CaseView.</p></section>\n",
                html_escape(raw_url)
            ));
        }
    }
    out.push_str("</body></html>\n");
    out
}

fn column_name(n: u32) -> String {
    cw_cv::CellNumber { column: n, row: 1 }.column_name()
}

fn escape_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if (c as u32) < 0x20 || c == '\u{7f}' {
                format!("\\x{:02x}", c as u32)
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// Visible text: control bytes are shown escaped, not dropped, so a cell that
/// carries only formatting bytes is visibly not "empty text from a pool".
fn escape_text_visible(text: &str) -> String {
    escape_name(text)
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

const STYLE: &str = "<style>
:root{--ink:#16202a;--muted:#5d6b7a;--line:#d7dee6;--bg:#f6f8fa;--card:#fff;--accent:#0b6bcb}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--ink);font:13px/1.45 -apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif}
header.bar{display:flex;justify-content:space-between;align-items:flex-start;gap:16px;padding:14px 18px;background:var(--card);border-bottom:1px solid var(--line)}
h1{font-size:15px;margin:0 0 2px}.sub{color:var(--muted);font-size:12px}
.acts .btn{border:1px solid var(--line);border-radius:4px;padding:5px 10px;color:var(--accent);text-decoration:none;background:#fff;font-size:12px}
.facts{display:flex;flex-wrap:wrap;gap:14px;padding:8px 18px;background:#eef2f6;border-bottom:1px solid var(--line);font-size:12px;color:var(--muted)}
.facts b{color:var(--ink);font-weight:600;margin-right:4px}
section.block{margin:16px 18px;background:var(--card);border:1px solid var(--line);border-radius:5px;padding:10px 12px;overflow:auto}
section.block h2{font-size:12px;margin:0 0 8px;font-weight:600;color:var(--ink)}
section.block h2 .group{font-family:ui-monospace,Menlo,monospace;color:var(--accent);margin-left:6px}
section.block h2 .meta{font-weight:400;color:var(--muted);margin-left:6px}
table.sheet{border-collapse:collapse;font:12px/1.35 ui-monospace,Menlo,monospace}
table.sheet th,table.sheet td{border:1px solid var(--line);padding:2px 6px;white-space:pre;min-width:34px;text-align:left}
table.sheet thead th{background:#eef2f6;text-align:center;color:var(--muted);font-weight:600}
table.sheet td.gap{background:#fbfcfd}
table.sheet th.rowhead{background:#eef2f6;color:var(--muted);text-align:right}
th.corner{background:var(--card);border:none}
.issues{margin:12px 18px;background:#fff8e6;border:1px solid #ead9a8;border-radius:5px;padding:8px 12px;font-size:12px}
.issues h2{font-size:12px;margin:0 0 4px}.issues ul{margin:0;padding-left:18px}
.failed{margin:18px;background:#fdecea;border:1px solid #f2b8b5;border-radius:5px;padding:12px 14px}
.failed h2{margin:0 0 6px;font-size:13px}.failed a{color:var(--accent)}
.dup{color:#8a5a00;font-size:11px;margin:0 0 6px}
details.sys{margin:14px 18px;font-size:12px}details.sys summary{cursor:pointer;color:var(--muted)}
table.syscells{border-collapse:collapse;margin-top:6px}table.syscells th,table.syscells td{border:1px solid var(--line);padding:2px 6px;font:12px ui-monospace,Menlo,monospace;text-align:left}
</style>";

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_and_mime_tables_cover_the_corpus() {
        assert_eq!(file_class("cvw").0, "cvw");
        assert_eq!(file_class("CVW").0, "other", "extension is lower-cased by the caller");
        assert_eq!(file_class("docx").0, "word");
        assert_eq!(file_class("xlsx").0, "excel");
        assert_eq!(file_class("pdf").2, Some("pdf"));
        assert_eq!(file_class("dbf").0, "internal");
        assert_eq!(file_class("cdx").0, "internal");
        assert_eq!(file_class("fpt").0, "internal");
        assert_eq!(file_class("tv6").0, "timevalue");
        assert_eq!(file_class("tvr").0, "internal");
        assert_eq!(mime_for("pdf"), "application/pdf");
        assert_eq!(mime_for("png"), "image/png");
    }

    #[test]
    fn skip_law_matches_the_package_path() {
        assert!(is_skipped(".DS_Store"));
        assert!(is_skipped("._Invoice.docx"));
        assert!(is_skipped("SYNTH.ac_"));
        assert!(is_skipped("x.~ocw"));
        assert!(is_skipped("old.bak"));
        assert!(!is_skipped("Invoice.docx"));
        assert!(!is_skipped("SH.dbf"));
    }

    #[test]
    fn norm_key_folds_case_punctuation_and_accents() {
        assert_eq!(norm_key("0146 REV"), "0146rev");
        assert_eq!(norm_key("0146_Review"), "0146review");
        assert_eq!(norm_key("Cumulative Effect – 2024"), "cumulativeeffect2024");
        assert_eq!(norm_key("Café"), "cafe");
        assert_eq!(norm_key("0120 REV"), norm_key("0120-rev"));
    }

    #[test]
    fn iso_and_stamp_formatting() {
        assert_eq!(iso_from_epoch(0), "");
        assert_eq!(iso_from_epoch(1_700_000_000), "2023-11-14 22:13");
        assert_eq!(cw_stamp("20240517205000"), "2024-05-17 20:50");
        assert_eq!(cw_stamp("nonsense"), "nonsense");
        assert_eq!(cw_stamp(""), "");
    }

    #[test]
    fn pct_path_keeps_separators_and_encodes_spaces() {
        assert_eq!(pct_encode_path("sub/a b.docx"), "sub/a%20b.docx");
        assert_eq!(pct_encode_path("a&B.pdf"), "a%26B.pdf");
    }

    #[test]
    fn repair_replaces_a_missing_eof_and_truncates_a_short_tail() {
        let mut t = Table {
            raw_header: Vec::new(),
            version: 0x03,
            header_len: 0,
            record_len: 0,
            fields: Vec::new(),
            records: Vec::new(),
        };
        // One C(4) field, two live records.
        let mut header: Vec<u8> = vec![0x03, 0x7A, 0x0A, 0x16];
        header.extend_from_slice(&2u32.to_le_bytes());
        let header_len = 32 + 32 + 1;
        header.extend_from_slice(&(header_len as u16).to_le_bytes());
        header.extend_from_slice(&5u16.to_le_bytes());
        header.extend(std::iter::repeat(0u8).take(20));
        let mut d = [0u8; 32];
        d[..4].copy_from_slice(b"NAME");
        d[11] = b'C';
        d[16] = 4;
        header.extend_from_slice(&d);
        header.push(0x0D);
        t.raw_header = header;
        t.header_len = header_len as u16;
        t.record_len = 5;
        t.fields = vec![cw_core::dbf::Field {
            name: "NAME".into(),
            kind: b'C',
            len: 4,
            dec: 0,
        }];
        t.records = vec![b" abcd".to_vec(), b" efgh".to_vec()];
        let mut bytes = t.raw_header.clone();
        for r in &t.records {
            bytes.extend_from_slice(r);
        }
        // No 0x1A terminator at all (the expanded hull corpus shape).
        let (fixed, dropped) = repair_table_bytes(&bytes);
        assert_eq!(dropped, 0);
        assert_eq!(fixed.last(), Some(&0x1A));
        assert!(Table::parse(&fixed).is_ok());

        // Short tail: declared 2 records but only the first is present.
        let mut short = bytes[..bytes.len() - 5].to_vec();
        short[4..8].copy_from_slice(&2u32.to_le_bytes());
        let (fixed, dropped) = repair_table_bytes(&short);
        assert_eq!(dropped, 1);
        assert_eq!(Table::parse(&fixed).unwrap().count(), 1);
    }

    #[test]
    fn frontier_stops_at_the_first_garbage_flag() {
        let mut t = Table {
            raw_header: Vec::new(),
            version: 0x03,
            header_len: 0,
            record_len: 2,
            fields: Vec::new(),
            records: vec![
                vec![0x20, b'a'],
                vec![0x2A, b'b'],
                vec![0x00, b'c'],
                vec![0x20, b'd'],
            ],
        };
        assert_eq!(readable_frontier(&t), 2);
        t.records.pop();
        t.records.pop();
        assert_eq!(readable_frontier(&t), 2);
        let mut all = t.clone();
        all.records = vec![vec![0x20, b'a'], vec![0x2A, b'b']];
        assert_eq!(readable_frontier(&all), 2);
    }

    #[test]
    fn schedule_token_gate() {
        assert!(is_schedule_token("0146 REV"));
        assert!(is_schedule_token("0272"));
        assert!(!is_schedule_token("EXPORT"));
        assert!(!is_schedule_token("79B1993C-E48"));
        assert!(!is_schedule_token(""));
    }

    #[test]
    fn clean_rejects_control_bytes() {
        assert!(is_clean("Summary of Uncorrected Misstatements"));
        assert!(!is_clean("\u{0}\u{0}garbage"));
        assert!(!is_clean("qg\u{1}152"));
    }
}
