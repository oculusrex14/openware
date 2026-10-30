//! cw-app protection / users / sign-in-out (FEAT.006, F47).
//!
//! Contract: `attachments/security.md` v1.0 (F47) + `lockdown.md` §5.4 +
//! `features-cw.md` row *Protection Setup, users/groups, sign-in/out,
//! check-out/in*. Evidence tiers used here, strongest first:
//!
//! 1. **frozen package bytes** (`tests/golden/pkg/{hull2024,corp2026}.ac_`) — the
//!    two protection streams decode byte-exactly, and this module re-encodes
//!    them byte-identically;
//! 2. **format constants** — the 26-column / 531-byte `EC` layout and the `SH`
//!    role/flag cells, asserted against all three frozen corpora;
//! 3. **spec-recorded COM laws** — error strings, the admin gate, the
//!    one-way document-lockdown rule (no COM round is replayed here).
//!
//! ## Where the state lives (spec §2, re-verified on the frozen containers)
//!
//! | state | storage | corpus |
//! |---|---|---|
//! | engagement access list | `.ac` stream `CaseWareProtectionInfo` | hull2024 758 B / 39 ids; corp2026 + mwgh2025 the OFF marker |
//! | the same list, ` sup` trailer | `.ac` stream `CaseWareUserlist` | hull2024 636 B |
//! | per-engagement employee roster | `EC` table (26 cols, 531 B/rec, header 865, v`0xF5`) | **0 rows in every corpus** |
//! | per-document role/flag cells | `SH.ROLESET` / `SH.ACCESS` / `SH.TASKUSER` / `SH.SIGN` / `SH.LOCKDOWN` | `ROLESET` names a set on **no** document; `ACCESS='CN'` on 2 of hull2024's 101 documents and nowhere else; `TASKUSER` populated on exactly one hull2024 row and on **no document** (that row also has a blank `PRG`/`NAME`/`CWGUID`, so it is not a document row); `SIGN='Y'` on none; `LOCKDOWN='Y'` on 84 of 93 corp2026 / 93 of 94 mwgh2025 / 98 of 101 hull2024 documents |
//! | file check-out record | `<stem>.locks` sidecar | present in every expanded engagement, **0 bytes** in all of them (checked in the gate when the directories are on disk) |
//!
//! Both protection streams use one codec: `u32` LE **byte** count, then that
//! many bytes of UTF-16LE, ending in one NUL unit (the NUL is inside the
//! count). Protection OFF is therefore exactly the 6-byte marker
//! `02 00 00 00 00 00` (count 2 = one NUL unit), which is what `encode_*`
//! emits for an empty payload and what the frozen packages carry. The
//! `CaseWareProtectionInfo` payload is
//! `{"AllowAutoGlobalGroups":<0|1>,"adgroups":"…","groups":"…","users":"<ids>"}\n`
//! followed by the NUL; `CaseWareUserlist` carries the *same* id list with a
//! ` sup` trailer instead of the JSON. Group/adgroup/user fields are
//! single-space separated. `ProtectionInfo::users` and the `Userlist` body are
//! the same list in the corpus; a container where they disagree is carried
//! verbatim and reported by [`ProtectionStore::lists_agree`].
//!
//! ## Decoded (implemented)
//!
//! - the stream codec, both payload shapes, protection ON/OFF;
//! - the admin gate on `SetProtection`: empty acting id ⇒ `Id is a mandatory
//!   field.`; non-admin ⇒ `You must be an administrator to access this
//!   functionality.`; an **empty password is accepted** (spec §3:
//!   `SetProtection(True,'E099901','')` ⇒ protection on);
//! - `CanUserAccess`: a user absent from the access list is refused; a listed
//!   user is refused at level 0 (spec §3) and undecided above it;
//! - `EC` employee records: `EMP_NUM` is `UserId` and `PRE_NAME`/`FRST_NAME`/
//!   `LAST_NAME`/`EMAIL` are `PreName`/`FirstName`/`LastName`/`EmailAddress`,
//!   plus every remaining column carried verbatim. **[INFERENCE]** The four
//!   name-identical columns are the strong case; `EMP_NUM` -> `UserId` rests on
//!   the column name plus the spec's `Employees.Add('TST01')` /
//!   `emp1 UserId=TST01` probe (an id of the same shape and width). No
//!   put->diff can exist for it: `EC` has 0 rows in every corpus, so a CW
//!   `Employees.Add` round never landed in a frozen file;
//! - group ids (≤5 chars), `CaseViewAccessLevel` 0..=3 with `ADMN`=3,
//!   `AllAccessRights`, `AssignedEmployees`, and opaque `AccessRights(kind,…)`
//!   pairs the caller supplies;
//! - the `ProtectionSetup` view (admin gate, `Users`/`Groups`) and the
//!   `sec.Users` signon mirror with the 1-based `Item(i)` collection law;
//! - file check-out (`SignIn`/`SignOut`/`IsSignedOut`): the "not signed out"
//!   refusal and the state transition;
//! - the 3-strike failed-attempt park and the wrong-password path;
//! - `SH` role/flag cells (`ROLESET`, `ACCESS`, `TASKUSER`, `SIGN`,
//!   `LOCKDOWN`) and the one-way `doc.Lockdown` rule while the engagement is
//!   locked (interplay with [`crate::lockdown`]);
//! - the role name/number configuration: `NumberRoles=8`, `Role(1..3)` =
//!   *Prepared by n*, `Role(4..8)` = *Reviewed by n*, default `RoleSets` 0
//!   (names are re-exported from [`crate::signoff`], not duplicated).
//!
//! ## Documented-unknown — deliberately **not** invented
//!
//! - **Password cryptography** (`SetPassword`, `ChangeIdentity`, the
//!   `SetProtection` secret; protection crypto, `cwsrv64`). No
//!   digest is decoded, so the engine never stores or compares a password: the
//!   caller supplies the verdict ([`CredentialCheck`]) and the password field
//!   of [`ProtectionRequest`] is accepted and discarded.
//! - **`CanUserAccess` level semantics** (spec §5, G2). Only "not listed ⇒
//!   false" and "level 0 refused for a listed user too" are recorded, so a
//!   listed user above level 0 yields [`AccessVerdict::Unresolved`].
//! - **`Group.AccessRights(kind,bool)` kinds** and the `CaseViewAccessLevel`
//!   names for 0..2 (only `ADMN`=3 is named). Rights are carried as opaque
//!   `(kind, bool)` pairs; levels are a validated 0..=3 number.
//! - **`Employee.Administrator` / `ShortName` / `Position` / `Type` /
//!   `Designation` / `ActualRate` / `PlanRate`**: no `EC` column is named for
//!   any of them (the 26 columns are the roster's own) and the corpus roster is
//!   empty, so admin-ness is a *caller-supplied directory fact* (`is_admin`),
//!   not a file read. `sec.Employees` is machine-wide — 129k rows on the probe
//!   box, spec §2 — and has no fixture. The one column that could still carry
//!   such flags is the `EXINFO` memo, whose FPT payload this lane does not
//!   decode (`EmployeeRecord::administrator` returns `None` rather than
//!   guessing from an unread blob).
//! - **`sec.Groups` / `sec.Users` on-disk form** (machine-wide global store, 7
//!   groups / 6 signon users in the spec probe, no file). [`SecurityGroups`] is
//!   the in-memory mirror; nothing is written into the engagement.
//! - **`.locks` record layout** (`ICWDocumentLockInfo`: `UserName`, …). The
//!   corpus sidecars are zero bytes, so [`read_checkout`] reports
//!   [`CheckoutState::Unknown`] for a non-empty sidecar instead of guessing.
//! - **`ClientOptions.RoleReadyForReview(i)` / `RoleFlagExternalAndCV(i)` /
//!   `RoleCVSignOffReadOnly(i)` / `DisplayRoles*`**, and
//!   **`ICWSignoffRequirements.Role1..8Required` / `AnyOfTheFollowing`**, and
//!   the `SH.S1..S8`/`SD1..SD8` family: the COM accessors exist but no value
//!   was read or written, and no registry table is decoded (the engagement
//!   options block is unmapped). They are absent from the API rather than
//!   defaulted.
//! - **The parked-attempt path itself.** Exactly two things about it are
//!   recorded: the threshold (the `3-fail+ rule` the probe lane ran under —
//!   `export.md` §4 parks a lane after 3 cumulative failed COM probes,
//!   `journals.md` §"3-fail rule engaged") and the fact that a park is terminal
//!   for the lane. **No COM password-refusal message, no park flag on disk, and
//!   no unlock path were ever observed for the product**, so
//!   [`AttemptLedger`] is an explicit in-memory model with no invented error
//!   string: [`AttemptOutcome`]/[`SignInOutcome`] are typed outcomes, and the
//!   park release ([`SecuritySession::reset_park`]) is labelled operator action.
//!   Where CW would keep such a counter is undecoded.
//! - **Session (`CurrentUser`) semantics beyond the property.** Only
//!   "protection OFF ⇒ `CurrentUser=''`" and "protection ON populates the access
//!   list" are recorded; the sign-in-as-admin flow is an open item (G2). The
//!   [`SecuritySession`] transitions are an explicit model (documented), not a
//!   replayed COM round.
//! - **Write-side collateral of enabling protection** (`AC.dbf`/`.cdx` rewrite,
//!   `.ac` mtime bump — spec §3). The index writer is a different lane; this
//!   module writes only the two streams and says so in the report.
//!
//! ## `PR` (program checklist)
//!
//! `PR` holds per-item assertion initials (`A1..A7`) and `M1..M4`, not a user or
//! permission store; [`crate::program`] owns it. The signoff ladder itself
//! (`SH.A1..8`/`AD1..8`, `SIGN`) belongs to [`crate::signoff`]. Neither is
//! duplicated here.
//!
//! ## Deviation from CW
//!
//! The container write is a **CFB rebuild** over the parsed stream set (payload
//! carried verbatim, every untouched stream byte-identical), not CW's in-place
//! patch: both protection streams are mini streams (6 / 758 / 636 bytes, all
//! under the 4096-byte cutoff) and `Cfb::patch_stream_in_place` neither accepts a
//! mini stream nor a length change, so no in-place write exists for them. The
//! stream set and its contents are preserved; the CFB sector layout is ours
//! (`CfbBuilder`), exactly as the package rebuild path already does for `.ac_`
//! files. Nested (sub-storage) streams are refused explicitly — the rebuild is a
//! flat-root profile and must not silently flatten a path.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use cw_core::cfb::{Cfb, CfbBuilder, EntryKind};
use cw_core::dbf::Table;
use cw_core::guid::{CwGuid, Rng};
use cw_core::json;
use cw_core::pkg::{S_PROTECTION, S_USERLIST};
use cw_core::transact::commit_if_changed;
use cw_core::{CoreError, Result};

use crate::accounts::write_cell;
use crate::lockdown::LockdownState;
use crate::signoff::{self, SignoffBook};

// ---------------------------------------------------------------------------
// COM error strings and lane constants (spec-recorded)
// ---------------------------------------------------------------------------

/// `SetProtection(True, nonAdmin, pwd)` (spec §3, verbatim).
pub const ERR_ADMIN_REQUIRED: &str =
    "You must be an administrator to access this functionality.";
/// `app.ProtectionSetup('', '')`, `Employees.Add('')`, `Groups.Add('')` (spec §1).
pub const ERR_MANDATORY_ID: &str = "Id is a mandatory field.";
/// `client.SignIn()` on a file that is not signed out (spec §1, verbatim).
pub const ERR_SIGNIN_NOT_SIGNED_OUT: &str =
    "Unable to sign in. You can't sign in a file that is not signed out.";
/// `doc.Lockdown` put after `client.Lockdown()` (spec §1, verbatim including
/// the source's `propery` spelling).
pub const ERR_DOC_LOCKDOWN_LOCKED: &str =
    "Can't change document lockdown propery after file has been locked down.";

/// Maximum security-group id length (spec §2: `TSTGRP` refused, `T1` accepted).
pub const MAX_GROUP_ID_LEN: usize = 5;
/// `CaseViewAccessLevel` of the `ADMN` (Administrators) group (spec §1).
pub const ADMIN_ACCESS_LEVEL: u8 = 3;
/// Name of the administrator group in the global store (spec §2).
pub const ADMIN_GROUP: &str = "ADMN";

/// COM `ClientOptions.NumberRoles` (spec §1).
pub const NUMBER_ROLES: u8 = 8;
/// COM `ClientOptions.RoleSets` factory default (spec §1: "0 default").
pub const DEFAULT_ROLE_SET: u8 = 0;
/// Failed sign-on attempts before the identity is parked (lane law: D4b
/// "3-fail park", `export.md` §4 "3-fail+ rule").
pub const PARK_AFTER_FAILURES: u32 = 3;

/// The two `.ac` streams carrying the engagement access list, in the order
/// CW's `SetProtection` populates them.
pub const PROTECTION_STREAMS: [&str; 2] = [S_PROTECTION, S_USERLIST];

/// `CaseWareUserlist` trailer after the id list (spec §2).
pub const USERLIST_TRAILER: &str = " sup";

/// Byte length of the protection-OFF payload (`u32` count 2 + one NUL unit).
pub const OFF_STREAM_LEN: usize = 6;

/// `EC` file version byte in every frozen corpus (`0xF5`, CW-marked dBase).
pub const EC_FILE_VERSION: u8 = 0xF5;

fn cw(msg: &str) -> CoreError {
    CoreError::Format(msg.to_string())
}

/// `Groups.Add(id)` id rule (spec §2). The spec truncates the message; the
/// shape is reconstructed from the sibling law recorded verbatim in
/// `journals.md` (`Id must be no more than 2 characters.`).
pub fn group_id_error() -> String {
    format!("Id must be no more than {MAX_GROUP_ID_LEN} characters.")
}

// ---------------------------------------------------------------------------
// Stream codec: u32 LE byte count + UTF-16LE + NUL
// ---------------------------------------------------------------------------

/// Encode one protection-stream payload. An empty string yields the 6-byte
/// OFF marker `02 00 00 00 00 00`, which is the corpus form.
pub fn encode_utf16_stream(text: &str) -> Vec<u8> {
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0u16)).collect();
    let mut out = Vec::with_capacity(4 + units.len() * 2);
    out.extend_from_slice(&((units.len() * 2) as u32).to_le_bytes());
    for u in units {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

/// Decode one protection-stream payload, stripping the trailing NUL units.
///
/// Strict: the declared count is a **byte** count and must equal the stream
/// length minus the 4-byte prefix (that holds on both frozen containers).
pub fn decode_utf16_stream(bytes: &[u8]) -> Result<String> {
    if bytes.len() < 4 {
        return Err(cw("protection stream: shorter than its 4-byte length prefix"));
    }
    let n = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let payload = &bytes[4..];
    if n != payload.len() {
        return Err(cw(&format!(
            "protection stream: declared {n} bytes, carries {}",
            payload.len()
        )));
    }
    if n % 2 != 0 {
        return Err(cw("protection stream: odd byte count (not UTF-16LE)"));
    }
    let units: Vec<u16> = payload
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let mut text = String::from_utf16_lossy(&units);
    while text.ends_with('\u{0}') {
        text.pop();
    }
    Ok(text)
}

// ---------------------------------------------------------------------------
// CaseWareProtectionInfo
// ---------------------------------------------------------------------------

/// Decoded `CaseWareProtectionInfo` JSON payload (the engagement access list).
///
/// Field names and order are the corpus form:
/// `AllowAutoGlobalGroups`, `adgroups`, `groups`, `users`. Only
/// `AllowAutoGlobalGroups` is a number (`0`/`1`); the rest are single-space
/// separated id strings. An unknown key is a format error (a shape change must
/// be visible, not silently dropped).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProtectionInfo {
    /// COM `sec.AllowAutoAssignedGlobalGroups`.
    pub allow_auto_global_groups: bool,
    /// AD group ids (empty in the corpus).
    pub adgroups: Vec<String>,
    /// Global security-group ids (empty in the corpus).
    pub groups: Vec<String>,
    /// User ids granted engagement access.
    pub users: Vec<String>,
}

const INFO_KEYS: [&str; 4] = ["AllowAutoGlobalGroups", "adgroups", "groups", "users"];

fn split_ids(text: &str) -> Vec<String> {
    text.split_whitespace().map(|s| s.to_string()).collect()
}

fn json_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

impl ProtectionInfo {
    /// Parse the JSON text (trailing CR/LF tolerated; the corpus has `\n`).
    pub fn parse(text: &str) -> Result<ProtectionInfo> {
        let body = text.trim_end_matches(['\n', '\r']);
        let doc = json::parse(body).map_err(|e| cw(&format!("CaseWareProtectionInfo: {e}")))?;
        let obj = doc
            .as_obj()
            .ok_or_else(|| cw("CaseWareProtectionInfo: payload is not a JSON object"))?;
        if obj.len() != INFO_KEYS.len() || !INFO_KEYS.iter().all(|k| obj.iter().any(|(n, _)| n == k))
        {
            let keys: Vec<&str> = obj.iter().map(|(k, _)| k.as_str()).collect();
            return Err(cw(&format!(
                "CaseWareProtectionInfo: unexpected key set {keys:?}"
            )));
        }
        let flag = doc
            .get("AllowAutoGlobalGroups")
            .ok_or_else(|| cw("CaseWareProtectionInfo: AllowAutoGlobalGroups missing"))?;
        let allow_auto_global_groups = match flag {
            json::Json::Bool(b) => *b,
            other => match other.as_f64() {
                Some(v) if v == 0.0 => false,
                Some(v) if v == 1.0 => true,
                _ => {
                    return Err(cw(
                        "CaseWareProtectionInfo: AllowAutoGlobalGroups is not 0/1",
                    ))
                }
            },
        };
        let string_field = |name: &str| -> Result<Vec<String>> {
            let v = doc
                .get(name)
                .ok_or_else(|| cw(&format!("CaseWareProtectionInfo: {name} missing")))?;
            match v {
                json::Json::Str(s) => Ok(split_ids(s)),
                _ => Err(cw(&format!("CaseWareProtectionInfo: {name} is not a string"))),
            }
        };
        Ok(ProtectionInfo {
            allow_auto_global_groups,
            adgroups: string_field("adgroups")?,
            groups: string_field("groups")?,
            users: string_field("users")?,
        })
    }

    /// Emit the corpus payload text (JSON + `\n`, no NUL — the codec adds it).
    pub fn to_text(&self) -> String {
        format!(
            "{{\"AllowAutoGlobalGroups\":{},\"adgroups\":\"{}\",\"groups\":\"{}\",\"users\":\"{}\"}}\n",
            if self.allow_auto_global_groups { 1 } else { 0 },
            json_escape(&self.adgroups.join(" ")),
            json_escape(&self.groups.join(" ")),
            json_escape(&self.users.join(" ")),
        )
    }

    /// Full stream payload (codec applied).
    pub fn to_stream(&self) -> Vec<u8> {
        encode_utf16_stream(&self.to_text())
    }

    /// True when the payload names at least one user or group.
    pub fn is_empty(&self) -> bool {
        self.users.is_empty() && self.groups.is_empty() && self.adgroups.is_empty()
    }
}

/// Decode `CaseWareProtectionInfo`; `None` is the protection-OFF marker.
pub fn decode_protection_stream(bytes: &[u8]) -> Result<Option<ProtectionInfo>> {
    let text = decode_utf16_stream(bytes)?;
    if text.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(ProtectionInfo::parse(&text)?))
}

/// Encode `CaseWareProtectionInfo`; `None` emits the 6-byte OFF marker.
pub fn encode_protection_stream(info: Option<&ProtectionInfo>) -> Vec<u8> {
    match info {
        Some(info) => info.to_stream(),
        None => encode_utf16_stream(""),
    }
}

// ---------------------------------------------------------------------------
// CaseWareUserlist
// ---------------------------------------------------------------------------

/// Decode `CaseWareUserlist`: the same ids as `ProtectionInfo.users` with the
/// ` sup` trailer (empty list ⇒ the bare NUL marker, as in the corpus).
pub fn decode_userlist_stream(bytes: &[u8]) -> Result<Vec<String>> {
    let text = decode_utf16_stream(bytes)?;
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let body = text.strip_suffix(USERLIST_TRAILER).ok_or_else(|| {
        cw(&format!(
            "CaseWareUserlist: payload does not end in {USERLIST_TRAILER:?}"
        ))
    })?;
    if body.trim().is_empty() {
        return Err(cw("CaseWareUserlist: trailer without a user list"));
    }
    Ok(split_ids(body))
}

/// Encode `CaseWareUserlist` (an empty list emits the 6-byte OFF marker).
pub fn encode_userlist_stream(users: &[String]) -> Vec<u8> {
    if users.is_empty() {
        return encode_utf16_stream("");
    }
    let mut text = users.join(" ");
    text.push_str(USERLIST_TRAILER);
    encode_utf16_stream(&text)
}

// ---------------------------------------------------------------------------
// ProtectionStore: the engagement access list
// ---------------------------------------------------------------------------

/// The engagement's protection state, read from the two `.ac` streams.
///
/// `Protection` itself is derived from the presence of the JSON payload: the
/// OFF form is the empty marker, the ON form is the populated JSON (spec §3,
/// confirmed on the frozen packages — hull2024 ON, corp2026/mwgh2025 OFF).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProtectionStore {
    info: Option<ProtectionInfo>,
    /// `CaseWareUserlist` ids, carried verbatim (see [`Self::lists_agree`]).
    userlist: Vec<String>,
}

impl ProtectionStore {
    /// Parse the streams out of any container that carries them (`.ac_`
    /// package or an expanded hub `.ac`). A container without the streams is
    /// read as protection-OFF (the corpus default).
    pub fn from_container(bytes: &[u8]) -> Result<ProtectionStore> {
        let cfb = Cfb::parse(bytes)?;
        let info = match cfb.stream(S_PROTECTION) {
            Ok(stream) => decode_protection_stream(&stream)?,
            Err(_) => None,
        };
        let userlist = match cfb.stream(S_USERLIST) {
            Ok(stream) => decode_userlist_stream(&stream)?,
            Err(_) => Vec::new(),
        };
        Ok(ProtectionStore { info, userlist })
    }

    pub fn from_path(path: impl AsRef<Path>) -> Result<ProtectionStore> {
        let bytes = fs::read(path.as_ref())?;
        ProtectionStore::from_container(&bytes)
    }

    /// Open the engagement's protection state and report which container it
    /// came from.
    ///
    /// `<stem>.ac_` (the package) is preferred; an expanded hub `<stem>.ac` is
    /// accepted when it is a real CFB. The 5-byte stub CW leaves after
    /// compressing carries no streams, so that case is an explicit error rather
    /// than a silent protection-OFF.
    pub fn from_dir(dir: impl AsRef<Path>, stem: &str) -> Result<(PathBuf, ProtectionStore)> {
        let dir = dir.as_ref();
        let want_pkg = format!("{stem}.ac_").to_ascii_lowercase();
        let want_hub = format!("{stem}.ac").to_ascii_lowercase();
        let mut hub: Option<PathBuf> = None;
        if let Ok(rd) = fs::read_dir(dir) {
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if name == want_pkg {
                    let path = entry.path();
                    let store = ProtectionStore::from_path(&path)?;
                    return Ok((path, store));
                }
                if name == want_hub {
                    hub = Some(entry.path());
                }
            }
        }
        let Some(hub) = hub else {
            return Err(cw(&format!(
                "missing hub {stem}.ac_ or {stem}.ac in {}",
                dir.display()
            )));
        };
        let bytes = fs::read(&hub)?;
        if bytes.len() < 512 || !bytes.starts_with(&cw_core::cfb::SIGNATURE) {
            return Err(cw(&format!(
                "{} is the compressed-engagement stub ({len} bytes); the protection streams live in `{stem}.ac_`",
                hub.display(),
                len = bytes.len()
            )));
        }
        let store = ProtectionStore::from_container(&bytes)?;
        Ok((hub, store))
    }

    /// COM `sec.Protection`.
    pub fn protection(&self) -> bool {
        self.info.is_some()
    }

    /// COM `ProtectionSetup.Users` / the stream `users` entry.
    pub fn users(&self) -> &[String] {
        self.info.as_ref().map(|i| i.users.as_slice()).unwrap_or(&[])
    }

    /// The `CaseWareUserlist` ids, verbatim.
    pub fn userlist(&self) -> &[String] {
        &self.userlist
    }

    /// COM `ProtectionSetup.Groups` / `sec.GlobalGroups` intersection.
    pub fn groups(&self) -> &[String] {
        self.info.as_ref().map(|i| i.groups.as_slice()).unwrap_or(&[])
    }

    /// AD group ids from the JSON payload (empty in the corpus).
    pub fn adgroups(&self) -> &[String] {
        self.info.as_ref().map(|i| i.adgroups.as_slice()).unwrap_or(&[])
    }

    /// COM `sec.AllowAutoAssignedGlobalGroups` (false when protection is off).
    pub fn allow_auto_global_groups(&self) -> bool {
        self.info.as_ref().map(|i| i.allow_auto_global_groups).unwrap_or(false)
    }

    pub fn info(&self) -> Option<&ProtectionInfo> {
        self.info.as_ref()
    }

    /// True when both streams carry the same id list (the corpus state).
    pub fn lists_agree(&self) -> bool {
        self.users() == self.userlist.as_slice()
    }

    /// Re-encoded `CaseWareProtectionInfo` payload (byte-stable with the
    /// corpus when the inputs came from it).
    pub fn info_stream(&self) -> Vec<u8> {
        encode_protection_stream(self.info.as_ref())
    }

    /// Re-encoded `CaseWareUserlist` payload.
    pub fn userlist_stream(&self) -> Vec<u8> {
        encode_userlist_stream(self.users())
    }

    /// `SetProtection(enable, user, pwd)` (spec §3): validation + in-memory
    /// mutation only. Preview with [`Self::encode_into`], commit with
    /// [`Self::save_to`].
    ///
    /// The password is accepted and discarded — the digest is not decoded (see
    /// the module docs), and the spec's own probe passes an empty one.
    pub fn apply(&mut self, req: &ProtectionRequest) -> Result<ProtectionChange> {
        if req.user.trim().is_empty() {
            return Err(cw(ERR_MANDATORY_ID));
        }
        if !req.is_admin {
            return Err(cw(ERR_ADMIN_REQUIRED));
        }
        let _ = &req.password;
        if req.enable {
            self.info = Some(ProtectionInfo {
                allow_auto_global_groups: req.allow_auto_global_groups,
                adgroups: req.adgroups.clone(),
                groups: req.groups.clone(),
                users: req.users.clone(),
            });
            self.userlist = req.users.clone();
            return Ok(ProtectionChange::Enabled {
                users: req.users.len(),
                groups: req.groups.len(),
                adgroups: req.adgroups.len(),
            });
        }
        self.info = None;
        self.userlist.clear();
        Ok(ProtectionChange::Disabled)
    }

    /// `CanUserAccess(user, level)` as far as it is decoded.
    ///
    /// - protection OFF ⇒ [`AccessVerdict::Unresolved`]: the spec's probes were
    ///   taken on a protected file and with no access list the question has no
    ///   recorded answer;
    /// - user not in the access list ⇒ [`AccessVerdict::Denied`] (spec §3);
    /// - listed user at level 0 ⇒ [`AccessVerdict::Denied`] (spec §3);
    /// - listed user above level 0 ⇒ [`AccessVerdict::Unresolved`] (level
    ///   semantics undecoded, spec §5).
    pub fn can_user_access(&self, user: &str, level: u8) -> AccessVerdict {
        let Some(info) = self.info.as_ref() else {
            return AccessVerdict::Unresolved;
        };
        if !info.users.iter().any(|u| u == user) {
            return AccessVerdict::Denied;
        }
        if level == 0 {
            return AccessVerdict::Denied;
        }
        AccessVerdict::Unresolved
    }

    /// Rebuild a container with these two streams replaced: every other stream
    /// (the `CaseWareDocument` payload included) is carried byte-for-byte.
    pub fn encode_into(&self, base: &[u8]) -> Result<Vec<u8>> {
        let cfb = Cfb::parse(base)?;
        let mut streams: Vec<(String, Vec<u8>)> = Vec::new();
        let (mut seen_info, mut seen_list) = (false, false);
        for entry in cfb.entries()? {
            if entry.kind != EntryKind::Stream {
                continue;
            }
            let name = entry.name();
            if entry.path.len() != 1 {
                return Err(cw(&format!(
                    "protection: nested stream {name:?} cannot be rebuilt by the flat-root profile"
                )));
            }
            if name == S_PROTECTION {
                streams.push((name, self.info_stream()));
                seen_info = true;
            } else if name == S_USERLIST {
                streams.push((name, self.userlist_stream()));
                seen_list = true;
            } else {
                streams.push((name.clone(), cfb.stream(&name)?));
            }
        }
        if !seen_info {
            streams.push((S_PROTECTION.to_string(), self.info_stream()));
        }
        if !seen_list {
            streams.push((S_USERLIST.to_string(), self.userlist_stream()));
        }
        let mut builder = CfbBuilder::new();
        for (name, data) in streams {
            builder.add(name, data);
        }
        builder.build()
    }

    /// Commit the container at `base_path` atomically; returns whether the
    /// bytes changed (an unchanged save is a true no-op, `transact` law).
    pub fn save_to(&self, base_path: impl AsRef<Path>) -> Result<bool> {
        let path = base_path.as_ref();
        let base = fs::read(path)?;
        let bytes = self.encode_into(&base)?;
        commit_if_changed(path, &bytes)
    }
}

/// COM `sec.CanUserAccess` result as far as the spec decodes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessVerdict {
    /// Recorded refusal (not listed, or level 0).
    Denied,
    /// The question is not answerable from the decoded surface.
    Unresolved,
}

/// One `SetProtection` request. `is_admin` is the `IsUserAdmin(user)` answer
/// from the machine-wide directory — the `EC` roster has no `Administrator`
/// column (see the module docs), so it cannot be read from the file.
#[derive(Clone, Debug)]
pub struct ProtectionRequest {
    pub enable: bool,
    /// Acting identity (`SetProtection(user,pwd)`).
    pub user: String,
    /// Accepted and discarded: the password digest is not decoded.
    pub password: String,
    /// `sec.IsUserAdmin(user)`.
    pub is_admin: bool,
    /// Access list materialized when enabling (`ProtectionSetup.Users`).
    pub users: Vec<String>,
    /// Global security-group ids (`groups` entry).
    pub groups: Vec<String>,
    /// AD group ids (`adgroups` entry).
    pub adgroups: Vec<String>,
    /// `AllowAutoGlobalGroups` entry on a materialized list. The only
    /// protection-ON corpus sample (hull2024) carries `1`.
    pub allow_auto_global_groups: bool,
}

impl ProtectionRequest {
    /// `SetProtection(true, user, pwd)` with the hull2024 ON defaults.
    pub fn enable(user: impl Into<String>, is_admin: bool) -> ProtectionRequest {
        ProtectionRequest {
            enable: true,
            user: user.into(),
            password: String::new(),
            is_admin,
            users: Vec::new(),
            groups: Vec::new(),
            adgroups: Vec::new(),
            allow_auto_global_groups: true,
        }
    }

    /// `SetProtection(false, user, pwd)`: no list is supplied, because turning
    /// protection off empties both streams regardless of what the caller passes.
    pub fn disable(user: impl Into<String>, is_admin: bool) -> ProtectionRequest {
        ProtectionRequest {
            enable: false,
            users: Vec::new(),
            ..ProtectionRequest::enable(user, is_admin)
        }
    }

    pub fn with_users(mut self, users: &[&str]) -> ProtectionRequest {
        self.users = users.iter().map(|u| (*u).to_string()).collect();
        self
    }

    pub fn with_groups(mut self, groups: &[&str]) -> ProtectionRequest {
        self.groups = groups.iter().map(|g| (*g).to_string()).collect();
        self
    }

    pub fn with_adgroups(mut self, adgroups: &[&str]) -> ProtectionRequest {
        self.adgroups = adgroups.iter().map(|g| (*g).to_string()).collect();
        self
    }

    pub fn with_allow_auto_global_groups(mut self, allow: bool) -> ProtectionRequest {
        self.allow_auto_global_groups = allow;
        self
    }

    /// Accepted and discarded — the digest is undecoded (module docs). This
    /// builder exists so a caller can state the credential it was given, not so
    /// the engine can check it.
    pub fn with_password(mut self, password: impl Into<String>) -> ProtectionRequest {
        self.password = password.into();
        self
    }
}

/// What a [`ProtectionStore::apply`] changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtectionChange {
    /// `Protection` turned on and the access list materialized.
    Enabled { users: usize, groups: usize, adgroups: usize },
    /// `Protection` turned off (both streams return to the empty marker).
    Disabled,
}

// ---------------------------------------------------------------------------
// Role configuration (ClientOptions)
// ---------------------------------------------------------------------------

/// COM `ClientOptions.Role(1..8)` names (spec §1). Delegates to the signoff
/// ladder so there is one role table in the crate.
pub fn role_names() -> &'static [&'static str] {
    &signoff::ROLE_NAMES
}

/// COM `ClientOptions.Role(i)` name for level `1..=8`.
pub fn role_name(level: u8) -> Result<&'static str> {
    signoff::role_name(level)
}

// ---------------------------------------------------------------------------
// Per-document role/flag cells (SH)
// ---------------------------------------------------------------------------

/// The `SH` role/permission cells that have no owner in [`crate::signoff`].
///
/// All three are empty on every live row of the frozen corpora; `SIGN` and
/// `LOCKDOWN` are decoded flags (spec §2/§3). `ROLESET` names a role set from
/// the undecoded engagement options block, so the name is carried verbatim and
/// no registry lookup is invented.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocumentRoles {
    pub guid: String,
    /// `SH.ROLESET` (15 chars; empty in the corpus).
    pub roleset: String,
    /// `SH.ACCESS` (4 chars; empty in the corpus).
    pub access: String,
    /// `SH.TASKUSER` (22 chars; empty in the corpus).
    pub task_user: String,
    /// `SH.SIGN='Y'` (COM `doc.Sign`).
    pub sign: bool,
    /// `SH.LOCKDOWN='Y'` (COM `doc.Lockdown`; template default on review docs).
    pub lockdown: bool,
}

fn sh_cell(t: &Table, row: usize, name: &str) -> Result<String> {
    let raw = t
        .cell(row, name)
        .ok_or_else(|| cw(&format!("SH: missing column {name}")))?;
    Ok(raw
        .iter()
        .map(|&b| b as char)
        .collect::<String>()
        .trim_end_matches([' ', '\u{0}'])
        .to_string())
}

fn sh_flag(t: &Table, row: usize, name: &str) -> Result<bool> {
    Ok(sh_cell(t, row, name)?.eq_ignore_ascii_case("Y"))
}

/// Decode the role/flag cells of one physical `SH` row.
pub fn document_roles(sh: &Table, row: usize) -> Result<DocumentRoles> {
    Ok(DocumentRoles {
        guid: sh_cell(sh, row, "CWGUID")?,
        roleset: sh_cell(sh, row, "ROLESET")?,
        access: sh_cell(sh, row, "ACCESS")?,
        task_user: sh_cell(sh, row, "TASKUSER")?,
        sign: sh_flag(sh, row, "SIGN")?,
        lockdown: sh_flag(sh, row, "LOCKDOWN")?,
    })
}

/// One local signon account (`sec.Users` / `ICWUser`, spec §1/§2).
///
/// The probe read 6 accounts (`''`, `DW`, `E109583`, `E109585`, `E120235`,
/// `RSM`) from the machine-wide store — that store has no engagement file, so
/// this is an in-memory mirror and nothing is written into the engagement. The
/// empty first entry is the probe's own placeholder and is refused here (an id
/// is mandatory).
/// Only `UserId`/`CanUserAccess` are named; `ShortName`/`FullName` are carried
/// because the COM type exposes them, but no value was probed for either and
/// this module never invents one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UserAccount {
    /// COM `UserId` (the `HI.USER` id in the corpus: `RSM`, `E120235`, …).
    pub user_id: String,
    /// COM `ShortName`, carried verbatim (unobserved in the probe).
    pub short_name: String,
    /// COM `FullName`, carried verbatim (unobserved in the probe).
    pub full_name: String,
}

impl UserAccount {
    /// `Users.Add(id)`: the id is mandatory (spec §1's `Id is a mandatory
    /// field.` is the shared collection rule).
    pub fn new(user_id: impl Into<String>) -> Result<UserAccount> {
        let user_id = user_id.into();
        if user_id.trim().is_empty() {
            return Err(cw(ERR_MANDATORY_ID));
        }
        Ok(UserAccount { user_id: user_id.trim().to_string(), ..Default::default() })
    }

    /// COM `CanUserAccess(level)` against the engagement access list: the same
    /// law as [`ProtectionStore::can_user_access`], so the two cannot drift.
    pub fn can_user_access(&self, store: &ProtectionStore, level: u8) -> AccessVerdict {
        store.can_user_access(&self.user_id, level)
    }
}

/// In-memory `sec.Users` mirror (`GetIfExists`/`Add`/`Remove`/`Item(i)`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SecurityUsers {
    users: Vec<UserAccount>,
}

impl SecurityUsers {
    pub fn new() -> SecurityUsers {
        SecurityUsers::default()
    }

    /// `Users.Add(id)`.
    pub fn add(&mut self, user_id: &str) -> Result<usize> {
        let user = UserAccount::new(user_id)?;
        if self.get(&user.user_id).is_some() {
            return Err(cw(&format!("User {} already exists", user.user_id)));
        }
        self.users.push(user);
        Ok(self.users.len() - 1)
    }

    /// `Users.GetIfExists(id)`.
    pub fn get(&self, user_id: &str) -> Option<&UserAccount> {
        self.users.iter().find(|u| u.user_id.eq_ignore_ascii_case(user_id.trim()))
    }

    pub fn get_mut(&mut self, user_id: &str) -> Option<&mut UserAccount> {
        self.users.iter_mut().find(|u| u.user_id.eq_ignore_ascii_case(user_id.trim()))
    }

    /// `Users.Remove(id)`.
    pub fn remove(&mut self, user_id: &str) -> Result<()> {
        let before = self.users.len();
        self.users.retain(|u| !u.user_id.eq_ignore_ascii_case(user_id.trim()));
        if self.users.len() == before {
            return Err(cw(&format!("User {user_id} not found")));
        }
        Ok(())
    }

    /// 1-based `Users.Item(i)` (VT_BSTR id, per the collection law).
    pub fn item(&self, index: usize) -> Option<&UserAccount> {
        index.checked_sub(1).and_then(|i| self.users.get(i))
    }

    pub fn users(&self) -> &[UserAccount] {
        &self.users
    }

    pub fn len(&self) -> usize {
        self.users.len()
    }

    pub fn is_empty(&self) -> bool {
        self.users.is_empty()
    }
}

/// `ICWProtectionSetup` view over one engagement container: the admin-gated
/// `Users`/`Groups` pair (spec §1).
///
/// The COM object is constructed with credentials (`app.ProtectionSetup(user,
/// pwd)`), and the spec's probe shows an empty credential pair raising
/// [`ERR_MANDATORY_ID`] *before* any collection access. Because the password
/// digest is undecoded, the constructor here takes the same gate as
/// `SetProtection` — an id plus a directory-verified administrator — instead of
/// checking a password it cannot check.
#[derive(Clone, Debug)]
pub struct ProtectionSetup {
    /// The engagement's access list (`ProtectionSetup.Users`).
    pub access: ProtectionStore,
    /// The global group mirror (`ProtectionSetup.Groups`).
    pub groups: SecurityGroups,
    /// The local signon mirror (`sec.Users`).
    pub users: SecurityUsers,
}

impl ProtectionSetup {
    /// `app.ProtectionSetup(user, pwd)` for an engagement container.
    pub fn open(
        container: &[u8],
        user: &str,
        is_admin: bool,
    ) -> Result<ProtectionSetup> {
        if user.trim().is_empty() {
            return Err(cw(ERR_MANDATORY_ID));
        }
        if !is_admin {
            return Err(cw(ERR_ADMIN_REQUIRED));
        }
        Ok(ProtectionSetup {
            access: ProtectionStore::from_container(container)?,
            groups: SecurityGroups::new(),
            users: SecurityUsers::new(),
        })
    }

    /// `ProtectionSetup.Users`: the engagement access list ids.
    pub fn access_ids(&self) -> &[String] {
        self.access.users()
    }

    /// `CanUserAccess` for one listed id (level semantics stay undecoded).
    pub fn can_user_access(&self, user: &str, level: u8) -> AccessVerdict {
        self.access.can_user_access(user, level)
    }
}

/// `doc.Lockdown` put under the engagement's lock state.
///
/// The rule is one-way (spec §1): once `client.Lockdown()` has run, the
/// per-document flag can no longer be changed and CW raises
/// [`ERR_DOC_LOCKDOWN_LOCKED`]. The un-locked path delegates to
/// [`SignoffBook::set_lockdown`] so there is one `SH` writer.
pub fn set_document_lockdown(
    book: &mut SignoffBook,
    guid: &str,
    locked: bool,
    state: &LockdownState,
) -> Result<()> {
    if state.locked {
        return Err(cw(ERR_DOC_LOCKDOWN_LOCKED));
    }
    book.set_lockdown(guid, locked)
}

// ---------------------------------------------------------------------------
// Employees: the EC roster table
// ---------------------------------------------------------------------------

/// One `EC` field descriptor (physical order, inventory `dbf-schema.tsv`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EcField {
    pub name: &'static str,
    /// dBase type letter (`C`/`D`/`L`/`M`).
    pub kind: u8,
    pub len: u8,
    pub dec: u8,
}

const fn f(name: &'static str, kind: u8, len: u8, dec: u8) -> EcField {
    EcField { name, kind, len, dec }
}

/// Physical `EC` field count (identical in all three frozen corpora).
pub const EC_FIELD_COUNT: usize = 26;
/// Physical `EC` record length, deletion flag included.
pub const EC_RECORD_LEN: usize = 531;

/// Canonical `EC` schema, physical order (verified field-for-field against
/// corp2026, hull2024 and mwgh2025; all three carry 0 rows).
pub static EC_SCHEMA: [EcField; EC_FIELD_COUNT] = [
    f("EMP_NUM", b'C', 20, 0),
    f("PRE_NAME", b'C', 20, 0),
    f("FRST_NAME", b'C', 20, 0),
    f("MID_NAME", b'C', 20, 0),
    f("LAST_NAME", b'C', 20, 0),
    f("POST_NAME", b'C', 20, 0),
    f("S_DATE", b'D', 8, 0),
    f("SIN", b'C', 11, 0),
    f("ADDR1", b'C', 40, 0),
    f("ADDR2", b'C', 40, 0),
    f("CITY", b'C', 25, 0),
    f("PROV", b'C', 20, 0),
    f("COUNTRY", b'C', 3, 0),
    f("PT_CODE", b'C', 10, 0),
    f("PHONE1", b'C', 30, 0),
    f("PHONE2", b'C', 30, 0),
    f("EMAIL", b'C', 100, 0),
    f("IDATE", b'D', 8, 0),
    f("EXINFO", b'M', 10, 0),
    f("TGUID", b'C', 22, 0),
    f("BOX13STAT", b'L', 1, 0),
    f("BOX13RETIR", b'L', 1, 0),
    f("BOX13SICK", b'L', 1, 0),
    f("CWGUID", b'C', 22, 0),
    f("CWDATE", b'C', 14, 0),
    f("CREATEDON", b'C', 14, 0),
];

/// True when the table is the canonical 26-column `EC` layout.
pub fn ec_schema_matches(t: &Table) -> bool {
    t.fields.len() == EC_FIELD_COUNT
        && t.record_len as usize == EC_RECORD_LEN
        && EC_SCHEMA.iter().zip(&t.fields).all(|(e, f)| {
            f.name.eq_ignore_ascii_case(e.name) && f.kind == e.kind && f.len == e.len && f.dec == e.dec
        })
}

fn check_ec_schema(t: &Table) -> Result<()> {
    if !ec_schema_matches(t) {
        return Err(cw(&format!(
            "EC: {} fields / {} bytes, expected {EC_FIELD_COUNT} / {EC_RECORD_LEN} (canonical roster)",
            t.fields.len(),
            t.record_len
        )));
    }
    Ok(())
}

/// Empty canonical `EC` table (865-byte header, 531-byte records), for an
/// engagement that has no roster — the corpus normal form. dBase type letters,
/// widths and the CW-marked version byte match the frozen tables; the header
/// date is zero (a synthesized table has no last-update).
pub fn blank_ec_table() -> Table {
    let cols: Vec<(&str, u8, u8, u8)> =
        EC_SCHEMA.iter().map(|f| (f.name, f.kind, f.len, f.dec)).collect();
    let mut t = crate::entities::blank_table(&cols);
    t.version = EC_FILE_VERSION;
    t.raw_header[0] = EC_FILE_VERSION;
    t
}

/// One decoded `EC` row (the COM `ICWEmployee` subset the table actually
/// carries, the rest verbatim).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EmployeeRecord {
    pub row: usize,
    /// COM `UserId` (`EMP_NUM`; the spec's `Employees.Add('TST01')` id).
    pub user_id: String,
    /// COM `PreName` (`PRE_NAME`).
    pub pre_name: String,
    /// COM `FirstName` (`FRST_NAME`).
    pub first_name: String,
    /// `MID_NAME` (no established COM property).
    pub middle_name: String,
    /// COM `LastName` (`LAST_NAME`).
    pub last_name: String,
    /// `POST_NAME` (post-nominal letters; no established COM property).
    pub post_name: String,
    /// `S_DATE` (`yyyymmdd`).
    pub start_date: String,
    pub sin: String,
    pub addr1: String,
    pub addr2: String,
    pub city: String,
    pub prov: String,
    pub country: String,
    pub postal_code: String,
    pub phone1: String,
    pub phone2: String,
    /// COM `EmailAddress` (`EMAIL`).
    pub email: String,
    /// `IDATE` (`yyyymmdd`).
    pub idate: String,
    /// `EXINFO` memo pointer, carried verbatim (payload is the FPT lane).
    pub exinfo: String,
    pub tguid: String,
    pub box13_stat: String,
    pub box13_retire: String,
    pub box13_sick: String,
    /// COM `ItemIdentifier` equivalent (`CWGUID`, family A — the corpus never
    /// populates EC, so the allocator's documented fallback applies).
    pub cwguid: String,
    pub cw_date: String,
    pub created_on: String,
    /// Physical tombstone (`*`) flag.
    pub deleted: bool,
}

impl EmployeeRecord {
    /// Decode one row by column name (schema drift is rejected by
    /// [`EcTable::from_table`] up front).
    pub fn decode(table: &Table, row: usize) -> Result<EmployeeRecord> {
        Ok(EmployeeRecord {
            row,
            user_id: sh_cell(table, row, "EMP_NUM")?,
            pre_name: sh_cell(table, row, "PRE_NAME")?,
            first_name: sh_cell(table, row, "FRST_NAME")?,
            middle_name: sh_cell(table, row, "MID_NAME")?,
            last_name: sh_cell(table, row, "LAST_NAME")?,
            post_name: sh_cell(table, row, "POST_NAME")?,
            start_date: sh_cell(table, row, "S_DATE")?,
            sin: sh_cell(table, row, "SIN")?,
            addr1: sh_cell(table, row, "ADDR1")?,
            addr2: sh_cell(table, row, "ADDR2")?,
            city: sh_cell(table, row, "CITY")?,
            prov: sh_cell(table, row, "PROV")?,
            country: sh_cell(table, row, "COUNTRY")?,
            postal_code: sh_cell(table, row, "PT_CODE")?,
            phone1: sh_cell(table, row, "PHONE1")?,
            phone2: sh_cell(table, row, "PHONE2")?,
            email: sh_cell(table, row, "EMAIL")?,
            idate: sh_cell(table, row, "IDATE")?,
            exinfo: sh_cell(table, row, "EXINFO")?,
            tguid: sh_cell(table, row, "TGUID")?,
            box13_stat: sh_cell(table, row, "BOX13STAT")?,
            box13_retire: sh_cell(table, row, "BOX13RETIR")?,
            box13_sick: sh_cell(table, row, "BOX13SICK")?,
            cwguid: sh_cell(table, row, "CWGUID")?,
            cw_date: sh_cell(table, row, "CWDATE")?,
            created_on: sh_cell(table, row, "CREATEDON")?,
            deleted: table.is_deleted(row),
        })
    }

    /// `PreName FirstName LastName` with blanks collapsed (display helper).
    pub fn full_name(&self) -> String {
        [self.pre_name.as_str(), self.first_name.as_str(), self.last_name.as_str()]
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// `Employee.Administrator`: no `EC` column is named for it, and the only
    /// column that could hide it is the undecoded `EXINFO` memo (module docs), so
    /// this is never a file fact here.
    pub fn administrator(&self) -> Option<bool> {
        None
    }
}

/// `EC.dbf` reader/writer: the per-engagement employee roster.
///
/// The corpus normal form is an empty roster — `sec.Employees` is the
/// machine-wide store (spec §2) — so this type's job is an honest, byte-stable
/// row model for a file that CW itself would populate.
#[derive(Clone, Debug)]
pub struct EcTable {
    table: Table,
    records: Vec<EmployeeRecord>,
}

impl EcTable {
    pub fn from_bytes(bytes: &[u8]) -> Result<EcTable> {
        EcTable::from_table(Table::parse(bytes)?)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<EcTable> {
        EcTable::from_bytes(&fs::read(path)?)
    }

    pub fn from_table(table: Table) -> Result<EcTable> {
        check_ec_schema(&table)?;
        let mut records = Vec::with_capacity(table.count());
        for row in 0..table.count() {
            records.push(EmployeeRecord::decode(&table, row)?);
        }
        Ok(EcTable { table, records })
    }

    /// Every physical row, tombstones included.
    pub fn records(&self) -> &[EmployeeRecord] {
        &self.records
    }

    /// Live rows only.
    pub fn live(&self) -> Vec<&EmployeeRecord> {
        self.records.iter().filter(|r| !r.deleted).collect()
    }

    pub fn count(&self) -> usize {
        self.records.iter().filter(|r| !r.deleted).count()
    }

    pub fn find(&self, user_id: &str) -> Option<&EmployeeRecord> {
        self.records
            .iter()
            .find(|r| !r.deleted && r.user_id.trim() == user_id.trim())
    }

    /// `Employees.Add(userId)`: append a roster row. `EMP_NUM` is the id and is
    /// mandatory (spec §1); the row id and stamps are allocated here. Every
    /// other column stays blank — CW's own roster never materializes values
    /// this module could copy, so nothing is invented.
    pub fn add_employee(
        &mut self,
        user_id: &str,
        stamp: &str,
        rng: &mut Rng,
    ) -> Result<usize> {
        if user_id.trim().is_empty() {
            return Err(cw(ERR_MANDATORY_ID));
        }
        if self.find(user_id).is_some() {
            return Err(cw(&format!("Employee {user_id} already exists")));
        }
        let guid = CwGuid::allocate_for_table("EC", rng).encode();
        let row = self.table.records.len();
        let mut rec = vec![b' '; self.table.record_len as usize];
        rec[0] = b' ';
        self.table.records.push(rec);
        write_cell(&mut self.table, row, "EMP_NUM", user_id.trim())?;
        write_cell(&mut self.table, row, "CWGUID", &guid)?;
        write_cell(&mut self.table, row, "CWDATE", stamp)?;
        write_cell(&mut self.table, row, "CREATEDON", stamp)?;
        self.records.push(EmployeeRecord::decode(&self.table, row)?);
        Ok(row)
    }

    /// Tombstone one row (CW's table delete).
    pub fn remove(&mut self, row: usize) -> Result<()> {
        let rec = self
            .table
            .records
            .get_mut(row)
            .ok_or_else(|| cw(&format!("EC: row {row} out of range")))?;
        rec[0] = b'*';
        if let Some(r) = self.records.get_mut(row) {
            r.deleted = true;
        }
        Ok(())
    }

    pub fn encode(&self) -> Vec<u8> {
        self.table.encode()
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        self.table.write_path(path)
    }

    pub fn table(&self) -> &Table {
        &self.table
    }
}

// ---------------------------------------------------------------------------
// Groups (global store mirror)
// ---------------------------------------------------------------------------

/// `Group.CaseViewAccessLevel` (0..=3). Only `3` is named in the evidence
/// (`ADMN` = Administrators); the 0..2 names are undecoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CaseViewAccess(u8);

impl CaseViewAccess {
    pub fn parse(value: u8) -> Result<CaseViewAccess> {
        if value <= ADMIN_ACCESS_LEVEL {
            Ok(CaseViewAccess(value))
        } else {
            Err(cw(&format!(
                "CaseViewAccessLevel {value} outside 0..={ADMIN_ACCESS_LEVEL}"
            )))
        }
    }

    /// The `ADMN` level (3).
    pub fn admin() -> CaseViewAccess {
        CaseViewAccess(ADMIN_ACCESS_LEVEL)
    }

    pub fn level(self) -> u8 {
        self.0
    }

    pub fn is_admin(self) -> bool {
        self.0 == ADMIN_ACCESS_LEVEL
    }
}

impl Default for CaseViewAccess {
    fn default() -> CaseViewAccess {
        CaseViewAccess(0)
    }
}

/// One security-group record (`sec.Groups` / `Group`, spec §1).
///
/// The global store's on-disk form is not part of the corpus, so this is an
/// in-memory mirror: nothing is written into the engagement. `AccessRights`
/// kinds are undecoded and carried as opaque `(kind, bool)` pairs the caller
/// supplies — they are never synthesized.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupRecord {
    /// Group id, ≤ [`MAX_GROUP_ID_LEN`] chars (`ADMN` is the administrator).
    pub id: String,
    /// COM `Name`.
    pub name: String,
    /// COM `Description`.
    pub description: String,
    /// COM `CaseViewAccessLevel`.
    pub access_level: CaseViewAccess,
    /// COM `AllAccessRights(bool)`.
    pub all_access_rights: bool,
    /// COM `AccessRights(kind,bool)` pairs, opaque.
    pub rights: Vec<(String, bool)>,
    /// COM `AssignedEmployees` (user ids).
    pub employees: Vec<String>,
}

impl GroupRecord {
    pub fn is_admin(&self) -> bool {
        self.access_level.is_admin()
    }
}

/// In-memory `sec.Groups` mirror (see [`GroupRecord`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SecurityGroups {
    groups: Vec<GroupRecord>,
}

impl SecurityGroups {
    pub fn new() -> SecurityGroups {
        SecurityGroups::default()
    }

    /// `Groups.Add(id)`: id is mandatory and ≤ [`MAX_GROUP_ID_LEN`] chars, and
    /// ids are unique (`GetIfExists(id)` is the lookup the spec names).
    pub fn add(&mut self, id: &str) -> Result<usize> {
        let id = id.trim();
        if id.is_empty() {
            return Err(cw(ERR_MANDATORY_ID));
        }
        if id.chars().count() > MAX_GROUP_ID_LEN {
            return Err(cw(&group_id_error()));
        }
        if self.get(id).is_some() {
            return Err(cw(&format!("Group {id} already exists")));
        }
        self.groups.push(GroupRecord { id: id.to_string(), ..Default::default() });
        Ok(self.groups.len() - 1)
    }

    /// `Groups.GetIfExists(id)`.
    pub fn get(&self, id: &str) -> Option<&GroupRecord> {
        self.groups.iter().find(|g| g.id.eq_ignore_ascii_case(id.trim()))
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut GroupRecord> {
        self.groups.iter_mut().find(|g| g.id.eq_ignore_ascii_case(id.trim()))
    }

    /// `Groups.Remove(id)`.
    pub fn remove(&mut self, id: &str) -> Result<()> {
        let before = self.groups.len();
        self.groups.retain(|g| !g.id.eq_ignore_ascii_case(id.trim()));
        if self.groups.len() == before {
            return Err(cw(&format!("Group {id} not found")));
        }
        Ok(())
    }

    pub fn groups(&self) -> &[GroupRecord] {
        &self.groups
    }

    pub fn len(&self) -> usize {
        self.groups.len()
    }

    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// `sec.IsUserAdmin(user)` against the mirror: the user belongs to the
    /// `ADMN` group. (The engagement file cannot answer this — module docs.)
    pub fn is_user_admin(&self, user: &str) -> bool {
        self.get(ADMIN_GROUP)
            .map(|g| g.employees.iter().any(|u| u.trim() == user.trim()))
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// File check-out / check-in (client.SignIn / SignOut / IsSignedOut)
// ---------------------------------------------------------------------------

/// `<stem>.locks` sidecar suffix (spec §1 `ICWDocumentLockInfo`).
pub const LOCKS_SUFFIX: &str = ".locks";

/// The file-level check-out state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckoutState {
    /// No `<stem>.locks` record: `IsSignedOut == False` (the corpus state —
    /// every corpus sidecar is zero bytes).
    NotSignedOut,
    /// A non-empty `.locks` sidecar exists; its record layout is undecoded.
    Unknown,
}

/// Read `<dir>/<stem>.locks` and classify it.
///
/// An absent or empty sidecar is the corpus normal form (nothing checked out).
/// A non-empty sidecar is reported as [`CheckoutState::Unknown`]: the
/// `ICWDocumentLockInfo` *fields* are mapped but the byte layout is not, so no
/// parser is invented (module docs).
pub fn read_checkout(dir: impl AsRef<Path>, stem: &str) -> Result<CheckoutState> {
    let dir = dir.as_ref();
    let want = format!("{stem}{LOCKS_SUFFIX}").to_ascii_lowercase();
    let Some(path) = fs::read_dir(dir).ok().and_then(|rd| {
        rd.flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().to_ascii_lowercase() == want)
                    .unwrap_or(false)
            })
    }) else {
        return Ok(CheckoutState::NotSignedOut);
    };
    let len = fs::metadata(&path)?.len();
    Ok(if len == 0 { CheckoutState::NotSignedOut } else { CheckoutState::Unknown })
}

/// `client.SignIn()/SignOut()/IsSignedOut` state machine.
///
/// The one-way evidence is the refusal: `SignIn` on a file that is not signed
/// out raises [`ERR_SIGNIN_NOT_SIGNED_OUT`]. A second `SignOut` is not probed,
/// so it is treated as idempotent (documented) rather than inventing an error.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FileCheckout {
    signed_out: bool,
}

impl FileCheckout {
    /// A file in the corpus normal form: not signed out.
    pub fn new() -> FileCheckout {
        FileCheckout::default()
    }

    /// Seed from a sidecar classification: only `NotSignedOut` is decodable.
    pub fn from_state(state: CheckoutState) -> Result<FileCheckout> {
        match state {
            CheckoutState::NotSignedOut => Ok(FileCheckout::new()),
            CheckoutState::Unknown => Err(cw(
                "check-out state undecoded: the .locks record layout is not mapped",
            )),
        }
    }

    /// COM `IsSignedOut`.
    pub fn is_signed_out(&self) -> bool {
        self.signed_out
    }

    /// COM `SignOut()` (file check-out).
    pub fn sign_out(&mut self) {
        self.signed_out = true;
    }

    /// COM `SignIn()` (file check-in): only valid on a signed-out file.
    pub fn sign_in(&mut self) -> Result<()> {
        if !self.signed_out {
            return Err(cw(ERR_SIGNIN_NOT_SIGNED_OUT));
        }
        self.signed_out = false;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Failed-attempt park (3-strike) and the session view
// ---------------------------------------------------------------------------

/// Outcome of one password check. The digest is undecoded, so the caller (the
/// crypto/COM lane) supplies the verdict and this module owns the law.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialCheck {
    Accepted,
    Rejected,
}

/// What one attempt did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptOutcome {
    /// Password accepted and the identity was not parked.
    Accepted,
    /// Refused: `failures` so far, `remaining` before the park.
    WrongPassword { failures: u32, remaining: u32 },
    /// The identity is parked: this attempt tripped the 3rd failure, or it was
    /// already parked (a parked identity refuses even a correct password).
    Parked,
}

/// Per-identity failed-attempt ledger.
///
/// The threshold is [`PARK_AFTER_FAILURES`]. No on-disk counter is evidenced
/// (module docs), so the ledger is in memory; [`AttemptLedger::reset`] is the
/// explicit operator release (no COM path is recorded for it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AttemptLedger {
    entries: BTreeMap<String, Attempts>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Attempts {
    failures: u32,
    parked: bool,
}

impl AttemptLedger {
    pub fn new() -> AttemptLedger {
        AttemptLedger::default()
    }

    pub fn failures(&self, identity: &str) -> u32 {
        self.entries.get(identity).map(|a| a.failures).unwrap_or(0)
    }

    pub fn is_parked(&self, identity: &str) -> bool {
        self.entries.get(identity).map(|a| a.parked).unwrap_or(false)
    }

    /// Record one attempt for `identity`.
    pub fn attempt(&mut self, identity: &str, check: CredentialCheck) -> AttemptOutcome {
        let entry = self.entries.entry(identity.to_string()).or_default();
        if entry.parked {
            return AttemptOutcome::Parked;
        }
        match check {
            CredentialCheck::Accepted => {
                *entry = Attempts::default();
                AttemptOutcome::Accepted
            }
            CredentialCheck::Rejected => {
                entry.failures += 1;
                if entry.failures >= PARK_AFTER_FAILURES {
                    entry.parked = true;
                    AttemptOutcome::Parked
                } else {
                    AttemptOutcome::WrongPassword {
                        failures: entry.failures,
                        remaining: PARK_AFTER_FAILURES - entry.failures,
                    }
                }
            }
        }
    }

    /// Release a park / clear the counter for one identity.
    pub fn reset(&mut self, identity: &str) {
        self.entries.remove(identity);
    }
}

/// Result of [`SecuritySession::sign_in`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignInOutcome {
    SignedIn,
    WrongPassword { failures: u32, remaining: u32 },
    Parked,
}

/// COM `client.Security` session model: `CurrentUser` + the attempt ledger.
///
/// Only two session facts are evidenced: `CurrentUser=''` while protection is
/// off (spec §3) and the access-list/level answers above. The transitions are
/// the engine's explicit model (module docs, G2 open item); the ledger law
/// (3 strikes, then park) is the recorded lane constant.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SecuritySession {
    current_user: String,
    ledger: AttemptLedger,
}

impl SecuritySession {
    pub fn new() -> SecuritySession {
        SecuritySession::default()
    }

    /// COM `sec.CurrentUser` (empty when nobody is signed in).
    pub fn current_user(&self) -> &str {
        &self.current_user
    }

    pub fn ledger(&self) -> &AttemptLedger {
        &self.ledger
    }

    pub fn failures(&self, identity: &str) -> u32 {
        self.ledger.failures(identity)
    }

    pub fn is_parked(&self, identity: &str) -> bool {
        self.ledger.is_parked(identity)
    }

    /// `ChangeIdentity(u,p,new)`-shaped sign-on: the caller supplies the
    /// password verdict. A parked identity refuses even an accepted password
    /// until [`Self::reset_park`] runs.
    pub fn sign_in(&mut self, user: &str, check: CredentialCheck) -> Result<SignInOutcome> {
        let user = user.trim();
        if user.is_empty() {
            return Err(cw(ERR_MANDATORY_ID));
        }
        Ok(match self.ledger.attempt(user, check) {
            AttemptOutcome::Accepted => {
                self.current_user = user.to_string();
                SignInOutcome::SignedIn
            }
            AttemptOutcome::WrongPassword { failures, remaining } => {
                SignInOutcome::WrongPassword { failures, remaining }
            }
            AttemptOutcome::Parked => SignInOutcome::Parked,
        })
    }

    /// Clear `CurrentUser` (session sign-off; see the module docs — CW's
    /// `SignOut` is the file check-out, modelled by [`FileCheckout`]).
    pub fn sign_out(&mut self) {
        self.current_user.clear();
    }

    /// Release a park for one identity (operator action; no COM path recorded).
    pub fn reset_park(&mut self, identity: &str) {
        self.ledger.reset(identity.trim());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_payload_is_the_off_marker() {
        let bytes = encode_utf16_stream("");
        assert_eq!(bytes, vec![2, 0, 0, 0, 0, 0]);
        assert_eq!(bytes.len(), OFF_STREAM_LEN);
        assert_eq!(decode_utf16_stream(&bytes).unwrap(), "");
        assert!(decode_protection_stream(&bytes).unwrap().is_none());
        assert_eq!(encode_protection_stream(None), bytes);
        assert!(decode_userlist_stream(&bytes).unwrap().is_empty());
        assert_eq!(encode_userlist_stream(&[]), bytes);
    }

    #[test]
    fn stream_codec_roundtrips_and_rejects_short_counts() {
        let text = "{\"a\":\"b c\"}\n";
        let bytes = encode_utf16_stream(text);
        assert_eq!(decode_utf16_stream(&bytes).unwrap(), text);
        assert_eq!(bytes.len(), 4 + (text.chars().count() + 1) * 2);
        // declared count is bytes, not units
        let mut bad = bytes.clone();
        bad[0] = (bad.len() as u8).wrapping_sub(1);
        assert!(decode_utf16_stream(&bad).is_err());
        assert!(decode_utf16_stream(&[2, 0, 0]).is_err());
    }

    #[test]
    fn protection_info_text_is_the_corpus_shape() {
        let info = ProtectionInfo {
            allow_auto_global_groups: true,
            adgroups: Vec::new(),
            groups: Vec::new(),
            users: vec!["E109586".into(), "E120235".into()],
        };
        assert_eq!(
            info.to_text(),
            "{\"AllowAutoGlobalGroups\":1,\"adgroups\":\"\",\"groups\":\"\",\"users\":\"E109586 E120235\"}\n"
        );
        assert_eq!(ProtectionInfo::parse(&info.to_text()).unwrap(), info);
        // unknown keys are refused, not dropped
        let bad = "{\"AllowAutoGlobalGroups\":1,\"adgroups\":\"\",\"groups\":\"\",\"users\":\"\",\"x\":1}\n";
        assert!(ProtectionInfo::parse(bad).is_err());
        // non-0/1 flag is refused
        let bad = "{\"AllowAutoGlobalGroups\":2,\"adgroups\":\"\",\"groups\":\"\",\"users\":\"\"}\n";
        assert!(ProtectionInfo::parse(bad).is_err());
    }

    #[test]
    fn userlist_requires_the_sup_trailer_when_populated() {
        let users = vec!["E109586".to_string()];
        let bytes = encode_userlist_stream(&users);
        assert_eq!(decode_userlist_stream(&bytes).unwrap(), users);
        let missing = encode_utf16_stream("E109586");
        assert!(decode_userlist_stream(&missing).is_err());
    }

    #[test]
    fn ledger_parks_on_the_third_failure() {
        let mut ledger = AttemptLedger::new();
        assert_eq!(
            ledger.attempt("u", CredentialCheck::Rejected),
            AttemptOutcome::WrongPassword { failures: 1, remaining: 2 }
        );
        assert_eq!(
            ledger.attempt("u", CredentialCheck::Rejected),
            AttemptOutcome::WrongPassword { failures: 2, remaining: 1 }
        );
        assert_eq!(ledger.attempt("u", CredentialCheck::Rejected), AttemptOutcome::Parked);
        assert!(ledger.is_parked("u"));
        assert_eq!(ledger.attempt("u", CredentialCheck::Accepted), AttemptOutcome::Parked);
        assert_eq!(ledger.failures("u"), PARK_AFTER_FAILURES);
        // per identity
        assert_eq!(ledger.attempt("v", CredentialCheck::Accepted), AttemptOutcome::Accepted);
        ledger.reset("u");
        assert!(!ledger.is_parked("u"));
        assert_eq!(ledger.failures("u"), 0);
    }

    #[test]
    fn group_ids_and_access_levels() {
        let mut groups = SecurityGroups::new();
        assert_eq!(groups.add("ADMN").unwrap(), 0);
        groups.get_mut("ADMN").unwrap().access_level = CaseViewAccess::admin();
        groups.get_mut("ADMN").unwrap().employees.push("E099901".into());
        assert!(groups.get("admn").unwrap().is_admin());
        assert!(groups.is_user_admin("E099901"));
        assert!(!groups.is_user_admin("E109586"));
        assert!(groups.add("T1").is_ok());
        assert_eq!(
            groups.add("TSTGRP").unwrap_err().to_string(),
            format!("format: {}", group_id_error())
        );
        assert_eq!(
            groups.add("  ").unwrap_err().to_string(),
            format!("format: {ERR_MANDATORY_ID}")
        );
        assert!(groups.add("T1").is_err(), "ids are keys");
        assert!(CaseViewAccess::parse(0).unwrap().level() == 0);
        assert!(CaseViewAccess::parse(3).unwrap().is_admin());
        assert!(CaseViewAccess::parse(4).is_err());
    }

    #[test]
    fn checkout_refuses_sign_in_when_not_signed_out() {
        let mut checkout = FileCheckout::new();
        assert!(!checkout.is_signed_out());
        assert_eq!(
            checkout.sign_in().unwrap_err().to_string(),
            format!("format: {ERR_SIGNIN_NOT_SIGNED_OUT}")
        );
        checkout.sign_out();
        assert!(checkout.is_signed_out());
        checkout.sign_out();
        assert!(checkout.is_signed_out(), "second SignOut is idempotent");
        checkout.sign_in().unwrap();
        assert!(!checkout.is_signed_out());
    }

    #[test]
    fn setup_and_signon_collections() {
        assert!(ProtectionSetup::open(&[], "", true).is_err());
        let mut users = SecurityUsers::new();
        users.add("DW").unwrap();
        users.add("RSM").unwrap();
        assert_eq!(users.item(1).unwrap().user_id, "DW");
        assert!(users.item(0).is_none());
        assert!(users.add("dw").is_err());
        // `CanUserAccess` with protection off is unresolved, not denied: there is
        // no list to be absent from.
        let off = ProtectionStore::default();
        let u = UserAccount::new("DW").unwrap();
        assert_eq!(u.can_user_access(&off, 1), AccessVerdict::Unresolved);
        assert_eq!(ProtectionSetup::open(&[], "u", false).unwrap_err().to_string(),
            format!("format: {ERR_ADMIN_REQUIRED}"));
    }

    #[test]
    fn blank_roster_has_the_corpus_geometry() {
        let t = blank_ec_table();
        assert_eq!(t.fields.len(), EC_FIELD_COUNT);
        assert_eq!(t.record_len as usize, EC_RECORD_LEN);
        assert_eq!(t.header_len as usize, 32 + 32 * EC_FIELD_COUNT + 1);
        assert_eq!(t.version, EC_FILE_VERSION);
        assert!(ec_schema_matches(&t));
        let bytes = t.encode();
        let round = Table::parse(&bytes).unwrap();
        assert_eq!(round.encode(), bytes);
        assert!(EcTable::from_table(round).unwrap().records().is_empty());
    }
}
