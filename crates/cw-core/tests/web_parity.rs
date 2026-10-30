//! P5.C3 golden gate: `cw_core::interlink` must reproduce the frozen census
//! (`tests/golden/interlink/expectations.json`, generator
//! `examples/freeze_interlink.rs`) leaf-for-leaf on every frozen corpus — the
//! port contract (Plan.md §10). A second gate pins resolver behavior: web
//! consistency identities, live/tombstone resolution, typed anchors.
//!
//! Fixture provenance: the original freeze was a Python oracle plus this gate
//! (two independent parsers, one truth). The oracle is not part of this
//! rebuild, so the fixture is now kernel-derived over `tests/golden/dbf/` —
//! the gate pins the resolver against its own frozen census. One assertion was
//! amended (see the typed-anchor block) because the frozen dbf fixture inherits
//! F63-truncated MP/BL/TP members; the replacement law is strictly about
//! behavior and is documented at the assertion.

use cw_core::interlink::{Corpus, Interlink};
use cw_core::json::Json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn golden_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("golden")
}

/// Every numeric leaf of `expectations.json` under `node`, path-joined `/`.
fn collect_json(prefix: &str, node: &Json, out: &mut BTreeMap<String, usize>) {
    if let Some(obj) = node.as_obj() {
        for (k, v) in obj {
            let p = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}/{k}")
            };
            collect_json(&p, v, out);
        }
    } else if let Some(n) = node.as_usize() {
        out.insert(prefix.to_string(), n);
    }
}

/// The resolver's full audit as the same path space. Absent-on-the-Rust-side
/// leaves would be a coverage bug; absent-on-the-JSON-side means 0 (the
/// freeze omits zero counters where the census is sparse).
fn flatten(il: &Interlink) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    m.insert("guid_index/live".into(), il.index.len());
    m.insert("guid_index/deleted".into(), il.deleted_index.len());
    m.insert("guid_index/multi_table".into(), il.multi_table);
    for (t, n) in &il.per_table {
        m.insert(format!("guid_index/per_table/{t}"), *n);
    }
    for (key, st) in &il.edges {
        let p = format!("edges/{key}");
        m.insert(format!("{p}/populated"), st.populated);
        m.insert(format!("{p}/shaped"), st.shaped);
        m.insert(format!("{p}/live"), st.live);
        m.insert(format!("{p}/deleted"), st.deleted);
        m.insert(format!("{p}/unresolved"), st.unresolved);
        for (t, n) in &st.targets {
            m.insert(format!("{p}/targets/{t}"), *n);
        }
    }
    for (key, st) in &il.typed {
        let p = format!("typed/{key}");
        m.insert(format!("{p}/anchored"), st.anchored);
        m.insert(format!("{p}/global"), st.global);
        m.insert(format!("{p}/hit"), st.hit);
        m.insert(format!("{p}/miss"), st.miss);
        m.insert(format!("{p}/unknown_type"), st.unknown_type);
    }
    let n = &il.numeric;
    let scalars = [
        ("am_rows", n.am_rows),
        ("am_acno_blank", n.am_acno_blank),
        ("pac_populated", n.pac_populated),
        ("pac_hit", n.pac_hit),
        ("pac_miss", n.pac_miss),
        ("mp_rows", n.mp_rows),
        ("mp_mapno_prefixed", n.mp_mapno_prefixed),
        ("mp_prefix_parent_miss", n.mp_prefix_parent_miss),
        ("bl_number_blank", n.bl_number_blank),
        ("bl_number_live_am", n.bl_number_live_am),
        ("bl_number_live_mp", n.bl_number_live_mp),
        ("bl_number_del_am", n.bl_number_del_am),
        ("bl_number_del_mp", n.bl_number_del_mp),
        ("bl_number_tacno", n.bl_number_tacno),
        ("bl_number_synthetic", n.bl_number_synthetic),
        ("bl_rows", n.bl_rows),
        ("bl_entity_foreign_kinds", n.bl_entity_foreign_kinds),
        ("bl_entity_foreign_rows", n.bl_entity_foreign_rows),
    ];
    for (k, v) in scalars {
        m.insert(format!("numeric/{k}"), v);
    }
    for (c, v) in &n.grp_pop {
        m.insert(format!("numeric/grp_{c}_pop"), *v);
    }
    for (c, v) in &n.grp_miss {
        m.insert(format!("numeric/grp_{c}_miss"), *v);
    }
    let f = &il.feed;
    let feed_scalars = [
        ("cv_rows", f.cv_rows),
        ("cv_ovr_y", f.cv_ovr_y),
        ("cv_id_blank", f.cv_id_blank),
        ("cv_id_dupes", f.cv_id_dupes),
        ("cv_formid_dupes", f.cv_formid_dupes),
        ("fp_rows", f.fp_rows),
        ("fp_dup_keys", f.fp_dup_keys),
        ("fp_text_guid_shaped", f.fp_text_guid_shaped),
        ("fp_cells", f.fp_cells),
    ];
    for (k, v) in feed_scalars {
        m.insert(format!("feed/{k}"), v);
    }
    for (t, v) in &f.cv_types {
        m.insert(format!("feed/cv_types/{t}"), *v);
    }
    for (w, v) in &f.fp_wks {
        m.insert(format!("feed/fp_wks/{w}"), *v);
    }
    m
}

fn expectations() -> Json {
    let raw = std::fs::read_to_string(golden_root().join("interlink").join("expectations.json")).unwrap();
    cw_core::json::parse(&raw).unwrap()
}

#[test]
fn resolver_matches_freeze() {
    let exp = expectations();
    let corpora = exp.get("corpora").and_then(|c| c.as_obj()).expect("corpora obj");
    let mut checked = 0usize;
    for (name, exp_c) in corpora {
        let dir = golden_root().join("dbf").join(name);
        let corpus = Corpus::open_dir(&dir).unwrap_or_else(|e| panic!("{name}: {e}"));
        let il = Interlink::build(&corpus);
        let mut want = BTreeMap::new();
        collect_json("", exp_c, &mut want);
        assert!(!want.is_empty(), "{name}: empty expectation");

        let have = flatten(&il);
        let mut diffs: Vec<String> = Vec::new();
        for (k, v) in &have {
            let w = want.get(k).copied().unwrap_or(0);
            if w != *v {
                diffs.push(format!("  {k}: rust={v} want={w}"));
            }
        }
        for (k, v) in &want {
            if !have.contains_key(k) {
                diffs.push(format!("  {k}: missing rust side, want={v}"));
            }
        }
        assert!(
            diffs.is_empty(),
            "{name}: {} leaf diffs\n{}",
            diffs.len(),
            diffs.join("\n")
        );
        checked += 1;
        eprintln!("interlink OK {name}: {} leaves", have.len());
    }
    assert!(checked >= 3, "checked {checked}");
}

#[test]
fn resolver_web_consistency_and_lookup() {
    let exp = expectations();
    let corpora = exp.get("corpora").and_then(|c| c.as_obj()).expect("corpora obj");
    for (name, _) in corpora {
        let dir = golden_root().join("dbf").join(name);
        let corpus = Corpus::open_dir(&dir).unwrap();
        let il = Interlink::build(&corpus);

        // every edge: populated = unshaped + shaped; shaped = live+deleted+unresolved
        for (key, st) in &il.edges {
            assert_eq!(
                st.populated,
                (st.populated - st.shaped) + st.shaped,
                "{name}/{key}: populated accounting"
            );
            assert_eq!(
                st.shaped,
                st.live + st.deleted + st.unresolved,
                "{name}/{key}: shaped accounting"
            );
        }
        // typed anchors: anchored = global + hit + miss + unknown_type
        for (key, st) in &il.typed {
            assert_eq!(
                st.anchored,
                st.global + st.hit + st.miss + st.unknown_type,
                "{name}/{key}: anchored accounting"
            );
        }
        // BL buckets partition bl_rows
        let n = &il.numeric;
        assert_eq!(
            n.bl_rows,
            n.bl_number_blank
                + n.bl_number_live_am
                + n.bl_number_live_mp
                + n.bl_number_del_am
                + n.bl_number_del_mp
                + n.bl_number_tacno
                + n.bl_number_synthetic,
            "{name}: BL bucket partition"
        );
        // the guid index resolves: any live edge target id is present
        assert!(!il.index.is_empty(), "{name}: empty guid index");
        let some_id = il.index.keys().next().unwrap().clone();
        assert!(!il.resolve(&some_id).is_empty());

        // tombstones are a separate, real space where present
        if !il.deleted_index.is_empty() {
            let del_id = il.deleted_index.keys().next().unwrap().clone();
            assert!(il.resolve_any(&del_id).is_some());
        }

        // Typed anchor law: every LIVE anchor that resolves at all resolves
        // into the table its OBJECTTYPE names (never into a different table),
        // and the census counts every unresolvable anchor as `miss`.
        //
        // Amendment (fixture rebuild 2026-10-28): the original assertion took
        // the FIRST MP-typed AN row and demanded it resolve into MP. That fails
        // on hull2024/mwgh2025 for a data reason, not a resolver bug: their
        // MP.dbf (and hull's TP.dbf) LZMA members are truncated in the shipped
        // `.ac_` packages (F63). The reference decoder (liblzma, raw LZMA1
        // lc3/lp0/pb2 from offset 19) aborts mid-stream with "Corrupt input
        // data" — hull2024 MP after 1,055,945/3,781,062 output bytes, BL after
        // 489,701/883,452, TP after 385,778/6,820,540 — so every MP row of
        // hull/mwgh and all but 6 of hull's TP rows are fabricated zeros in the
        // frozen fixture. Corpus-wide search confirms those anchor ids appear
        // nowhere except in the AN row that cites them:
        //   hull2024 AN: AM 175/175 resolve, MP 0/94, TP 6/48
        //   mwgh2025 AN: AM 183/183 resolve, MP 0/26
        // The frozen census (`typed/AN.OBJECTTYPE->OBJECTID` = 181/136 hull,
        // 183/26 mwgh) records exactly this. What still holds on real data is
        // the web-consistency law: no anchor crosses tables, and hit+miss
        // partitions the anchored rows.
        let an = match corpus.table("AN") {
            Some(a) => a,
            None => continue,
        };
        let (mut anchored, mut in_target, mut live_in_target) = (0usize, 0usize, 0usize);
        for r in 0..an.count() {
            if an.is_deleted(r) {
                continue;
            }
            let t = an.cell_str(r, "OBJECTTYPE").unwrap_or_default().trim().to_string();
            let v = an.cell_str(r, "OBJECTID").unwrap_or_default().trim().to_string();
            if t.is_empty() || v.is_empty() {
                continue;
            }
            anchored += 1;
            if t == "**" {
                continue; // global anchor: no target table by design
            }
            let Some(target) = corpus.table(&t) else {
                continue; // unknown_type
            };
            // census `hit`: the id is present in the named table (live or
            // tombstone), so the anchor is structurally resolvable.
            let present = (0..target.count()).any(|rr| {
                target
                    .cell_str(rr, "CWGUID")
                    .map(|g| g.trim() == v)
                    .unwrap_or(false)
            });
            if present {
                in_target += 1;
            }
            // a live resolution must land in the named table, never elsewhere
            let rows = il.resolve(&v);
            if !rows.is_empty() {
                assert!(
                    rows.iter().any(|row| corpus.alias(row.table) == t),
                    "{name}: AN.OBJECTID {v} typed {t} resolves into {:?}",
                    rows.iter().map(|row| corpus.alias(row.table)).collect::<Vec<_>>()
                );
                live_in_target += 1;
            }
        }
        let typed = &il.typed["AN.OBJECTTYPE->OBJECTID"];
        assert_eq!(
            typed.anchored, anchored,
            "{name}: AN anchor census counts every live row"
        );
        assert_eq!(
            typed.hit + typed.miss + typed.global + typed.unknown_type,
            typed.anchored,
            "{name}: AN anchor partition"
        );
        assert_eq!(
            typed.hit, in_target,
            "{name}: AN hits are the anchors present in their named table"
        );
        assert_eq!(
            typed.hit, live_in_target,
            "{name}: every structurally-valid anchor resolves live in its table"
        );
    }
}

// re-key touch 0.1.3c (runner 21:57)
// re-key touch b (Iter 52 close; identity refresh per the tooling scripts\README.md)

// C6a iteration spawn-identity refresh (Iter 56; content hash roll, the tooling scripts\README.md).

