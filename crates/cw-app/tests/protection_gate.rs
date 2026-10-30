//! FEAT.006 gate: protection setup store, users/groups, roles, sign-in/out, the
//! 3-strike park, and the lockdown interplay (`attachments/security.md`, F47).
//!
//! ## Provenance
//!
//! Three tiers, strongest first — the same order the area gates use:
//!
//! 1. **frozen container bytes.** `tests/golden/pkg/hull2024.ac_` is the only
//!    shipped protection-**ON** engagement (39 ids in both streams);
//!    `tests/golden/pkg/corp2026.ac_` and `tests/golden/lockdown/before.ac` are
//!    protection-**OFF** (the 6-byte marker). Every stream assertion re-reads
//!    those bytes; nothing about them is searched for.
//! 2. **format constants.** The `EC` 26-column / 531-byte / 865-byte-header
//!    layout and the `SH` role/flag cell set are asserted inline against all
//!    three frozen corpora (`EC` is 0 rows in each, so the schema is the whole
//!    fact).
//! 3. **spec-recorded laws.** The verbatim COM error strings, the admin gate,
//!    level-0 refusal, the file check-out refusal, the 3-fail park constant and
//!    the one-way `doc.Lockdown` rule. No COM round is replayed in this rebuild,
//!    so these are asserted as the constants the spec records
//!    (`attachments/security.md` §1/§3, `export.md` §4) rather than re-probed.
//!
//! The gate states what the corpus cannot evidence at every point where a
//! reader might otherwise assume a fact: the protection-ON roster is empty, no
//! `EC` row exists anywhere, no `ROLESET` is set on a document, no `.locks`
//! sidecar carries a byte, and the password digest is undecoded.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cw_app::lockdown::{LockdownState, LOCKED_PERIOD};
use cw_app::protection::{
    blank_ec_table, decode_protection_stream, decode_userlist_stream, document_roles,
    encode_protection_stream, encode_userlist_stream, group_id_error, read_checkout, role_name,
    role_names, set_document_lockdown, AccessVerdict, AttemptOutcome, CaseViewAccess,
    CheckoutState, CredentialCheck, DocumentRoles, EcTable, FileCheckout, ProtectionInfo,
    ProtectionRequest, ProtectionSetup, ProtectionStore, SecurityGroups, SecuritySession,
    SecurityUsers, SignInOutcome, UserAccount,
    ADMIN_GROUP, DEFAULT_ROLE_SET, EC_FIELD_COUNT, EC_FILE_VERSION, EC_RECORD_LEN,
    ERR_ADMIN_REQUIRED, ERR_DOC_LOCKDOWN_LOCKED, ERR_MANDATORY_ID, ERR_SIGNIN_NOT_SIGNED_OUT,
    LOCKS_SUFFIX, MAX_GROUP_ID_LEN, NUMBER_ROLES, OFF_STREAM_LEN, PARK_AFTER_FAILURES,
    PROTECTION_STREAMS, USERLIST_TRAILER,
};
use cw_app::signoff::SignoffBook;
use cw_core::cfb::Cfb;
use cw_core::dbf::Table;
use cw_core::sha256::sha256_hex;

const CORP_STEM: &str = "USI Compilation Training(1155962)";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn golden() -> PathBuf {
    repo().join("tests/golden")
}

fn pkg(name: &str) -> PathBuf {
    golden().join("pkg").join(format!("{name}.ac_"))
}

fn corpus(tag: &str, suffix: &str) -> PathBuf {
    let dir = golden().join("dbf").join(tag);
    let want = suffix.to_ascii_lowercase();
    std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase().ends_with(&want))
                .unwrap_or(false)
        })
        .unwrap_or_else(|| panic!("no *{suffix} in {}", dir.display()))
}

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("ocw-feat006-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// Stream name -> bytes, for the whole container.
fn streams(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let cfb = Cfb::parse(bytes).unwrap();
    let mut out = BTreeMap::new();
    for entry in cfb.entries().unwrap() {
        if entry.kind != cw_core::cfb::EntryKind::Stream {
            continue;
        }
        out.insert(entry.name(), cfb.stream(&entry.name()).unwrap());
    }
    out
}

// ---------------------------------------------------------------------------
// 1. the stream codec against the shipped bytes
// ---------------------------------------------------------------------------

/// hull2024 is the only protection-ON engagement in the corpus: both streams
/// must decode, agree, and re-encode to the identical bytes.
#[test]
fn protection_on_streams_decode_and_reencode_the_corpus() {
    let bytes = std::fs::read(pkg("hull2024")).unwrap();
    let store = ProtectionStore::from_container(&bytes).unwrap();
    assert!(store.protection(), "hull2024 carries a protection-ON payload");

    // 39 ids, both streams, single-space separated (spec §2/§3).
    assert_eq!(store.users().len(), 39);
    assert_eq!(store.users().first().map(String::as_str), Some("E005535"));
    assert_eq!(store.users().last().map(String::as_str), Some("E110306"));
    assert!(store.users().iter().any(|u| u == "E109586"));
    assert!(store.lists_agree(), "CaseWareUserlist repeats the access list");
    assert_eq!(store.userlist(), store.users());
    assert!(store.groups().is_empty() && store.adgroups().is_empty());
    assert!(store.allow_auto_global_groups(), "the ON sample carries 1");

    // Payload-level byte stability: the re-encoded streams ARE the corpus bytes.
    let all = streams(&bytes);
    assert_eq!(&store.info_stream(), all.get(PROTECTION_STREAMS[0]).unwrap());
    assert_eq!(&store.userlist_stream(), all.get(PROTECTION_STREAMS[1]).unwrap());
    assert_eq!(all[PROTECTION_STREAMS[0]].len(), 758);
    assert_eq!(all[PROTECTION_STREAMS[1]].len(), 636);

    // The payload is the documented shape: u32 LE byte count, UTF-16LE, NUL.
    let raw = &all[PROTECTION_STREAMS[0]][4..];
    assert_eq!(
        u32::from_le_bytes(all[PROTECTION_STREAMS[0]][..4].try_into().unwrap()) as usize,
        raw.len(),
        "count is bytes and includes the NUL unit"
    );
    assert_eq!(&raw[raw.len() - 2..], &[0, 0], "NUL unit terminates the payload");
    let text = decode_protection_stream(&all[PROTECTION_STREAMS[0]])
        .unwrap()
        .unwrap()
        .to_text();
    assert!(text.starts_with("{\"AllowAutoGlobalGroups\":1,\"adgroups\":\"\",\"groups\":\"\",\"users\":\"E005535 "));
    assert!(text.ends_with("\"}\n"));

    // The userlist is the same list with the ` sup` trailer (spec §2).
    let list_text = cw_app::protection::decode_utf16_stream(&all[PROTECTION_STREAMS[1]]).unwrap();
    assert!(list_text.ends_with(USERLIST_TRAILER));
    assert_eq!(list_text.trim_end_matches(USERLIST_TRAILER), store.users().join(" "));
}

/// corp2026 (and the rebuilt hub) carry the protection-OFF marker: exactly the
/// 6 bytes `02 00 00 00 00 00` in both streams.
#[test]
fn protection_off_marker_is_the_corpus_state() {
    for (label, bytes) in [
        ("pkg/corp2026", std::fs::read(pkg("corp2026")).unwrap()),
        ("lockdown/before.ac", std::fs::read(golden().join("lockdown/before.ac")).unwrap()),
    ] {
        let store = ProtectionStore::from_container(&bytes).unwrap();
        assert!(!store.protection(), "{label}: protection is OFF");
        assert!(store.users().is_empty(), "{label}");
        assert!(store.info().is_none(), "{label}");
        assert!(store.lists_agree(), "{label}: two empty lists agree");
        // `AllowAutoGlobalGroups` is only carried by a JSON payload; with the
        // marker there is nothing to read, so the accessor is false.
        assert!(!store.allow_auto_global_groups(), "{label}");
        assert_eq!(store.info_stream(), vec![2, 0, 0, 0, 0, 0], "{label}");
        assert_eq!(store.userlist_stream(), vec![2, 0, 0, 0, 0, 0], "{label}");
        assert_eq!(store.info_stream().len(), OFF_STREAM_LEN);
        let all = streams(&bytes);
        for name in PROTECTION_STREAMS {
            assert_eq!(all[name].len(), OFF_STREAM_LEN, "{label}: {name}");
        }
    }
    // The codec is total on the marker: it decodes to an empty text, and an
    // empty text encodes back to the marker.
    assert_eq!(decode_protection_stream(&[2, 0, 0, 0, 0, 0]).unwrap(), None);
    assert_eq!(encode_protection_stream(None), vec![2, 0, 0, 0, 0, 0]);
    assert!(decode_userlist_stream(&[2, 0, 0, 0, 0, 0]).unwrap().is_empty());
    assert_eq!(encode_userlist_stream(&[]), vec![2, 0, 0, 0, 0, 0]);
}

/// A container rebuild must preserve every stream, and a second rebuild must be
/// a true no-op (the `transact` law this module's save path uses).
#[test]
fn container_rebuild_preserves_every_stream_and_is_idempotent() {
    for (label, name) in [("hull2024", "hull2024"), ("corp2026", "corp2026")] {
        let bytes = std::fs::read(pkg(name)).unwrap();
        let store = ProtectionStore::from_container(&bytes).unwrap();
        let rebuilt = store.encode_into(&bytes).unwrap();
        let before = streams(&bytes);
        let after = streams(&rebuilt);
        assert_eq!(
            before.keys().collect::<Vec<_>>(),
            after.keys().collect::<Vec<_>>(),
            "{label}: the ten canonical streams survive"
        );
        assert_eq!(before.len(), 10, "{label}: canonical container");
        for (key, value) in &before {
            assert_eq!(&after[key], value, "{label}: stream {key} is byte-identical");
        }
        assert!(
            before.contains_key("CaseWareDocument"),
            "{label}: the payload stream is carried"
        );

        // Idempotence through the real save path (no-op ⇒ nothing written).
        let dir = tmp(&format!("rebuild-{label}"));
        let hub = dir.join(format!("{label}.ac_"));
        std::fs::write(&hub, &rebuilt).unwrap();
        let stamp = std::fs::metadata(&hub).unwrap().modified().unwrap();
        let store2 = ProtectionStore::from_container(&rebuilt).unwrap();
        assert!(!store2.save_to(&hub).unwrap(), "{label}: second save is a no-op");
        assert_eq!(
            std::fs::metadata(&hub).unwrap().modified().unwrap(),
            stamp,
            "{label}: an unchanged save must not touch the file"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. setup / parse roundtrip byte stability
// ---------------------------------------------------------------------------

/// `SetProtection` on the OFF hub materializes both streams, and `SetProtection`
/// off restores the exact corpus marker bytes.
#[test]
fn set_protection_roundtrips_to_the_corpus_bytes() {
    let hub = std::fs::read(golden().join("lockdown/before.ac")).unwrap();
    let mut store = ProtectionStore::from_container(&hub).unwrap();
    assert!(!store.protection());

    // Protection ON with the hull2024 roster.
    let hull_store =
        ProtectionStore::from_container(&std::fs::read(pkg("hull2024")).unwrap()).unwrap();
    let users: Vec<&str> = hull_store.users().iter().map(String::as_str).collect();
    let req = ProtectionRequest::enable("E099901", true).with_users(&users);
    let change = store.apply(&req).unwrap();
    assert_eq!(
        change,
        cw_app::protection::ProtectionChange::Enabled {
            users: 39,
            groups: 0,
            adgroups: 0
        }
    );
    assert!(store.protection());
    assert!(store.lists_agree());

    // The encoded container carries exactly hull2024's two stream payloads —
    // the setup write is byte-identical to CW's own ON form.
    let on = store.encode_into(&hub).unwrap();
    let on_streams = streams(&on);
    let hull_streams = streams(&std::fs::read(pkg("hull2024")).unwrap());
    assert_eq!(on_streams[PROTECTION_STREAMS[0]], hull_streams[PROTECTION_STREAMS[0]]);
    assert_eq!(on_streams[PROTECTION_STREAMS[1]], hull_streams[PROTECTION_STREAMS[1]]);

    // Parse the written container: the list survives the roundtrip verbatim.
    let reread = ProtectionStore::from_container(&on).unwrap();
    assert_eq!(reread, store);
    assert_eq!(reread.users(), hull_store.users());
    assert!(reread.allow_auto_global_groups());

    // Protection OFF returns both streams to the 6-byte corpus marker…
    let mut off = reread;
    assert_eq!(
        off.apply(&ProtectionRequest::disable("E099901", true)).unwrap(),
        cw_app::protection::ProtectionChange::Disabled
    );
    let off_streams = streams(&off.encode_into(&hub).unwrap());
    for name in PROTECTION_STREAMS {
        assert_eq!(off_streams[name], vec![2, 0, 0, 0, 0, 0], "{name} back to the marker");
    }
    // …and the OFF container rebuilds to a stream set equal to the corpus hub's.
    assert_eq!(off_streams, streams(&hub));

    // A list with groups and AD groups survives too: those entries are carried,
    // never interpreted (the corpus ON sample has both empty).
    let mut scoped = ProtectionStore::from_container(&hub).unwrap();
    scoped
        .apply(
            &ProtectionRequest::enable("E099901", true)
                .with_users(&["E109586", "E120235"])
                .with_groups(&["ADMN", "AUDT"])
                .with_adgroups(&["ADMINS"])
                .with_allow_auto_global_groups(false),
        )
        .unwrap();
    assert_eq!(scoped.users(), ["E109586", "E120235"]);
    assert_eq!(scoped.groups(), ["ADMN", "AUDT"]);
    assert_eq!(scoped.adgroups(), ["ADMINS"]);
    assert!(!scoped.allow_auto_global_groups());
    assert!(scoped.lists_agree(), "the userlist stays the user list only");
    // ProtectionInfo::users and the userlist agree even though the JSON carries
    // group ids that the userlist has no place for.
    let reread = ProtectionStore::from_container(&scoped.encode_into(&hub).unwrap()).unwrap();
    assert_eq!(reread, scoped);
    assert!(reread.info().unwrap().to_text().contains("\"groups\":\"ADMN AUDT\""));
}

/// The admin gate and the mandatory-id rule are the spec's own strings; an
/// empty password is accepted (spec §3 probe).
#[test]
fn set_protection_is_admin_gated() {
    let hub = std::fs::read(golden().join("lockdown/before.ac")).unwrap();
    let mut store = ProtectionStore::from_container(&hub).unwrap();

    let err = store.apply(&ProtectionRequest::enable("E109586", false)).unwrap_err();
    assert_eq!(err.to_string(), format!("format: {ERR_ADMIN_REQUIRED}"));
    assert!(!store.protection(), "a refused call leaves the store untouched");

    let err = store.apply(&ProtectionRequest::enable("   ", true)).unwrap_err();
    assert_eq!(err.to_string(), format!("format: {ERR_MANDATORY_ID}"));

    // Empty credentials are the spec's own ON probe (`SetProtection(True,
    // 'E099901','')` ⇒ Protection=True): id present, password empty.
    let mut ok = ProtectionRequest::enable("E099901", true).with_users(&["E109586"]);
    ok.password.clear();
    assert!(store.apply(&ok).is_ok());
    assert!(store.protection());
    // The password is neither stored nor compared: the payload carries no
    // digest field, so the stream is the same with any password.
    let with_pw = ProtectionStore::from_container(&hub).unwrap();
    let mut a = with_pw.clone();
    a.apply(&ok.clone().with_password("hunter2")).unwrap();
    assert_eq!(a.info_stream(), store.info_stream());
    assert!(!String::from_utf8_lossy(&store.info_stream()).contains("hunter2"));
    let text = store.info().unwrap().to_text();
    assert!(!text.contains("pass"), "no password material in the payload: {text}");
}

/// `EC` is empty in all three corpora; the schema, the blank-table bytes and an
/// added row are the whole surface. A roster row round-trips through dBase
/// bytes and keeps every column verbatim.
#[test]
fn ec_roster_schema_and_setup_roundtrip() {
    for tag in ["corp2026", "hull2024", "mwgh2025"] {
        let bytes = std::fs::read(corpus(tag, "EC.dbf")).unwrap();
        let t = Table::parse(&bytes).unwrap();
        assert_eq!(t.fields.len(), EC_FIELD_COUNT, "{tag}");
        assert_eq!(t.record_len as usize, EC_RECORD_LEN, "{tag}");
        assert_eq!(t.header_len as usize, 32 + 32 * EC_FIELD_COUNT + 1, "{tag}");
        assert_eq!(t.version, EC_FILE_VERSION, "{tag}");
        assert_eq!(t.count(), 0, "{tag}: the corpus roster is empty (spec §2)");
        let roster = EcTable::from_bytes(&bytes).unwrap();
        assert!(roster.records().is_empty(), "{tag}");
        assert_eq!(roster.encode(), bytes, "{tag}: no-op save is byte-stable");
    }

    // The synthesized blank table has the corpus geometry, and the header is a
    // valid dBase header for the same 26 fields.
    let blank = blank_ec_table();
    assert_eq!(blank.fields.len(), EC_FIELD_COUNT);
    assert_eq!(blank.record_len as usize, EC_RECORD_LEN);
    assert_eq!(blank.header_len as usize, 865);
    assert_eq!(blank.version, EC_FILE_VERSION);
    let blank_bytes = blank.encode();
    assert_eq!(blank_bytes.len(), 866, "header + EOF, 0 records");
    let reparsed = Table::parse(&blank_bytes).unwrap();
    assert!(cw_app::protection::ec_schema_matches(&reparsed));
    assert_eq!(reparsed.encode(), blank_bytes);

    // `Employees.Add('TST01')` (spec §2's probe id) then encode/reparse.
    let mut roster = EcTable::from_table(reparsed).unwrap();
    let mut rng = cw_core::guid::Rng::new_seeded(0x00FE_0A06);
    let stamp = "20261028120000";
    let row = roster.add_employee("TST01", stamp, &mut rng).unwrap();
    assert_eq!(row, 0);
    let added = roster.find("TST01").unwrap();
    assert_eq!(added.user_id, "TST01");
    assert_eq!(added.cw_date, stamp);
    assert_eq!(added.created_on, stamp);
    assert_eq!(added.cwguid.len(), 22);
    assert_eq!(added.full_name(), "", "nothing is invented for the name");
    assert!(added.administrator().is_none(), "no EC column carries the flag");
    // Every other cell is blank: the writer fills nothing it cannot evidence.
    for cell in [
        &added.sin,
        &added.email,
        &added.city,
        &added.exinfo,
        &added.box13_stat,
        &added.tguid,
    ] {
        assert!(cell.is_empty(), "unexpected value {cell:?}");
    }
    assert!(roster.add_employee("TST01", stamp, &mut rng).is_err(), "ids are keys");
    assert_eq!(
        roster.add_employee("  ", stamp, &mut rng).unwrap_err().to_string(),
        format!("format: {ERR_MANDATORY_ID}")
    );

    let bytes = roster.encode();
    let round = Table::parse(&bytes).unwrap();
    assert_eq!(round.count(), 1);
    assert_eq!(round.encode(), bytes, "roster bytes are stable");
    let back = EcTable::from_bytes(&bytes).unwrap();
    assert_eq!(back.records(), roster.records());

    roster.remove(0).unwrap();
    assert_eq!(roster.count(), 0);
    assert!(roster.find("TST01").is_none(), "a tombstone is not a live row");
}

/// Groups: the ≤5-char id rule, uniqueness, the access-level range and the
/// admin group. `AccessRights` kinds stay opaque pairs.
#[test]
fn groups_and_role_configuration() {
    assert_eq!(NUMBER_ROLES, 8);
    assert_eq!(DEFAULT_ROLE_SET, 0);
    assert_eq!(role_names().len(), NUMBER_ROLES as usize);
    assert_eq!(role_name(1).unwrap(), "Prepared by 1");
    assert_eq!(role_name(3).unwrap(), "Prepared by 3");
    assert_eq!(role_name(4).unwrap(), "Reviewed by 1");
    assert_eq!(role_name(8).unwrap(), "Reviewed by 5");
    assert!(role_name(0).is_err() && role_name(9).is_err());

    let mut groups = SecurityGroups::new();
    // The spec's own probe: `T1` is accepted, `TSTGRP` is refused (6 chars).
    groups.add("T1").unwrap();
    assert_eq!(MAX_GROUP_ID_LEN, 5);
    assert_eq!(
        groups.add("TSTGRP").unwrap_err().to_string(),
        format!("format: {}", group_id_error())
    );
    assert_eq!(groups.add("ADMN").unwrap(), 1);
    assert_eq!(groups.len(), 2);
    assert!(groups.add("admn").is_err(), "ids are keys");
    assert!(groups.add("").is_err(), "{}", ERR_MANDATORY_ID);

    let admin = groups.get_mut(ADMIN_GROUP).unwrap();
    admin.access_level = CaseViewAccess::admin();
    admin.name = "Administrators".into();
    admin.all_access_rights = true;
    // Opaque right kinds: carried, never interpreted.
    admin.rights.push(("ReadOnly".into(), true));
    admin.employees.push("E099901".into());
    assert!(groups.get("ADMN").unwrap().is_admin());
    assert_eq!(groups.get("ADMN").unwrap().access_level.level(), 3);
    assert!(groups.is_user_admin("E099901"));
    assert!(!groups.is_user_admin("E064121"), "only an assigned member is an admin");
    assert!(groups.get("T1").unwrap().access_level.level() == 0, "0 is the default level");
    assert!(CaseViewAccess::parse(4).is_err(), "levels are 0..=3");
    groups.remove("T1").unwrap();
    assert!(groups.get("T1").is_none());
    assert!(groups.remove("T1").is_err());
}

/// `CanUserAccess` as far as the spec decodes it: not listed ⇒ denied, level 0
/// ⇒ denied, anything else unresolved (level semantics are an open item).
#[test]
fn can_user_access_only_answers_what_the_spec_records() {
    let bytes = std::fs::read(pkg("hull2024")).unwrap();
    let store = ProtectionStore::from_container(&bytes).unwrap();
    // A listed user at level 0 is refused (spec §3: "0 refused for listed user too").
    assert_eq!(store.can_user_access("E109586", 0), AccessVerdict::Denied);
    // A non-listed user is refused at any level (spec §3).
    assert_eq!(store.can_user_access("NOBODY", 1), AccessVerdict::Denied);
    // Above level 0 for a listed user the level semantics are undecoded.
    assert_eq!(store.can_user_access("E109586", 1), AccessVerdict::Unresolved);

    // With protection OFF there is no access list, so the question is unanswered
    // rather than "nobody may": the spec never probed an unprotected file.
    let off = ProtectionStore::from_container(&std::fs::read(pkg("corp2026")).unwrap()).unwrap();
    assert_eq!(off.can_user_access("E109586", 1), AccessVerdict::Unresolved);
}

// ---------------------------------------------------------------------------
// 3. document roles / flags on the real SH tables
// ---------------------------------------------------------------------------

fn sh_rows(t: &Table, frontier: usize) -> Vec<DocumentRoles> {
    let mut out = Vec::new();
    for row in 0..t.count().min(frontier) {
        if t.is_deleted(row) {
            continue;
        }
        let roles = document_roles(t, row).unwrap();
        if roles.guid.len() == 22 {
            out.push(roles);
        }
    }
    out
}

/// The `SH` role/flag cells: `LOCKDOWN` is populated (template default on review
/// documents), `SIGN` is not, and no document carries a role set, an access code
/// or a task user except hull2024's two `CN` rows.
#[test]
fn document_role_flags_match_the_corpus_census() {
    // corp2026: 93 live rows, 84 locked down (the same set `client.Lockdown()`
    // writes DOCLOCK rows for), no role cells at all.
    let sh = Table::parse_path(corpus("corp2026", "SH.dbf")).unwrap();
    let rows = sh_rows(&sh, sh.count());
    assert_eq!(rows.len(), 93);
    assert_eq!(rows.iter().filter(|r| r.lockdown).count(), 84);
    assert_eq!(rows.iter().filter(|r| r.sign).count(), 0);
    assert!(rows.iter().all(|r| r.roleset.is_empty()));
    assert!(rows.iter().all(|r| r.access.is_empty()));
    assert!(rows.iter().all(|r| r.task_user.is_empty()));

    // mwgh2025: 94 documents, 93 locked down, no role cells.
    let sh = Table::parse_path(corpus("mwgh2025", "SH.dbf")).unwrap();
    let rows = sh_rows(&sh, sh.count());
    assert_eq!(rows.len(), 94);
    assert_eq!(rows.iter().filter(|r| r.lockdown).count(), 93);
    assert!(rows.iter().all(|r| r.roleset.is_empty() && r.task_user.is_empty()));

    // hull2024 inside the pinned readable frontier (603 of 664 physical rows;
    // past it the LZMA tail is synthesis, tests/golden/README.md).
    let sh = Table::parse_path(corpus("hull2024", "SH.dbf")).unwrap();
    let rows = sh_rows(&sh, 603);
    assert_eq!(rows.len(), 101);
    assert_eq!(rows.iter().filter(|r| r.lockdown).count(), 98);
    let cn: Vec<&DocumentRoles> = rows.iter().filter(|r| r.access == "CN").collect();
    assert_eq!(cn.len(), 2, "ACCESS='CN' on exactly the two 0390/0391 audit docs");
    assert!(rows.iter().all(|r| r.roleset.is_empty()), "no document names a role set");
    assert!(rows.iter().all(|r| r.task_user.is_empty()));
    assert_eq!(rows.iter().filter(|r| r.sign).count(), 0);
}

// ---------------------------------------------------------------------------
// 4. sign-in / sign-out state transitions
// ---------------------------------------------------------------------------

#[test]
fn file_checkout_state_transitions() {
    // The corpus normal form: an empty `.locks` sidecar, i.e. not signed out.
    let dir = tmp("checkout");
    std::fs::write(dir.join(format!("{CORP_STEM}{LOCKS_SUFFIX}")), b"").unwrap();
    assert_eq!(read_checkout(&dir, CORP_STEM).unwrap(), CheckoutState::NotSignedOut);
    // An absent sidecar is the same state.
    assert_eq!(read_checkout(&dir, "NOPE").unwrap(), CheckoutState::NotSignedOut);
    // A non-empty sidecar is undecoded: reported, never guessed.
    std::fs::write(dir.join(format!("{CORP_STEM}{LOCKS_SUFFIX}")), b"\x01\x02\x03\x04").unwrap();
    assert_eq!(read_checkout(&dir, CORP_STEM).unwrap(), CheckoutState::Unknown);
    assert!(FileCheckout::from_state(CheckoutState::Unknown).is_err());

    let mut checkout = FileCheckout::from_state(read_checkout(&tmp("checkout2"), "NONE").unwrap())
        .unwrap();
    assert!(!checkout.is_signed_out());
    // `SignIn` on a file that is not signed out is the spec's exact refusal.
    let err = checkout.sign_in().unwrap_err();
    assert_eq!(err.to_string(), format!("format: {ERR_SIGNIN_NOT_SIGNED_OUT}"));

    // SignOut → IsSignedOut → SignIn → back to the corpus state.
    checkout.sign_out();
    assert!(checkout.is_signed_out());
    checkout.sign_out();
    assert!(checkout.is_signed_out(), "a repeated SignOut is idempotent");
    checkout.sign_in().unwrap();
    assert!(!checkout.is_signed_out());
    assert!(checkout.sign_in().is_err(), "the refusal holds after check-in");
}

#[test]
fn session_sign_in_and_sign_out() {
    let mut session = SecuritySession::new();
    assert_eq!(session.current_user(), "");
    assert!(session.sign_in("  ", CredentialCheck::Accepted).is_err());

    assert_eq!(
        session.sign_in("E120235", CredentialCheck::Accepted).unwrap(),
        SignInOutcome::SignedIn
    );
    assert_eq!(session.current_user(), "E120235");
    session.sign_out();
    assert_eq!(session.current_user(), "");
    assert_eq!(session.failures("E120235"), 0, "a success clears the counter");
}

// ---------------------------------------------------------------------------
// 5. wrong password / 3-strike park
// ---------------------------------------------------------------------------

#[test]
fn three_wrong_passwords_park_the_identity() {
    assert_eq!(PARK_AFTER_FAILURES, 3);
    let mut session = SecuritySession::new();
    let user = "E109586";

    assert_eq!(
        session.sign_in(user, CredentialCheck::Rejected).unwrap(),
        SignInOutcome::WrongPassword { failures: 1, remaining: 2 }
    );
    assert_eq!(session.current_user(), "", "a wrong password never signs in");
    assert_eq!(session.failures(user), 1);
    assert_eq!(
        session.sign_in(user, CredentialCheck::Rejected).unwrap(),
        SignInOutcome::WrongPassword { failures: 2, remaining: 1 }
    );
    // The third failure trips the park.
    assert_eq!(session.sign_in(user, CredentialCheck::Rejected).unwrap(), SignInOutcome::Parked);
    assert!(session.is_parked(user));
    assert_eq!(session.failures(user), PARK_AFTER_FAILURES);

    // A parked identity refuses even the correct password (a park is not a
    // counter the next attempt can reset).
    assert_eq!(session.sign_in(user, CredentialCheck::Accepted).unwrap(), SignInOutcome::Parked);
    assert_eq!(session.current_user(), "");
    assert_eq!(session.sign_in(user, CredentialCheck::Rejected).unwrap(), SignInOutcome::Parked);
    assert_eq!(session.failures(user), PARK_AFTER_FAILURES, "a parked ledger stops counting");

    // The park is per identity.
    assert_eq!(
        session.sign_in("E120235", CredentialCheck::Accepted).unwrap(),
        SignInOutcome::SignedIn
    );

    // The explicit release is operator action (no COM path is recorded).
    session.reset_park(user);
    assert!(!session.is_parked(user));
    assert_eq!(session.failures(user), 0);
    assert_eq!(
        session.sign_in(user, CredentialCheck::Accepted).unwrap(),
        SignInOutcome::SignedIn
    );
    assert_eq!(session.current_user(), user);
}

/// A success before the third failure resets the ladder, so interleaved
/// identities cannot accumulate strikes against each other.
#[test]
fn wrong_password_then_success_resets_the_ladder() {
    let mut ledger = cw_app::protection::AttemptLedger::new();
    assert_eq!(
        ledger.attempt("u", CredentialCheck::Rejected),
        AttemptOutcome::WrongPassword { failures: 1, remaining: 2 }
    );
    assert_eq!(
        ledger.attempt("u", CredentialCheck::Rejected),
        AttemptOutcome::WrongPassword { failures: 2, remaining: 1 }
    );
    assert_eq!(ledger.attempt("u", CredentialCheck::Accepted), AttemptOutcome::Accepted);
    assert_eq!(ledger.failures("u"), 0);
    assert!(!ledger.is_parked("u"));
    // The full ladder is available again.
    for _ in 0..2 {
        assert!(matches!(
            ledger.attempt("u", CredentialCheck::Rejected),
            AttemptOutcome::WrongPassword { .. }
        ));
    }
    assert_eq!(ledger.attempt("u", CredentialCheck::Rejected), AttemptOutcome::Parked);
}

// ---------------------------------------------------------------------------
// 6. lockdown interaction
// ---------------------------------------------------------------------------

/// `doc.Lockdown` is writable only while the engagement is unlocked; once
/// `client.Lockdown()` has run, the put is the spec's refusal. The per-document
/// flag and the engagement lock stay independent (lockdown.md §5.4).
#[test]
fn document_lockdown_is_refused_after_the_engagement_locks() {
    let sh_bytes = std::fs::read(corpus("corp2026", "SH.dbf")).unwrap();
    let mut book = SignoffBook::from_bytes(&sh_bytes).unwrap();
    // The corpus hub is not locked down.
    let hub = std::fs::read(golden().join("lockdown/before.ac")).unwrap();
    let state = LockdownState::from_ac(&hub).unwrap();
    assert!(!state.locked);
    // The unlocked corpus hub still carries pid 277, but as the zero FILETIME
    // (`lockdown.md` §2.1 "before: 0"): the sentinel is a date, not a lock.
    assert_eq!(state.date, Some(0), "pid 277 is zero until the engagement locks");

    // The per-document template default is `Y` on the review documents.
    let target = book
        .sheets()
        .iter()
        .find(|s| s.locked_down)
        .expect("corpus has locked-down review documents")
        .guid
        .clone();
    let locked_before = book.sheet(&target).unwrap().locked_down;

    // Unlocked engagement: the put lands in `SH.LOCKDOWN`.
    set_document_lockdown(&mut book, &target, !locked_before, &state).unwrap();
    assert_eq!(book.sheet(&target).unwrap().locked_down, !locked_before);
    set_document_lockdown(&mut book, &target, locked_before, &state).unwrap();
    assert_eq!(book.sheet(&target).unwrap().locked_down, locked_before);
    // …and the re-encoded table is byte-identical to the corpus once restored.
    assert_eq!(book.encode(), sh_bytes, "a flipped-then-restored flag is byte-stable");

    // Locked engagement: both directions are refused with the spec's string.
    let locked_state = LockdownState {
        locked: true,
        date: Some(134_372_735_990_000_000),
        stamp: Some(134_372_176_730_000_000),
    };
    for value in [true, false] {
        let err = set_document_lockdown(&mut book, &target, value, &locked_state).unwrap_err();
        assert_eq!(err.to_string(), format!("format: {ERR_DOC_LOCKDOWN_LOCKED}"));
    }
    // The refusal left the table untouched.
    assert_eq!(book.encode(), sh_bytes);

    // `LockdownState::from_ac` reads the same state the writer stores: the
    // engagement lock (`\x05DocumentSummaryInformation` pids 267/273/277) is a
    // separate mark from `SH.LOCKDOWN`, which the engagement lock leaves alone.
    let mut patched = hub.clone();
    let after = cw_app::lockdown::mark_lockdown(
        &mut patched,
        134_372_176_730_000_000,
        134_372_735_990_000_000,
    )
    .unwrap();
    assert!(after.locked);
    let reread = LockdownState::from_ac(&patched).unwrap();
    assert_eq!(reread, after);
    // The locked hub's other streams (including the protection pair) are intact.
    let before_streams = streams(&hub);
    let after_streams = streams(&patched);
    for name in PROTECTION_STREAMS {
        assert_eq!(after_streams[name], before_streams[name], "{name} untouched by the lock");
    }
    // And the per-document marker the engagement lock writes is a *period* code,
    // not `SH.LOCKDOWN` — the two flags stay independent.
    assert_eq!(LOCKED_PERIOD, "Y1");
    assert_ne!(LOCKED_PERIOD, "Y");
}

/// What the corpus cannot evidence, asserted as absence so a future fixture
/// that gains the data becomes a visible change instead of a silent widening.
#[test]
fn undocumented_surface_is_reported_absent() {
    // No protection-ON engagement besides hull2024; no roster anywhere.
    for tag in ["corp2026", "hull2024", "mwgh2025"] {
        let roster = EcTable::from_bytes(&std::fs::read(corpus(tag, "EC.dbf")).unwrap()).unwrap();
        assert_eq!(roster.count(), 0, "{tag}: sec.Employees is machine-wide, not per-file");
    }
    // No `.locks` byte in any shipped engagement directory: only the empty
    // sidecar exists, so the check-out *record* layout stays undecoded. The
    // expanded engagements are not part of the frozen fixture set, so this
    // fact is asserted where the directories are on disk and skipped otherwise
    // (the same contract the cw-cv corpus gates use).
    let expanded = repo().join("expanded");
    if expanded.is_dir() {
        let mut seen = 0usize;
        let mut nonempty = Vec::new();
        for entry in std::fs::read_dir(&expanded).unwrap().filter_map(Result::ok) {
            if !entry.path().is_dir() {
                continue;
            }
            let dir = entry.path();
            let Some(locks) = std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .map(|e| e.path())
                .find(|p| p.to_string_lossy().to_ascii_lowercase().ends_with(LOCKS_SUFFIX))
            else {
                continue;
            };
            let len = std::fs::metadata(&locks).unwrap().len();
            seen += 1;
            if len > 0 {
                nonempty.push(locks);
            }
        }
        assert!(seen >= 3, "the engagement set ships .locks sidecars");
        assert!(nonempty.is_empty(), "no corpus sidecar carries a record: {nonempty:?}");
    }
    // The frozen table corpora never carry a sidecar at all.
    for tag in ["corp2026", "hull2024", "mwgh2025"] {
        let hit = std::fs::read_dir(golden().join("dbf").join(tag))
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| p.to_string_lossy().to_ascii_lowercase().ends_with(LOCKS_SUFFIX));
        assert!(hit.is_none(), "{tag}: the frozen corpora are table-only");
    }

    // The protection payload has no field for a password, an administrator flag
    // or a role table — the undecoded halves are structural, not missing code.
    let store =
        ProtectionStore::from_container(&std::fs::read(pkg("hull2024")).unwrap()).unwrap();
    let info = store.info().unwrap();
    assert!(info.groups.is_empty() && info.adgroups.is_empty());
    assert_eq!(store.users().len(), 39);
    let keys = info.to_text().matches("\":").count();
    assert_eq!(keys, 4, "exactly the four decoded keys");
}

/// `CaseWareUserlist` disagreement is carried verbatim, never silently fixed:
/// the store reports it and the encoder writes the `ProtectionInfo` list.
#[test]
fn diverging_userlist_is_reported_not_repaired() {
    let store = ProtectionStore::from_container(&std::fs::read(pkg("hull2024")).unwrap()).unwrap();
    assert!(store.lists_agree());
    // Build a container whose userlist drops the last id.
    let base = std::fs::read(pkg("hull2024")).unwrap();
    let mut users = store.users().to_vec();
    users.pop();
    let list = encode_userlist_stream(&users);
    let cfb = Cfb::parse(&base).unwrap();
    let mut builder = cw_core::cfb::CfbBuilder::new();
    for entry in cfb.entries().unwrap() {
        if entry.kind != cw_core::cfb::EntryKind::Stream {
            continue;
        }
        let name = entry.name();
        let data = if name == PROTECTION_STREAMS[1] {
            list.clone()
        } else {
            cfb.stream(&name).unwrap()
        };
        builder.add(name, data);
    }
    let split = builder.build().unwrap();
    let read_back = ProtectionStore::from_container(&split).unwrap();
    assert!(!read_back.lists_agree(), "the disagreement is visible");
    assert_eq!(read_back.users().len(), 39, "the JSON list is authoritative");
    assert_eq!(read_back.userlist().len(), 38);
    // Re-encoding restores the JSON list into the userlist (the CW law the
    // corpus shows), and the digest of the repaired container matches the
    // original stream bytes.
    let repaired = read_back.encode_into(&split).unwrap();
    let repaired_streams = streams(&repaired);
    let original = streams(&base);
    assert_eq!(repaired_streams[PROTECTION_STREAMS[1]], original[PROTECTION_STREAMS[1]]);
    assert_eq!(
        sha256_hex(&repaired_streams[PROTECTION_STREAMS[0]]),
        sha256_hex(&original[PROTECTION_STREAMS[0]])
    );
    assert_eq!(read_back.users().len(), 39);
}

/// The protection store opens from a package path without an explicit stem
/// choice, and refuses the 5-byte compressed-engagement stub instead of
/// reporting protection-OFF.
#[test]
fn store_opens_from_a_package_and_rejects_the_hub_stub() {
    let dir = tmp("open");
    let hull_pkg = pkg("hull2024");
    let want = dir.join("HULLCOPY.ac_");
    std::fs::copy(&hull_pkg, &want).unwrap();
    let (path, store) = ProtectionStore::from_dir(&dir, "HULLCOPY").unwrap();
    assert_eq!(path, want);
    assert!(store.protection());
    assert_eq!(store.users().len(), 39);

    // A `.ac` stub (what CW leaves after compressing) carries no streams.
    std::fs::write(dir.join("STUB.ac"), [0x05u8, b'8', b'.', b'9', b'8']).unwrap();
    let err = ProtectionStore::from_dir(&dir, "STUB").unwrap_err();
    assert!(
        err.to_string().contains("stub"),
        "the stub must be named, not read as protection-OFF: {err}"
    );

    // …and the corpus hub reads protection-OFF through the same entry point.
    let hub_dir = tmp("open-hub");
    std::fs::copy(golden().join("lockdown/before.ac"), hub_dir.join("HUB.ac")).unwrap();
    let (_, hub_store) = ProtectionStore::from_dir(&hub_dir, "HUB").unwrap();
    assert!(!hub_store.protection());
}

/// The `CaseWareProtectionInfo` payload refuses shapes it does not decode rather
/// than dropping fields (a change in CW's JSON must be visible).
#[test]
fn protection_payload_shape_is_enforced() {
    let ok = ProtectionInfo {
        allow_auto_global_groups: false,
        adgroups: vec!["ADMINS".into()],
        groups: vec!["ADMN".into()],
        users: vec!["E109586".into(), "E120235".into()],
    };
    assert_eq!(ProtectionInfo::parse(&ok.to_text()).unwrap(), ok);
    assert_eq!(
        ok.to_text(),
        "{\"AllowAutoGlobalGroups\":0,\"adgroups\":\"ADMINS\",\"groups\":\"ADMN\",\"users\":\"E109586 E120235\"}\n"
    );
    // CRLF is tolerated on read, normalized on write.
    let crlf = ok.to_text().replace('\n', "\r\n");
    assert_eq!(ProtectionInfo::parse(&crlf).unwrap(), ok);

    for bad in [
        // an unknown key
        "{\"AllowAutoGlobalGroups\":1,\"adgroups\":\"\",\"groups\":\"\",\"users\":\"\",\"x\":1}\n",
        // a missing key
        "{\"AllowAutoGlobalGroups\":1,\"adgroups\":\"\",\"groups\":\"\"}\n",
        // a non-boolean flag
        "{\"AllowAutoGlobalGroups\":2,\"adgroups\":\"\",\"groups\":\"\",\"users\":\"\"}\n",
        // a non-string list
        "{\"AllowAutoGlobalGroups\":1,\"adgroups\":\"\",\"groups\":\"\",\"users\":7}\n",
        // not an object at all
        "[]\n",
    ] {
        assert!(ProtectionInfo::parse(bad).is_err(), "accepted {bad:?}");
    }
    // A populated userlist without the trailer is a shape error.
    let missing = cw_app::protection::encode_utf16_stream("E109586");
    assert!(decode_userlist_stream(&missing).is_err());
}

// ---------------------------------------------------------------------------
// 7. ProtectionSetup / sec.Users
// ---------------------------------------------------------------------------

/// `app.ProtectionSetup(user,pwd)` is admin-gated with the spec's mandatory-id
/// refusal, and its `Users` view is the engagement access list.
#[test]
fn protection_setup_view_is_admin_gated() {
    let hull = std::fs::read(pkg("hull2024")).unwrap();
    assert_eq!(
        ProtectionSetup::open(&hull, "  ", true).unwrap_err().to_string(),
        format!("format: {ERR_MANDATORY_ID}")
    );
    assert_eq!(
        ProtectionSetup::open(&hull, "E109586", false).unwrap_err().to_string(),
        format!("format: {ERR_ADMIN_REQUIRED}")
    );

    let setup = ProtectionSetup::open(&hull, "E099901", true).unwrap();
    assert!(setup.access.protection());
    assert_eq!(setup.access_ids().len(), 39);
    // The setup view and the store answer identically (one law, one place).
    assert_eq!(setup.can_user_access("E109586", 0), AccessVerdict::Denied);
    assert_eq!(setup.can_user_access("E109586", 2), AccessVerdict::Unresolved);
    assert_eq!(setup.can_user_access("NOBODY", 2), AccessVerdict::Denied);

    // The corpus OFF hub opens too, with no access ids.
    let off = std::fs::read(golden().join("lockdown/before.ac")).unwrap();
    let setup = ProtectionSetup::open(&off, "E099901", true).unwrap();
    assert!(!setup.access.protection());
    assert!(setup.access_ids().is_empty());
}

/// `sec.Users`: the 1-based collection law, id uniqueness, and `CanUserAccess`
/// delegating to the engagement list.
#[test]
fn signon_users_mirror() {
    let mut users = SecurityUsers::new();
    assert!(users.is_empty());
    // The spec's probe read six accounts; the id is what identifies them.
    for id in ["", "DW", "E109583", "E109585", "E120235", "RSM"] {
        if id.is_empty() {
            assert_eq!(
                users.add(id).unwrap_err().to_string(),
                format!("format: {ERR_MANDATORY_ID}")
            );
            continue;
        }
        users.add(id).unwrap();
    }
    assert_eq!(users.len(), 5);
    assert_eq!(users.item(1).unwrap().user_id, "DW", "Item(i) is 1-based");
    assert_eq!(users.item(5).unwrap().user_id, "RSM");
    assert!(users.item(0).is_none(), "there is no Item(0)");
    assert!(users.item(6).is_none());
    assert!(users.get("dw").is_some(), "lookup is case-insensitive");
    assert!(users.add("RSM").is_err(), "ids are keys");
    assert!(users.get("E109586").is_none());

    // `CanUserAccess` is the store's law, not a second implementation.
    let store =
        ProtectionStore::from_container(&std::fs::read(pkg("hull2024")).unwrap()).unwrap();
    assert!(users.get("E109586").is_none(), "E109586 is not a local signon account");
    let listed = UserAccount::new("E109586").unwrap();
    assert_eq!(listed.can_user_access(&store, 0), AccessVerdict::Denied);
    assert_eq!(listed.can_user_access(&store, 1), AccessVerdict::Unresolved);
    let unlisted = UserAccount::new("RSM").unwrap();
    assert_eq!(unlisted.can_user_access(&store, 1), AccessVerdict::Denied);

    users.remove("RSM").unwrap();
    assert!(users.get("RSM").is_none());
    assert!(users.remove("RSM").is_err());
    assert_eq!(users.len(), 4);
    // A mirrored account carries no invented display name.
    assert_eq!(users.item(1).unwrap().full_name, "");
    assert_eq!(users.item(1).unwrap().short_name, "");
}

/// `PR` owns per-item initials, not users or permissions: the signoff/permission
/// surface must not appear there, and the `EC`/`SH` split must hold.
#[test]
fn program_table_is_not_a_permission_store() {
    let pr = Table::parse_path(corpus("corp2026", "PR.dbf")).unwrap();
    let names: Vec<String> = pr
        .fields
        .iter()
        .map(|f| f.name.to_ascii_uppercase())
        .collect();
    for forbidden in ["EMP_NUM", "USERID", "ROLESET", "ACCESS", "LOCKDOWN", "PASSWORD"] {
        assert!(!names.iter().any(|n| n == forbidden), "PR must not carry {forbidden}");
    }
    // The initials family PR does carry is the checklist assertion block.
    for expected in ["A1", "A7", "M1", "M4", "STAFF", "DONE_BY"] {
        assert!(names.iter().any(|n| n == expected), "PR carries {expected}");
    }
    // EC is the employee roster and SH the document register: no overlap of the
    // protection columns into them.
    let ec = Table::parse_path(corpus("corp2026", "EC.dbf")).unwrap();
    let ec_names: Vec<String> =
        ec.fields.iter().map(|f| f.name.to_ascii_uppercase()).collect();
    assert!(!ec_names.iter().any(|n| n == "ROLESET" || n == "ACCESS"));
    assert!(ec_names.iter().any(|n| n == "EMP_NUM"));
    let sh = Table::parse_path(corpus("corp2026", "SH.dbf")).unwrap();
    let sh_names: Vec<String> =
        sh.fields.iter().map(|f| f.name.to_ascii_uppercase()).collect();
    assert!(!sh_names.iter().any(|n| n == "EMP_NUM" || n == "PASSWORD"));
    for expected in ["ROLESET", "ACCESS", "TASKUSER", "SIGN", "LOCKDOWN"] {
        assert!(sh_names.iter().any(|n| n == expected), "SH carries {expected}");
    }
}
