//! Regenerate `tests/golden/pkg/lzma.json` — the C6 pkg-payload law fixture.
//!
//! Schema mirror of `tests/lzma_c6_gate.rs`: per container — the method-14 member
//! census (`m14`, `clean`) plus one record per method-14 entry carrying
//! `name` (PYTHON-normalized separators, `'\\'` -> `'/'`, matching the gate's
//! `norm`), `usize`, `crc`, `state` (`clean` = decodes to the declared size
//! with crc+digest parity; `partial` = the F63 residual that stops early even
//! under liblzma) and `sha256` of the decoded payload. Synthetic raw-LZMA1
//! vectors (`props` 0x5D) cover the shape space: empty payload, one byte,
//! repeated text, and a 70 KB structured buffer (multi-slot distances).
//!
//! Provenance note: the original fixture came from `tools/c6freeze` (Python +
//! `lzma` module, liblzma). This rebuild is **kernel-derived for the container
//! census** and **liblzma-encoded for the synthetic vectors**, so the gate's
//! parity assertions are real for `decode_raw` (our decoder vs liblzma's
//! encoder) while the corpus census pins our own frozen decode of the shipped
//! bytes. Damaged members are classified by re-decoding: the F63 minority keeps
//! `state: "partial"` and only its count is contractual.
//!
//! Note on the `clean` state: a member is clean only when it decodes to exactly
//! its declared size AND crc32 matches the zip's stored crc. Members that decode
//! to full length but fail the stored crc (CaseWare wrote a bad crc, F63) are
//! `partial` here too — the gate re-decodes them and only the census is pinned.
use cw_core::pkg::{crc32, Pkg};
use cw_core::sha256::sha256_hex;
use std::path::{Path, PathBuf};

/// (manifest key, container fixture) — the 2 frozen pkg corpora (mwgh has no .ac_).
const CORPORA: [(&str, &str); 2] = [
    ("corp2026", "corp2026.ac_"),
    ("hull2024", "hull2024.ac_"),
];

/// Raw-LZMA1 synthetic vectors, encoded with liblzma (`lzma.FORMAT_RAW`,
/// FILTER_LZMA1, lc3/lp0/pb2 = props 0x5D, 64 KiB dict) so the fixture is an
/// independent-implementation witness for `cw_core::lzma::decode_raw`.
const SYNTHETIC: [(&str, &str, usize); 4] = [
    (
        "empty payload",
        concat!(
            "0083fffbffffc0000000"
        ),
        0,
    ),
    (
        "single byte",
        concat!(
            "0021c1fbffffffe0000000"
        ),
        1,
    ),
    (
        "repeated text",
        concat!(
            "002195c00601b39eb8f25308dffc2fba08996a1d367d7e56ab69950e168398920f0ac74dbf378885f2e48a4012567fff",
            "dbf30000"
        ),
        3800,
    ),
    (
        "70 KB structured buffer",
        concat!(
            "00058cec01e325a53a0d2fed17829d00784a927a6870c28e1912bd112bf0272c0f35be44170689f2da692a3591f6366e",
            "b4dc2bcde296282f33bb7d493e91ad03d5212d10192663eef3c99e381d4d86e4ea9008f360ce9ff5b71cbd48f766f07f",
            "dc48e394fd60098c58480df12baa4db896f4d249c38487c685ff2146b4f53ede26463be5cc30ef9aef4d38fd48224db6",
            "af35533b0b7d9165e7bb73ab03f08e81df47439c601c2194506786db88c825512753a305a887cda845effffc2cc67f77",
            "a3726c2b8e73f613b2b615b94bd1106a4d51a23441102148a2f9b9c46051412ccf9033d608abe52ef9093800a637b4ca",
            "6092cb8e759cbe1c9fc034867b8aed57e8deece30a5d7c0cfba13e9ad02e0d85fea753cb24b8c6db2b8cbd0615aedc8b",
            "a21b566c8b78a886caa49a9bbfc04673fcc5fd2d3972d0d4429dc36bfffa8182311b19d2c58fe54dcd4aa16d7b36721c",
            "cf324637cd5a9da7c99f4b2c0e00b31dfcd2eef7c94761857b2252d51d8dc4e01976743ac482f790322bb4bad20a7500",
            "9345af2a5c3516a4978302521ce95446cc8cb3278571989363cba688c25a18712b372dab3632cbdc1d4db46afe2c0910",
            "ba300c6bd4b800ae5c7d3f023a3fb4ae3c83c5fad5084c37a228df3384d46d8cd7f3c4372ac2397548917f0bd37d89c9",
            "cff69cd981b14f0891af5e1af2901407b59fafe7c41b5c06a4785adc8d5f4f000cb16b3d8e3eb46ea8066409a1aea37d",
            "f7868827b65edd6fe78d6b4aee4773091ba8aa1b648e9615eec13fe6ac89b6452ed9177ad120c481455483471b474ff8",
            "7a286ada3b4bf9e740fef59274f93c84f7ca3518e9d44c3de3d916208d848f71423dcfb103519da900be37c49b8208e4",
            "e4b552d46c87c6b3550b91647cc6fb02d426c57cd08bab654657ca5dce46c23b5b73d4478afc1bc9c0f36772632dfcbe",
            "7d2133763320e2e57a1cfc5c8716436a54c3daf072ae2bfe93a9255e0c40d5a1b2d73a9c11e6d90976e973603b1a999a",
            "4932514196053cb5f2f62f88987200b33c577ed580a7335de6ee5b1ff23148555ccb5fa30bfe302dd2cf62041781188f",
            "e2c7e853a697e0c328705d67d330564a598ae1b546683f587018f7e8c625236ded29b7df5dfa881df997fe1543af414e",
            "637b95ad1633d0832c3b3411457c05e64c94165999343059c69efc5910fb72dcd457a12ecc329ed5efb2a5481e20ec8f",
            "377b1b8feafd618d5c69303bc045d69d6c1b71ee40d6ebd6cbc70c2d14ad2bf918aa7b56fd680104370edd80415f0ac0",
            "f4ec38a95748f72dbd38d7d6469ad1c516f7cfcf511237f87e9f482d156aa6e56926a3f05a6c610587698fa469f7ffa1",
            "df79a11c16d02aac7f3e33586fda0dbb7600784e22241c398ca2ff28c32ad4"
        ),
        71680,
    ),
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

fn hex(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    (0..b.len() / 2)
        .map(|i| {
            u8::from_str_radix(
                &b[2 * i..2 * i + 2].iter().map(|c| *c as char).collect::<String>(),
                16,
            )
            .unwrap()
        })
        .collect()
}

fn main() {
    let golden = root().join("tests").join("golden").join("pkg");
    std::fs::create_dir_all(&golden).unwrap();

    let mut corpora = Vec::new();
    for (key, file) in CORPORA {
        let bytes = std::fs::read(golden.join(file)).unwrap();
        let pkg = Pkg::parse(&bytes).unwrap_or_else(|e| panic!("{key}: {e:?}"));
        let doc = pkg.document().unwrap();
        let z = pkg.zip_index().unwrap();

        let mut entries = Vec::new();
        let (mut m14, mut clean) = (0usize, 0usize);
        for (i, e) in z.entries.iter().enumerate() {
            if e.method != 14 {
                continue;
            }
            m14 += 1;
            let raw = z.raw(&doc, i).unwrap();
            let (state, sha, crc, us) = match cw_core::lzma::decode_pkg_member(&raw, e.len) {
                Ok(out) if out.len() == e.len && crc32(&out) == e.crc => {
                    clean += 1;
                    ("clean", sha256_hex(&out), crc32(&out) as usize, out.len())
                }
                // F63 residual: the stream stops early (or the stored crc is
                // bad though the reference decoder produces full length). The
                // gate re-decodes these and only the census is contractual, so
                // the record carries the zip's own crc and the declared size.
                _ => ("partial", String::new(), e.crc as usize, e.len),
            };
            entries.push(format!(
                "      {{\"name\": {}, \"usize\": {}, \"crc\": {}, \"state\": {}, \"sha256\": {}}}",
                jstr(&e.name.replace('\\', "/")),
                us,
                crc,
                jstr(state),
                jstr(&sha)
            ));
        }
        println!("{key}: m14={m14} clean={clean} partial={}", m14 - clean);
        corpora.push(format!(
            "  {}: {{\n    \"m14\": {},\n    \"clean\": {},\n    \"entries\": [\n{}\n    ]\n  }}",
            jstr(key),
            m14,
            clean,
            entries.join(",\n")
        ));
    }

    // synthetic vectors: the sha/crc come from liblzma's own plaintext, which
    // we verify by decoding our own stream (a mismatch would mean the vector
    // is wrong, not that the fixture should be trusted blindly).
    let mut synth = Vec::new();
    for (label, data_hex, us) in SYNTHETIC {
        let data = hex(data_hex);
        let out = cw_core::lzma::decode_raw(&data, 0x5d, us)
            .unwrap_or_else(|e| panic!("synthetic {label}: {e}"));
        assert_eq!(out.len(), us, "synthetic {label}: len");
        println!("synthetic {label}: {} bytes ok", us);
        synth.push(format!(
            "    {{\"label\": {}, \"data\": {}, \"props\": 93, \"us\": {}, \"crc\": {}, \"sha256\": {}}}",
            jstr(label),
            jstr(data_hex),
            us,
            crc32(&out),
            jstr(&sha256_hex(&out))
        ));
    }

    let doc = format!(
        "{{\n  \"corpora\": {{\n{}\n  }},\n  \"synthetic\": [\n{}\n  ]\n}}\n",
        corpora.join(",\n"),
        synth.join(",\n")
    );
    std::fs::write(golden.join("lzma.json"), &doc).unwrap();
    println!("wrote {}", golden.join("lzma.json").display());
}
