//! P5.G G4 gate: `SH` Document Manager register/tree + `WR` references +
//! `WT` note rows.
//!
//! Golden: `tests/golden/documents/law.json` (built by
//! `examples/freeze_documents.rs`) — per corpus, the `SH` readable frontier and
//! the census of `rows[..frontier]`, plus the `WR`/`WT` sizes and the form /
//! heading title lists.
//!
//! ## Readable-prefix law (F63)
//!
//! `hull2024/SH` declares 664 records over 1,157,370 bytes, but its `.dbf` member
//! is a truncated LZMA stream: the reference decoder returns 1,049,873 bytes, i.e.
//! 603 records, and everything past that is zero-fill synthesis whose row flags are
//! not even `0x20`/`0x2A`. `cw-core/tests/guid_law_gate.rs` pins the same boundary
//! in its `DAMAGED` table. The census below is therefore stated over the readable
//! prefix, which is why `hull2024` reads 103 live / 500 tombstones here rather than
//! the pre-rebuild full-table 118 / 546. `corp2026` and `mwgh2025` are readable end
//! to end and are asserted in full. The frontier is a frozen fixture constant, not
//! a search, so a newly damaged table fails loudly.
//!
//! Everything else the gate locks — schema identity, byte-stable no-op saves, the
//! register order / `LINENO` lattice, the flat-but-parent-capable tree, the
//! delete/tombstone law, and every writer path (add/insert/rename/move/archive/
//! delete/restore, `WR` reference memo, synthetic `WT` note) — is unchanged.

use std::path::{Path, PathBuf};

use cw_app::documents::{
    DocumentBook, DocumentDraft, DocumentKind, NoteBook, NoteDraft, ReferenceBook, ReferenceDraft,
    LINENO_BASE, LINENO_STEP, SH_FIELD_COUNT, SH_RECORD_LEN, WR_FIELD_COUNT, WR_RECORD_LEN,
    WT_FIELD_COUNT, WT_RECORD_LEN,
};
use cw_core::dbf::Table;
use cw_core::guid::{CwGuid, Family, Rng};
use cw_core::json::{self, Json};

const MWGH: &str = "MWGH LLC 9-30-25 REVIEW(1239263)";
const HULL: &str = "2024 Hull Lift Truck Review(1148441)";
const CORP: &str = "USI Compilation Training(1155962)";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn corpus(name: &str) -> PathBuf {
    repo().join("tests/golden/dbf").join(name)
}

fn law() -> Json {
    let p = repo().join("tests/golden/documents/law.json");
    json::parse(&std::fs::read_to_string(&p).unwrap_or_else(|e| {
        panic!("{}: {e} (run `cargo run -p cw-app --example freeze_documents`)", p.display())
    }))
    .unwrap()
}

/// The frozen per-corpus entry.
fn entry(name: &str) -> Json {
    law().get("corpora")
        .and_then(|c| c.get(name))
        .cloned()
        .unwrap_or_else(|| panic!("law.json corpora.{name}"))
}

fn n(j: &Json, key: &str) -> usize {
    j.get(key).and_then(|v| v.as_usize()).unwrap_or(usize::MAX)
}

fn s<'a>(j: &'a Json, key: &str) -> &'a str {
    j.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

fn dbf(dir: &Path, stem: &str, table: &str) -> Vec<u8> {
    std::fs::read(dir.join(format!("{stem}{table}.dbf"))).unwrap()
}

fn fpt(dir: &Path, stem: &str, table: &str) -> Vec<u8> {
    std::fs::read(dir.join(format!("{stem}{table}.fpt")))
        .or_else(|_| std::fs::read(dir.join(format!("{stem}{table}.FPT"))))
        .unwrap_or_default()
}

/// The `SH` bytes the law is stated over: the whole table, or its readable
/// prefix re-encoded with a consistent record count.
fn sh_fixture(dir: &Path, stem: &str, e: &Json) -> Vec<u8> {
    let raw = dbf(dir, stem, "SH");
    match e.get("frontier").and_then(|v| v.as_usize()) {
        None => raw,
        Some(frontier) => {
            let mut t = Table::parse(&raw).unwrap();
            assert_eq!(t.count(), n(e, "declared_records"), "{stem}: declared SH rows");
            assert!(frontier < t.count(), "{stem}: frontier cuts the fabricated tail");
            t.records.truncate(frontier);
            t.encode()
        }
    }
}

#[test]
fn sh_schema_and_noop_save_are_byte_stable() {
    for (name, stem) in [("hull2024", HULL), ("mwgh2025", MWGH), ("corp2026", CORP)] {
        let e = entry(name);
        let dir = corpus(name);
        let sh = e.get("sh").unwrap();
        let raw = sh_fixture(&dir, stem, &e);
        let book = DocumentBook::from_bytes(&raw).unwrap();
        assert_eq!(book.table().fields.len(), SH_FIELD_COUNT, "{name} SH columns");
        assert_eq!(book.table().record_len as usize, SH_RECORD_LEN, "{name} SH record len");
        for (got, want) in book.table().fields.iter().zip(cw_app::documents::SH_COLUMNS.iter()) {
            assert_eq!(&got.name, want, "{name} SH column order");
        }
        assert_eq!(book.count(), n(sh, "live"), "{name} live SH");
        assert_eq!(book.tombstone_count(), n(sh, "deleted"), "{name} SH tombstones");
        assert_eq!(book.encode(), raw, "{name} SH no-op save changed bytes");
        assert_eq!(book.census().cycles, n(sh, "cycles"), "{name} SH parent cycle");
    }
}

#[test]
fn sh_census_tree_and_lineno_laws() {
    let e = entry("corp2026");
    let sh = e.get("sh").unwrap();
    let corp = {
        let dir = corpus("corp2026");
        DocumentBook::from_bytes(&sh_fixture(&dir, CORP, &e)).unwrap()
    };
    let c = corp.census();
    assert_eq!((c.live, c.deleted), (n(sh, "live"), n(sh, "deleted")));
    for letter in ['*', 'H', 'F', 'M', 'A', ' '] {
        let want = sh
            .get("by_type")
            .and_then(|t| t.get(&letter.to_string()))
            .and_then(|v| v.as_usize());
        assert_eq!(c.by_type.get(&letter).copied(), want, "by_type {letter:?}");
    }
    assert_eq!(
        (c.headings, c.forms, c.analyses, c.index_rows),
        (n(sh, "headings"), n(sh, "forms"), n(sh, "analyses"), n(sh, "index_rows"))
    );
    assert_eq!(c.by_prg.get("ac"), sh.get("by_prg").and_then(|p| p.get("ac")).and_then(|v| v.as_usize()).as_ref());
    assert_eq!(c.by_prg.get("IR"), sh.get("by_prg").and_then(|p| p.get("IR")).and_then(|v| v.as_usize()).as_ref());
    assert_eq!(c.by_prg.get("EX"), sh.get("by_prg").and_then(|p| p.get("EX")).and_then(|v| v.as_usize()).as_ref());
    assert_eq!(
        (c.archived, c.rolled, c.locked),
        (n(sh, "archived"), n(sh, "rolled"), n(sh, "locked"))
    );
    assert_eq!(c.blank_lineno, n(sh, "blank_lineno"), "EX rows carry a blank LINENO");
    assert_eq!(c.with_parent, n(sh, "with_parent"), "corpora carry DOCUMENTID empty (F60)");
    assert_eq!(c.roots, n(sh, "roots"));
    assert_eq!(c.cycles, 0);

    // Register order = ascending (LINENO, physical row); index rows share 0.
    let ordered = corp.ordered();
    let mut last = i64::MIN;
    for d in &ordered {
        let v = d.lineno.unwrap_or(i64::MAX);
        assert!(v >= last, "register order regressed at {}", d.cwguid);
        last = v;
    }
    assert_eq!(ordered.len(), n(sh, "live"));
    assert_eq!(ordered[0].lineno, Some(0));

    // Lattice: app rows start at 1,000,000 and step 32,000 until insertions.
    let app: Vec<i64> = ordered
        .iter()
        .filter(|d| d.is_app())
        .filter_map(|d| d.lineno)
        .collect();
    assert_eq!(app.len(), n(sh, "app_rows"));
    assert_eq!(app[0], LINENO_BASE);
    assert_eq!(n(sh, "app_lineno_first") as i64, app[0]);
    assert_eq!(app[1] - app[0], LINENO_STEP);
    assert_eq!(app[2] - app[1], LINENO_STEP);
    assert_eq!(*app.last().unwrap(), n(sh, "app_lineno_last") as i64, "insertion lattice tail");

    // Tree is flat but parent-capable; depth-first projection keeps order.
    let tree = corp.tree();
    assert_eq!(tree.len(), n(sh, "live"));
    assert_eq!(tree.iter().filter(|n| n.depth == 0).count(), n(sh, "roots"));
    let pre = corp.preorder();
    assert_eq!(pre.len(), n(sh, "live"));

    // Forms: CaseView response documents, CVID = right-padded schedule id.
    let forms = corp.forms();
    assert_eq!(forms.len(), n(sh, "forms"));
    assert!(forms.iter().all(|f| !f.cvid.is_empty() && f.kind == DocumentKind::Form));
    let form_detail = e.get("forms_detail").and_then(|v| v.as_arr()).unwrap();
    for (f, want) in forms.iter().zip(form_detail) {
        assert_eq!(f.cvid.trim(), s(want, "cvid"), "form CVID");
        assert_eq!(f.schedule.trim(), s(want, "schedule"), "form SCHEDULE");
        assert_eq!(f.title.trim(), s(want, "title"), "form title");
    }
    let prog = corp.by_schedule("0400 (M1)");
    assert_eq!(prog.len(), 1);
    assert_eq!(prog[0].cvid, "04000RG2");
    assert_eq!(prog[0].kind, DocumentKind::Form);
    assert_eq!(corp.by_schedule("0400 (M1)")[0].title, "Program for Review Engagements");
    assert!(corp.by_schedule("no such schedule").is_empty());

    // Heading nodes are the folder titles; Recycle bin exists as an H node.
    let heading_titles = e.get("heading_titles").and_then(|v| v.as_arr()).unwrap();
    assert_eq!(corp.headings().len(), heading_titles.len());
    for (h, want) in corp.headings().iter().zip(heading_titles) {
        assert_eq!(h.title.trim(), want.as_str().unwrap(), "heading title");
    }
    assert_eq!(
        corp.headings().iter().filter(|h| h.title.trim() == "1000 CASH").count(),
        1,
        "the cash folder"
    );
    assert_eq!(corp.recycle_bin().map(|d| d.title.as_str()), Some("Recycle bin"));

    // Paths and guid resolution.
    let cash = corp.headings().into_iter().find(|h| h.title == "1000 CASH").unwrap();
    assert_eq!(corp.path_of(&cash.cwguid).unwrap(), "1000 CASH");
    assert_eq!(corp.resolve(&cash.cwguid).unwrap().schedule, "");
    assert!(corp.resolve("ZZZZZZZZZZZZZZZZZZZZZZ").is_none());

    // hull / mwgh censuses.
    for (name, stem) in [("hull2024", HULL), ("mwgh2025", MWGH)] {
        let e = entry(name);
        let sh = e.get("sh").unwrap();
        let dir = corpus(name);
        let book = DocumentBook::from_bytes(&sh_fixture(&dir, stem, &e)).unwrap();
        let c = book.census();
        assert_eq!((c.live, c.deleted), (n(sh, "live"), n(sh, "deleted")), "{name} SH census");
        assert_eq!(c.by_type.get(&'*'), sh.get("by_type").and_then(|t| t.get("*")).and_then(|v| v.as_usize()).as_ref(), "{name} stars");
        assert_eq!(c.by_type.get(&'H'), sh.get("by_type").and_then(|t| t.get("H")).and_then(|v| v.as_usize()).as_ref(), "{name} headings");
        assert_eq!((c.forms, c.analyses, c.index_rows), (n(sh, "forms"), n(sh, "analyses"), n(sh, "index_rows")), "{name} doc classes");
        assert_eq!((c.archived, c.rolled, c.locked), (n(sh, "archived"), n(sh, "rolled"), n(sh, "locked")), "{name} flags");
        assert_eq!(c.cycles, 0, "{name} cycles");
    }
}

#[test]
fn sh_writer_adds_moves_and_recycles() {
    let e = entry("corp2026");
    let dir = corpus("corp2026");
    let mut book = DocumentBook::from_bytes(&sh_fixture(&dir, CORP, &e)).unwrap();
    let before_live = book.count();
    let before_phys = book.table().count();
    let mut rng = Rng::new_seeded(0x64);
    let stamp = "20261023190000";
    let tail = book
        .ordered()
        .iter()
        .filter(|d| d.is_app())
        .filter_map(|d| d.lineno)
        .max()
        .expect("app lattice");

    // Append on the 32,000 lattice.
    let h = book
        .add(DocumentDraft::heading("9999 NEW SECTION"), &mut rng, stamp)
        .unwrap();
    assert_eq!(book.count(), before_live + 1);
    let heading = &book.documents()[h];
    assert_eq!(heading.kind, DocumentKind::Heading);
    assert_eq!(heading.title, "9999 NEW SECTION");
    assert_eq!(heading.lineno, Some(tail + LINENO_STEP));
    assert_eq!(heading.prg, "ac");
    assert!(heading.archive && heading.roll && !heading.lockdown);
    assert!(CwGuid::parse(&heading.cwguid).unwrap().matches(Family::B));
    let heading_guid = heading.cwguid.clone();

    // Form requires a CVID; index rows keep PRG=IR and no archive flag.
    let bad = DocumentDraft::form("0123", "Bad form", "");
    assert!(book.add(bad, &mut rng, stamp).is_err());
    let idx = book.add(DocumentDraft::index("wtb"), &mut rng, stamp).unwrap();
    assert_eq!(book.documents()[idx].prg, "IR");
    assert_eq!(book.documents()[idx].lineno, Some(tail + 2 * LINENO_STEP));
    assert!(!book.documents()[idx].archive);

    // Insert directly after the new heading: midpoint gap, still ordered.
    let ins = book
        .insert_after(h, DocumentDraft::form("0123 (M1)", "Inserted Form", "01230RG2"), &mut rng, stamp)
        .unwrap();
    let inserted = &book.documents()[ins];
    assert_eq!(inserted.kind, DocumentKind::Form);
    assert_eq!(inserted.cvid, "01230RG2");
    let lo = book.documents()[h].lineno.unwrap();
    let mid = inserted.lineno.unwrap();
    assert!(mid > lo && mid < lo + LINENO_STEP, "midpoint insert {mid}");

    // Rename + re-schedule + archive flag.
    book.rename(ins, "Inserted Form (renamed)", stamp).unwrap();
    book.set_schedule(ins, "0124 (M1)").unwrap();
    book.set_archived(ins, false).unwrap();
    assert_eq!(book.documents()[ins].title, "Inserted Form (renamed)");
    assert_eq!(book.documents()[ins].schedule, "0124 (M1)");
    assert!(!book.documents()[ins].archive && !book.documents()[ins].roll);

    // Parent/child tree: move the form under the heading, then read it back.
    let parent = book.resolve(&heading_guid).unwrap().cwguid.clone();
    book.move_to(ins, Some(&parent), stamp).unwrap();
    assert_eq!(book.documents()[ins].document_id, parent);
    let tree = book.tree();
    assert_eq!(tree.len(), before_live + 2, "child no longer a root");
    let node = tree.iter().find(|n| n.guid == parent).expect("parent node");
    assert_eq!(node.children.len(), 1);
    assert_eq!(node.children[0].depth, 1);
    assert_eq!(node.children[0].guid, book.documents()[ins].cwguid);
    let child_guid = book.documents()[ins].cwguid.clone();
    assert_eq!(book.path_of(&child_guid).unwrap(), "9999 NEW SECTION/Inserted Form (renamed)");
    assert_eq!(book.census().with_parent, n(e.get("sh").unwrap(), "with_parent") + 1);

    // Cycle rejection: parent -> child is refused, self-parent refused.
    let child = child_guid.clone();
    assert!(book.move_to(h, Some(&child), stamp).is_err());
    let self_guid = book.documents()[h].cwguid.clone();
    assert!(book.move_to(h, Some(&self_guid), stamp).is_err());
    assert!(!book.would_cycle(&child, ""));

    // Reload through the encoded image.
    let image = book.encode();
    let round = DocumentBook::from_bytes(&image).unwrap();
    assert_eq!(round.count(), before_live + 3);
    assert_eq!(round.resolve(&heading_guid).unwrap().title, "9999 NEW SECTION");
    let child_back = round.resolve(&child_guid).unwrap();
    assert_eq!(child_back.document_id, parent);
    assert_eq!(round.path_of(&child_guid).unwrap(), "9999 NEW SECTION/Inserted Form (renamed)");
    assert_eq!(round.census().cycles, 0);

    // Delete to the Recycle Bin = physical tombstone; live shrinks, physical
    // rows stay, and the tombstone is restorable.
    let victim_row = round.resolve(&child_guid).unwrap().row;
    let victim_index = round.index_of(&child_guid).unwrap();
    let mut work = round;
    work.delete_at(victim_index).unwrap();
    assert_eq!(work.count(), before_live + 2);
    assert_eq!(work.table().count(), before_phys + 3, "records are never dropped");
    assert!(work.table().is_deleted(victim_row));
    assert!(work.resolve(&child_guid).is_none());
    let restored = work.restore_row(victim_row).unwrap();
    assert_eq!(work.count(), before_live + 3);
    assert_eq!(&work.documents()[restored].cwguid, &child_guid);
    assert_eq!(work.resolve(&child_guid).unwrap().document_id, parent);

    // Renumber re-spaces app rows on the lattice and preserves order.
    work.renumber().unwrap();
    let app: Vec<i64> = work
        .ordered()
        .iter()
        .filter(|d| d.is_app())
        .filter_map(|d| d.lineno)
        .collect();
    assert_eq!(app[0], LINENO_BASE);
    assert!(app.windows(2).all(|w| w[1] - w[0] == LINENO_STEP));
    assert_eq!(
        work.census().with_parent,
        n(e.get("sh").unwrap(), "with_parent") + 1,
        "renumber keeps the parent link"
    );
}

#[test]
fn wr_references_schema_memos_and_dangling() {
    for (name, stem) in [("hull2024", HULL), ("mwgh2025", MWGH), ("corp2026", CORP)] {
        let e = entry(name);
        let wr_law = e.get("wr").unwrap();
        let dir = corpus(name);
        let raw = dbf(&dir, stem, "WR");
        let raw_fpt = fpt(&dir, stem, "WR");
        let book = ReferenceBook::from_bytes(&raw, &raw_fpt).unwrap();
        assert_eq!(book.table().fields.len(), WR_FIELD_COUNT, "{name} WR columns");
        assert_eq!(book.table().record_len as usize, WR_RECORD_LEN, "{name} WR record len");
        for (got, want) in book.table().fields.iter().zip(cw_app::documents::WR_COLUMNS.iter()) {
            assert_eq!(&got.name, want, "{name} WR column order");
        }
        assert_eq!(book.count(), n(wr_law, "live"), "{name} live WR");
        assert_eq!(book.table().count(), n(wr_law, "physical"), "{name} physical WR");
        assert_eq!(book.encode(), raw, "{name} WR no-op save changed bytes");
        assert_eq!(book.encode_fpt(), raw_fpt, "{name} WR.fpt no-op save changed bytes");
        for r in book.references() {
            assert_eq!(r.lineno, "111111", "{name} WR LINENO sentinel");
            assert_eq!(r.cwguid.len(), 22);
            assert!(r.from_ref.is_empty() && r.memo_no.is_empty() && r.rev_code.is_empty());
            assert!(CwGuid::parse(&r.cwguid).unwrap().matches(Family::C));
        }
        // Live WR rows resolve into live SH (F60); dangling is empty.
        let sh = DocumentBook::from_bytes(&sh_fixture(&dir, stem, &e)).unwrap();
        assert!(book.dangling(&sh).is_empty(), "{name} live WR danglers");
    }

    // corp2026 payload: the SGA expectation/conclusion memo.
    let e = entry("corp2026");
    let dir = corpus("corp2026");
    let mut book = ReferenceBook::from_bytes(&dbf(&dir, CORP, "WR"), &fpt(&dir, CORP, "WR")).unwrap();
    let doc = DocumentBook::from_bytes(&sh_fixture(&dir, CORP, &e)).unwrap();
    let memo = &book.references()[0];
    assert!(memo.memo.starts_with("Expectation: RSM notes that SG&A expenses"));
    assert!(memo.memo.contains("Conclusion: Changes in the account balances"));
    assert!(doc.resolve(&memo.doc_guid).is_some());

    // Writer: reference memo on a live document, reloaded from the image.
    let target = doc.headings()[0].cwguid.clone();
    let mut rng = Rng::new_seeded(0x5757);
    let stamp = "20261023190000";
    let idx = book
        .add(
            ReferenceDraft::new(&target, "Cross-reference: see lead sheet 1200.")
                .from_ref("1200")
                .revision("A"),
            &mut rng,
            stamp,
        )
        .unwrap();
    let added = &book.references()[idx];
    assert_eq!(added.doc_guid, target);
    assert_eq!(added.from_ref, "1200");
    assert_eq!(added.rev_code, "A");
    assert_eq!(added.lineno, "111111");
    assert_eq!(added.cw_date, stamp);
    assert!(CwGuid::parse(&added.cwguid).unwrap().matches(Family::C));

    let base_live = n(e.get("wr").unwrap(), "live");
    let round = ReferenceBook::from_bytes(&book.encode(), &book.encode_fpt()).unwrap();
    assert_eq!(round.count(), base_live + 1);
    let back = round.by_document(&target);
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].memo, "Cross-reference: see lead sheet 1200.");
    assert!(round.dangling(&doc).is_empty());

    // Validation: DOCGUID mandatory + 22 chars.
    assert!(book.add(ReferenceDraft::new("", "x"), &mut rng, stamp).is_err());
    assert!(book.add(ReferenceDraft::new("short", "x"), &mut rng, stamp).is_err());

    // Removing a document leaves its reference dangling.
    let mut doc2 = doc;
    let idx2 = doc2.index_of(&target).unwrap();
    doc2.delete_at(idx2).unwrap();
    assert_eq!(round.dangling(&doc2), vec![target.clone()]);
}

#[test]
fn wt_schema_is_empty_and_notes_round_trip() {
    let mut empty_fpt_seen = 0;
    for (name, stem) in [("hull2024", HULL), ("mwgh2025", MWGH), ("corp2026", CORP)] {
        let e = entry(name);
        let wt_law = e.get("wt").unwrap();
        let dir = corpus(name);
        let raw = dbf(&dir, stem, "WT");
        let book = NoteBook::from_bytes(&raw).unwrap();
        assert_eq!(book.table().fields.len(), WT_FIELD_COUNT, "{name} WT columns");
        assert_eq!(book.table().record_len as usize, WT_RECORD_LEN, "{name} WT record len");
        for (got, want) in book.table().fields.iter().zip(cw_app::documents::WT_COLUMNS.iter()) {
            assert_eq!(&got.name, want, "{name} WT column order");
        }
        assert_eq!(book.count(), n(wt_law, "live"), "{name} WT live rows");
        assert_eq!(book.table().count(), n(wt_law, "physical"), "{name} WT physical rows");
        assert_eq!(book.count(), 0, "{name} WT has no live rows");
        assert_eq!(book.table().count(), 0, "{name} WT has no physical rows");
        assert_eq!(book.encode(), raw, "{name} WT no-op save changed bytes");
        empty_fpt_seen += 1;
    }
    assert_eq!(empty_fpt_seen, 3);

    // Synthetic note: values in A/B, format, per-cent, reload.
    let e = entry("corp2026");
    let dir = corpus("corp2026");
    let raw = dbf(&dir, CORP, "WT");
    let mut book = NoteBook::from_bytes(&raw).unwrap();
    let doc = DocumentBook::from_bytes(&sh_fixture(&dir, CORP, &e)).unwrap();
    let target = doc.forms()[0].cwguid.clone();
    let mut rng = Rng::new_seeded(0x57);
    let stamp = "20261023190000";
    let idx = book
        .add(
            NoteDraft::new(&target, "Variance note")
                .number('A', 1234.5)
                .number('B', -0.25)
                .format('A', "#,##0.00")
                .percent(12.5),
            &mut rng,
            stamp,
        )
        .unwrap();
    let note = &book.notes()[idx];
    assert_eq!(note.lineno, Some(1));
    assert_eq!(note.doc_guid, target);
    assert_eq!(note.text, "Variance note");
    assert_eq!(note.kind, "T");
    assert_eq!(note.numbers.get(&'A'), Some(&1234.5));
    assert_eq!(note.numbers.get(&'B'), Some(&-0.25));
    assert_eq!(note.formats.get(&'A').map(String::as_str), Some("#,##0.00"));
    assert_eq!(note.percent, Some(12.5));
    assert_eq!(note.cw_date, stamp);

    let round = NoteBook::from_bytes(&book.encode()).unwrap();
    assert_eq!(round.count(), 1);
    assert_eq!(round.by_document(&target).len(), 1);
    assert_eq!(round.notes()[0].numbers.get(&'A'), Some(&1234.5));

    // Slot validation + tombstone.
    assert!(book.add(NoteDraft::new(&target, "x").number('Z', 1.0), &mut rng, stamp).is_err());
    book.remove_at(0).unwrap();
    assert_eq!(book.count(), 0);
    assert!(book.table().is_deleted(0));
}
