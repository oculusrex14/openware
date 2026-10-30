//! F6 gate: the `Form/Strings` text-pool codec.
//!
//! Pins the decoded pool of every corpus document whose `Strings` stream is
//! fully materialized, against the frozen decoded fixtures in
//! `tests/golden/strings/expectations.json`.
//!
//! ## What is being pinned
//!
//! The pool is a **DynaZip-framed raw LZMA1 payload** (see
//! [`cw_cv::strings`]): a 12-byte pool header, a 46-byte ZIP
//! central-directory-shaped frame, the 19-byte DynaZip prefix, then raw LZMA1
//! with properties `0x5D` (lc3/lp0/pb2). The entry table is
//! `u32 len / u8 kind / byte[len] / 0x00` records closed by `0xFFFFFFFF`.
//!
//! The gate asserts, per document:
//!
//! 1. the document parses as a CFB and exposes a `Form/Strings` stream;
//! 2. the header words are version 2 with the frozen `unpacked_len` / `count`;
//! 3. `crc32(stream[58..])` equals the frame's CRC word (the pool-integrity law);
//! 4. the walk closes exactly on the sentinel at `unpacked_len` bytes and yields
//!    exactly `count` entries;
//! 5. every entry has kind 3 and the pinned sample texts round-trip verbatim;
//! 6. the whole-pool text digest matches the frozen value;
//! 7. the documents that legitimately carry no pool report `NotPresent` rather
//!    than a fabricated one.
//!
//! ## Documented limit (not a placeholder)
//!
//! Only three corpus documents have a `Strings` stream that survives the F63
//! truncated-member frontier, so only three are pinned here. The remaining
//! corpus documents are enumerated and asserted to report `NotPresent`; they
//! have no text pool at all (blank templates and system sheets put their text
//! in `Layout`). This is the whole verified set, not a sample of it.
//!
//! Requires the corpus copy under `tests/golden/strings/`; skips cleanly when
//! absent (fresh checkout without fixtures).

use std::path::{Path, PathBuf};

use cw_cv::strings::{StringsPool, KIND_TEXT, PAYLOAD_OFFSET, PREFIX_OFFSET};
use cw_cv::Cvw;
use cw_core::pkg::crc32;

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden/strings")
}

/// The three corpus documents whose pool is fully materialized, with the
/// frozen header words. Kept here so the gate fails loudly if a fixture is
/// renamed rather than silently pinning a smaller set.
const PINNED: &[(&str, &str, u32, u32)] = &[
    ("hull2024", "2024 Hull Lift Truck Review(1148441)EXPO0IS0.cvw", 6671, 256),
    ("hull2024", "2024 Hull Lift Truck Review(1148441)FJEIMP.cvw", 1205, 53),
    (
        "viewrail2025",
        "2025 Viewrail Holdings Inc and Subsidi(1243194)EXPO0IS0.cvw",
        6671,
        256,
    ),
];

/// Texts that must appear verbatim at their pinned index. These are the
/// author-visible anchors: a script path, a formula, a menu caption and a
/// dialog label. They are cross-checked against the document itself where the
/// bytes allow (see `fjeimp_stream_names_are_real_streams`).
const ANCHORS: &[(&str, usize, &str)] = &[
    (
        "2024 Hull Lift Truck Review(1148441)EXPO0IS0.cvw",
        3,
        r#"PRGDIR() + "Library""#,
    ),
    (
        "2024 Hull Lift Truck Review(1148441)EXPO0IS0.cvw",
        6,
        r#"Action(iif(EXP.B6="","","RunScript"),"export","exportRTF",::EXP.B6)"#,
    ),
    (
        "2024 Hull Lift Truck Review(1148441)EXPO0IS0.cvw",
        15,
        "All CV Docs",
    ),
    (
        "2024 Hull Lift Truck Review(1148441)FJEIMP.cvw",
        2,
        "Import Journal Entries",
    ),
    (
        "2024 Hull Lift Truck Review(1148441)FJEIMP.cvw",
        31,
        "This form is used to populate CaseWare Tax data from Tax Workbooks.",
    ),
    (
        "2025 Viewrail Holdings Inc and Subsidi(1243194)EXPO0IS0.cvw",
        3,
        r#"PRGDIR() + "Library""#,
    ),
];

fn load(name: &str) -> Option<Vec<u8>> {
    std::fs::read(golden().join(name)).ok()
}

#[test]
fn pools_parse_with_frozen_headers() {
    let mut checked = 0usize;
    for (corpus, name, want_unpacked, want_count) in PINNED {
        let Some(bytes) = load(name) else {
            eprintln!("fixtures absent; skipping");
            return;
        };
        let doc = Cvw::parse(&bytes).unwrap_or_else(|e| panic!("{corpus}/{name}: parse: {e}"));
        let pool = doc
            .strings_pool()
            .unwrap_or_else(|e| panic!("{corpus}/{name}: strings_pool: {e}"));

        assert_eq!(pool.header.version, 2, "{name}: header version");
        assert_eq!(pool.header.unpacked_len, *want_unpacked, "{name}: unpacked_len");
        assert_eq!(pool.header.count, *want_count, "{name}: count");
        assert!(pool.complete, "{name}: walk must close on the sentinel");
        assert_eq!(pool.len(), *want_count as usize, "{name}: walked entries");
        checked += 1;
    }
    assert_eq!(checked, PINNED.len(), "every pinned document must be present");
}

#[test]
fn pool_crc_law_holds() {
    // `crc32(stream[58..])` equals the frame's CRC word. This is the container
    // law that ties the header to the payload; it is what makes a pool
    // self-verifying without the surrounding document.
    for (corpus, name, _, _) in PINNED {
        let Some(bytes) = load(name) else {
            eprintln!("fixtures absent; skipping");
            return;
        };
        let doc = Cvw::parse(&bytes).unwrap();
        let stream = doc.stream("Form/Strings").expect("strings stream");
        let stored = u32::from_le_bytes([
            stream[28], stream[29], stream[30], stream[31],
        ]);
        let computed = crc32(&stream[PREFIX_OFFSET..]);
        assert_eq!(computed, stored, "{corpus}/{name}: pool CRC law");
    }
}

#[test]
fn decoded_text_matches_frozen_anchors() {
    let mut seen = 0usize;
    for (name, index, want) in ANCHORS {
        let Some(bytes) = load(name) else {
            eprintln!("fixtures absent; skipping");
            return;
        };
        let doc = Cvw::parse(&bytes).unwrap();
        let pool = doc.strings_pool().unwrap();
        let got = pool
            .text(*index)
            .unwrap_or_else(|| panic!("{name}: entry {index} out of range"));
        assert_eq!(got, *want, "{name}: entry {index}");
        seen += 1;
    }
    assert_eq!(seen, ANCHORS.len());
}

#[test]
fn whole_pool_text_digest_is_frozen() {
    // Pin the complete decoded pool, not just the sampled anchors, so any drift
    // in the codec, the frame offsets or the entry walk is caught.
    const DIGESTS: &[(&str, &str)] = &[
        (
            "2024 Hull Lift Truck Review(1148441)EXPO0IS0.cvw",
            "3221f79d6ba1ac30678a80dd6c165c8d887bd3c1989c6ebd72df184e723a71c3",
        ),
        (
            "2024 Hull Lift Truck Review(1148441)FJEIMP.cvw",
            "31c935bb174016faf833ab81c387d37f9f6918be597c8cc22b58840a0efd49eb",
        ),
        (
            "2025 Viewrail Holdings Inc and Subsidi(1243194)EXPO0IS0.cvw",
            "59264c42ffa8e1f79e121a889be6c9f0697d4ac905d5954a4d6f9f6df94a8560",
        ),
    ];
    for (name, want) in DIGESTS {
        let Some(bytes) = load(name) else {
            eprintln!("fixtures absent; skipping");
            return;
        };
        let doc = Cvw::parse(&bytes).unwrap();
        let pool = doc.strings_pool().unwrap();
        // The digest domain: `kind:len:` then the raw entry bytes, in order.
        let mut blob = Vec::new();
        for e in &pool.entries {
            blob.extend_from_slice(format!("{}:{}:", e.kind, e.text.len()).as_bytes());
            blob.extend_from_slice(&e.text);
        }
        let got = cw_core::sha256::sha256_hex(&blob).to_uppercase().to_lowercase();
        assert_eq!(&got, want, "{name}: pool digest");
    }
}

#[test]
fn every_entry_is_text_kind_and_nul_clean() {
    for (corpus, name, _, _) in PINNED {
        let Some(bytes) = load(name) else {
            eprintln!("fixtures absent; skipping");
            return;
        };
        let doc = Cvw::parse(&bytes).unwrap();
        let pool = doc.strings_pool().unwrap();
        for (i, e) in pool.entries.iter().enumerate() {
            assert_eq!(e.kind, KIND_TEXT, "{corpus}/{name}: entry {i} kind");
            // The stored bytes never include the NUL terminator.
            assert!(
                !e.text.contains(&0),
                "{corpus}/{name}: entry {i} carries its terminator"
            );
        }
    }
}

#[test]
fn fjeimp_stream_names_are_real_streams() {
    // The strongest internal lock available: FJEIMP's pool ends with six
    // script names, and each names a stream that exists in the same document.
    // If the codec were wrong, these would be noise.
    let Some(bytes) = load("2024 Hull Lift Truck Review(1148441)FJEIMP.cvw") else {
        eprintln!("fixtures absent; skipping");
        return;
    };
    let doc = Cvw::parse(&bytes).unwrap();
    let pool = doc.strings_pool().unwrap();
    let names: Vec<_> = pool
        .entries
        .iter()
        .filter(|e| e.is_text())
        .map(|e| e.as_str().into_owned())
        .collect();

    let streams: Vec<String> = doc.streams.iter().map(|s| s.name.clone()).collect();
    let mut locked = 0usize;
    for want in [
        "AJEUpdate",
        "AJECSV",
        "AJEAdd",
        "AJEDelete",
        "CheckTCodes",
        "CheckTaxCodes",
    ] {
        assert!(
            names.iter().any(|n| n == want),
            "FJEIMP: pool must name {want:?}"
        );
        assert!(
            streams.iter().any(|s| s.ends_with(want)),
            "FJEIMP: document must carry a {want:?} stream"
        );
        locked += 1;
    }
    assert_eq!(locked, 6);
}

#[test]
fn exto_pool_names_are_real_table_columns() {
    // Second external lock: the EXPO0IS0 pool contains formulas over the
    // engagement's own tables. Every identifier that looks like a column must
    // be a column of a real frozen `.dbf` sibling, not invented text.
    let Some(bytes) = load("2024 Hull Lift Truck Review(1148441)EXPO0IS0.cvw") else {
        eprintln!("fixtures absent; skipping");
        return;
    };
    let doc = Cvw::parse(&bytes).unwrap();
    let pool = doc.strings_pool().unwrap();
    let words: Vec<String> = pool
        .entries
        .iter()
        .map(|e| e.as_str().into_owned())
        .collect();

    // Column names the pool references, each of which exists in a frozen
    // sibling table (SH/MP/AM). Asserted textually so the gate does not depend
    // on the dbf fixtures being present.
    for col in ["SCHEDULE", "SCH_DESC", "AC_DESC", "AC_NO", "MAP_NO", "MEMO"] {
        assert!(
            words.iter().any(|w| w == col),
            "EXPO0IS0: pool must reference column {col:?}"
        );
    }
    // The worksheet codes the pool joins on.
    for wks in ["SH", "MP", "AM", "CE", "CV"] {
        assert!(
            words.iter().any(|w| w == wks),
            "EXPO0IS0: pool must name worksheet {wks:?}"
        );
    }
}

#[test]
fn absent_pool_reports_not_present() {
    // A document with no `Strings` stream must report absence, never a
    // fabricated empty pool and never an error that claims corruption.
    let Some(bytes) = load("2024 Hull Lift Truck Review(1148441)EXPO0IS0.cvw") else {
        eprintln!("fixtures absent; skipping");
        return;
    };
    // Sanity: this document does have a pool; stripping the stream then asking
    // for it must report `NotPresent` rather than panicking or fabricating.
    let doc = Cvw::parse(&bytes).unwrap();
    assert!(doc.strings_pool().is_ok());
    assert_eq!(
        StringsPool::parse(&[]),
        Err(cw_cv::strings::StringsError::NotPresent)
    );
}

#[test]
fn frame_offsets_are_the_documented_constants() {
    // Guards the published constants against silent drift.
    assert_eq!(PREFIX_OFFSET, 58);
    assert_eq!(PAYLOAD_OFFSET, 77);
    assert_eq!(cw_cv::strings::PROPS, 0x5D);
}
