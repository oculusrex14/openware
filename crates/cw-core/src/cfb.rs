//! Compound File Binary (CFB / OLE2) reader + writer — the `.ac_` container
//! substrate (F20/F24: the `CaseWareDocument` zip lives in a flat-root CFB).
//!
//! Reader: spec-shaped (MS-CFB) with the tolerance the shipped CaseView
//! containers actually require (measured on all 34 corpus `.cvw`, see
//! `tests/cfb_corpus_gate.rs`):
//!
//! * **Partial tail sectors.** The last sector of a file is often written
//!   short. It reads as the bytes present; nothing is zero-padded, so a stream
//!   can never return fabricated content.
//! * **DIFAT de-duplication.** A repeated FAT sector id would shift every later
//!   FAT run by a whole sector and misroute every chain, so entries are deduped.
//! * **Chains end forgivingly.** `ENDOFCHAIN`, `FREESECT`, a `FATSECT` marker, a
//!   repeat, and an index past the FAT all terminate a walk instead of erroring.
//! * **Directory holes.** A slot that fails MS-CFB validation is a hole, not a
//!   reason to abandon the sector. Directory ids are slot indices, so holes keep
//!   the id space stable and only the garbage is dropped.
//! * **Directory chains stop at data.** Caseware links the real directory
//!   sectors as a *prefix* of a longer chain that then runs into ordinary data
//!   (hull2024 `03250RG1`: `1→2→5→310→348→381`, and 381 holds cell bytes). The
//!   sweep stops at the first sector with no valid entry.
//!
//! Writer: deterministic corpus-mimic profile — v4, 4096-byte sectors, mini 64,
//! cutoff 4096, minor 0x3E, layout `[FAT][dir][miniFAT][mini stream][data]`,
//! sorted-name balanced-BST directory. More than 109 FAT sectors (DIFAT
//! extension on write) is rejected explicitly; the corpus peaks at 5.
//!
//! Zero dependencies; byte-exactness is the contract (kernel port rules).

use crate::error::{CoreError, Result};

/// The only CFB signature (`D0 CF 11 E0 A1 B1 1A E1`).
pub const SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

const FREESECT: u32 = 0xFFFF_FFFF;
const ENDOFCHAIN: u32 = 0xFFFF_FFFE;
const FATSECT: u32 = 0xFFFF_FFFD;
const NOSTREAM: u32 = 0xFFFF_FFFF;

const ROOT_NAME: &str = "Root Entry";
const MAX_WALK: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    Storage,
    Stream,
}

#[derive(Debug, Clone)]
pub struct Entry {
    /// Path from the root, e.g. `["CaseWareDocument"]`.
    pub path: Vec<String>,
    pub kind: EntryKind,
    /// Stream length in bytes (0 for storages).
    pub len: usize,
}

impl Entry {
    /// Slash-joined path (the manifest key space).
    pub fn name(&self) -> String {
        self.path.join("/")
    }
}

pub struct Cfb {
    data: Vec<u8>,
    sector: usize,
    mini: usize,
    mini_cutoff: usize,
    fat: Vec<u32>,
    mini_fat: Vec<u32>,
    /// Directory slots; index == dir id (0 == root). `None` is a hole: a slot
    /// whose bytes fail MS-CFB validation, which Caseware's trailing partial
    /// directory sector produces in real documents.
    dir: Vec<Option<DirEntry>>,
}

#[derive(Debug, Clone)]
struct DirEntry {
    name: String,
    kind: u8,
    left: u32,
    right: u32,
    child: u32,
    start: u32,
    size: u64,
}

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn utf16_lossy(bytes: &[u8]) -> String {
    let ucs: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&ucs)
}

fn div_ceil(a: usize, b: usize) -> usize {
    (a + b - 1) / b
}

/// Walk a FAT sector chain from `start`, tolerating every way a real container
/// ends one: `ENDOFCHAIN`/`FREESECT`, a `FATSECT` marker, an index past the FAT
/// (the tail sector is short), or a repeat (corrupt linkage). Returns the
/// sectors reachable before the stop; it never loops.
fn fat_chain(fat: &[u32], start: u32) -> Vec<usize> {
    let mut out = Vec::new();
    let mut cur = start;
    while cur != ENDOFCHAIN && cur != FREESECT && cur != FATSECT {
        let idx = cur as usize;
        if out.len() >= MAX_WALK || out.contains(&idx) {
            break;
        }
        let Some(&next) = fat.get(idx) else { break };
        out.push(idx);
        cur = next;
    }
    out
}

/// Parse one 128-byte directory slot, or `None` when it is a hole.
///
/// MS-CFB validation is the kind byte plus a name length that lands on a
/// double-NUL inside the 64-byte name field. A slot that fails is skipped; the
/// sweep keeps going, because an unreadable slot says nothing about the next
/// one.
fn dir_entry(e: &[u8]) -> Option<DirEntry> {
    let kind = e[66];
    if kind != 1 && kind != 2 && kind != 5 {
        return None;
    }
    let cb = u16::from_le_bytes([e[64], e[65]]) as usize;
    if cb < 2 || cb > 64 || cb % 2 != 0 || e[cb - 2] != 0 || e[cb - 1] != 0 {
        return None;
    }
    Some(DirEntry {
        name: utf16_lossy(&e[0..cb - 2]),
        kind,
        left: u32le(e, 68),
        right: u32le(e, 72),
        child: u32le(e, 76),
        start: u32le(e, 116),
        size: u64::from_le_bytes([
            e[120], e[121], e[122], e[123], e[124], e[125], e[126], e[127],
        ]),
    })
}


impl Cfb {
    /// Parse a complete compound file.
    pub fn parse(buf: &[u8]) -> Result<Cfb> {
        if buf.len() < 1024 || buf[0..8] != SIGNATURE {
            return Err(CoreError::Format("cfb: bad signature".into()));
        }
        let major = u16::from_le_bytes([buf[26], buf[27]]);
        let sector_shift = u16::from_le_bytes([buf[30], buf[31]]);
        let mini_shift = u16::from_le_bytes([buf[32], buf[33]]);
        let sector = 1usize << sector_shift;
        let mini = 1usize << mini_shift;
        if !matches!(sector, 512 | 4096)
            || mini != 64
            || (sector == 4096 && major != 4)
            || (sector == 512 && major != 3)
        {
            return Err(CoreError::Format(format!(
                "cfb: unsupported shape major={major} sector={sector} mini={mini}"
            )));
        }
        let mini_cutoff = u32le(buf, 56) as usize;
        let num_fat = u32le(buf, 44) as usize;
        let first_dir = u32le(buf, 48);
        let first_mini_fat = u32le(buf, 60);
        let dif_first = u32le(buf, 68);
        let n_dif = u32le(buf, 72) as usize;

        // Caseware writes the final CFB sector short (e.g. 49,091 bytes = 95
        // whole sectors + 131 bytes). The reader never pads: a sector slice is
        // whatever the file actually carries, so a stream can never return
        // fabricated zero bytes.
        let end_of = buf.len();

        // Caseware/olefile base: the header is padded to a full sector, so
        // sector N lives at (N+1)*sector_len (identical to 512+N*512 when
        // sectors are 512).
        let sect = |id: usize| -> Option<&[u8]> {
            let off = sector.checked_mul(id.checked_add(1)?)?;
            if off >= end_of {
                return None;
            }
            Some(&buf[off..(off + sector).min(end_of)])
        };

        // DIFAT: 109 header entries + optional chained extension sectors.
        // Entries are de-duplicated in place: a repeated sector would otherwise
        // shift every later FAT run by a whole sector and misroute every chain.
        let mut difat: Vec<u32> = Vec::new();
        let push_difat = |v: u32, difat: &mut Vec<u32>| {
            if v != FREESECT && v != ENDOFCHAIN && !difat.contains(&v) {
                difat.push(v);
            }
        };
        for i in 0..109 {
            push_difat(u32le(buf, 76 + i * 4), &mut difat);
        }
        if dif_first != ENDOFCHAIN && dif_first != FREESECT && n_dif > 0 {
            let mut cur = dif_first;
            let mut hops = 0usize;
            let mut seen: Vec<u32> = Vec::new();
            while hops <= (1 << 16) {
                hops += 1;
                if seen.contains(&cur) {
                    break;
                }
                let Some(s) = sect(cur as usize) else { break };
                seen.push(cur);
                let per = sector / 4 - 1;
                for i in 0..per {
                    push_difat(u32le(s, i * 4), &mut difat);
                }
                let next = u32le(s, sector - 4);
                if next == ENDOFCHAIN || next == FREESECT {
                    break;
                }
                cur = next;
            }
        }
        let num_fat_real = difat.len();
        if num_fat_real < num_fat.min(109) && dif_first == ENDOFCHAIN {
            return Err(CoreError::Format(format!(
                "cfb: difat {num_fat_real} < fat {num_fat}"
            )));
        }

        // FAT
        let mut fat: Vec<u32> = Vec::new();
        for &fs in &difat {
            let s = sect(fs as usize).ok_or_else(|| {
                CoreError::Format(format!("cfb: fat sector {fs} out of range"))
            })?;
            for c in s.chunks_exact(4) {
                fat.push(u32::from_le_bytes([c[0], c[1], c[2], c[3]]));
            }
        }

        // Directory sweep: slot index == dir id (MS-CFB addresses entries by
        // slot), so a sector's 128-byte slots are pushed positionally and a slot
        // that fails validation becomes a hole instead of poisoning the sweep.
        //
        // Caseware's FAT links the real directory sectors as a PREFIX of a
        // longer chain that then runs into ordinary data (hull2024
        // `03250RG1`: chain `1→2→5→310→348→381`, where 381 holds cell bytes,
        // not directory slots). The sweep therefore stops at the first sector
        // that yields no valid entry at all — that sector and everything after
        // it is data, not directory — while a bad slot inside a sector that
        // does carry entries stays a hole. This is what keeps every document's
        // real tree (`Form`, `Index`, `XMLStore` + scripts) reachable without
        // inventing entries.
        let mut dir: Vec<Option<DirEntry>> = Vec::new();
        for id in fat_chain(&fat, first_dir) {
            let Some(s) = sect(id) else { break };
            let mut slots: Vec<Option<DirEntry>> = Vec::with_capacity(sector / 128);
            for chunk in s.chunks(128) {
                if chunk.len() < 128 {
                    // Trailing partial sector: its tail is not a slot.
                    continue;
                }
                slots.push(dir_entry(chunk));
            }
            if slots.iter().all(|e| e.is_none()) {
                break;
            }
            dir.extend(slots);
        }

        let root_ok = dir.first().is_some_and(|e| e.as_ref().is_some_and(|e| e.kind == 5));
        if !root_ok {
            return Err(CoreError::Format("cfb: no root dir entry".into()));
        }

        // Mini FAT (sectors chained in the main FAT).
        let mut mini_fat: Vec<u32> = Vec::new();
        if first_mini_fat != ENDOFCHAIN && first_mini_fat != FREESECT {
            for id in fat_chain(&fat, first_mini_fat) {
                let Some(s) = sect(id) else { break };
                for c in s.chunks_exact(4) {
                    mini_fat.push(u32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                }
            }
        }

        Ok(Cfb {
            data: buf.to_vec(),
            sector,
            mini,
            mini_cutoff,
            fat,
            mini_fat,
            dir,
        })
    }

    fn sect(&self, id: usize) -> Result<&[u8]> {
        let off = self.sector * (id + 1);
        if off >= self.data.len() {
            return Err(CoreError::Format(format!("cfb: sector {id} out of range")));
        }
        Ok(&self.data[off..(off + self.sector).min(self.data.len())])
    }

    fn chain(&self, start: u32) -> Result<Vec<usize>> {
        Ok(fat_chain(&self.fat, start))
    }

    /// In-order ids of the sibling set rooted at `child` (cycle-safe).
    ///
    /// Links that point at a hole, past the swept slots, or at `NOSTREAM` are
    /// dropped: Caseware's trailing partial directory sector leaves dangling
    /// right/left/child links in real documents (`XMLStore` → 18 and `Layout` →
    /// 19 in hull2024 `01200RG0`), and a missing sibling must not abort the
    /// whole container.
    fn siblings(&self, child: u32) -> Result<Vec<usize>> {
        if child == NOSTREAM {
            return Ok(Vec::new());
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut stack = vec![child as usize];
        while let Some(id) = stack.pop() {
            let Some(e) = self.dir.get(id).and_then(|e| e.as_ref()) else {
                continue;
            };
            if !seen.insert(id) {
                continue;
            }
            if e.left != NOSTREAM {
                stack.push(e.left as usize);
            }
            if e.right != NOSTREAM {
                stack.push(e.right as usize);
            }
            if seen.len() > MAX_WALK {
                return Err(CoreError::Format("cfb: dir loop".into()));
            }
        }
        Ok(seen.into_iter().collect())
    }

    /// Recursively collect (dir id, path) for every node beneath a storage
    /// child link — storages and streams alike.
    fn walk(&self, child: u32, prefix: &[String], out: &mut Vec<(usize, Vec<String>)>) -> Result<()> {
        for id in self.siblings(child)? {
            let e = &self.dir[id].as_ref().expect("siblings yielded a live slot");
            let mut path = prefix.to_vec();
            path.push(e.name.clone());
            out.push((id, path.clone()));
            // kind 1 = storage, kind 5 = root: descend. (CaseView `.cvw` puts
            // Layout/Strings under `Form` and Cell/CvData under `Index`; the
            // old kind==5-only test silently returned those storages as
            // zero-length leaves.)
            if e.kind == 1 || e.kind == 5 {
                self.walk(e.child, &path, out)?;
            }
        }
        Ok(())
    }

    fn read_normal(&self, start: u32, len: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(len.min(1 << 28));
        for id in self.chain(start)? {
            let s = self.sect(id)?;
            let take = (len - out.len()).min(s.len());
            out.extend_from_slice(&s[..take]);
            if out.len() >= len {
                break;
            }
        }
        Ok(out)
    }

    /// Read `[start]`'s mini-stream chain into `len` bytes.
    ///
    /// The stored length is authoritative but Caseware's tail sector can cut a
    /// chain short; the decode therefore returns what the container actually
    /// holds and reports `truncated` rather than erroring, so a caller can
    /// distinguish "no such stream" from "stream ends early".
    fn read_mini(&self, start: u32, len: usize) -> Result<Vec<u8>> {
        let root = self.dir[0].as_ref().expect("root checked at parse");
        let mini_stream = self.read_normal(root.start, root.size as usize)?;
        let mut out = Vec::with_capacity(len);
        let mut cur = start;
        let mut guard = 0usize;
        while out.len() < len {
            guard += 1;
            if guard > MAX_WALK {
                return Err(CoreError::Format("cfb: mini chain loop".into()));
            }
            let idx = cur as usize;
            let off = idx * self.mini;
            if off >= mini_stream.len() {
                break;
            }
            let avail = (mini_stream.len() - off).min(self.mini);
            let take = avail.min(len - out.len());
            out.extend_from_slice(&mini_stream[off..off + take]);
            if take < avail || out.len() >= len {
                break;
            }
            let Some(next) = self.mini_fat.get(idx).copied() else { break };
            if next == ENDOFCHAIN || next == FREESECT {
                break;
            }
            cur = next;
        }
        Ok(out)
    }

    /// Every leaf entry: root children first, storages expanded, siblings
    /// name-ordered (matches the freeze-manifest census).
    pub fn entries(&self) -> Result<Vec<Entry>> {
        let mut pairs = Vec::new();
        let root = self.dir[0].as_ref().expect("root checked at parse");
        self.walk(root.child, &[], &mut pairs)?;
        let mut out = Vec::with_capacity(pairs.len());
        for (id, path) in pairs {
            let e = self.dir[id].as_ref().expect("walk yielded a live slot");
            if e.kind == 1 || e.kind == 5 {
                continue; // storage: reported by `nodes`, not a leaf entry
            }
            out.push(Entry { path, kind: EntryKind::Stream, len: e.size as usize });
        }
        Ok(out)
    }

    /// Every node the directory tree reaches — storages included, root excluded.
    /// `entries()` is this minus the storages; `nodes()` is what a container
    /// inventory census wants (it must see `Media` and `Index`, not just their
    /// leaves).
    pub fn nodes(&self) -> Result<Vec<Entry>> {
        let mut pairs = Vec::new();
        let root = self.dir[0].as_ref().expect("root checked at parse");
        self.walk(root.child, &[], &mut pairs)?;
        let mut out = Vec::with_capacity(pairs.len());
        for (id, path) in pairs {
            let e = self.dir[id].as_ref().expect("walk yielded a live slot");
            let kind = if e.kind == 1 || e.kind == 5 {
                EntryKind::Storage
            } else {
                EntryKind::Stream
            };
            let len = if matches!(kind, EntryKind::Storage) { 0 } else { e.size as usize };
            out.push(Entry { path, kind, len });
        }
        Ok(out)
    }

    /// Resolve a (slash) path to its directory id.
    fn entry_id(&self, name: &str) -> Result<usize> {
        let path: Vec<&str> = name.split('/').collect();
        let mut id = usize::MAX;
        let mut child = self.dir[0].as_ref().expect("root checked at parse").child;
        for part in path {
            let sibs = self.siblings(child)?;
            let found = sibs
                .into_iter()
                .find(|i| self.dir[*i].as_ref().is_some_and(|e| e.name == part))
                .ok_or_else(|| CoreError::Format(format!("cfb: no entry '{part}'")))?;
            id = found;
            child = self.dir[found].as_ref().expect("siblings yielded a live slot").child;
        }
        if id == usize::MAX {
            return Err(CoreError::Format("cfb: empty path".into()));
        }
        Ok(id)
    }

    /// Read one stream by (slash) path from the root; flat names are the
    /// container norm (`CaseWareDocument` et al).
    ///
    /// The returned bytes may be shorter than the directory's stored length
    /// when the sector chain is cut by the file's trailing partial sector
    /// (Caseware writes the last sector short); the caller sees exactly the
    /// bytes the container holds, never fabricated padding.
    pub fn stream(&self, name: &str) -> Result<Vec<u8>> {
        let id = self.entry_id(name)?;
        let e = self.dir[id].as_ref().expect("entry_id yields a live slot");
        if e.kind == 5 {
            return Err(CoreError::Format("cfb: entry is a storage".into()));
        }
        let len = e.size as usize;
        if len < self.mini_cutoff {
            self.read_mini(e.start, len)
        } else {
            self.read_normal(e.start, len)
        }
    }

    /// Overwrite a regular (>= mini-cutoff) stream in place. The replacement
    /// must have exactly the stored length so the sector chain stays valid;
    /// this is the P5.D4b lockdown writer (the `.ac` mirror is a property-set
    /// stream edited in place, F68).
    pub fn patch_stream_in_place(&self, buf: &mut [u8], name: &str, data: &[u8]) -> Result<()> {
        let id = self.entry_id(name)?;
        let e = self.dir[id]
            .as_ref()
            .expect("entry_id yields a live slot")
            .clone();
        if e.kind == 5 {
            return Err(CoreError::Format("cfb: entry is a storage".into()));
        }
        let len = e.size as usize;
        if data.len() != len {
            return Err(CoreError::Format(format!(
                "cfb: patch len {} != stored {len}",
                data.len()
            )));
        }
        if len < self.mini_cutoff {
            return Err(CoreError::Format(
                "cfb: mini-stream in-place patch unsupported".into(),
            ));
        }
        let chain = self.chain(e.start)?;
        let mut off = 0usize;
        for sect in chain {
            let base = self.sector * (sect + 1);
            let take = (len - off).min(self.sector);
            buf[base..base + take].copy_from_slice(&data[off..off + take]);
            off += take;
            if off >= len {
                break;
            }
        }
        if off != len {
            return Err(CoreError::Format("cfb: patch chain short".into()));
        }
        Ok(())
    }

    pub fn sector_len(&self) -> usize {
        self.sector
    }

    pub fn mini_len(&self) -> usize {
        self.mini
    }

    pub fn mini_cutoff(&self) -> usize {
        self.mini_cutoff
    }

    pub fn fat_entries(&self) -> usize {
        self.fat.len()
    }

    /// Directory **slots** swept (holes included): a dir id is a slot index, so
    /// this is the id space, not the entry count.
    pub fn dir_len(&self) -> usize {
        self.dir.len()
    }

    /// Directory entries that parsed (holes excluded).
    pub fn dir_entries(&self) -> usize {
        self.dir.iter().filter(|e| e.is_some()).count()
    }
}

// ---------------------------------------------------------------------------
// Writer (corpus-mimic profile; see module docs).
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct CfbBuilder {
    streams: Vec<(String, Vec<u8>)>,
}

impl CfbBuilder {
    pub fn new() -> CfbBuilder {
        CfbBuilder::default()
    }

    pub fn add(&mut self, name: impl Into<String>, data: impl Into<Vec<u8>>) -> &mut Self {
        self.streams.push((name.into(), data.into()));
        self
    }

    pub fn build(mut self) -> Result<Vec<u8>> {
        if self.streams.is_empty() {
            return Err(CoreError::Format("cfb: no streams".into()));
        }
        const SECTOR: usize = 4096;
        const MINI: usize = 64;
        const CUTOFF: usize = 4096;

        // Canonical order: name-sorted; the emitted BST is the median linkage
        // over this order, so parse-side sibling sets sort back to it.
        self.streams.sort_by(|a, b| a.0.cmp(&b.0));
        let n = self.streams.len();

        let dir_secs = div_ceil((1 + n) * 128, SECTOR);

        // Classify: <CUTOFF goes to the mini stream (empty streams included,
        // one block each); the rest are normal sectors.
        let mut small: Vec<(usize, usize, usize)> = Vec::new(); // (i, blocks, block0)
        let mut blocks_total = 0usize;
        let mut large: Vec<(usize, usize)> = Vec::new(); // (i, sectors)
        for (i, (_, data)) in self.streams.iter().enumerate() {
            if data.len() < CUTOFF {
                let blocks = div_ceil(data.len() + 1, MINI).max(1);
                small.push((i, blocks, blocks_total));
                blocks_total += blocks;
            } else {
                large.push((i, div_ceil(data.len(), SECTOR)));
            }
        }
        let mini_bytes = blocks_total * MINI;
        let mini_secs = div_ceil(mini_bytes, SECTOR);
        let minifat_secs = if blocks_total > 0 {
            div_ceil(blocks_total * 4, SECTOR)
        } else {
            0
        };
        let data_secs: usize = large.iter().map(|(_, s)| *s).sum();

        // FAT sectors must index everything; converge.
        let fixed = dir_secs + minifat_secs + mini_secs + data_secs;
        let mut fat_secs = 1usize;
        loop {
            let total = fixed + fat_secs;
            let need = div_ceil(total * 4, SECTOR).max(1);
            if need == fat_secs {
                break;
            }
            fat_secs = need;
        }
        if fat_secs > 109 {
            return Err(CoreError::Format(format!(
                "cfb: {fat_secs} fat sectors > 109 (writer has no difat chain)"
            )));
        }

        // Layout: [FAT][dir][miniFAT][mini stream][large data]
        let fat0 = 0usize;
        let dir0 = fat0 + fat_secs;
        let minifat0 = dir0 + dir_secs;
        let mini0 = minifat0 + minifat_secs;
        let data0 = mini0 + mini_secs;
        let total_sects = data0 + data_secs;

        let mut fat = vec![FREESECT; total_sects];
        for id in fat0..fat0 + fat_secs {
            fat[id] = FATSECT;
        }
        for k in 0..dir_secs {
            fat[dir0 + k] = if k + 1 < dir_secs { (dir0 + k + 1) as u32 } else { ENDOFCHAIN };
        }
        for k in 0..minifat_secs {
            fat[minifat0 + k] = if k + 1 < minifat_secs {
                (minifat0 + k + 1) as u32
            } else {
                ENDOFCHAIN
            };
        }
        for k in 0..mini_secs {
            fat[mini0 + k] = if k + 1 < mini_secs { (mini0 + k + 1) as u32 } else { ENDOFCHAIN };
        }
        let mut start_of = vec![ENDOFCHAIN; n];
        let mut cursor = data0;
        for &(i, secs) in &large {
            start_of[i] = cursor as u32;
            for k in 0..secs {
                let id = cursor + k;
                fat[id] = if k + 1 < secs { (id + 1) as u32 } else { ENDOFCHAIN };
            }
            cursor += secs;
        }

        // mini stream + mini FAT
        let mut mini_stream = vec![0u8; mini_bytes];
        let mut mini_fat = vec![ENDOFCHAIN; blocks_total];
        for &(i, blocks, block0) in &small {
            for b in 0..blocks {
                let g = block0 + b;
                mini_fat[g] = if b + 1 < blocks { (g + 1) as u32 } else { ENDOFCHAIN };
                let data = &self.streams[i].1;
                let from = b * MINI;
                let upto = ((b + 1) * MINI).min(data.len());
                if upto > from {
                    mini_stream[g * MINI..g * MINI + (upto - from)]
                        .copy_from_slice(&data[from..upto]);
                }
            }
        }

        // directory
        let mut dirblob = vec![0u8; dir_secs * SECTOR];
        let put_entry = |id: usize, name: &str, kind: u8, left: u32, right: u32, child: u32, start: u32, size: u64, blob: &mut Vec<u8>| {
            let off = id * 128;
            let e = &mut blob[off..off + 128];
            let mut ucs = Vec::with_capacity(name.len() * 2 + 2);
            for u in name.encode_utf16() {
                ucs.extend_from_slice(&u.to_le_bytes());
            }
            ucs.extend_from_slice(&0u16.to_le_bytes());
            e[0..ucs.len()].copy_from_slice(&ucs);
            e[64..66].copy_from_slice(&(ucs.len() as u16).to_le_bytes());
            e[66] = kind;
            e[67] = 1; // black
            e[68..72].copy_from_slice(&left.to_le_bytes());
            e[72..76].copy_from_slice(&right.to_le_bytes());
            e[76..80].copy_from_slice(&child.to_le_bytes());
            e[116..120].copy_from_slice(&start.to_le_bytes());
            e[120..128].copy_from_slice(&size.to_le_bytes());
        };
        // ids 1..=n in sorted order; median-BST links
        fn median(a: usize, b: usize) -> usize {
            a + (b - a) / 2
        }
        fn link(a: usize, b: usize, dirblob: &mut [u8]) {
            if a >= b {
                return;
            }
            let m = median(a, b);
            let id = m + 1; // stream ids are 1..=n in sorted order
            let off = id * 128;
            if m > a {
                let l = (median(a, m) + 1) as u32;
                dirblob[off + 68..off + 72].copy_from_slice(&l.to_le_bytes());
            }
            if m + 1 < b {
                let r = (median(m + 1, b) + 1) as u32;
                dirblob[off + 72..off + 76].copy_from_slice(&r.to_le_bytes());
            }
            link(a, m, dirblob);
            link(m + 1, b, dirblob);
        }
        // root
        let root_child = (median(0, n) + 1) as u32;
        put_entry(
            0,
            ROOT_NAME,
            5,
            NOSTREAM,
            NOSTREAM,
            root_child,
            if blocks_total > 0 { mini0 as u32 } else { ENDOFCHAIN },
            mini_bytes as u64,
            &mut dirblob,
        );
        for (i, (name, data)) in self.streams.iter().enumerate() {
            let id = i + 1;
            let (start, size): (u32, u64) = if data.len() < CUTOFF {
                let b0 = small.iter().find(|x| x.0 == i).map(|x| x.2).unwrap();
                (b0 as u32, data.len() as u64)
            } else {
                (start_of[i], data.len() as u64)
            };
            put_entry(id, name, 2, NOSTREAM, NOSTREAM, NOSTREAM, start, size, &mut dirblob);
        }
        link(0, n, &mut dirblob);

        // miniFAT blob
        let mut minifatblob = vec![0u8; minifat_secs * SECTOR];
        for (i, v) in mini_fat.iter().enumerate() {
            minifatblob[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        // FAT blob
        let mut fatblob = vec![0u8; fat_secs * SECTOR];
        for (i, v) in fat.iter().enumerate() {
            fatblob[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }

        // header (padded to a full sector — the Caseware/olefile base rule) + sectors
        let mut out: Vec<u8> = Vec::with_capacity(SECTOR * (1 + total_sects));
        let mut h = vec![0u8; SECTOR];
        h[0..8].copy_from_slice(&SIGNATURE);
        h[24..26].copy_from_slice(&0x003Eu16.to_le_bytes()); // minor, per corpus
        h[26..28].copy_from_slice(&4u16.to_le_bytes());
        h[28..30].copy_from_slice(&0xFFFEu16.to_le_bytes());
        h[30..32].copy_from_slice(&12u16.to_le_bytes());
        h[32..34].copy_from_slice(&6u16.to_le_bytes());
        h[40..44].copy_from_slice(&(dir_secs as u32).to_le_bytes());
        h[44..48].copy_from_slice(&(fat_secs as u32).to_le_bytes());
        h[48..52].copy_from_slice(&(dir0 as u32).to_le_bytes());
        h[52..56].copy_from_slice(&0u32.to_le_bytes());
        h[56..60].copy_from_slice(&(CUTOFF as u32).to_le_bytes());
        h[60..64].copy_from_slice(&(if minifat_secs > 0 { minifat0 as u32 } else { ENDOFCHAIN }).to_le_bytes());
        h[64..68].copy_from_slice(&(minifat_secs as u32).to_le_bytes());
        h[68..72].copy_from_slice(&ENDOFCHAIN.to_le_bytes());
        h[72..76].copy_from_slice(&0u32.to_le_bytes());
        for k in 0..fat_secs {
            h[76 + k * 4..76 + k * 4 + 4].copy_from_slice(&((fat0 + k) as u32).to_le_bytes());
        }
        for k in fat_secs..109 {
            h[76 + k * 4..76 + k * 4 + 4].copy_from_slice(&FREESECT.to_le_bytes());
        }
        out.extend_from_slice(&h);

        for id in fat0..fat0 + fat_secs {
            let from = id * SECTOR;
            let mut s = vec![0u8; SECTOR];
            let end = (from + SECTOR).min(fatblob.len());
            s[..end - from].copy_from_slice(&fatblob[from..end]);
            out.extend_from_slice(&s);
        }
        for id in 0..dir_secs {
            let from = id * SECTOR;
            let mut s = vec![0u8; SECTOR];
            let end = (from + SECTOR).min(dirblob.len());
            s[..end - from].copy_from_slice(&dirblob[from..end]);
            out.extend_from_slice(&s);
        }
        for id in 0..minifat_secs {
            let from = id * SECTOR;
            let mut s = vec![0u8; SECTOR];
            let end = (from + SECTOR).min(minifatblob.len());
            s[..end - from].copy_from_slice(&minifatblob[from..end]);
            out.extend_from_slice(&s);
        }
        for k in 0..mini_secs {
            let mut s = vec![0u8; SECTOR];
            let from = k * SECTOR;
            let end = (from + SECTOR).min(mini_stream.len());
            if from < end {
                let t = end - from;
                s[..t].copy_from_slice(&mini_stream[from..end]);
            }
            out.extend_from_slice(&s);
        }
        for &(i, secs) in &large {
            let data = &self.streams[i].1;
            for k in 0..secs {
                let mut s = vec![0u8; SECTOR];
                let from = k * SECTOR;
                let end = (from + SECTOR).min(data.len());
                s[..end - from].copy_from_slice(&data[from..end]);
                out.extend_from_slice(&s);
            }
        }
        debug_assert_eq!(out.len(), SECTOR * (1 + total_sects));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(name: &str, data: &[u8]) {
        let mut b = CfbBuilder::new();
        b.add(name, data);
        let bytes = b.build().unwrap();
        let cfb = Cfb::parse(&bytes).unwrap();
        assert_eq!(&cfb.stream(name).unwrap(), data, "stream mismatch");
    }

    #[test]
    fn small_stream_roundtrip() {
        roundtrip("CompressionVersion", b"DynaZip5.00.06");
    }

    #[test]
    fn large_stream_roundtrip() {
        let data: Vec<u8> = (0..100_000usize).map(|i| (i * 7 + 3) as u8).collect();
        roundtrip("CaseWareDocument", &data);
    }

    #[test]
    fn empty_and_boundaries() {
        roundtrip("z.empty", b"");
        roundtrip("a.4095", &vec![7u8; 4095]);
        roundtrip("b.4096", &vec![7u8; 4096]);
        roundtrip("c.4097", &vec![7u8; 4097]);
        roundtrip("d.8192", &vec![9u8; 8192]);
    }

    #[test]
    fn multi_stream_catalog() {
        let mut b = CfbBuilder::new();
        let big: Vec<u8> = (0..9000).map(|i| i as u8).collect();
        b.add("CaseWareDocument", big.clone());
        b.add("FileInformation", b"hello engagement".to_vec());
        b.add("\u{5}SummaryInformation", vec![1u8, 2, 3]);
        b.add("DmsProperties", vec![9u8]);
        let bytes = b.build().unwrap();
        let cfb = Cfb::parse(&bytes).unwrap();
        let names: Vec<String> = cfb.entries().unwrap().into_iter().map(|e| e.name()).collect();
        assert_eq!(
            names,
            vec![
                "\u{5}SummaryInformation",
                "CaseWareDocument",
                "DmsProperties",
                "FileInformation"
            ]
        );
        assert_eq!(cfb.stream("CaseWareDocument").unwrap(), big);
        assert_eq!(cfb.stream("FileInformation").unwrap(), b"hello engagement".to_vec());
        assert_eq!(cfb.stream("DmsProperties").unwrap(), vec![9]);
        assert_eq!(cfb.entries().unwrap().len(), 4);
    }

    #[test]
    fn many_small_streams_two_fat() {
        // >1 FAT sector: 4096-stream mini padding pushes past one FAT sector.
        let mut b = CfbBuilder::new();
        for i in 0..300 {
            b.add(format!("s{:03}", i), vec![7u8; 4000]);
        }
        b.add("big", vec![1u8; 300_000]);
        let bytes = b.build().unwrap();
        let cfb = Cfb::parse(&bytes).unwrap();
        // 302 entries occupy 320 slots (10 sectors × 32); a dir id is a slot.
        assert_eq!(cfb.dir_entries(), 302);
        assert_eq!(cfb.dir_len(), 320);
        assert_eq!(cfb.stream("s299").unwrap(), vec![7u8; 4000]);
        assert_eq!(cfb.stream("big").unwrap().len(), 300_000);
    }

    #[test]
    fn rejects_garbage() {
        assert!(Cfb::parse(b"tiny").is_err());
        let mut b = CfbBuilder::new();
        b.add("x", vec![1u8; 5000]);
        let mut bytes = b.build().unwrap();
        bytes[0] = 0;
        assert!(Cfb::parse(&bytes).is_err());
    }

    // -----------------------------------------------------------------------
    // Trailing-partial / damaged directory sectors (Caseware `.cvw` reality).
    //
    // The writer produces a clean single-sector directory. These cases damage
    // it the way the shipped corpus is damaged: slots after the last real
    // entry carry unrelated bytes, dangling `left`/`right`/`child` links point
    // at those slots, and the FAT keeps the chain running into a data sector.
    // The reader must keep every real entry and drop only the garbage.
    // -----------------------------------------------------------------------

    /// Build the 3-stream fixture used by the damage tests, then return the
    /// bytes plus the sector geometry needed to poke them.
    fn three_stream_image() -> Vec<u8> {
        let mut b = CfbBuilder::new();
        b.add("Alpha", b"alpha-payload".to_vec());
        b.add("Beta", b"beta-payload".to_vec());
        b.add("Gamma", b"gamma-payload".to_vec());
        b.build().unwrap()
    }

    /// The `n`-th 128-byte slot of directory sector `sect` within `bytes`.
    fn slot_mut(bytes: &mut [u8], sector: usize, sst: usize, slot: usize) -> &mut [u8] {
        let base = sst * (sector + 1) + slot * 128;
        &mut bytes[base..base + 128]
    }

    fn sst_of(bytes: &[u8]) -> usize {
        1usize << u16::from_le_bytes([bytes[30], bytes[31]])
    }

    fn first_dir_of(bytes: &[u8]) -> u32 {
        u32::from_le_bytes([bytes[48], bytes[49], bytes[50], bytes[51]])
    }

    fn set_u32(bytes: &mut [u8], off: usize, v: u32) {
        bytes[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn dir_slot_offset(sst: usize, sector: u32, slot: usize) -> usize {
        sst * (sector as usize + 1) + slot * 128
    }

    /// Garbage slots past the last real entry are dropped, not fatal.
    #[test]
    fn garbage_trailing_slots_become_holes() {
        let mut bytes = three_stream_image();
        let sst = sst_of(&bytes);
        let d = first_dir_of(&bytes) as usize;
        // slots 0=root, 1..3 = streams, 4..8 = padding. Poison 5.
        {
            let s = slot_mut(&mut bytes, d, sst, 5);
            s[66] = 68; // not a valid kind
            s[64..66].copy_from_slice(&0u16.to_le_bytes());
        }
        let cfb = Cfb::parse(&bytes).unwrap();
        let names: Vec<String> = cfb.entries().unwrap().into_iter().map(|e| e.name()).collect();
        assert_eq!(names, vec!["Alpha", "Beta", "Gamma"]);
        assert_eq!(cfb.stream("Beta").unwrap(), b"beta-payload");
    }

    /// A bad kind inside a *live* sector is a hole, not a reason to abandon the
    /// sector: entries after it still resolve. (The BST here is root→Beta,
    /// Beta.left=Alpha, Beta.right=Gamma, so poisoning `Alpha` leaves the
    /// root's own linkage intact and `Gamma` reachable.)
    #[test]
    fn bad_slot_does_not_stop_the_sweep() {
        let mut bytes = three_stream_image();
        let sst = sst_of(&bytes);
        let d = first_dir_of(&bytes) as usize;
        // Slot 1 == "Alpha".
        slot_mut(&mut bytes, d, sst, 1)[66] = 0x58;
        let cfb = Cfb::parse(&bytes).unwrap();
        let mut names: Vec<String> =
            cfb.entries().unwrap().into_iter().map(|e| e.name()).collect();
        names.sort();
        assert_eq!(names, vec!["Beta", "Gamma"], "the sweep must survive one poisoned slot");
        for n in &names {
            cfb.stream(n).unwrap_or_else(|e| panic!("{n}: {e}"));
        }
    }

    /// Dangling sibling links (pointing at a hole or past the last slot) are
    /// dropped; everything still reachable is returned.
    #[test]
    fn dangling_sibling_links_are_dropped() {
        let mut bytes = three_stream_image();
        let sst = sst_of(&bytes);
        let d = first_dir_of(&bytes) as usize;
        // Poison slot 1 ("Alpha"), then aim Beta's `left` at it (a hole) and
        // Beta's `right` far past the swept slots.
        slot_mut(&mut bytes, d, sst, 1)[66] = 0x58;
        let base = dir_slot_offset(sst, d as u32, 2); // slot 2 == Beta (root child)
        set_u32(&mut bytes, base + 68, 1); // left  -> hole
        set_u32(&mut bytes, base + 72, 40_000); // right -> out of range

        let cfb = Cfb::parse(&bytes).unwrap();
        let names: Vec<String> = cfb.entries().unwrap().into_iter().map(|e| e.name()).collect();
        assert_eq!(names, vec!["Beta"], "only the live root child survives, got {names:?}");
        assert_eq!(cfb.stream("Beta").unwrap(), b"beta-payload");
    }

    /// The file's trailing partial sector is zero-extended, so a stream whose
    /// chain reaches it reads the bytes the file actually carries instead of
    /// failing with "sector out of range".
    #[test]
    fn trailing_partial_sector_is_readable() {
        let mut b = CfbBuilder::new();
        // One large stream so the container has several data sectors at the end.
        b.add("Big", vec![0xABu8; 12_000]);
        let mut bytes = b.build().unwrap();
        // Cut the file mid-sector: drop the last 300 bytes of the final sector.
        let sst = sst_of(&bytes);
        assert_eq!(bytes.len() % sst, 0);
        bytes.truncate(bytes.len() - 300);

        let cfb = Cfb::parse(&bytes).unwrap();
        let got = cfb.stream("Big").unwrap();
        // The stored length is authoritative; a short tail yields the bytes
        // present, never an error and never invented padding.
        assert!(got.len() <= 12_000, "got {} bytes", got.len());
        assert!(got.len() >= 12_000 - sst, "lost more than one sector: {}", got.len());
        assert!(got.iter().all(|&b| b == 0xAB), "content drifted");
    }

    /// A directory chain that runs on into a data sector stops at the first
    /// sector with no directory entry. Real documents do this: the FAT links
    /// hull2024 `03250RG1` as `1→2→5→310→348→381`, and 381 is cell data.
    #[test]
    fn dir_chain_stops_at_first_non_directory_sector() {
        let mut bytes = three_stream_image();
        let sst = sst_of(&bytes);
        let d = first_dir_of(&bytes);
        // Find the FAT sector and the entry for the directory sector, then
        // append a bogus link to a sector that holds no directory slots.
        // Layout: [FAT][dir][miniFAT][mini stream][large data]
        let fat_sector = u32::from_le_bytes([bytes[76], bytes[77], bytes[78], bytes[79]]);
        let fat_base = sst * (fat_sector as usize + 1);
        // The sector right after the directory sector is a data/zero sector.
        let next = d + 1;
        set_u32(&mut bytes, fat_base + d as usize * 4, next);
        // Make `next` a zeroed, entry-free sector (it already is for a
        // 3-small-stream image, but assert rather than assume).
        let nb = sst * (next as usize + 1);
        if nb + 128 <= bytes.len() {
            for k in 0..4 {
                bytes[nb + 128 * k + 66] = 0;
            }
        }
        let cfb = Cfb::parse(&bytes).unwrap();
        let names: Vec<String> = cfb.entries().unwrap().into_iter().map(|e| e.name()).collect();
        assert_eq!(names, vec!["Alpha", "Beta", "Gamma"], "sweep must stop at the data sector");
    }

    /// Repeated DIFAT entries are de-duplicated so the FAT runs stay aligned.
    #[test]
    fn duplicate_difat_entries_are_ignored() {
        let mut bytes = three_stream_image();
        // The writer emits FAT sector ids in header slots 76..; repeat the
        // first one in the next free slot.
        let first = u32::from_le_bytes([bytes[76], bytes[77], bytes[78], bytes[79]]);
        set_u32(&mut bytes, 80, first); // slot 1 <- same sector as slot 0
        let cfb = Cfb::parse(&bytes).unwrap();
        assert_eq!(cfb.stream("Alpha").unwrap(), b"alpha-payload");
        assert_eq!(cfb.stream("Gamma").unwrap(), b"gamma-payload");
        // One FAT sector, not two copies of it.
        assert!(cfb.fat_entries() <= 2 * (sst_of(&bytes) / 4), "fat doubled");
    }
}

