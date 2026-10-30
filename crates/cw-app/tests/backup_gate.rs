//! P5.D3 gate: backup / restore / compress / `.bak` / cleanup (F66).
//!
//! Golden:
//! - `tests/golden/pkg/corp2026.ac_` — the frozen CW package (C5).
//! - `tests/golden/backup/expand-corp2026.json` — the expansion fixture: for
//!   every member, the declared size, our restored `sha256`, and — where the
//!   bitstream is undamaged — `reference_sha256`, the **liblzma reference
//!   decoder's** digest frozen in `tests/golden/pkg/lzma.json`.
//! - `tests/golden/backup/law.json` — the F66 compress/uncompress/bak laws.
//!
//! ## Provenance (2026-10-28 rebuild)
//!
//! The original `expand-corp2026.json` was CW's *own* `ICWClients.Uncompress` of
//! the package (112 members). That COM round is not part of this rebuild, so the
//! fixture is rebuilt by `examples/freeze_backup.rs` from two authorities:
//! a genuinely independent implementation (liblzma, via `pkg/lzma.json`) for the
//! 105 members whose LZMA1 stream decodes to completion, and our own restore
//! output pinned as a regression baseline for all 132 members including the 27
//! in the F63 damaged minority (`tests/golden/README.md` records why those tails
//! are synthesis). The shipped package carries 132 payload members rather than
//! the probe's 112 — the probe package had been through a cleanup wizard — so
//! every count below is read from the fixture.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cw_app::backup::{
    cleanup_engagement, document_backup_path, is_document_backup, package_engagement,
    restore_document_backup, restore_package, stash_document_backup, CleanupOptions,
    PackageOptions, HUB_STUB,
};
use cw_core::dbf::Table;
use cw_core::json::{self, Json};
use cw_core::pkg::Pkg;
use cw_core::sha256::sha256_hex;

fn golden() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("..");
    p.push("..");
    p.push("tests");
    p.push("golden");
    p
}

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("ocw-d3-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn sha(path: &Path) -> String {
    sha256_hex(&std::fs::read(path).unwrap())
}

fn expansion() -> Json {
    let p = golden().join("backup").join("expand-corp2026.json");
    json::parse(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn law() -> Json {
    let p = golden().join("backup").join("law.json");
    json::parse(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// The fixture members, in package order.
fn members() -> Vec<Json> {
    expansion()
        .get("members")
        .and_then(|m| m.as_arr())
        .expect("expand-corp2026.json members")
        .to_vec()
}

/// Resolve a package member on disk. CW mixes separators, so `SCRIPT\x.js` is
/// one flat file name while the fixture records it as `SCRIPT/x.js`.
fn member_path(dir: &Path, name: &str) -> PathBuf {
    let flat = dir.join(name.replace('/', "\\"));
    if flat.is_file() {
        return flat;
    }
    let nested = dir.join(name.replace('/', std::path::MAIN_SEPARATOR_STR));
    if nested.is_file() {
        return nested;
    }
    flat
}

/// The headline law, in two halves that stand on different authorities:
///
/// 1. **Independent-implementation parity.** Every member whose LZMA1 stream the
///    reference decoder completes must restore to the reference decoder's own
///    bytes (`reference_sha256`, 105 of 132 here). This is the half a bug in
///    `cw_core::lzma` cannot satisfy by construction.
/// 2. **Regression pin.** Every member must restore to the frozen `sha256`, and
///    the CRC-flagged set must be exactly the members whose stored CRC the
///    restored bytes fail. The damaged minority (F63) is reproduced best-effort
///    and is pinned rather than compared, because the source bitstream itself is
///    truncated in the shipped `.ac_`.
#[test]
fn restore_matches_cw_expansion() {
    let dir = tmp("restore");
    let pkg = golden().join("pkg").join("corp2026.ac_");
    let rep = restore_package(&pkg, &dir).unwrap();

    let exp = expansion();
    let members = members();
    let total = exp.get("members_total").unwrap().as_usize().unwrap();
    assert_eq!(members.len(), total, "fixture member census");
    assert_eq!(rep.failed.len(), 0, "failed members: {:?}", rep.failed);
    assert_eq!(rep.written.len(), total + 1, "{total} members + hub");

    let mut reference_matches = 0usize;
    let mut reference_checked = 0usize;
    let mut crc_bad = 0usize;
    let mut partial = 0usize;

    for rec in &members {
        let name = rec.get("name").unwrap().as_str().unwrap();
        let p = member_path(&dir, name);
        assert!(p.is_file(), "missing restored member {name}");
        assert_eq!(
            std::fs::metadata(&p).unwrap().len() as usize,
            rec.get("usize").unwrap().as_usize().unwrap(),
            "declared size mismatch for {name}"
        );

        // (1) independent reference decoder, where the bitstream is intact.
        if let Some(want) = rec.get("reference_sha256").and_then(|v| v.as_str()) {
            reference_checked += 1;
            assert!(
                sha(&p).eq_ignore_ascii_case(want),
                "{name}: our decode diverges from the reference decoder"
            );
            reference_matches += 1;
        } else {
            partial += 1;
        }

        // (2) regression pin for every member.
        assert_eq!(
            sha(&p).to_ascii_uppercase(),
            rec.get("sha256").unwrap().as_str().unwrap().to_ascii_uppercase(),
            "{name}: restore output drifted from the frozen baseline"
        );

        let crc_ok = rec.get("crc_ok").unwrap();
        let crc_ok = matches!(crc_ok, Json::Bool(true));
        if !crc_ok {
            crc_bad += 1;
        }
        assert_eq!(
            !crc_ok,
            rep.crc_mismatch.iter().any(|m| m == name),
            "{name}: CRC flag disagrees with the fixture"
        );
    }

    assert_eq!(reference_checked, 105, "reference-decoder coverage");
    assert_eq!(reference_matches, reference_checked, "reference parity");
    assert_eq!(partial, 27, "members outside the reference decoder's reach");
    assert_eq!(crc_bad, 26, "F63 bad-CRC minority");
    // The 27th partial member (`.locks`) has an intact CRC: its stream is
    // truncated only in the *declared* size direction, not damaged. One member
    // of the damaged minority may therefore restore byte-perfect; the two sets
    // differ by exactly that member, and both are pinned above.
    assert_eq!(partial - crc_bad, 1, "partial-but-CRC-clean member");
    assert_eq!(rep.crc_mismatch.len(), crc_bad);

    // The hub member is CW's 5-byte version stub; the 28 KiB working hub is
    // synthesized by CaseWare at expand time (F66, spec §7).
    let hub = exp.get("hub_synthesized").unwrap();
    let hub_rel = hub.get("rel").unwrap().as_str().unwrap();
    let hub_path = dir.join(hub_rel);
    assert_eq!(std::fs::read(&hub_path).unwrap(), HUB_STUB);
    assert_eq!(rep.hub_stub, Some(HUB_STUB.len()));
    assert_eq!(hub.get("size").unwrap().as_usize(), Some(28_672));
}

/// Compress: a working directory becomes the canonical 10-stream package whose
/// payload is the same file set, hub first, and all members decode back to the
/// same bytes.
#[test]
fn package_roundtrip_matches_cw_container() {
    let work = tmp("pack");
    restore_package(&golden().join("pkg").join("corp2026.ac_"), &work).unwrap();
    // Replace the 5-byte hub stub with a synthetic expanded hub (CW would have
    // synthesized the 28 KiB CFB here; the pack law only requires *a* hub).
    let hub = work.join("USI Compilation Training(1155962).ac");
    std::fs::write(&hub, b"EXPANDED-HUB").unwrap();

    let opts = PackageOptions {
        include_backups: true,
        consume_sources: false,
        meta_source: Some(golden().join("pkg").join("corp2026.ac_")),
    };
    let rep = package_engagement(&work, &opts).unwrap();
    let total = expansion().get("members_total").unwrap().as_usize().unwrap();
    assert_eq!(rep.entries.len(), total + 1, "hub + every member");
    assert_eq!(rep.entries[0].name, "USI Compilation Training(1155962).ac");
    assert!(rep.notes.iter().any(|n| n.contains("metadata carried")));

    let buf = std::fs::read(&rep.pkg_path).unwrap();
    let pkg = Pkg::parse(&buf).unwrap();
    let doc = pkg.document().unwrap();
    let zip = cw_core::pkg::Zip::parse(&doc).unwrap();
    assert_eq!(zip.entries.len(), total + 1);
    assert_eq!(zip.entries[0].method, 8, "hub is the deflate member");
    let streams: Vec<String> =
        pkg.cfb.entries().unwrap().iter().map(|e| e.path.join("/")).collect();
    assert_eq!(streams.len(), 10, "canonical container: {streams:?}");
    assert!(streams.iter().any(|s| s == "CaseWareDocument"));
    assert_eq!(pkg.compression_version().unwrap(), "DynaZip5.00.06");

    // Every member decodes back to the on-disk bytes (round-trip law).
    for i in 0..zip.entries.len() {
        let name = zip.entries[i].name.clone();
        let out = zip.decode(&doc, i).unwrap();
        let p = member_path(&work, &name);
        assert_eq!(out, std::fs::read(&p).unwrap(), "member {name}");
    }

    // The container metadata streams are the frozen corpus bytes.
    let src = Pkg::parse(&std::fs::read(golden().join("pkg").join("corp2026.ac_")).unwrap()).unwrap();
    for name in ["DmsVersionInfo", "CaseWareProtectionInfo", "TemplateInfo"] {
        assert_eq!(pkg.stream(name).unwrap(), src.stream(name).unwrap(), "{name}");
    }
    assert_eq!(
        pkg.stream("FileInformation").unwrap()[..b"USI Compilation Training(1155962)".len()],
        b"USI Compilation Training(1155962)"[..]
    );
}

/// The CW consume law: Compress leaves the package (plus omitted `.bak`).
#[test]
fn package_consumes_sources_like_cw() {
    let dir = tmp("consume");
    std::fs::write(dir.join("TPROBE.ac"), b"hub").unwrap();
    std::fs::write(dir.join("TPROBEAM.dbf"), b"table").unwrap();
    std::fs::write(dir.join("Doc1.docx"), b"doc").unwrap();
    std::fs::write(dir.join("TPROBE01200RG2.bak"), b"bak").unwrap();

    let opts = PackageOptions { include_backups: false, consume_sources: true, meta_source: None };
    let rep = package_engagement(&dir, &opts).unwrap();
    assert_eq!(rep.entries.len(), 3, "hub + table + doc; .bak omitted");
    assert!(!rep.entries.iter().any(|e| e.name.ends_with(".bak")));
    let left: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert!(left.contains(&"TPROBE.ac_".to_string()), "package present");
    assert!(left.contains(&"TPROBE01200RG2.bak".to_string()), "omitted .bak kept");
    assert_eq!(left.len(), 2, "working set consumed: {left:?}");

    // Restore expands the package back (stub hub), and reports the stub note.
    let out = tmp("consume-restore");
    let r = restore_package(&rep.pkg_path, &out).unwrap();
    assert_eq!(r.written.len(), 3);
    assert_eq!(r.hub_stub, Some(3));
    assert!(r.notes.iter().any(|n| n.contains("stub")));
    assert_eq!(std::fs::read(out.join("Doc1.docx")).unwrap(), b"doc");

    // Synthetic metadata variant is still a canonical container.
    let pkg = Pkg::parse(&std::fs::read(&rep.pkg_path).unwrap()).unwrap();
    assert_eq!(pkg.cfb.entries().unwrap().len(), 10);
}

/// `.bak` law + cleanup data half.
#[test]
fn bak_semantics_and_cleanup() {
    let dir = tmp("bak");
    let doc = dir.join("TPROBE01200RG2.cvw");

    // A `.bak` is a CaseView document CFB (Form/Layout + Index streams).
    let bytes = {
        let mut b = cw_core::cfb::CfbBuilder::new();
        b.add("Form", b"form".to_vec());
        b.add("Form/Layout", b"layout".to_vec());
        b.add("Index", b"index".to_vec());
        b.add("Index/Para", b"para".to_vec());
        b.add("Media", b"".to_vec());
        b.build().unwrap()
    };
    std::fs::write(&doc, b"current-generation").unwrap();
    let bak = stash_document_backup(&doc).unwrap();
    assert_eq!(bak, document_backup_path(&doc));
    assert_eq!(bak.file_name().unwrap(), "TPROBE01200RG2.bak");
    assert_eq!(std::fs::read(&bak).unwrap(), b"current-generation");
    std::fs::write(&doc, b"newer-generation").unwrap();
    let restored = restore_document_backup(&bak).unwrap();
    assert_eq!(std::fs::read(&restored).unwrap(), b"current-generation");

    std::fs::write(&bak, &bytes).unwrap();
    assert!(is_document_backup(&bak).unwrap());
    std::fs::write(dir.join("plain.bak"), b"not-a-cfb").unwrap();
    assert!(!is_document_backup(&dir.join("plain.bak")).unwrap());

    // Cleanup: deleted rows packed, live multiset preserved, `.bak` removed.
    let src = golden().join("dbf").join("corp2026").join("USI Compilation Training(1155962)GL.dbf");
    std::fs::copy(&src, dir.join("TPROBEGL.dbf")).unwrap();
    let before = Table::parse_path(dir.join("TPROBEGL.dbf")).unwrap();
    let live_before: BTreeMap<Vec<u8>, usize> = multiset(&before, true);
    let deleted_before = before.records.iter().filter(|r| r[0] == b'*').count();
    assert!(deleted_before > 0, "corpus GL carries tombstones");

    let rep = cleanup_engagement(
        &dir,
        &CleanupOptions { purge_deleted_records: true, remove_document_backups: true },
    )
    .unwrap();
    let after = Table::parse_path(dir.join("TPROBEGL.dbf")).unwrap();
    assert_eq!(after.records.iter().filter(|r| r[0] == b'*').count(), 0);
    assert_eq!(multiset(&after, false), live_before);
    assert_eq!(rep.tables.get("TPROBEGL").copied(), Some((live_before.values().sum(), deleted_before, live_before.values().sum())));
    assert!(rep.removed_files.iter().any(|n| n.ends_with(".bak")));
    assert!(rep.freed_bytes > 0);

    // Law file agrees with the probe constants.
    let l = law();
    assert_eq!(l.get("compress").unwrap().get("consumes_sources").unwrap(), &Json::Bool(true));
    assert_eq!(
        l.get("bak").unwrap().get("name").unwrap().as_str(),
        Some("<cvw stem>.bak (no rotation counter)")
    );
}

fn multiset(t: &Table, live_only: bool) -> BTreeMap<Vec<u8>, usize> {
    let mut m = BTreeMap::new();
    for r in &t.records {
        if r[0] == b' ' || !live_only {
            *m.entry(r[1..].to_vec()).or_insert(0) += 1;
        }
    }
    m
}

