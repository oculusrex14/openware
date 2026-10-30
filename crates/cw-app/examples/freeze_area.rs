//! Regenerate `tests/golden/area/domain.json` from the frozen dbf corpora.
//!
//! One fixture for the seven tables the P5.E–P5.H domain gates read (`AM`, `BL`,
//! `GL`, `MP`, `PR`, `SH`, `TP`) across all three corpora, so the gates pin one
//! census instead of each carrying private constants. Schema mirror of the gate
//! assertions: per table `records/live/deleted` plus the table-specific
//! population census (`MP` schemes + assignment + lead-schedule link, `TP` code
//! censuses + reference audit, `SH` signoff ladder, `BL` per-slice consolidation,
//! `AM` chart kinds, `GL` journal census).
//!
//! # Provenance
//!
//! **This fixture is a kernel-derived regression pin, not an independent
//! oracle.** Every leaf is `cw_app`'s own decode of the frozen bytes under
//! `tests/golden/dbf/<corpus>` (the C1 freeze produced by
//! `cw-core/examples/freeze_roundtrip.rs`). The gates therefore pin the domain
//! models against their own frozen census — a regression/coverage law. Where a
//! value is fixed by an external fact it is asserted **inline in the gate**
//! instead, and this fixture never overrides it:
//!
//! 1. **spec/format constants** — physical schemas, record lengths, column
//!    widths. These come from the shipped type-library headers (`cw_core::dbf`,
//!    `inventory/dbf-schema.tsv`) and are asserted in the gates as constants.
//! 2. **reference-decoder frontier** — the `DAMAGED` table in
//!    `cw_core/tests/guid_law_gate.rs`, derived from liblzma/xz 5.8.4 aborting
//!    mid-stream. Frozen here as `frontier` per table.
//! 3. **engine self-freeze** — everything else (this file).
//!
//! # The assertion frontier
//!
//! 12 of the 66 frozen tables carry a fabricated tail: their `.dbf` zip member
//! is a truncated LZMA stream whose reference decode stops mid-stream, and
//! `cw_core::lzma`'s `RangeDec` zero-fills past end-of-buffer, so the tail is
//! synthesis rather than corpus data. `F63`.
//!
//! A table's `frontier` is the largest `N` such that rows `[0, N)` are both
//! **readable** under the guid law and **decodable** by the domain model:
//!
//! ```text
//! frontier(T) = min(guid_law_frontier(T), maximal_decodable_prefix(T))
//! ```
//!
//! The first term is the reference-decoder evidence (`DAMAGED`); the second is
//! measured here by bisection over "does the model accept rows [0, N)?" and is
//! printed on every run so a moved boundary is visible. Both are compared, not
//! searched, by the gates: the pinned value is what keeps the asserted domain
//! fixed. `MP` is the only table where the two differ (hull2024: readable to 785,
//! decodable to 786; mwgh2025: readable to 787, decodable to 785), because a
//! record can hold a well-formed guid while its numeric cells are already
//! half-shifted.
//!
//! Consequences the gates state at each assertion:
//!
//! * `corp2026`'s `TP` frontier (2,299 of 22,150 records) is **100% tombstones**;
//!   its 19,805 "live" `TP` rows all live in the fabricated tail, so no tax-code
//!   assertion is possible for that corpus.
//! * `MP.GROUP1` (the lead-schedule link) is populated only inside the fabricated
//!   tails: zero authentic-prefix rows in all three corpora.
//! * `AM.GROUPn` assignments resolve only where the target scheme is authentic:
//!   `mwgh2025` scheme 8 (279 rows) yes; `hull2024` slots 2 and 9 (213 + 340 rows)
//!   point at schemes that exist only in its tail, so every one of them is an
//!   `UnknownTarget` residual.
//! * `hull2024`'s authentic `BL` prefix carries no `YEAR='0'`/`GROUP='A'` slice;
//!   its year-0 data is `GROUP='T'`, and years 1..4 are `A`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cw_app::accounts::Chart;
use cw_app::adjustments::Adjustments;
use cw_app::consolidation::consolidate;
use cw_app::entities::{Org, Year};
use cw_app::groups::{
    assignments, dangling_lead_schedules, lead_schedule_codes, mp_schema_drift,
    mp_schema_matches, unassigned_rows, MapNode, MpTable, NodeKind, ASSIGNABLE_DIGITS,
};
use cw_app::signoff::SignoffBook;
use cw_app::tax::{account_tax_issues, mapping_tax_issues, TaxExportTable};
use cw_core::cube::Cube;
use cw_core::dbf::Table;

/// The three frozen corpora (the C1 dbf freeze).
const CORPORA: [(&str, &str); 3] = [
    ("corp2026", "USI Compilation Training(1155962)"),
    ("hull2024", "2024 Hull Lift Truck Review(1148441)"),
    ("mwgh2025", "MWGH LLC 9-30-25 REVIEW(1239263)"),
];

/// Reference-decoder readable frontiers (`guid_law_gate::DAMAGED`); a table
/// absent from that table is clean end to end and is bounded by its own record
/// count. `usize::MAX` means "no damage".
const GUID_FRONTIER: [(&&str, &str, usize); 12] = [
    (&&"corp2026", "HI", 1510),
    (&&"corp2026", "MP", 786),
    (&&"corp2026", "TP", 2299),
    (&&"hull2024", "BL", 1918),
    (&&"hull2024", "HI", 1346),
    (&&"hull2024", "MP", 785),
    (&&"hull2024", "SH", 603),
    (&&"hull2024", "TP", 2184),
    (&&"mwgh2025", "BL", 1894),
    (&&"mwgh2025", "HI", 1405),
    (&&"mwgh2025", "MP", 787),
    (&&"mwgh2025", "TP", 2305),
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn golden_dbf() -> PathBuf {
    root().join("tests").join("golden").join("dbf")
}

/// Case-insensitive table lookup inside one corpus (`mwgh2025` mixes case).
fn open(tag: &str, suffix: &str) -> Table {
    let dir = golden_dbf().join(tag);
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

fn guid_frontier(tag: &str, table: &str) -> Option<usize> {
    GUID_FRONTIER
        .iter()
        .find(|(c, t, _)| **c == tag && *t == table)
        .map(|(_, _, n)| *n)
}

/// Rows `[0, n)` of a table, keeping the header verbatim.
fn prefix(t: &Table, n: usize) -> Table {
    let mut c = t.clone();
    c.records.truncate(n.min(t.count()));
    c
}

fn live(t: &Table) -> usize {
    (0..t.count()).filter(|&r| !t.is_deleted(r)).count()
}

/// Largest prefix the model accepts, by bisection (the predicate is monotone:
/// appending a row can only add a decode failure).
fn decodable_prefix(t: &Table, ok: impl Fn(&Table) -> bool) -> usize {
    if ok(t) {
        return t.count();
    }
    let (mut lo, mut hi) = (0usize, t.count());
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        if ok(&prefix(t, mid)) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

// ---------------------------------------------------------------------------
// JSON emission (the workspace is dependency-free; the other freezers hand-roll
// the same minimal writer).
// ---------------------------------------------------------------------------

fn jstr(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || (c as u32) >= 0x7f => {
                out.push_str(&format!("\\u{:04x}", c as u32))
            }
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn fnum(v: f64) -> String {
    if v == 0.0 {
        "0".to_string()
    } else {
        format!("{v}")
    }
}

fn usmap(m: &BTreeMap<String, usize>) -> String {
    let pairs: Vec<String> = m.iter().map(|(k, v)| format!("{}: {v}", jstr(k))).collect();
    format!("{{{}}}", pairs.join(", "))
}

fn triples(rows: &[(String, String, usize)]) -> String {
    let items: Vec<String> = rows
        .iter()
        .map(|(a, b, n)| format!("[{}, {}, {n}]", jstr(a), jstr(b)))
        .collect();
    format!("[{}]", items.join(", "))
}

// ---------------------------------------------------------------------------
// Per-table censuses
// ---------------------------------------------------------------------------

fn am_json(tag: &str) -> String {
    let t = open(tag, "AM.dbf");
    let chart = Chart::from_table(&t).unwrap_or_else(|e| panic!("{tag} AM: {e}"));
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for a in chart.accounts() {
        let key = match a.kind {
            cw_app::accounts::AccountKind::BalanceSheet => "BalanceSheet",
            cw_app::accounts::AccountKind::Income => "Income",
            cw_app::accounts::AccountKind::Memo => "Memo",
            cw_app::accounts::AccountKind::Other(_) => "Other",
        };
        *kinds.entry(key.to_string()).or_insert(0) += 1;
    }
    format!(
        "{{\"records\": {}, \"live\": {}, \"deleted\": {}, \"fields\": {}, \
         \"record_len\": {}, \"posting\": {}, \"numbers\": {}, \"kinds\": {}, \
         \"issues\": {}}}",
        t.count(),
        live(&t),
        t.count() - live(&t),
        t.fields.len(),
        t.record_len,
        chart.posting().count(),
        chart.numbers().count(),
        usmap(&kinds),
        chart.issues().len(),
    )
}

fn gl_json(tag: &str) -> String {
    let t = open(tag, "GL.dbf");
    let j = Adjustments::from_table(&t).unwrap_or_else(|e| panic!("{tag} GL: {e}"));
    format!(
        "{{\"records\": {}, \"live\": {}, \"deleted\": {}, \"entries\": {}, \
         \"lines\": {}, \"rows\": {}, \"issues\": {}, \"unbalanced\": {}}}",
        t.count(),
        live(&t),
        t.count() - live(&t),
        j.len(),
        j.line_count(),
        j.entries().iter().map(|e| e.row_count()).sum::<usize>(),
        j.issues().len(),
        j.unbalanced().count(),
    )
}

fn mp_json(tag: &str) -> String {
    let full = open(tag, "MP.dbf");
    let decodable = decodable_prefix(&full, |c| MpTable::from_table(c).is_ok());
    let frontier = guid_frontier(tag, "MP").map_or(decodable, |n| n.min(decodable));
    let t = prefix(&full, frontier);
    let mp = MpTable::from_table(&t).unwrap_or_else(|e| panic!("{tag} MP prefix {frontier}: {e}"));
    let am = open(tag, "AM.dbf");

    // per-scheme census, keyed by the scheme letter
    let mut schemes: Vec<(String, String)> = Vec::new();
    for s in mp.scheme_keys() {
        let sc = mp.scheme(s).unwrap();
        let count = |k: NodeKind| sc.coded().filter(|n| n.kind == k).count();
        let other = sc
            .coded()
            .filter(|n| matches!(n.kind, NodeKind::Other(_)))
            .count();
        schemes.push((
            s.as_str(),
            format!(
                "{{\"nodes\": {}, \"coded\": {}, \"blank\": {}, \"walk\": {}, \
                 \"roots\": {}, \"title\": {}, \"filtered\": {}, \"calculated\": {}, \
                 \"unassignable\": {}, \"other\": {}}}",
                sc.len(),
                sc.coded().count(),
                sc.nodes().iter().filter(|n| !n.has_code()).count(),
                sc.walk().len(),
                sc.roots().len(),
                count(NodeKind::Title),
                count(NodeKind::Filtered),
                count(NodeKind::Calculated),
                count(NodeKind::Unassignable),
                other,
            ),
        ));
    }
    let schemes = format!(
        "{{{}}}",
        schemes
            .iter()
            .map(|(k, v)| format!("{}: {v}", jstr(k)))
            .collect::<Vec<_>>()
            .join(", ")
    );

    let mut issues: BTreeMap<(String, String), usize> = BTreeMap::new();
    for i in mp.issues() {
        *issues
            .entry((i.scheme.to_string(), i.kind.to_string()))
            .or_insert(0) += 1;
    }
    let issues: Vec<(String, String, usize)> = issues
        .into_iter()
        .map(|((s, k), n)| (s, k, n))
        .collect();

    let mut assigned: BTreeMap<String, usize> = BTreeMap::new();
    for d in ASSIGNABLE_DIGITS {
        let n = assignments(&am, d).len();
        if n > 0 {
            assigned.insert(d.to_string(), n);
        }
    }
    let mut vacant: BTreeMap<String, usize> = BTreeMap::new();
    for d in ASSIGNABLE_DIGITS {
        let n = unassigned_rows(&am, d).len();
        if n > 0 {
            vacant.insert(d.to_string(), n);
        }
    }
    let mut assign_issues: BTreeMap<(String, String), usize> = BTreeMap::new();
    for i in mp.assignment_issues(&am) {
        *assign_issues
            .entry((i.digit.to_string(), i.kind.to_string()))
            .or_insert(0) += 1;
    }
    let assign_issues: Vec<(String, String, usize)> = assign_issues
        .into_iter()
        .map(|((d, k), n)| (d, k, n))
        .collect();

    // CWGUID widths over the frontier's live rows (the guid law's own witness).
    let mut guids: BTreeMap<String, usize> = BTreeMap::new();
    for r in 0..t.count() {
        if t.is_deleted(r) {
            continue;
        }
        let n = MapNode::from_table(&t, r).unwrap();
        *guids.entry(n.cwguid.chars().count().to_string()).or_insert(0) += 1;
    }

    let schedule_cells = (0..am.count())
        .filter(|&r| {
            !am.is_deleted(r)
                && !am
                    .cell_str(r, "SCHEDULE")
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
        })
        .count();

    let dangling = dangling_lead_schedules(&am, &mp).len();
    let group1_nodes = mp
        .schemes()
        .flat_map(|s| s.nodes().iter())
        .filter(|n| !n.lead_schedule.trim().is_empty())
        .count();

    format!(
        "{{\"records\": {}, \"decodable\": {}, \"frontier\": {}, \"live\": {}, \
         \"deleted\": {}, \"unassigned\": {}, \"schemes\": {schemes}, \
         \"lead_codes\": {}, \"group1_nodes\": {group1_nodes}, \"dangling\": {dangling}, \
         \"schedule_cells\": {schedule_cells}, \"assignment\": {}, \
         \"unassigned_rows\": {}, \"assignment_issues\": {ai}, \"guid_widths\": {gw}, \
         \"issues\": {iss}, \"mapping\": {}, \"digit2\": {}, \"digit9\": {}, \
         \"schema_matches\": {}, \"schema_drift\": {}}}",
        full.count(),
        decodable,
        frontier,
        t.count() - mp.deleted(),
        mp.deleted(),
        mp.unassigned().len(),
        lead_schedule_codes(&mp).len(),
        usmap(&assigned),
        usmap(&vacant),
        mp.mapping().is_some(),
        mp.digit(2).is_some(),
        mp.digit(9).is_some(),
        mp_schema_matches(&t),
        mp_schema_drift(&t).len(),
        ai = triples(&assign_issues),
        gw = usmap(&guids),
        iss = triples(&issues),
    )
}

fn tp_json(tag: &str) -> String {
    let full = open(tag, "TP.dbf");
    let decodable = decodable_prefix(&full, |t| TaxExportTable::from_table(t).is_ok());
    let frontier = guid_frontier(tag, "TP").map_or(decodable, |n| n.min(decodable));
    let t = prefix(&full, frontier);
    let tp = TaxExportTable::from_table(&t).unwrap_or_else(|e| panic!("{tag} TP prefix {frontier}: {e}"));

    let mut udef: BTreeMap<String, usize> = BTreeMap::new();
    for r in tp.rows() {
        *udef.entry(r.user_defined.raw().to_string()).or_insert(0) += 1;
    }
    let mut types: BTreeMap<String, usize> = BTreeMap::new();
    for r in tp.rows() {
        *types.entry(r.row_type.clone()).or_insert(0) += 1;
    }

    // The `01A` row the gate projects the COM `Get` onto.
    let first = tp.get("01A");
    let first = match first {
        Some(r) => format!(
            "{{\"row\": {}, \"form\": {}, \"entity\": {}, \"year\": {}, \
             \"detail\": {}, \"percent\": {}, \"row_type\": {}, \"guid\": {}}}",
            r.row,
            jstr(&r.form),
            jstr(&r.entity),
            jstr(&r.year),
            jstr(&r.detail),
            fnum(r.percent),
            jstr(&r.row_type),
            jstr(&r.cwguid),
        ),
        None => "null".to_string(),
    };

    // Widest scope fan-out in the table (the gate's `all_by_code` census).
    let mut max_code = String::new();
    let mut max_scopes = 0usize;
    for code in tp.codes() {
        let n = tp.all_by_code(&code).len();
        if n > max_scopes {
            max_scopes = n;
            max_code = code;
        }
    }

    let am = open(tag, "AM.dbf");
    let mut refs = std::collections::BTreeSet::new();
    for r in 0..am.count() {
        if am.is_deleted(r) {
            continue;
        }
        let v = am.cell_str(r, "TAC_NO").unwrap_or_default();
        let v = v.trim_end().to_string();
        if !v.trim().is_empty() {
            refs.insert(v);
        }
    }
    let missing = refs.iter().filter(|c| tp.get(c).is_none()).count();

    let mp_frontier = {
        let full_mp = open(tag, "MP.dbf");
        let dec = decodable_prefix(&full_mp, |c| MpTable::from_table(c).is_ok());
        guid_frontier(tag, "MP").map_or(dec, |n| n.min(dec))
    };
    let mp = prefix(&open(tag, "MP.dbf"), mp_frontier);

    format!(
        "{{\"records\": {}, \"decodable\": {}, \"frontier\": {}, \"live\": {}, \
         \"deleted\": {}, \"codes\": {}, \"udef\": {}, \"types\": {}, \"first_01a\": {first}, \
         \"max_scopes\": {}, \"max_scopes_code\": {}, \"am_refs\": {}, \
         \"am_refs_missing\": {missing}, \"am_issues\": {}, \"mp_slots\": {}, \
         \"mp_issues\": {}}}",
        full.count(),
        decodable,
        frontier,
        tp.len(),
        tp.deleted(),
        tp.codes().len(),
        usmap(&udef),
        usmap(&types),
        max_scopes,
        jstr(&max_code),
        refs.len(),
        account_tax_issues(&am, &tp).len(),
        (0..mp.count())
            .filter(|&r| !mp.is_deleted(r))
            .map(|r| (0..=4)
                .filter(|i| {
                    !mp.cell_str(r, &format!("TAX{i}"))
                        .unwrap_or_default()
                        .trim()
                        .is_empty()
                })
                .count())
            .sum::<usize>(),
        mapping_tax_issues(&mp, &tp).len(),
    )
}

fn sh_json(tag: &str) -> String {
    let full = open(tag, "SH.dbf");
    let frontier = guid_frontier(tag, "SH").unwrap_or(full.count());
    let t = prefix(&full, frontier);
    let book = SignoffBook::from_table(t.clone())
        .unwrap_or_else(|e| panic!("{tag} SH prefix {frontier}: {e}"));
    let c = book.census();
    let w = book.wpg_census();

    // Ladder census straight from `SH`. A level counts as signed when either COM
    // pair (`A{n}` initials or `AD{n}` date) is populated.
    //
    // Two hull2024 rows inside the readable prefix carry *fabricated ladder cells*:
    // their flag/date/guid columns still pass the readable test while `A5`/`AD5`/
    // `AC8`/`AD8` hold shifted text (`AD5='VGssJex/'`, `AD8="'Uni0B0"`, `AC8` with
    // `0x97 0x91`). They are excluded from the histograms (`excluded_rows`) and
    // are the reason the pre-amendment "deepest L8" reading was not evidence.
    let mut per_level = [0usize; 8];
    let mut deepest = [0usize; 9];
    let mut clean_per_level = [0usize; 8];
    let mut clean_deepest = [0usize; 9];
    let mut excluded: Vec<usize> = Vec::new();
    let mut ac_pop = 0usize;
    let mut au_pop = 0usize;
    // Companion-slot shape, counted over clean rows only: `AC` is all-populated or
    // all-blank *per document* (never mixed across one row's signed levels); `AU`
    // is populated on every signed level of every signed document.
    let mut ac_rows_full = 0usize;
    let mut ac_rows_blank = 0usize;
    let mut au_rows_full = 0usize;
    // `S{n}` cells: the family is an *offset mirror* of `AU`, not a parallel ladder,
    // so `S{n}` names the user who signed level `n+1`. Counted on clean rows only:
    // the fabricated rows contribute NUL-filled `S`/`SD` cells that would otherwise
    // inflate the census and the no-match bucket.
    let mut s_cells = 0usize;
    let mut s_cells_all = 0usize;
    let mut s_mirror_next = 0usize;
    let mut s_same_level = 0usize;
    let mut s_no_match = 0usize;

    for r in 0..t.count() {
        if t.is_deleted(r) {
            continue;
        }
        let signed_at = |l: u8| {
            let a = t.cell_str(r, &format!("A{l}")).unwrap_or_default();
            let ad = t.cell_str(r, &format!("AD{l}")).unwrap_or_default();
            !a.trim().is_empty() || !ad.trim().is_empty()
        };

        // A row is clean when every populated ladder cell sits in its declared
        // width family: `A`<=8, `AD`=8 digits, `AC`=22, `AU`<=7, all printable
        // ASCII (`S`/`SD` similarly). Anything else is fabricated payload.
        let mut clean = true;
        for l in 1..=8u8 {
            for (col, w) in [("A", 8usize), ("AC", 22), ("AU", 7), ("S", 20)] {
                let v = t.cell_str(r, &format!("{col}{l}")).unwrap_or_default();
                if !v.trim().is_empty() && (!v.trim().is_ascii() || v.trim().chars().count() > w) {
                    clean = false;
                }
            }
            let ad = t.cell_str(r, &format!("AD{l}")).unwrap_or_default();
            if !ad.trim().is_empty()
                && !(ad.trim().len() == 8 && ad.trim().bytes().all(|b| b.is_ascii_digit()))
            {
                clean = false;
            }
            let sd = t.cell_str(r, &format!("SD{l}")).unwrap_or_default();
            if !sd.trim().is_empty()
                && !(sd.trim().len() == 8 && sd.trim().bytes().all(|b| b.is_ascii_digit()))
            {
                clean = false;
            }
        }
        if !clean {
            excluded.push(r);
        }

        let mut d = 0usize;
        let (mut ac_here, mut ac_blank_here, mut au_here, mut au_blank_here) = (0usize, 0usize, 0usize, 0usize);
        for l in 1..=8u8 {
            if !signed_at(l) {
                continue;
            }
            if t
                .cell_str(r, &format!("AC{l}"))
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                ac_blank_here += 1;
            } else {
                ac_here += 1;
            }
            if t
                .cell_str(r, &format!("AU{l}"))
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                au_blank_here += 1;
            } else {
                au_here += 1;
            }
            per_level[(l - 1) as usize] += 1;
            if clean {
                clean_per_level[(l - 1) as usize] += 1;
            }
            d = l as usize;

            if clean {
                let ac = t.cell_str(r, &format!("AC{l}")).unwrap_or_default();
                let au = t.cell_str(r, &format!("AU{l}")).unwrap_or_default();
                if !ac.trim().is_empty() {
                    ac_pop += 1;
                }
                if !au.trim().is_empty() {
                    au_pop += 1;
                }
            }
        }
        if d > 0 {
            deepest[d] += 1;
            if clean {
                clean_deepest[d] += 1;
            }
            if clean {
                if ac_here > 0 && ac_blank_here == 0 {
                    ac_rows_full += 1;
                } else if ac_here == 0 {
                    ac_rows_blank += 1;
                }
                if au_here > 0 && au_blank_here == 0 {
                    au_rows_full += 1;
                }
            }
        }

        for l in 1..=8u8 {
            let sv = t.cell_str(r, &format!("S{l}")).unwrap_or_default();
            let sv = sv.trim();
            if sv.is_empty() {
                continue;
            }
            s_cells_all += 1;
            if !clean {
                continue;
            }
            s_cells += 1;
            let same = t.cell_str(r, &format!("AU{l}")).unwrap_or_default();
            let next = if l < 8 {
                t.cell_str(r, &format!("AU{}", l + 1)).unwrap_or_default()
            } else {
                String::new()
            };
            if !next.trim().is_empty() && sv.eq_ignore_ascii_case(next.trim()) {
                s_mirror_next += 1;
            } else if !same.trim().is_empty() && sv.eq_ignore_ascii_case(same.trim()) {
                s_same_level += 1;
            } else {
                s_no_match += 1;
            }
        }
    }

    let levels = |a: [usize; 8]| {
        format!(
            "[{}]",
            a.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", ")
        )
    };
    let deepest_json = |a: [usize; 9]| {
        format!(
            "[{}]",
            a[1..].iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", ")
        )
    };
    let excluded_json = format!(
        "[{}]",
        excluded.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(", ")
    );

    format!(
        "{{\"records\": {}, \"frontier\": {}, \"live\": {}, \"deleted\": {}, \
         \"census\": {{\"documents\": {}, \"signed\": {}, \"complete\": {}, \"levels\": {}}}, \
         \"wpg\": {{\"documents\": {}, \"signed\": {}, \"levels\": {}}}, \
         \"ladder\": {{\"per_level\": {}, \"deepest\": {}, \"clean_per_level\": {}, \
         \"clean_deepest\": {}, \"excluded_rows\": {excluded_json}, \"ac_populated\": {ac_pop}, \
         \"ac_rows_full\": {ac_rows_full}, \"ac_rows_blank\": {ac_rows_blank}, \
         \"au_populated\": {au_pop}, \"au_rows_full\": {au_rows_full}}}, \
         \"s_family\": {{\"cells\": {s_cells}, \"cells_with_fabricated\": {s_cells_all}, \
         \"mirror_next_level\": {s_mirror_next}, \"same_level\": {s_same_level}, \
         \"no_match\": {s_no_match}}}}}",
        full.count(),
        frontier,
        book.count(),
        t.count() - book.count(),
        c.documents,
        c.signed_documents,
        c.complete_documents,
        levels(c.level_counts),
        w.documents,
        w.signed_documents,
        levels(w.level_counts),
        levels(per_level),
        deepest_json(deepest),
        levels(clean_per_level),
        deepest_json(clean_deepest),
    )
}

fn bl_json(tag: &str) -> String {
    let full = open(tag, "BL.dbf");
    let frontier = guid_frontier(tag, "BL").unwrap_or(full.count());
    let t = prefix(&full, frontier);
    let cube = Cube::from_table(&t).unwrap_or_else(|e| panic!("{tag} BL prefix {frontier}: {e}"));

    // every (year, group) slice present in the authentic prefix
    let mut slices: BTreeMap<(i32, String), usize> = BTreeMap::new();
    for r in cube.rows() {
        *slices.entry((r.year, r.group.clone())).or_insert(0) += 1;
    }
    let year_of = |y: i32| match y {
        0 => Some(Year::Current),
        1 => Some(Year::Prior1),
        2 => Some(Year::Prior2),
        3 => Some(Year::Prior3),
        4 => Some(Year::Prior4),
        5 => Some(Year::Budget),
        6 => Some(Year::Forecast),
        _ => None,
    };
    let mut parts: Vec<String> = Vec::new();
    for ((year, group), rows) in &slices {
        let Some(yr) = year_of(*year) else { continue };
        let nums: std::collections::BTreeSet<&str> = cube
            .rows()
            .iter()
            .filter(|r| r.year == *year && &r.group == group)
            .map(|r| r.number.as_str())
            .collect();
        let cons = consolidate(&cube, &Org::default(), yr, group);
        let mismatch = cons
            .accounts
            .iter()
            .filter(|(n, a)| {
                (a.total - cube.balance(n, *year, group).unwrap_or(f64::NAN)).abs() > 1e-6
            })
            .count();
        parts.push(format!(
            "{}: {{\"rows\": {rows}, \"numbers\": {}, \"consolidated\": {}, \
             \"mismatch\": {mismatch}, \"checksum\": {}}}",
            jstr(&format!("{year}|{group}")),
            nums.len(),
            cons.accounts.len(),
            jstr(&cons.checksum()),
        ));
    }
    format!(
        "{{\"records\": {}, \"frontier\": {}, \"rows\": {}, \"slices\": {{{}}}}}",
        full.count(),
        frontier,
        cube.rows().len(),
        parts.join(", ")
    )
}

fn pr_json(tag: &str) -> String {
    let t = open(tag, "PR.dbf");
    format!(
        "{{\"records\": {}, \"live\": {}, \"deleted\": {}}}",
        t.count(),
        live(&t),
        t.count() - live(&t),
    )
}

fn corpus_json(tag: &str) -> String {
    format!(
        "{{\n      \"am\": {},\n      \"bl\": {},\n      \"gl\": {},\n      \
         \"mp\": {},\n      \"pr\": {},\n      \"sh\": {},\n      \"tp\": {}\n    }}",
        am_json(tag),
        bl_json(tag),
        gl_json(tag),
        mp_json(tag),
        pr_json(tag),
        sh_json(tag),
        tp_json(tag),
    )
}

fn main() {
    println!("area fixture: frontier = min(guid_law readable, model-decodable)\n");
    let mut blocks = Vec::new();
    for (tag, stem) in CORPORA {
        println!("== {tag} ({stem})");
        for (table, suffix) in [
            ("AM", "AM.dbf"),
            ("BL", "BL.dbf"),
            ("GL", "GL.dbf"),
            ("MP", "MP.dbf"),
            ("PR", "PR.dbf"),
            ("SH", "SH.dbf"),
            ("TP", "TP.dbf"),
        ] {
            let t = open(tag, suffix);
            let guid = guid_frontier(tag, table).unwrap_or(t.count());
            let dec = match table {
                "MP" => decodable_prefix(&t, |c| MpTable::from_table(c).is_ok()),
                "TP" => decodable_prefix(&t, |c| TaxExportTable::from_table(c).is_ok()),
                _ => t.count(),
            };
            println!(
                "   {table:>2}: records {:>6}  guid-frontier {:>6}  decodable {:>6}  asserted {:>6}  live {:>6}",
                t.count(),
                guid,
                dec,
                guid.min(dec),
                live(&t),
            );
        }
        blocks.push(format!("    {}: {}", jstr(tag), corpus_json(tag)));
    }

    let doc = format!("{{\n  \"corpora\": {{\n{}\n  }}\n}}\n", blocks.join(",\n"));
    let out_dir = root().join("tests").join("golden").join("area");
    std::fs::create_dir_all(&out_dir).unwrap();
    let path = out_dir.join("domain.json");
    std::fs::write(&path, &doc).unwrap();
    println!("\nwrote {} ({} bytes)", path.display(), doc.len());
}
