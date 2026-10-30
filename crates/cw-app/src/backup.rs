//! cw-app backup / restore / compress / cleanup (P5.D3).
//!
//! Behavior contract: `docs\specs\backup.md` (v1.0), fixture laws F66.
//!
//! Verified against CaseWare 25.0 behavior on staged sandbox copies:
//! - `ICWClients.Compress(hub.ac, IncludeBackups=True)` writes `<stem>.ac_`
//!   beside the hub and then **consumes the expanded working set** (the
//!   directory is left holding the package, plus any omitted `.bak` files).
//!   `Compress2(hub, Flags)` mirrors it: `cOmitBackups=1` keeps `.bak`
//!   siblings, `cCompressAllSubFolders=4` behaves like IncludeBackups=True.
//! - The package payload is the hub first (method 8) then every other file
//!   (method 14). The hub member is a 5-byte version stub (`\x058.98`); the
//!   expanded 28 KiB `.ac` CFB hub is **synthesized** by CW on uncompress.
//! - `ICWClients.Uncompress(.ac_)` expands in place and removes the package.
//! - `.bak` = per-CV-document previous-generation snapshot: same 21-stream
//!   CaseView set as the sibling `.cvw`, same stem, no rotation counter.
//! - Cleanup wizard surface: `client.Purges`, `EmptyRecycleBin`, `BadIndices`,
//!   `ClientOptions.RetainOnCleanup`/`AllowOverwriteCleanup` (mapped, F66);
//!   the v1 engine implements the data half (deleted-row pack per the F64
//!   live-pack law) and `.bak` removal. COM purge-list semantics stay open.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use cw_core::dbf::Table;
use cw_core::pkg::{Pkg, ZipWriter, S_COMPRESSION, S_DOCUMENT, S_FILEINFO};
use cw_core::transact::commit_bytes;
use cw_core::{CoreError, Result};

pub const HUB_EXT: &str = "ac";
pub const PKG_EXT: &str = "ac_";
pub const BAK_EXT: &str = "bak";
/// CW's in-package hub stub (F66): 5 bytes, the expansion writes a 28 KiB CFB.
pub const HUB_STUB: &[u8] = b"\x058.98";

/// True for filesystem noise that is never engagement content: dot-prefixed
/// names (`.DS_Store`, `._<name>` AppleDouble resource forks that macOS writes
/// beside every file on exFAT/NTFS/SMB volumes, and editor droppings).
fn is_os_noise(path: &Path) -> bool {
    path.file_name()
        .map(|n| n.to_string_lossy().starts_with('.'))
        .unwrap_or(false)
}

/// Files the pack always leaves alone (the package being built, and OS noise).
fn is_skipped(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".ac_") || lower.ends_with(".~ocw")
}

/// Recursive file walk producing package entry names (`SCRIPT\x.js` — the CW
/// mixed-separator convention, F62).
fn collect_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, PathBuf)>,
    include_backups: bool,
) -> Result<()> {
    for e in fs::read_dir(dir).map_err(CoreError::from)? {
        let e = e.map_err(CoreError::from)?;
        let path = e.path();
        if path.is_dir() {
            collect_files(root, &path, out, include_backups)?;
            continue;
        }
        if !path.is_file() {
            continue;
        }
        // Never pack OS noise: a macOS `._<name>` companion or `.DS_Store`
        // would otherwise become a bogus member (and `._x.ac` would read as a
        // second hub).
        if is_os_noise(&path) {
            continue;
        }
        let rel = path.strip_prefix(root).map_err(|_| CoreError::Format("path escape".into()))?;
        let name = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("\\");
        if !include_backups && name.to_ascii_lowercase().ends_with(".bak") {
            continue;
        }
        out.push((name, path));
    }
    Ok(())
}

fn find_hub(dir: &Path) -> Result<(String, PathBuf)> {    let mut hub: Option<(String, PathBuf)> = None;
    for e in fs::read_dir(dir).map_err(CoreError::from)? {
        let e = e.map_err(CoreError::from)?;
        if !e.path().is_file() {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        // OS noise (`.DS_Store`, macOS `._<name>` resource forks on exFAT/NTFS/
        // SMB volumes, editor droppings) is never the engagement hub.
        if name.starts_with('.') {
            continue;
        }
        if name.to_ascii_lowercase().ends_with(".ac") {
            if hub.is_some() {
                return Err(CoreError::Format("multiple .ac hubs in directory".into()));
            }
            hub = Some((name.trim_end_matches(".ac").to_string(), e.path()));
        }
    }
    hub.ok_or_else(|| CoreError::Format("no .ac hub in directory".into()))
}

/// CW flag mirror (`CWCompressFlags`): `cOmitBackups=1`, `cOmitIndices=2`,
/// `cOmitBackupAndIndicies=3`, `cCompressAllSubFolders=4`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageOptions {
    pub include_backups: bool,
    /// v1 defaults to **false** (safe); CW's own call consumes the sources
    /// after a successful package write. Set true for the CW-equal behavior.
    pub consume_sources: bool,
    /// Carry the container metadata streams from this package (the corpus-way:
    /// SummaryInformation, DocumentSummaryInformation, TemplateInfo, Dms*).
    pub meta_source: Option<PathBuf>,
}

impl Default for PackageOptions {
    fn default() -> Self {
        PackageOptions { include_backups: true, consume_sources: false, meta_source: None }
    }
}

impl PackageOptions {
    /// `Compress2` flags → options (2 = omit indices is accepted; our writer
    /// carries no index set, so it is a no-op here).
    pub fn from_flags(flags: u32) -> PackageOptions {
        PackageOptions {
            include_backups: flags & 1 == 0 && flags & 3 != 3,
            consume_sources: true,
            meta_source: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageEntry {
    pub name: String,
    pub size: usize,
    pub method: u16,
}

#[derive(Clone, Debug, Default)]
pub struct PackageReport {
    pub pkg_path: PathBuf,
    pub stem: String,
    pub entries: Vec<PackageEntry>,
    pub consumed: Vec<String>,
    pub notes: Vec<String>,
}

/// Build `<stem>.ac_` from an expanded working directory (F66 compress law).
pub fn package_engagement(dir: &Path, opts: &PackageOptions) -> Result<PackageReport> {
    let (stem, hub_path) = find_hub(dir)?;
    let hub_name = format!("{stem}.ac");

    // Hub first, then every other file (recursive, CW mixes separators) in
    // name order (the corpus order).
    let mut others: Vec<(String, PathBuf)> = Vec::new();
    collect_files(dir, dir, &mut others, opts.include_backups)?;
    others.retain(|(name, _)| name != &hub_name && !is_skipped(name));
    others.sort_by(|a, b| a.0.cmp(&b.0));

    let mut zw = ZipWriter::new();
    let hub = fs::read(&hub_path).map_err(CoreError::from)?;
    zw.add_stored(hub_name.clone(), hub);
    let mut entries = vec![PackageEntry {
        name: hub_name.clone(),
        size: fs::metadata(&hub_path)?.len() as usize,
        method: 8,
    }];
    for (name, path) in &others {
        let data = fs::read(path).map_err(CoreError::from)?;
        entries.push(PackageEntry { name: name.clone(), size: data.len(), method: 8 });
        zw.add_stored(name.clone(), data);
    }
    let document = zw.build()?;

    let mut rep = PackageReport {
        pkg_path: dir.join(format!("{stem}.ac_")),
        stem: stem.clone(),
        entries,
        ..Default::default()
    };

    let streams = container_streams(&stem, document, opts.meta_source.as_deref(), &mut rep.notes)?;
    let bytes = Pkg::build(streams)?;
    commit_bytes(&rep.pkg_path, &bytes)?;

    if opts.consume_sources {
        // CW's own post-compress state: package only, minus omitted files.
        let mut names: Vec<String> = rep.entries.iter().map(|e| e.name.clone()).collect();
        let mut removed = Vec::new();
        for name in names.drain(..) {
            let p = dir.join(&name);
            if name == hub_name || name.to_ascii_lowercase().ends_with(".bak") {
                if fs::remove_file(&p).is_ok() {
                    removed.push(name);
                }
            } else if fs::remove_file(&p).is_ok() {
                removed.push(name);
            }
        }
        rep.consumed = removed;
    }
    rep.notes.push(if opts.include_backups {
        ".bak siblings packed".into()
    } else {
        ".bak siblings omitted (kept on disk)".into()
    });
    Ok(rep)
}

/// The ten canonical container streams; payload + metadata.
fn container_streams(
    stem: &str,
    document: Vec<u8>,
    meta_source: Option<&Path>,
    notes: &mut Vec<String>,
) -> Result<Vec<(String, Vec<u8>)>> {
    let payload = vec![(S_DOCUMENT.to_string(), document)];
    if let Some(src) = meta_source {
        let buf = fs::read(src).map_err(CoreError::from)?;
        let pkg = Pkg::parse(&buf)?;
        let mut out = Vec::new();
        for name in [
            "\u{5}DocumentSummaryInformation",
            "\u{5}SummaryInformation",
            "CaseWareProtectionInfo",
            "CaseWareUserlist",
            "CompressionVersion",
            "DmsProperties",
            "DmsVersionInfo",
            S_FILEINFO,
            "TemplateInfo",
        ] {
            out.push((name.to_string(), pkg.stream(name)?));
        }
        // FileInformation names the engagement, not the template package.
        out.retain(|(n, _)| n != S_FILEINFO);
        out.push((S_FILEINFO.to_string(), file_information(stem)));
        notes.push(format!("container metadata carried from {}", src.display()));
        out.extend(payload);
        return Ok(out);
    }
    notes.push("synthetic container metadata (no meta_source)".into());
    let mut dms = vec![0u8; 536];
    dms[0..4].copy_from_slice(&[0x34, 0x12, 0, 0]);
    dms[4..8].copy_from_slice(&[0x10, 0, 0, 0]);
    dms[16..20].copy_from_slice(&[0x10, 0, 0, 0]);
    dms[20..24].copy_from_slice(&[1, 0, 0, 0]);
    let mut out: Vec<(String, Vec<u8>)> = vec![
        ("\u{5}DocumentSummaryInformation".into(), vec![0u8; 5204]),
        ("\u{5}SummaryInformation".into(), vec![0u8; 128]),
        ("CaseWareProtectionInfo".into(), vec![2, 0, 0, 0, 0, 0]),
        ("CaseWareUserlist".into(), vec![2, 0, 0, 0, 0, 0]),
        (S_COMPRESSION.into(), b"DynaZip5.00.06".to_vec()),
        ("DmsProperties".into(), vec![0u8; 4]),
        ("DmsVersionInfo".into(), dms),
        (S_FILEINFO.into(), file_information(stem)),
        ("TemplateInfo".into(), b"<values><group name=\"SourceInfo\"/></values>\r\n\0".to_vec()),
    ];
    out.extend(payload);
    Ok(out)
}

/// `FileInformation`: name then NUL padding (observed size 261 on the corpus).
fn file_information(stem: &str) -> Vec<u8> {
    let mut v = stem.as_bytes().to_vec();
    let target = (v.len() + 1).max(261);
    v.resize(target, 0);
    v
}

#[derive(Clone, Debug, Default)]
pub struct RestoreReport {
    pub stem: String,
    pub members: Vec<PackageEntry>,
    pub written: Vec<String>,
    pub failed: Vec<(String, String)>,
    /// Members CW itself restores despite a bad stored CRC (F63 residual).
    pub crc_mismatch: Vec<String>,
    pub hub_stub: Option<usize>,
    pub notes: Vec<String>,
}

/// Expand `<stem>.ac_` in place (F66 uncompress law). Tolerant by design:
/// members the corpus itself cannot decode (F63 truncated/bad-CRC minority)
/// are reported, not fatal. The hub is written as the 5-byte stub because CW
/// synthesizes the real CFB hub on expansion (documented gap, spec §7).
pub fn restore_package(pkg_path: &Path, dst_dir: &Path) -> Result<RestoreReport> {
    let buf = fs::read(pkg_path).map_err(CoreError::from)?;
    let pkg = Pkg::parse(&buf)?;
    let document = pkg.document()?;
    let zip = if document.is_empty() { None } else { Some(cw_core::pkg::Zip::parse(&document)?) };
    let Some(zip) = zip else {
        return Err(CoreError::Format("package carries no CaseWareDocument payload".into()));
    };
    let stem = if !pkg.stream(S_FILEINFO).is_ok() {
        pkg_path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
    } else {
        cw_core::pkg::engagement_name(&pkg.stream(S_FILEINFO)?)
    };
    fs::create_dir_all(dst_dir).map_err(CoreError::from)?;

    let mut rep = RestoreReport { stem: stem.clone(), ..Default::default() };
    for (i, e) in zip.entries.iter().enumerate() {
        let rel = e.name.replace('/', std::path::MAIN_SEPARATOR_STR);
        let target = dst_dir.join(&rel);
        let is_hub = rel.to_ascii_lowercase().ends_with(".ac");
        match zip.decode_lenient(&document, i) {
            Ok((bytes, crc_ok)) => {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(CoreError::from)?;
                }
                commit_bytes(&target, &bytes)?;
                rep.members.push(PackageEntry { name: e.name.clone(), size: bytes.len(), method: e.method });
                if is_hub {
                    rep.hub_stub = Some(bytes.len());
                }
                if !crc_ok {
                    rep.crc_mismatch.push(e.name.clone());
                }
                rep.written.push(e.name.clone());
            }
            Err(err) => rep.failed.push((e.name.clone(), err.to_string())),
        }
    }
    rep.written.sort();
    if let Some(n) = rep.hub_stub {
        rep.notes.push(format!(
            "hub written as the {n}-byte package stub; CW synthesizes the CFB hub on expand"
        ));
    }
    Ok(rep)
}

// ---------------------------------------------------------------------------
// .bak — per-document previous generation (F30(3) confirmed by F66)
// ---------------------------------------------------------------------------

/// `<doc>.cvw` → `<doc>.bak` (same stem, no rotation counter).
pub fn document_backup_path(doc: &Path) -> PathBuf {
    doc.with_extension(BAK_EXT)
}

/// The 21-stream CaseView document set observed on both `.cvw` and `.bak`.
pub const CV_DOC_STREAMS: [&str; 4] = ["Form", "Form/Layout", "Index", "Index/Para"];

/// True when `path` is a CaseView document snapshot CFB (`.bak` shape).
pub fn is_document_backup(path: &Path) -> Result<bool> {
    let buf = fs::read(path).map_err(CoreError::from)?;
    if buf.len() < 8 || buf[0..8] != [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1] {
        return Ok(false);
    }
    let cfb = cw_core::cfb::Cfb::parse(&buf)?;
    let paths: Vec<String> = cfb.entries()?.iter().map(|e| e.path.join("/")).collect();
    Ok(CV_DOC_STREAMS.iter().all(|want| {
        paths
            .iter()
            .any(|p| p == want || p.starts_with(&format!("{want}/")))
    }))
}

/// Stash the current document as its `.bak` sibling (CW's save-time rollback).
pub fn stash_document_backup(doc: &Path) -> Result<PathBuf> {
    if !doc.is_file() {
        return Err(CoreError::Format(format!("no such document: {}", doc.display())));
    }
    let bak = document_backup_path(doc);
    fs::copy(doc, &bak).map_err(CoreError::from)?;
    Ok(bak)
}

/// Restore a document from its `.bak` sibling (inverse of the stash).
pub fn restore_document_backup(bak: &Path) -> Result<PathBuf> {
    if bak.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()) != Some(BAK_EXT.into()) {
        return Err(CoreError::Format("not a .bak file".into()));
    }
    let doc = bak.with_extension("cvw");
    fs::copy(bak, &doc).map_err(CoreError::from)?;
    Ok(doc)
}

// ---------------------------------------------------------------------------
// cleanup
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanupOptions {
    /// Pack every table to live rows (deleted tombstones dropped).
    pub purge_deleted_records: bool,
    /// Remove per-document `.bak` snapshots.
    pub remove_document_backups: bool,
}

impl Default for CleanupOptions {
    fn default() -> Self {
        CleanupOptions { purge_deleted_records: true, remove_document_backups: false }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CleanupReport {
    /// table suffix -> (before live, before deleted, after live)
    pub tables: BTreeMap<String, (usize, usize, usize)>,
    pub removed_files: Vec<String>,
    pub freed_bytes: u64,
}

const LIVE: u8 = b' ';

/// Engagement cleanup v1: the data half of the wizard. Deleted-record pack is
/// the F64 CopyAll law applied in place; `.bak` removal is the file half.
pub fn cleanup_engagement(dir: &Path, opts: &CleanupOptions) -> Result<CleanupReport> {
    let mut rep = CleanupReport::default();
    let mut names: Vec<String> = Vec::new();
    for e in fs::read_dir(dir).map_err(CoreError::from)? {
        let e = e.map_err(CoreError::from)?;
        if e.path().is_file() {
            names.push(e.file_name().to_string_lossy().to_string());
        }
    }
    names.sort();
    for name in &names {
        let lower = name.to_ascii_lowercase();
        let path = dir.join(name);
        if lower.ends_with(".dbf") && opts.purge_deleted_records {
            let t = Table::parse_path(&path)?;
            let (live, del) = census(&t.records);
            if del == 0 {
                continue;
            }
            let kept: Vec<cw_core::dbf::RawRecord> =
                t.records.iter().filter(|r| r[0] == LIVE).cloned().collect();
            let out = Table {
                raw_header: t.raw_header.clone(),
                version: t.version,
                header_len: t.header_len,
                record_len: t.record_len,
                fields: t.fields.clone(),
                records: kept,
            };
            out.write_path(&path)?;
            let suffix = name.trim_end_matches(".dbf").to_string();
            rep.tables.insert(suffix, (live, del, live));
        } else if lower.ends_with(".bak") && opts.remove_document_backups {
            let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            fs::remove_file(&path).map_err(CoreError::from)?;
            rep.removed_files.push(name.clone());
            rep.freed_bytes += size;
        }
    }
    Ok(rep)
}

fn census(records: &[cw_core::dbf::RawRecord]) -> (usize, usize) {
    let mut live = 0;
    let mut del = 0;
    for r in records {
        match r[0] {
            b'*' => del += 1,
            b' ' => live += 1,
            _ => {}
        }
    }
    (live, del)
}

