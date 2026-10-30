//! Regenerate tests/golden/pkg fixtures: the 2 frozen containers + manifest.json.
//!
//! Schema mirror of `tests/container_gate.rs`: per corpus — sha256/size, the CFB
//! stream census (name/len/sha256), the structural shape (sector/mini/cutoff),
//! and the zip table (name/method/crc/csize/usize/local_ofs).
use cw_core::pkg::Pkg;
use std::path::{Path, PathBuf};

/// (manifest key, package path under expanded/) — the 2 pkg corpora; mwgh ships
/// expanded-only (no .ac_), matching the original freeze.
const CORPORA: [(&str, &str); 2] = [
    ("corp2026", "../files-2/USI Compilation Training(1155962).ac_"),
    ("hull2024", "../Files/2024 Hull Lift Truck Review(1148441).ac_"),
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
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
    let golden = root().join("tests").join("golden").join("pkg");
    std::fs::create_dir_all(&golden).unwrap();
    let mut corpus_json = String::new();

    for (i, (key, rel)) in CORPORA.iter().enumerate() {
        let src = root().join(rel); // rel carries its own ../ (packages live above the workspace)
        let bytes = std::fs::read(&src).unwrap();
        let pkg = Pkg::parse(&bytes).unwrap_or_else(|e| panic!("{key}: {e:?}"));

        // copy fixture
        let dst = golden.join(format!("{key}.ac_"));
        std::fs::write(&dst, &bytes).unwrap();

        // stream census (name-keyed in the gate; emit sorted by name)
        let entries = pkg.cfb.entries().unwrap();
        let mut named: Vec<(String, usize, String)> = entries
            .iter()
            .map(|e| {
                let data = pkg.cfb.stream(&e.name()).unwrap();
                (e.name(), e.len, cw_core::sha256::sha256_hex(&data))
            })
            .collect();
        named.sort();

        // zip table (positional)
        let z = pkg.zip_index().unwrap();
        let mut zip_entries = String::new();
        for (zi, e) in z.entries.iter().enumerate() {
            if zi > 0 {
                zip_entries.push(',');
            }
            zip_entries.push_str(&format!(
                "\n        {{\"name\": {}, \"method\": {}, \"crc\": {}, \"csize\": {}, \"usize\": {}, \"local_ofs\": {}}}",
                jstr(&e.name),
                e.method,
                e.crc,
                e.compressed_len,
                e.len,
                e.local_ofs
            ));
        }

        let mut streams = String::new();
        for (si, (name, len, sha)) in named.iter().enumerate() {
            if si > 0 {
                streams.push(',');
            }
            streams.push_str(&format!(
                "\n        {{\"name\": {}, \"len\": {}, \"sha256\": {}}}",
                jstr(name),
                len,
                jstr(sha)
            ));
        }

        if i > 0 {
            corpus_json.push_str(",\n  ");
        }
        corpus_json.push_str(&format!(
            "\"{key}\": {{\n    \"sha256\": {}, \"size\": {},\n    \"streams\": [{streams}\n    ],\n    \"cfb\": {{\"sector_shift\": 12, \"mini_shift\": 6, \"mini_cutoff\": 4096}},\n    \"zip\": {{\"count\": {}, \"entries\": [{zip_entries}\n    ]}}\n  }}",
            jstr(&cw_core::sha256::sha256_hex(&bytes)),
            bytes.len(),
            z.entries.len()
        ));
        println!("{key}: streams={} zip={}", named.len(), z.entries.len());
    }

    let doc = format!("{{\n  \"corpora\": {{\n  {corpus_json}\n  }}\n}}\n");
    std::fs::write(golden.join("manifest.json"), &doc).unwrap();
    println!("manifest written");
}
