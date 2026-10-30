//! FEAT.018 gate: the account renumber cascade.
//!
//! Contract: `attachments/accounts.md` §6 (which parked this lane with "does
//! not pretend an AM-only renumber is safe"), `attachments/ui-shell.md` §9 (the
//! dialog contract), and `attachments/interlink.md` §4 (the numeric web the
//! cascade must keep intact).
//!
//! Three authorities, in the order the other domain gates use:
//!
//! 1. **Spec constants** — the cascade map (`AM.AC_NO`, `AM.AC_NO2`,
//!    `AM.AC_NO2CR`, `AM.PAC_NO`, `BL.NUMBER`, `GL.AC_NO`, `GL.B_ACT`) is asserted
//!    inline against the shipped `AM`/`GL`/`BL` schemas, and so is the measured
//!    per-column alignment law (`AM.AC_NO`/`BL.NUMBER`/`GL.AC_NO` right-justified,
//!    `AM.AC_NO2` left, `GL.B_ACT` genuinely mixed).
//! 2. **Reference-decoder evidence** — `hull2024/BL` (frontier 1918 of 3462) and
//!    `mwgh2025/BL` (1894 of 1958) are truncated LZMA members; the cascade runs
//!    over the readable prefix only (`tests/golden/README.md`, `support::FRONTIER`).
//! 3. **Engine self-freeze** — the population numbers below are recomputed from
//!    the frozen bytes here, never copied from a fixture.
//!
//! What is locked:
//!
//! * the full cascade on a synthetic engagement (all seven columns, both
//!   alignments, tombstones preserved, the GL header sentinel untouched);
//! * the cascade on the frozen corpora (`hull2024` full shift, `mwgh2025` single
//!   rename, `corp2026` refusal) with exact cell/row/tombstone counts;
//! * collision refusal in every namespace plus ambiguous-source refusal;
//! * byte stability (encode(parse(x)) == x and no-op ⇒ no byte changed);
//! * idempotence: a plan already in effect is an empty plan and writes nothing; a
//!   *partly* live plan (what a re-applied dense shift looks like) is refused as
//!   `PartialApplication` rather than double-applied;
//! * the one-pass snapshot law for chained renames (`1000 -> 2000`,
//!   `2000 -> 3001` in a single plan);
//! * reference resolvability recomputed from the produced tables, independently of
//!   the module's own audit.
//!
//! Every corpus assertion runs on a throwaway copy, and the damaged tables are
//! truncated to their readable prefix, so nothing here depends on fabricated tail
//! bytes (`tests/golden/README.md` F63).

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use cw_app::accounts::{Chart, AM_SCHEMA};
use cw_app::adjustments::{GL_HEADER_ACCOUNT, GL_SCHEMA};
use cw_app::renumber::{
    cascade, canonical_number, cell_align, Align, CascadeIssue, Rename, RenumberPlan, RenumberScope,
    AM_REFERENCE_COLUMNS, BL_REFERENCE_COLUMNS, CASCADE_MAP, DELETED, GL_REFERENCE_COLUMNS,
    MAX_ACCOUNT_WIDTH,
};
use cw_core::dbf::Table;
use support::open;

// ---------------------------------------------------------------------------
// Synthetic engagement
// ---------------------------------------------------------------------------

fn field_descriptor(name: &str, kind: u8, len: u8, dec: u8) -> Vec<u8> {
    let mut d = vec![0u8; 32];
    d[..name.len()].copy_from_slice(name.as_bytes());
    d[11] = kind;
    d[16] = len;
    d[17] = dec;
    d
}

/// The physical `BL` layout (`cw_core::cube::CUBE_COLUMNS` widths, 255-byte
/// records) — a synthetic `BL` must carry the semantic columns the cube reads.
const BL_LAYOUT: [(&str, u8, u8, u8); 19] = [
    ("ID", b'C', 1, 0),
    ("GROUP", b'C', 1, 0),
    ("NUMBER", b'C', 40, 0),
    ("ENTITY", b'C', 40, 0),
    ("YEAR", b'C', 1, 0),
    ("BUCKET", b'C', 1, 0),
    ("TYPE", b'C', 1, 0),
    ("BOOKED", b'C', 1, 0),
    ("PERIOD", b'C', 2, 0),
    ("CUSTOM", b'C', 2, 0),
    ("AMT", b'N', 19, 2),
    ("AMT_YTD", b'N', 19, 2),
    ("AMT_DIST", b'N', 19, 2),
    ("FX", b'N', 19, 2),
    ("FX_YTD", b'N', 19, 2),
    ("FX_DIST", b'N', 19, 2),
    ("CWGUID", b'C', 22, 0),
    ("CWDATE", b'C', 14, 0),
    ("CREATEDON", b'C', 14, 0),
];

fn table(layout: &[(&str, u8, u8, u8)], rows: &[&[(&str, &str)]]) -> Table {
    let mut h = Vec::new();
    h.push(0xF5u8);
    h.extend_from_slice(&[0x7A, 0x0A, 0x16]);
    h.extend_from_slice(&(rows.len() as u32).to_le_bytes());
    let header_len = 32 + layout.len() * 32 + 1;
    h.extend_from_slice(&(header_len as u16).to_le_bytes());
    let record_len = 1 + layout.iter().map(|f| f.2 as usize).sum::<usize>();
    h.extend_from_slice(&(record_len as u16).to_le_bytes());
    h.extend(std::iter::repeat(0u8).take(20));
    for (name, kind, len, dec) in layout {
        h.extend(field_descriptor(name, *kind, *len, *dec));
    }
    h.push(0x0D);
    for row in rows {
        let mut rec = vec![b' '; record_len];
        rec[0] = b' ';
        let mut off = 1usize;
        for (name, kind, len, _) in layout {
            let width = *len as usize;
            if let Some((_, text)) = row.iter().find(|(c, _)| c == name) {
                if *text == "*" {
                    rec[0] = DELETED;
                } else {
                    let bytes = text.as_bytes();
                    assert!(bytes.len() <= width, "{name} {text:?} wider than {width}");
                    // Corpus law (ui-shell.md §4): the cascade's reference
                    // columns are right-justified; numeric cells too.
                    let start = if *kind == b'N' {
                        off + width - bytes.len()
                    } else {
                        off + width - bytes.len()
                    };
                    rec[start..start + bytes.len()].copy_from_slice(bytes);
                }
            }
            off += width;
        }
        h.extend(rec);
    }
    h.push(0x1A);
    Table::parse(&h).expect("synthetic table parses")
}

/// The 81-column `AM` layout as `(name, kind, len, dec)` triples.
fn am_layout() -> Vec<(&'static str, u8, u8, u8)> {
    AM_SCHEMA.iter().map(|f| (f.name, f.kind, f.len, f.dec)).collect()
}

/// The 51-column `GL` layout.
fn gl_layout() -> Vec<(&'static str, u8, u8, u8)> {
    GL_SCHEMA.iter().map(|f| (f.name, f.kind, f.len, f.dec)).collect()
}

/// A four-table synthetic engagement: `AM`, `BL`, `GL`, `MP`.
///
/// Two posting accounts (`1000`, `2000`), one memo row (`AC_NO='z'`), one
/// `AM` tombstone still carrying `1000`, cube rows per account plus a `BL`
/// tombstone, and a `GL` journal with a 40-Z header plus two lines.
struct Synthetic {
    am: Table,
    bl: Table,
    gl: Table,
    mp: Table,
}

fn synthetic() -> Synthetic {
    let am = table(
        &am_layout(),
        &[
            &[("AC_NO", "1000"), ("TYPE", "B"), ("AC_DESC", "Cash")],
            &[
                ("AC_NO", "2000"),
                ("TYPE", "I"),
                ("AC_NO2", "1000"),
                ("PAC_NO", "1000"),
                ("AC_DESC", "Sales"),
            ],
            &[("AC_NO", "z"), ("TYPE", "M"), ("AC_DESC", "Adjustments")],
            // tombstone that still references 1000
            &[("AC_NO", "*"), ("TYPE", "*"), ("AC_NO2", "1000")],
        ],
    );
    let bl = table(
        &BL_LAYOUT,
        &[
            &[("NUMBER", "1000"), ("ID", "X"), ("GROUP", "A"), ("AMT", "10.00")],
            &[("NUMBER", "2000"), ("ID", "X"), ("GROUP", "A"), ("AMT", "20.00")],
            &[("NUMBER", "9000"), ("ID", "X"), ("GROUP", "A"), ("AMT", "30.00")],
            // tombstone cube row on 1000
            &[("NUMBER", "1000"), ("ID", "P"), ("GROUP", "A"), ("AMT", "*")],
        ],
    );
    let gl = table(
        &gl_layout(),
        &[
            &[
                ("AC_NO", GL_HEADER_ACCOUNT),
                ("B_ACT", GL_HEADER_ACCOUNT),
                ("REFNO", "A1"),
            ],
            &[("AC_NO", "1000"), ("B_ACT", "2000"), ("REFNO", "A1")],
            &[("AC_NO", "2000"), ("B_ACT", "1000"), ("REFNO", "A1")],
            // tombstone journal row
            &[("AC_NO", "1000"), ("B_ACT", "*"), ("REFNO", "A0")],
        ],
    );
    let mp = table(
        &[
            ("MAP_NO", b'C', 40, 0),
            ("TAC_NO", b'C', 16, 0),
            ("TAX1", b'C', 16, 0),
        ],
        &[
            &[("MAP_NO", "100"), ("TAC_NO", "")],
            &[("MAP_NO", "5000"), ("TAC_NO", "TAX1")],
        ],
    );
    Synthetic { am, bl, gl, mp }
}

impl Synthetic {
    fn scope(&self) -> RenumberScope {
        RenumberScope::new(self.am.clone(), self.bl.clone(), self.gl.clone())
            .with_mp(self.mp.clone())
    }

    fn reencode(&self) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        (self.am.encode(), self.bl.encode(), self.gl.encode())
    }
}

/// Chart account numbers with CW's right-justification padding removed.
///
/// `Chart` indexes by the *raw* `AC_NO` cell (ui-shell.md §4: `Chart::get` needs
/// the padded value, the shell resolves through `find_account`), so a gate that
/// talks about "the account number" must trim.
fn chart_numbers(chart: &Chart) -> BTreeSet<String> {
    chart
        .posting()
        .map(|a| a.ac_no.trim().to_string())
        .collect()
}

fn tokens(t: &Table, column: &str) -> Vec<(usize, String)> {
    (0..t.count())
        .map(|r| {
            let v = t.cell_str(r, column).unwrap().trim().to_string();
            (r, v)
        })
        .collect()
}

/// The cascade map is the spec's column list, in this exact order.
#[test]
fn cascade_map_is_the_spec_list() {
    assert_eq!(
        CASCADE_MAP,
        [
            ("AM", "AC_NO"),
            ("AM", "AC_NO2"),
            ("AM", "AC_NO2CR"),
            ("AM", "PAC_NO"),
            ("BL", "NUMBER"),
            ("GL", "AC_NO"),
            ("GL", "B_ACT"),
        ]
    );
    assert_eq!(AM_REFERENCE_COLUMNS, ["AC_NO2", "AC_NO2CR", "PAC_NO"]);
    assert_eq!(BL_REFERENCE_COLUMNS, ["NUMBER"]);
    assert_eq!(GL_REFERENCE_COLUMNS, ["AC_NO", "B_ACT"]);
    assert_eq!(MAX_ACCOUNT_WIDTH, 40);
    // Every mapped column exists in the corresponding reference schema.
    let s = synthetic();
    for (t, col) in CASCADE_MAP {
        let table = match t {
            "AM" => &s.am,
            "BL" => &s.bl,
            _ => &s.gl,
        };
        assert!(table.field_index(col).is_some(), "{t}.{col} missing");
    }
}

#[test]
fn synthetic_cascade_rewrites_every_column_and_preserves_tombstones() {
    let s = synthetic();
    let scope = s.scope();
    let before = s.reencode();

    let plan = RenumberPlan::from_pairs(&[("1000".to_string(), "1100".to_string())]).unwrap();
    let preview = cascade(&scope, &plan).expect("cascade applies");
    assert!(preview.changed, "the cascade changes bytes");

    // AM.AC_NO primary row
    assert_eq!(preview.am.cell_str(0, "AC_NO").unwrap().trim(), "1100");
    // AM.AC_NO2 / PAC_NO references on the 2000 row
    assert_eq!(preview.am.cell_str(1, "AC_NO2").unwrap().trim(), "1100");
    assert_eq!(preview.am.cell_str(1, "PAC_NO").unwrap().trim(), "1100");
    // untouched neighbours
    assert_eq!(preview.am.cell_str(1, "AC_NO").unwrap().trim(), "2000");
    assert_eq!(preview.am.cell_str(2, "AC_NO").unwrap().trim(), "z", "memo marker");
    // BL.NUMBER live rows move, the 9000 row does not
    let bl = tokens(&preview.bl, "NUMBER");
    assert_eq!(bl[0].1, "1100");
    assert_eq!(bl[1].1, "2000");
    assert_eq!(bl[2].1, "9000");
    // GL line cells on both sides of the entry move; the header sentinel does not
    assert_eq!(
        preview.gl.cell_str(0, "AC_NO").unwrap().trim(),
        GL_HEADER_ACCOUNT,
        "the 40-Z journal header is not an account reference"
    );
    assert_eq!(preview.gl.cell_str(1, "AC_NO").unwrap().trim(), "1100");
    assert_eq!(preview.gl.cell_str(1, "B_ACT").unwrap().trim(), "2000");
    assert_eq!(preview.gl.cell_str(2, "B_ACT").unwrap().trim(), "1100");

    // Tombstones are byte-identical: AM row 3 and BL row 3 and GL row 3.
    for r in [3usize] {
        assert_eq!(
            preview.am.records[r], s.am.records[r],
            "AM tombstone {r} is preserved verbatim"
        );
        assert_eq!(
            preview.bl.records[r], s.bl.records[r],
            "BL tombstone {r} is preserved verbatim"
        );
        assert_eq!(
            preview.gl.records[r], s.gl.records[r],
            "GL tombstone {r} is preserved verbatim"
        );
    }
    assert_eq!(
        preview.report.tombstones,
        vec![("AM", 3usize), ("BL", 3usize), ("GL", 3usize)],
        "every tombstone holding a renamed account is reported"
    );

    // Edits: 1 AM.AC_NO + 2 AM refs + 1 BL + 1 GL.AC_NO + 2 GL.B_ACT cells.
    assert_eq!(
        preview.report.table_cells("AM"),
        3,
        "edits: {:?}",
        preview.report.edits
    );
    assert_eq!(preview.report.table_cells("BL"), 1);
    assert_eq!(
        preview.report.table_cells("GL"),
        2,
        "one cell per journal line: AC_NO then B_ACT"
    );
    assert_eq!(preview.report.cells(), 6);
    assert_eq!(
        preview.report.table_stats(),
        vec![("AM", 3, 2), ("BL", 1, 1), ("GL", 2, 2)]
    );
    for e in &preview.report.edits {
        assert_eq!(e.before, "1000");
        assert_eq!(e.after, "1100");
    }

    // Danglers: the single pre-existing one (2000 is not a BL key... it is) —
    // nothing is created and nothing disappears.
    assert_eq!(preview.report.dangling_before, preview.report.dangling_after);

    // Byte stability: every produced table re-encodes identically, and the input
    // tables are still intact on the scope.
    assert_eq!(preview.am.encode(), Table::parse(&preview.am.encode()).unwrap().encode());
    assert_eq!(preview.bl.encode(), Table::parse(&preview.bl.encode()).unwrap().encode());
    assert_eq!(preview.gl.encode(), Table::parse(&preview.gl.encode()).unwrap().encode());
    assert_eq!(s.reencode(), before, "the scope is untouched by the cascade");
    assert_ne!(preview.am.encode(), s.am.encode());
}

#[test]
fn cascade_preserves_alignment() {
    let s = synthetic();
    let scope = s.scope();
    let plan = RenumberPlan::from_pairs(&[("1000".to_string(), "1100".to_string())]).unwrap();
    let preview = cascade(&scope, &plan).unwrap();

    for (label, table, original) in [
        ("AM", &preview.am, &s.am),
        ("BL", &preview.bl, &s.bl),
        ("GL", &preview.gl, &s.gl),
    ] {
        for r in 0..table.count() {
            for (t, col) in CASCADE_MAP {
                if t != label {
                    continue;
                }
                let before = original.cell(r, col).unwrap();
                let after = table.cell(r, col).unwrap();
                if before == after {
                    continue;
                }
                assert_eq!(
                    cell_align(before),
                    cell_align(after),
                    "{label}.{col} row {r} changed alignment"
                );
                // Only token bytes moved: the width is the field width.
                assert_eq!(before.len(), after.len());
            }
        }
    }
}

/// `1000 -> 1100` and `2000 -> 1100` cannot both be applied.
#[test]
fn duplicate_targets_are_refused() {
    let s = synthetic();
    let scope = s.scope();
    let plan = RenumberPlan::from_pairs(&[
        ("1000".to_string(), "1100".to_string()),
        ("2000".to_string(), "1100".to_string()),
    ])
    .unwrap();
    let res = plan.resolve(&scope).unwrap();
    assert!(res.is_blocked());
    assert!(matches!(
        res.issues[0],
        CascadeIssue::DuplicateTarget { .. }
    ));
    let err = cascade(&scope, &plan).unwrap_err().to_string();
    assert!(err.contains("renumber refused"), "{err}");
    assert!(err.contains("claimed by 2 accounts"), "{err}");
}

#[test]
fn collisions_and_ambiguous_sources_are_refused() {
    let s = synthetic();
    let scope = s.scope();

    // target taken in the chart namespace (a stationary account)
    let plan = RenumberPlan::from_pairs(&[("1000".to_string(), "2000".to_string())]).unwrap();
    let res = plan.resolve(&scope).unwrap();
    assert!(res.issues.iter().any(|i| matches!(
        i,
        CascadeIssue::TargetTaken { number, namespace } if number == "2000" && *namespace == "chart"
    )), "{:?}", res.issues);

    // target taken in the map namespace
    let plan = RenumberPlan::from_pairs(&[("1000".to_string(), "5000".to_string())]).unwrap();
    let res = plan.resolve(&scope).unwrap();
    assert!(res.issues.iter().any(|i| matches!(
        i,
        CascadeIssue::TargetTaken { number, namespace } if number == "5000" && *namespace == "map"
    )), "{:?}", res.issues);

    // target taken in the tax namespace
    let plan = RenumberPlan::from_pairs(&[("1000".to_string(), "TAX1".to_string())]).unwrap();
    let res = plan.resolve(&scope).unwrap();
    assert!(res.issues.iter().any(|i| matches!(
        i,
        CascadeIssue::TargetTaken { number, namespace } if number == "TAX1" && *namespace == "tax"
    )), "{:?}", res.issues);

    // target taken in the cube namespace (a BL key that is not an account)
    let plan = RenumberPlan::from_pairs(&[("1000".to_string(), "9000".to_string())]).unwrap();
    let res = plan.resolve(&scope).unwrap();
    assert!(res.issues.iter().any(|i| matches!(
        i,
        CascadeIssue::TargetTaken { number, namespace } if number == "9000" && *namespace == "cube"
    )), "{:?}", res.issues);

    // unknown source
    let plan = RenumberPlan::from_pairs(&[("7777".to_string(), "8888".to_string())]).unwrap();
    assert!(matches!(
        plan.resolve(&scope).unwrap().issues[0],
        CascadeIssue::MissingAccount { .. }
    ));
}

#[test]
fn ambiguous_source_is_refused_when_the_number_is_also_a_map_code() {
    let s = synthetic();
    // 1000 is both a chart account and (in this engagement) a map code.
    let mp = table(
        &[
            ("MAP_NO", b'C', 40, 0),
            ("TAC_NO", b'C', 16, 0),
            ("TAX1", b'C', 16, 0),
        ],
        &[&[("MAP_NO", "1000"), ("TAC_NO", "")]],
    );
    let scope = RenumberScope::new(s.am.clone(), s.bl.clone(), s.gl.clone()).with_mp(mp);

    let plan = RenumberPlan::from_pairs(&[("1000".to_string(), "1100".to_string())]).unwrap();
    let res = plan.resolve(&scope).unwrap();
    assert!(res.issues.iter().any(|i| matches!(
        i,
        CascadeIssue::AmbiguousSource { number, namespace } if number == "1000" && *namespace == "map"
    )), "{:?}", res.issues);
}

#[test]
fn invalid_targets_are_refused() {
    let s = synthetic();
    let scope = s.scope();
    assert!(
        RenumberPlan::from_pairs(&[("1000".to_string(), String::new())]).is_err(),
        "an empty target is refused at plan time"
    );
    for (to, reason) in [
        (" 1100 ".to_string(), "leading/trailing whitespace"),
        ("1".repeat(41), "wider than the 40-byte account field"),
        ("1100\u{20ac}".to_string(), "non-Latin-1"),
    ] {
        let plan = RenumberPlan::from_pairs(&[("1000".to_string(), to.clone())]).unwrap();
        let res = plan.resolve(&scope).unwrap();
        assert!(
            res.issues
                .iter()
                .any(|i| matches!(i, CascadeIssue::InvalidTarget { .. })),
            "{reason}: {to:?} -> {:?}",
            res.issues
        );
    }
    let empty_from = RenumberPlan::from_pairs(&[("".to_string(), "1100".to_string())]);
    assert!(empty_from.is_err(), "an empty source is refused at plan time");
}

#[test]
fn empty_and_identity_plans_are_byte_identical() {
    let s = synthetic();
    let scope = s.scope();
    let before = s.reencode();

    for plan in [
        RenumberPlan::default(),
        RenumberPlan::from_pairs(&[]).unwrap(),
        RenumberPlan::from_pairs(&[("1000".to_string(), "1000".to_string())]).unwrap(),
    ] {
        assert!(plan.is_empty());
        let preview = cascade(&scope, &plan).unwrap();
        assert!(!preview.changed);
        assert_eq!(preview.report.cells(), 0);
        assert_eq!(preview.am.encode(), before.0);
        assert_eq!(preview.bl.encode(), before.1);
        assert_eq!(preview.gl.encode(), before.2);
        assert_eq!(
            preview.changed_tables(&scope),
            vec![("AM", false), ("BL", false), ("GL", false)]
        );
    }
}

#[test]
fn applying_the_same_plan_twice_is_a_fixed_point() {
    let s = synthetic();
    let plan = RenumberPlan::from_pairs(&[("1000".to_string(), "1100".to_string())]).unwrap();

    let first = cascade(&s.scope(), &plan).unwrap();
    assert!(first.changed);

    let applied = RenumberScope::new(first.am.clone(), first.bl.clone(), first.gl.clone())
        .with_mp(s.mp.clone());
    let res = plan.resolve(&applied).unwrap();
    assert!(!res.is_blocked(), "{:?}", res.issues);
    assert!(res.renames.is_empty(), "nothing left to rename");
    assert_eq!(res.already_applied, vec!["1000".to_string()]);

    let second = cascade(&applied, &plan).unwrap();
    assert!(!second.changed, "the second cascade is a true no-op");
    assert_eq!(second.report.cells(), 0);
    assert_eq!(second.am.encode(), first.am.encode());
    assert_eq!(second.bl.encode(), first.bl.encode());
    assert_eq!(second.gl.encode(), first.gl.encode());
}

/// Chained renames must not double-apply: `1000 -> 1100` and `1100 -> 1200`
/// in one plan move the 1000 row to 1200 and the 2000 row stays put.
#[test]
fn chained_renames_apply_once_from_the_snapshot() {
    let s = synthetic();
    let scope = s.scope();
    let plan = RenumberPlan::from_pairs(&[
        ("1100".to_string(), "1200".to_string()),
        ("1000".to_string(), "1100".to_string()),
    ])
    .unwrap();
    // `1100` is not an account in this fixture, so the plan is refused: the chain
    // source must exist. Prove the snapshot law with a legal chain instead.
    assert!(plan.resolve(&scope).unwrap().is_blocked());

    // Legal chain on a fixture whose two accounts are 1000 and 2000:
    // 1000 -> 2000 is a collision with the stationary 2000 row, so build a
    // three-account variant where the chain is legal.
    let am = table(
        &am_layout(),
        &[
            &[("AC_NO", "1000"), ("TYPE", "B")],
            &[("AC_NO", "2000"), ("TYPE", "B")],
            &[("AC_NO", "3000"), ("TYPE", "B")],
        ],
    );
    let bl = table(
        &BL_LAYOUT,
        &[
            &[("NUMBER", "1000"), ("ID", "X")],
            &[("NUMBER", "2000"), ("ID", "X")],
            &[("NUMBER", "3000"), ("ID", "X")],
        ],
    );
    let gl = table(
        &gl_layout(),
        &[&[("AC_NO", "1000"), ("B_ACT", "2000")]],
    );
    let scope = RenumberScope::new(am, bl, gl);
    let plan = RenumberPlan::from_pairs(&[
        ("1000".to_string(), "2000".to_string()),
        ("2000".to_string(), "3001".to_string()),
    ])
    .unwrap();
    let res = plan.resolve(&scope).unwrap();
    assert!(!res.is_blocked(), "{:?}", res.issues);
    let preview = cascade(&scope, &plan).unwrap();
    assert_eq!(preview.am.cell_str(0, "AC_NO").unwrap().trim(), "2000");
    assert_eq!(preview.am.cell_str(1, "AC_NO").unwrap().trim(), "3001");
    assert_eq!(
        preview.am.cell_str(2, "AC_NO").unwrap().trim(),
        "3000",
        "a stationary account is never touched"
    );
    let numbers: Vec<String> = tokens(&preview.bl, "NUMBER")
        .into_iter()
        .map(|(_, v)| v)
        .collect();
    assert_eq!(numbers, vec!["2000", "3001", "3000"]);
    assert_eq!(preview.gl.cell_str(0, "AC_NO").unwrap().trim(), "2000");
    assert_eq!(preview.gl.cell_str(0, "B_ACT").unwrap().trim(), "3001");
}

// ---------------------------------------------------------------------------
// Corpus
// ---------------------------------------------------------------------------

fn corpus_dir(tag: &str) -> PathBuf {
    support::dbf_root().join(tag)
}

/// A throwaway copy of one corpus' AM/BL/GL/MP tables (the frozen fixtures are
/// never written by this gate).
static SCRATCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = SCRATCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dst = std::env::temp_dir().join(format!(
        "ocw-renumber-{tag}-{}-{}-{n}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_dir_all(&dst);
    fs::create_dir_all(&dst).unwrap();
    for entry in fs::read_dir(corpus_dir(tag)).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        if ["am.dbf", "bl.dbf", "gl.dbf", "mp.dbf"]
            .iter()
            .any(|s| lower.ends_with(s))
        {
            fs::copy(entry.path(), dst.join(&name)).unwrap();
        }
    }
    dst
}

/// Corpus scope, with each damaged table truncated to its readable prefix.
fn corpus_scope(tag: &str) -> (RenumberScope, usize, usize) {
    let am = open(tag, "AM.dbf");
    let bl_full = open(tag, "BL.dbf");
    let gl = open(tag, "GL.dbf");
    let mp_full = open(tag, "MP.dbf");
    let bl_frontier = support::frontier(tag, "BL", &bl_full);
    let mp_frontier = support::frontier(tag, "MP", &mp_full);
    let bl = support::prefix(&bl_full, bl_frontier);
    let mp = support::prefix(&mp_full, mp_frontier);
    (
        RenumberScope::new(am, bl, gl).with_mp(mp),
        bl_frontier,
        mp_frontier,
    )
}

/// The corpus alignment law, per column (`ui-shell.md` §4 and the census this
/// gate measured): the columns the cascade writes are **unanimously**
/// right-justified except `AM.AC_NO2` (unanimously left) and `GL.B_ACT`, which
/// is genuinely mixed (hull 24 right / 33 left, mwgh 24 / 49). The cascade never
/// normalizes: it re-emits each rewritten cell with the alignment it had.
#[test]
fn corpus_reference_columns_keep_their_alignment() {
    for tag in ["corp2026", "hull2024", "mwgh2025"] {
        let s = corpus_scope(tag).0;
        let chart = Chart::from_table(&s.am).unwrap();
        let posting: BTreeSet<usize> = chart.posting().map(|a| a.row).collect();

        let mut law: BTreeMap<(&str, &str), Align> = BTreeMap::new();
        law.insert(("AM", "AC_NO"), Align::Right);
        law.insert(("AM", "AC_NO2"), Align::Left);
        law.insert(("BL", "NUMBER"), Align::Right);
        law.insert(("GL", "AC_NO"), Align::Right);

        for (table, name) in [("AM", &s.am), ("BL", &s.bl), ("GL", &s.gl)] {
            for (t, col) in CASCADE_MAP {
                if t != table || name.field_index(col).is_none() {
                    continue;
                }
                for r in 0..name.count() {
                    if name.is_deleted(r) {
                        continue;
                    }
                    // `AM.AC_NO` is only a reference on a posting row; the memo
                    // marker (`z`) is left-aligned single-character text.
                    if table == "AM" && col == "AC_NO" && !posting.contains(&r) {
                        continue;
                    }
                    let raw = name.cell(r, col).unwrap();
                    let token = cw_core::canon::latin1(raw);
                    if token.trim().is_empty() || token.trim() == GL_HEADER_ACCOUNT {
                        continue;
                    }
                    if let Some(&want) = law.get(&(table, col)) {
                        assert_eq!(
                            cell_align(raw),
                            want,
                            "{tag} {table}.{col} row {r} = {:?}",
                            token.trim()
                        );
                    }
                }
            }
        }
    }
}

/// `GL.B_ACT` is the one mixed column; a rewrite must preserve whichever
/// alignment the original cell carried, byte for byte.
#[test]
fn mixed_alignment_column_is_preserved_per_cell() {
    let (scope, _, _) = corpus_scope("hull2024");
    let left = left_rows(&scope.gl).len();
    assert_eq!(left, 33, "hull GL.B_ACT left-justified body cells");

    let plan = RenumberPlan::from_delta(&scope, 100).unwrap();
    let preview = cascade(&scope, &plan).unwrap();
    for e in &preview.report.edits {
        if e.table != "GL" || e.column != "B_ACT" {
            continue;
        }
        let before = scope.gl.cell(e.row, "B_ACT").unwrap();
        let after = preview.gl.cell(e.row, "B_ACT").unwrap();
        assert_eq!(cell_align(before), cell_align(after), "{e:?}");
    }
    for r in left_rows(&scope.gl) {
        let before = scope.gl.cell(r, "B_ACT").unwrap();
        let after = preview.gl.cell(r, "B_ACT").unwrap();
        assert_eq!(cell_align(before), cell_align(after), "row {r}");
    }
}

/// Live journal body rows (the 40-Z header row is not a line) whose `B_ACT`
/// cell is left-justified.
fn left_rows(t: &Table) -> Vec<usize> {
    (0..t.count())
        .filter(|&r| !t.is_deleted(r))
        .filter(|&r| t.cell_str(r, "AC_NO").unwrap().trim() != GL_HEADER_ACCOUNT)
        .filter(|&r| {
            let raw = t.cell(r, "B_ACT").unwrap();
            !cw_core::canon::latin1(raw).trim().is_empty() && cell_align(raw) == Align::Left
        })
        .collect()
}

#[test]
fn hull2024_full_shift_cascades_bl_and_gl() {
    let (scope, bl_frontier, _) = corpus_scope("hull2024");
    assert_eq!(bl_frontier, 1918, "hull BL readable prefix (F63)");
    let before = (scope.am.encode(), scope.bl.encode(), scope.gl.encode());

    let plan = RenumberPlan::from_delta(&scope, 100).unwrap();
    assert_eq!(plan.renames().len(), 348, "hull numeric posting accounts");
    assert_eq!(
        plan.skipped(),
        ["26023X", "NETINC", "RSM01", "RSM02"],
        "non-canonical ids are labelled, not shifted"
    );

    let preview = cascade(&scope, &plan).expect("hull cascade applies");
    assert!(preview.changed);

    // AM: one primary cell per renamed row, no AM reference columns populated.
    assert_eq!(preview.report.table_cells("AM"), 348);
    // BL: every live prefix row whose NUMBER is one of the 348 accounts.
    assert_eq!(preview.report.table_cells("BL"), 1778);
    // GL: both sides of every journal line on a renamed account.
    assert_eq!(preview.report.table_cells("GL"), 42);
    assert_eq!(preview.report.cells(), 348 + 1778 + 42);

    // GL tombstones carry two renamed accounts and are preserved.
    assert_eq!(
        preview
            .report
            .tombstones
            .iter()
            .filter(|(t, _)| *t == "GL")
            .count(),
        2
    );

    // Every changed cell is a `+100` numeric shift and keeps its alignment.
    for e in &preview.report.edits {
        let from: i64 = e.before.parse().expect("numeric source");
        let to: i64 = e.after.parse().expect("numeric target");
        assert_eq!(to, from + 100, "{e:?}");
    }

    // The cube keeps resolving: the cascade creates no danglers.
    assert_eq!(preview.report.dangling_before, preview.report.dangling_after);

    // Byte stability of the produced tables, and the scope itself is untouched.
    for t in [&preview.am, &preview.bl, &preview.gl] {
        assert_eq!(t.encode(), Table::parse(&t.encode()).unwrap().encode());
    }
    assert_eq!((scope.am.encode(), scope.bl.encode(), scope.gl.encode()), before);
    assert_eq!(
        preview.changed_tables(&scope),
        vec![("AM", true), ("BL", true), ("GL", true)]
    );

    // The new chart numbers are exactly the old ones shifted, and the chart is
    // still clean (no duplicates introduced).
    let after = Chart::from_table(&preview.am).unwrap();
    assert!(after.issues().is_empty(), "{:?}", after.issues());
    assert_eq!(after.posting().count(), 352);
    let numbers = chart_numbers(&after);
    assert!(!numbers.contains("10020"), "the old number is gone");
    assert!(numbers.contains("10120"), "the new number is live");
    assert_eq!(numbers.len(), 352, "348 shifted + 4 labelled ids");

    // Idempotence, stated honestly for a dense shift. 25 of hull's 348 accounts are
    // exactly 100 apart (`16100`, `16200`), so after the shift some sources are
    // live again (as another row's new number) and some are not. "Already applied"
    // and "not yet applied" are then genuinely indistinguishable by inspection, and
    // the cascade refuses instead of guessing — the only outcome that cannot
    // double-shift.
    let apart: Vec<u64> = {
        let mut v: Vec<u64> = numbers
            .iter()
            .filter_map(|n| n.parse::<u64>().ok())
            .filter(|n| numbers.contains(&(n + 100).to_string()))
            .collect();
        v.sort();
        v
    };
    assert_eq!(apart.len(), 25, "hull accounts exactly 100 apart: {apart:?}");

    let applied = RenumberScope::new(preview.am.clone(), preview.bl.clone(), preview.gl.clone());
    let res = plan.resolve(&applied).unwrap();
    assert!(res.is_blocked(), "a re-applied dense shift is refused, not repeated");
    assert!(
        res.issues
            .iter()
            .any(|i| matches!(i, CascadeIssue::PartialApplication { .. })),
        "{:?}",
        res.issues
    );
    assert!(cascade(&applied, &plan).is_err());

    // A plan that is genuinely already in effect is a true no-op (single rename).
    let single = RenumberPlan::from_pairs(&[("10020".to_string(), "10120".to_string())]).unwrap();
    let once = cascade(&scope, &single).unwrap();
    assert!(once.changed);
    let done = RenumberScope::new(once.am.clone(), once.bl.clone(), once.gl.clone());
    let replay_plan = single.resolve(&done).unwrap();
    assert!(replay_plan.renames.is_empty(), "{:?}", replay_plan.issues);
    assert_eq!(replay_plan.already_applied, vec!["10020".to_string()]);
    let replay = cascade(&done, &single).unwrap();
    assert!(!replay.changed);
    assert_eq!(replay.report.cells(), 0);
    assert_eq!(replay.am.encode(), once.am.encode());
}

#[test]
fn mwgh2025_single_rename_cascades_the_cube() {
    let (scope, bl_frontier, _) = corpus_scope("mwgh2025");
    assert_eq!(bl_frontier, 1894, "mwgh BL readable prefix (F63)");
    // mwgh account numbers are `NNNN-NN`; no canonical integer exists, so the
    // delta plan is empty and every id is labelled instead.
    let delta_plan = RenumberPlan::from_delta(&scope, 100).unwrap();
    assert!(delta_plan.is_empty());
    assert_eq!(delta_plan.skipped().len(), 312);
    let noop = cascade(&scope, &delta_plan).unwrap();
    assert!(!noop.changed);
    assert_eq!(noop.am.encode(), scope.am.encode());

    // An explicit rename still cascades across AM/BL/GL.
    let plan = RenumberPlan::from_pairs(&[("1030-00".to_string(), "9030-00".to_string())]).unwrap();
    let res = plan.resolve(&scope).unwrap();
    assert!(!res.is_blocked(), "{:?}", res.issues);
    let preview = cascade(&scope, &plan).unwrap();
    assert_eq!(preview.report.table_cells("AM"), 1);
    assert_eq!(preview.report.table_cells("BL"), 8, "cube rows on 1030-00");
    assert_eq!(preview.report.table_cells("GL"), 0);
    let chart = Chart::from_table(&preview.am).unwrap();
    assert!(chart_numbers(&chart).contains("9030-00"));
    assert!(!chart_numbers(&chart).contains("1030-00"));
    assert!(chart.issues().is_empty(), "{:?}", chart.issues());
    assert_eq!(preview.report.dangling_before, preview.report.dangling_after);
}

#[test]
fn corp2026_refuses_the_ambiguous_shift() {
    let (scope, _, _) = corpus_scope("corp2026");
    let plan = RenumberPlan::from_delta(&scope, 100).unwrap();
    assert_eq!(plan.renames().len(), 62, "corp numeric posting accounts");

    // 1500/4000/5000/5500/8000 are chart accounts AND map codes: a value cascade
    // would rewrite the map references too. 1500 is also a legitimate target.
    let res = plan.resolve(&scope).unwrap();
    let ambiguous: BTreeSet<&str> = res
        .issues
        .iter()
        .filter_map(|i| match i {
            CascadeIssue::AmbiguousSource { number, namespace } if *namespace == "map" => {
                Some(number.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        ambiguous,
        BTreeSet::from(["1500", "4000", "5000", "5500", "8000"]),
        "{:?}",
        res.issues
    );
    assert!(res.issues.iter().any(|i| matches!(
        i,
        CascadeIssue::TargetTaken { number, namespace } if number == "8100" && *namespace == "map"
    )));

    let err = cascade(&scope, &plan).unwrap_err().to_string();
    assert!(err.contains("renumber refused"), "{err}");
    assert!(err.contains("also a map code"), "{err}");
}

#[test]
fn corp2026_single_unambiguous_rename_is_legal() {
    let (scope, _, _) = corpus_scope("corp2026");
    // 1001 -> 1099 is free in every namespace (no chart/map/tax/cube/journal hit).
    let plan = RenumberPlan::from_pairs(&[("1001".to_string(), "1099".to_string())]).unwrap();
    let res = plan.resolve(&scope).unwrap();
    assert!(!res.is_blocked(), "{:?}", res.issues);
    let preview = cascade(&scope, &plan).unwrap();
    assert_eq!(preview.report.table_cells("AM"), 1);
    assert_eq!(preview.report.table_cells("BL"), 0, "1001 has no live cube row");
    assert_eq!(preview.report.table_cells("GL"), 0);
    // Four BL tombstones still key on 1001; they are preserved verbatim.
    assert_eq!(
        preview
            .report
            .tombstones
            .iter()
            .filter(|(t, _)| *t == "BL")
            .count(),
        4
    );

    // Refusal leaves every table byte-identical: `1000` is free in the chart but
    // is a live `MP.MAP_NO`, so a value cascade would hijack a map reference.
    let before = preview.am.encode();
    let applied = RenumberScope::new(preview.am.clone(), preview.bl.clone(), preview.gl.clone())
        .with_mp(scope.mp.clone().expect("corp scope carries MP"));
    let bad = RenumberPlan::from_pairs(&[("1099".to_string(), "1000".to_string())]).unwrap();
    let res = bad.resolve(&applied).unwrap();
    assert!(res.issues.iter().any(|i| matches!(
        i,
        CascadeIssue::TargetTaken { number, namespace } if number == "1000" && *namespace == "map"
    )), "{:?}", res.issues);
    assert!(cascade(&applied, &bad).is_err());
    assert_eq!(preview.am.encode(), before);
}

#[test]
fn corpus_chart_is_still_clean_after_a_full_cascade() {
    let (scope, _, _) = corpus_scope("hull2024");
    let plan = RenumberPlan::from_delta(&scope, -100).unwrap();
    let preview = cascade(&scope, &plan).unwrap();
    let chart = Chart::from_table(&preview.am).unwrap();
    assert!(chart.issues().is_empty(), "{:?}", chart.issues());
    // The memo block (`TYPE='M'`, AC_NO='z') is never renumbered.
    assert_eq!(
        chart
            .accounts()
            .iter()
            .filter(|a| a.kind == cw_app::accounts::AccountKind::Memo)
            .count(),
        50
    );
    // The posting multiset is exactly the plan's mapping.
    let mut expected: Vec<String> = Chart::from_table(&scope.am)
        .unwrap()
        .posting()
        .map(|a| {
            let n = a.ac_no.trim();
            match canonical_number(n) {
                Some(v) => (v - 100).to_string(),
                None => n.to_string(),
            }
        })
        .collect();
    let mut actual: Vec<String> = chart.posting().map(|a| a.ac_no.trim().to_string()).collect();
    expected.sort();
    actual.sort();
    assert_eq!(actual, expected);
}

#[test]
fn canonical_number_law() {
    assert_eq!(canonical_number("1001"), Some(1001));
    assert_eq!(canonical_number("0"), Some(0));
    assert_eq!(canonical_number("007"), None, "leading zero is not canonical");
    assert_eq!(canonical_number("1332-00"), None);
    assert_eq!(canonical_number("1030-00"), None);
    assert_eq!(canonical_number("NETINC"), None);
    assert_eq!(canonical_number("z"), None);
    assert_eq!(canonical_number(""), None);
    assert_eq!(canonical_number(&"9".repeat(19)), None, "beyond i64");
    assert_eq!(canonical_number("9".repeat(18).as_str()), Some(999_999_999_999_999_999));
}

#[test]
fn delta_plan_labels_rather_than_invents() {
    let s = synthetic();
    let scope = s.scope();
    let plan = RenumberPlan::from_delta(&scope, 50).unwrap();
    // only `1000` and `2000` are canonical; `z` is a memo marker but it is not a
    // posting row, so it never enters the plan at all.
    let renames: Vec<(&str, &str)> = plan
        .renames()
        .iter()
        .map(|r: &Rename| (r.from.as_str(), r.to.as_str()))
        .collect();
    assert_eq!(renames, vec![("1000", "1050"), ("2000", "2050")]);
    assert!(plan.skipped().is_empty());
    assert_eq!(RenumberPlan::from_delta(&scope, 0).unwrap().renames().len(), 0);
}

#[test]
fn fixture_paths_exist() {
    // Guard against silently skipping the corpus tests on a broken checkout.
    for tag in ["corp2026", "hull2024", "mwgh2025"] {
        let dir: &Path = &corpus_dir(tag);
        assert!(dir.is_dir(), "{dir:?}");
    }
}

/// Reference resolvability is preserved cell by cell, recomputed independently of
/// the module's own audit.
///
/// The cascade owns every reference column that exists (`CASCADE_MAP` is the whole
/// set), so a live reference to a renamed account always moves with it. This test
/// re-derives that from the produced tables alone: every live reference either
/// still resolves inside chart ∪ map ∪ tax, or was already unresolvable before.
/// That is the "no new danglers" law (`attachments/accounts.md` §6) and it is why
/// an AM-only write can never be produced by this lane.
///
/// `RenumberPlan::resolve` also refuses a plan whose *projected* reference set
/// would dangle. Under the present map that guard cannot fire for a live cell — it
/// exists so that adding a reference column to a table without adding it to
/// `CASCADE_MAP` produces a refusal instead of silent corruption on the day the
/// spec grows.
#[test]
fn no_live_reference_is_left_dangling() {
    for (tag, delta) in [("hull2024", 100), ("corp2026", 100)] {
        let (scope, _, _) = corpus_scope(tag);
        let plan = RenumberPlan::from_delta(&scope, delta).unwrap();
        // corp2026 refuses the shift (ambiguous map codes); hull2024 applies it.
        let Ok(preview) = cascade(&scope, &plan) else {
            let res = plan.resolve(&scope).unwrap();
            assert!(res.is_blocked(), "{tag}: a refusal must carry issues");
            continue;
        };
        let _ = preview;
    }

    let (scope, _, _) = corpus_scope("hull2024");
    let plan = RenumberPlan::from_delta(&scope, 100).unwrap();
    let preview = cascade(&scope, &plan).unwrap();

    let accounts_after: BTreeSet<String> = Chart::from_table(&preview.am)
        .unwrap()
        .posting()
        .map(|a| a.ac_no.trim().to_string())
        .collect();
    let map_after: BTreeSet<String> = {
        let mp = scope.mp.as_ref().unwrap();
        (0..mp.count())
            .filter(|&r| !mp.is_deleted(r))
            .map(|r| mp.cell_str(r, "MAP_NO").unwrap().trim().to_string())
            .filter(|v| !v.is_empty())
            .collect()
    };
    let tax_after: BTreeSet<String> = (0..preview.am.count())
        .filter(|&r| !preview.am.is_deleted(r))
        .map(|r| preview.am.cell_str(r, "TAC_NO").unwrap().trim().to_string())
        .filter(|v| !v.is_empty())
        .collect();
    let resolves_after =
        |v: &str| accounts_after.contains(v) || map_after.contains(v) || tax_after.contains(v);

    let accounts_before: BTreeSet<String> = Chart::from_table(&scope.am)
        .unwrap()
        .posting()
        .map(|a| a.ac_no.trim().to_string())
        .collect();
    let tax_before: BTreeSet<String> = (0..scope.am.count())
        .filter(|&r| !scope.am.is_deleted(r))
        .map(|r| scope.am.cell_str(r, "TAC_NO").unwrap().trim().to_string())
        .filter(|v| !v.is_empty())
        .collect();
    let resolves_before =
        |v: &str| accounts_before.contains(v) || map_after.contains(v) || tax_before.contains(v);

    let mapping = plan.mapping();
    let mut checked = 0usize;
    for (table, before, after) in [
        ("BL", &scope.bl, &preview.bl),
        ("GL", &scope.gl, &preview.gl),
    ] {
        for (t, column) in CASCADE_MAP {
            if t != table || before.field_index(column).is_none() {
                continue;
            }
            for r in 0..before.count() {
                if before.is_deleted(r) {
                    continue;
                }
                let old = before.cell_str(r, column).unwrap().trim().to_string();
                if old.is_empty() || old == GL_HEADER_ACCOUNT {
                    continue;
                }
                if !resolves_before(&old) {
                    continue;
                }
                let new = after.cell_str(r, column).unwrap().trim().to_string();
                assert_eq!(
                    new,
                    mapping.get(&old).cloned().unwrap_or_else(|| old.clone()),
                    "{table}.{column} row {r} is not the mapped value"
                );
                assert!(
                    resolves_after(&new),
                    "{table}.{column} row {r}: {old:?} -> {new:?} no longer resolves"
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 1800, "hull: {checked} live references checked");
}
