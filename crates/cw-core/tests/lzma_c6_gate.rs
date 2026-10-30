//! C6 golden gate: the kernel LZMA1-decodes the pkg payload members.
//!
//! Fixtures: `tests/golden/pkg/lzma.json` (generator `examples/freeze_lzma.rs`)
//! on top of the verbatim containers. For every method-14 member the gate
//! re-derives the framing law (19-byte DynaZip prefix, props at [18], stream
//! at [19]) and, for the clean set, asserts length + crc32 + sha256 parity
//! with the frozen decode. The partial-decode minority is count-pinned
//! (F63 residual - they stop early even under the reference decoder; see the
//! measurement in `cw_core::lzma`'s module docs).

use cw_core::json::Json;
use cw_core::pkg::{Pkg, crc32};
use cw_core::sha256::sha256_hex;

fn golden_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("golden")
        .join("pkg")
}

fn str<'a>(j: &'a Json, k: &str) -> &'a str {
    j.get(k).and_then(|x| x.as_str()).unwrap()
}

fn us(j: &Json, k: &str) -> usize {
    j.get(k).and_then(|x| x.as_usize()).unwrap()
}

fn hex(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    (0..b.len() / 2)
        .map(|i| u8::from_str_radix(&b[2 * i..2 * i + 2].iter().map(|c| *c as char).collect::<String>(), 16).unwrap())
        .collect()
}

fn manifest() -> Json {
    let s = std::fs::read_to_string(golden_root().join("lzma.json")).unwrap();
    cw_core::json::parse(&s).unwrap()
}

/// Corpus law: every clean member decodes to its declared size with crc and
/// digest parity; clean/partial census matches the freeze exactly.
#[test]
fn pkg_lzma_corpus_law() {
    let m = manifest();
    let corpora = m.get("corpora").unwrap().as_obj().unwrap();
    assert_eq!(corpora.len(), 2, "the 2 frozen pkg corpora");
    for (key, meta) in corpora {
        let path = golden_root().join(format!("{key}.ac_"));
        let bytes = std::fs::read(&path).unwrap();
        let pkg = Pkg::parse(&bytes).unwrap_or_else(|e| panic!("{key}: {e}"));
        let doc = pkg.document().unwrap();
        let z = pkg.zip_index().unwrap();

        let entries = meta.get("entries").and_then(|x| x.as_arr()).unwrap();
        let mut clean_seen = 0usize;
        let mut partial_seen = 0usize;
        for e in entries {
            // Python's zipfile normalizes stored '\\' separators to '/', so the
            // frozen manifest carries the normalized spelling; the pkg index
            // keeps the raw stored name (the writer needs it byte-exact).
            let norm = |s: &str| s.replace('\\', "/");
            let name = norm(str(e, "name"));
            let want_us = us(e, "usize");
            let want_crc = us(e, "crc") as u32;
            let idx = z
                .entries
                .iter()
                .position(|x| norm(&x.name) == name)
                .unwrap_or_else(|| panic!("{key}: zip entry '{name}' missing"));
            let raw = z.raw(&doc, idx).unwrap();
            assert_eq!(raw.len(), z.entries[idx].compressed_len, "{key}/{name}: csize");

            match str(e, "state") {
                "clean" => {
                    let out = cw_core::lzma::decode_pkg_member(&raw, want_us)
                        .unwrap_or_else(|er| panic!("{key}/{name}: {er}"));
                    assert_eq!(out.len(), want_us, "{key}/{name}: len");
                    assert_eq!(crc32(&out), want_crc, "{key}/{name}: crc");
                    assert_eq!(sha256_hex(&out), str(e, "sha256"), "{key}/{name}: sha");
                    clean_seen += 1;
                }
                "partial" => {
                    // F63: reference-decoder parity - our decoder may stop
                    // early too; only the census is contractual here.
                    let _ = cw_core::lzma::decode_pkg_member(&raw, want_us);
                    partial_seen += 1;
                }
                s => panic!("{key}/{name}: unknown state {s}"),
            }
        }
        assert_eq!(clean_seen, us(meta, "clean"), "{key}: clean census");
        assert_eq!(clean_seen + partial_seen, us(meta, "m14"), "{key}: m14 census");
    }
}

/// Synthetic raw-LZMA1 vectors (python encoder, props 0x5D): the decoder
/// handles the empty payload, a byte, repeated text and a 70 KB buffer
/// (multi-slot distance coding + EOS-marked stream).
#[test]
fn lzma_synthetic_vectors_roundtrip() {
    let m = manifest();
    for (i, v) in m.get("synthetic").unwrap().as_arr().unwrap().iter().enumerate() {
        let data = hex(str(v, "data"));
        let props = v.get("props").and_then(|x| x.as_usize()).unwrap() as u8;
        let out = cw_core::lzma::decode_raw(&data, props, us(v, "us"))
            .unwrap_or_else(|e| panic!("vector {i}: {e}"));
        assert_eq!(out.len(), us(v, "us"), "vector {i} len got {} want {}", out.len(), us(v, "us"));
        assert_eq!(crc32(&out), us(v, "crc") as u32, "vector {i} crc");
        assert_eq!(sha256_hex(&out), str(v, "sha256"), "vector {i} sha");
    }
}

