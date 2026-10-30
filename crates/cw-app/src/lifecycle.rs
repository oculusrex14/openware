//! cw-app lifecycle — engagement copy / new-from-template (P5.D1).
//!
//! Behavior contract: `docs\specs\lifecycle.md` v0.9; fixture laws F64
//! (`evidence\the reference notes`), probes `a sample copy\`. The engine reproduces the
//! ROW-SET laws (live/deleted census + multiset parity), not CW's byte
//! cosmetics (memo renumbering, cdx rebuild, cvw path rewrite) — those are
//! documented deviations, v2 lane.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use cw_core::dbf::{RawRecord, Table};
use cw_core::{CoreError, Result};

/// Deletion-flag byte for a live record.
pub const LIVE: u8 = b' ';
/// Deletion-flag byte for a deleted (tombstone) record.
pub const DELETED: u8 = b'*';

/// Runtime artifacts CaseWare does not carry into a copy (F64 (2)).
const SKIP_FILES: [&str; 2] = ["copy.ini", "CmpChart.txt"];
const SKIP_SUFFIX: &str = ".locks";

/// The ICWCopyTemplate option mask (F64 (1)); `Default` = factory defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CopyOptions {
    pub copy_all: bool,
    /// v2 engine: recorded, not yet implemented (spec §5).
    pub copy_taxonomy: bool,
    /// v2 engine: recorded, not yet implemented (spec §5).
    pub copy_trial_balance: bool,
    /// Post-copy modifier: BL former-live rows flag-deleted, GL emptied.
    pub clear_account_balances: bool,
}

impl CopyOptions {
    /// True when at least one copy-selection is present (DoCopy's own rule:
    /// `Must complete components information` otherwise, F64 (1)).
    fn has_selection(&self) -> bool {
        self.copy_all || self.copy_taxonomy || self.copy_trial_balance
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableReport {
    pub src_live: usize,
    pub dst_live: usize,
    pub src_deleted: usize,
    pub dst_deleted: usize,
}

#[derive(Clone, Debug, Default)]
pub struct CopyReport {
    pub source_stem: String,
    pub dest_stem: String,
    pub tables: BTreeMap<String, TableReport>,
    /// Destination-relative names of written/copied files.
    pub files: Vec<String>,
    pub notes: Vec<String>,
}

/// Split `TPROBEAC.dbf` into (`TPROBE`, `AC`): the engagement stem plus the
/// table suffix (or document key, for `<stem><SCHED>.cvb` style sidecars).
fn stem_suffix(name: &str, stem: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix(stem)?;
    let (base, ext) = rest.rsplit_once('.')?;
    Some((base.to_string(), ext.to_ascii_lowercase()))
}

fn census(t: &Table) -> (usize, usize) {
    let mut live = 0;
    let mut del = 0;
    for r in &t.records {
        match r[0] {
            b'*' => del += 1,
            b' ' => live += 1,
            _ => {}
        }
    }
    (live, del)
}

/// Copy the engagement at `src_dir` into `<dst_root>/<dest_stem>/` per `opts`
/// (F64). Row-set-faithful; byte cosmetics deferred (spec §4/§5).
pub fn copy_engagement(
    src_dir: &Path,
    dst_root: &Path,
    dest_stem: &str,
    opts: &CopyOptions,
) -> Result<CopyReport> {
    if dest_stem.is_empty() {
        return Err(CoreError::Format("empty destination stem".into()));
    }
    if !opts.has_selection() {
        // CW's own error, pre-empted: a copy-selection is mandatory (F64 (1)).
        return Err(CoreError::Format(
            "no copy selection: set copy_all or a component flag".into(),
        ));
    }
    let mut unsupported: Vec<&'static str> = Vec::new();
    if !opts.copy_all && (opts.copy_taxonomy || opts.copy_trial_balance) {
        unsupported.push("partial component masks (v2)");
    }

    // The source engagement is identified by its single `*.ac` hub.
    let mut src_stem: Option<String> = None;
    for e in fs::read_dir(src_dir).map_err(CoreError::from)? {
        let e = e.map_err(CoreError::from)?;
        let name = e.file_name().to_string_lossy().to_string();
        // OS noise (`.DS_Store`, macOS `._<name>` resource forks on exFAT/NTFS/
        // SMB volumes) is never the engagement hub.
        if name.starts_with('.') {
            continue;
        }
        if name.to_ascii_lowercase().ends_with(".ac") {
            if src_stem.is_some() {
                return Err(CoreError::Format("multiple .ac hubs in source".into()));
            }
            src_stem = Some(name.trim_end_matches(".ac").to_string());
        }
    }
    let src_stem = src_stem
        .ok_or_else(|| CoreError::Format("no .ac hub in source dir".into()))?;

    let dst_dir = dst_root.join(dest_stem);
    std::fs::create_dir_all(&dst_dir).map_err(CoreError::from)?;

    let mut rep = CopyReport {
        source_stem: src_stem.clone(),
        dest_stem: dest_stem.to_string(),
        ..Default::default()
    };
    if !unsupported.is_empty() {
        rep.notes.push(format!("unsupported: {}", unsupported.join(", ")));
    }

    for e in fs::read_dir(src_dir).map_err(CoreError::from)? {
        let e = e.map_err(CoreError::from)?;
        let path = e.path();
        if !path.is_file() {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        if name.ends_with(SKIP_SUFFIX) || SKIP_FILES.contains(&name.as_str()) {
            continue;
        }
        // Never copy OS noise into a new engagement (`.DS_Store`, AppleDouble
        // `._<name>` companions written by macOS on foreign volumes).
        if name.starts_with('.') {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".dbf") {
            let Some((table, _ext)) = stem_suffix(&name, &src_stem) else {
                continue; // foreign table, not part of this engagement
            };
            let sfx = table.to_ascii_uppercase();
            let t = Table::parse_path(&path)?;
            let (sl, sd) = census(&t);
            let mut recs: Vec<RawRecord> = Vec::with_capacity(t.records.len());
            if opts.copy_all {
                // live rows only: the pack law (F64 (3)).
                for r in &t.records {
                    if r[0] == LIVE {
                        recs.push(r.clone());
                    }
                }
                if opts.clear_account_balances {
                    match sfx.as_str() {
                        "BL" => {
                            // former-live rows survive flag-deleted; prior
                            // tombstones were purged with the pack.
                            for r in recs.iter_mut() {
                                r[0] = DELETED;
                            }
                        }
                        "GL" => recs.clear(),
                        _ => {}
                    }
                }
            } else {
                // v2: component filters (recorded, unsupported above).
                recs = t.records.clone();
            }
            let out = Table {
                raw_header: t.raw_header.clone(),
                version: t.version,
                header_len: t.header_len,
                record_len: t.record_len,
                fields: t.fields.clone(),
                records: recs,
            };
            let (dl, dd) = census(&out);
            let fname = format!("{dest_stem}{sfx}.dbf");
            out.write_path(dst_dir.join(&fname))?;
            rep.tables.insert(
                sfx,
                TableReport { src_live: sl, dst_live: dl, src_deleted: sd, dst_deleted: dd },
            );
            rep.files.push(fname);
        } else if name.starts_with(src_stem.as_str()) {
            // sidecar / document hub: verbatim copy under the new stem
            // (F64 (2)); the CV/.ac content rewrite is a documented v2
            // deviation.
            let rest = &name[src_stem.len()..];
            let fname = format!("{dest_stem}{rest}");
            std::fs::copy(&path, dst_dir.join(&fname)).map_err(CoreError::from)?;
            rep.files.push(fname);
        } else {
            // plain document: verbatim
            std::fs::copy(&path, dst_dir.join(&name)).map_err(CoreError::from)?;
            rep.files.push(name);
        }
    }
    rep.files.sort();
    Ok(rep)
}

