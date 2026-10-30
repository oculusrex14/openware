//! `Cell` stream: the worksheet's cell directory (F1).
//!
//! Layout established against the shipped corpus (193 `.cvw`, 1,342,416 records,
//! 100% consumption of every `Cell` stream — `the tooling scripts*` oracle):
//!
//! ```text
//! Cell stream := header(16) then { name(B)  layout_offset(u32 LE) }*
//! header      := u32 version(=4) u32 marker(=114) u32 aux u32 cell_count
//! ```
//!
//! Proven invariants over the 193-file corpus (1,342,416 cells): `version` is
//! 4 and `marker` is 114 in every file; `cell_count` (header word 3) equals the
//! number of walked records in 193/193; `aux` (word 2) is not yet explained and
//! is preserved raw.
//!
//! The record has **no explicit name length**. Name ends are recovered by
//! cross-checking against the `Layout` stream: a candidate split `(name, off)`
//! is accepted only when `name` occurs inside the Layout block that starts at
//! `off` (within the first [`NAME_WINDOW`] bytes). Offsets are strictly
//! ascending, which bounds the search. That check is what makes the walk
//! unambiguous; the heuristic is stated in full here.
//!
//! Cell names are arbitrary bytes: system cells carry 0x1C/0x80/0x90 class
//! prefixes (e.g. `\x1caEXR.C9`), so names are decoded as Latin-1, one char
//! per byte.

/// Bytes of a Layout block scanned when cross-checking a candidate name.
pub const NAME_WINDOW: usize = 96;
/// Longest cell name accepted by the parser.
pub const MAX_NAME: usize = 40;
/// Constant record marker observed in every corpus `Cell` stream.
pub const MARKER: u32 = 114;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellEntry {
    /// Cell name, Latin-1 decoded (byte-preserving).
    pub name: String,
    /// Offset of this cell's block in the `Layout` stream.
    pub layout_offset: u32,
}

impl CellEntry {
    /// The original name bytes (Latin-1 round-trip; needed for byte search).
    pub fn raw_name(&self) -> Vec<u8> {
        self.name.chars().map(|c| c as u32 as u8).collect()
    }
}

#[derive(Debug, Clone)]
pub struct CellDirectory {
    /// The four header words: `[version=4, marker=114, a, b]`.
    pub header: [u32; 4],
    /// Records in stream order (Layout offsets ascending).
    pub cells: Vec<CellEntry>,
    /// Bytes consumed by the walk (normally == cell stream length).
    pub consumed: usize,
    /// True when the walk reached the end of the stream.
    pub complete: bool,
}

impl CellDirectory {
    /// Cell count declared in the header (word 3). Equals [`Self::len`] on
    /// every corpus file, so it is the walk's integrity check.
    pub fn declared_count(&self) -> u32 {
        self.header[3]
    }

    /// Walk the `Cell` stream, using `layout` as the name-length oracle.
    pub fn parse(cell: &[u8], layout: &[u8]) -> CellDirectory {
        if cell.len() < 16 {
            return CellDirectory {
                header: [0; 4],
                cells: Vec::new(),
                consumed: 0,
                complete: false,
            };
        }
        let header = [
            u32::from_le_bytes([cell[0], cell[1], cell[2], cell[3]]),
            u32::from_le_bytes([cell[4], cell[5], cell[6], cell[7]]),
            u32::from_le_bytes([cell[8], cell[9], cell[10], cell[11]]),
            u32::from_le_bytes([cell[12], cell[13], cell[14], cell[15]]),
        ];
        let mut cells = Vec::new();
        let mut pos = 16usize;
        let mut prev: i64 = -1;
        while pos + 4 < cell.len() {
            let mut hit = None;
            let max_end = (pos + MAX_NAME).min(cell.len() - 3);
            for end in pos + 1..=max_end {
                let off = u32::from_le_bytes([cell[end], cell[end + 1], cell[end + 2], cell[end + 3]]);
                if (off as i64) < prev || off as usize >= layout.len() {
                    continue;
                }
                if layout_block_contains(layout, off as usize, &cell[pos..end]) {
                    hit = Some((end, off));
                    break;
                }
            }
            let Some((end, off)) = hit else { break };
            cells.push(CellEntry {
                name: latin1(&cell[pos..end]),
                layout_offset: off,
            });
            prev = off as i64;
            pos = end + 4;
        }
        let complete = pos + 4 >= cell.len();
        CellDirectory {
            header,
            cells,
            consumed: pos,
            complete,
        }
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Cell names only, in directory order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.cells.iter().map(|c| c.name.as_str())
    }
}

fn layout_block_contains(layout: &[u8], off: usize, name: &[u8]) -> bool {
    let end = (off + NAME_WINDOW).min(layout.len());
    if off >= end || name.is_empty() || name.len() > end - off {
        return false;
    }
    layout[off..end].windows(name.len()).any(|w| w == name)
}

/// Byte-preserving Latin-1 decode (cell names are not UTF-8).
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic vector: header declares 3 cells; each name lives at the Layout
    /// offset that follows it, so the oracle resolves every name boundary.
    #[test]
    fn walks_synthetic_directory_fully() {
        let mut cell = Vec::new();
        for w in [4u32, MARKER, 0, 3] {
            cell.extend_from_slice(&w.to_le_bytes());
        }
        for (name, off) in [("A1", 0x10u32), ("B2", 0x20), ("C3", 0x30)] {
            cell.extend_from_slice(name.as_bytes());
            cell.extend_from_slice(&off.to_le_bytes());
        }
        let mut layout = vec![0u8; 0x60];
        layout[0x10..0x12].copy_from_slice(b"A1");
        layout[0x20..0x22].copy_from_slice(b"B2");
        layout[0x30..0x32].copy_from_slice(b"C3");

        let dir = CellDirectory::parse(&cell, &layout);
        assert_eq!(dir.header[0], 4);
        assert_eq!(dir.header[1], MARKER);
        assert_eq!(dir.declared_count(), 3);
        assert_eq!(dir.len(), 3);
        assert!(dir.complete, "walk must consume the whole stream");
        assert_eq!(dir.cells[0].name, "A1");
        assert_eq!(dir.cells[2].layout_offset, 0x30);
        assert_eq!(dir.cells[2].raw_name(), b"C3");
    }

    /// Control-byte names (system cells) round-trip as Latin-1 bytes.
    #[test]
    fn control_byte_names_are_byte_preserving() {
        let mut cell = Vec::new();
        for w in [4u32, MARKER, 0, 1] {
            cell.extend_from_slice(&w.to_le_bytes());
        }
        let name = b"\x1caEXR.C9";
        cell.extend_from_slice(name);
        cell.extend_from_slice(&0x10u32.to_le_bytes());
        let mut layout = vec![0u8; 0x80];
        layout[0x10..0x10 + name.len()].copy_from_slice(name);
        let dir = CellDirectory::parse(&cell, &layout);
        assert_eq!(dir.len(), 1);
        assert_eq!(dir.cells[0].raw_name(), name);
    }

    #[test]
    fn empty_and_short_streams_are_safe() {
        let d = CellDirectory::parse(&[], b"");
        assert!(d.is_empty() && !d.complete);
        let d = CellDirectory::parse(&[1, 2, 3], b"");
        assert!(d.is_empty() && !d.complete);
    }
}

