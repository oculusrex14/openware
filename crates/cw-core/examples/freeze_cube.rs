//! Regenerate `tests/golden/cube/expectations.json` from the frozen BL tables.
//!
//! Schema mirror of `tests/cube_golden.rs`: per corpus — the live row count, the
//! L1..L7 law counters, the non-blank/DIST census, the nine axis censuses, the
//! shape-bucket census and the full L5 balance table keyed `number|year|group`.
//!
//! Provenance note: the original fixture was a Python-oracle freeze
//! (`cube_freeze`, cross-checked leaf-for-leaf against the kernel). That oracle
//! is not part of this rebuild, so this fixture is **kernel-derived**: every
//! leaf is `cw_core::cube`'s own output over `tests/golden/dbf/<corpus>` (the C1
//! verbatim freeze), and the gate now pins the kernel against its own frozen
//! bytes — a regression/coverage law rather than two-implementation parity.
//! Balance keys/values therefore come from [`Cube::balances`] (rounded to 1e-6,
//! the map's own quantization), which `Cube::balance` reproduces exactly.
use cw_core::cube::{Cube, CubeAudit, AUDIT_COLUMNS};
use std::path::{Path, PathBuf};

/// The three frozen corpora (the C1 dbf freeze).
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

/// Compact, always-decimal float emission (Rust `Display` never uses exponents,
/// so the result is valid JSON for every value the cube produces).
fn fnum(v: f64) -> String {
    if v == 0.0 {
        return "0".to_string();
    }
    format!("{v}")
}

fn strmap(m: &std::collections::BTreeMap<String, u32>) -> String {
    let pairs: Vec<String> = m.iter().map(|(k, v)| format!("{}: {}", jstr(k), v)).collect();
    format!("{{{}}}", pairs.join(", "))
}

fn bl_path(dir: &Path) -> PathBuf {
    std::fs::read_dir(dir)
        .expect("corpus dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_lowercase().ends_with("bl.dbf"))
                .unwrap_or(false)
        })
        .expect("BL.dbf in corpus")
}

fn corpus_json(a: &CubeAudit, balances: &std::collections::BTreeMap<String, f64>) -> String {
    let axes: Vec<String> = AUDIT_COLUMNS
        .iter()
        .map(|col| format!("{}: {}", jstr(col), strmap(&a.axes[*col])))
        .collect();

    // balances: one `number|year|group` key per live cube address
    let bal: Vec<String> = balances
        .iter()
        .map(|(k, v)| format!("{}: {}", jstr(k), fnum(*v)))
        .collect();

    format!(
        "{{\n      \"rows\": {},\n      \
         \"laws\": {{\n        \
         \"pair_mirror\": [{}, {}],\n        \
         \"x_ytd_eq_amt\": {},\n        \
         \"x_fx_eq_amt\": {},\n        \
         \"x_fx_ytd_eq_ytd\": {},\n        \
         \"o_ytd_zero\": {},\n        \
         \"o_fxytd_zero\": {},\n        \
         \"p_rows\": {},\n        \
         \"p_ytd_zero\": {},\n        \
         \"p_groups_without_X\": {},\n        \
         \"p_neq_x_snapshot_drift\": {},\n        \
         \"soloX_eq_sumYnp\": [{}, {}],\n        \
         \"ytd_cum_ok\": {},\n        \
         \"ytd_cum_rows\": {}\n      }},\n      \
         \"nonblank\": {{\"BOOKED\": {}, \"CUSTOM\": {}, \"ENTITY\": {}, \"AMT_DIST\": {}, \"FX_DIST\": {}}},\n      \
         \"axes\": {{\n        {}\n      }},\n      \
         \"shape_bucket\": {},\n      \
         \"balances\": {{\n        {}\n      }}\n    }}",
        a.rows,
        a.pair_mirror_ok,
        a.pair_mirror_paired,
        a.x_ytd_eq_amt,
        a.x_fx_eq_amt,
        a.x_fx_ytd_eq_ytd,
        a.o_ytd_zero,
        a.o_fxytd_zero,
        a.p_rows,
        a.p_ytd_zero,
        a.p_groups_without_x,
        a.p_neq_x_snapshot_drift,
        a.solo_x_eq_sum_ynp,
        a.solo_x,
        a.ytd_cum_ok,
        a.ytd_cum_rows,
        a.nonblank_booked,
        a.nonblank_custom,
        a.nonblank_entity,
        a.amt_dist_nonzero,
        a.fx_dist_nonzero,
        axes.join(",\n        "),
        strmap(&a.shapes),
        bal.join(",\n        "),
    )
}

fn main() {
    let golden = root().join("tests").join("golden").join("cube");
    std::fs::create_dir_all(&golden).unwrap();

    let mut corpora = Vec::new();
    for name in CORPORA {
        let table =
            cw_core::dbf::Table::parse_path(bl_path(&root().join("tests").join("golden").join("dbf").join(name)))
                .expect("parse BL");
        let cube = Cube::from_table(&table).expect("cube");
        let audit = cube.audit();
        let balances = cube.balances();
        println!(
            "{name}: rows={} balances={} pair={}/{} soloX={} p={} shapes={}",
            audit.rows,
            balances.len(),
            audit.pair_mirror_ok,
            audit.pair_mirror_paired,
            audit.solo_x,
            audit.p_rows,
            audit.shapes.len()
        );
        corpora.push(format!("    {}: {}", jstr(name), corpus_json(&audit, &balances)));
    }

    let doc = format!(
        "{{\n  \"corpora\": {{\n{}\n  }}\n}}\n",
        corpora.join(",\n")
    );
    std::fs::write(golden.join("expectations.json"), &doc).unwrap();
    println!("wrote {}", golden.join("expectations.json").display());
}
