//! Gate: `CE` entity model + org setup (P5.H H1).
//!
//! Two truths:
//! 1. The frozen corpus `CE` layout is 33 fields / 1089-byte header /
//!    448-byte records with **zero live rows** in all three corpora.
//! 2. The model round-trips an org through real dBase bytes, validates setup
//!    errors, and reproduces the single-blank-entity law against each corpus
//!    `AM.ENTITY` dimension.

use std::path::PathBuf;

use cw_app::entities::{
    blank_ce_table, ce_schema_matches, Entity, EntityDimension, Org, OrgIssueKind, OrgSetup,
    Ownership, Percent, Year, CE_FIELD_COUNT, CE_HEADER_LEN, CE_RECORD_LEN, CE_SCHEMA,
};
use cw_core::dbf::Table;
use cw_core::guid::{CwGuid, Family, Rng};

const CORPORA: [&str; 3] = ["hull2024", "corp2026", "mwgh2025"];

fn corpus_file(tag: &str, suffix: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/dbf")
        .join(tag);
    let want = suffix.to_ascii_lowercase();
    std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {dir:?}: {e}"))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase().ends_with(&want))
                .unwrap_or(false)
        })
        .unwrap_or_else(|| panic!("no *{suffix} in {dir:?}"))
}

#[test]
fn corpus_ce_schema_is_frozen() {
    for tag in CORPORA {
        let t = Table::parse_path(corpus_file(tag, "CE.dbf"))
            .unwrap_or_else(|e| panic!("{tag} CE parse: {e}"));
        assert_eq!(t.fields.len(), CE_FIELD_COUNT, "{tag}: field count");
        assert_eq!(t.header_len as usize, CE_HEADER_LEN, "{tag}: header");
        assert_eq!(t.record_len as usize, CE_RECORD_LEN, "{tag}: record");
        assert_eq!(t.count(), 0, "{tag}: CE must be empty (normal form)");
        assert!(ce_schema_matches(&t), "{tag}: schema drift");
        for (i, f) in CE_SCHEMA.iter().enumerate() {
            assert_eq!(
                t.fields[i].name.to_ascii_uppercase(),
                f.name,
                "{tag}: column {i}"
            );
        }
        let org = Org::from_table(&t).unwrap();
        assert!(org.is_single_blank(), "{tag}: CE empty ⇒ single blank");
        assert!(org.validate().is_empty());
    }
}

#[test]
fn blank_ce_table_is_canonical_bytes() {
    let t = blank_ce_table();
    assert_eq!(t.header_len as usize, CE_HEADER_LEN);
    assert_eq!(t.record_len as usize, CE_RECORD_LEN);
    assert_eq!(t.encode().len(), CE_HEADER_LEN + 1); // header + 0x1A
    let re = Table::parse(&t.encode()).unwrap();
    assert_eq!(re.count(), 0);
    assert!(ce_schema_matches(&re));
}

#[test]
fn org_roundtrips_through_dbase_bytes() {
    let mut setup = OrgSetup::single_blank();
    let parent = setup.add_entity("PARENT", "Parent Holdings").unwrap();
    parent.ownership = Ownership::full();
    parent.abbr = "PAR".into();
    parent.include_prior = cw_app::entities::CeBool::yes();

    let sub = setup.add_entity("SUB1", "Subsidiary One").unwrap();
    sub.ownership.set(Year::Current, Some(Percent::new(80.0).unwrap()));
    sub.ownership.set(Year::Prior1, Some(Percent::new(75.5).unwrap()));
    sub.ownership.set(Year::Prior2, Some(Percent::new(100.0).unwrap()));
    sub.ownership.set(Year::Budget, Some(Percent::new(80.0).unwrap()));
    sub.ownership.set(Year::Forecast, Some(Percent::new(82.5).unwrap()));
    sub.apply_fx = cw_app::entities::CeBool::yes();
    sub.unmap_new_accounts = cw_app::entities::CeBool::yes();
    sub.division = "D1".into();
    sub.kind = "S".into();

    let (org, issues) = setup.finish();
    assert!(issues.is_empty(), "{issues:?}");

    let table = org.to_table().unwrap();
    assert_eq!(table.count(), 2);
    assert_eq!(table.encode().len(), CE_HEADER_LEN + 2 * CE_RECORD_LEN + 1);
    let back = Org::from_table(&Table::parse(&table.encode()).unwrap()).unwrap();
    assert_eq!(back, org);
    assert_eq!(back.year_percent("SUB1", Year::Current), Some(80.0));
    assert_eq!(back.year_percent("SUB1", Year::Prior1), Some(75.5));
    assert_eq!(back.year_percent("SUB1", Year::Prior4), None);
    assert_eq!(back.year_percent("SUB1", Year::Forecast), Some(82.5));
    assert!(back.entities[1].apply_fx.is_true());
    assert!(back.entities[1].unmap_new_accounts.is_true());
    assert_eq!(back.consolidation_units().len(), 2); // 80% + 100%
    assert!(back.entities[1].is_included());

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("entities-gate");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("CE.dbf");
    org.write_path(&path).unwrap();
    assert_eq!(Org::read_path(&path).unwrap(), org);
}

#[test]
fn exclusion_removes_unit_from_consolidation() {
    let mut setup = OrgSetup::single_blank();
    let a = setup.add_entity("A", "Alpha").unwrap();
    a.ownership = Ownership::full();
    let b = setup.add_entity("B", "Beta").unwrap();
    b.ownership = Ownership::full();
    b.exclude = cw_app::entities::CeBool::yes();
    let (org, _) = setup.finish();
    let units = org.consolidation_units();
    assert_eq!(units.len(), 1);
    assert_eq!(units[0].code, "A");
    assert!(org.by_code("B").is_some());
}

#[test]
fn org_validation_reports_setup_errors() {
    let mut dup = Entity::new("A", "Alpha").unwrap();
    dup.ownership.set(Year::Current, Percent::parse("140").unwrap());
    let dup2 = Entity::new("a", "").unwrap();
    let blank = Entity::new("", "Blank").unwrap();
    let org = Org::from_entities(vec![dup, dup2, blank]);
    let issues = org.validate();
    let kinds: Vec<OrgIssueKind> = issues.iter().map(|i| i.kind).collect();
    for want in [
        OrgIssueKind::DuplicateEntityCode,
        OrgIssueKind::MissingName,
        OrgIssueKind::PercentOutOfRange,
        OrgIssueKind::BlankEntityCode,
    ] {
        assert!(kinds.contains(&want), "missing {want:?} in {issues:?}");
    }
}

#[test]
fn corpus_am_is_single_blank_entity() {
    let org = Org::single_blank();
    for tag in CORPORA {
        let am = Table::parse_path(corpus_file(tag, "AM.dbf"))
            .unwrap_or_else(|e| panic!("{tag} AM: {e}"));
        let dim = EntityDimension::from_table(&am).unwrap();
        assert!(dim.is_single_blank(), "{tag}: non-blank ENTITY rows {dim:?}");
        assert!(dim.codes().next().is_none());
        assert!(dim.check(&org).is_empty(), "{tag}: dimension check");
    }
}

#[test]
fn entity_dimension_flags_unknown_codes() {
    let mut setup = OrgSetup::single_blank();
    setup.add_entity("A", "Alpha").unwrap();
    let (org, _) = setup.finish();

    let mut t = cw_app::entities::blank_table(&[("ENTITY", b'C', 40, 0)]);
    let mut row = vec![b' '; 41];
    row[1] = b'B';
    t.records.push(row); // ENTITY='B'
    let dim = EntityDimension::from_table(&t).unwrap();
    let issues = dim.check(&org);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].kind, OrgIssueKind::UnknownEntity);
    assert_eq!(issues[0].entity, "B");
}

#[test]
fn entity_guid_is_allocated_in_the_base_family() {
    let mut rng = Rng::new_seeded(7);
    let e = Entity::new_with_guid("X", "Entity X", &mut rng).unwrap();
    assert_eq!(e.cwguid.len(), 22);
    let g = CwGuid::parse(&e.cwguid).unwrap();
    assert!(g.matches(Family::A), "CE has no corpus rows ⇒ family A");
}

