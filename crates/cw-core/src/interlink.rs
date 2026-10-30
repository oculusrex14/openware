//! The interlink resolver — P5.C3 (F32, F60).
//!
//! CaseWare wires every object together through three fabrics; this module
//! indexes and audits all of them over a loaded engagement directory:
//!
//! * **GUID web** — every core row's `CWGUID` is the object key; link columns
//!   (`GL.LINKGUID`, `AN.DOCUMENTID`, `PR/WR/WT.DOCGUID`, `TS.DOCID`, ...) hold
//!   22-char ids resolved against the live-row index, then the deleted-row
//!   (tombstone) index. Unresolved edges are CW's own danglers: counted,
//!   never rewritten (guid.md §4 policy).
//! * **Typed anchors** — `(OBJECTTYPE,OBJECTID)` pairs (AN/TS) resolve inside
//!   the named table's guid set (live + tombstones); `**` = global, no target.
//! * **Numeric web** — MP parentage is the `MAP_NO` dotted-prefix (`8300` →
//!   `8300.02`; `MP.ID` itself is a 1-char flag, corpus-unstable — F60);
//!   `AM.PAC_NO` → `AC_NO`; `AM.GROUPn(CR)` → `MP.MAP_NO` (F36); `BL.NUMBER`
//!   classified liveAM→liveMP→delAM→delMP→tacno→synthetic, first hit.
//! * **CV/FP feed** — `CV` keyed (FORM,ID); `FP` = (WKS,CELLNO) × SEQNO →
//!   literal TEXT pushed into CaseView linked sheets.
//!
//! Port contract (Plan.md §10): every counter here must equal the census frozen
//! in `tests\golden\interlink\expectations.json`. Fixture provenance: the
//! original freeze was a Python oracle (two independent parsers, one truth);
//! that oracle is not part of the rebuild, so the fixture is kernel-derived
//! (`examples/freeze_interlink.rs`) and the gate is a regression/coverage law
//! over frozen bytes. Golden gate: `cw-core\tests\web_parity.rs`.

use crate::dbf::Table;
use std::collections::BTreeMap;
use std::path::Path;

/// Guid-web edge registry: `(table, column)`. Columns absent from a corpus
/// (or empty tables) contribute zeroed stats — the audit still records them.
pub const GUID_EDGES: &[(&str, &str)] = &[
    ("GL", "LINKGUID"),
    ("GL", "LINK2GUID"),
    ("GL", "ITEMGUID"),
    ("SH", "SRCGUID"),
    ("SH", "DOCUMENTID"),
    ("PR", "DOCGUID"),
    ("WR", "DOCGUID"),
    ("WT", "DOCGUID"),
    ("AN", "DOCUMENTID"),
    ("TS", "DOCID"),
    ("TS", "FILEID"),
    ("TS", "CREATEDBY"),
    ("TS", "ASSIGNEDTO"),
    ("TS", "CLEAREDBY"),
    ("TS", "COMPLETEBY"),
    ("TS", "HISTORYCRE"),
    ("TS", "HISTORYCOM"),
    ("CV", "OWNER"),
    ("CV", "GROUP"),
    ("CV", "FORM"),
    ("FP", "TEXT"),
    ("GR", "DOCID"),
    ("EC", "TGUID"),
    ("SH", "TASKUSER"),
    ("SH", "AU1"),
    ("SH", "AU2"),
    ("SH", "AU3"),
    ("SH", "AU4"),
    ("SH", "AU5"),
    ("SH", "AU6"),
    ("SH", "AU7"),
    ("SH", "AU8"),
];

/// Typed-object anchors: `(table, type-col, id-col)`.
pub const TYPED_ANCHORS: &[(&str, &str, &str)] = &[
    ("AN", "OBJECTTYPE", "OBJECTID"),
    ("TS", "OBJECTTYPE", "OBJECTID"),
];

/// AM group-assignment columns → `MP.MAP_NO` (F36 assignment law).
const GROUP_COLS: &[&str] = &[
    "GROUP2", "GROUP3", "GROUP4", "GROUP5", "GROUP6", "GROUP7", "GROUP8", "GROUP9", "GROUP0",
    "GROUP2CR", "GROUP3CR", "GROUP4CR", "GROUP5CR", "GROUP6CR", "GROUP7CR", "GROUP8CR",
    "GROUP9CR", "GROUP0CR",
];

/// 22-char CWGUID shape: length plus the allocator alphabet. Family masks are
/// deliberately NOT required — the reader tolerates any id CW wrote (the
/// family law itself is `guid`'s gate).
fn is_shaped(s: &str) -> bool {
    s.len() == crate::guid::LEN
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'-')
}

/// An engagement directory: every `*<ALIAS>.dbf` parsed; alias = trailing
/// ASCII letters of the stem, uppercased (CW names `<engname>(<id>)<TAB>.dbf`).
pub struct Corpus {
    /// `(alias, table)` sorted by alias — stable indices for [`RowRef::table`].
    pub tables: Vec<(String, Table)>,
}

impl Corpus {
    pub fn open_dir(dir: impl AsRef<Path>) -> crate::Result<Corpus> {
        let mut entries: Vec<(String, Table)> = Vec::new();
        for e in std::fs::read_dir(dir.as_ref())? {
            let p = e?.path();
            let is_dbf = p
                .extension()
                .map(|x| x.eq_ignore_ascii_case("dbf"))
                .unwrap_or(false);
            if !is_dbf {
                continue;
            }
            let stem = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let alias = alias_of(&stem);
            if alias.is_empty() {
                continue;
            }
            let table = Table::parse_path(&p)?;
            entries.push((alias, table));
        }
        if entries.is_empty() {
            return Err(crate::CoreError::Format(format!(
                "no dbf tables under {}",
                dir.as_ref().display()
            )));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Corpus { tables: entries })
    }

    pub fn table(&self, alias: &str) -> Option<&Table> {
        let a = alias.to_ascii_uppercase();
        self.tables.iter().find(|(n, _)| *n == a).map(|(_, t)| t)
    }

    fn idx(&self, alias: &str) -> Option<usize> {
        let a = alias.to_ascii_uppercase();
        self.tables.iter().position(|(n, _)| *n == a)
    }

    /// Alias of table index `i`.
    pub fn alias(&self, i: usize) -> &str {
        &self.tables[i].0
    }
}

/// Trailing-ASCII-letters run of a stem, uppercased (`...930)am` → `AM`).
fn alias_of(stem: &str) -> String {
    let n = stem
       .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_alphabetic())
        .last()
        .map(|(i, _)| i)
        // no trailing letters: whole stem when every char is a letter
        .unwrap_or(if stem.chars().all(|c| c.is_ascii_alphabetic()) { 0 } else { stem.len() });
    stem[n..].to_ascii_uppercase()
}

/// Where a resolved object lives: table index into [`Corpus::tables`] plus the
/// physical record number (deletion flags preserved in that numbering).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RowRef {
    pub table: usize,
    pub record: usize,
}

/// Per-edge census. `targets` histograms the index entries of resolved ids by
/// table (an id may exist in several tables; entries count, not distinct ids).
#[derive(Debug, Default, Clone)]
pub struct EdgeStat {
    pub populated: usize,
    pub shaped: usize,
    pub live: usize,
    pub deleted: usize,
    pub unresolved: usize,
    pub targets: BTreeMap<String, usize>,
}

/// Typed-anchor census.
#[derive(Debug, Default, Clone)]
pub struct TypedStat {
    pub anchored: usize,
    pub global: usize,
    pub hit: usize,
    pub miss: usize,
    pub unknown_type: usize,
}

/// Numeric-web census. `bl_number_*` buckets are first-hit, mutually exclusive.
#[derive(Debug, Default, Clone)]
pub struct NumericStat {
    pub am_rows: usize,
    pub am_acno_blank: usize,
    pub pac_populated: usize,
    pub pac_hit: usize,
    pub pac_miss: usize,
    pub mp_rows: usize,
    pub mp_mapno_prefixed: usize,
    pub mp_prefix_parent_miss: usize,
    pub grp_pop: BTreeMap<String, usize>,
    pub grp_miss: BTreeMap<String, usize>,
    pub bl_number_blank: usize,
    pub bl_number_live_am: usize,
    pub bl_number_live_mp: usize,
    pub bl_number_del_am: usize,
    pub bl_number_del_mp: usize,
    pub bl_number_tacno: usize,
    pub bl_number_synthetic: usize,
    pub bl_rows: usize,
    pub bl_entity_foreign_kinds: usize,
    pub bl_entity_foreign_rows: usize,
}

/// CV/FP feed census.
#[derive(Debug, Default, Clone)]
pub struct FeedStat {
    pub cv_rows: usize,
    pub cv_ovr_y: usize,
    pub cv_types: BTreeMap<String, usize>,
    pub cv_id_blank: usize,
    pub cv_id_dupes: usize,
    pub cv_formid_dupes: usize,
    pub fp_rows: usize,
    pub fp_dup_keys: usize,
    pub fp_wks: BTreeMap<String, usize>,
    pub fp_text_guid_shaped: usize,
    pub fp_cells: usize,
}

/// The resolver: guid indexes plus every audit, built once, queried many.
#[derive(Debug, Default)]
pub struct Interlink {
    /// CWGUID → live rows.
    pub index: BTreeMap<String, Vec<RowRef>>,
    /// CWGUID → tombstoned (deleted-flag) rows: still resolvable, flagged.
    pub deleted_index: BTreeMap<String, Vec<RowRef>>,
    /// Live guid-bearing rows per table.
    pub per_table: BTreeMap<String, usize>,
    /// Live ids whose entries span more than one table (corpus: none).
    pub multi_table: usize,
    /// `"TAB.COL"` → census.
    pub edges: BTreeMap<String, EdgeStat>,
    /// `"TAB.TYPECOL->IDCOL"` → census.
    pub typed: BTreeMap<String, TypedStat>,
    pub numeric: NumericStat,
    pub feed: FeedStat,
}

fn cell(table: &Table, r: usize, name: &str) -> Option<String> {
    let raw = table.cell(r, name)?;
    let s: String = crate::canon::latin1(raw).into();
    Some(s.trim().to_string())
}

fn is_live(table: &Table, r: usize) -> bool {
    table.records.get(r).map(|rec| rec[0] == b' ').unwrap_or(false)
}

/// Every CWGUID-shaped value in `col` over `rows`, flag-agnostic.
fn shaped_set(table: &Table, rows: impl Iterator<Item = usize>, col: &str) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    if table.field_index(col).is_some() {
        for r in rows {
            if let Some(v) = cell(table, r, col) {
                if is_shaped(&v) {
                    out.insert(v);
                }
            }
        }
    }
    out
}

fn string_set(table: &Table, rows: &[usize], col: &str) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    if table.field_index(col).is_some() {
        for &r in rows {
            if let Some(v) = cell(table, r, col) {
                if !v.is_empty() {
                    out.insert(v);
                }
            }
        }
    }
    out
}

impl Interlink {
    /// Build every index and audit for `corpus`. Deterministic; equality with
    /// the Python freeze is the golden gate.
    pub fn build(corpus: &Corpus) -> Interlink {
        let mut il = Interlink {
            edges: GUID_EDGES
                .iter()
                .map(|(t, c)| (format!("{t}.{c}"), EdgeStat::default()))
                .collect(),
            typed: TYPED_ANCHORS
                .iter()
                .map(|(t, tc, ic)| (format!("{t}.{tc}->{ic}"), TypedStat::default()))
                .collect(),
            ..Interlink::default()
        };

        // ---------------- guid indexes (live + tombstones)
        for (ti, (alias, table)) in corpus.tables.iter().enumerate() {
            if table.field_index(crate::guid::CWGUID_COL).is_none() {
                continue;
            }
            for r in 0..table.count() {
                let v = match cell(table, r, crate::guid::CWGUID_COL) {
                    Some(v) if is_shaped(&v) => v,
                    _ => continue,
                };
                if is_live(table, r) {
                    il.index
                        .entry(v)
                        .or_default()
                        .push(RowRef { table: ti, record: r });
                    *il.per_table.entry(alias.clone()).or_default() += 1;
                } else if table.is_deleted(r) {
                    il.deleted_index
                        .entry(v)
                        .or_default()
                        .push(RowRef { table: ti, record: r });
                }
            }
        }
        il.multi_table = il
            .index
            .values()
            .filter(|rows| {
                let mut t: Vec<usize> = rows.iter().map(|x| x.table).collect();
                t.sort_unstable();
                t.dedup();
                t.len() > 1
            })
            .count();

        // ---------------- guid-web edges
        for (tab, col) in GUID_EDGES {
            let stat = il.edges.get_mut(&format!("{tab}.{col}")).unwrap();
            let ti = match corpus.idx(tab) {
                Some(i) => i,
                None => continue,
            };
            let table = &corpus.tables[ti].1;
            if table.field_index(col).is_none() {
                continue;
            }
            for r in 0..table.count() {
                if !is_live(table, r) {
                    continue;
                }
                let v = match cell(table, r, col) {
                    Some(v) => v,
                    None => continue,
                };
                if v.is_empty() {
                    continue;
                }
                stat.populated += 1;
                if !is_shaped(&v) {
                    continue;
                }
                stat.shaped += 1;
                if let Some(rows) = il.index.get(&v) {
                    stat.live += 1;
                    for row in rows {
                        *stat.targets.entry(corpus.alias(row.table).to_string()).or_default() += 1;
                    }
                } else if let Some(rows) = il.deleted_index.get(&v) {
                    stat.deleted += 1;
                    for row in rows {
                        *stat.targets.entry(corpus.alias(row.table).to_string()).or_default() += 1;
                    }
                } else {
                    stat.unresolved += 1;
                }
            }
        }

        // ---------------- typed anchors
        for (tab, tcol, icol) in TYPED_ANCHORS {
            let stat = il.typed.get_mut(&format!("{tab}.{tcol}->{icol}")).unwrap();
            let ti = match corpus.idx(tab) {
                Some(i) => i,
                None => continue,
            };
            let table = &corpus.tables[ti].1;
            for r in 0..table.count() {
                if !is_live(table, r) {
                    continue;
                }
                let t = match cell(table, r, tcol) {
                    Some(t) => t.to_ascii_uppercase(),
                    None => continue,
                };
                let v = match cell(table, r, icol) {
                    Some(v) => v,
                    None => continue,
                };
                if t.is_empty() || v.is_empty() {
                    continue;
                }
                stat.anchored += 1;
                if t == "**" {
                    stat.global += 1;
                } else if corpus.idx(&t).is_some() {
                    let tt = corpus.table(&t).unwrap();
                    let targets = shaped_set(tt, 0..tt.count(), crate::guid::CWGUID_COL);
                    if targets.contains(&v) {
                        stat.hit += 1;
                    } else {
                        stat.miss += 1;
                    }
                } else {
                    stat.unknown_type += 1;
                }
            }
        }

        // ---------------- CV/FP feed census
        if let Some(ci) = corpus.idx("CV") {
            let cv = &corpus.tables[ci].1;
            let live: Vec<usize> = (0..cv.count()).filter(|&r| is_live(cv, r)).collect();
            let f = &mut il.feed;
            f.cv_rows = live.len();
            f.cv_ovr_y = live
                .iter()
                .filter(|&&r| cell(cv, r, "OVR").map_or(false, |v| v.eq_ignore_ascii_case("Y")))
                .count();
            for &r in &live {
                let t = cell(cv, r, "TYPE").unwrap_or_default();
                *f.cv_types.entry(t).or_default() += 1;
            }
            let ids: Vec<String> = live.iter().filter_map(|&r| cell(cv, r, "ID")).collect();
            let nonblank: Vec<&String> = ids.iter().filter(|s| !s.is_empty()).collect();
            f.cv_id_blank = ids.len() - nonblank.len();
            let mut distinct = nonblank.clone();
            distinct.sort();
            distinct.dedup();
            f.cv_id_dupes = nonblank.len() - distinct.len();
            let formid: Vec<String> = live
                .iter()
                .filter_map(|&r| {
                    let id = cell(cv, r, "ID").unwrap_or_default();
                    if id.is_empty() {
                        None
                    } else {
                        Some(format!("{}|{}", cell(cv, r, "FORM").unwrap_or_default(), id))
                    }
                })
                .collect();
            let mut fdup = formid.clone();
            fdup.sort();
            fdup.dedup();
            f.cv_formid_dupes = formid.len() - fdup.len();
        }
        if let Some(fi) = corpus.idx("FP") {
            let fp = &corpus.tables[fi].1;
            let live: Vec<usize> = (0..fp.count()).filter(|&r| is_live(fp, r)).collect();
            let f = &mut il.feed;
            f.fp_rows = live.len();
            let mut keys: Vec<(String, String, String)> = Vec::with_capacity(live.len());
            for &r in &live {
                let wks = cell(fp, r, "WKS").unwrap_or_default();
                let cellno = cell(fp, r, "CELLNO").unwrap_or_default();
                let seqno = cell(fp, r, "SEQNO").unwrap_or_default();
                if cell(fp, r, "TEXT").map_or(false, |t| is_shaped(&t)) {
                    f.fp_text_guid_shaped += 1;
                }
                *f.fp_wks.entry(wks.clone()).or_default() += 1;
                keys.push((wks, cellno, seqno));
            }
            let mut k2 = keys.clone();
            k2.sort();
            k2.dedup();
            f.fp_dup_keys = keys.len() - k2.len();
            let mut cells: Vec<(String, String)> =
                keys.iter().map(|(w, c, _)| (w.clone(), c.clone())).collect();
            cells.sort();
            cells.dedup();
            f.fp_cells = cells.len();
        }

        // ---------------- numeric web
        let n = &mut il.numeric;
        let (Some(am_i), Some(mp_i)) = (corpus.idx("AM"), corpus.idx("MP")) else {
            return il;
        };
        let am = &corpus.tables[am_i].1;
        let mp = &corpus.tables[mp_i].1;
        let am_live: Vec<usize> = (0..am.count()).filter(|&r| is_live(am, r)).collect();
        let mp_live: Vec<usize> = (0..mp.count()).filter(|&r| is_live(mp, r)).collect();
        let am_del: Vec<usize> = (0..am.count()).filter(|&r| am.is_deleted(r)).collect();
        let mp_del: Vec<usize> = (0..mp.count()).filter(|&r| mp.is_deleted(r)).collect();

        n.am_rows = am_live.len();
        n.am_acno_blank = am_live
            .iter()
            .filter(|&&r| cell(am, r, "AC_NO").map_or(true, |v| v.is_empty()))
            .count();
        let am_no = string_set(am, &am_live, "AC_NO");
        let mp_no = string_set(mp, &mp_live, "MAP_NO");
        let am_del_no = string_set(am, &am_del, "AC_NO");
        let mp_del_no = string_set(mp, &mp_del, "MAP_NO");
        let mut tacno = string_set(am, &am_live, "TAC_NO");
        tacno.extend(string_set(mp, &mp_live, "TAC_NO"));

        for &r in &am_live {
            if let Some(v) = cell(am, r, "PAC_NO") {
                if !v.is_empty() {
                    n.pac_populated += 1;
                    if am_no.contains(&v) {
                        n.pac_hit += 1;
                    } else {
                        n.pac_miss += 1;
                    }
                }
            }
        }

        n.mp_rows = mp_live.len();
        for &r in &mp_live {
            let v = cell(mp, r, "MAP_NO").unwrap_or_default();
            if v.contains('.') {
                n.mp_mapno_prefixed += 1;
                let parent = &v[..v.rfind('.').unwrap()];
                if !mp_no.contains(parent) {
                    n.mp_prefix_parent_miss += 1;
                }
            }
        }

        for col in GROUP_COLS {
            let mut pop = 0;
            let mut miss = 0;
            for &r in &am_live {
                let v = cell(am, r, col).unwrap_or_default();
                if !v.is_empty() {
                    pop += 1;
                    if !mp_no.contains(&v) {
                        miss += 1;
                    }
                }
            }
            n.grp_pop.insert(col.to_string(), pop);
            n.grp_miss.insert(col.to_string(), miss);
        }

        // BL cube keys: classify NUMBER, count foreign entities.
        let am_entities: std::collections::HashSet<String> = am_live
            .iter()
            .filter_map(|&r| cell(am, r, "ENTITY"))
            .collect();
        if let Some(bi) = corpus.idx("BL") {
            let bl = &corpus.tables[bi].1;
            let mut entity_rows: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            for r in 0..bl.count() {
                if !is_live(bl, r) {
                    continue;
                }
                let v = cell(bl, r, "NUMBER").unwrap_or_default();
                let bucket = if v.is_empty() {
                    "blank"
                } else if am_no.contains(&v) {
                    "live_am"
                } else if mp_no.contains(&v) {
                    "live_mp"
                } else if am_del_no.contains(&v) {
                    "del_am"
                } else if mp_del_no.contains(&v) {
                    "del_mp"
                } else if tacno.contains(&v) {
                    "tacno"
                } else {
                    "synthetic"
                };
                match bucket {
                    "blank" => n.bl_number_blank += 1,
                    "live_am" => n.bl_number_live_am += 1,
                    "live_mp" => n.bl_number_live_mp += 1,
                    "del_am" => n.bl_number_del_am += 1,
                    "del_mp" => n.bl_number_del_mp += 1,
                    "tacno" => n.bl_number_tacno += 1,
                    _ => n.bl_number_synthetic += 1,
                }
                n.bl_rows += 1;
                let e = cell(bl, r, "ENTITY").unwrap_or_default();
                *entity_rows.entry(e).or_default() += 1;
            }
            n.bl_entity_foreign_kinds = entity_rows.keys().filter(|e| !am_entities.contains(*e)).count();
            n.bl_entity_foreign_rows = entity_rows
                .iter()
                .filter(|(e, _)| !am_entities.contains(*e))
                .map(|(_, c)| *c)
                .sum();
        }
        il
    }

    /// Live rows for `id` (empty if unknown).
    pub fn resolve(&self, id: &str) -> &[RowRef] {
        self.index.get(id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Live first, tombstones second.
    pub fn resolve_any(&self, id: &str) -> Option<&[RowRef]> {
        self.index
            .get(id)
            .map(Vec::as_slice)
            .or_else(|| self.deleted_index.get(id).map(Vec::as_slice))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_trailing_run() {
        assert_eq!(alias_of("MWGH LLC 9-30-25 REVIEW(1239263)am"), "AM");
        assert_eq!(alias_of("gl"), "GL");
        assert_eq!(alias_of("SH"), "SH");
        assert_eq!(alias_of("123"), "");
    }

    #[test]
    fn shape_matches_guid_alphabet() {
        assert!(is_shaped("D32ZB7OtE-qDpVPoZIhhuS"));
        assert!(is_shaped("Cu5u6JTq0I2CRjBsub+imY"));
        assert!(!is_shaped("short"));
        assert!(!is_shaped("!!!!!!!!!!!!!!!!!!!!!!"));
    }
}

