//! P5.E2a gate — `MP` row model + tree walk (thin; full law in `groupings_gate`).
//!
//! Fixture: `tests/golden/area/domain.json` (`examples/freeze_area.rs`), a
//! kernel-derived regression pin; the readable frontier is the reference-decoder
//! evidence from `cw_core/tests/guid_law_gate.rs` (`DAMAGED`). See
//! `tests/support/mod.rs` for the F63 rationale: `hull2024/MP` is asserted over
//! its first 785 rows only, because its shipped LZMA stream is truncated and
//! everything after is fabricated.

mod support;

use cw_app::mappings::{MpTable, NodeKind, Scheme, MP_SCHEMA};
use support::{fixture, frontier, open, prefix, table_fixture, usize_at};

const HULL: &str = "hull2024";

#[test]
fn mp_schema_and_tree_walk() {
    // Format constant: 67 descriptors summing to the 1332-byte record (spec
    // `mapping.md`; the widths are the shipped type-library header, not a
    // measurement of these files).
    assert_eq!(MP_SCHEMA.len(), 67);
    assert_eq!(
        MP_SCHEMA.iter().map(|f| f.len as usize).sum::<usize>() + 1,
        1332,
        "declared widths must sum to the record length"
    );

    let raw = open(HULL, "MP.dbf");
    assert_eq!(raw.record_len, 1332);

    // The assertion domain: header + first 785 rows (F63).
    let n = frontier(HULL, "MP", &raw);
    assert_eq!(n, 785, "hull2024/MP readable frontier (guid_law_gate DAMAGED)");
    let t = prefix(&raw, n);
    let table = MpTable::from_table(&t).unwrap();

    // The corpus census, pinned by the area fixture and re-derived here.
    let f = table_fixture(HULL, "mp");
    assert_eq!(usize_at(&f, "frontier"), n);
    assert_eq!(usize_at(&f, "records"), raw.count());
    assert_eq!(usize_at(&f, "live"), t.count() - table.deleted());

    // The statements tree is `MP.ID='8'` in the authentic prefix; the separate
    // `MP.ID='M'` mapping scheme does not appear until the fabricated tail, so
    // no authentic claim can be made about it.
    assert!(table.mapping().is_none(), "MP.ID='M' is fabricated-tail-only here");
    let m = table.digit(8).expect("hull scheme 8 statements tree");
    assert_eq!(m.get("111.100").unwrap().kind, NodeKind::Assignable);
    assert_eq!(m.path("111.100")[0].map_no, "100");
    assert_eq!(m.path("111.100").last().unwrap().map_no, "111.100");
    assert_eq!(m.depth("111.100"), 3);
    assert_eq!(
        m.path("111.100")
            .iter()
            .map(|n| n.map_no.as_str())
            .collect::<Vec<_>>(),
        vec!["100", "110", "111", "111.100"],
        "dotted nesting under the 100/110/111 template"
    );

    // Pre-order covers every coded node of every scheme exactly once.
    for s in table.scheme_keys() {
        let sc = table.scheme(s).unwrap();
        assert_eq!(sc.walk().len(), sc.coded().count(), "scheme {s}");
    }

    // Digit 2 is one of the schemes that only exists in the fabricated tail, so
    // the authentic prefix must not carry it (`MP.ID='2'` is tail-only).
    assert!(table.digit(2).is_none(), "scheme 2 is fabricated-tail-only");
    assert_eq!(
        table.scheme_keys(),
        vec![Scheme::Digit(1), Scheme::Digit(8)],
        "hull2024 authentic scheme set"
    );

    // The digit-8 tree is the real statements tree; `10-7` is a scheme-2 code.
    let d8 = table.digit(8).unwrap();
    assert_eq!(d8.get("111.100").unwrap().kind, NodeKind::Assignable);
    assert!(d8.get("10-7").is_none(), "10-7 belongs to the tail-only scheme 2");

    // `mwgh2025` carries the same statements tree over the same kind of
    // truncated member, with a different frontier (785 of 2342).
    let mraw = open("mwgh2025", "MP.dbf");
    let mn = frontier("mwgh2025", "MP", &mraw);
    assert_eq!(mn, 785);
    let mwgh = MpTable::from_table(&prefix(&mraw, mn)).unwrap();
    assert!(mwgh.digit(2).is_none());
    assert!(mwgh.digit(9).is_none(), "mwgh scheme 9 is tail-only");

    // The fixture is the single source every other gate in this area reads.
    let corpora = fixture();
    assert!(
        corpora.get("corpora").and_then(|c| c.as_obj()).map(|v| v.len()) >= Some(3),
        "at least three frozen corpora"
    );
}
