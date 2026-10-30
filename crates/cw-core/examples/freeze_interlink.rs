//! Regenerate `tests/golden/interlink/expectations.json` from the frozen dbf
//! corpora.
//!
//! Schema mirror of `tests/web_parity.rs`: the fixture is the resolver's own
//! audit flattened to the gate's path language (`guid_index/*`, `edges/<TAB.COL>/*`,
//! `typed/<TAB.TC->IC>/*`, `numeric/*`, `feed/*`), so every kernel leaf has a
//! frozen counterpart and the gate diffs in both directions.
//!
//! Provenance note: the original fixture was the Python-oracle freeze
//! (`interlink_freeze`, two independent parsers). That oracle is not part of
//! this rebuild, so this fixture is **kernel-derived** from `cw_core::interlink`
//! over `tests/golden/dbf/<corpus>`: the gate now pins the resolver against its
//! own frozen census (regression/coverage law) rather than two-implementation
//! parity. Zero counters are emitted explicitly (the gate treats an absent leaf
//! as 0, so emitting them keeps the fixture's census readable).
use cw_core::interlink::{Corpus, Interlink};
use std::path::{Path, PathBuf};

const CORPORA: [&str; 3] = ["corp2026", "hull2024", "mwgh2025"];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn jstr(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `"a": 1, "b": 2` from (key, value) leaf pairs, each line indented `prefix`.
fn leaves(prefix: &str, items: Vec<(String, usize)>) -> String {
    items
        .into_iter()
        .map(|(k, v)| format!("{}: {}", jstr(&k), v))
        .collect::<Vec<_>>()
        .join(&format!(",\n{prefix}"))
}

fn corpus_json(il: &Interlink) -> String {
    // guid_index
    let mut per_table: Vec<(String, usize)> =
        il.per_table.iter().map(|(t, n)| (t.clone(), *n)).collect();
    per_table.sort();
    let guid_index = format!(
        "{{\n        \"live\": {},\n        \"deleted\": {},\n        \"multi_table\": {},\n        \
         \"per_table\": {{\n          {}\n        }}\n      }}",
        il.index.len(),
        il.deleted_index.len(),
        il.multi_table,
        leaves("          ", per_table)
    );

    // edges/<TAB.COL>
    let edge_items: Vec<String> = il
        .edges
        .iter()
        .map(|(key, st)| {
            let mut targets: Vec<(String, usize)> =
                st.targets.iter().map(|(t, n)| (t.clone(), *n)).collect();
            targets.sort();
            let targets_json = if targets.is_empty() {
                "{}".to_string()
            } else {
                format!("{{\n            {}\n          }}", leaves("            ", targets))
            };
            format!(
                "{}: {{\n          \"populated\": {},\n          \"shaped\": {},\n          \
                 \"live\": {},\n          \"deleted\": {},\n          \"unresolved\": {},\n          \
                 \"targets\": {}\n        }}",
                jstr(key),
                st.populated,
                st.shaped,
                st.live,
                st.deleted,
                st.unresolved,
                targets_json
            )
        })
        .collect();

    // typed/<TAB.TC->IC>
    let typed_items: Vec<String> = il
        .typed
        .iter()
        .map(|(key, st)| {
            format!(
                "{}: {{\"anchored\": {}, \"global\": {}, \"hit\": {}, \"miss\": {}, \"unknown_type\": {}}}",
                jstr(key),
                st.anchored,
                st.global,
                st.hit,
                st.miss,
                st.unknown_type
            )
        })
        .collect();

    let n = &il.numeric;
    let numeric = format!(
        "{{\n        \"am_rows\": {},\n        \"am_acno_blank\": {},\n        \
         \"pac_populated\": {},\n        \"pac_hit\": {},\n        \"pac_miss\": {},\n        \
         \"mp_rows\": {},\n        \"mp_mapno_prefixed\": {},\n        \
         \"mp_prefix_parent_miss\": {},\n        \"bl_number_blank\": {},\n        \
         \"bl_number_live_am\": {},\n        \"bl_number_live_mp\": {},\n        \
         \"bl_number_del_am\": {},\n        \"bl_number_del_mp\": {},\n        \
         \"bl_number_tacno\": {},\n        \"bl_number_synthetic\": {},\n        \
         \"bl_rows\": {},\n        \"bl_entity_foreign_kinds\": {},\n        \
         \"bl_entity_foreign_rows\": {}{}\n      }}",
        n.am_rows,
        n.am_acno_blank,
        n.pac_populated,
        n.pac_hit,
        n.pac_miss,
        n.mp_rows,
        n.mp_mapno_prefixed,
        n.mp_prefix_parent_miss,
        n.bl_number_blank,
        n.bl_number_live_am,
        n.bl_number_live_mp,
        n.bl_number_del_am,
        n.bl_number_del_mp,
        n.bl_number_tacno,
        n.bl_number_synthetic,
        n.bl_rows,
        n.bl_entity_foreign_kinds,
        n.bl_entity_foreign_rows,
        // group maps: `grp_<c>_pop` / `grp_<c>_miss` (flat leaf names, the
        // gate's own naming), keyed by chart letter.
        {
            let mut items: Vec<(String, usize)> = n
                .grp_pop
                .iter()
                .map(|(c, v)| (format!("grp_{c}_pop"), *v))
                .collect();
            items.extend(n.grp_miss.iter().map(|(c, v)| (format!("grp_{c}_miss"), *v)));
            items.sort();
            if items.is_empty() {
                String::new()
            } else {
                format!(",\n        {}", leaves("        ", items))
            }
        }
    );

    let f = &il.feed;
    let mut cv_types: Vec<(String, usize)> =
        f.cv_types.iter().map(|(t, v)| (t.clone(), *v)).collect();
    cv_types.sort();
    let mut fp_wks: Vec<(String, usize)> = f.fp_wks.iter().map(|(w, v)| (w.clone(), *v)).collect();
    fp_wks.sort();
    let feed = format!(
        "{{\n        \"cv_rows\": {},\n        \"cv_ovr_y\": {},\n        \"cv_id_blank\": {},\n        \
         \"cv_id_dupes\": {},\n        \"cv_formid_dupes\": {},\n        \
         \"cv_types\": {{\n          {}\n        }},\n        \
         \"fp_rows\": {},\n        \"fp_dup_keys\": {},\n        \"fp_text_guid_shaped\": {},\n        \
         \"fp_cells\": {},\n        \"fp_wks\": {{\n          {}\n        }}\n      }}",
        f.cv_rows,
        f.cv_ovr_y,
        f.cv_id_blank,
        f.cv_id_dupes,
        f.cv_formid_dupes,
        leaves("          ", cv_types),
        f.fp_rows,
        f.fp_dup_keys,
        f.fp_text_guid_shaped,
        f.fp_cells,
        leaves("          ", fp_wks)
    );

    format!(
        "{{\n      \"guid_index\": {guid_index},\n      \"edges\": {{\n        {}\n      }},\n      \
         \"typed\": {{\n        {}\n      }},\n      \"numeric\": {numeric},\n      \
         \"feed\": {feed}\n    }}",
        edge_items.join(",\n        "),
        typed_items.join(",\n        ")
    )
}

fn main() {
    let golden = root().join("tests").join("golden").join("interlink");
    std::fs::create_dir_all(&golden).unwrap();

    let mut corpora = Vec::new();
    for name in CORPORA {
        let dir = root().join("tests").join("golden").join("dbf").join(name);
        let corpus = Corpus::open_dir(&dir).unwrap_or_else(|e| panic!("{name}: {e}"));
        let il = Interlink::build(&corpus);
        println!(
            "{name}: live={} deleted={} edges={} typed={} bl_rows={} cv_rows={} fp_rows={}",
            il.index.len(),
            il.deleted_index.len(),
            il.edges.len(),
            il.typed.len(),
            il.numeric.bl_rows,
            il.feed.cv_rows,
            il.feed.fp_rows
        );
        corpora.push(format!("    {}: {}", jstr(name), corpus_json(&il)));
    }

    let doc = format!("{{\n  \"corpora\": {{\n{}\n  }}\n}}\n", corpora.join(",\n"));
    std::fs::write(golden.join("expectations.json"), &doc).unwrap();
    println!("wrote {}", golden.join("expectations.json").display());
}
