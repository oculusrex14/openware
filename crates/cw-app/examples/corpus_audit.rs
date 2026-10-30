//! Corpus audit: decode every `expanded/` engagement inside its readable
//! frontier and emit the per-corpus fact sheet + `CORPUS-FACTS.md`.
//!
//! Five engagements are on disk; three of them (corp2026, hull2024,
//! mwgh2025) are the frozen golden corpora, two are extra (Viewrail 2025 and
//! the small USI variant). Several `.dbf` members shipped with a **truncated
//! LZMA bitstream**, so the tail past the last authentic record is
//! zero-fill synthesis (see `tests/golden/README.md` and the `DAMAGED` table
//! in `crates/cw-core/tests/guid_law_gate.rs`). Every census below therefore
//! runs over the **readable prefix** only:
//!
//! * record flag is 0x20 (live) or 0x2A (tombstone), and
//! * for live rows both timestamp columns are blank or a real
//!   `YYYYMMDDhhmmss`, and
//! * `CWGUID`, when present, is blank, an 8-char legacy relic, or a
//!   structurally valid 22-char id.
//!
//! That is the same predicate the GUID law gate pins, so the frontier printed
//! here equals its `DAMAGED` constant for the three frozen corpora (the two
//! extra engagements are not pinned anywhere; their frontiers are reported
//! facts, not assertions).
//!
//! Decoding reuses the domain layer's own enums (`AccountKind`, `FxType`,
//! `EntryType`, `Scheme`, `NodeKind`, `UserDefinedKind`, `SignoffSheet`) so a
//! classification printed here is the classification the rest of the crate
//! would apply; the cells are read raw because a census must not be gated by
//! schema-drift refusal on a table it is only describing.
//!
//! Run: `cargo run -q -p cw-app --example corpus_audit`
//! Writes: `../CORPUS-FACTS.md` (workspace parent, next to `CONTEXT.md`).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use cw_app::accounts::AccountKind;
use cw_app::adjustments::EntryType;
use cw_app::fx::FxType;
use cw_app::groups::{NodeKind, Scheme};
use cw_app::signoff::{SignoffSheet, SIGNOFF_LEVELS};
use cw_app::tax::UserDefinedKind;
use cw_core::canon::latin1;
use cw_core::dbf::Table;
use cw_core::guid::CwGuid;
use cw_core::sha256::sha256_hex;

/// One engagement on disk.
struct Corpus {
    /// Stable short key used in the report.
    key: &'static str,
    /// Directory name under `expanded/`.
    dir: &'static str,
    /// Engagement identity (package stem).
    identity: &'static str,
    /// Fixture duty, if any.
    duty: &'static str,
}

const CORPORA: [Corpus; 5] = [
    Corpus {
        key: "corp2026",
        dir: "USI_Compilation_Training(1155962)",
        identity: "USI Compilation Training(1155962)",
        duty: "golden corpus (corp2026)",
    },
    Corpus {
        key: "hull2024",
        dir: "2024_Hull_Lift_Truck_Review(1148441)",
        identity: "2024 Hull Lift Truck Review(1148441)",
        duty: "golden corpus (hull2024)",
    },
    Corpus {
        key: "mwgh2025",
        dir: "MWGH_LLC_9-30-25_REVIEW(1239263)",
        identity: "MWGH LLC 9-30-25 REVIEW(1239263)",
        duty: "golden corpus (mwgh2025)",
    },
    Corpus {
        key: "viewrail2025",
        dir: "2025_Viewrail_Holdings_Inc_and_Subsidi(1243194)",
        identity: "2025 Viewrail Holdings Inc and Subsidi(1243194)",
        duty: "extra engagement (no fixture duty)",
    },
    Corpus {
        key: "usi-small",
        dir: "USI_Compilation_Training(1155962)(1)",
        identity: "USI Compilation Training(1155962)(1)",
        duty: "small USI variant (no fixture duty)",
    },
];

/// Readable-prefix frontier pinned by `crates/cw-core/tests/guid_law_gate.rs`
/// (`DAMAGED`), for the three frozen corpora. Reproduced here so this audit
/// proves it agrees with the law gate rather than merely trusting its own
/// predicate. Tables not listed are readable end to end.
const DAMAGED: [(&str, &str, usize); 12] = [
    ("corp2026", "HI", 1510),
    ("corp2026", "MP", 786),
    ("corp2026", "TP", 2299),
    ("hull2024", "BL", 1918),
    ("hull2024", "HI", 1346),
    ("hull2024", "MP", 785),
    ("hull2024", "SH", 603),
    ("hull2024", "TP", 2184),
    ("mwgh2025", "BL", 1894),
    ("mwgh2025", "HI", 1405),
    ("mwgh2025", "MP", 787),
    ("mwgh2025", "TP", 2305),
];

/// The 22 table keys every CW engagement carries.
const TABLES: [&str; 22] = [
    "AC", "AM", "AN", "BL", "CE", "CV", "EC", "FP", "GL", "GR", "GS", "HI", "MP", "PR", "RA",
    "RL", "SH", "TM", "TP", "TS", "WR", "WT",
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Case-insensitive `<stem><KEY>.dbf` lookup (same rule as `freeze_roundtrip`).
fn find_table(dir: &Path, key: &str) -> Option<PathBuf> {
    let want = format!("{}.DBF", key.to_uppercase());
    let mut hit = None;
    for e in std::fs::read_dir(dir).ok()? {
        let p = e.ok()?.path();
        if !p.is_file() {
            continue;
        }
        let name = p.file_name()?.to_string_lossy().to_uppercase();
        if name.ends_with(&want) {
            hit = Some(p);
        }
    }
    hit
}

/// Parse a table, canonicalizing the one-byte CW quirk first.
///
/// Every one of the five engagements has members whose final byte is not
/// `0x1A` while the length law `header + count*rec + 1` still holds exactly
/// (the quirk documented at the top of `examples/freeze_roundtrip.rs`). The
/// strict reader requires the canonical byte, so it is repaired in memory
/// here — the audit reads the shipped bytes and never writes them back.
fn load_table(path: &Path) -> Result<Table, String> {
    let mut bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() >= 12 {
        let count = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let hlen = u16::from_le_bytes(bytes[8..10].try_into().unwrap()) as usize;
        let rlen = u16::from_le_bytes(bytes[10..12].try_into().unwrap()) as usize;
        let expected = hlen + count * rlen + 1;
        if bytes.len() == expected && bytes[expected - 1] != 0x1A {
            bytes[expected - 1] = 0x1A;
        }
    }
    Table::parse(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// `true` when `cell` is an empty/space cell; NUL padding counts as empty.
fn blank(cell: &str) -> bool {
    cell.trim_matches(|c: char| c == ' ' || c == '\u{0}').is_empty()
}

/// Raw cell, decoded Latin-1 and stripped of trailing padding only.
fn cell(t: &Table, r: usize, name: &str) -> String {
    match t.cell(r, name) {
        Some(raw) => latin1(raw).trim_end_matches([' ', '\u{0}']).to_string(),
        None => String::new(),
    }
}

/// Trimmed cell (the form used for key/axis comparisons).
fn key_cell(t: &Table, r: usize, name: &str) -> String {
    cell(t, r, name).trim().to_string()
}

/// Numeric cell; blank, `.` and unparsable cells read as 0.
fn num(t: &Table, r: usize, name: &str) -> f64 {
    let s = key_cell(t, r, name);
    if s.is_empty() || s == "." {
        0.0
    } else {
        s.parse::<f64>().unwrap_or(0.0)
    }
}

fn timestamp_ok(cell: &str) -> bool {
    let c = cell.trim();
    if c.is_empty() {
        return true;
    }
    if c.len() != 14 || !c.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let n = |a: usize, b: usize| c[a..b].parse::<u32>().unwrap();
    let (y, mo, d, h, mi, se) = (n(0, 4), n(4, 6), n(6, 8), n(8, 10), n(10, 12), n(12, 14));
    (1990..=2100).contains(&y)
        && (1..=12).contains(&mo)
        && (1..=31).contains(&d)
        && h < 24
        && mi < 60
        && se < 60
}

/// The readable-prefix predicate of `guid_law_gate.rs` (frontier law).
fn readable(t: &Table, r: usize) -> bool {
    if !matches!(t.records[r][0], 0x20 | 0x2A) {
        return false;
    }
    if t.is_deleted(r) {
        return true;
    }
    for col in ["CWDATE", "CREATEDON"] {
        if t.field_index(col).is_some() {
            let v = latin1(t.cell(r, col).unwrap());
            if !timestamp_ok(&v) {
                return false;
            }
        }
    }
    if t.field_index("CWGUID").is_some() {
        let g = latin1(t.cell(r, "CWGUID").unwrap());
        let g = g.trim();
        if !g.is_empty() && g.len() != 8 && CwGuid::parse(g).is_err() {
            return false;
        }
    }
    true
}

/// First row index of the fabricated tail (`t.count()` when clean end-to-end).
fn frontier(t: &Table) -> usize {
    (0..t.count()).find(|&r| !readable(t, r)).unwrap_or(t.count())
}

/// Live physical row indices `[0, frontier)`.
fn live_rows(t: &Table, front: usize) -> Vec<usize> {
    (0..front).filter(|&r| !t.is_deleted(r)).collect()
}

/// Distribution renderer: `k=v ...` by descending count, `<=cap` entries.
fn dist(m: &BTreeMap<String, usize>, blank_label: &str, cap: usize) -> String {
    let mut v: Vec<(&String, &usize)> = m.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let mut out = String::new();
    for (i, (k, n)) in v.iter().enumerate() {
        if i == cap {
            let _ = write!(out, " +{} more", v.len() - cap);
            break;
        }
        if i > 0 {
            out.push(' ');
        }
        let label = if k.is_empty() { blank_label } else { k.as_str() };
        let _ = write!(out, "{label}={n}");
    }
    if out.is_empty() {
        out.push_str("(none)");
    }
    out
}

/// Per-table physical census.
#[derive(Clone, Debug)]
struct TableFacts {
    present: bool,
    records: usize,
    frontier: usize,
    live: usize,
    tombs: usize,
    /// Per-row content keys over the readable prefix (audit stamp / id / memo
    /// pointer / checksum columns blanked), used for the cross-engagement copy
    /// relation.
    row_keys: Vec<u64>,
    /// Digest of `row_keys`.
    prefix_digest: String,
}

/// Columns that record *when* and *by which copy* a row was written, plus the
/// memo pointers: blanked before the cross-engagement content digest so a
/// re-opened copy still compares equal.
const AUDIT_COLUMNS: [&str; 11] = [
    "CWDATE",
    "CREATEDON",
    "CWGUID",
    "CWCHECKSUM",
    "CONSCHKSUM",
    "EXTCHKSUM",
    "EXINFO",
    "MEMO",
    "UID_NOTE",
    "FORMULA",
    "TAGS",
];

/// Content key of one row: SHA-256 (first 8 bytes) of the record with the
/// audit stamp / id / memo-pointer / checksum columns blanked, so a re-opened
/// copy compares equal to its source.
fn row_key(t: &Table, r: usize, offsets: &[usize], blanked: &[bool]) -> u64 {
    let mut rec = t.records[r].clone();
    for (i, blank) in blanked.iter().enumerate() {
        if *blank {
            let off = offsets[i];
            let len = t.fields[i].len as usize;
            rec[off..off + len].iter_mut().for_each(|b| *b = b' ');
        }
    }
    let digest = sha256_hex(&rec);
    u64::from_str_radix(&digest[..16], 16).unwrap()
}

fn row_keys(t: &Table, front: usize) -> Vec<u64> {
    let offsets = t.offsets();
    let blanked: Vec<bool> = t
        .fields
        .iter()
        .map(|f| AUDIT_COLUMNS.iter().any(|c| c.eq_ignore_ascii_case(&f.name)))
        .collect();
    (0..front).map(|r| row_key(t, r, &offsets, &blanked)).collect()
}

/// Prefix digest over the row keys (the "same content?" summary per table).
fn keys_digest(keys: &[u64]) -> String {
    let mut buf = Vec::with_capacity(keys.len() * 8);
    for k in keys {
        buf.extend_from_slice(&k.to_le_bytes());
    }
    sha256_hex(&buf)
}

impl TableFacts {
    fn of(t: &Table) -> TableFacts {
        let front = frontier(t);
        let keys = row_keys(t, front);
        TableFacts {
            present: true,
            records: t.count(),
            frontier: front,
            live: live_rows(t, front).len(),
            tombs: (0..front).filter(|&r| t.is_deleted(r)).count(),
            prefix_digest: keys_digest(&keys),
            row_keys: keys,
        }
    }

    fn missing() -> TableFacts {
        TableFacts {
            present: false,
            records: 0,
            frontier: 0,
            live: 0,
            tombs: 0,
            row_keys: Vec::new(),
            prefix_digest: String::new(),
        }
    }

    fn damaged(&self) -> usize {
        self.records - self.frontier
    }
}

#[derive(Clone, Debug, Default)]
struct AmFacts {
    rows: usize,
    entity: BTreeMap<String, usize>,
    kind: BTreeMap<String, usize>,
    div_nonblank: usize,
    sub_nonblank: usize,
    unit_nonblank: usize,
    fxtype: BTreeMap<String, usize>,
    rate_nonzero: usize,
    fx_cytd_nonzero: usize,
    fx_pytd_nonzero: usize,
    tac_populated: usize,
    gifi_populated: usize,
    posting: usize,
}

#[derive(Clone, Debug, Default)]
struct BlFacts {
    rows: usize,
    numbers: usize,
    year: BTreeMap<String, usize>,
    period: BTreeMap<String, usize>,
    bucket: BTreeMap<String, usize>,
    id: BTreeMap<String, usize>,
    entity: BTreeMap<String, usize>,
    group: BTreeMap<String, usize>,
    kind: BTreeMap<String, usize>,
    max_period: i64,
    solo_x: usize,
    solo_x_open: usize,
    solo_x_year: usize,
    fx_nonzero: usize,
    /// Non-zero `FX` rows carrying `ID='X'` with `FX == AMT == FX_YTD`.
    fx_x_overlay_only: usize,
}

#[derive(Clone, Debug, Default)]
struct GlFacts {
    rows: usize,
    status: BTreeMap<String, usize>,
    status_label: BTreeMap<String, usize>,
    jour: BTreeMap<String, usize>,
    per_blank: usize,
    /// Live rows whose `PER` is the string `1`.
    per_one: usize,
}

#[derive(Clone, Debug, Default)]
struct CeFacts {
    rows: usize,
    kind: BTreeMap<String, usize>,
    entity: BTreeMap<String, usize>,
    div_nonblank: usize,
    abbr_nonblank: usize,
    per_populated: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, Default)]
struct TpFacts {
    rows: usize,
    form: BTreeMap<String, usize>,
    entity: BTreeMap<String, usize>,
    year: BTreeMap<String, usize>,
    kind: BTreeMap<String, usize>,
    udef: BTreeMap<String, usize>,
    /// `(UDEF, FORM)` cross-tab, the shipped-vs-user code split.
    udef_form: BTreeMap<(String, String), usize>,
    form_entity_year: usize,
    blank_year: usize,
    /// Distinct `PER` cell values over live rows.
    per: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, Default)]
struct ShFacts {
    rows: usize,
    doc_type: BTreeMap<String, usize>,
    /// Rows carrying exactly `n` signed levels (A/AD), index 0..=8.
    level_hist: [usize; 9],
    /// Rows whose deepest signed level is `n`, index 0..=8.
    max_level_hist: [usize; 9],
    /// Signed-row count per level, index 0 = level 1.
    per_level: [usize; 8],
    s_present: usize,
    sd_present: usize,
    ac_present: usize,
    au_present: usize,
    /// Rows with `S1..S8` populated, per level (index 0 = level 1).
    s_levels: [usize; 8],
    /// `S{n}` cells whose text equals `AU{n}` at the same level.
    s_eq_au_same: usize,
    /// `S{n}` cells whose text equals `AU{n+1}` (one level later).
    s_eq_au_next: usize,
    /// `S{n}` cells matching no `AU` cell on the same row at all.
    s_au_none: usize,
    /// `S{n}` cells with any value.
    s_cells: usize,
    /// `AC{n}` cell-text length histogram over populated cells (22 = the
    /// GUID-width companion slot).
    ac_len: BTreeMap<String, usize>,
    /// `AU{n}` cell-text length histogram over populated cells (7 = the
    /// `e097283` user id).
    au_len: BTreeMap<String, usize>,
    sign_yes: usize,
    lockdown_yes: usize,
    invalid_rows: usize,
}

#[derive(Clone, Debug, Default)]
struct MpFacts {
    rows: usize,
    schemes: BTreeMap<String, usize>,
    kinds: BTreeMap<String, usize>,
    unassigned: usize,
    titles: BTreeMap<String, usize>,
    map_types: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, Default)]
struct FileFacts {
    total: usize,
    cvw: usize,
    tv: usize,
    docx: usize,
    xlsx: usize,
    xls: usize,
    pdf: usize,
    dbf: usize,
}

#[derive(Clone, Debug)]
struct Report {
    key: &'static str,
    identity: &'static str,
    duty: &'static str,
    dir: String,
    tables: BTreeMap<String, TableFacts>,
    am: Option<AmFacts>,
    bl: Option<BlFacts>,
    gl: Option<GlFacts>,
    ce: Option<CeFacts>,
    tp: Option<TpFacts>,
    sh: Option<ShFacts>,
    mp: Option<MpFacts>,
    files: FileFacts,
}

/// Printable-ASCII cell with trailing padding removed (signoff initials are
/// 8 chars right-justified; a fabricated tail row carries binary instead).
fn initials_ok(s: &str) -> bool {
    s.chars().all(|c| (' '..='~').contains(&c))
}

fn date_ok(s: &str) -> bool {
    let s = s.trim();
    s.is_empty() || (s.len() == 8 && s.bytes().all(|b| b.is_ascii_digit()))
}

fn audit_am(t: &Table, front: usize) -> AmFacts {
    let rows = live_rows(t, front);
    let mut f = AmFacts {
        rows: rows.len(),
        ..AmFacts::default()
    };
    for &r in &rows {
        *f.entity.entry(key_cell(t, r, "ENTITY")).or_insert(0) += 1;
        let kind_raw = key_cell(t, r, "TYPE");
        let kind_label = match AccountKind::parse(&kind_raw) {
            Ok(k) => k.label().to_string(),
            Err(_) => format!("?{kind_raw}"),
        };
        if AccountKind::parse(&kind_raw).map(|k| k.is_posting()).unwrap_or(false) {
            f.posting += 1;
        }
        *f.kind.entry(format!("{} ({kind_label})", if kind_raw.is_empty() { "<blank>" } else { &kind_raw })).or_insert(0) += 1;
        if !blank(&cell(t, r, "DIV")) {
            f.div_nonblank += 1;
        }
        if !blank(&cell(t, r, "SUB")) {
            f.sub_nonblank += 1;
        }
        if !blank(&cell(t, r, "UNIT")) {
            f.unit_nonblank += 1;
        }
        let fx_raw = key_cell(t, r, "FXTYPE");
        *f.fxtype.entry(FxType::parse(&fx_raw).label().to_string()).or_insert(0) += 1;
        if num(t, r, "C_RATE") != 0.0 {
            f.rate_nonzero += 1;
        }
        if num(t, r, "FXCYTD") != 0.0 {
            f.fx_cytd_nonzero += 1;
        }
        if num(t, r, "FXPYTD") != 0.0 {
            f.fx_pytd_nonzero += 1;
        }
        if !key_cell(t, r, "TAC_NO").is_empty() {
            f.tac_populated += 1;
        }
        if !key_cell(t, r, "GIFI").is_empty() {
            f.gifi_populated += 1;
        }
    }
    f
}

fn audit_bl(t: &Table, front: usize) -> BlFacts {
    let rows = live_rows(t, front);
    let mut f = BlFacts {
        rows: rows.len(),
        ..BlFacts::default()
    };
    let mut numbers: BTreeSet<String> = BTreeSet::new();
    // group key -> ID set (the solo-X marker law: a group whose only overlay
    // is `X` has no official sibling and is a CW-derived marker).
    let mut groups: BTreeMap<(String, String, String, String), BTreeSet<String>> = BTreeMap::new();
    for &r in &rows {
        let number = key_cell(t, r, "NUMBER");
        let year = key_cell(t, r, "YEAR");
        let period = key_cell(t, r, "PERIOD");
        let bucket = key_cell(t, r, "BUCKET");
        let id = key_cell(t, r, "ID");
        let group = key_cell(t, r, "GROUP");
        numbers.insert(number.clone());
        *f.year.entry(year.clone()).or_insert(0) += 1;
        *f.period.entry(period.clone()).or_insert(0) += 1;
        *f.bucket.entry(bucket.clone()).or_insert(0) += 1;
        *f.id.entry(id.clone()).or_insert(0) += 1;
        *f.entity.entry(key_cell(t, r, "ENTITY")).or_insert(0) += 1;
        *f.group.entry(group.clone()).or_insert(0) += 1;
        *f.kind.entry(key_cell(t, r, "TYPE")).or_insert(0) += 1;
        f.max_period = f.max_period.max(period.parse::<i64>().unwrap_or(0));
        let fx = num(t, r, "FX");
        if fx != 0.0 {
            f.fx_nonzero += 1;
            if id == "X"
                && (fx - num(t, r, "AMT")).abs() < 1e-2
                && (fx - num(t, r, "FX_YTD")).abs() < 1e-2
            {
                f.fx_x_overlay_only += 1;
            }
        }
        groups
            .entry((number, year, bucket, group))
            .or_default()
            .insert(id);
    }
    f.numbers = numbers.len();
    for ((_, _, bucket, _), ids) in groups {
        if ids.len() == 1 && ids.contains("X") {
            f.solo_x += 1;
            match bucket.as_str() {
                "O" => f.solo_x_open += 1,
                "Y" => f.solo_x_year += 1,
                _ => {}
            }
        }
    }
    f
}

fn audit_gl(t: &Table, front: usize) -> GlFacts {
    let rows = live_rows(t, front);
    let mut f = GlFacts {
        rows: rows.len(),
        ..GlFacts::default()
    };
    for &r in &rows {
        let status = key_cell(t, r, "STATUS");
        *f.status.entry(if status.is_empty() { "<blank>".into() } else { status.clone() }).or_insert(0) += 1;
        let label = match EntryType::from_status(&status) {
            Ok(e) => e.label().to_string(),
            Err(_) => format!("unmapped {status:?}"),
        };
        *f.status_label.entry(label).or_insert(0) += 1;
        let j = key_cell(t, r, "JOURSOURCE");
        *f.jour.entry(if j.is_empty() { "<blank>".into() } else { j }).or_insert(0) += 1;
        match key_cell(t, r, "PER").as_str() {
            "" => f.per_blank += 1,
            "1" => f.per_one += 1,
            _ => {}
        }
    }
    f
}

fn audit_ce(t: &Table, front: usize) -> CeFacts {
    let rows = live_rows(t, front);
    let mut f = CeFacts {
        rows: rows.len(),
        ..CeFacts::default()
    };
    for &r in &rows {
        *f.kind.entry(key_cell(t, r, "TYPE")).or_insert(0) += 1;
        *f.entity.entry(key_cell(t, r, "ENTITY")).or_insert(0) += 1;
        if !blank(&cell(t, r, "DIV")) {
            f.div_nonblank += 1;
        }
        if !blank(&cell(t, r, "ABBR")) {
            f.abbr_nonblank += 1;
        }
        for col in ["PER", "PER_P1", "PER_P2", "PER_P3", "PER_P4", "PER_BUD", "PER_FCAST"] {
            if t.field_index(col).is_some() && !key_cell(t, r, col).is_empty() {
                *f.per_populated.entry(col.to_string()).or_insert(0) += 1;
            }
        }
    }
    f
}

fn audit_tp(t: &Table, front: usize) -> TpFacts {
    let rows = live_rows(t, front);
    let mut f = TpFacts {
        rows: rows.len(),
        ..TpFacts::default()
    };
    let mut scopes: BTreeSet<(String, String, String)> = BTreeSet::new();
    for &r in &rows {
        let form = key_cell(t, r, "FORM");
        let entity = key_cell(t, r, "ENTITY");
        let year = key_cell(t, r, "YEAR");
        *f.form.entry(form.clone()).or_insert(0) += 1;
        *f.entity.entry(entity.clone()).or_insert(0) += 1;
        *f.year.entry(year.clone()).or_insert(0) += 1;
        *f.kind.entry(key_cell(t, r, "TYPE")).or_insert(0) += 1;
        let udef = UserDefinedKind::parse(&key_cell(t, r, "UDEF"));
        *f.udef.entry(udef.label().to_string()).or_insert(0) += 1;
        if udef != UserDefinedKind::Standard {
            *f.udef_form
                .entry((
                    udef.label().to_string(),
                    if form.is_empty() { "<blank>".into() } else { form.clone() },
                ))
                .or_insert(0) += 1;
        }
        if year.is_empty() {
            f.blank_year += 1;
        }
        let per = key_cell(t, r, "PER");
        *f.per
            .entry(if per.is_empty() { "<blank>".into() } else { per })
            .or_insert(0) += 1;
        scopes.insert((form, entity, year));
    }
    f.form_entity_year = scopes.len();
    f
}

fn audit_sh(t: &Table, front: usize) -> ShFacts {
    let rows = live_rows(t, front);
    let mut f = ShFacts {
        rows: rows.len(),
        ..ShFacts::default()
    };
    // `SignoffSheet` is the crate's own decode of one SH row's ladder.
    for &r in &rows {
        *f.doc_type
            .entry(if key_cell(t, r, "TYPE").is_empty() { "<blank>".into() } else { key_cell(t, r, "TYPE") })
            .or_insert(0) += 1;
        if key_cell(t, r, "SIGN").eq_ignore_ascii_case("Y") {
            f.sign_yes += 1;
        }
        if key_cell(t, r, "LOCKDOWN").eq_ignore_ascii_case("Y") {
            f.lockdown_yes += 1;
        }
        let mut invalid = false;
        for lvl in 1..=SIGNOFF_LEVELS {
            let a = cell(t, r, &format!("A{lvl}"));
            let ad = cell(t, r, &format!("AD{lvl}"));
            let s = cell(t, r, &format!("S{lvl}"));
            let sd = cell(t, r, &format!("SD{lvl}"));
            let ac = cell(t, r, &format!("AC{lvl}"));
            let au = cell(t, r, &format!("AU{lvl}"));
            if !initials_ok(&a) || !initials_ok(&s) || !initials_ok(&ac) || !initials_ok(&au) {
                invalid = true;
            }
            if !date_ok(&ad) || !date_ok(&sd) {
                invalid = true;
            }
        }
        if invalid {
            f.invalid_rows += 1;
            continue;
        }
        let sheet = match SignoffSheet::from_sh_row(t, r) {
            Ok(s) => s,
            Err(_) => {
                f.invalid_rows += 1;
                continue;
            }
        };
        let signed = sheet.signed_levels();
        f.level_hist[signed.len()] += 1;
        let deepest = signed.last().copied().unwrap_or(0);
        f.max_level_hist[deepest as usize] += 1;
        for l in signed {
            f.per_level[(l - 1) as usize] += 1;
        }
        let any = |p: &str| {
            (1..=SIGNOFF_LEVELS).any(|i| !cell(t, r, &format!("{p}{i}")).is_empty())
        };
        f.s_present += any("S") as usize;
        f.sd_present += any("SD") as usize;
        f.ac_present += any("AC") as usize;
        f.au_present += any("AU") as usize;
        for lvl in 1..=SIGNOFF_LEVELS {
            for (prefix, m) in [("AC", &mut f.ac_len), ("AU", &mut f.au_len)] {
                let v = cell(t, r, &format!("{prefix}{lvl}")).trim().to_string();
                if !v.is_empty() {
                    *m.entry(v.chars().count().to_string()).or_insert(0) += 1;
                }
            }
        }
        // `S1..S8` is a separate column block; its exact relation to the
        // `A/AD/AC/AU` ladder is unproven, so the census records the raw
        // coincidence pattern against `AU` rather than asserting one.
        let au: Vec<String> = (1..=SIGNOFF_LEVELS)
            .map(|l| cell(t, r, &format!("AU{l}")).trim().to_ascii_lowercase())
            .collect();
        for lvl in 1..=SIGNOFF_LEVELS {
            let s_cell = cell(t, r, &format!("S{lvl}")).trim().to_ascii_lowercase();
            if s_cell.is_empty() {
                continue;
            }
            f.s_levels[(lvl - 1) as usize] += 1;
            f.s_cells += 1;
            if au[(lvl - 1) as usize] == s_cell {
                f.s_eq_au_same += 1;
            }
            if lvl < SIGNOFF_LEVELS && au[lvl as usize] == s_cell {
                f.s_eq_au_next += 1;
            }
            if !au.iter().any(|a| !a.is_empty() && *a == s_cell) {
                f.s_au_none += 1;
            }
        }
    }
    f
}

fn audit_mp(t: &Table, front: usize) -> MpFacts {
    let rows = live_rows(t, front);
    let mut f = MpFacts {
        rows: rows.len(),
        ..MpFacts::default()
    };
    for &r in &rows {
        match Scheme::parse(&key_cell(t, r, "ID")) {
            Ok(Scheme::Unassigned) => f.unassigned += 1,
            Ok(s) => *f.schemes.entry(s.as_str()).or_insert(0) += 1,
            Err(_) => *f.schemes.entry("?".into()).or_insert(0) += 1,
        }
        let title = key_cell(t, r, "TITLE");
        let mtype = key_cell(t, r, "MAPTYPE");
        *f.kinds
            .entry(NodeKind::parse(&title, &mtype).label().to_string())
            .or_insert(0) += 1;
        *f.titles
            .entry(if title.is_empty() { "<blank>".into() } else { title })
            .or_insert(0) += 1;
        *f.map_types
            .entry(if mtype.is_empty() { "<blank>".into() } else { mtype })
            .or_insert(0) += 1;
    }
    f
}

fn file_facts(dir: &Path) -> FileFacts {
    let mut f = FileFacts::default();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return f;
    };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        f.total += 1;
        let name = p.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
        let ext = name.rsplit('.').next().unwrap_or("").to_string();
        if name.ends_with(".dbf") {
            f.dbf += 1;
        }
        if ext == "cvw" {
            f.cvw += 1;
        }
        if ext.starts_with("tv") {
            f.tv += 1;
        }
        match ext.as_str() {
            "docx" => f.docx += 1,
            "xlsx" => f.xlsx += 1,
            "xls" => f.xls += 1,
            "pdf" => f.pdf += 1,
            _ => {}
        }
    }
    f
}

fn audit(c: &Corpus) -> Report {
    let dir = root().join("expanded").join(c.dir);
    let mut tables = BTreeMap::new();
    let mut am = None;
    let mut bl = None;
    let mut gl = None;
    let mut ce = None;
    let mut tp = None;
    let mut sh = None;
    let mut mp = None;

    for key in TABLES {
        let Some(path) = find_table(&dir, key) else {
            tables.insert(key.to_string(), TableFacts::missing());
            continue;
        };
        let t = load_table(&path).unwrap_or_else(|e| panic!("{}: {e}", c.key));
        let facts = TableFacts::of(&t);
        let front = facts.frontier;
        match key {
            "AM" => am = Some(audit_am(&t, front)),
            "BL" => bl = Some(audit_bl(&t, front)),
            "GL" => gl = Some(audit_gl(&t, front)),
            "CE" => ce = Some(audit_ce(&t, front)),
            "TP" => tp = Some(audit_tp(&t, front)),
            "SH" => sh = Some(audit_sh(&t, front)),
            "MP" => mp = Some(audit_mp(&t, front)),
            _ => {}
        }
        tables.insert(key.to_string(), facts);
    }

    Report {
        key: c.key,
        identity: c.identity,
        duty: c.duty,
        dir: format!("expanded/{}", c.dir),
        tables,
        am,
        bl,
        gl,
        ce,
        tp,
        sh,
        mp,
        files: file_facts(&dir),
    }
}

fn render_console(r: &Report) -> String {
    let mut o = String::new();
    let _ = writeln!(
        o,
        "\n=== {}  {}\n    dir {}  [{}]",
        r.key, r.identity, r.dir, r.duty
    );
    if let Some(a) = &r.am {
        let _ = writeln!(
            o,
            "  AM  live={:4} | ENTITY {} | TYPE {} | posting={}",
            a.rows,
            dist(&a.entity, "<blank>", 6),
            dist(&a.kind, "<blank>", 6),
            a.posting
        );
        let _ = writeln!(
            o,
            "      segments DIV/SUB/UNIT nonblank={}/{}/{} | FXTYPE {} | nonzero C_RATE/FXCYTD/FXPYTD={}/{}/{} | TAC_NO={} GIFI={}",
            a.div_nonblank,
            a.sub_nonblank,
            a.unit_nonblank,
            dist(&a.fxtype, "<blank>", 6),
            a.rate_nonzero,
            a.fx_cytd_nonzero,
            a.fx_pytd_nonzero,
            a.tac_populated,
            a.gifi_populated
        );
    }
    if let Some(b) = &r.bl {
        let _ = writeln!(
            o,
            "  BL  live={:4} numbers={} | YEAR {} | PERIOD {} max={} | BUCKET {}",
            b.rows,
            b.numbers,
            dist(&b.year, "<blank>", 6),
            dist(&b.period, "<blank>", 6),
            b.max_period,
            dist(&b.bucket, "<blank>", 4)
        );
        let _ = writeln!(
            o,
            "      ID {} | GROUP {} | TYPE {} | ENTITY {} | soloX={} (O={} Y={}) | FX!=0 {} (all X-overlay: {})",
            dist(&b.id, "<blank>", 4),
            dist(&b.group, "<blank>", 3),
            dist(&b.kind, "<blank>", 5),
            dist(&b.entity, "<blank>", 6),
            b.solo_x,
            b.solo_x_open,
            b.solo_x_year,
            b.fx_nonzero,
            b.fx_x_overlay_only
        );
    }
    if let Some(g) = &r.gl {
        let _ = writeln!(
            o,
            "  GL  live={:4} | STATUS {} | decoded {} | JOURSOURCE {} | PER 1={} blank={} other={}",
            g.rows,
            dist(&g.status, "<blank>", 14),
            dist(&g.status_label, "<blank>", 4),
            dist(&g.jour, "<blank>", 4),
            g.per_one,
            g.per_blank,
            g.rows.saturating_sub(g.per_one + g.per_blank)
        );
    }
    if let Some(c) = &r.ce {
        let pops: BTreeMap<String, usize> = c.per_populated.clone();
        let _ = writeln!(
            o,
            "  CE  rows={:3} | TYPE {} | ENTITY {} | DIV nonblank={} ABBR nonblank={} | PER* {}",
            c.rows,
            dist(&c.kind, "<blank>", 6),
            dist(&c.entity, "<blank>", 6),
            c.div_nonblank,
            c.abbr_nonblank,
            dist(&pops, "-", 8)
        );
    }
    if let Some(t) = &r.tp {
        let _ = writeln!(
            o,
            "  TP  live={:4} | distinct(FORM,ENTITY,YEAR)={} | FORM {} | ENTITY {}",
            t.rows,
            t.form_entity_year,
            dist(&t.form, "<blank>", 5),
            dist(&t.entity, "<blank>", 4)
        );
        let _ = writeln!(
            o,
            "      YEAR {} blank_year={} | PER {} | UDEF {} | TYPE {}",
            dist(&t.year, "<blank>", 6),
            t.blank_year,
            dist(&t.per, "<blank>", 4),
            dist(&t.udef, "<blank>", 4),
            dist(&t.kind, "<blank>", 4)
        );
        if !t.udef_form.is_empty() {
            let mut m: BTreeMap<String, usize> = BTreeMap::new();
            for ((u, f), n) in &t.udef_form {
                *m.entry(format!("{u}:{f}")).or_insert(0) += n;
            }
            let _ = writeln!(o, "      UDEF x FORM {}", dist(&m, "-", 6));
        }
    }
    if let Some(s) = &r.sh {
        let h = |a: &[usize]| {
            (0..a.len())
                .filter(|&i| a[i] > 0)
                .map(|i| format!("{i}:{}", a[i]))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let _ = writeln!(
            o,
            "  SH  live={:4} | TYPE {} | signed-levels {} | max-level {} | per-level {}",
            s.rows,
            dist(&s.doc_type, "<blank>", 6),
            h(&s.level_hist),
            h(&s.max_level_hist),
            s.per_level
                .iter()
                .enumerate()
                .map(|(i, n)| format!("{}:{n}", i + 1))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let _ = writeln!(
            o,
            "      S1..8 rows={} (per-level {}) SD1..8 rows={} | AC1..8 rows={} text-len {} | AU1..8 rows={} text-len {} | S cells {}: =AU same {} / =AU next {} / no AU {} | SIGN=Y {} LOCKDOWN=Y {} | invalid-cell rows {}",
            s.s_present,
            s.s_levels
                .iter()
                .enumerate()
                .map(|(i, n)| format!("{}:{n}", i + 1))
                .collect::<Vec<_>>()
                .join(" "),
            s.sd_present,
            s.ac_present,
            dist(&s.ac_len, "-", 4),
            s.au_present,
            dist(&s.au_len, "-", 4),
            s.s_cells,
            s.s_eq_au_same,
            s.s_eq_au_next,
            s.s_au_none,
            s.sign_yes,
            s.lockdown_yes,
            s.invalid_rows
        );
    }
    if let Some(m) = &r.mp {
        let _ = writeln!(
            o,
            "  MP  live={:4} | schemes {} | kinds {} | TITLE {} | MAPTYPE {} | unassigned={}",
            m.rows,
            dist(&m.schemes, "<blank>", 8),
            dist(&m.kinds, "-", 6),
            dist(&m.titles, "<blank>", 4),
            dist(&m.map_types, "<blank>", 4),
            m.unassigned
        );
    }
    let extra = ["PR", "RA", "WT", "TS", "AN", "WR", "HI"];
    let mut line = String::from("  aux ");
    for k in extra {
        let f = &r.tables[k];
        let _ = write!(line, "{k}=live{}", f.live);
        if f.tombs > 0 || !f.present {
            let _ = write!(line, "/tomb{}", f.tombs);
        }
        line.push(' ');
    }
    let _ = writeln!(o, "{line}");
    let ff = &r.files;
    let _ = writeln!(
        o,
        "  files total={} dbf={} cvw={} TV*={} docx={} xlsx={} xls={} pdf={}",
        ff.total, ff.dbf, ff.cvw, ff.tv, ff.docx, ff.xlsx, ff.xls, ff.pdf
    );
    let mut tl = String::from("  census ");
    for k in TABLES {
        let f = &r.tables[k];
        let _ = write!(tl, "{k}:{}", if f.present { f.live.to_string() } else { "-".into() });
        if f.damaged() > 0 {
            let _ = write!(tl, "(+{})", f.damaged());
        }
        tl.push(' ');
    }
    let _ = writeln!(o, "{tl}");
    o
}

/// One cross-corpus coverage verdict.
struct Verdict {
    shape: &'static str,
    covered: bool,
    detail: String,
}

fn verdicts(reports: &[Report]) -> Vec<Verdict> {
    let mut v = Vec::new();

    // multi-entity: more than one distinct ENTITY across CE/BL live rows.
    let mut detail = String::new();
    let mut any = false;
    for r in reports {
        let ce = r.ce.as_ref().map(|c| c.entity.len()).unwrap_or(0);
        let bl = r.bl.as_ref().map(|b| b.entity.len()).unwrap_or(0);
        let multi = ce > 1 || bl > 1;
        any |= multi;
        let _ = write!(detail, "{}: CE {} / BL {} entity codes{}; ", r.key, ce, bl, if multi { " (multi)" } else { "" });
    }
    let detail = detail.trim_end_matches("; ").to_string();
    v.push(Verdict {
        shape: "multi-entity (CE roster + BL.ENTITY non-blank)",
        covered: any,
        detail,
    });

    // PERIOD > 1.
    let maxp = reports
        .iter()
        .filter_map(|r| r.bl.as_ref().map(|b| (r.key, b.max_period)))
        .collect::<Vec<_>>();
    let any = maxp.iter().any(|(_, p)| *p > 1);
    v.push(Verdict {
        shape: "BL PERIOD > 1",
        covered: any,
        detail: maxp
            .iter()
            .map(|(k, p)| format!("{k}: max {p}"))
            .collect::<Vec<_>>()
            .join("; "),
    });

    // CE populated.
    let ce = reports.iter().map(|r| (r.key, r.ce.as_ref().map(|c| c.rows).unwrap_or(0))).collect::<Vec<_>>();
    v.push(Verdict {
        shape: "populated CE (consolidation roster)",
        covered: ce.iter().any(|(_, n)| *n > 0),
        detail: ce.iter().map(|(k, n)| format!("{k}: {n}")).collect::<Vec<_>>().join("; "),
    });

    // PER* ownership grid populated.
    let per = reports
        .iter()
        .map(|r| {
            let n = r
                .ce
                .as_ref()
                .map(|c| c.per_populated.values().sum::<usize>())
                .unwrap_or(0);
            (r.key, n)
        })
        .collect::<Vec<_>>();
    v.push(Verdict {
        shape: "CE PER/PER_P*/PER_BUD population",
        covered: per.iter().any(|(_, n)| *n > 0),
        detail: per.iter().map(|(k, n)| format!("{k}: {n} cells")).collect::<Vec<_>>().join("; "),
    });

    // fund segments (DIV/SUB/UNIT).
    let fund = reports
        .iter()
        .map(|r| {
            let a = r.am.as_ref();
            let n = a.map(|a| a.div_nonblank + a.sub_nonblank + a.unit_nonblank).unwrap_or(0);
            (r.key, n)
        })
        .collect::<Vec<_>>();
    v.push(Verdict {
        shape: "non-blank fund segment (AM DIV/SUB/UNIT)",
        covered: fund.iter().any(|(_, n)| *n > 0),
        detail: fund.iter().map(|(k, n)| format!("{k}: {n} non-blank cells")).collect::<Vec<_>>().join("; "),
    });

    // PR/RA/WT rows.
    let mut detail = String::new();
    let mut any = false;
    for k in ["PR", "RA", "WT"] {
        let mut parts = Vec::new();
        for r in reports {
            let f = &r.tables[k];
            any |= f.present && f.live > 0;
            parts.push(format!("{}={}", r.key, if f.present { f.live.to_string() } else { "absent".into() }));
        }
        let _ = write!(detail, "{k}[{}] ", parts.join(" "));
    }
    let detail = detail.trim_end().to_string();
    v.push(Verdict {
        shape: "populated PR / RA / WT",
        covered: any,
        detail,
    });

    // signoff beyond level 1.
    let sh: Vec<(&str, usize, u8, usize)> = reports
        .iter()
        .map(|r| {
            let s = r.sh.as_ref();
            let multi = s
                .map(|s| (2..=8).map(|i| s.max_level_hist[i]).sum::<usize>())
                .unwrap_or(0);
            let deepest = s
                .map(|s| (0..9).rev().find(|&i| s.max_level_hist[i] > 0).unwrap_or(0) as u8)
                .unwrap_or(0);
            let signed = s
                .map(|s| s.level_hist.iter().sum::<usize>() - s.level_hist[0])
                .unwrap_or(0);
            (r.key, multi, deepest, signed)
        })
        .collect();
    v.push(Verdict {
        shape: "multi-level signoff (SH ladder depth > level 1)",
        covered: sh.iter().any(|(_, m, ..)| *m > 0),
        detail: sh
            .iter()
            .map(|(k, m, deepest, signed)| {
                format!("{k}: {signed} signed rows, {m} beyond level 1, deepest L{deepest}")
            })
            .collect::<Vec<_>>()
            .join("; "),
    });

    // S1..S8 signoff-required family.
    let s_fam: Vec<(&str, usize, usize, usize, usize, usize, usize)> = reports
        .iter()
        .map(|r| {
            let s = r.sh.as_ref();
            (
                r.key,
                s.map(|s| s.s_present).unwrap_or(0),
                s.map(|s| s.sd_present).unwrap_or(0),
                s.map(|s| s.s_cells).unwrap_or(0),
                s.map(|s| s.s_eq_au_same).unwrap_or(0),
                s.map(|s| s.s_eq_au_next).unwrap_or(0),
                s.map(|s| s.s_au_none).unwrap_or(0),
            )
        })
        .collect();
    v.push(Verdict {
        shape: "SH S1..S8 / SD1..S8 population",
        covered: s_fam.iter().any(|(_, s, sd, ..)| *s > 0 || *sd > 0),
        detail: s_fam
            .iter()
            .map(|(k, s, sd, cells, same, next, none)| {
                format!(
                    "{k}: S rows {s} ({cells} cells: {same} = AU same level, \
                     {next} = AU +1 level, {none} match no AU), SD rows {sd}"
                )
            })
            .collect::<Vec<_>>()
            .join("; "),
    });

    // AC/AU companion slots.
    let acau = reports
        .iter()
        .map(|r| (r.key, r.sh.as_ref().map(|s| (s.ac_present, s.au_present)).unwrap_or((0, 0))))
        .collect::<Vec<_>>();
    v.push(Verdict {
        shape: "SH AC/AU companion slots populated",
        covered: acau.iter().any(|(_, (ac, au))| *ac > 0 || *au > 0),
        detail: acau.iter().map(|(k, (ac, au))| format!("{k}: AC {ac}, AU {au}")).collect::<Vec<_>>().join("; "),
    });

    // AM FX: nonzero rate or translated amount.
    let amfx = reports
        .iter()
        .map(|r| {
            let a = r.am.as_ref();
            let n = a.map(|a| a.rate_nonzero + a.fx_cytd_nonzero + a.fx_pytd_nonzero).unwrap_or(0);
            (r.key, n)
        })
        .collect::<Vec<_>>();
    v.push(Verdict {
        shape: "AM C_RATE/FXCYTD/FXPYTD non-zero",
        covered: amfx.iter().any(|(_, n)| *n > 0),
        detail: amfx.iter().map(|(k, n)| format!("{k}: {n} cells")).collect::<Vec<_>>().join("; "),
    });

    // BL FX translated amounts.
    let blfx = reports
        .iter()
        .map(|r| (r.key, r.bl.as_ref().map(|b| b.fx_nonzero).unwrap_or(0)))
        .collect::<Vec<_>>();
    v.push(Verdict {
        shape: "BL FX non-zero (translation applied)",
        covered: blfx.iter().any(|(_, n)| *n > 0),
        detail: blfx.iter().map(|(k, n)| format!("{k}: {n} rows")).collect::<Vec<_>>().join("; "),
    });

    // TP user-defined rows.
    let tp_udef = reports
        .iter()
        .map(|r| {
            let t = r.tp.as_ref();
            let user = t.map(|t| t.udef.get("user").copied().unwrap_or(0)).unwrap_or(0);
            let ext = t.map(|t| t.udef.get("extension").copied().unwrap_or(0)).unwrap_or(0);
            (r.key, user, ext)
        })
        .collect::<Vec<_>>();
    v.push(Verdict {
        shape: "TP UDEF user / extension rows",
        covered: tp_udef.iter().any(|(_, u, x)| *u > 0 || *x > 0),
        detail: tp_udef
            .iter()
            .map(|(k, u, x)| format!("{k}: U {u}, X {x}"))
            .collect::<Vec<_>>()
            .join("; "),
    });

    // MP scheme variety.
    let mps = reports
        .iter()
        .map(|r| {
            let m = r.mp.as_ref();
            let ids = m.map(|m| m.schemes.keys().cloned().collect::<Vec<_>>().join("+")).unwrap_or_default();
            (r.key, ids)
        })
        .collect::<Vec<_>>();
    v.push(Verdict {
        shape: "MP scheme ids `M` (statement tree) / `W` / blank",
        covered: mps.iter().any(|(_, ids)| {
            ids.split('+')
                .any(|x| x == "M" || x == "W" || x == "(blank)")
        }),
        detail: mps.iter().map(|(k, s)| format!("{k}: [{s}]")).collect::<Vec<_>>().join("; "),
    });

    // GL adjusting-entry classes.
    let gl = reports
        .iter()
        .map(|r| {
            let g = r.gl.as_ref();
            (r.key, g.map(|g| g.status.len()).unwrap_or(0))
        })
        .collect::<Vec<_>>();
    v.push(Verdict {
        shape: "GL STATUS classes (adjusting-entry types)",
        covered: gl.iter().any(|(_, n)| *n >= 2),
        detail: gl.iter().map(|(k, n)| format!("{k}: {n} letters")).collect::<Vec<_>>().join("; "),
    });

    // TP PER spread beyond the constant.
    let tp_per = reports
        .iter()
        .filter_map(|r| r.tp.as_ref().filter(|t| t.rows > 0).map(|t| (r.key, t.per.len())))
        .collect::<Vec<_>>();
    v.push(Verdict {
        shape: "TP PER varying (not a single constant)",
        covered: tp_per.iter().any(|(_, n)| *n > 1),
        detail: tp_per
            .iter()
            .map(|(k, n)| format!("{k}: {n} distinct PER values"))
            .collect::<Vec<_>>()
            .join("; "),
    });

    // GL / BL period spread beyond period 1.
    let periods = reports
        .iter()
        .map(|r| {
            let bl = r.bl.as_ref().map(|b| b.max_period).unwrap_or(-1);
            let gl_other = r
                .gl
                .as_ref()
                .map(|g| g.rows.saturating_sub(g.per_one + g.per_blank))
                .unwrap_or(0);
            (r.key, bl, gl_other)
        })
        .collect::<Vec<_>>();
    v.push(Verdict {
        shape: "GL PER after period 1",
        covered: periods.iter().any(|(_, _, other)| *other > 0),
        detail: periods
            .iter()
            .map(|(k, bl, other)| format!("{k}: BL max PERIOD {bl}, GL other-PER rows {other}"))
            .collect::<Vec<_>>()
            .join("; "),
    });

    // GL JOURSOURCE variety.
    let jour = reports
        .iter()
        .filter_map(|r| r.gl.as_ref().map(|g| (r.key, g.jour.clone())))
        .collect::<Vec<_>>();
    v.push(Verdict {
        shape: "GL JOURSOURCE beyond the manual journal source",
        covered: jour
            .iter()
            .any(|(_, m)| m.keys().any(|k| k != "AJ" && k != "<blank>")),
        detail: jour
            .iter()
            .map(|(k, m)| format!("{k}: {}", dist(m, "<blank>", 4)))
            .collect::<Vec<_>>()
            .join("; "),
    });

    v
}

/// `k1 n1; k2 n2` for one per-corpus numeric projection.
fn series(reports: &[Report], f: impl Fn(&Report) -> String) -> String {
    reports
        .iter()
        .map(|r| format!("{} {}", r.key, f(r)))
        .collect::<Vec<_>>()
        .join("; ")
}

/// `key=value` pairs (for prose where `key value` would read oddly).
fn series_eq(reports: &[Report], f: impl Fn(&Report) -> String) -> String {
    reports
        .iter()
        .map(|r| format!("{}={}", r.key, f(r)))
        .collect::<Vec<_>>()
        .join("; ")
}

fn render_markdown(reports: &[Report]) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "# CORPUS-FACTS — five-engagement decode census");
    let _ = writeln!(o);
    let _ = writeln!(
        o,
        "Generated by `crates/cw-app/examples/corpus_audit.rs` \
         (`cargo run -q -p cw-app --example corpus_audit`). Every number below is \
         decoded from the engagement directories under `opencw/expanded/`, over the \
         **readable prefix** only: record flag `0x20`/`0x2A`, live rows with a blank or \
         real `YYYYMMDDhhmmss` timestamp, and a blank/legacy-8/valid-22-char `CWGUID`. \
         That is the predicate pinned by `crates/cw-core/tests/guid_law_gate.rs`, so the \
         frontier for corp2026/hull2024/mwgh2025 equals its `DAMAGED` constant; the two \
         extra engagements are reported facts (no fixture, no pin).");
    let _ = writeln!(o);
    let _ = writeln!(
        o,
        "Truncated LZMA members (`tests/golden/README.md`, F63) put a zero-fill \
         fabricated tail after the frontier. Those rows are excluded from every census \
         and counted in the `damaged` column only. Censuses are therefore lower bounds on \
         the real engagement content, and exact for the shipped bytes up to the frontier."
    );
    let _ = writeln!(o);

    // ---- coverage matrix -------------------------------------------------
    let _ = writeln!(o, "## 1. Shapes that were corpus-absent — status now\n");
    let _ = writeln!(o, "| shape | status | evidence |");
    let _ = writeln!(o, "|---|---|---|");
    for v in verdicts(reports) {
        let _ = writeln!(
            o,
            "| {} | {} | {} |",
            v.shape,
            if v.covered { "**NOW COVERED**" } else { "still absent" },
            v.detail.replace('|', "\\|")
        );
    }
    let _ = writeln!(o);

    // ---- per-corpus table -------------------------------------------------
    let _ = writeln!(o, "## 2. Per-engagement table census\n");
    let _ = writeln!(
        o,
        "`n` = physical records, `front` = readable-prefix length, `live`/`tomb` = \
         live/tombstoned rows inside the prefix, `damaged` = fabricated tail rows.\n"
    );
    let _ = writeln!(o, "| corpus | table | n | front | live | tomb | damaged |");
    let _ = writeln!(o, "|---|---|---|---|---|---|---|");
    for r in reports {
        for k in TABLES {
            let f = &r.tables[k];
            if !f.present {
                let _ = writeln!(o, "| {} | {k} | – | – | – | – | absent |", r.key);
                continue;
            }
            let _ = writeln!(
                o,
                "| {} | {k} | {} | {} | {} | {} | {} |",
                r.key,
                f.records,
                f.frontier,
                f.live,
                f.tombs,
                f.damaged()
            );
        }
    }
    let _ = writeln!(o);

    // ---- semantics per corpus --------------------------------------------
    let _ = writeln!(o, "## 3. Semantic facts per engagement\n");
    for r in reports {
        let _ = writeln!(o, "### {} — {}\n", r.key, r.identity);
        let _ = writeln!(o, "- dir: `{}`  ·  {}", r.dir, r.duty);
        if let Some(a) = &r.am {
            let _ = writeln!(
                o,
                "- **AM** live {} (posting {}): ENTITY {}; TYPE {}; DIV/SUB/UNIT non-blank {}/{}/{}; FXTYPE {}; non-zero C_RATE/FXCYTD/FXPYTD {}/{}/{}; TAC_NO populated {}; GIFI populated {}",
                a.rows,
                a.posting,
                dist(&a.entity, "<blank>", 8),
                dist(&a.kind, "<blank>", 8),
                a.div_nonblank,
                a.sub_nonblank,
                a.unit_nonblank,
                dist(&a.fxtype, "<blank>", 8),
                a.rate_nonzero,
                a.fx_cytd_nonzero,
                a.fx_pytd_nonzero,
                a.tac_populated,
                a.gifi_populated
            );
        }
        if let Some(b) = &r.bl {
            let _ = writeln!(
                o,
                "- **BL** live {} / {} distinct NUMBER: YEAR {}; PERIOD {} (max {}); BUCKET {}; ID {}; GROUP {}; TYPE {}; ENTITY {}; solo-X markers {} (O bucket {}, Y bucket {}); FX non-zero {} rows ({} of them `ID='X'` with `FX == AMT == FX_YTD`)",
                b.rows,
                b.numbers,
                dist(&b.year, "<blank>", 8),
                dist(&b.period, "<blank>", 8),
                b.max_period,
                dist(&b.bucket, "<blank>", 4),
                dist(&b.id, "<blank>", 4),
                dist(&b.group, "<blank>", 4),
                dist(&b.kind, "<blank>", 6),
                dist(&b.entity, "<blank>", 8),
                b.solo_x,
                b.solo_x_open,
                b.solo_x_year,
                b.fx_nonzero,
                b.fx_x_overlay_only
            );
        }
        if let Some(g) = &r.gl {
            let _ = writeln!(
                o,
                "- **GL** live {}: STATUS {} (decoded {}); JOURSOURCE {}; PER `1` {} rows, blank {} rows, other {}",
                g.rows,
                dist(&g.status, "<blank>", 15),
                dist(&g.status_label, "<blank>", 5),
                dist(&g.jour, "<blank>", 4),
                g.per_one,
                g.per_blank,
                g.rows.saturating_sub(g.per_one + g.per_blank)
            );
        }
        if let Some(c) = &r.ce {
            let pops = c.per_populated.clone();
            let _ = writeln!(
                o,
                "- **CE** rows {}: TYPE {}; ENTITY {}; DIV non-blank {}; ABBR non-blank {}; PER* populated {}",
                c.rows,
                dist(&c.kind, "<blank>", 6),
                dist(&c.entity, "<blank>", 8),
                c.div_nonblank,
                c.abbr_nonblank,
                dist(&pops, "-", 8)
            );
        }
        if let Some(t) = &r.tp {
            let _ = writeln!(
                o,
                "- **TP** live {}: FORM {}; ENTITY {}; YEAR {} (blank {}); distinct (FORM,ENTITY,YEAR) {}; PER {}; UDEF {}; TYPE {}",
                t.rows,
                dist(&t.form, "<blank>", 6),
                dist(&t.entity, "<blank>", 4),
                dist(&t.year, "<blank>", 8),
                t.blank_year,
                t.form_entity_year,
                dist(&t.per, "<blank>", 4),
                dist(&t.udef, "<blank>", 4),
                dist(&t.kind, "<blank>", 4)
            );
            if !t.udef_form.is_empty() {
                let mut m: BTreeMap<String, usize> = BTreeMap::new();
                for ((u, f), n) in &t.udef_form {
                    *m.entry(format!("`{u}` × `{f}`")).or_insert(0) += n;
                }
                let _ = writeln!(o, "  · user/extension rows by form: {}", dist(&m, "-", 8));
            }
        }
        if let Some(s) = &r.sh {
            let h = |a: &[usize]| {
                (0..a.len())
                    .filter(|&i| a[i] > 0)
                    .map(|i| format!("{i}:{}", a[i]))
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            let _ = writeln!(
                o,
                "- **SH** live {}: TYPE {}; signed-level depth {}; deepest-level {}; per-level {}; S1..8 {} rows (per-level {}; of {} populated S cells {} equal `AU` at the same level, {} equal `AU` one level later, {} match no `AU` cell); SD1..8 {} rows; AC1..8 {} rows with text lengths {}; AU1..8 {} rows with text lengths {}; SIGN=Y {}; LOCKDOWN=Y {}; rows with unreadable ladder cells {} (excluded)",
                s.rows,
                dist(&s.doc_type, "<blank>", 6),
                h(&s.level_hist),
                h(&s.max_level_hist),
                s.per_level
                    .iter()
                    .enumerate()
                    .map(|(i, n)| format!("{}:{n}", i + 1))
                    .collect::<Vec<_>>()
                    .join(" "),
                s.s_present,
                s.s_levels
                    .iter()
                    .enumerate()
                    .map(|(i, n)| format!("{}:{n}", i + 1))
                    .collect::<Vec<_>>()
                    .join(" "),
                s.s_cells,
                s.s_eq_au_same,
                s.s_eq_au_next,
                s.s_au_none,
                s.sd_present,
                s.ac_present,
                dist(&s.ac_len, "-", 4),
                s.au_present,
                dist(&s.au_len, "-", 4),
                s.sign_yes,
                s.lockdown_yes,
                s.invalid_rows
            );
        }
        if let Some(m) = &r.mp {
            let _ = writeln!(
                o,
                "- **MP** live {}: schemes {}; node kinds {}; TITLE {}; MAPTYPE {}; unassigned {}",
                m.rows,
                dist(&m.schemes, "<blank>", 8),
                dist(&m.kinds, "-", 8),
                dist(&m.titles, "<blank>", 4),
                dist(&m.map_types, "<blank>", 4),
                m.unassigned
            );
        }
        let mut aux = Vec::new();
        for k in ["PR", "RA", "WT", "TS", "AN", "WR", "HI"] {
            let f = &r.tables[k];
            aux.push(if f.present {
                format!("{k} live {}/tomb {}", f.live, f.tombs)
            } else {
                format!("{k} absent")
            });
        }
        let _ = writeln!(o, "- **aux** {}", aux.join("; "));
        let ff = &r.files;
        let _ = writeln!(
            o,
            "- **files** total {}: dbf {} · cvw {} · TV* {} · docx {} · xlsx {} · xls {} · pdf {}",
            ff.total, ff.dbf, ff.cvw, ff.tv, ff.docx, ff.xlsx, ff.xls, ff.pdf
        );
        let _ = writeln!(o);
    }

    // ---- cross-corpus matrix ---------------------------------------------
    let _ = writeln!(o, "## 4. Cross-corpus matrix\n");
    let _ = writeln!(o, "| fact | {} |", reports.iter().map(|r| r.key).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "|---|{}", "---|".repeat(reports.len()));
    // header labels (kept explicit so the table is readable in raw form)
    let _ = writeln!(o, "| AM live | {} |", reports.iter().map(|r| r.am.as_ref().map(|a| a.rows.to_string()).unwrap_or("-".into())).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| AM distinct ENTITY | {} |", reports.iter().map(|r| r.am.as_ref().map(|a| a.entity.len().to_string()).unwrap_or("-".into())).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| AM posting rows | {} |", reports.iter().map(|r| r.am.as_ref().map(|a| a.posting.to_string()).unwrap_or("-".into())).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| AM non-blank fund cells | {} |", reports.iter().map(|r| r.am.as_ref().map(|a| (a.div_nonblank + a.sub_nonblank + a.unit_nonblank).to_string()).unwrap_or("-".into())).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| AM FXTYPE H/C/unset | {} |", reports.iter().map(|r| match &r.am {
        Some(a) => format!("{}/{}/{}", a.fxtype.get("historical").copied().unwrap_or(0), a.fxtype.get("current").copied().unwrap_or(0), a.fxtype.get("unset").copied().unwrap_or(0)),
        None => "-".into(),
    }).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| AM FX non-zero cells | {} |", reports.iter().map(|r| r.am.as_ref().map(|a| (a.rate_nonzero + a.fx_cytd_nonzero + a.fx_pytd_nonzero).to_string()).unwrap_or("-".into())).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| AM TAC_NO populated | {} |", reports.iter().map(|r| r.am.as_ref().map(|a| a.tac_populated.to_string()).unwrap_or("-".into())).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| BL live | {} |", reports.iter().map(|r| r.bl.as_ref().map(|b| b.rows.to_string()).unwrap_or("-".into())).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| BL distinct ENTITY | {} |", reports.iter().map(|r| r.bl.as_ref().map(|b| b.entity.len().to_string()).unwrap_or("-".into())).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| BL max PERIOD | {} |", reports.iter().map(|r| r.bl.as_ref().map(|b| b.max_period.to_string()).unwrap_or("-".into())).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| BL solo-X markers | {} |", reports.iter().map(|r| r.bl.as_ref().map(|b| format!("{}(O{}/Y{})", b.solo_x, b.solo_x_open, b.solo_x_year)).unwrap_or("-".into())).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| BL FX non-zero rows | {} |", reports.iter().map(|r| r.bl.as_ref().map(|b| b.fx_nonzero.to_string()).unwrap_or("-".into())).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| GL live / STATUS letters | {} |", reports.iter().map(|r| r.gl.as_ref().map(|g| format!("{}/{}", g.rows, g.status.len())).unwrap_or("-".into())).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| CE rows | {} |", reports.iter().map(|r| r.ce.as_ref().map(|c| c.rows.to_string()).unwrap_or("-".into())).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| TP live | {} |", reports.iter().map(|r| r.tp.as_ref().map(|t| t.rows.to_string()).unwrap_or("-".into())).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| TP distinct (FORM,ENTITY,YEAR) | {} |", reports.iter().map(|r| r.tp.as_ref().map(|t| t.form_entity_year.to_string()).unwrap_or("-".into())).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| SH live | {} |", reports.iter().map(|r| r.sh.as_ref().map(|s| s.rows.to_string()).unwrap_or("-".into())).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| SH rows signed (any level) | {} |", reports.iter().map(|r| r.sh.as_ref().map(|s| (s.level_hist.iter().sum::<usize>() - s.level_hist[0]).to_string()).unwrap_or("-".into())).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| SH deepest level | {} |", reports.iter().map(|r| r.sh.as_ref().map(|s| (0..9).rev().find(|&i| s.max_level_hist[i] > 0).unwrap_or(0).to_string()).unwrap_or("-".into())).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| SH S1..8 rows | {} |", reports.iter().map(|r| r.sh.as_ref().map(|s| s.s_present.to_string()).unwrap_or("-".into())).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| MP live / schemes | {} |", reports.iter().map(|r| r.mp.as_ref().map(|m| format!("{}/{}", m.rows, m.schemes.len())).unwrap_or("-".into())).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| PR/RA/WT live | {} |", reports.iter().map(|r| format!("{}/{}/{}", r.tables["PR"].live, r.tables["RA"].live, r.tables["WT"].live)).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| TS live/tomb | {} |", reports.iter().map(|r| format!("{}/{}", r.tables["TS"].live, r.tables["TS"].tombs)).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| AN live/tomb | {} |", reports.iter().map(|r| format!("{}/{}", r.tables["AN"].live, r.tables["AN"].tombs)).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| HI live/tomb | {} |", reports.iter().map(|r| format!("{}/{}", r.tables["HI"].live, r.tables["HI"].tombs)).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| WR live/tomb | {} |", reports.iter().map(|r| format!("{}/{}", r.tables["WR"].live, r.tables["WR"].tombs)).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| files cvw/TV* | {} |", reports.iter().map(|r| format!("{}/{}", r.files.cvw, r.files.tv)).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "| files docx/xlsx | {} |", reports.iter().map(|r| format!("{}/{}", r.files.docx, r.files.xlsx)).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o);

    // ---- shared content --------------------------------------------------
    let _ = writeln!(o, "## 5. Cross-engagement content overlap\n");
    let _ = writeln!(
        o,
        "`prefix digest` = SHA-256 over the readable-prefix records with the audit \
         stamp / id / memo-pointer / checksum columns blanked (`CWDATE`, `CREATEDON`, \
         `CWGUID`, `CWCHECKSUM`, `CONSCHKSUM`, `EXTCHKSUM`, `EXINFO`, `MEMO`, `UID_NOTE`, \
         `FORMULA`, `TAGS`). Two engagements with the same digest for a table hold the \
         same logical content in that table, even when their stamp columns and their \
         fabricated tails differ — the test for \"is this a re-opened copy?\".\n"
    );
    let _ = writeln!(
        o,
        "| table | {} |",
        reports.iter().map(|r| r.key).collect::<Vec<_>>().join(" | ")
    );
    let _ = writeln!(o, "|---|{}", "---|".repeat(reports.len()));
    for k in TABLES {
        let cell_txt = |r: &Report| -> String {
            let f = &r.tables[k];
            if !f.present {
                return "–".into();
            }
            if f.frontier == 0 {
                return "∅".into();
            }
            f.prefix_digest[..8].to_string()
        };
        let row: Vec<String> = reports.iter().map(cell_txt).collect();
        let _ = writeln!(o, "| {k} | {} |", row.join(" | "));
    }
    let _ = writeln!(o);
    let _ = writeln!(
        o,
        "Row-level overlap (share of `A`'s live prefix rows whose key also occurs in \
         `B`; 100% on a table means `B` reproduces `A`'s content, so the table is one \
         dataset seen twice, not two independent samples):\n"
    );
    let _ = writeln!(o, "| pair | table | rows in A | rows in B | shared | A covered |");
    let _ = writeln!(o, "|---|---|---|---|---|---|");
    let mut any = false;
    let live_keys = |r: &Report, k: &str| -> Vec<u64> {
        let f = &r.tables[k];
        f.row_keys.clone()
    };
    for i in 0..reports.len() {
        for j in (i + 1)..reports.len() {
            let (a, b) = (&reports[i], &reports[j]);
            for k in TABLES {
                let fa = &a.tables[k];
                let fb = &b.tables[k];
                if !fa.present || !fb.present || fa.row_keys.is_empty() || fb.row_keys.is_empty() {
                    continue;
                }
                let set_b: BTreeSet<u64> = live_keys(b, k).into_iter().collect();
                let shared = live_keys(a, k).iter().filter(|x| set_b.contains(x)).count();
                // Report only relations large enough to be structural: a handful
                // of coincidental rows (repeated blank/sentinel records) is noise.
                let coverage = shared as f64 / fa.row_keys.len() as f64;
                if shared < 10 || coverage < 0.40 {
                    continue;
                }
                any = true;
                let _ = writeln!(
                    o,
                    "| {} / {} | {k} | {} | {} | {} | {:.1}% |",
                    a.key,
                    b.key,
                    fa.row_keys.len(),
                    fb.row_keys.len(),
                    shared,
                    100.0 * shared as f64 / fa.row_keys.len() as f64
                );
            }
        }
    }
    if !any {
        let _ = writeln!(o, "| – | – | – | – | – | – |");
    }
    let _ = writeln!(o);
    // Computed support for the prose below (no hand-typed figures).
    let fp_rows = series_eq(reports, |r| r.tables["FP"].live.to_string());
    let cv_rows = series_eq(reports, |r| r.tables["CV"].live.to_string());
    // largest TP overlap observed between two engagements with different identity
    let mut best_tp = (String::new(), 0.0_f64);
    for i in 0..reports.len() {
        for j in (i + 1)..reports.len() {
            let (a, b) = (&reports[i], &reports[j]);
            if a.key == "usi-small" || b.key == "usi-small" {
                continue; // the copy relation is discussed separately
            }
            let (fa, fb) = (&a.tables["TP"], &b.tables["TP"]);
            if fa.row_keys.is_empty() || fb.row_keys.is_empty() {
                continue;
            }
            let set_b: BTreeSet<u64> = fb.row_keys.iter().copied().collect();
            let shared = fa.row_keys.iter().filter(|x| set_b.contains(x)).count();
            let cov = 100.0 * shared as f64 / fa.row_keys.len() as f64;
            if cov > best_tp.1 {
                best_tp = (format!("{} / {}", a.key, b.key), cov);
            }
        }
    }
    let mut best_mp = (String::new(), 0.0_f64);
    for i in 0..reports.len() {
        for j in (i + 1)..reports.len() {
            let (a, b) = (&reports[i], &reports[j]);
            let (Some(ma), Some(mb)) = (&a.mp, &b.mp) else { continue };
            let ids = |m: &MpFacts| m.schemes.keys().cloned().collect::<Vec<_>>();
            // The `usi-small` copy relation is discussed in its own bullet; here we
            // want the shipped-boilerplate signal between *independent* engagements.
            if a.key == "usi-small" || b.key == "usi-small" {
                continue;
            }
            if ids(ma) != ids(mb) {
                continue;
            }
            let (fa, fb) = (&a.tables["MP"], &b.tables["MP"]);
            if fa.row_keys.is_empty() || fb.row_keys.is_empty() {
                continue;
            }
            let set_b: BTreeSet<u64> = fb.row_keys.iter().copied().collect();
            let shared = fa.row_keys.iter().filter(|x| set_b.contains(x)).count();
            let cov = 100.0 * shared as f64 / fa.row_keys.len() as f64;
            if cov > best_mp.1 {
                best_mp = (format!("{} / {}", a.key, b.key), cov);
            }
        }
    }

    // --- the usi-small <-> corp2026 copy relation, measured per table ---------
    // `coverage` = share of the *smaller* table's prefix rows (live **and**
    // tombstoned) that also occur in the other; `identical` = the row sets are
    // equal, i.e. a genuine duplicate table.
    let copy_profile: Vec<(&str, String, f64)> = {
        let mut out = Vec::new();
        let pair = (
            reports.iter().find(|r| r.key == "corp2026"),
            reports.iter().find(|r| r.key == "usi-small"),
        );
        if let (Some(a), Some(b)) = pair {
            for k in TABLES {
                let (fa, fb) = (&a.tables[k], &b.tables[k]);
                if !fa.present || !fb.present || fa.row_keys.is_empty() || fb.row_keys.is_empty() {
                    out.push((k, "-".into(), 0.0));
                    continue;
                }
                let sa: BTreeSet<u64> = fa.row_keys.iter().copied().collect();
                let sb: BTreeSet<u64> = fb.row_keys.iter().copied().collect();
                let shared = sa.intersection(&sb).count();
                let smaller = sa.len().min(sb.len());
                let identical = sa == sb;
                let cov = 100.0 * shared as f64 / smaller as f64;
                let note = if identical {
                    format!("identical ({shared})")
                } else {
                    format!("{shared}/{smaller} = {cov:.1}%")
                };
                out.push((k, note, cov));
            }
        }
        out
    };

    let copy_hi_list = copy_profile
        .iter()
        .filter(|(_, _, cov)| *cov >= 90.0)
        .map(|(k, _, _)| *k)
        .collect::<Vec<_>>()
        .join("/");
    let copy_lo_list = copy_profile
        .iter()
        .filter(|(_, _, cov)| *cov > 0.0 && *cov < 90.0)
        .map(|(k, _, _)| *k)
        .collect::<Vec<_>>()
        .join("/");

    let _ = writeln!(
        o,
        "Reading of the table (rows shown only at ≥10 shared rows and ≥40% of `A`):\n\n\
         * **FP is a stock table.** Live `FP` rows per engagement: {fp_rows}. The row set \
         is CaseWare's shipped formatting/print definitions, not engagement work.\n\
         * **CV is largely stock too**, but its size swings and the shared fraction \
         swings with it — live rows: {cv_rows}. The engagements carry a common \
         boilerplate core and then their own CaseView documents.\n\
         * **MP schemes are shipped boilerplate where the `MP.ID` sets are equal.** The \
         largest MP overlap between two engagements with identical `MP.ID` sets is \
         {mp_cov:.1}% ({mp_pair}); engagements whose sets differ share almost \
         nothing. The lead-sheet/mapping tree is the product default and only the \
         residual is engagement work.\n\
         * **usi-small is a re-worked copy of corp2026, not an independent client.** \
         Per-table copy profile (share of the smaller table's rows present in both; \
         `identical` = the same row set): {copy}. Tables reproducing corp2026 at \
         ≥90%: {copy_hi}; \
         so those censuses must not be counted twice; AM/BL/HI/SH do **not** — this is \
         the same client at a different revision, not the same file.\n\
         * **TP overlap between two different clients reaches {tp_cov:.1}%** ({tp_pair}), \
         consistent \
         with a shared shipped tax-code catalogue plus client-specific codes — which is \
         what `TP.UDEF` marks.\n",
        copy_hi = copy_hi_list,
        copy = copy_profile
            .iter()
            .filter(|(_, _, cov)| *cov > 0.0)
            .map(|(k, note, _)| format!("{k}: {note}"))
            .collect::<Vec<_>>()
            .join(", "),
        mp_cov = best_mp.1,
        mp_pair = best_mp.0,
        tp_cov = best_tp.1,
        tp_pair = best_tp.0
    );
    let _ = writeln!(o);

    let _ = writeln!(o, "## 6. What this changes in the specs\n");
    let _ = writeln!(
        o,
        "Each row names the module/spec text that the evidence above contradicts or \
         extends. Nothing here was edited; these are the claims a spec amendment would \
         have to cite.\n"
    );
    let _ = writeln!(o, "| spec claim | corpus evidence | status |");
    let _ = writeln!(o, "|---|---|---|");
    let _ = writeln!(
        o,
        "| `cw-app/src/entities.rs` + `specs/entities.md` §\"CE has no populated row \
         anywhere\" and the `Org::is_single_blank` normal form | live `CE` rows / \
         distinct `CE.ENTITY` / distinct `BL.ENTITY` per engagement: {} | **contradicted** \
         (one engagement has 10 CE rows and 9 BL entity codes) |",
        series(reports, |r| format!(
            "{} / {} / {}",
            r.ce.as_ref().map(|c| c.rows).unwrap_or(0),
            r.ce.as_ref().map(|c| c.entity.len()).unwrap_or(0),
            r.bl.as_ref().map(|b| b.entity.len()).unwrap_or(0)
        ))
    );
    let _ = writeln!(
        o,
        "| `cw-app/src/fund.rs` \"AM.DIV/SUB/UNIT blank on every row\" | non-blank \
         `AM` fund cells and `CE.DIV` cells per engagement: {} | holds |",
        series(reports, |r| format!(
            "{}/{}",
            r.am
                .as_ref()
                .map(|a| a.div_nonblank + a.sub_nonblank + a.unit_nonblank)
                .unwrap_or(0),
            r.ce.as_ref().map(|c| c.div_nonblank).unwrap_or(0)
        ))
    );
    let _ = writeln!(
        o,
        "| `cw-app/src/signoff.rs` \"levels 2..8 are empty in every corpus\" | rows \
         signed past level 1 and the deepest signed level, per engagement: {} | \
         **contradicted** |",
        series(reports, |r| match &r.sh {
            Some(s) => {
                let deepest = (0..9).rev().find(|&i| s.max_level_hist[i] > 0).unwrap_or(0);
                let beyond: usize = (2..=8).map(|i| s.max_level_hist[i]).sum();
                format!("{beyond} rows, deepest L{deepest}")
            }
            None => "-".into(),
        })
    );
    let _ = writeln!(
        o,
        "| `cw-app/src/signoff.rs` \"`S1..S8` empty in all corpora\" | populated `S` \
         rows/cells per engagement: {} | **contradicted** |",
        series(reports, |r| match &r.sh {
            Some(s) => format!("{} rows/{} cells", s.s_present, s.s_cells),
            None => "-".into(),
        })
    );
    let _ = writeln!(
        o,
        "| `cw-app/src/signoff.rs` \"`AU='E120235'` is a user id; `AC` semantics \
         unverified\" | text-length histograms over every populated `AC{{n}}`/`AU{{n}}` \
         cell: AU {}; AC {} — `AU` is uniformly the 7-char `e097283` user-id form and \
         `AC` is uniformly 22 chars (GUID width), on every signed level, not just level \
         1 | `AU` confirmed; `AC` semantics still unverified |",
        series(reports, |r| match &r.sh {
            Some(s) => format!(
                "{} cells {}",
                s.au_len.values().sum::<usize>(),
                dist(&s.au_len, "-", 3)
            ),
            None => "-".into(),
        }),
        series(reports, |r| match &r.sh {
            Some(s) => format!(
                "{} cells {}",
                s.ac_len.values().sum::<usize>(),
                dist(&s.ac_len, "-", 3)
            ),
            None => "-".into(),
        })
    );
    let _ = writeln!(
        o,
        "| `specs/cube.md` \"BL Y rows PERIOD 1..12 (corpora at 1)\" | distinct \
         `BL.PERIOD` values and their max, per engagement: {} | holds / narrowed |",
        series(reports, |r| match &r.bl {
            Some(b) => format!("{} (max {})", dist(&b.period, "<blank>", 4), b.max_period),
            None => "-".into(),
        })
    );
    let _ = writeln!(
        o,
        "| `specs/cube.md` \"BOOKED/CUSTOM/ENTITY blank in all corpora\" | `BL.BOOKED` and \
         `BL.CUSTOM` are blank on every live row; distinct `BL.ENTITY` codes: {} | \
         **partly contradicted** |",
        series(reports, |r| r
            .bl
            .as_ref()
            .map(|b| b.entity.len().to_string())
            .unwrap_or("-".into()))
    );
    let _ = writeln!(
        o,
        "| `cw-app/src/fx.rs` \"all FX amounts/rates zero\" (TODO.md P5.H H3) | non-zero \
         `AM.C_RATE`+`FXCYTD`+`FXPYTD` cells: {}; non-zero `BL.FX` rows (X-overlay share): \
         {} | holds for rates; BL amounts are populated |",
        series(reports, |r| r
            .am
            .as_ref()
            .map(|a| (a.rate_nonzero + a.fx_cytd_nonzero + a.fx_pytd_nonzero).to_string())
            .unwrap_or("-".into())),
        series(reports, |r| match &r.bl {
            Some(b) => format!("{}/{}", b.fx_nonzero, b.fx_x_overlay_only),
            None => "-".into(),
        })
    );
    let _ = writeln!(
        o,
        "| `cw-app/src/tax.rs` \"`X` = extension, and the `FORM='Unknown'` rows are those \
         extensions\" | `UDEF` total and the `FORM` of every non-standard row: {} | `U` \
         also sits on `FORM='Unknown'`; `X` occurs in one engagement only |",
        series(reports, |r| match &r.tp {
            Some(t) if t.rows > 0 => {
                let mut m: BTreeMap<String, usize> = BTreeMap::new();
                for ((u, f), n) in &t.udef_form {
                    *m.entry(format!("{u}:{f}")).or_insert(0) += n;
                }
                format!("{} [{}]", dist(&t.udef, "<blank>", 3), dist(&m, "-", 4))
            }
            _ => "-".into(),
        })
    );
    let _ = writeln!(
        o,
        "| `cw-app/src/groups.rs` \"schemas 1..9, `M`, `W`, blank\" | observed `MP.ID` \
         sets: {} | narrowed: no `M`, `W` or blank id anywhere |",
        series(reports, |r| match &r.mp {
            Some(m) => format!(
                "[{}]",
                m.schemes.keys().cloned().collect::<Vec<_>>().join(",")
            ),
            None => "-".into(),
        })
    );
    let _ = writeln!(
        o,
        "| `cw-app/src/adjustments.rs` `STATUS` letters | observed `GL.STATUS` per \
         engagement: {} | only `N`/`R`/`T` exercised; the rest unexercised, not \
         contradicted |",
        series(reports, |r| match &r.gl {
            Some(g) if g.rows > 0 => dist(&g.status, "<blank>", 15),
            _ => "-".into(),
        })
    );
    let _ = writeln!(
        o,
        "| `cw-app/src/documents.rs` / `program.rs` \"`PR`/`WT` have zero rows\" | live \
         `PR`/`RA`/`WT` rows per engagement: {} | holds everywhere |",
        series(reports, |r| format!(
            "{}/{}/{}",
            r.tables["PR"].live, r.tables["RA"].live, r.tables["WT"].live
        ))
    );
    let _ = writeln!(
        o,
        "| `cw-core/src/interlink.rs` `AN.DOCUMENTID` danglers | `AN` live/tombstone \
         rows: {} | census supplied |",
        series(reports, |r| format!(
            "{}/{}",
            r.tables["AN"].live, r.tables["AN"].tombs
        ))
    );
    let _ = writeln!(o);

    let _ = writeln!(o, "## 7. Caveats\n");
    let _ = writeln!(
        o,
        "1. **Fabricated tails.** corp2026 (HI/MP/TP), hull2024 (BL/HI/MP/SH/TP) and \
         mwgh2025 (BL/HI/MP/TP) carry truncated LZMA members; the extra Viewrail \
         engagement is damaged in AN/AM/BL/HI/MP/SH/TP/TS as well (see the `damaged` \
         column). Every census above stops at the last readable row, so counts are \
         exact for authentic bytes and lower bounds for the engagement."
    );
    let _ = writeln!(
        o,
        "1b. **Frontier, not file size.** Fabricated rows per engagement: {}. Any number \
         in this report that looks small relative to a table's record count is that \
         ratio, not a decode failure. The three frozen corpora are checked against \
         `guid_law_gate.rs`'s pinned `DAMAGED` frontiers by this example, so a moved \
         boundary fails the run instead of silently changing the census.",
        series(reports, |r| {
            let n: usize = r.tables.values().map(|t| t.damaged()).sum();
            let worst = r
                .tables
                .iter()
                .filter(|(_, t)| t.damaged() > 0)
                .map(|(k, t)| (k.clone(), t.damaged(), t.records))
                .max_by_key(|(_, d, _)| *d);
            match worst {
                Some((k, d, n_rec)) => format!("{n} rows (worst {k} {d}/{n_rec})"),
                None => format!("{n} rows"),
            }
        })
    );
    let _ = writeln!(
        o,
        "2. **`usi-small` is the same client as corp2026 at a different revision.** \
         Tables reproducing corp2026 at ≥90%: {}; tables that do not: {}. Count the \
         former once when aggregating corpus facts. It ships no `TV*` file (corp2026 \
         likewise) and carries a single `.cvw` against corp2026's 7.",
        copy_hi_list,
        copy_lo_list
    );
    let _ = writeln!(
        o,
        "3. **hull2024 SH ladder cells.** Two live SH rows inside the prefix carry \
         non-printable initials / non-date `AD` cells — fabricated payload that still \
         passes the timestamp/GUID frontier law (row 601 `AD5='VGssJex/'`, row 602 \
         `AD8=\"'Uni0B0\"`). They are counted under `invalid-cell rows` and excluded from \
         the histograms, so hull2024's deepest real signed level is 5 and the apparent \
         level-8 row is not signoff evidence. Viewrail has 1 such row."
    );
    let _ = writeln!(
        o,
        "4. **BL solo-X.** A marker is a `(NUMBER,YEAR,BUCKET,GROUP)` group whose only \
         overlay id is `X` (no `O` official sibling) — the `NETINC`/`GROUP__NI` law. The \
         `O bucket` column is what `cw_core::cube::CubeAudit::solo_x` counts; `Y bucket` \
         markers also occur (mwgh2025 has 37 of its 42) and are reported separately."
    );
    let _ = writeln!(
        o,
        "4b. **BL FX non-zero is the X overlay, not translation.** Non-zero `BL.FX` rows \
         per engagement (and how many are `ID='X'` with `FX == AMT == FX_YTD`): {}. \
         `AM.C_RATE`/`FXCYTD`/`FXPYTD` are 0 on every row everywhere, so no translated \
         amount can exist: the BL figures are the mirrored overlay fill of `cube.md` §3. \
         FX *amounts* are populated; FX *rates* are still corpus-absent.",
        series(reports, |r| match &r.bl {
            Some(b) => format!("{} ({} X-overlay)", b.fx_nonzero, b.fx_x_overlay_only),
            None => "-".into(),
        })
    );
    let _ = writeln!(
        o,
        "4c. **`TP.PER` is effectively a constant.** Distinct live `PER` distributions: \
         {}. The tax-export percentage is not a varying axis in this corpus; only its \
         blank-vs-`100.00` split matters.",
        series(reports, |r| match &r.tp {
            Some(t) if t.rows > 0 => dist(&t.per, "<blank>", 4),
            _ => "-".into(),
        })
    );
    let _ = writeln!(
        o,
        "4d. **`GL.PER` is period 1 or blank.** Live rows with `PER='1'` / blank / other: \
         {}. No engagement posts a GL line in a period after 1, matching `BL`'s \
         `max PERIOD == 1` everywhere.",
        series(reports, |r| match &r.gl {
            Some(g) => format!(
                "{}/{}/{}",
                g.per_one,
                g.per_blank,
                g.rows.saturating_sub(g.per_one + g.per_blank)
            ),
            None => "-".into(),
        })
    );
    let _ = writeln!(
        o,
        "4e. **`S1..S8` is a second signer ladder, offset by one level.** hull2024 \
         populates 58 `S` cells on 50 rows (levels 1–3) and Viewrail 2 rows; every `SD` \
         cell is empty in all five. The values are `E097283`-style user ids, and 45 of \
         the 58 equal `AU{{n+1}}` — not `AU{{n}}` (0 same-level matches). The 13 \
         non-matching cells are a different user (`E010508`/`E002414` at levels 2/3, i.e. \
         that row's own preparer ladder) plus one fabricated cell. `signoff.rs` treats the \
         `S` family as out of scope; the data shows it is populated and carries signer \
         identity, so that residual is about the *write* path and the COM surface, not \
         about whether the corpus has rows."
    );
    let _ = writeln!(
        o,
        "5. **No write path.** This audit only reads. It never writes into `expanded/` and \
         never touches the `.ac_` packages."
    );
    o
}

/// Compare every pinned frontier against the measured one. Returns the
/// mismatch descriptions (empty = the audit agrees with the law gate).
fn verify_frontiers(reports: &[Report]) -> Vec<String> {
    let mut problems = Vec::new();
    let pinned: BTreeMap<(&str, &str), usize> = DAMAGED
        .iter()
        .map(|(c, t, n)| ((*c, *t), *n))
        .collect();
    let frozen: BTreeSet<&str> = DAMAGED.iter().map(|(c, _, _)| *c).collect();
    for r in reports {
        for (t, f) in &r.tables {
            match pinned.get(&(r.key, t.as_str())) {
                Some(&n) if f.present && f.frontier != n => problems.push(format!(
                    "{}/{}: pinned frontier {n}, measured {}",
                    r.key, t, f.frontier
                )),
                Some(_) if !f.present => {
                    problems.push(format!("{}/{}: pinned but absent", r.key, t))
                }
                Some(_) => {}
                // Only the frozen corpora are pinned; the two extra
                // engagements have no `DAMAGED` duty, so a fabricated tail
                // there is a reported fact rather than drift.
                None if frozen.contains(r.key) => {
                    if f.present && f.damaged() > 0 {
                        problems.push(format!(
                            "{}/{}: fabricated tail {} rows but not pinned in DAMAGED",
                            r.key,
                            t,
                            f.damaged()
                        ));
                    }
                }
                None => {}
            }
        }
    }
    // a pin for a table that the corpus does not have at all is also drift
    for (c, t) in pinned.keys() {
        let known = reports.iter().find(|r| r.key == *c);
        match known {
            Some(r) if r.tables.get(*t).map(|f| f.present).unwrap_or(false) => {}
            Some(_) => problems.push(format!("{c}/{t}: pinned table missing on disk")),
            None => problems.push(format!("{c}/{t}: pinned corpus not audited")),
        }
    }
    problems
}

fn main() {
    let reports: Vec<Report> = CORPORA.iter().map(audit).collect();

    let problems = verify_frontiers(&reports);
    if problems.is_empty() {
        println!(
            "frontier pins: {} pinned tables agree with guid_law_gate.rs DAMAGED; \
             every unpinned table of the three frozen corpora is readable end to end \
             (the two extra engagements are unpinned facts, not assertions)",
            DAMAGED.len()
        );
    } else {
        for p in &problems {
            println!("frontier pin MISMATCH: {p}");
        }
        panic!("{} frontier pin mismatch(es)", problems.len());
    }
    for r in &reports {
        print!("{}", render_console(r));
    }

    // Cross-corpus one-liners on stdout (the markdown carries the same data).
    println!("\n=== cross-corpus coverage");
    for v in verdicts(&reports) {
        println!(
            "  {:<7} {}  [{}]",
            if v.covered { "COVERED" } else { "absent" },
            v.shape,
            v.detail.trim_end_matches("; ")
        );
    }

    let md = render_markdown(&reports);
    let out = root().join("..").join("CORPUS-FACTS.md");
    std::fs::write(&out, md).unwrap_or_else(|e| panic!("write {}: {e}", out.display()));
    println!("\nwrote {}", out.canonicalize().unwrap_or(out).display());
}
