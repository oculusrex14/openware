//! FEAT.009 gate: the data-import engine (column map, ASCII reader, F52
//! write-back).
//!
//! Golden inputs: the frozen `AM`/`BL` tables of `tests/golden/dbf/corp2026`
//! (325 chart rows / 2160 BL rows, 962 live) — the very engagement the F52a COM
//! run imported into (`AM 325->329`, `BL 490<->490 X/O-pair churn`).
//!
//! Provenance: `specs/import.md` §4b (the `RAWDATA` id-101 anatomy), §8.2 (the
//! binding grammar) and §8.5 (the green run + the value-landing table) are
//! format constants asserted **inline as literals** here. The corpus only
//! supplies the engagement state the engine must preserve; nothing about the
//! import grammar is read from a fixture, because no shipped layout file or
//! preset blob is part of the corpus.
//!
//! The gate locks:
//!
//! 1. **Grammar** — 1-based inclusive fixed ranges and 1-based delimited
//!    columns read the cells they name; 0-based bindings, `start > end`, an
//!    absent account-number binding and mixed binding families all refuse.
//! 2. **Layout storage** — a parsed layout re-emits its exact bytes; the preset
//!    anatomy (`hdr`/`count`/24-byte rows) parses and re-encodes byte-exactly;
//!    a programmatic map emits the minimal F52 cover layout and re-reads it
//!    identically.
//! 3. **AM write-back** — new accounts land with the CW row defaults (`LINENO`
//!    on the 32-grid, `POST='Y'`, `TYPE`, `SIGN`, fresh family-A `CWGUID`), while
//!    existing accounts are untouched unless `AVOverwriteAccDescription` is set.
//! 4. **BL write-back** — a non-zero balance claims a year-0 `O`/`X` pair with
//!    the exact cube fill law (`X`: `AMT_YTD = FX = FX_YTD = AMT`,
//!    `AMT_DIST = FX_DIST = "0.00"`; `O`: `AMT` only), a zero balance claims no
//!    row, and re-running the same import is byte-identical.
//! 5. **Refusals** — a short line, a missing column, a non-decimal amount, an
//!    over-wide account and a non-TB component all refuse without touching a
//!    byte.

use std::path::PathBuf;

use cw_app::accounts::Chart;
use cw_app::import::{
    apply, preset_layout, preview, read_ascii, read_ascii_report, read_grid, BalanceKind, Column,
    ColumnMap, Cents, ImportComponents, ImportField, ImportOptions, ImportPackage, IssueKind,
    LayoutFile, LayoutMode, LayoutOptions, Preset, PresetRow, PresetStore, Setting, SettingValue,
    ImportIssue, Severity, YearOffset, AM_DESC_WIDTH, AM_LINE_STEP, BL_COMPUTED_ID, BL_OPEN_ID,
};
use cw_core::cube::Cube;
use cw_core::dbf::Table;

const STAMP: &str = "20260203040506";

// ---- fixtures -------------------------------------------------------------

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/dbf/corp2026")
}

fn corpus_table(suffix: &str) -> Table {
    let dir = corpus_dir();
    for entry in std::fs::read_dir(&dir).expect("corpus dir") {
        let path = entry.unwrap().path();
        if path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_ascii_lowercase()
            .ends_with(suffix)
        {
            return Table::parse_path(&path).unwrap();
        }
    }
    panic!("no {suffix} in {}", dir.display());
}

fn am() -> Table {
    corpus_table("am.dbf")
}

fn bl() -> Table {
    corpus_table("bl.dbf")
}

fn opts() -> ImportOptions {
    ImportOptions {
        stamp: STAMP.to_string(),
        seed: 0x1_4D_50_52,
        ..Default::default()
    }
}

fn cell(t: &Table, r: usize, col: &str) -> String {
    t.cell_str(r, col).unwrap().trim().to_string()
}

/// Look a chart account up by its trimmed number.
///
/// The corpus stores `AM.AC_NO` right-justified (325/325 rows), and
/// `Chart::get` keys on the cell text with trailing spaces only, so every
/// lookup here trims. This is a corpus convention the gate asserts, not a
/// workaround: a new row is written right-justified for the same reason.
fn account(chart: &Chart, no: &str) -> Option<cw_app::accounts::AmAccount> {
    chart
        .accounts()
        .iter()
        .find(|a| a.ac_no.trim() == no)
        .cloned()
}

fn live_bl(t: &Table) -> usize {
    (0..t.count()).filter(|r| !t.is_deleted(*r)).count()
}

/// Live BL rows at `(number, year, bucket, id, group)`.
fn bl_rows(t: &Table, number: &str, year: &str, bucket: &str, id: &str, group: &str) -> Vec<usize> {
    (0..t.count())
        .filter(|r| {
            !t.is_deleted(*r)
                && cell(t, *r, "NUMBER") == number
                && cell(t, *r, "YEAR") == year
                && cell(t, *r, "BUCKET") == bucket
                && cell(t, *r, "ID") == id
                && cell(t, *r, "GROUP") == group
        })
        .collect()
}

/// The synthetic trial balance: `acct,desc,balance` (the F51 probe shape).
const SOURCE_CSV: &str = "\
acct,desc,balance
90001,Imported Bank Account,1250.00
90002,Imported Payable,-1250.00
1001,CASH - Depository Acct 289,0.00
";

/// The F52a shape-4 cover layout: `[COMMON]`/`[DELM]` with a 1-based column per
/// field, the `Current Year Balance db` triple, `delm_0/1/2 = 44` and
/// `multiple_specific=1`.
fn cover_layout_text() -> String {
    "\
[COMMON]\r\n\
multiple_specific=1\r\n\
skipheaderlines=1\r\n\
\r\n\
[DELM]\r\n\
editlist_account number=1\r\n\
editlist_account description=2\r\n\
editlist_Current Year Balance db=3\r\n\
delm.delm_0=44\r\n\
delm.delm_1=44\r\n\
delm.delm_2=44\r\n\
"
    .to_string()
}

fn cover_layout() -> ColumnMap {
    LayoutFile::parse(&cover_layout_text(), Some(LayoutMode::Delimited))
        .unwrap()
        .map()
        .clone()
}

/// The fixed-layout equivalent: 1-based inclusive byte ranges over a 42-char
/// record (`acct` 1..=8, `desc` 9..=32, `balance` 33..=42).
fn fixed_layout() -> ColumnMap {
    let mut map = ColumnMap {
        mode: Some(LayoutMode::Fixed),
        ..Default::default()
    };
    map.bind(ImportField::AccountNumber, Column::Fixed { start: 1, end: 8 });
    map.bind(
        ImportField::AccountDescription,
        Column::Fixed { start: 9, end: 32 },
    );
    map.bind(ImportField::NormalSign, Column::Fixed { start: 33, end: 33 });
    map
}

fn fixed_source() -> String {
    // 8 + 24 + 1 chars: account, description, D/C
    let rows = [
        ("90001", "Imported Bank Account"),
        ("90002", "Imported Payable"),
    ];
    let mut out = String::new();
    for (i, (no, desc)) in rows.iter().enumerate() {
        out.push_str(&format!("{no:<8}{desc:<24}{}\n", if i == 1 { 'C' } else { 'D' }));
    }
    out
}

// ---- 1. grammar -----------------------------------------------------------

#[test]
fn delimited_columns_are_one_based() {
    let map = cover_layout();
    assert!(map.refusals().is_empty(), "{:?}", map.refusals());
    let data = read_ascii(&map, SOURCE_CSV.as_bytes()).unwrap();
    assert_eq!(data.rows.len(), 3);
    assert_eq!(data.skipped_header, 1, "skipheaderlines=1");
    assert_eq!(data.delimiter, Some(','), "delm slot 2 = 0x2C");
    assert_eq!(data.rows[0].account_number(), "90001");
    assert_eq!(data.rows[0].description(), "Imported Bank Account");
    assert_eq!(
        data.rows[0].balance(BalanceKind::CurrentYear),
        Some(Cents(125_000))
    );
    assert_eq!(
        data.rows[1].balance(BalanceKind::CurrentYear),
        Some(Cents(-125_000))
    );
    assert_eq!(
        data.rows[2].balance(BalanceKind::CurrentYear),
        Some(Cents::ZERO)
    );
}

#[test]
fn fixed_ranges_are_one_based_inclusive() {
    let map = fixed_layout();
    assert!(map.refusals().is_empty(), "{:?}", map.refusals());
    let data = read_ascii(&map, fixed_source().as_bytes()).unwrap();
    assert_eq!(data.rows.len(), 2);
    assert_eq!(data.rows[0].account_number(), "90001");
    assert_eq!(data.rows[0].description(), "Imported Bank Account");
    assert_eq!(data.rows[0].normal_sign(), Some('D'));
    assert_eq!(data.rows[1].normal_sign(), Some('C'));
    // A 1-based range of 1..=8 is exactly 8 bytes: 8 would mean 9 bytes here.
    let col = Column::Fixed { start: 1, end: 8 };
    assert_eq!(col.width(), 8);
    assert_eq!(col.key_value(), "1,8,0,-1");
}

#[test]
fn zero_based_and_structural_bindings_refuse() {
    // 0-based start: the pre-F52 root cause, now an explicit refusal.
    let mut zero = ColumnMap {
        mode: Some(LayoutMode::Fixed),
        ..Default::default()
    };
    zero.bind(ImportField::AccountNumber, Column::Fixed { start: 0, end: 8 });
    let refusals = zero.refusals();
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].kind, IssueKind::ZeroBase);
    assert_eq!(refusals[0].severity, Severity::Refuse);

    // start > end.
    let mut inverted = ColumnMap {
        mode: Some(LayoutMode::Fixed),
        ..Default::default()
    };
    inverted.bind(ImportField::AccountNumber, Column::Fixed { start: 9, end: 2 });
    assert_eq!(inverted.refusals()[0].kind, IssueKind::InvalidRange);

    // no account-number binding.
    let mut anchorless = ColumnMap {
        mode: Some(LayoutMode::Delimited),
        ..Default::default()
    };
    anchorless.bind(
        ImportField::AccountDescription,
        Column::Delimited { column: 2 },
    );
    assert!(anchorless
        .refusals()
        .iter()
        .any(|i| i.kind == IssueKind::NoAccountNumber));

    // mixed families with no dispatch extension.
    let mut mixed = ColumnMap::default();
    mixed.bind(ImportField::AccountNumber, Column::Fixed { start: 1, end: 8 });
    mixed.bind(
        ImportField::AccountDescription,
        Column::Delimited { column: 2 },
    );
    assert!(mixed.resolved_mode().is_err());
    assert_eq!(mixed.refusals()[0].kind, IssueKind::NoFields);

    // zero delimited column.
    let mut zero_col = ColumnMap {
        mode: Some(LayoutMode::Delimited),
        ..Default::default()
    };
    zero_col.bind(ImportField::AccountNumber, Column::Delimited { column: 0 });
    assert!(zero_col
        .refusals()
        .iter()
        .any(|i| i.kind == IssueKind::ZeroBase));
}

#[test]
fn overlapping_bindings_refuse() {
    let mut map = ColumnMap {
        mode: Some(LayoutMode::Fixed),
        ..Default::default()
    };
    map.bind(ImportField::AccountNumber, Column::Fixed { start: 1, end: 8 });
    map.bind(
        ImportField::AccountDescription,
        Column::Fixed { start: 8, end: 20 },
    );
    let refusals = map.refusals();
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].kind, IssueKind::Overlapping);
    assert!(refusals[0].detail.contains("account description"));
}

// ---- 2. layout storage ----------------------------------------------------

#[test]
fn layout_file_round_trips_bytes() {
    for text in [
        cover_layout_text(),
        "[FIXED]\r\ndbfields_account number=1,8,0,-1\r\n".to_string(),
        "[COMMON]\n\n[DELM]\neditlist_account number=1\n".to_string(),
        "; a comment\r\n[DELM]\r\neditlist_account number=1\r\n".to_string(),
        String::new(),
    ] {
        let f = LayoutFile::parse(&text, None).unwrap();
        assert_eq!(f.emit(), text, "no-op save must be byte-identical");
        assert_eq!(f.save_text(), text);
    }
}

#[test]
fn layout_file_mutation_keeps_the_rest_verbatim() {
    let text = "\
; my layout\r\n\
[DELM]\r\n\
editlist_account number=1\r\n\
editlist_account description=2\r\n\
"
    .to_string();
    let mut f = LayoutFile::parse(&text, Some(LayoutMode::Delimited)).unwrap();
    f.set("delm", "editlist_account description", "5").unwrap();
    let out = f.emit();
    assert!(out.starts_with("; my layout\r\n[DELM]\r\n"));
    assert!(out.contains("editlist_account number=1\r\n"));
    assert!(out.contains("editlist_account description=5\r\n"));
    assert_eq!(
        f.map().binding(&ImportField::AccountDescription).unwrap().column,
        Column::Delimited { column: 5 }
    );
    // Appending a new key lands inside the existing section.
    f.set("DELM", "editlist_Current Year Balance db", "9").unwrap();
    assert_eq!(
        f.map()
            .balances
            .iter()
            .find(|b| b.kind == BalanceKind::CurrentYear)
            .and_then(|b| b.debit),
        Some(9)
    );
    // Section/key names are case-insensitive.
    f.set("common", "skipheaderlines", "2").unwrap();
    assert_eq!(f.map().options.skip_header_lines, 2);
}

#[test]
fn programmatic_map_emits_the_f52_cover_shape() {
    let map = cover_layout();
    let ini = map.to_ini().unwrap();
    assert!(ini.starts_with("[COMMON]\r\n"), "{ini}");
    assert!(ini.contains("multiple_specific=1\r\n"));
    assert!(ini.contains("editlist_account number=1\r\n"));
    assert!(ini.contains("editlist_Current Year Balance db=3\r\n"));
    assert!(ini.contains("delm.delm_2=44\r\n"));
    // ...and re-reads to the same map.
    let again = LayoutFile::parse(&ini, Some(LayoutMode::Delimited)).unwrap();
    assert_eq!(again.map(), &map);
    assert!(again.map().refusals().is_empty());

    // The fixed form is the bare `[FIXED]` cover the green run proved.
    let fixed_ini = fixed_layout().to_ini().unwrap();
    assert!(fixed_ini.starts_with("[FIXED]\r\n"));
    assert!(fixed_ini.contains("dbfields_account number=1,8,0,-1\r\n"));
    assert_eq!(
        LayoutFile::parse(&fixed_ini, Some(LayoutMode::Fixed))
            .unwrap()
            .map()
            .fields
            .len(),
        fixed_layout().fields.len()
    );
}

#[test]
fn layout_extension_selects_the_mode() {
    for (ext, mode) in [
        (".vtb", LayoutMode::Delimited),
        (".vgl", LayoutMode::Delimited),
        (".faj", LayoutMode::Fixed),
        (".ftb", LayoutMode::Fixed),
        (".crd", LayoutMode::Fixed),
    ] {
        assert_eq!(LayoutMode::from_extension(ext), Some(mode), "{ext}");
        assert_eq!(mode.label() == "fixed-length", mode == LayoutMode::Fixed);
    }
    assert_eq!(LayoutMode::from_extension(".csv"), None);
    assert_eq!(LayoutMode::from_extension(".xlsx"), None);
}

#[test]
fn preset_record_re_encodes_byte_exactly() {
    // A Dynacom-GL-shaped record: f64 9.0 default, 5 rows on the 5/15/25 grid.
    let mut rows = Vec::new();
    for (i, offset) in [5u32, 15, 25, 35, 45].iter().enumerate() {
        rows.push(PresetRow { a: 0, b: 1 + i as u32, c: *offset, d: 0 });
    }
    let preset = Preset {
        name: "DYNACOM GL\0\0\0\0\0\0\0\0\0\0\0\0\0\0".to_string(),
        product: "DYNACOM\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0".to_string(),
        description: "General Ledger\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0".to_string(),
        pad: vec![0u8; preset_layout::DEFAULT - preset_layout::PAD],
        default: 9.0,
        header: [1, 0, 0],
        rows,
    };
    let bytes = preset.encode();
    assert_eq!(bytes.len(), preset_layout::PROLOGUE + 5 * preset_layout::ROW_LEN);
    assert_eq!(bytes.len(), 0x160 + 120);
    let (back, next) = Preset::parse_at(&bytes, 0).unwrap();
    assert_eq!(next, bytes.len());
    assert_eq!(back.encode(), bytes);
    assert_eq!(back.default, 9.0);
    assert_eq!(back.product_name(), "DYNACOM");
    assert_eq!(back.layout_name(), "DYNACOM GL");
    // `c` ascends by 10 across the rows — the fixed-length signature.
    let offsets: Vec<u32> = back.rows.iter().map(|r| r.c).collect();
    assert_eq!(offsets, [5, 15, 25, 35, 45]);

    // The body writer drops only the product prologue (spec §4b.3).
    let body = back.encode_body();
    assert_eq!(body.len(), bytes.len() - preset_layout::HEADER);
    assert_eq!(&body[body.len() - 120..], &bytes[bytes.len() - 120..]);

    // A two-record store round-trips and counts unique layouts.
    let mut store = PresetStore::parse(&bytes).unwrap();
    assert_eq!(store.presets.len(), 1);
    store.presets.push(back.clone());
    assert_eq!(store.unique_layouts(), 1);
    assert_eq!(PresetStore::parse(&store.encode()).unwrap(), store);
    assert!(store.find("dynacom gl.imp").is_some());
    assert_eq!(store.find("ac10"), None);

    // A truncated count is refused, not guessed.
    let mut short = bytes.clone();
    short[preset_layout::COUNT..preset_layout::COUNT + 4]
        .copy_from_slice(&9u32.to_le_bytes());
    assert!(Preset::parse_at(&short, 0).is_err());
}

// ---- 3/4. write-back ------------------------------------------------------

#[test]
fn round_trip_lands_am_rows_and_bl_pairs() {
    let mut am = am();
    let mut bl = bl();
    let am_before = am.encode();
    let bl_before = bl.encode();
    let live_before = live_bl(&bl);
    let bl_count_before = bl.count();
    let map = cover_layout();
    let data = read_ascii(&map, SOURCE_CSV.as_bytes()).unwrap();

    // -- preview must not touch a byte.
    let plan = preview(&am, &bl, &data, &opts()).unwrap();
    assert_eq!(am.encode(), am_before);
    assert_eq!(bl.encode(), bl_before);
    assert_eq!(plan.accounts_in_file, 3);
    assert_eq!(plan.inserts.len(), 2, "90001/90002 are new; 1001 exists");
    assert_eq!(plan.inserts[0].account, "90001");
    assert_eq!(plan.inserts[0].line_no, 11_400);
    assert_eq!(plan.inserts[1].line_no, 11_400 + AM_LINE_STEP);
    assert_eq!(plan.kept, ["1001"]);
    assert!(plan.updates.is_empty(), "overwrite is off by default");
    assert_eq!(plan.zero_balance, ["1001"]);
    assert_eq!(plan.bl_pairs.len(), 2, "zero balance claims no BL row");
    assert_eq!(plan.totals.net, Cents::ZERO, "the TB balances");
    assert_eq!(plan.totals.debits, Cents(125_000));
    assert_eq!(plan.totals.credits, Cents(-125_000));

    // -- apply.
    let report = apply(&mut am, &mut bl, &plan, &opts()).unwrap();
    assert_eq!(report.inserted, 2);
    assert_eq!(report.updated, 0);
    assert_eq!(report.bl_appended, 4, "two fresh O/X pairs");
    assert_eq!(report.bl_rewritten, 0);
    assert_eq!(report.bl_skipped, 1);
    assert_eq!(am.count(), 325 + 2);
    assert_eq!(bl.count(), bl_count_before + 4);
    assert_eq!(live_bl(&bl), live_before + 4);

    // -- AM cell-level outcome (F52a: new accounts land, defaults are CW's).
    let chart = Chart::from_table(&am).unwrap();
    assert!(chart.issues().is_empty(), "{:?}", chart.issues());
    for (no, desc, sign, line) in [
        ("90001", "Imported Bank Account", "D", 11_400),
        ("90002", "Imported Payable", "D", 11_432),
    ] {
        let acct = account(&chart, no).unwrap_or_else(|| panic!("{no} landed"));
        assert_eq!(acct.desc, desc);
        assert_eq!(acct.kind, cw_app::accounts::AccountKind::BalanceSheet);
        assert_eq!(acct.props.post, cw_app::accounts::Flag::yes());
        assert_eq!(acct.props.sign.as_str(), sign);
        assert_eq!(acct.line_no, line);
        assert_eq!(
            acct.ac_no.len(),
            40,
            "AC_NO is stored right-justified in the corpus field width"
        );
        assert_eq!(acct.cwguid.len(), 22, "fresh 22-char CWGUID");
        assert_eq!(acct.cwdate, STAMP);
        assert_eq!(acct.createdon, STAMP);
        // Everything the corpus leaves blank on a chart row stays blank.
        for (col, got) in [
            ("CLASS", acct.class.as_str()),
            ("DC", acct.props.dc.as_str()),
            ("COLUMN", acct.props.column.raw()),
            ("BALSTAT", acct.balstat.as_str()),
            ("ENTITY", acct.entity.as_str()),
            ("GIFI", acct.gifi.as_str()),
            ("SCHEDULE", acct.schedule.as_str()),
        ] {
            assert!(got.is_empty(), "{no}.{col} must stay blank, got {got:?}");
        }
        assert_eq!(acct.open, 0.0, "AM balances stay the dead cache (F35)");
        assert_eq!(acct.cur_ytd, 0.0);
    }
    // -- BL cell-level outcome (F52b: year-0 O/X pair per imported account).
    for (no, amount) in [("90001", "1250.00"), ("90002", "-1250.00")] {
        let opens = bl_rows(&bl, no, "0", "O", BL_OPEN_ID, "A");
        let comps = bl_rows(&bl, no, "0", "O", BL_COMPUTED_ID, "A");
        assert_eq!(opens.len(), 1, "{no} O row");
        assert_eq!(comps.len(), 1, "{no} X row");
        let o = opens[0];
        let x = comps[0];
        assert!(o < x, "{no}: the O row precedes its X twin");
        // O carries AMT only; every fill cell is blank (cube L2).
        assert_eq!(cell(&bl, o, "AMT"), amount);
        for col in ["AMT_YTD", "AMT_DIST", "FX", "FX_YTD", "FX_DIST"] {
            assert_eq!(cell(&bl, o, col), "", "{no}.O {col}");
        }
        // X mirrors AMT and fills YTD/FX; DIST normalizes to 0.00.
        assert_eq!(cell(&bl, x, "AMT"), amount);
        assert_eq!(cell(&bl, x, "AMT_YTD"), amount);
        assert_eq!(cell(&bl, x, "FX"), amount);
        assert_eq!(cell(&bl, x, "FX_YTD"), amount);
        assert_eq!(cell(&bl, x, "AMT_DIST"), "0.00");
        assert_eq!(cell(&bl, x, "FX_DIST"), "0.00");
        // Axis anatomy of a claimed pair.
        for r in [o, x] {
            assert_eq!(cell(&bl, r, "YEAR"), "0");
            assert_eq!(cell(&bl, r, "BUCKET"), "O");
            assert_eq!(cell(&bl, r, "GROUP"), "A");
            assert_eq!(cell(&bl, r, "ENTITY"), "");
            assert_eq!(cell(&bl, r, "TYPE"), "", "O rows are TYPE='' (cube §2)");
            assert_eq!(cell(&bl, r, "PERIOD"), "0");
            assert_eq!(cell(&bl, r, "BOOKED"), "");
            assert_eq!(cell(&bl, r, "CUSTOM"), "");
            assert_eq!(cell(&bl, r, "CWDATE"), STAMP);
            assert_eq!(cell(&bl, r, "CWGUID").len(), 22);
        }
        // Physical form: the `N19.2` amounts are right-justified and the 2-wide
        // `PERIOD` cell is right-justified, exactly as the corpus stores them.
        assert_eq!(bl.cell(o, "AMT").unwrap().len(), 19);
        assert_eq!(bl.cell(o, "AMT").unwrap(), format!("{:>19}", amount).as_bytes());
        assert_eq!(bl.cell(o, "PERIOD").unwrap(), b" 0");
        assert_eq!(bl.cell(x, "AMT_DIST").unwrap().len(), 19);
        assert_eq!(bl.cell(x, "AMT_DIST").unwrap(), b"               0.00");

        // The pair is balanced through the kernel's own L5 law.
        assert_eq!(Cube::from_table(&bl).unwrap().balance(no, 0, "A"), Some(
            amount.parse::<f64>().unwrap()
        ));
    }
    // The zero-balance account claimed nothing.
    assert!(bl_rows(&bl, "1001", "0", "O", BL_OPEN_ID, "A")
        .iter()
        .all(|r| cell(&bl, *r, "CWDATE") != STAMP));

    // -- idempotence: the same import again changes no byte.
    let am_after = am.encode();
    let bl_after = bl.encode();
    let data2 = read_ascii(&map, SOURCE_CSV.as_bytes()).unwrap();
    let plan2 = preview(&am, &bl, &data2, &opts()).unwrap();
    assert!(plan2.inserts.is_empty(), "both accounts already exist");
    assert_eq!(plan2.bl_pairs.len(), 2);
    let report2 = apply(&mut am, &mut bl, &plan2, &opts()).unwrap();
    assert_eq!(report2.inserted, 0);
    assert_eq!(report2.bl_rewritten, 2, "both pairs rewritten in place");
    assert_eq!(report2.bl_appended, 0);
    assert_eq!(am.encode(), am_after, "second run must be byte-identical");
    assert_eq!(bl.encode(), bl_after, "second run must be byte-identical");
}

#[test]
fn existing_pairs_are_rewritten_in_place_not_appended_in_new_entity() {
    // 1570-00 already owns a year-0 O/X pair in the corpus (AMT 7304745.00).
    let mut am = am();
    let mut bl = bl();
    let count_before = bl.count();
    let live_before = live_bl(&bl);
    let source = "acct,desc,balance\n1570-00,Machinery and equipment,100.00\n";
    let map = cover_layout();
    let data = read_ascii(&map, source.as_bytes()).unwrap();
    let o_before = bl_rows(&bl, "1570-00", "0", "O", BL_OPEN_ID, "A")[0];
    let x_before = bl_rows(&bl, "1570-00", "0", "O", BL_COMPUTED_ID, "A")[0];
    let guid_o = cell(&bl, o_before, "CWGUID");
    let guid_x = cell(&bl, x_before, "CWGUID");
    let created_o = cell(&bl, o_before, "CREATEDON");
    let plan = preview(&am, &bl, &data, &opts()).unwrap();
    assert_eq!(plan.inserts.len(), 0, "1570-00 is an existing chart account");
    assert_eq!(plan.kept, ["1570-00"]);
    assert_eq!(plan.bl_pairs.len(), 1);
    assert!(plan.bl_pairs[0].open_row.is_some());
    assert!(plan.bl_pairs[0].computed_row.is_some());

    let report = apply(&mut am, &mut bl, &plan, &opts()).unwrap();
    assert_eq!(report.inserted, 0);
    assert_eq!(report.bl_rewritten, 1);
    assert_eq!(report.bl_appended, 0);
    assert_eq!(bl.count(), count_before, "pair churn, not growth");
    assert_eq!(live_bl(&bl), live_before);

    // The corpus pair now carries the imported value in the exact fill law.
    let o = bl_rows(&bl, "1570-00", "0", "O", BL_OPEN_ID, "A")[0];
    let x = bl_rows(&bl, "1570-00", "0", "O", BL_COMPUTED_ID, "A")[0];
    assert_eq!(cell(&bl, o, "AMT"), "100.00");
    assert_eq!(cell(&bl, o, "AMT_YTD"), "");
    assert_eq!(cell(&bl, x, "AMT"), "100.00");
    assert_eq!(cell(&bl, x, "AMT_YTD"), "100.00");
    assert_eq!(cell(&bl, x, "AMT_DIST"), "0.00");
    // Identity cells of the existing rows are preserved (F52a churns values,
    // never keys).
    assert_eq!(o, o_before, "the same physical row is rewritten");
    assert_eq!(x, x_before);
    assert_eq!(cell(&bl, o, "CWGUID"), guid_o);
    assert_eq!(cell(&bl, x, "CWGUID"), guid_x);
    assert_eq!(cell(&bl, o, "CREATEDON"), created_o);

    // ...while the year-1 layer (an untouched prior year) keeps its own value.
    let y1 = bl_rows(&bl, "1570-00", "1", "O", BL_COMPUTED_ID, "A");
    assert_eq!(y1.len(), 1);
    assert_ne!(cell(&bl, y1[0], "AMT"), "100.00");
}

#[test]
fn existing_accounts_stay_untouched_unless_overwrite_is_set() {
    let am = am();
    let mut bl = bl();
    let row = account(&Chart::from_table(&am).unwrap(), "1001").unwrap().row;
    let desc_before = cell(&am, row, "AC_DESC");
    let guid_before = cell(&am, row, "CWGUID");
    let source = "acct,desc,balance\n1001,Replaced Description,0.00\n";
    let map = cover_layout();
    let data = read_ascii(&map, source.as_bytes()).unwrap();

    // Default: untouched.
    let plan = preview(&am, &bl, &data, &opts()).unwrap();
    assert!(plan.updates.is_empty());
    assert_eq!(plan.kept, ["1001"]);
    let report = apply(&mut am.clone(), &mut bl, &plan, &opts()).unwrap();
    assert_eq!(report.updated, 0);
    assert_eq!(cell(&am, row, "AC_DESC"), desc_before);
    assert_eq!(cell(&am, row, "CWGUID"), guid_before);

    // AVOverwriteAccDescription=1: AC_DESC (and only that) is replaced.
    let mut ow = opts();
    ow.set(Setting::AvOverwriteAccDescription, SettingValue::Bool(true));
    let plan = preview(&am, &bl, &data, &ow).unwrap();
    assert_eq!(plan.updates.len(), 1);
    assert_eq!(plan.updates[0].old_description, desc_before);
    assert_eq!(plan.updates[0].new_description, "Replaced Description");
    let mut am2 = am.clone();
    let mut bl2 = bl;
    apply(&mut am2, &mut bl2, &plan, &ow).unwrap();
    assert_eq!(cell(&am2, row, "AC_DESC"), "Replaced Description");
    assert_eq!(cell(&am2, row, "CWGUID"), guid_before, "identity preserved");
    assert_eq!(cell(&am2, row, "LINENO"), cell(&am, row, "LINENO"));
    assert_eq!(cell(&am2, row, "CWDATE"), STAMP);
}

#[test]
fn mapped_field_columns_land_in_their_am_cells() {
    let mut am = am();
    let mut bl = bl();
    let mut map = cover_layout();
    map.bind(ImportField::Leadsheet, Column::Delimited { column: 4 });
    map.bind(ImportField::TaxIdent, Column::Delimited { column: 5 });
    map.bind(ImportField::StatementType, Column::Delimited { column: 6 });
    let source = "\
acct,desc,balance,leadsheet,tax,type
90010,Mapped Row,10.00,4000.01,T1,I
";
    let data = read_ascii(&map, source.as_bytes()).unwrap();
    let plan = preview(&am, &bl, &data, &opts()).unwrap();
    assert_eq!(plan.inserts.len(), 1);
    assert_eq!(plan.inserts[0].kind, 'I', "the file's statement type wins");
    assert_eq!(plan.inserts[0].leadsheet, "4000.01");
    assert_eq!(plan.inserts[0].tax_ident, "T1");
    apply(&mut am, &mut bl, &plan, &opts()).unwrap();
    let chart = Chart::from_table(&am).unwrap();
    let acct = account(&chart, "90010").unwrap();
    assert_eq!(acct.kind, cw_app::accounts::AccountKind::Income);
    assert_eq!(acct.schedule, "4000.01");
    assert_eq!(acct.tax_id, "T1");
    assert_eq!(acct.props.sign.as_str(), "D", "default sign");
}

#[test]
fn fixed_layout_import_matches_the_delimited_outcome() {
    let mut am_d = am();
    let mut bl_d = bl();
    let plan_d = preview(
        &am_d,
        &bl_d,
        &read_ascii(&cover_layout(), SOURCE_CSV.as_bytes()).unwrap(),
        &opts(),
    )
    .unwrap();
    apply(&mut am_d, &mut bl_d, &plan_d, &opts()).unwrap();

    // The fixed file carries no balance column, so only AM grows.
    let mut am_f = am();
    let mut bl_f = bl();
    let data_f = read_ascii(&fixed_layout(), fixed_source().as_bytes()).unwrap();
    assert_eq!(data_f.rows.len(), 2);
    let plan_f = preview(&am_f, &bl_f, &data_f, &opts()).unwrap();
    assert_eq!(plan_f.inserts.len(), 2);
    assert_eq!(plan_f.bl_pairs.len(), 0, "no balance binding, no BL rows");
    let report = apply(&mut am_f, &mut bl_f, &plan_f, &opts()).unwrap();
    assert_eq!(report.bl_appended, 0);
    assert_eq!(
        report.bl_skipped, 2,
        "no balance is bound, so neither account claims a BL row"
    );
    assert_eq!(bl_f.encode(), bl().encode(), "BL untouched without balances");

    // The AM rows agree cell-for-cell with the delimited run.
    for no in ["90001", "90002"] {
        let a = Chart::from_table(&am_d).unwrap();
        let b = Chart::from_table(&am_f).unwrap();
        let x = account(&a, no).unwrap();
        let y = account(&b, no).unwrap();
        assert_eq!(x.line_no, y.line_no);
        assert_eq!(x.desc, y.desc);
        assert_eq!(x.kind, y.kind);
        assert_eq!(x.cwguid, y.cwguid, "same seed, same allocator stream");
        // The fixed file's normal-sign column is landed.
        assert_eq!(
            y.props.sign.as_str(),
            if no == "90001" { "D" } else { "C" }
        );
    }
}

#[test]
fn grid_reader_shares_the_delimited_law() {
    let map = cover_layout();
    let grid: Vec<Vec<String>> = [["acct", "desc", "balance"]]
        .iter()
        .map(|r| r.iter().map(|s| s.to_string()).collect())
        .chain(std::iter::once(vec![
            "90001".into(),
            "Imported Bank Account".into(),
            "1250.00".into(),
        ]))
        .collect();
    let data = read_grid(&map, &grid).unwrap();
    assert_eq!(data.rows.len(), 1, "the header row has a non-numeric balance");
    assert_eq!(data.rows[0].account_number(), "90001");
    assert_eq!(
        data.rows[0].balance(BalanceKind::CurrentYear),
        Some(Cents(125_000))
    );
}

// ---- 5. refusals ----------------------------------------------------------

#[test]
fn layout_mismatch_refuses_and_names_the_binding() {
    let map = fixed_layout();
    // A 20-byte line cannot satisfy the 1..=32 description range.
    let err = read_ascii(&map, b"90001      short\n").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("layout mismatch"), "{msg}");
    assert!(msg.contains("account description"), "{msg}");
    assert!(msg.contains("1-based inclusive"), "{msg}");

    // A delimited line missing a bound column refuses as well.
    let map = cover_layout();
    let err = read_ascii(&map, b"acct,desc,balance\n90001,only-desc\n").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("column 3"), "{msg}");
    assert!(msg.contains("line 2"), "{msg}");

    // The lenient scanner keeps the readable rows and types each failure, so a
    // single bad trailer cannot hide the rest of a file.
    let src = "acct,desc,balance\n90001,good,1.00\n90002,bad,zzz\n90003,short\n";
    let data = read_ascii_report(&map, src.as_bytes()).unwrap();
    assert_eq!(data.rows.len(), 1);
    assert_eq!(data.rows[0].account_number(), "90001");
    let kinds: Vec<IssueKind> = data.refusals().iter().map(|i| i.kind).collect();
    assert_eq!(kinds, [IssueKind::BadAmount, IssueKind::ColumnMissing]);
    assert_eq!(data.refusals()[0].line, Some(3));
    assert_eq!(data.refusals()[1].line, Some(4));
    assert_eq!(
        data.refusals()[0].field.as_ref().map(|f| f.key_label()),
        Some("Current Year Balance".to_string())
    );

    // A short fixed line is the `LineTooShort` case.
    let short = read_ascii_report(&fixed_layout(), b"90001      short\n").unwrap();
    assert!(short.rows.is_empty());
    assert_eq!(short.refusals().len(), 1);
    assert_eq!(short.refusals()[0].kind, IssueKind::LineTooShort);
    assert_eq!(short.refusals()[0].line, Some(1));
}

#[test]
fn bad_amounts_refuse() {
    let map = cover_layout();
    for bad in ["(123.00)", "USD 12", "12.3.4", "1 234.00", "--1.00"] {
        let src = format!("acct,desc,balance\n90001,x,{bad}\n");
        let err = read_ascii(&map, src.as_bytes()).unwrap_err();
        assert!(
            err.to_string().contains("plain decimal"),
            "{bad}: {err}"
        );
    }
}

#[test]
fn over_wide_and_non_latin1_rows_refuse_before_any_write() {
    let am = am();
    let bl = bl();
    let am_before = am.encode();
    let bl_before = bl.encode();
    let map = cover_layout();

    // Account wider than AM.AC_NO (40).
    let long_no = "9".repeat(41);
    let src = format!("acct,desc,balance\n{long_no},x,1.00\n");
    let data = read_ascii(&map, src.as_bytes()).unwrap();
    let err = preview(&am, &bl, &data, &opts()).unwrap_err();
    assert!(err.to_string().contains("AM.AC_NO"), "{err}");

    // Description wider than AM.AC_DESC (50).
    let long_desc = "d".repeat(AM_DESC_WIDTH + 1);
    let src = format!("acct,desc,balance\n90001,{long_desc},1.00\n");
    let data = read_ascii(&map, src.as_bytes()).unwrap();
    let err = preview(&am, &bl, &data, &opts()).unwrap_err();
    assert!(err.to_string().contains("AM.AC_DESC"), "{err}");

    assert_eq!(am.encode(), am_before, "a refused preview writes nothing");
    assert_eq!(bl.encode(), bl_before);
}

#[test]
fn non_tb_components_refuse() {
    let am = am();
    let bl = bl();
    let data = read_ascii(&cover_layout(), SOURCE_CSV.as_bytes()).unwrap();
    for (component, needle) in [
        (ImportComponents::GeneralLedger, "general ledger"),
        (ImportComponents::AdjustingJournalEntry, "adjusting journal"),
    ] {
        let o = ImportOptions { components: component, ..opts() };
        let err = preview(&am, &bl, &data, &o).unwrap_err();
        assert!(err.to_string().contains(needle), "{err}");
        assert!(err.to_string().contains("not implemented"), "{err}");
    }
    assert_eq!(ImportComponents::parse(0).unwrap(), ImportComponents::TrialBalance);
    assert!(ImportComponents::parse(3).is_err());
}

#[test]
fn com_surface_domains_match_the_typelib() {
    // Package ordinals: 1..=4 legal, 0/5 refused.
    assert_eq!(ImportPackage::parse(1).unwrap(), ImportPackage::AccountView);
    assert_eq!(ImportPackage::parse(2).unwrap(), ImportPackage::Ascii);
    assert_eq!(ImportPackage::parse(3).unwrap(), ImportPackage::ProSysEngagement);
    assert_eq!(ImportPackage::parse(4).unwrap(), ImportPackage::Xaf);
    for bad in [0, 5, 6, -1] {
        assert!(ImportPackage::parse(bad).is_err(), "{bad}");
    }
    assert_eq!(ImportPackage::Ascii.code(), 2);

    // Settings vector: exactly 0..=19.
    assert_eq!(Setting::ALL.len(), 20);
    assert!(Setting::parse(19).is_ok());
    assert!(Setting::parse(20).is_err());
    let mut o = opts();
    assert_eq!(o.get(Setting::AvPriorYearData), None, "unset reads None");
    o.set(Setting::AvPriorYearData, SettingValue::Bool(true));
    assert_eq!(
        o.get(Setting::AvPriorYearData),
        Some(&SettingValue::Bool(true))
    );
    assert!(o.notes().iter().any(|n| n.contains("AVPriorYearData")));
    // A text VARIANT is truthy the same way CW coerces it.
    o.set(
        Setting::AvNonfinancialData,
        SettingValue::Text("yes".into()),
    );
    assert!(o.nonfinancial_data());
}

// ---- documented residuals, asserted so they cannot silently change ---------

#[test]
fn only_current_year_balance_lands_and_others_are_reported() {
    let mut map = cover_layout();
    map.balances.push(cw_app::import::BalanceBinding {
        kind: BalanceKind::Prior(YearOffset::Y1),
        debit: Some(7),
        credit: None,
        fx: None,
    });
    let src = "acct,desc,balance,leadsheet,tax,type,py1\n90001,x,10.00,,,,10.00\n";
    let data = read_ascii(&map, src.as_bytes()).unwrap();
    assert_eq!(
        data.rows[0].balance(BalanceKind::Prior(YearOffset::Y1)),
        Some(Cents(1_000))
    );
    let warn = data
        .issues
        .iter()
        .find(|i| i.kind == IssueKind::UnsupportedBalanceKind)
        .expect("prior-year binding is warned about");
    assert_eq!(warn.severity, Severity::Warn);
    assert!(warn.detail.contains("Prior Year 1 Balance"));

    // Only the current-year value reaches BL.
    let am = am();
    let bl = bl();
    let plan = preview(&am, &bl, &data, &opts()).unwrap();
    assert_eq!(plan.bl_pairs.len(), 1, "one pair, for the current year only");
    assert_eq!(plan.bl_pairs[0].cents, Cents(1_000));
}

#[test]
fn zero_based_legacy_layout_reads_nothing_and_says_so() {
    // The exact pre-F52 mistake: a working-looking map that registers nothing.
    let mut legacy = ColumnMap {
        mode: Some(LayoutMode::Fixed),
        ..Default::default()
    };
    legacy.bind(ImportField::AccountNumber, Column::Fixed { start: 0, end: 7 });
    legacy.bind(
        ImportField::AccountDescription,
        Column::Fixed { start: 8, end: 31 },
    );
    let err = read_ascii(&legacy, fixed_source().as_bytes()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("layout refused"), "{msg}");
    assert!(msg.contains("1-based"), "{msg}");
}

#[test]
fn nozero_and_skip_lines_are_honoured() {
    let mut map = cover_layout();
    map.options.no_zero = true;
    let src = "acct,desc,balance\n90001,a,1.00\n90002,b,0.00\n90003,c,-1.00\n90004,d,,\n";
    let data = read_ascii(&map, src.as_bytes()).unwrap();
    assert_eq!(
        data.rows
            .iter()
            .map(|r| r.account_number())
            .collect::<Vec<_>>(),
        ["90001", "90003"],
        "nozero drops the zero and the unbound row"
    );

    let mut skip = cover_layout();
    skip.options.skip_footer_lines = 1;
    let src = "acct,desc,balance\n90001,a,1.00\n90002,b,2.00\nfooter\n";
    let data = read_ascii(&skip, src.as_bytes()).unwrap();
    assert_eq!(data.source_lines, 4);
    assert_eq!(data.skipped_header, 1);
    assert_eq!(data.skipped_footer, 1);
    assert_eq!(
        data.rows
            .iter()
            .map(|r| r.account_number())
            .collect::<Vec<_>>(),
        ["90001", "90002"],
        "the footer line is never read as a record"
    );
}

#[test]
fn duplicate_accounts_collapse_with_the_last_line_winning() {
    let map = cover_layout();
    let src = "acct,desc,balance\n90001,first,1.00\n90001,second,2.00\n";
    let data = read_ascii(&map, src.as_bytes()).unwrap();
    assert_eq!(data.rows.len(), 1);
    assert_eq!(data.rows[0].description(), "second");
    assert_eq!(
        data.rows[0].balance(BalanceKind::CurrentYear),
        Some(Cents(200))
    );
    let issue = data
        .issues
        .iter()
        .find(|i| i.kind == IssueKind::DuplicateAccount)
        .expect("duplicate reported");
    assert_eq!(issue.line, Some(3));
}

#[test]
fn options_default_block_is_empty_and_extra_keys_survive() {
    // §8.3 keys that are not modelled land in `extra`/`profile`, never dropped.
    let text = "\
[COMMON]\r\n\
exchangerateperioddatesequence=Y\r\n\
accdivsub.profiled=2\r\n\
cleargl=1\r\n\
acctype=B\r\n\
unknown_new_key=42\r\n\
";
    let f = LayoutFile::parse(text, Some(LayoutMode::Delimited)).unwrap();
    let m = f.map();
    assert_eq!(m.options.acctype, Some('B'));
    assert!(m.options.clear.contains("gl"));
    assert_eq!(
        m.options
            .profile
            .get("exchangerateperioddatesequence")
            .map(|s| s.as_str()),
        Some("Y")
    );
    assert_eq!(
        m.options.extra.get("unknown_new_key").map(|s| s.as_str()),
        Some("42")
    );
    assert_eq!(m.options.delimiter_string().len(), 10);
    assert_eq!(LayoutOptions::default().delimiter_string(), "\0".repeat(10));
    assert!(LayoutOptions::default().delimiter().is_err());
}

#[test]
fn noop_preview_and_rebuild_close_the_public_surface() {
    // `ImportPreview::is_noop` is the caller's cheap "do not even open the
    // transaction" gate.
    let am = am();
    let bl = bl();
    let map = cover_layout();
    let empty = read_ascii(&map, b"acct,desc,balance\n").unwrap();
    let plan = preview(&am, &bl, &empty, &opts()).unwrap();
    assert!(plan.is_noop(), "nothing in the file, nothing to do");
    assert!(plan.inserts.is_empty() && plan.bl_pairs.is_empty());

    // Any new account or claimed value makes it false.
    let plan2 = preview(
        &am,
        &bl,
        &read_ascii(&map, SOURCE_CSV.as_bytes()).unwrap(),
        &opts(),
    )
    .unwrap();
    assert!(!plan2.is_noop());

    // `LayoutFile::rebuild` re-emits from the map, so a mutated map reaches disk
    // in the minimal cover form and still parses back identically.
    let mut f = LayoutFile::parse(&cover_layout_text(), Some(LayoutMode::Delimited)).unwrap();
    f.map_mut()
        .bind(ImportField::Leadsheet, Column::Delimited { column: 4 });
    f.rebuild().unwrap();
    let text = f.save_text();
    assert!(text.contains("editlist_leadsheet=4\r\n"), "{text}");
    assert_eq!(
        LayoutFile::parse(&text, Some(LayoutMode::Delimited))
            .unwrap()
            .map(),
        f.map()
    );
    // Mutating the map alone leaves the document untouched (`rebuild` commits).
    let before = f.save_text();
    f.map_mut().options.skip_header_lines = 3;
    assert_eq!(f.save_text(), before);

    // Presentational helpers on the small value types.
    assert_eq!(ImportPackage::Ascii.label(), "ASCII");
    assert_eq!(ImportComponents::parse(2).unwrap().label(), "adjusting journal entries");
    assert_eq!(YearOffset::Y3.year(), 3);
    assert_eq!(YearOffset::parse(5), None);
    assert!(!SettingValue::Text("no".into()).truthy());
    assert_eq!(Cents(125_000).to_f64(), 1250.0);
    assert_eq!(Cents::ZERO.to_string(), "0.00");
    assert_eq!(Column::Delimited { column: 3 }.key_value(), "3");
    assert_eq!(Severity::Warn.label(), "warn");
    assert_eq!(IssueKind::ZeroBase.label(), "zero-based binding");
    let issue = ImportIssue {
        severity: Severity::Refuse,
        line: Some(7),
        field: None,
        kind: IssueKind::Schema,
        detail: "boom".into(),
    };
    assert_eq!(issue.to_string(), "refuse: boom (line 7)");
}

#[test]
fn fixed_fourteen_int_balance_rows_are_carried_not_landed() {
    // §8.5 residual: the fixed balance row is a 14-int sscanf form whose slot
    // semantics were never decoded, so it parses, round-trips and lands nothing.
    let text = "[FIXED]\r\n\
dbfields_account number=1,8,0,-1\r\n\
dbfields_Current Year Balance=1,8,9,32,0,0,0,0,0,0,0,0,0,0\r\n";
    let f = LayoutFile::parse(text, Some(LayoutMode::Fixed)).unwrap();
    let map = f.map();
    assert_eq!(map.fixed_balances.len(), 1);
    let row = &map.fixed_balances[0];
    assert_eq!(row.kind, BalanceKind::CurrentYear);
    assert_eq!(row.ints[0], 1);
    assert_eq!(row.ints[3], 32);
    assert_eq!(row.ints[13], 0);
    // Carried verbatim: 14 slots, the shipped `-1` default for the rest.
    assert_eq!(
        row.key_value(),
        "1,8,9,32,0,0,0,0,0,0,0,0,0,0"
    );
    // Warned, not refused — it is a shape we tolerate, not one we can land.
    let issues = map.validate();
    assert!(issues
        .iter()
        .any(|i| i.kind == IssueKind::UnsupportedBalanceKind && i.severity == Severity::Warn));
    assert!(map.refusals().is_empty(), "{:?}", map.refusals());
    assert_eq!(f.emit(), text);
    // A short row pads with `-1`, the parser's default.
    let short = LayoutFile::parse("[FIXED]\r\ndbfields_account number=1,8,0,-1\r\ndbfields_Current Year Balance=1,8\r\n", Some(LayoutMode::Fixed)).unwrap();
    assert_eq!(short.map().fixed_balances[0].ints[2..], [-1i64; 12]);

    // ...and a fixed row inside a delimited layout is a family mismatch.
    let mixed = LayoutFile::parse(
        "[DELM]\r\neditlist_account number=1\r\ndbfields_Current Year Balance=1,8\r\n",
        Some(LayoutMode::Delimited),
    )
    .unwrap();
    assert!(mixed
        .map()
        .refusals()
        .iter()
        .any(|i| i.detail.contains("dbfields_")));
}

#[test]
fn xlsx_is_documented_as_next_wave_not_stubbed() {
    let plan = cw_app::import::xlsx_plan();
    assert!(plan.contains("wave 2"));
    assert!(plan.contains("read_grid"));
    assert!(plan.contains("sharedStrings"));
}
