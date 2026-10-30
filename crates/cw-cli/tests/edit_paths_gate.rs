//! P5.I I2 gate: interactive editing paths end-to-end.
//!
//! Every path runs against a **throwaway copy** of the frozen golden corpus
//! (`tests/golden/dbf`), never the corpus itself:
//!
//! - `edit_account`   `AM` description + the 12 property columns
//! - `edit_grouping`  `AM.GROUPn <- MP.MAP_NO` assign / clear
//! - `edit_entry`     balanced AJE create + same-shape replace in `GL`
//!
//! The gate locks the shell contract: preview writes nothing, commit writes
//! only the targeted table through the atomic transaction lane, invalid edits
//! leave bytes untouched, and every written table re-encodes byte-identically.

use std::fs;
use std::path::{Path, PathBuf};

use cw_app::accounts::{AmAccount, Chart};
use cw_app::adjustments::{Adjustments, EntryType};
use cw_app::groups::{MapNode, MpTable};
use cw_app::renumber::RenumberPlan;
use cw_cli::{
    edit_account, edit_entry, edit_grouping, render_renumber, renumber_dialog, renumber_scope,
    Engagement,
};
use cw_core::dbf::Table;

fn source_dir(tag: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("golden")
        .join("dbf")
        .join(tag)
}

fn stamp() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

/// Process-wide counter: nanosecond stamps alone can collide between the tests
/// running in parallel threads (they share `process::id()` and often the tag), and
/// a shared scratch directory makes one test's `GL` edit visible to the other.
static SCRATCH_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Copy only the tables a path needs into a fresh temp directory.
fn copy_tables(tag: &str, suffixes: &[&str]) -> PathBuf {
    let src = source_dir(tag);
    let n = SCRATCH_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dst = std::env::temp_dir().join(format!(
        "ocw-i2-{tag}-{}-{}-{n}",
        std::process::id(),
        stamp()
    ));
    let _ = fs::remove_dir_all(&dst);
    fs::create_dir(&dst).expect("scratch dir is exclusive to this test");
    for entry in fs::read_dir(&src).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        if suffixes
            .iter()
            .any(|s| lower.ends_with(&format!("{}.dbf", s.to_ascii_lowercase())))
        {
            fs::copy(entry.path(), dst.join(&name)).unwrap();
        }
    }
    dst
}

fn bytes(eng: &Engagement, suffix: &str) -> Vec<u8> {
    fs::read(eng.table_path(suffix).unwrap()).unwrap()
}

fn assert_roundtrip(raw: &[u8]) {
    let table = Table::parse(raw).expect("written table parses");
    assert_eq!(table.encode(), raw, "written table must re-encode byte-identically");
}

#[test]
fn account_edit_previews_commits_and_refuses_atomically() {
    let dir = copy_tables("corp2026", &["AM", "MP", "GL"]);
    let eng = Engagement::open(&dir).unwrap();
    let chart = Chart::from_table(&eng.table("AM").unwrap()).unwrap();
    let acct = chart.posting().next().expect("posting account").clone();
    let ac_no = acct.ac_no.trim().to_string();
    let am_before = bytes(&eng, "AM");
    let mp_before = bytes(&eng, "MP");
    let gl_before = bytes(&eng, "GL");

    // preview: in-memory change, zero bytes written
    let preview = edit_account(
        &eng,
        &ac_no,
        Some("I2 edited description"),
        &[("HIDDEN".to_string(), "Y".to_string())],
        false,
    )
    .expect("preview account edit");
    assert!(preview.changed, "preview reports a pending change");
    assert!(!preview.written, "preview never commits");
    assert_eq!(bytes(&eng, "AM"), am_before);
    assert!(preview.render("hdr").contains("preview only"));

    // commit
    let out = edit_account(
        &eng,
        &ac_no,
        Some("I2 edited description"),
        &[
            ("HIDDEN".to_string(), "Y".to_string()),
            ("DC".to_string(), "C".to_string()),
        ],
        true,
    )
    .expect("commit account edit");
    assert!(out.changed && out.written);
    assert!(out.render("hdr").contains("WROTE"));

    let t = Table::parse(&bytes(&eng, "AM")).unwrap();
    let a = AmAccount::from_table(&t, acct.row).unwrap();
    assert_eq!(a.desc, "I2 edited description");
    assert!(a.props.hidden.is_yes());
    assert_eq!(a.props.dc.as_str(), "C");
    assert_roundtrip(&bytes(&eng, "AM"));
    assert_eq!(bytes(&eng, "MP"), mp_before, "account edit touches AM only");
    assert_eq!(bytes(&eng, "GL"), gl_before, "account edit touches AM only");

    // unknown property column is refused without touching bytes
    let committed = bytes(&eng, "AM");
    let err = edit_account(
        &eng,
        &ac_no,
        None,
        &[("NOT_A_COLUMN".to_string(), "Y".to_string())],
        true,
    );
    assert!(err.is_err());
    assert_eq!(bytes(&eng, "AM"), committed, "refused edit leaves bytes intact");

    // re-applying the same values is a true no-op
    let noop = edit_account(
        &eng,
        &ac_no,
        Some("I2 edited description"),
        &[
            ("HIDDEN".to_string(), "Y".to_string()),
            ("DC".to_string(), "C".to_string()),
        ],
        true,
    )
    .unwrap();
    assert!(!noop.changed && !noop.written, "no-op edit does not write");
    assert_eq!(bytes(&eng, "AM"), committed);

    let _ = fs::remove_dir_all(&dir);
}

/// Corpus + scheme the grouping path runs on.
///
/// Pre-rebuild this test used `hull2024` scheme `2`. `hull2024/MP` is a truncated
/// LZMA member: its readable prefix is 785 of 2,837 records (`guid_law_gate.rs`
/// `DAMAGED`) and holds only schemes `1` and `8` — scheme `2` exists exclusively in
/// the fabricated tail. Repointing at `mwgh2025` is not a weakening: it is the
/// corpus whose `MP` prefix *and* `AM` grouping columns are both authentic, and
/// whose 40 `AM.GROUP8` codes all resolve to nodes inside the readable prefix
/// (verified — no assignment target is invented). `hull2024`'s only populated
/// column, `GROUP2`, references 52 codes that resolve nowhere, so the law cannot
/// be exercised there at all.
const GROUP_CORPUS: &str = "mwgh2025";
const GROUP_SCHEME: &str = "8";
const GROUP_DIGIT: u8 = 8;

/// Readable-row prefix of that corpus' `MP` (`cw-core/tests/guid_law_gate.rs`
/// `DAMAGED` pins mwgh2025/MP at 787 on the record-flag/timestamp test; the first
/// row the *engine* cannot decode is 785 — `CONV="."` — so the law is stated over
/// rows 0..785). Truncating the throwaway copy is what lets the decode be
/// asserted at all: `MpTable::from_table` decodes every physical row, and rows past
/// the LZMA frontier are zero-fill synthesis.
const GROUP_MP_FRONTIER: usize = 785;

/// Replace a copied table with its first `frontier` records (a self-consistent
/// readable-prefix image, same header/schema/record length).
fn truncate_table(path: &Path, frontier: usize) {
    let mut t = Table::parse_path(path).unwrap();
    assert!(frontier <= t.count(), "{}: frontier past the table", path.display());
    t.records.truncate(frontier);
    t.write_path(path).unwrap();
}

#[test]
fn grouping_edit_assigns_and_clears_through_the_am_slot() {
    let dir = copy_tables(GROUP_CORPUS, &["AM", "MP"]);
    let mp_file = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.file_name().unwrap().to_string_lossy().to_ascii_uppercase().ends_with("MP.DBF"))
        .expect("copied MP");
    truncate_table(&mp_file, GROUP_MP_FRONTIER);
    let eng = Engagement::open(&dir).unwrap();
    let mp = MpTable::from_table(&eng.table("MP").unwrap()).unwrap();
    assert_eq!(
        eng.table("MP").unwrap().count(),
        GROUP_MP_FRONTIER,
        "the readable prefix is the decode domain"
    );
    let scheme = mp
        .digit(GROUP_DIGIT)
        .unwrap_or_else(|| panic!("{GROUP_CORPUS} MP scheme {GROUP_DIGIT}"));
    let target = scheme
        .coded()
        .find(|n: &&MapNode| n.kind.is_assignable())
        .expect("an assignable node")
        .map_no
        .clone();
    // The corpus exercises this path: at least one account is already assigned in
    // the slot, so the write lands on real data rather than an empty column.
    let chart = Chart::from_table(&eng.table("AM").unwrap()).unwrap();
    let slot = cw_app::accounts::group_slot(GROUP_DIGIT).unwrap();
    let assigned = chart
        .posting()
        .filter(|a| !a.groups[slot].trim().is_empty())
        .count();
    assert!(assigned > 0, "{GROUP_CORPUS} AM carries GROUP{GROUP_DIGIT} assignments");
    let acct = chart.posting().next().expect("posting account").clone();
    let ac_no = acct.ac_no.trim().to_string();
    let before = bytes(&eng, "AM");

    // preview
    let preview = edit_grouping(&eng, GROUP_SCHEME, &ac_no, Some(&target), false).unwrap();
    assert!(preview.changed && !preview.written);
    assert_eq!(bytes(&eng, "AM"), before);

    // assign
    let out = edit_grouping(&eng, GROUP_SCHEME, &ac_no, Some(&target), true).unwrap();
    assert!(out.changed && out.written);
    let t = Table::parse(&bytes(&eng, "AM")).unwrap();
    let a = AmAccount::from_table(&t, acct.row).unwrap();
    assert_eq!(a.groups[slot].trim(), target);
    assert_roundtrip(&bytes(&eng, "AM"));

    // clear
    let cleared = edit_grouping(&eng, GROUP_SCHEME, &ac_no, None, true).unwrap();
    assert!(cleared.changed && cleared.written);
    let t = Table::parse(&bytes(&eng, "AM")).unwrap();
    let a = AmAccount::from_table(&t, acct.row).unwrap();
    assert!(a.groups[slot].trim().is_empty(), "clear empties AM.GROUP{GROUP_DIGIT}");

    // unknown node refused, bytes untouched
    let committed = bytes(&eng, "AM");
    assert!(edit_grouping(&eng, GROUP_SCHEME, &ac_no, Some("no-such-node"), true).is_err());
    assert_eq!(bytes(&eng, "AM"), committed);
    // unassignable scheme (mapping has no AM column) refused
    assert!(edit_grouping(&eng, "mapping", &ac_no, Some(&target), true).is_err());
    assert_eq!(bytes(&eng, "AM"), committed);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn entry_edit_creates_replaces_and_refuses_unbalanced() {
    let dir = copy_tables("corp2026", &["AM", "GL"]);
    let eng = Engagement::open(&dir).unwrap();
    let chart = Chart::from_table(&eng.table("AM").unwrap()).unwrap();
    let mut posting = chart.posting();
    let debit = posting.next().expect("debit account").ac_no.trim().to_string();
    let credit = posting.next().expect("credit account").ac_no.trim().to_string();
    let gl_before = bytes(&eng, "GL");
    let am_before = bytes(&eng, "AM");

    let lines = vec![(debit.clone(), 250.00), (credit.clone(), -250.00)];

    // preview writes nothing
    let preview = edit_entry(
        &eng,
        "AJE #9001",
        "I2 shell entry",
        "OCW-I2",
        EntryType::Normal,
        "20251231",
        Some(12),
        &lines,
        false,
    )
    .unwrap();
    assert!(preview.changed && !preview.written);
    assert_eq!(bytes(&eng, "GL"), gl_before);

    // create
    let out = edit_entry(
        &eng,
        "AJE #9001",
        "I2 shell entry",
        "OCW-I2",
        EntryType::Normal,
        "20251231",
        Some(12),
        &lines,
        true,
    )
    .unwrap();
    assert!(out.changed && out.written && out.render("hdr").contains("created"));
    let gl_bytes = bytes(&eng, "GL");
    assert_roundtrip(&gl_bytes);
    assert_eq!(bytes(&eng, "AM"), am_before, "entry edit touches GL only");

    let adj = Adjustments::from_table(&Table::parse(&gl_bytes).unwrap()).unwrap();
    let created = adj.get("AJE #9001").expect("created entry decodes");
    assert_eq!(created.line_count(), 2);
    assert!(created.is_balanced());
    assert_eq!(created.description, "I2 shell entry");

    // replace with the same row shape
    let changed_lines = vec![(debit.clone(), 125.00), (credit.clone(), -125.00)];
    let out = edit_entry(
        &eng,
        "AJE #9001",
        "I2 shell entry v2",
        "OCW-I2",
        EntryType::Reclassifying,
        "20251231",
        Some(12),
        &changed_lines,
        true,
    )
    .unwrap();
    assert!(out.changed && out.written && out.render("hdr").contains("replaced"));
    let gl_bytes = bytes(&eng, "GL");
    assert_roundtrip(&gl_bytes);
    let adj = Adjustments::from_table(&Table::parse(&gl_bytes).unwrap()).unwrap();
    let replaced = adj.get("AJE #9001").expect("replaced entry decodes");
    assert_eq!(replaced.description, "I2 shell entry v2");
    assert_eq!(replaced.entry_type, EntryType::Reclassifying);
    assert_eq!(replaced.line_count(), 2, "same-shape replace keeps row count");
    assert!(replaced.is_balanced());

    // unbalanced replacement is refused before any byte changes
    let committed = bytes(&eng, "GL");
    let bad = vec![(debit.clone(), 10.00), (credit.clone(), -5.00)];
    assert!(edit_entry(
        &eng,
        "AJE #9001",
        "bad",
        "OCW-I2",
        EntryType::Normal,
        "20251231",
        Some(12),
        &bad,
        true
    )
    .is_err());
    assert_eq!(bytes(&eng, "GL"), committed);

    // unknown account is refused too
    assert!(edit_entry(
        &eng,
        "AJE #9002",
        "bad account",
        "OCW-I2",
        EntryType::Normal,
        "20251231",
        Some(12),
        &[("no-such-account".to_string(), 1.0), (credit.clone(), -1.0)],
        true
    )
    .is_err());
    assert_eq!(bytes(&eng, "GL"), committed);

    let _ = fs::remove_dir_all(&dir);
}


// ---------------------------------------------------------------------------
// FEAT.018: the renumber cascade through the shell
// ---------------------------------------------------------------------------

/// The renumber dialog drives the real cascade: preview writes nothing, `--write`
/// commits each changed table atomically, and a re-applied single rename is a
/// true no-op (byte- and mtime-identical).
#[test]
fn renumber_dialog_previews_commits_and_noops() {
    let dir = copy_tables("corp2026", &["AM", "BL", "GL", "MP"]);
    let eng = Engagement::open(&dir).unwrap();
    let am_before = bytes(&eng, "AM");
    let bl_before = bytes(&eng, "BL");
    let gl_before = bytes(&eng, "GL");

    let scope = renumber_scope(&eng).unwrap();
    let plan = RenumberPlan::from_pairs(&[("1001".to_string(), "1099".to_string())]).unwrap();
    assert!(!plan.is_empty());

    // Preview: the cascade really runs (it reports the edit) but nothing lands.
    let preview = renumber_dialog(&eng, &plan, 10, false).unwrap();
    assert!(preview.changed(), "the cascade reports a pending change");
    assert!(!preview.written_any());
    let text = preview.render("hdr", "preview");
    assert!(text.contains("Renumber Accounts"), "{text}");
    assert!(text.contains("1001"), "{text}");
    assert!(text.contains("1099"), "{text}");
    assert!(text.contains("preview only"), "{text}");
    assert!(text.contains("AM:"), "per-table before/after block: {text}");
    assert_eq!(bytes(&eng, "AM"), am_before);
    assert_eq!(bytes(&eng, "BL"), bl_before);
    assert_eq!(bytes(&eng, "GL"), gl_before);

    // Commit: only AM changed (1001 has no live BL/GL reference in corp2026).
    let out = renumber_dialog(&eng, &plan, 10, true).unwrap();
    assert!(out.changed() && out.written_any());
    assert_eq!(out.written, vec!["AM"]);
    assert!(out.render("hdr", "commit").contains("WROTE AM.dbf"));
    let am_after = bytes(&eng, "AM");
    assert_ne!(am_after, am_before);
    assert_roundtrip(&am_after);
    assert_eq!(bytes(&eng, "BL"), bl_before, "BL is not touched");
    assert_eq!(bytes(&eng, "GL"), gl_before, "GL is not touched");
    let chart = Chart::from_table(&Table::parse(&am_after).unwrap()).unwrap();
    let numbers: Vec<String> = chart.posting().map(|a| a.ac_no.trim().to_string()).collect();
    assert!(numbers.contains(&"1099".to_string()));
    assert!(!numbers.contains(&"1001".to_string()));

    // Re-apply: already in effect. No bytes, no mtime, no staging file.
    let mtime = fs::metadata(eng.table_path("AM").unwrap())
        .unwrap()
        .modified()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let noop = renumber_dialog(&eng, &plan, 10, true).unwrap();
    assert!(!noop.changed() && !noop.written_any());
    assert!(noop.render("hdr", "commit").contains("no change"));
    assert_eq!(bytes(&eng, "AM"), am_after);
    let mtime2 = fs::metadata(eng.table_path("AM").unwrap())
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(mtime, mtime2, "a no-op cascade must not touch the file");
    let stage = eng
        .table_path("AM")
        .unwrap()
        .with_extension("dbf.~ocw");
    assert!(!stage.exists(), "no staging litter: {stage:?}");

    let _ = fs::remove_dir_all(&dir);
}

/// A refused cascade (ambiguous map-code source) writes nothing at all.
#[test]
fn renumber_dialog_refusal_writes_nothing() {
    let dir = copy_tables("corp2026", &["AM", "BL", "GL", "MP"]);
    let eng = Engagement::open(&dir).unwrap();
    let am_before = bytes(&eng, "AM");
    let bl_before = bytes(&eng, "BL");

    let scope = renumber_scope(&eng).unwrap();
    // The +100 shift is ambiguous in corp2026 (1500/4000/5000/5500/8000 are both
    // chart accounts and map codes).
    let plan = RenumberPlan::from_delta(&scope, 100).unwrap();
    let out = renumber_dialog(&eng, &plan, 10, true).expect("the dialog reports a refusal");
    assert!(out.blocked(), "the plan is refused");
    assert!(!out.changed() && !out.written_any());
    let text = out.render("hdr", "commit");
    assert!(text.contains("REFUSED"), "{text}");
    assert!(text.contains("also a map code"), "{text}");
    assert_eq!(bytes(&eng, "AM"), am_before);
    assert_eq!(bytes(&eng, "BL"), bl_before);

    let _ = fs::remove_dir_all(&dir);
}

/// The `--delta` preview keeps the shell's long-standing rule: numeric accounts
/// are proposed, non-numeric ids are labelled rather than invented.
#[test]
fn renumber_preview_labels_non_numeric_accounts() {
    let dir = copy_tables("corp2026", &["AM", "BL", "GL", "MP"]);
    let eng = Engagement::open(&dir).unwrap();
    let text = render_renumber(&eng, 100, 40).unwrap();
    assert!(text.contains("Renumber Accounts | delta +100"), "{text}");
    assert!(text.contains("not renumbered (non-numeric)"), "{text}");
    // `RSM - 01` is a real corp2026 id and must be labelled, not shifted.
    assert!(text.contains("RSM - 01") || text.contains("1332-00"), "{text}");
    let _ = fs::remove_dir_all(&dir);
}
