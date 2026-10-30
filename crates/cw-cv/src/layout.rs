//! `Layout` stream: the packed cell-block store (F1) + the cell→text binding.
//!
//! Proven shape (193/193 corpus files, `layout` sizes 15 KiB…3 MiB):
//!
//! ```text
//! Layout := magic("CV" 0xDBAC5643) version(u32) … build/block-size(u32) …
//!           TextInfo chunk (at 0x60) … index blocks … cell blocks …
//! cell block := <format/style bytes> <cell name> <attribute bytes> <trailer(7)>
//! ```
//!
//! The `Cell` directory points at block starts. Block length is the distance to
//! the next cell offset (the last block runs to end of stream). The name sits at
//! a **variable** offset inside the block (observed 3…31 bytes), so it is
//! located by searching for the name bytes the directory reported. Names are
//! NUL/space padded, never length-prefixed.
//!
//! ## Cell → `Strings` pool binding (established)
//!
//! Every cell block ends in a fixed **7-byte trailer**:
//!
//! ```text
//! trailer := u8 flag  u16be slot  u32 zero
//! ```
//!
//! `slot` is **not** a direct pool index: the pool's first two physical entries
//! are a zero-length entry and a two-byte control entry (`0x01 0x02`) that are
//! never addressed by a cell, so the addressing space starts at physical entry
//! [`POOL_INDEX_BASE`] = 2. The text for a cell is therefore
//! `pool.entries[slot + POOL_INDEX_BASE]`.
//!
//! Verified on the staged v24 document `03250RG0` for all four annotated
//! records, and the +2 shift is what makes the fourth (`@@FIRM`) bind:
//!
//! | cell | trailer slot | physical entry | text |
//! |---|---|---|---|
//! | ` 1ML3` | `0x002F` | 49 | `General` |
//! | `00CVR` | `0x0048` | 74 | `Understanding the Client and its Industry` |
//! | `00KLL` | `0x006B` | 109 | `field("SH","AD3",docnum())` |
//! | `@@FIRM` | `0x0091` | 147 | `rc(1#)=""&rc(1#)=""` |
//!
//! The `flag` byte is `0` or `1`; its meaning is not established, so it is
//! preserved raw. `zero` is 0 in every record that carries the trailer, but a
//! minority of blocks (system cells such as `@QHELP2`, `PPCLOSE`, `PPDOCID`)
//! end in a different 8-byte form whose second half is a layout offset rather
//! than zero, so the trailer is only accepted when the `zero` word is 0.

/// First four magic bytes of every corpus `Layout` stream (`"CV"` + 0xDBAC).
pub const MAGIC: [u8; 4] = [0x43, 0x56, 0xAC, 0xDB];

/// Physical pool entry that addressing slot 0 selects.
///
/// The `Strings` pool opens with two entries that no cell addresses (entry 0 is
/// zero-length, entry 1 is the two-byte control run `0x01 0x02`), so a block
/// trailer's `slot` names the pool entry at `slot + POOL_INDEX_BASE`.
pub const POOL_INDEX_BASE: usize = 2;

/// Bytes of the fixed trailer that closes a cell block.
pub const TRAILER_LEN: usize = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutHeader {
    pub magic_ok: bool,
    /// u32 at 0x08 (the document format version: 22/23/24 in the corpus).
    pub version: u32,
    /// u32 at 0x14 (the index block size: 164 on `03250RG0`, 0xA4).
    pub block_size: u32,
}

impl LayoutHeader {
    pub fn parse(layout: &[u8]) -> LayoutHeader {
        let u32_at = |off: usize| {
            layout
                .get(off..off + 4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .unwrap_or(0)
        };
        LayoutHeader {
            magic_ok: layout.len() >= 4 && layout[0..4] == MAGIC,
            version: u32_at(8),
            block_size: u32_at(20),
        }
    }
}

/// The 7-byte block trailer: `u8 flag, u16be slot, u32 zero`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockTrailer {
    /// First trailer byte; `0` or `1` on the staged documents, meaning open.
    pub flag: u8,
    /// Big-endian addressing slot into the `Strings` pool (add
    /// [`POOL_INDEX_BASE`] for the physical entry).
    pub slot: u16,
}

impl BlockTrailer {
    /// Read the trailer from the last 7 bytes of a cell block.
    ///
    /// Returns `None` when the block is too short, when the trailing `zero`
    /// word is not 0 (the block uses the alternate system-cell tail), or when
    /// the flag byte is outside the observed `{0, 1}` set.
    pub fn parse(block: &[u8]) -> Option<BlockTrailer> {
        if block.len() < TRAILER_LEN {
            return None;
        }
        let t = &block[block.len() - TRAILER_LEN..];
        let zero = u32::from_le_bytes([t[3], t[4], t[5], t[6]]);
        if zero != 0 || t[0] > 1 {
            return None;
        }
        Some(BlockTrailer {
            flag: t[0],
            slot: u16::from_be_bytes([t[1], t[2]]),
        })
    }

    /// Physical entry index in a decoded pool, when the slot can address one.
    pub fn pool_index(self) -> usize {
        self.slot as usize + POOL_INDEX_BASE
    }
}

/// One cell block, keyed by the directory entry that pointed at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellBlock {
    pub name: String,
    pub offset: u32,
    pub len: u32,
    /// Offset of the name inside the block (absolute, not block-relative).
    pub name_offset: Option<u32>,
    /// The block's `Strings`-pool trailer, when the block carries one.
    pub trailer: Option<BlockTrailer>,
}

impl CellBlock {
    /// The `Strings` pool slot this cell addresses, if any.
    pub fn pool_slot(&self) -> Option<u16> {
        self.trailer.map(|t| t.slot)
    }
}

/// Pair directory offsets with block spans and locate each name.
///
/// `cells` must be in ascending-offset order (the `Cell` walk guarantees it).
pub fn blocks(layout: &[u8], cells: &[crate::cell::CellEntry], window: usize) -> Vec<CellBlock> {
    let mut out = Vec::with_capacity(cells.len());
    for (i, c) in cells.iter().enumerate() {
        let start = c.layout_offset as usize;
        let end = cells
            .get(i + 1)
            .map(|n| n.layout_offset as usize)
            .unwrap_or(layout.len())
            .min(layout.len());
        if start >= end {
            out.push(CellBlock {
                name: c.name.clone(),
                offset: c.layout_offset,
                len: 0,
                name_offset: None,
                trailer: None,
            });
            continue;
        }
        let hay = &layout[start..end.min(start + window.max(c.name.len()))];
        let raw = c.raw_name();
        let name_offset = hay
            .windows(raw.len())
            .position(|w| w == raw.as_slice())
            .map(|p| (start + p) as u32);
        let block = &layout[start..end];
        out.push(CellBlock {
            name: c.name.clone(),
            offset: c.layout_offset,
            len: (end - start) as u32,
            name_offset,
            trailer: BlockTrailer::parse(block),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::CellEntry;

    fn trailer(flag: u8, slot: u16) -> [u8; 7] {
        let [hi, lo] = slot.to_be_bytes();
        [flag, hi, lo, 0, 0, 0, 0]
    }

    #[test]
    fn header_constants() {
        let mut lay = vec![0u8; 64];
        lay[0..4].copy_from_slice(&MAGIC);
        lay[8..12].copy_from_slice(&24u32.to_le_bytes());
        lay[20..24].copy_from_slice(&0xA4u32.to_le_bytes());
        let h = LayoutHeader::parse(&lay);
        assert!(h.magic_ok);
        assert_eq!(h.version, 24);
        assert_eq!(h.block_size, 0xA4);
        assert!(!LayoutHeader::parse(&[0; 3]).magic_ok);
    }

    #[test]
    fn blocks_split_on_next_offset() {
        let mut lay = vec![0u8; 64];
        lay[10..12].copy_from_slice(b"A1");
        lay[30..32].copy_from_slice(b"B2");
        let cells = vec![
            CellEntry { name: "A1".into(), layout_offset: 8 },
            CellEntry { name: "B2".into(), layout_offset: 28 },
        ];
        let b = blocks(&lay, &cells, 32);
        assert_eq!(b[0].len, 20);
        assert_eq!(b[0].name_offset, Some(10));
        assert_eq!(b[1].len, 36);
        assert_eq!(b[1].name_offset, Some(30));
    }

    /// The four annotated `03250RG0` trailers, verbatim from the stream:
    /// the slot is big-endian and the pool entry is `slot + 2`.
    #[test]
    fn trailer_law_matches_annotated_records() {
        // " 1ML3" -> ... 01 00 2f 00 00 00 00  => flag 1, be-slot 0x002F
        assert_eq!(BlockTrailer::parse(&trailer(1, 0x2F)).unwrap().slot, 0x2F);
        assert_eq!(BlockTrailer::parse(&trailer(1, 0x2F)).unwrap().flag, 1);
        assert_eq!(BlockTrailer::parse(&trailer(1, 0x2F)).unwrap().pool_index(), 49);
        // "00CVR" -> flag 1, be-slot 0x0048 -> pool[74]
        assert_eq!(BlockTrailer::parse(&trailer(1, 0x48)).unwrap().pool_index(), 74);
        // "00KLL" -> flag 1, be-slot 0x006B -> pool[109]
        assert_eq!(BlockTrailer::parse(&trailer(1, 0x6B)).unwrap().pool_index(), 109);
        // "@@FIRM" -> flag 1, be-slot 0x0091 -> pool[147]
        assert_eq!(BlockTrailer::parse(&trailer(1, 0x91)).unwrap().pool_index(), 147);
    }

    /// A little-endian read of the same bytes would be a different slot, so the
    /// byte order is load-bearing (0x2F00 vs 0x002F).
    #[test]
    fn trailer_slot_is_big_endian() {
        let t = BlockTrailer::parse(&trailer(1, 0x0048)).unwrap();
        assert_eq!(t.slot, 0x48);
        assert_ne!(t.slot, 0x4800);
    }

    /// The alternate system-cell tail (non-zero trailing word) is rejected
    /// rather than mis-read as a pool slot.
    #[test]
    fn alternate_tail_is_rejected() {
        let mut b = [0u8; 7];
        b[0] = 0;
        b[1] = 0x02;
        b[2] = 0xD3;
        b[6] = 0x00; // zero word still 0 here
        assert!(BlockTrailer::parse(&b).is_some());
        // `@QHELP2` ends `00 02 d3 00 00 00 00` -> flag 0, slot 0x02D3 (723)
        assert_eq!(BlockTrailer::parse(&b).unwrap().slot, 723);

        // a block ending in a real layout offset instead of zero is not a trailer
        let mut c = [0u8; 7];
        c[1] = 0x08;
        c[2] = 0x7a;
        c[3] = 1; // tail word 0x00000001
        assert!(BlockTrailer::parse(&c).is_none());
    }

    #[test]
    fn short_block_has_no_trailer() {
        assert!(BlockTrailer::parse(&[1, 0, 0x2f, 0, 0]).is_none());
        assert!(BlockTrailer::parse(&[]).is_none());
    }
}
