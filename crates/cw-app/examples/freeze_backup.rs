//! Freeze `tests/golden/backup/{expand-corp2026.json,law.json}` — P5.D3.
//!
//! ## Provenance (2026-10-28 rebuild)
//!
//! Two authorities feed this fixture, and the gate distinguishes them:
//!
//! 1. **Reference decoder (independent implementation).** `tests/golden/pkg/lzma.json`
//!    (see `examples/freeze_lzma.rs`) records, per package member, the declared
//!    `usize`, the stored CRC and — for members whose method-14 LZMA bitstream
//!    decodes to completion under liblzma — a `sha256` of those bytes with
//!    `state: "clean"`. Those 105 digests are *not* produced by `cw_core`; they are
//!    the independent-implementation parity half of this fixture.
//! 2. **Our own expansion** (`cw_app::backup::restore_package`), recorded for every
//!    member as a regression pin. It must reproduce authority 1 on all clean
//!    members and is the only record for the F63 damaged minority (`state:
//!    "partial"`: liblzma aborts mid-stream, `cw_core::lzma` zero-fills to the
//!    declared size, so those tails are synthesis — see `tests/golden/README.md`).
//!
//! The original fixture was CW's *own* `ICWClients.Uncompress` of the package
//! (112 members, `tests\golden\backup\expand-corp2026.json`). That COM round is
//! not part of this rebuild, and the shipped package carries 132 payload members
//! (not 112) — the probe package had been through a cleanup wizard. The counts
//! below are the shipped package's.
use std::path::{Path, PathBuf};

use cw_app::backup::{restore_package, HUB_STUB};
use cw_core::json;
use cw_core::sha256::sha256_hex;

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
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn main() {
    let golden = root().join("tests").join("golden");
    let pkg = golden.join("pkg").join("corp2026.ac_");
    let out = golden.join("backup");
    std::fs::create_dir_all(&out).unwrap();

    // Restore into a scratch dir under the repo (writes stay inside the project).
    let scratch = root().join("target").join("freeze-backup");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    let rep = restore_package(&pkg, &scratch).unwrap();
    assert!(rep.failed.is_empty(), "failed members: {:?}", rep.failed);

    // Reference-decoder authority (independent implementation).
    let lz = json::parse(&std::fs::read_to_string(golden.join("pkg").join("lzma.json")).unwrap())
        .unwrap();
    let refs = lz
        .get("corpora")
        .and_then(|c| c.get("corp2026"))
        .and_then(|c| c.get("entries"))
        .and_then(|e| e.as_arr())
        .expect("lzma.json corpora.corp2026.entries");

    let mut members = String::new();
    let mut clean = 0usize;
    let mut partial = 0usize;
    for e in refs {
        let name = e.get("name").and_then(|v| v.as_str()).unwrap_or_default();
        let usize_ = e.get("usize").and_then(|v| v.as_usize()).unwrap_or(0);
        let crc = e.get("crc").and_then(|v| v.as_usize()).unwrap_or(0);
        let state = e.get("state").and_then(|v| v.as_str()).unwrap_or_default();
        let reference_sha = e.get("sha256").and_then(|v| v.as_str()).unwrap_or_default();

        // `lzma.json` normalizes CW's mixed separators (`SCRIPT\x.js` -> `SCRIPT/x.js`),
        // but the entry really is one flat file name; look it up both ways.
        let bytes = std::fs::read(scratch.join(name.replace('/', "\\")))
            .or_else(|_| std::fs::read(scratch.join(name.replace('/', std::path::MAIN_SEPARATOR_STR))))
            .or_else(|_| std::fs::read(scratch.join(name)))
            .unwrap_or_else(|err| panic!("restore did not write {name}: {err}"));
        assert_eq!(bytes.len(), usize_, "{name}: declared size");
        let ours = sha256_hex(&bytes);
        let crc_ok = cw_core::pkg::crc32(&bytes) as usize == crc;
        if state == "clean" {
            clean += 1;
            assert!(
                ours.eq_ignore_ascii_case(reference_sha),
                "{name}: our decode diverges from the reference decoder"
            );
        } else {
            partial += 1;
        }

        if !members.is_empty() {
            members.push_str(",\n");
        }
        members.push_str(&format!(
            "    {{\"name\": {}, \"usize\": {usize_}, \"state\": {}, \"sha256\": {}, \"reference_sha256\": {}, \"crc_ok\": {}}}",
            jstr(name),
            jstr(state),
            jstr(&ours),
            if reference_sha.is_empty() { "null".to_string() } else { jstr(&reference_sha.to_ascii_uppercase()) },
            crc_ok
        ));
    }

    // The hub is CW's 5-byte version stub; the real 28 KiB CFB is synthesized by
    // CaseWare at expand time (spec backup.md §3, F66).
    let hub_rel = format!("{}.ac", rep.stem);
    let hub_path = std::fs::read_dir(&scratch)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_name().unwrap().to_string_lossy().to_ascii_lowercase()
                == hub_rel.to_ascii_lowercase()
        })
        .unwrap_or_else(|| panic!("restore did not write the hub {hub_rel}"));
    let hub_bytes = std::fs::read(&hub_path).unwrap();
    assert_eq!(hub_bytes, HUB_STUB, "restore writes the package stub");
    let hub_sha = sha256_hex(&hub_bytes);

    let flagged = rep.crc_mismatch.len();
    let expand = format!(
        r#"{{
  "provenance": "2026-10-28 rebuild. `reference_sha256` is the liblzma reference-decoder digest frozen in tests/golden/pkg/lzma.json (independent implementation) and is non-null only for `state: clean` members. `sha256` is our own restore output, pinned as a regression baseline for every member including the F63 damaged minority (see tests/golden/README.md). Replace-with: CW's own ICWClients.Uncompress, which is not part of this rebuild; the shipped package carries 132 payload members, not the probe's 112.",
  "package": "tests/golden/pkg/corp2026.ac_",
  "stem": {},
  "members_total": {},
  "members_clean": {clean},
  "members_partial": {partial},
  "written": {},
  "failed": 0,
  "crc_mismatch": {flagged},
  "hub_synthesized": {{"rel": {}, "stub_len": {}, "size": 28672, "stub_sha256": {}}},
  "members": [
{members}
  ]
}}
"#,
        jstr(&rep.stem),
        refs.len(),
        rep.written.len(),
        jstr(&hub_rel),
        HUB_STUB.len(),
        jstr(&hub_sha)
    );
    std::fs::write(out.join("expand-corp2026.json"), expand).unwrap();

    // ---- law.json: the F66 compress / .bak laws the gate asserts.
    let law = r#"{
  "provenance": "F66 laws from attachments/backup.md (v1.0) — COM-measured on staged sandbox copies and corroborated by the shipped package shape. Only the two assertions the gate reads are recorded here; the rest of §3 is asserted structurally by the gate itself.",
  "compress": {
    "output_name": "<stem>.ac_ beside the hub (same directory)",
    "consumes_sources": true,
    "include_backups": {"packed": true, "removed_from_disk": true},
    "omit_backups": {"packed": false, "kept_on_disk": true},
    "compress2_flags": {"cOmitBackups": 1, "cOmitIndices": 2, "cOmitBackupAndIndicies": 3, "cCompressAllSubFolders": 4},
    "payload": "hub first (method 8), every other file method 14 (our writer stores deflate; round-trip parity only, F66 §8.4)",
    "container_streams": 10,
    "compression_version": "DynaZip5.00.06",
    "file_information_bytes": 261
  },
  "bak": {
    "name": "<cvw stem>.bak (no rotation counter)",
    "shape": "CFB with the same CaseView stream set as the sibling .cvw (Form, Form/Layout, Index, ...)",
    "live_corpus_pairs": "1 .bak per CaseView document"
  },
  "cleanup": {
    "tables": "live rows kept, tombstones dropped in place; live row multiset preserved exactly (F64 CopyAll pack law)",
    "files": ".bak siblings removed when requested; bytes freed reported"
  }
}
"#;
    std::fs::write(out.join("law.json"), law).unwrap();
    let _ = std::fs::remove_dir_all(&scratch);
    println!(
        "wrote {}/{{expand-corp2026.json,law.json}}: {} members ({} clean / {} partial), written {}",
        out.display(),
        refs.len(),
        clean,
        partial,
        rep.written.len()
    );
}
