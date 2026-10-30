//! P5.E2b gate — `AM.GROUPn <- MAP_NO` assignment (thin; full law in
//! `groupings_gate`).
//!
//! Fixture: `tests/golden/area/domain.json`; readable frontiers from
//! `cw_core/tests/guid_law_gate.rs` (`DAMAGED`). See `tests/support/mod.rs`.
//!
//! **Corpus reality (why this gate uses `mwgh2025` scheme 8):** only one
//! engagement in the freeze has `AM.GROUPn` values that resolve against an
//! authentic `MP` prefix — `mwgh2025`, slot 8, 279 assignments, every one a
//! verbatim `MAP_NO` of an assignable scheme-8 node. `hull2024` populates slots
//! 2 (213) and 9 (340) but both target schemes exist only inside that member's
//! fabricated tail, so all 553 of its values are unresolvable by construction
//! (they are also stored space-padded in slot 9, i.e. never verbatim `MAP_NO`).
//! `corp2026` populates no `GROUPn` slot at all. The write law below is asserted
//! on the one corpus where the data can carry it.

mod support;

use cw_app::grouping_assign::{
    account_assignment, assign_account, assignments, ASSIGNABLE_DIGITS,
};
use cw_app::groups::MpTable;
use support::{frontier, open, prefix, table_fixture, triples_at, usize_at};

const MWGH: &str = "mwgh2025";

#[test]
fn f37_assignment_law_and_write() {
    let mp_raw = open(MWGH, "MP.dbf");
    let mp_n = frontier(MWGH, "MP", &mp_raw);
    let mp = MpTable::from_table(&prefix(&mp_raw, mp_n)).unwrap();
    let mut am = open(MWGH, "AM.dbf");

    // The census the fixture pins: one live slot, 279 values, all resolvable.
    let f = table_fixture(MWGH, "mp");
    assert_eq!(usize_at(&f, "frontier"), mp_n);
    let assigned = support::map_at(&f, "assignment");
    assert_eq!(assigned.get("8").copied(), Some(279), "mwgh slot 8 census");
    assert_eq!(
        assigned.len(),
        1,
        "mwgh populates exactly one grouping slot: {assigned:?}"
    );
    assert!(mp.assignment_issues(&am).is_empty(), "mwgh assignments all resolve");
    assert_eq!(triples_at(&f, "assignment_issues").len(), 0);

    // Slot 1 has no `AM` column; every other assignable digit does.
    assert!(ASSIGNABLE_DIGITS.iter().all(|&d| d != 1));
    assert_eq!(assignments(&am, 8).len(), 279);
    assert_eq!(assignments(&am, 2).len(), 0, "mwgh has no slot-2 assignments");

    // F37 write law on a real row: verbatim `MAP_NO`, assignable node only,
    // atomic refusal, byte-restoring round trip.
    let (row, current) = assignments(&am, 8)[0].clone();
    assert_eq!(account_assignment(&am, row, 8).unwrap(), current);
    let before = am.encode();

    assert!(
        assign_account(&mut am, row, &mp, 8, "no-such-node").is_err(),
        "unknown target is refused"
    );
    assert!(
        assign_account(&mut am, row, &mp, 1, "111").is_err(),
        "slot 1 has no AM column"
    );
    assert!(
        assign_account(&mut am, row, &mp, 8, &"9".repeat(41)).is_err(),
        "over-width code is refused by write_cell"
    );
    assert_eq!(am.encode(), before, "refused writes must not touch bytes");

    // A blank-code sentinel in the scheme cannot receive an account either.
    let blank_row = (0..mp_n)
        .find(|&r| {
            !prefix(&mp_raw, mp_n).is_deleted(r)
                && mp
                    .digit(8)
                    .unwrap()
                    .nodes()
                    .iter()
                    .any(|n| n.row == r && !n.has_code())
        })
        .expect("scheme-8 blank sentinel row");
    assert!(
        MpTable::from_table(&prefix(&mp_raw, mp_n))
            .unwrap()
            .digit(8)
            .unwrap()
            .nodes()
            .iter()
            .any(|n| n.row == blank_row && !n.has_code()),
        "sentinel row is inside the scheme"
    );

    // Legal re-assignment round-trips, and clearing then restoring is a no-op.
    assign_account(&mut am, row, &mp, 8, "111").unwrap();
    assert_eq!(account_assignment(&am, row, 8).unwrap(), "111");
    assert_eq!(
        cw_app::grouping_assign::clear_assignment(&mut am, row, 8).is_ok(),
        true
    );
    assert!(account_assignment(&am, row, 8).unwrap().is_empty());
    assign_account(&mut am, row, &mp, 8, &current).unwrap();
    assert_eq!(account_assignment(&am, row, 8).unwrap(), current);
}
