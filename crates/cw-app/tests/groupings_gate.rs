//! P5.E2 gate: the `MP` grouping/mapping model + `AM` leadsheet assignment.
//!
//! Fixture: `tests/golden/area/domain.json` (`examples/freeze_area.rs`), a
//! kernel-derived regression pin, plus the frozen dbf corpora themselves. The
//! readable frontiers are the reference-decoder evidence in
//! `cw_core/tests/guid_law_gate.rs` (`DAMAGED`); `tests/support/mod.rs` states the
//! F63 rationale. `MP` is the table where the two frontier definitions disagree
//! (a row can carry a valid guid while its numeric cells are half-shifted), so
//! the pinned value is `min(readable, decodable)` and the decodable term is
//! re-measured here — a moved boundary fails.
//!
//! Locks:
//!
//! - the physical 67-column / 1332-byte schema on all three corpora (format
//!   constant from the shipped type-library header, asserted inline);
//! - scheme decode (`MP.ID` = scheme selector, not a parent link);
//! - inferred hierarchy (dotted nesting + 100/110/111 numbering convention);
//! - F37 assignment law over the authentic prefixes;
//! - atomic assignment writes and refusals;
//! - lead-schedule linkage (`MP.GROUP1` <-> `AM.SCHEDULE`);
//! - byte-identical node/property round-trip.
//!
//! # What the authentic prefixes actually contain (F63 consequences)
//!
//! These are the facts the original gate got from the fabricated tails; each is
//! now stated on the domain where it is true, with the tail fact recorded here so
//! nothing is lost:
//!
//! | corpus | MP frontier | schemes in prefix | schemes tail-only |
//! |---|---|---|---|
//! | corp2026 | 786 / 1676 | 1, 7, 8 | 0, 2, 3, 5, 6, 9, M, … |
//! | hull2024 | 785 / 2837 | 1, 8 | 0, 3, 9, M |
//! | mwgh2025 | 785 / 2342 | 1, 8 | 0, 7, 9, M |
//!
//! * `MP.ID='M'` (the separate statement-mapping scheme) and `MP.ID='2'`/`'9'`
//!   never appear in an authentic prefix, so `TITLE='Y'` headings (19),
//!   `TITLE='F'` filtered nodes (2) and `MAPTYPE='C'` calculated nodes (22) —
//!   all hull numbers the original gate read — are fabricated-tail rows.
//! * `MP.GROUP1` (the `AM.SCHEDULE` link) is populated on **zero** authentic
//!   rows in all three corpora; `lead_schedule_codes` is therefore empty and
//!   every `AM.SCHEDULE` cell is a dangling link. The original gate's
//!   "hull has no dangling schedules" was a tail artifact.
//! * `hull2024`'s 553 `AM.GROUPn` values all point at tail-only schemes.
//!   `mwgh2025` slot 8 (279 values) is the only authentic, resolving assignment
//!   set in the freeze.

mod support;

use cw_app::accounts::{write_cell, Chart};
use cw_app::groups::{
    account_assignment, accounts_at, assign_account, assignments, clear_assignment,
    dangling_lead_schedules, inferred_parent, lead_schedule, lead_schedule_codes,
    mp_schema_drift, mp_schema_matches, rewrite_nodes, set_lead_schedule, unassigned_rows,
    MapNode, MpIssueKind, MpTable, NodeKind, Scheme, ASSIGNABLE_DIGITS, MAP_PROPERTY_COLUMNS,
    MP_SCHEMA,
};
use cw_core::dbf::Table;
use support::{frontier, map_at, open, prefix, table_fixture, triples_at, usize_at, CORPORA};

fn mp_of(tag: &str) -> MpTable {
    let raw = open(tag, "MP.dbf");
    let n = frontier(tag, "MP", &raw);
    MpTable::from_table(&prefix(&raw, n)).unwrap_or_else(|e| panic!("{tag} MP prefix {n}: {e}"))
}

#[test]
fn physical_schema_matches_inventory_on_all_corpora() {
    // Format constant: 67 descriptors / 1332-byte records (spec `mapping.md`).
    assert_eq!(MP_SCHEMA.len(), 67);
    for (tag, _) in CORPORA {
        let t = open(tag, "MP.dbf");
        assert_eq!(t.fields.len(), 67, "{tag} field count");
        assert_eq!(t.record_len, 1332, "{tag} record length");
        assert_eq!(
            t.fields.iter().map(|f| f.len as usize).sum::<usize>() + 1,
            t.record_len as usize,
            "{tag}: declared widths must sum to the record length"
        );
        assert!(
            mp_schema_matches(&t),
            "{tag} drift: {:?}",
            mp_schema_drift(&t)
        );

        // The area census agrees on the physical shape and the frontier.
        let f = table_fixture(tag, "mp");
        assert_eq!(usize_at(&f, "records"), t.count(), "{tag} census records");
        assert_eq!(
            usize_at(&f, "frontier"),
            frontier(tag, "MP", &t),
            "{tag} pinned frontier"
        );
    }
    // the MP property subset is exactly the ten physical dialog columns
    assert_eq!(MAP_PROPERTY_COLUMNS.len(), 10);
    for col in MAP_PROPERTY_COLUMNS {
        assert!(
            MP_SCHEMA.iter().any(|f| f.name == col),
            "missing MP column {col}"
        );
    }
}

#[test]
fn schemes_decode_as_observed() {
    // The three authentic scheme sets. `MP.ID` is a selector, not a parent link.
    let expected: [(&str, Vec<Scheme>); 3] = [
        (
            "corp2026",
            vec![Scheme::Digit(1), Scheme::Digit(7), Scheme::Digit(8)],
        ),
        ("hull2024", vec![Scheme::Digit(1), Scheme::Digit(8)]),
        ("mwgh2025", vec![Scheme::Digit(1), Scheme::Digit(8)]),
    ];
    for (tag, want) in expected {
        let mp = mp_of(tag);
        assert_eq!(mp.scheme_keys(), want, "{tag} authentic scheme set");

        // The fixture pins the same set and its per-scheme node censuses.
        let f = table_fixture(tag, "mp");
        let schemes = f.get("schemes").and_then(|x| x.as_obj()).expect("schemes");
        let got: Vec<String> = schemes.iter().map(|(k, _)| k.clone()).collect();
        let want_s: Vec<String> = want.iter().map(|s| s.as_str()).collect();
        assert_eq!(got, want_s, "{tag} fixture scheme set");

        // Every scheme's node / coded / walk census round-trips.
        for (key, val) in schemes {
            let s = mp.scheme(match key.as_str() {
                "1" => Scheme::Digit(1),
                "7" => Scheme::Digit(7),
                "8" => Scheme::Digit(8),
                "9" => Scheme::Digit(9),
                other => panic!("unexpected scheme {other}"),
            })
            .unwrap();
            assert_eq!(usize_at(val, "nodes"), s.len(), "{tag}/{key} nodes");
            assert_eq!(
                usize_at(val, "coded"),
                s.coded().count(),
                "{tag}/{key} coded"
            );
            assert_eq!(usize_at(val, "walk"), s.walk().len(), "{tag}/{key} walk");
            assert_eq!(
                usize_at(val, "roots"),
                s.roots().len(),
                "{tag}/{key} roots"
            );
            // Pre-order covers every coded node exactly once.
            assert_eq!(
                s.walk().len(),
                s.coded().count(),
                "{tag}/{key}: pre-order must cover every coded node"
            );
        }
    }

    // Corp: the 100/110/111 template over scheme 8, and the `8000/8200` block in
    // scheme 1 (the lead-schedule-shaped numbering that survives without GROUP1).
    let corp = mp_of("corp2026");
    let d8 = corp.digit(8).expect("corp scheme 8");
    assert_eq!(d8.coded().count(), 726);
    assert_eq!(d8.get("111.100").unwrap().kind, NodeKind::Assignable);
    // corp2026 carries the lead-schedule-shaped `8000/…` numbering in scheme 7
    // (hull and mwgh carry it in scheme 1); both are authentic.
    assert_eq!(corp.digit(1).unwrap().coded().count(), 28);
    assert_eq!(corp.digit(7).unwrap().coded().count(), 32);
    assert!(corp.digit(7).unwrap().get("8200").is_some());
    assert!(mp_of("hull2024").digit(1).unwrap().get("8200").is_some());
    assert!(mp_of("mwgh2025").digit(1).unwrap().get("8200").is_some());

    // Hull / mwgh: scheme 8 holds the statements tree with one blank sentinel.
    for tag in ["hull2024", "mwgh2025"] {
        let mp = mp_of(tag);
        let d8 = mp.digit(8).expect("scheme 8");
        assert_eq!(d8.len(), 757, "{tag} scheme-8 nodes");
        assert_eq!(d8.coded().count(), 756, "{tag} coded");
        assert_eq!(
            d8.nodes().iter().filter(|n| !n.has_code()).count(),
            1,
            "{tag}: exactly one blank-code sentinel"
        );
    }

    // The schemes the original gate read are fabricated-tail-only: assert their
    // absence so a future de-truncation is a loud fixture change, not a silent
    // widening of the asserted domain.
    for tag in ["corp2026", "hull2024", "mwgh2025"] {
        let mp = mp_of(tag);
        assert!(mp.digit(2).is_none(), "{tag}: scheme 2 is tail-only");
        assert!(mp.mapping().is_none(), "{tag}: scheme M is tail-only");
    }
    let hull = mp_of("hull2024");
    assert!(hull.digit(9).is_none(), "hull scheme 9 is tail-only");
    let mwgh = mp_of("mwgh2025");
    assert!(mwgh.digit(9).is_none(), "mwgh scheme 9 is tail-only");
    assert_eq!(hull.digit(8).unwrap().get("810").unwrap().row, 784);
    assert!(hull.digit(8).unwrap().get("810.100").is_none(), "row 785 is past hull's frontier");
}

#[test]
fn hierarchy_is_inferred_from_codes() {
    // pure helper: dotted nesting first, then the 100/110/111 numbering law
    let codes = ["100", "110", "111", "130", "170", "173", "111.100"];
    let lookup = |c: &str| codes.iter().position(|x| *x == c);
    assert_eq!(inferred_parent("111", lookup), Some(1));
    assert_eq!(inferred_parent("130", lookup), Some(0));
    assert_eq!(inferred_parent("173", lookup), Some(4));
    assert_eq!(inferred_parent("111.100", lookup), Some(2));
    assert_eq!(inferred_parent("100", lookup), None);

    let mp = mp_of("hull2024");
    let m = mp.digit(8).expect("hull scheme 8");
    assert_eq!(
        m.path("111")
            .iter()
            .map(|n| n.map_no.as_str())
            .collect::<Vec<_>>(),
        vec!["100", "110", "111"]
    );
    assert_eq!(
        m.path("130")
            .iter()
            .map(|n| n.map_no.as_str())
            .collect::<Vec<_>>(),
        vec!["100", "130"]
    );
    assert_eq!(
        m.path("111.100")
            .iter()
            .map(|n| n.map_no.as_str())
            .collect::<Vec<_>>(),
        vec!["100", "110", "111", "111.100"]
    );
    assert_eq!(m.depth("111.100"), 3);
    assert_eq!(m.descendants("111").len(), 4, "111.100/.200/.300/.400");
    assert!(m.children_of("110").iter().any(|n| n.map_no == "111"));
    assert_eq!(m.path("100").len(), 1);
    assert_eq!(m.descendants("170").len(), 16, "170.* + 171 + 173.*");

    // every coded node is reachable exactly once in pre-order
    let walk = m.walk();
    assert_eq!(
        walk.len(),
        m.coded().count(),
        "pre-order covers every coded node"
    );
    assert_eq!(walk.iter().filter(|n| n.map_no == "111.100").count(), 1);

    // The tree is the same template in every authentic prefix.
    for tag in ["corp2026", "mwgh2025"] {
        let mp = mp_of(tag);
        let m = mp.digit(8).expect("scheme 8");
        assert_eq!(m.depth("111.100"), 3, "{tag}");
        assert_eq!(m.descendants("111").len(), 4, "{tag}");
    }
}

#[test]
fn assignment_law_holds_on_all_corpora() {
    for (tag, _) in CORPORA {
        let mp = mp_of(tag);
        let am = open(tag, "AM.dbf");

        // The chart sees the same rows (AM decode still works alongside MP).
        let chart = Chart::from_table(&am).expect("chart still decodes");
        assert!(!chart.is_empty(), "{tag}: empty chart");

        // Slot columns really exist only for 0 and 2..=9.
        assert_eq!(
            unassigned_rows(&am, 1).len(),
            0,
            "{tag}: slot 1 is not addressable"
        );
        assert!(account_assignment(&am, 0, 1).is_err());
    }

    // corp2026: no GROUPn assignment at all (leadsheet links are SCHEDULE-only).
    let corp_am = open("corp2026", "AM.dbf");
    for d in ASSIGNABLE_DIGITS {
        assert!(
            assignments(&corp_am, d).is_empty(),
            "corp2026 slot {d} must be empty"
        );
    }
    let corp_f = table_fixture("corp2026", "mp");
    assert_eq!(map_at(&corp_f, "assignment").len(), 0);
    assert_eq!(
        usize_at(&corp_f, "schedule_cells"),
        315,
        "corp leadsheet links live in AM.SCHEDULE"
    );

    // mwgh2025: slot 8, 279 assignments, all resolvable, all assignable nodes.
    let mwgh_mp = mp_of("mwgh2025");
    let mwgh_am = open("mwgh2025", "AM.dbf");
    assert_eq!(assignments(&mwgh_am, 8).len(), 279);
    assert_eq!(
        unassigned_rows(&mwgh_am, 8).len(),
        349 - 279,
        "mwgh slot-8 vacancy"
    );
    assert!(
        mwgh_mp.assignment_issues(&mwgh_am).is_empty(),
        "every mwgh slot-8 value must be a verbatim assignable MAP_NO"
    );
    let at = accounts_at(&mwgh_am, &mwgh_mp, Scheme::Digit(8), "211");
    assert!(!at.is_empty(), "mwgh 211 is assigned");
    assert!(at.iter().all(|(_, ac)| !ac.is_empty()));

    // hull2024: slots 2 (213) and 9 (340) are populated but every value points
    // at a tail-only scheme, so the audit must report all 553 as unknown.
    let hull_mp = mp_of("hull2024");
    let hull_am = open("hull2024", "AM.dbf");
    assert_eq!(assignments(&hull_am, 2).len(), 213);
    assert_eq!(assignments(&hull_am, 9).len(), 340);
    assert_eq!(unassigned_rows(&hull_am, 2).len(), 402 - 213);
    let issues = hull_mp.assignment_issues(&hull_am);
    assert_eq!(issues.len(), 553, "hull residual assignments");
    let pinned = triples_at(&table_fixture("hull2024", "mp"), "assignment_issues");
    assert_eq!(
        pinned
            .get(&("2".to_string(), "assignment target missing from MP".to_string()))
            .copied(),
        Some(213),
        "fixture pins hull slot-2 residuals"
    );
    assert_eq!(
        pinned
            .get(&("9".to_string(), "assignment target missing from MP".to_string()))
            .copied(),
        Some(340),
        "fixture pins hull slot-9 residuals"
    );
    assert!(issues
        .iter()
        .all(|i| i.code.trim() == i.code || i.digit == 9));
    assert!(
        accounts_at(&hull_am, &hull_mp, Scheme::Digit(2), "10-7").is_empty(),
        "10-7 cannot resolve: scheme 2 is tail-only"
    );
}

#[test]
fn assignment_writes_are_atomic_and_verbatim() {
    // The write law is exercised on mwgh2025, the one corpus whose grouping data
    // is authentic on both sides.
    let mp_raw = open("mwgh2025", "MP.dbf");
    let mp = MpTable::from_table(&prefix(&mp_raw, frontier("mwgh2025", "MP", &mp_raw))).unwrap();
    let mut am = open("mwgh2025", "AM.dbf");

    let (row, current) = assignments(&am, 8)[0].clone();
    assert_eq!(account_assignment(&am, row, 8).unwrap(), current);
    let before = am.encode();

    // unknown target leaves the table untouched
    assert!(assign_account(&mut am, row, &mp, 8, "no-such-node").is_err());
    // slot 1 has no AM column
    assert!(assign_account(&mut am, row, &mp, 1, "111").is_err());
    // over-width code is refused by write_cell
    assert!(assign_account(&mut am, row, &mp, 8, &"9".repeat(41)).is_err());
    assert_eq!(am.encode(), before, "failed writes must not touch bytes");

    // a blank-code sentinel can never receive an account
    let blank = mp
        .digit(8)
        .unwrap()
        .nodes()
        .iter()
        .find(|n| !n.has_code())
        .expect("scheme-8 blank sentinel");
    let sentinel_code = blank.map_no.clone();
    assert!(assign_account(&mut am, row, &mp, 8, &sentinel_code).is_err());

    // legal re-assignment round-trips
    assign_account(&mut am, row, &mp, 8, "111").unwrap();
    assert_eq!(account_assignment(&am, row, 8).unwrap(), "111");
    clear_assignment(&mut am, row, 8).unwrap();
    assert!(account_assignment(&am, row, 8).unwrap().is_empty());
    assign_account(&mut am, row, &mp, 8, &current).unwrap();
    assert_eq!(am.encode(), before, "clear+reassign restores the original row");

    // The F37 refusal message names the reason (COM-matching shape).
    let err = assign_account(&mut am, row, &mp, 8, "no-such-node").unwrap_err();
    assert!(
        err.to_string().contains("no MP node"),
        "refusal message: {err}"
    );
}

#[test]
fn lead_schedule_links_mp_group1_to_am_schedule() {
    // `AM.SCHEDULE` is the lead-schedule code; the MP side is `MP.GROUP1`. The
    // link is only meaningful where GROUP1 is populated, and it is populated on
    // zero authentic rows in all three corpora (it lives in the fabricated
    // tails), so the law asserted here is the *shape* of the link, not a match
    // rate: codes are empty, so every schedule cell is dangling, and the writer
    // refuses any code the tree does not reference.
    for (tag, _) in CORPORA {
        let mp = mp_of(tag);
        assert!(
            lead_schedule_codes(&mp).is_empty(),
            "{tag}: no authentic MP.GROUP1 values"
        );
        let f = table_fixture(tag, "mp");
        assert_eq!(usize_at(&f, "lead_codes"), 0, "{tag} fixture lead codes");
        assert_eq!(usize_at(&f, "group1_nodes"), 0, "{tag} fixture GROUP1 nodes");

        let am = open(tag, "AM.dbf");
        let sched = (0..am.count())
            .filter(|&r| {
                !am.is_deleted(r)
                    && !am
                        .cell_str(r, "SCHEDULE")
                        .unwrap_or_default()
                        .trim()
                        .is_empty()
            })
            .count();
        assert_eq!(usize_at(&f, "schedule_cells"), sched, "{tag} schedule cells");
        assert_eq!(
            dangling_lead_schedules(&am, &mp).len(),
            sched,
            "{tag}: with GROUP1 empty, every populated SCHEDULE cell dangles"
        );
        assert_eq!(usize_at(&f, "dangling"), sched, "{tag} fixture dangling");
    }

    // The writer validates against the tree and is atomic either way.
    let mp = mp_of("hull2024");
    let mut am = open("hull2024", "AM.dbf");
    let before = am.encode();
    assert!(set_lead_schedule(&mut am, 0, &mp, "8200").is_err(), "8200 is not a GROUP1 value");
    assert!(set_lead_schedule(&mut am, 0, &mp, "not-a-schedule").is_err());
    assert_eq!(am.encode(), before, "refused writes must not touch bytes");

    // Clearing is always legal (blank needs no resolution) and round-trips.
    let original = lead_schedule(&am, 0).unwrap();
    assert!(!original.is_empty(), "hull row 0 carries a schedule code");
    set_lead_schedule(&mut am, 0, &mp, "").unwrap();
    assert!(lead_schedule(&am, 0).unwrap().is_empty());
    write_cell(&mut am, 0, "SCHEDULE", &original).unwrap();
    assert_eq!(am.encode(), before);
}

#[test]
fn node_and_property_roundtrip_is_byte_identical() {
    for (tag, _) in CORPORA {
        let original = prefix(&open(tag, "MP.dbf"), frontier(tag, "MP", &open(tag, "MP.dbf")));
        let mut t = original.clone();
        rewrite_nodes(&mut t).unwrap();
        assert_eq!(
            t.encode(),
            original.encode(),
            "{tag}: decoded MP nodes must re-encode byte-identically"
        );

        // Full decode of every row (memo pointers stay raw). CWGUID is 22 chars
        // on every authentic row; the corp2026 prefix carries one blank guid.
        let f = table_fixture(tag, "mp");
        let widths = map_at(&f, "guid_widths");
        for r in 0..t.count() {
            if t.is_deleted(r) {
                continue;
            }
            let n = MapNode::from_table(&t, r).unwrap();
            assert_eq!(n.row, r);
            assert!(
                n.cwguid.is_empty() || n.cwguid.chars().count() == 22,
                "{tag} row {r}: CWGUID width {}",
                n.cwguid.chars().count()
            );
        }
        assert_eq!(widths.get("22").copied().unwrap_or(0) as usize,
            (0..t.count()).filter(|&r| !t.is_deleted(r) && MapNode::from_table(&t, r).unwrap().cwguid.chars().count() == 22).count(),
            "{tag} fixture guid-width census");
    }
}

#[test]
fn issues_flag_duplicates_and_blank_codes() {
    // Hull's scheme 8 legitimately holds one blank-code sentinel; it is reported
    // as `MissingCode`, not as a duplicate or a fatal error.
    let raw = open("hull2024", "MP.dbf");
    let n = frontier("hull2024", "MP", &raw);
    let mp = MpTable::from_table(&prefix(&raw, n)).unwrap();
    let hull_issues = mp.issues();
    assert_eq!(hull_issues.len(), 1, "hull prefix issues: {hull_issues:?}");
    assert_eq!(hull_issues[0].kind, MpIssueKind::MissingCode);
    assert_eq!(hull_issues[0].scheme, Scheme::Digit(8));
    assert_eq!(
        triples_at(&table_fixture("hull2024", "mp"), "issues")
            .get(&("digit 8".to_string(), "blank MAP_NO".to_string()))
            .copied(),
        Some(1)
    );

    // Duplicate a MAP_NO inside the statements tree.
    let mut raw = prefix(&raw, n);
    let dup_row = mp.digit(8).unwrap().get("111.200").unwrap().row;
    write_cell(&mut raw, dup_row, "MAP_NO", "111.100").unwrap();
    let broken = MpTable::from_table(&raw).unwrap();
    assert!(
        broken
            .issues()
            .iter()
            .any(|i| i.kind == MpIssueKind::DuplicateCode
                && i.scheme == Scheme::Digit(8)
                && i.map_no == "111.100"),
        "duplicate MAP_NO must be reported"
    );
    // the first row still wins the index
    assert_eq!(
        broken.digit(8).unwrap().get("111.100").unwrap().row,
        mp.digit(8).unwrap().get("111.100").unwrap().row
    );

    // corp2026 and mwgh2025 prefixes are issue-clean apart from mwgh's sentinel.
    assert!(mp_of("corp2026").issues().is_empty(), "corp issues");
    let mwgh = mp_of("mwgh2025").issues();
    assert_eq!(mwgh.len(), 1, "mwgh prefix issues: {mwgh:?}");
    assert_eq!(mwgh[0].kind, MpIssueKind::MissingCode);
}

#[test]
fn schema_drift_is_refused() {
    let mut t = open("hull2024", "MP.dbf");
    let idx = t.field_index("AC_DESC").unwrap();
    t.fields[idx].len = 49;
    assert!(!mp_schema_matches(&t));
    assert!(mp_schema_drift(&t)
        .iter()
        .any(|d| d.column == "AC_DESC" && d.expected == "50" && d.actual == "49"));
    assert!(MpTable::from_table(&t).is_err());
}
