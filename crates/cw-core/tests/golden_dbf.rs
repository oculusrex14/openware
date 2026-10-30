//! P5.C1 golden gate: cw-core round-trips every frozen corpus table
//! byte-stably and reproduces the Python oracle's canonical hashes.
//!
//! Fixtures: `tests/golden/dbf/<corpus>/` written by `tools/golden_freeze`
//! from the bridge-expanded canonical corpora (hull2024 / corp2026 / mwgh2025;
//! established as pristine, F31). The oracle JSON pins counts, schema, first-record
//! canonical line and the all-records sha256 per table.

use cw_core::dbf::Table;
use cw_core::fpt::Fpt;

fn golden_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("golden")
        .join("dbf")
}

fn corpora() -> Vec<std::path::PathBuf> {
    let root = golden_root();
    let mut dirs: Vec<std::path::PathBuf> = std::fs::read_dir(&root)
        .expect("golden dbf root")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join("oracle.json").is_file())
        .collect();
    dirs.sort();
    assert!(
        dirs.len() >= 3,
        "expected >=3 frozen corpora under {}, found {}",
        root.display(),
        dirs.len()
    );
    dirs
}

/// Same-stem sidecar lookup, case-insensitive (CW mixes `AC.FPT`/`an.fpt`).
fn sidecar(dir: &std::path::Path, dbf_name: &str, ext: &str) -> Option<std::path::PathBuf> {
    let stem = dbf_name
        .strip_suffix(".dbf")
        .expect("dbf suffix")
        .to_ascii_lowercase();
    let prefix = format!("{stem}.");
    let suffix = format!(".{ext}");
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| match p.file_name() {
            Some(name) => {
                let name = name.to_string_lossy().to_lowercase();
                name.starts_with(&prefix) && name.ends_with(&suffix)
            }
            None => false,
        })
}

fn us(meta: &cw_core::json::Json, key: &str) -> usize {
    meta.get(key).and_then(|x| x.as_usize()).unwrap()
}

fn s<'a>(meta: &'a cw_core::json::Json, key: &str) -> &'a str {
    meta.get(key).and_then(|x| x.as_str()).unwrap()
}

#[test]
fn golden_roundtrip_and_oracle() {
    let mut tables_checked = 0usize;
    let mut records_checked = 0usize;
    for corpus in corpora() {
        let oracle_raw = std::fs::read_to_string(corpus.join("oracle.json")).unwrap();
        let oracle = cw_core::json::parse(&oracle_raw).unwrap();
        let tables = match oracle.get("tables") {
            Some(t) => t,
            None => panic!("{corpus:?}: oracle has no tables map"),
        };

        for (key, meta) in tables.as_obj().expect("tables obj") {
            let fname = s(meta, "file");
            let path = corpus.join(fname);
            let bytes = std::fs::read(&path).unwrap();
            let ctx = format!("{:?}/{key}", corpus.file_name().unwrap());

            // 1. frozen source identity
            assert_eq!(
                cw_core::sha256::sha256_hex(&bytes),
                s(meta, "sha256"),
                "{ctx}: source drifted"
            );

            // 2. parse
            let table =
                Table::parse(&bytes).unwrap_or_else(|e| panic!("{ctx}: parse: {e}"));

            // 3. shape vs oracle
            assert_eq!(table.count(), us(meta, "count"), "{ctx}: count");
            assert_eq!(table.header_len as usize, us(meta, "header_len"), "{ctx}: header_len");
            assert_eq!(table.record_len as usize, us(meta, "record_len"), "{ctx}: record_len");
            assert_eq!(
                table.version,
                u8::from_str_radix(s(meta, "ver"), 16).unwrap(),
                "{ctx}: version byte"
            );

            // 4. schema equality
            let fields = meta.get("fields").and_then(|x| x.as_arr()).unwrap();
            assert_eq!(table.fields.len(), fields.len(), "{ctx}: field count");
            for f in 0..fields.len() {
                let w = fields[f].as_arr().unwrap();
                assert_eq!(table.fields[f].name, w[0].as_str().unwrap(), "{ctx} field {f} name");
                assert_eq!(
                    table.fields[f].kind,
                    w[1].as_str().unwrap().as_bytes()[0],
                    "{ctx} field {f} kind"
                );
                assert_eq!(table.fields[f].len as usize, w[2].as_usize().unwrap(), "{ctx} field {f} len");
                assert_eq!(table.fields[f].dec, w[3].as_usize().unwrap() as u8, "{ctx} field {f} dec");
            }

            // 5. sidecar sanity + canonical hashes (memo resolution included)
            let fpt = sidecar(&corpus, fname, "fpt").map(|p| Fpt::parse_path(p).unwrap());
            if meta.get("fpt").is_some() {
                assert!(fpt.is_some(), "{ctx}: oracle fpt but sidecar missing");
                let fpt = fpt.as_ref().unwrap();
                assert_eq!(fpt.block_size as usize, us(meta.get("fpt").unwrap(), "block_size"));
            }
            let sample = meta.get("sample").and_then(|x| x.as_arr()).unwrap();
            if table.count() > 0 {
                assert_eq!(
                    cw_core::canon::record_line(&table, 0, fpt.as_ref()),
                    sample[0].as_str().unwrap(),
                    "{ctx}: first canonical record line diverges"
                );
            }
            assert_eq!(
                cw_core::canon::records_sha(&table, fpt.as_ref()),
                s(meta, "records_sha"),
                "{ctx}: canonical all-records sha diverges"
            );

            // 6. THE no-op save: byte-stable re-emit
            assert_eq!(table.encode(), bytes, "{ctx}: encode(parse(x)) != x");

            // 7. deleted-row census
            let deleted = (0..table.count()).filter(|r| table.is_deleted(*r)).count();
            assert_eq!(deleted, us(meta, "deleted"), "{ctx}: deleted census");

            tables_checked += 1;
            records_checked += table.count();
        }
    }
    assert!(tables_checked >= 66, "checked {tables_checked} tables");
    assert!(records_checked > 50_000, "rows {records_checked}");
    eprintln!("golden OK: {tables_checked} tables, {records_checked} records");
}

#[test]
fn golden_manifest_and_contract_present() {
    let root = golden_root().join("..");
    assert!(root.join("MANIFEST.toml").is_file(), "MANIFEST.toml");
    assert!(root.join("README.md").is_file(), "README.md");
}

