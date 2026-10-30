//! Shared fixture access for the P5.E–P5.H domain gates.
//!
//! Every gate in this area reads the same two things:
//!
//! 1. the frozen dbf corpora (`tests/golden/dbf/<corpus>/`, the C1 freeze), and
//! 2. the area census (`tests/golden/area/domain.json`, produced by
//!    `examples/freeze_area.rs`).
//!
//! The fixture is a **kernel-derived regression pin** (see that file's header):
//! spec/format constants are asserted inline in each gate as literal numbers,
//! and the fixture only pins the population censuses the models derive from the
//! frozen bytes. The readable-frontier constants below are the reference-decoder
//! evidence from `cw_core/tests/guid_law_gate.rs` (`DAMAGED`), restated here so
//! each gate can assert the domain it is allowed to talk about instead of
//! silently shrinking to nothing.
//!
//! ## F63: why a frontier exists at all
//!
//! Twelve of the 66 frozen tables carry a **fabricated tail**. Their `.dbf` zip
//! member is a method-14 LZMA stream damaged in the shipped `.ac_`; the reference
//! decoder (xz 5.8.4, raw LZMA1 lc3/lp0/pb2 from byte 19) aborts mid-stream, so
//! everything past the last authentic record is synthesis produced by
//! `cw_core::lzma`'s zero-fill. The gates therefore assert censuses over
//! `rows[..frontier]` and only laws that hold there.
//!
//! A frontier is `min(reference-readable rows, model-decodable rows)`. The
//! second term matters for `MP`, whose rows can hold a well-formed `CWGUID`
//! while their numeric cells are already half-shifted.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cw_core::dbf::Table;
use cw_core::json::Json;

/// The three frozen corpora and their on-disk engagement stems.
pub const CORPORA: [(&str, &str); 3] = [
    ("corp2026", "USI Compilation Training(1155962)"),
    ("hull2024", "2024 Hull Lift Truck Review(1148441)"),
    ("mwgh2025", "MWGH LLC 9-30-25 REVIEW(1239263)"),
];

/// Assertion frontier per `(corpus, table)`, from `guid_law_gate::DAMAGED`
/// (reference-decoder evidence) capped by the model's maximal decodable prefix.
///
/// Tables absent here are authentic end to end and are asserted in full. The
/// `MP` rows are the only place the two terms disagree; the smaller value wins
/// so a row is never asserted on either ground.
pub const FRONTIER: [(&str, &str, usize); 12] = [
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
    ("mwgh2025", "MP", 785),
    ("mwgh2025", "TP", 2305),
];

pub fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

pub fn golden() -> PathBuf {
    repo().join("tests/golden")
}

pub fn dbf_root() -> PathBuf {
    golden().join("dbf")
}

/// The area census fixture (`examples/freeze_area.rs`).
pub fn fixture() -> Json {
    let path = golden().join("area").join("domain.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {path:?}: {e} (run `cargo run -p cw-app --example freeze_area`)"));
    cw_core::json::parse(&text).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

/// The per-corpus object of [`fixture`].
pub fn corpus_fixture(tag: &str) -> Json {
    fixture()
        .get("corpora")
        .and_then(|c| c.get(tag))
        .unwrap_or_else(|| panic!("fixture has no corpus {tag}"))
        .clone()
}

/// The per-table object of [`corpus_fixture`].
pub fn table_fixture(tag: &str, table: &str) -> Json {
    corpus_fixture(tag)
        .get(table)
        .unwrap_or_else(|| panic!("fixture has no {tag}/{table}"))
        .clone()
}

/// Case-insensitive table lookup inside one corpus (`mwgh2025` mixes case).
pub fn open(tag: &str, suffix: &str) -> Table {
    let dir = dbf_root().join(tag);
    let want = suffix.to_ascii_lowercase();
    let path = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {dir:?}: {e}"))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase().ends_with(&want))
                .unwrap_or(false)
        })
        .unwrap_or_else(|| panic!("no *{suffix} in {dir:?}"));
    Table::parse_path(&path).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

/// Rows `[0, n)` of a table, header verbatim — the F63 assertion domain.
pub fn prefix(t: &Table, n: usize) -> Table {
    let mut c = t.clone();
    c.records.truncate(n.min(t.count()));
    c
}

/// The pinned assertion frontier for a table (whole table when undamaged).
pub fn frontier(tag: &str, table: &str, t: &Table) -> usize {
    FRONTIER
        .iter()
        .find(|(c, k, _)| *c == tag && *k == table)
        .map(|(_, _, n)| *n)
        .unwrap_or(t.count())
}

/// Live (non-tombstoned) rows in a table.
pub fn live(t: &Table) -> usize {
    (0..t.count()).filter(|&r| !t.is_deleted(r)).count()
}

// --- JSON readers -----------------------------------------------------------

pub fn usize_at(j: &Json, key: &str) -> usize {
    j.get(key)
        .and_then(|x| x.as_f64())
        .unwrap_or_else(|| panic!("missing usize {key} in {j:?}")) as usize
}

pub fn i64_at(j: &Json, key: &str) -> i64 {
    j.get(key)
        .and_then(|x| x.as_f64())
        .unwrap_or_else(|| panic!("missing number {key}")) as i64
}

pub fn f64_at(j: &Json, key: &str) -> f64 {
    j.get(key)
        .and_then(|x| x.as_f64())
        .unwrap_or_else(|| panic!("missing number {key}"))
}

pub fn str_at<'a>(j: &'a Json, key: &str) -> &'a str {
    j.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or_else(|| panic!("missing string {key}"))
}

pub fn map_at(j: &Json, key: &str) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    if let Some(pairs) = j.get(key).and_then(|x| x.as_obj()) {
        for (k, v) in pairs {
            out.insert(k.clone(), v.as_f64().unwrap_or(0.0) as usize);
        }
    }
    out
}

/// A `[a, b, n]` triple list flattened to `(a, b) -> n`.
pub fn triples_at(j: &Json, key: &str) -> BTreeMap<(String, String), usize> {
    let mut out = BTreeMap::new();
    if let Some(items) = j.get(key).and_then(|x| x.as_arr()) {
        for item in items {
            let a = item
                .as_arr()
                .and_then(|p| p.first())
                .and_then(|x| x.as_str())
                .expect("triple head")
                .to_string();
            let b = item
                .as_arr()
                .and_then(|p| p.get(1))
                .and_then(|x| x.as_str())
                .expect("triple tail")
                .to_string();
            let n = item
                .as_arr()
                .and_then(|p| p.get(2))
                .and_then(|x| x.as_f64())
                .expect("triple count") as usize;
            out.insert((a, b), n);
        }
    }
    out
}

/// A `[u8; 8]` census array.
pub fn levels_at(j: &Json, key: &str) -> Vec<usize> {
    j.get(key)
        .and_then(|x| x.as_arr())
        .unwrap_or_else(|| panic!("missing levels array {key}"))
        .iter()
        .map(|v| v.as_f64().expect("level count") as usize)
        .collect()
}
