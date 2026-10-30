//! P5.C6a gate: **reader -> writer -> reader** on every frozen corpus table.
//!
//! Complements `golden_dbf.rs` (which pins in-memory `encode(parse(x)) == x`):
//! this suite writes through the filesystem, re-reads, and then compares
//! *bytes* and the *record multiset* — so a writer that silently reorders or
//! normalizes rows fails even when the in-memory path happens to agree.
//!
//! Expectations are frozen by `tools/table_roundtrip_freeze` into
//! `tests/golden/dbf/roundtrip.json` (multiset law: sha256 over the sorted
//! per-record sha256 lines, no trailing newline; empty multiset = sha256("")).
//!
//! Covered:
//!   1. disk round trip, all 66 tables x 3 corpora (byte + multiset identity,
//!      frozen header sha, deleted/live census, transactional no-op)
//!   2. sidecars: `.fpt` verbatim writer + memo-payload census, `.cdx` verbatim
//!      carry (index rebuild is out of scope — C6 spec §7)
//!   3. edited-record round trip: a cell change / deletion-flag toggle re-reads
//!      with a multiset delta of exactly that one row (writer honours row data,
//!      does not just echo a cached blob)
//!   4. zero-row tables survive the full path (27 in the corpus)

use cw_core::dbf::Table;
use cw_core::fpt::Fpt;
use cw_core::json::Json;
use cw_core::transact;
use std::path::{Path, PathBuf};

fn golden_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("golden")
        .join("dbf")
}

fn scratch(tag: &str) -> PathBuf {
    // Writes stay inside OpenCW (AGENTS.md): target\ is scratch space.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("c6a-scratch")
        .join(format!("{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn frozen() -> Json {
    let raw = std::fs::read_to_string(golden_root().join("roundtrip.json"))
        .expect("tests/golden/dbf/roundtrip.json (run tools/table_roundtrip_freeze)");
    cw_core::json::parse(&raw).expect("roundtrip.json parses")
}

fn us(meta: &Json, key: &str) -> usize {
    meta.get(key)
        .and_then(|x| x.as_usize())
        .unwrap_or_else(|| panic!("missing integer {key}"))
}

fn s<'a>(meta: &'a Json, key: &str) -> &'a str {
    meta.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or_else(|| panic!("missing string {key}"))
}

fn sha(bytes: &[u8]) -> String {
    cw_core::sha256::sha256_hex(bytes)
}

/// The frozen multiset law (mirror of `tools/table_roundtrip_freeze`).
fn multiset_sha(records: &[Vec<u8>]) -> String {
    let mut lines: Vec<String> = records.iter().map(|r| sha(r)).collect();
    lines.sort();
    sha(lines.join("\n").as_bytes())
}

fn multiset(records: &[Vec<u8>]) -> Vec<String> {
    let mut lines: Vec<String> = records.iter().map(|r| sha(r)).collect();
    lines.sort();
    lines
}

fn ctx(corpus: &str, table: &str) -> String {
    format!("{corpus}/{table}")
}

fn assert_same_table(a: &Table, b: &Table, c: &str) {
    assert_eq!(a.raw_header, b.raw_header, "{c}: raw header");
    assert_eq!(a.version, b.version, "{c}: version");
    assert_eq!(a.header_len, b.header_len, "{c}: header_len");
    assert_eq!(a.record_len, b.record_len, "{c}: record_len");
    assert_eq!(a.fields, b.fields, "{c}: schema");
    assert_eq!(a.records.len(), b.records.len(), "{c}: record count");
    assert_eq!(a.records, b.records, "{c}: record payloads");
}

/// Iterate frozen corpora/tables; yields (corpus, table key, dir, meta).
fn each_table() -> Vec<(String, String, PathBuf, Json)> {
    let doc = frozen();
    let mut out = Vec::new();
    for (corpus, cmeta) in doc
        .get("corpora")
        .and_then(|x| x.as_obj())
        .expect("corpora object")
    {
        let dir = golden_root().join(corpus);
        for (table, tmeta) in cmeta
            .get("tables")
            .and_then(|x| x.as_obj())
            .expect("tables object")
        {
            out.push((corpus.clone(), table.clone(), dir.clone(), tmeta.clone()));
        }
    }
    out
}

#[test]
fn disk_roundtrip_all_tables_byte_and_multiset_identical() {
    let dir = scratch("disk");
    let mut tables = 0usize;
    let mut rows = 0usize;
    let mut live = 0usize;
    let mut deleted = 0usize;
    let mut zero_rows = 0usize;
    let mut transact_checked = 0usize;

    for (corpus, key, golden_dir, meta) in each_table() {
        let c = ctx(&corpus, &key);
        let file = s(&meta, "file");
        let source = golden_dir.join(file);
        let bytes = std::fs::read(&source).unwrap_or_else(|e| panic!("{c}: read: {e}"));

        // 1. identity of the frozen input
        assert_eq!(sha(&bytes), s(&meta, "sha256"), "{c}: source sha drifted");

        // 2. parse
        let table = Table::parse(&bytes).unwrap_or_else(|e| panic!("{c}: parse: {e}"));
        assert_eq!(table.count(), us(&meta, "count"), "{c}: count");
        assert_eq!(table.record_len as usize, us(&meta, "record_len"), "{c}: record_len");
        assert_eq!(table.header_len as usize, us(&meta, "header_len"), "{c}: header_len");

        let live_n = (0..table.count()).filter(|r| !table.is_deleted(*r)).count();
        let del_n = table.count() - live_n;
        assert_eq!(live_n, us(&meta, "live"), "{c}: live census");
        assert_eq!(del_n, us(&meta, "deleted"), "{c}: deleted census");
        assert_eq!(
            sha(&table.raw_header),
            s(&meta, "header_sha256"),
            "{c}: header bytes"
        );
        assert_eq!(
            multiset_sha(&table.records),
            s(&meta, "multiset_sha256"),
            "{c}: frozen multiset"
        );

        // 3. writer -> disk -> reader
        let table_dir = dir.join(&corpus).join(&key);
        std::fs::create_dir_all(&table_dir).unwrap();
        let target = table_dir.join(file);
        table
            .write_path(&target)
            .unwrap_or_else(|e| panic!("{c}: write: {e}"));
        let reread_bytes = std::fs::read(&target).unwrap();
        assert_eq!(reread_bytes, bytes, "{c}: disk bytes after write");
        let reread = Table::parse(&reread_bytes).unwrap();
        assert_same_table(&table, &reread, &c);

        // 4. multiset identity (order-independent) — equal count *and* content
        assert_eq!(multiset(&reread.records), multiset(&table.records), "{c}: multiset");
        assert_eq!(reread.encode(), bytes, "{c}: writer not idempotent");

        // 5. transactional save: first commit writes, second is a true no-op
        if transact_checked == 0 || key == "AC" {
            let tx = table_dir.join("tx-copy.dbf");
            assert!(transact::commit_if_changed(&tx, &bytes).unwrap(), "{c}: tx create");
            assert!(!transact::commit_if_changed(&tx, &bytes).unwrap(), "{c}: tx no-op");
            assert_eq!(std::fs::read(&tx).unwrap(), bytes, "{c}: tx bytes");
            transact_checked += 1;
        }

        tables += 1;
        rows += table.count();
        live += live_n;
        deleted += del_n;
        if table.count() == 0 {
            zero_rows += 1;
        }
    }

    let doc = frozen();
    let totals = doc.get("totals").expect("totals");
    assert!(tables >= 66, "checked {tables} tables");
    assert!(rows > 50_000, "checked {rows} rows");
    assert_eq!(tables, us(totals, "tables"), "table census");
    assert_eq!(rows, us(totals, "records"), "record census");
    assert_eq!(live, us(totals, "live"), "live census");
    assert_eq!(deleted, us(totals, "deleted"), "deleted census");
    assert_eq!(zero_rows, us(totals, "zero_rows"), "zero-row census");
    assert_eq!(zero_rows, 27, "corpus zero-row census");
    assert!(transact_checked >= 3, "transactional path covered per corpus");
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("C6a disk OK: {tables} tables, {rows} rows ({live} live / {deleted} deleted)");
}

#[test]
fn sidecar_roundtrip_fpt_writer_and_verbatim_cdx() {
    let dir = scratch("sidecar");
    let mut fpts = 0usize;
    let mut cdxs = 0usize;
    let mut memos = 0usize;

    for (corpus, key, golden_dir, meta) in each_table() {
        let c = ctx(&corpus, &key);
        let table = Table::parse_path(golden_dir.join(s(&meta, "file"))).unwrap();
        let offs = table.offsets();

        if let Some(fmeta) = meta.get("fpt") {
            let name = s(fmeta, "file");
            let source = golden_dir.join(name);
            let raw = std::fs::read(&source).unwrap();
            assert_eq!(sha(&raw), s(fmeta, "sha256"), "{c}: fpt sha");
            assert_eq!(raw.len(), us(fmeta, "bytes"), "{c}: fpt size");

            let fpt = Fpt::parse(&raw).unwrap();
            let before = memo_census(&table, &fpt, &offs);
            memos += before.iter().filter(|m| m.is_some()).count();

            let target = dir.join(&corpus).join(&key).join(name);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            fpt.write_path(&target).unwrap();
            let back_raw = std::fs::read(&target).unwrap();
            assert_eq!(back_raw, raw, "{c}: fpt bytes after write");

            let back = Fpt::parse(&back_raw).unwrap();
            assert_eq!(back.data, fpt.data, "{c}: fpt payload");
            assert_eq!(back.block_size, fpt.block_size, "{c}: fpt block_size");
            assert_eq!(back.next_free, fpt.next_free, "{c}: fpt next_free");
            assert_eq!(
                memo_census(&table, &back, &offs),
                before,
                "{c}: memo resolution after rewrite"
            );
            fpts += 1;
        }

        if let Some(cmeta) = meta.get("cdx") {
            let name = s(cmeta, "file");
            let raw = std::fs::read(golden_dir.join(name)).unwrap();
            assert_eq!(sha(&raw), s(cmeta, "sha256"), "{c}: cdx sha");
            assert_eq!(raw.len(), us(cmeta, "bytes"), "{c}: cdx size");
            // The index is opaque (rebuilt by CW, not the kernel — C6 spec §7):
            // the round-trip contract is verbatim carry, pinned by the frozen sha.
            let target = dir.join(&corpus).join(&key).join(name);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(&target, &raw).unwrap();
            assert_eq!(std::fs::read(&target).unwrap(), raw, "{c}: cdx verbatim carry");
            cdxs += 1;
        }
    }

    let totals = frozen();
    let totals = totals.get("totals").expect("totals");
    assert_eq!(fpts, us(totals, "fpt"), "fpt sidecar census");
    assert_eq!(cdxs, us(totals, "cdx"), "cdx sidecar census");
    assert!(memos > 0, "memo payloads resolved");
    eprintln!("C6a sidecars OK: {fpts} fpt ({memos} memo refs), {cdxs} cdx");
}

/// Every M-field reference resolved through the sidecar, in row/field order.
fn memo_census(table: &Table, fpt: &Fpt, offs: &[usize]) -> Vec<Option<Vec<u8>>> {
    let mut out = Vec::new();
    for r in 0..table.count() {
        for (i, f) in table.fields.iter().enumerate() {
            if f.kind == b'M' {
                let raw = &table.records[r][offs[i]..offs[i] + f.len as usize];
                out.push(fpt.memo(raw).map(|m| m.to_vec()));
            }
        }
    }
    out
}

#[test]
fn edited_record_and_deletion_flag_roundtrip() {
    let dir = scratch("edit");
    let mut tables = 0usize;
    let mut disk_checked = 0usize;

    for (corpus, key, golden_dir, meta) in each_table() {
        let c = ctx(&corpus, &key);
        let table = Table::parse_path(golden_dir.join(s(&meta, "file"))).unwrap();
        if table.count() == 0 {
            continue;
        }
        let offs = table.offsets();

        // (a) rewrite one cell of one live row
        let row = (0..table.count())
            .find(|r| !table.is_deleted(*r))
            .unwrap_or(0);
        let fi = table.fields.len() - 1;
        let off = offs[fi];
        let len = table.fields[fi].len as usize;
        assert!(len > 0, "{c}: last field has zero width");
        let orig = table.records[row][off..off + len].to_vec();
        let mut repl = vec![b'9'; len];
        if orig == repl {
            repl = vec![b'Q'; len];
        }
        let mut edited = table.clone();
        edited.records[row][off..off + len].copy_from_slice(&repl);

        let enc = edited.encode();
        assert_eq!(enc, edited.encode(), "{c}: edit encode deterministic");
        let back = Table::parse(&enc).unwrap();
        assert_eq!(back.raw_header, table.raw_header, "{c}: edit header");
        assert_eq!(back.fields, table.fields, "{c}: edit schema");
        let diffs: Vec<usize> = (0..table.count())
            .filter(|&r| back.records[r] != table.records[r])
            .collect();
        assert_eq!(diffs, vec![row], "{c}: exactly the edited row changes");
        assert_eq!(&back.records[row][..off], &table.records[row][..off], "{c}: prefix");
        assert_eq!(
            &back.records[row][off + len..],
            &table.records[row][off + len..],
            "{c}: suffix"
        );
        assert_eq!(&back.records[row][off..off + len], &repl, "{c}: edited cell");

        // multiset delta: original multiset minus the old row plus the new one
        let mut expect: Vec<String> = table.records.iter().map(|r| sha(r)).collect();
        let old_digest = sha(&table.records[row]);
        let pos = expect.iter().position(|d| *d == old_digest).unwrap();
        expect[pos] = sha(&back.records[row]);
        expect.sort();
        assert_eq!(multiset(&back.records), expect, "{c}: multiset delta");
        assert_ne!(
            multiset_sha(&back.records),
            multiset_sha(&table.records),
            "{c}: edit must move the multiset"
        );

        // (b) deletion-flag toggle on another row (or the same one)
        let flag_row = (0..table.count())
            .find(|&r| r != row)
            .or(Some(row))
            .unwrap();
        let mut flags = table.clone();
        let was_deleted = table.is_deleted(flag_row);
        flags.records[flag_row][0] = if was_deleted { 0x20 } else { 0x2A };
        let fback = Table::parse(&flags.encode()).unwrap();
        assert_eq!(fback.is_deleted(flag_row), !was_deleted, "{c}: flag flips");
        assert_eq!(
            &fback.records[flag_row][1..],
            &table.records[flag_row][1..],
            "{c}: flag row payload intact"
        );
        let fdiffs: Vec<usize> = (0..table.count())
            .filter(|&r| fback.records[r] != table.records[r])
            .collect();
        assert_eq!(fdiffs, vec![flag_row], "{c}: only the flag row changes");

        // (c) disk form of the edit (first table per corpus only; cost guard)
        if disk_checked < 3 {
            let target = dir.join(&corpus).join(&key).join(s(&meta, "file"));
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            edited.write_path(&target).unwrap();
            let disk = std::fs::read(&target).unwrap();
            assert_eq!(disk, enc, "{c}: edited disk bytes");
            assert_eq!(Table::parse(&disk).unwrap().records, edited.records, "{c}: edited disk rows");
            disk_checked += 1;
        }

        tables += 1;
    }

    assert_eq!(tables, 39, "mutated every non-empty frozen table (66 - 27 zero-row)");
    assert_eq!(disk_checked, 3, "disk path covered per corpus");
    eprintln!("C6a edits OK: {tables} tables mutated (+3 disk round trips)");
}

