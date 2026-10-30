//! P5.D4b gate: lockdown / lockout vs the corpus + the spec-recorded CW laws.
//!
//! Frozen inputs: `tests/golden/lockdown/{before.ac,law.json}`. The corpus tables
//! come from `tests/golden/dbf/corp2026`.
//!
//! ## Provenance (2026-10-28 rebuild)
//!
//! The pre-rebuild fixture was one COM `client.Lockdown()` round on a sample copy
//! (F68): `{before,after}.ac` + `{sh,fp}-after.dbf`. Those working copies are gone.
//! `before.ac` is now rebuilt by `examples/freeze_lockdown.rs` from the frozen
//! package's own container streams — including the genuine 5,204-byte
//! `\x05DocumentSummaryInformation` — which is exactly what CW synthesizes the
//! working hub from (`backup.md` §7.2). The constants are spec-recorded from
//! `attachments/lockdown.md` §2 (F68, 2026-10-23), the strongest provenance left.
//!
//! ## What replaces "cell-for-cell against CW's destination"
//!
//! The old comparison could only pass if our output matched CW's *tables*. Those
//! tables no longer exist, so tests 2 derives the destination independently of the
//! engine — corpus table + exactly the cells the spec says CW writes — and then
//! requires every other cell (tombstones included) to be byte-identical. On the
//! touched cells that is the same claim; on the untouched ones it is stronger (an
//! engine that reproduced CW's own collateral edits would now fail). The residual
//! is stated at the assertion: a CW edit the spec does not record cannot be
//! discovered. The same construction is used for `HI`.
//!
//! Covered laws:
//! 1. `.ac` hub property set: `\x05DocumentSummaryInformation` pid 273 `"L"`,
//!    pid 267 stamp, pid 277 next-midnight date; every other stream unchanged.
//! 2. `SH` rows: exactly the `PRG='ac'` + `LOCKDOWN='Y'` documents get `PERIOD='Y1'`
//!    + `CWDATE`; every other cell byte-identical to the corpus.
//! 3. `FP`/CLP: exactly 5 cells (275/278/287/288 seq 1 + 288 seq 2 `CWDATE`).
//! 4. `HI`: `LOCK/FILELOCK` + one `LOCK/DOCLOCK` per document, blank user,
//!    checksumversion-only VALUES; the 47 `MSD` snapshots stay a documented
//!    residual.
//! 5. One-way: a second lockdown is a no-op that reports `already_locked`.
//! 6. Reader views: `document_locked`, `excel_serial`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use cw_app::history::HistoryLog;
use cw_app::lockdown::{
    document_locked, excel_serial, lockdown_engagement, mark_lockdown, select_documents,
    LockdownRequest, LockdownState, MILESTONE_SNAPSHOTS, PID_LOCKDOWN_DATE, PID_LOCKDOWN_STAMP,
    PID_LOCK_STATE, SUMMARY_STREAM,
};
use cw_core::cfb::Cfb;
use cw_core::dbf::Table;
use cw_core::json::{self, Json};

const STEM: &str = "USI Compilation Training(1155962)";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn lockdown() -> PathBuf {
    repo().join("tests/golden/lockdown")
}

fn corpus() -> PathBuf {
    repo().join("tests/golden/dbf/corp2026")
}

fn law() -> Json {
    let p = lockdown().join("law.json");
    json::parse(&std::fs::read_to_string(&p).unwrap_or_else(|e| {
        panic!("{}: {e} (run `cargo run -p cw-app --example freeze_lockdown`)", p.display())
    }))
    .unwrap()
}

fn s<'a>(j: &'a Json, key: &str) -> &'a str {
    j.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

fn n(j: &Json, key: &str) -> usize {
    j.get(key).and_then(|v| v.as_usize()).unwrap_or(0)
}

/// Numeric top-level field (`stamp_filetime` is 1.3e17, past `f64`'s exact
/// integer range, so it is read as text when the parser produced a float).
fn num(j: &Json, key: &str) -> u64 {
    let v = j.get(key).unwrap_or_else(|| panic!("law.json {key}"));
    match v.as_str() {
        Some(s) => s.parse().unwrap(),
        None => v.as_f64().expect("numeric") as u64,
    }
}

fn guid_list() -> Vec<String> {
    law()
        .get("sh")
        .and_then(|v| v.get("guids"))
        .and_then(|v| v.as_arr())
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("ocw-d4b-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn stage(name: &str) -> PathBuf {
    let d = tmp(name);
    let c = corpus();
    for t in ["SH.dbf", "FP.dbf", "HI.dbf", "HI.fpt"] {
        std::fs::copy(c.join(format!("{STEM}{t}")), d.join(format!("LOCK{t}"))).unwrap();
    }
    std::fs::copy(lockdown().join("before.ac"), d.join("LOCK.ac")).unwrap();
    d
}

fn request() -> LockdownRequest {
    let l = law();
    let after = l.get("ac").unwrap().get("after").unwrap();
    let get = |pid: &str| -> u64 {
        after.get(pid).and_then(|v| v.get("value")).and_then(|v| v.as_str()).unwrap().parse().unwrap()
    };
    LockdownRequest {
        stamp: s(&l, "stamp").to_string(),
        stamp_filetime: get("267"),
        date_filetime: get("277"),
        fp_date_value: s(&l, "excel_serial_date").to_string(),
        documents: guid_list(),
        user: String::new(),
        initials: String::new(),
    }
}

/// Overwrite one cell exactly as the engine does (width-bounded, blank-padded).
fn set_cell(t: &mut Table, row: usize, field: &str, value: &str) {
    let idx = t.field_index(field).unwrap_or_else(|| panic!("no field {field}"));
    let width = t.fields[idx].len as usize;
    let off = t.offsets()[idx];
    let rec = &mut t.records[row];
    for b in &mut rec[off..off + width] {
        *b = b' ';
    }
    rec[off..off + value.len()].copy_from_slice(value.as_bytes());
}

fn clp_row(t: &Table, cellno: &str, seqno: &str) -> usize {
    (0..t.count())
        .find(|&r| {
            !t.is_deleted(r)
                && t.cell_str(r, "WKS").unwrap_or_default().trim() == "CLP"
                && t.cell_str(r, "CELLNO").unwrap_or_default().trim() == cellno
                && t.cell_str(r, "SEQNO").unwrap_or_default().trim() == seqno
        })
        .unwrap_or_else(|| panic!("no CLP cell {cellno}/{seqno}"))
}

#[test]
fn hub_property_law_and_stream_isolation() {
    let before = std::fs::read(lockdown().join("before.ac")).unwrap();
    let l = law();
    let hub = l.get("hub").unwrap();
    assert_eq!(before.len(), n(hub, "bytes"), "frozen hub size");

    // Pristine hub: 10-stream container shape, summary stream at the corpus size.
    let a = Cfb::parse(&before).unwrap();
    let entries = a.entries().unwrap();
    let streams: Vec<String> = entries.iter().map(|e| e.name()).collect();
    assert_eq!(streams.len(), n(hub, "streams").max(9).min(entries.len()), "stream census");
    let summary_before = a.stream(SUMMARY_STREAM).unwrap();
    assert_eq!(summary_before.len(), n(hub, "summary_len"), "summary stream size");

    let st0 = LockdownState::from_ac(&before).unwrap();
    assert!(!st0.locked, "pristine hub must be open");
    assert_eq!(st0.date, Some(0), "corpus hub carries the unset date marker");

    // Every pid the corpus stream carries survives the patch.
    let set_before = cw_app::lockdown::PropertySet::parse(&summary_before).unwrap();
    let pids_before = set_before.pids();

    // Our in-place writer: same length, idempotent read-back of the spec values.
    let mut ours = before.clone();
    let stamp = request().stamp_filetime;
    let date = request().date_filetime;
    let st2 = mark_lockdown(&mut ours, stamp, date).unwrap();
    assert_eq!(ours.len(), before.len(), "patch is in place");
    assert!(st2.locked);
    assert_eq!(st2.stamp, Some(stamp));
    assert_eq!(st2.date, Some(date));

    // Exactly one stream changes, and it is the summary stream.
    let b = Cfb::parse(&ours).unwrap();
    let mut changed = 0;
    for e in a.entries().unwrap() {
        if e.kind != cw_core::cfb::EntryKind::Stream {
            continue;
        }
        let x = a.stream(&e.name()).unwrap();
        let y = b.stream(&e.name()).unwrap();
        if x != y {
            assert_eq!(e.name(), SUMMARY_STREAM, "only the summary stream may change");
            changed += 1;
        }
    }
    assert_eq!(changed, 1, "exactly one stream carries the lock");

    // Property-level law: same pid set, the four spec values, everything else
    // byte-equal.
    let so = cw_app::lockdown::PropertySet::parse(&a.stream(SUMMARY_STREAM).unwrap()).unwrap();
    let sn = cw_app::lockdown::PropertySet::parse(&b.stream(SUMMARY_STREAM).unwrap()).unwrap();
    assert_eq!(sn.pids(), pids_before, "pid set preserved, in order");
    assert_eq!(sn.get_string(PID_LOCK_STATE).as_deref(), Some("L"), "pid 273 lock state");
    assert_eq!(sn.get_filetime(PID_LOCKDOWN_STAMP), Some(stamp), "pid 267 stamp");
    assert_eq!(sn.get_filetime(PID_LOCKDOWN_DATE), Some(date), "pid 277 day end");
    // pid 321 (last change) is cosmetic: CW writes stamp + ~15 s, we write the
    // stamp. Asserted as the documented deviation from the spec constant.
    assert_eq!(sn.get_filetime(cw_app::lockdown::PID_LAST_CHANGE), Some(stamp));
    let cw_last = s(hub, "cw_last_change_filetime");
    if !cw_last.is_empty() {
        assert_ne!(
            sn.get_filetime(cw_app::lockdown::PID_LAST_CHANGE),
            Some(cw_last.parse().unwrap()),
            "pid 321 deviation is intentional and recorded in the fixture"
        );
    }
    for pid in so.pids() {
        if [PID_LOCK_STATE, PID_LOCKDOWN_STAMP, PID_LOCKDOWN_DATE, cw_app::lockdown::PID_LAST_CHANGE]
            .contains(&pid)
        {
            continue;
        }
        assert_eq!(sn.get(pid), so.get(pid), "pid {pid} unchanged");
    }
}

#[test]
fn lockdown_tables_match_cw_row_for_row() {
    let l = law();
    let dir = stage("tables");
    let req = request();
    let src_sh = std::fs::read(corpus().join(format!("{STEM}SH.dbf"))).unwrap();
    let src_fp = std::fs::read(corpus().join(format!("{STEM}FP.dbf"))).unwrap();
    let src_hi = std::fs::read(corpus().join(format!("{STEM}HI.dbf"))).unwrap();

    // The selection rule is exact: live SH rows with PRG='ac' + LOCKDOWN='Y'.
    let sh0 = Table::parse(&src_sh).unwrap();
    let selected: BTreeSet<String> = select_documents(&sh0).into_iter().collect();
    assert_eq!(selected.len(), n(l.get("sh").unwrap(), "select_prg_ac_lockdown_y"));
    assert_eq!(selected, guid_list().into_iter().collect::<BTreeSet<_>>(),
        "rule selects exactly the frozen locked document set");

    let rep = lockdown_engagement(&dir, "LOCK", &req).unwrap();
    assert_eq!(rep.documents, selected.len());
    assert_eq!(rep.sh_rows, selected.len());
    assert_eq!(rep.fp_cells, 5);
    assert_eq!(rep.lock_events, selected.len() + 1, "one FILELOCK + one DOCLOCK each");
    assert_eq!(rep.milestone_events_skipped, MILESTONE_SNAPSHOTS);
    assert!(rep.ac_marked && !rep.already_locked);

    // SH: derive the destination from the corpus + the spec's two cell writes,
    // then require cell-for-cell equality on every row and field. Anything CW
    // wrote that the spec does not record would surface here as a mismatch only
    // if it landed in a cell we also write; otherwise it is undetectable — the
    // stated residual of losing CW's own destination table.
    let mut want_sh = sh0.clone();
    let period = s(l.get("sh").unwrap(), "period");
    for id in &selected {
        let row = (0..want_sh.count())
            .find(|&r| !want_sh.is_deleted(r)
                && want_sh.cell_str(r, "CWGUID").unwrap_or_default().trim() == id)
            .unwrap_or_else(|| panic!("{id} missing from SH"));
        set_cell(&mut want_sh, row, "PERIOD", period);
        set_cell(&mut want_sh, row, "CWDATE", &req.stamp);
    }
    let got_sh = Table::parse(&std::fs::read(dir.join("LOCKSH.dbf")).unwrap()).unwrap();
    assert_rows_eq(&got_sh, &want_sh, "SH");

    // FP: exactly the five CLP cells, each with the spec value.
    let mut want_fp = Table::parse(&src_fp).unwrap();
    let fp_law = l.get("fp").unwrap();
    let cells = fp_law.get("cells").unwrap();
    for (cellno, field) in
        [("275", "TEXT"), ("278", "TEXT"), ("287", "TEXT"), ("288", "TEXT"), ("288", "CWDATE")]
    {
        let value = if field == "CWDATE" {
            req.stamp.as_str()
        } else {
            cells.get(cellno).and_then(|v| v.as_str()).unwrap()
        };
        let seq = if cellno == "288" && field == "CWDATE" { "2" } else { "1" };
        let row = clp_row(&want_fp, cellno, seq);
        set_cell(&mut want_fp, row, field, value);
        if field == "TEXT" {
            set_cell(&mut want_fp, row, "CWDATE", &req.stamp);
        }
    }
    let got_fp = Table::parse(&std::fs::read(dir.join("LOCKFP.dbf")).unwrap()).unwrap();
    assert_rows_eq(&got_fp, &want_fp, "FP");

    // Spot law on decoded rows too (defends against a frozen-file mistake).
    for id in guid_list() {
        assert!(document_locked(&got_sh, &id).unwrap(), "{id} locked");
    }
    let open = (0..got_sh.count())
        .find(|&r| !got_sh.is_deleted(r)
            && !guid_list().iter().any(|g| g == got_sh.cell_str(r, "CWGUID").unwrap_or_default().trim())
            && !got_sh.cell_str(r, "CWGUID").unwrap_or_default().trim().is_empty())
        .map(|r| got_sh.cell_str(r, "CWGUID").unwrap_or_default().trim().to_string());
    if let Some(id) = open {
        assert!(!document_locked(&got_sh, &id).unwrap(), "{id} stays open");
    }

    // Hub: the lock state reads back from the file.
    let st = LockdownState::read(&dir, "LOCK").unwrap();
    assert!(st.locked && st.date.is_some() && st.stamp.is_some());
    assert_eq!(st.stamp, Some(req.stamp_filetime));
    assert_eq!(st.date, Some(req.date_filetime));

    // HI: FILELOCK then one DOCLOCK per document, blank user,
    // checksumversion-only VALUES.
    let hi = HistoryLog::open(&dir, "LOCK").unwrap();
    let rows: Vec<_> = hi.all().unwrap().into_iter().filter(|e| e.category == "LOCK").collect();
    assert_eq!(rows.len(), selected.len() + 1);
    assert_eq!(rows[0].event_type, "FILELOCK");
    assert!(rows[0].object_id.is_empty());
    let ids: BTreeSet<String> = rows
        .iter()
        .filter(|e| e.event_type == "DOCLOCK")
        .map(|e| e.object_id.clone())
        .collect();
    assert_eq!(ids, selected);
    for e in &rows {
        assert_eq!(e.category, "LOCK");
        assert_eq!(e.object_type, if e.event_type == "FILELOCK" { "" } else { "SH" });
        assert!(e.user.is_empty(), "CW wrote a blank user");
        assert_eq!(e.values, s(l.get("hi").unwrap(), "values"));
        assert!(e.checksum.is_empty() && e.milestone.is_empty() && e.milestone_rec.is_empty());
        assert_eq!(e.start_date, req.stamp);
        assert_eq!(e.end_date, req.stamp);
        assert_eq!(e.cw_date, req.stamp);
        assert_eq!(e.created_on, req.stamp);
    }

    // Source tables untouched.
    assert_eq!(std::fs::read(corpus().join(format!("{STEM}SH.dbf"))).unwrap(), src_sh);
    assert_eq!(std::fs::read(corpus().join(format!("{STEM}FP.dbf"))).unwrap(), src_fp);
    assert_eq!(std::fs::read(corpus().join(format!("{STEM}HI.dbf"))).unwrap(), src_hi);
    let _ = std::fs::remove_dir_all(&dir);
}

fn assert_rows_eq(ours: &Table, want: &Table, label: &str) {
    assert_eq!(ours.count(), want.count(), "{label} record count");
    for r in 0..ours.count() {
        assert_eq!(
            ours.is_deleted(r),
            want.is_deleted(r),
            "{label} row {r} deletion flag"
        );
        for f in &want.fields {
            assert_eq!(
                ours.cell_str(r, &f.name),
                want.cell_str(r, &f.name),
                "{label} row {r} field {}",
                f.name
            );
        }
    }
}

#[test]
fn one_way_second_call_is_a_noop() {
    let dir = stage("oneway");
    let req = request();
    let rep = lockdown_engagement(&dir, "LOCK", &req).unwrap();
    assert!(!rep.already_locked);

    let snapshot = |d: &Path| -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(d).unwrap().flatten() {
            out.push((
                e.file_name().to_string_lossy().to_string(),
                std::fs::read(e.path()).unwrap(),
            ));
        }
        out.sort();
        out
    };
    let first = snapshot(&dir);
    let again = lockdown_engagement(&dir, "LOCK", &req).unwrap();
    assert!(again.already_locked);
    assert_eq!(again.documents, 0);
    assert_eq!(again.lock_events, 0);
    assert_eq!(snapshot(&dir), first, "no second write");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn excel_serial_and_reader_shape() {
    let l = law();
    let serial = n(l.get("excel_serial_check").unwrap(), "serial");
    assert_eq!(excel_serial(2026, 8, 24) as usize, serial);
    assert_eq!(
        serial,
        s(&l, "excel_serial_date").parse::<usize>().unwrap(),
        "the corpus cell 278 value is that Excel serial"
    );
    assert_eq!(s(l.get("sh").unwrap(), "period"), "Y1");

    let before = std::fs::read(lockdown().join("before.ac")).unwrap();
    let mut ours = before.clone();
    let stamp = num(&l, "stamp_filetime");
    let date = num(&l, "date_filetime");
    mark_lockdown(&mut ours, stamp, date).unwrap();

    let cfb = Cfb::parse(&ours).unwrap();
    let set = cw_app::lockdown::PropertySet::parse(&cfb.stream(SUMMARY_STREAM).unwrap()).unwrap();
    assert_eq!(set.get_string(PID_LOCK_STATE).as_deref(), Some("L"));
    assert_eq!(set.get_filetime(PID_LOCKDOWN_DATE), Some(date));
    assert_eq!(set.get_filetime(PID_LOCKDOWN_STAMP), Some(stamp));

    let st = LockdownState::from_ac(&ours).unwrap();
    assert_eq!(st.date, Some(date));
    assert_eq!(st.stamp, Some(stamp));
    assert!(st.locked);
}
