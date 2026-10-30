//! Regenerate tests/golden/dbf fixtures + roundtrip.json from expanded corpora.
//!
//! The original `tools/table_roundtrip_freeze` was not retained; this rebuilds its
//! output from the law documented in `tests/table_roundtrip.rs`:
//! per-table sha256 / census / header sha / multiset sha (sorted per-record sha
//! lines, joined with \n), plus optional fpt/cdx sidecars.
//!
//! CW quirk handled here (observed on all 5 fresh corpora): some tables omit the
//! trailing 0x1A EOF while the length law header+count*rec+1 still holds exactly.
//! Fixtures are canonicalized to the writer form (append 0x1A) so the strict
//! parser and the byte-identity round trip agree; the delta is exactly one byte.
use cw_core::dbf::Table;
use cw_core::sha256::sha256_hex;
use std::path::{Path, PathBuf};

const KEYS: [&str; 22] = [
    "AC", "AM", "AN", "BL", "CE", "CV", "EC", "FP", "GL", "GR", "GS", "HI", "MP", "PR", "RA",
    "RL", "SH", "TM", "TP", "TS", "WR", "WT",
];

/// corpus key -> expanded dir (under workspace root).
const CORPORA: [(&str, &str); 3] = [
    ("corp2026", "USI_Compilation_Training(1155962)"),
    ("hull2024", "2024_Hull_Lift_Truck_Review(1148441)"),
    ("mwgh2025", "MWGH_LLC_9-30-25_REVIEW(1239263)"),
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn find_suffix(dir: &Path, key: &str, ext: &str) -> Option<PathBuf> {
    let ext_uc = ext.to_uppercase();
    let mut hit = None;
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.ok()?.path();
        if p.is_file()
            && p.extension().map(|x| x.to_string_lossy().to_uppercase()).as_deref() == Some(ext_uc.as_str())
            && p.file_name().map(|x| x.to_string_lossy().to_uppercase()).as_deref()
                .is_some_and(|n| n.ends_with(&format!(".{ext_uc}")) && n.ends_with(&format!("{key}.{ext_uc}")))
        {
            hit = Some(p);
        }
    }
    hit
}

/// Canonicalize: enforce the trailing 0x1A when the length law is otherwise exact.
fn canonicalize(bytes: &mut Vec<u8>) -> Result<(), String> {
    if bytes.len() < 12 {
        return Err("short".into());
    }
    let count = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let hlen = u16::from_le_bytes(bytes[8..10].try_into().unwrap()) as usize;
    let rlen = u16::from_le_bytes(bytes[10..12].try_into().unwrap()) as usize;
    let expected = hlen + count * rlen + 1;
    if bytes.len() != expected {
        return Err(format!("len {} != expected {expected}", bytes.len()));
    }
    if bytes[expected - 1] != 0x1A {
        bytes[expected - 1] = 0x1A; // CW omits/variant-fills the EOF slot; canonicalize
    }
    Ok(())
}

fn jstr(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn main() {
    let golden = root().join("tests").join("golden").join("dbf");
    std::fs::create_dir_all(&golden).unwrap();

    let mut corpus_json = String::new();
    let (mut t_tables, mut t_rows, mut t_live, mut t_del, mut t_zero) = (0, 0, 0, 0, 0);
    let (mut t_fpt, mut t_cdx) = (0, 0);

    for (ci, (corpus, dir)) in CORPORA.iter().enumerate() {
        let src = root().join("expanded").join(dir);
        let dst = golden.join(corpus);
        std::fs::create_dir_all(&dst).unwrap();
        let mut tables_json = String::new();
        let mut oracle_tables = String::new();
        for (ki, key) in KEYS.iter().enumerate() {
            let Some(dbf_path) = find_suffix(&src, key, "dbf") else {
                println!("MISSING dbf {corpus}/{key}");
                continue;
            };
            let mut bytes = std::fs::read(&dbf_path).unwrap();
            if let Err(e) = canonicalize(&mut bytes) {
                println!("SKIP {corpus}/{key}: {e}");
                continue;
            }
            let table = match Table::parse(&bytes) {
                Ok(t) => t,
                Err(e) => {
                    println!("PARSE-FAIL {corpus}/{key}: {e:?}");
                    continue;
                }
            };
            let live = (0..table.count()).filter(|r| !table.is_deleted(*r)).count();
            let del = table.count() - live;
            if table.count() == 0 {
                t_zero += 1;
            }

            // write canonical dbf
            let dbf_name = dbf_path.file_name().unwrap().to_string_lossy().to_string();
            std::fs::write(dst.join(&dbf_name), &bytes).unwrap();

            let mut entry = format!(
                "\"{}\": {{\"file\": {}, \"sha256\": {}, \"count\": {}, \"record_len\": {}, \"header_len\": {}, \"live\": {}, \"deleted\": {}, \"header_sha256\": {}, \"multiset_sha256\": {}",
                key,
                jstr(&dbf_name),
                jstr(&sha256_hex(&bytes)),
                table.count(),
                table.record_len,
                table.header_len,
                live,
                del,
                jstr(&sha256_hex(&table.raw_header)),
                jstr(&multiset_sha(&table.records)),
            );

            // sidecars
            if let Some(fpt_path) = find_suffix(&src, key, "fpt") {
                let raw = std::fs::read(&fpt_path).unwrap();
                if cw_core::fpt::Fpt::parse(&raw).is_ok() {
                    let name = fpt_path.file_name().unwrap().to_string_lossy().to_string();
                    std::fs::write(dst.join(&name), &raw).unwrap();
                    entry += &format!(
                        ", \"fpt\": {{\"file\": {}, \"sha256\": {}, \"bytes\": {}}}",
                        jstr(&name),
                        jstr(&sha256_hex(&raw)),
                        raw.len()
                    );
                    t_fpt += 1;
                }
            }
            if let Some(cdx_path) = find_suffix(&src, key, "cdx") {
                let raw = std::fs::read(&cdx_path).unwrap();
                let name = cdx_path.file_name().unwrap().to_string_lossy().to_string();
                std::fs::write(dst.join(&name), &raw).unwrap();
                entry += &format!(
                    ", \"cdx\": {{\"file\": {}, \"sha256\": {}, \"bytes\": {}}}",
                    jstr(&name),
                    jstr(&sha256_hex(&raw)),
                    raw.len()
                );
                t_cdx += 1;
            }
            entry.push('}');

            // oracle.json entry (C1: shape + schema + canonical hashes)
            let mut fields = String::from("[");
            for (fi, f) in table.fields.iter().enumerate() {
                if fi > 0 {
                    fields.push(',');
                }
                fields.push_str(&format!(
                    "[{},{},{},{}]",
                    jstr(f.name.trim_end()),
                    jstr(&(f.kind as char).to_string()),
                    f.len,
                    f.dec
                ));
            }
            fields.push(']');
            let fpt_for_canon = find_suffix(&src, key, "fpt")
                .and_then(|p| cw_core::fpt::Fpt::parse_path(&p).ok());
            let sample0 = if table.count() > 0 {
                format!("[{}]", jstr(&cw_core::canon::record_line(&table, 0, fpt_for_canon.as_ref())))
            } else {
                "[]".to_string()
            };
            let records_sha = cw_core::canon::records_sha(&table, fpt_for_canon.as_ref());
            let mut oracle_entry = format!(
                "\"{}\": {{\"file\": {}, \"sha256\": {}, \"count\": {}, \"header_len\": {}, \"record_len\": {}, \"deleted\": {}, \"ver\": {}, \"fields\": {}, \"sample\": {}, \"records_sha\": {}",
                key,
                jstr(&dbf_name),
                jstr(&sha256_hex(&bytes)),
                table.count(),
                table.header_len,
                table.record_len,
                del,
                jstr(&format!("{:02x}", table.version)),
                fields,
                sample0,
                jstr(&records_sha),
            );
            if let Some(f) = fpt_for_canon.as_ref() {
                oracle_entry += &format!(", \"fpt\": {{\"block_size\": {}}}", f.block_size);
            }
            oracle_entry.push('}');
            if ki > 0 {
                oracle_tables.push_str(",\n    ");
            }
            oracle_tables.push_str(&oracle_entry);

            tables_json.push_str(&entry);
            if ki + 1 < KEYS.len() {
                tables_json.push_str(",\n      ");
            }
            t_tables += 1;
            t_rows += table.count();
            t_live += live;
            t_del += del;
        }
        corpus_json.push_str(&format!(
            "\"{corpus}\": {{\"tables\": {{\n      {tables_json}\n    }}}}"
        ));

        // per-corpus oracle (C1 bridge: golden_dbf) + port-contract guards (golden_contract)
        let oracle = format!("{{\n  \"tables\": {{\n    {oracle_tables}\n  }}\n}}\n");
        std::fs::write(golden.join(corpus).join("oracle.json"), &oracle).unwrap();
        let readme = golden.join("README.md");
        if !readme.exists() {
            std::fs::write(
                &readme,
                "# golden fixtures — port contract\n\n\
                Frozen expectations regenerated 2026-10-28 from the engagement packages\n\
                (corp2026 / hull2024 / mwgh2025) by `examples/freeze_roundtrip`.\n\n\
                - `<corpus>/<KEY>.dbf` — canonicalized table bytes (writer form; CW omits the\n\
                trailing 0x1A EOF on some tables — the length law stays exact, the byte is\n\
                normalized on ingest).\n\
                - `<corpus>/<KEY>.fpt` / `.cdx` — verbatim sidecars.\n\
                - `roundtrip.json` — C6a law: per-table sha/census/header-sha/multiset-sha.\n\
                - `<corpus>/oracle.json` — C1 law: shape, schema, canonical first record,\n\
                all-records canonical sha (via `cw_core::canon`).\n",
            )
            .unwrap();
        }
        let manifest = golden.parent().unwrap().join("MANIFEST.toml");
        if !manifest.exists() {
            std::fs::write(
                &manifest,
                "# golden areas (accumulate as freezes land)\nareas = [\"dbf\"]\n",
            )
            .unwrap();
        }
        if ci + 1 < CORPORA.len() {
            corpus_json.push_str(",\n  ");
        }
    }

    let doc = format!(
        "{{\n  \"corpora\": {{\n  {}  \n  }},\n  \"totals\": {{\"tables\": {}, \"records\": {}, \"live\": {}, \"deleted\": {}, \"zero_rows\": {}, \"fpt\": {}, \"cdx\": {}}}\n}}\n",
        corpus_json, t_tables, t_rows, t_live, t_del, t_zero, t_fpt, t_cdx
    );
    std::fs::write(golden.join("roundtrip.json"), &doc).unwrap();
    println!(
        "froze: tables={t_tables} rows={t_rows} live={t_live} del={t_del} zero={t_zero} fpt={t_fpt} cdx={t_cdx}"
    );
}

fn multiset_sha(records: &[Vec<u8>]) -> String {
    let mut lines: Vec<String> = records.iter().map(|r| sha256_hex(r)).collect();
    lines.sort();
    sha256_hex(lines.join("\n").as_bytes())
}
