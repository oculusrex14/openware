//! `.cvw` / `.cvv` CaseView document container (F1).
//!
//! Every shipped file in the corpus is a **compound file (CFB/OLE2)** whose
//! root carries flat streams (F20 substrate; no `CaseWareDocument` storage):
//!
//! ```text
//! Root Entry
//!   Form Data Scripts Index Media XMLStore   (empty storages)
//!   Layout            packed cell blocks + TextInfo chunk
//!   Cell              cell directory  (see cell.rs)
//!   CvData            FORM code / validity block (see cvdata.rs)
//!   Strings           compressed string pool (header + block)
//!   Para Sect Table Styles Bookmarks Note NoteNumStyles Annotate
//!   XMLInfo XData Features  (+ document scripts AJE*/Check* on import sheets)
//! ```
//!
//! `CvData` is empty on pure form templates (e.g. `EXPO0IS0.cvw`); everything
//! else is present on both templates and populated worksheets. Stream order in
//! the directory is name-sorted by the CFB tree, not physical.
//!
//! `.cvv` is the same container (CaseView view/statement document); the corpus
//! currently carries `.cvw` only, so the extension is accepted but not yet
//! differentiated.

use cw_core::cfb::Cfb;
use cw_core::error::Result;

use crate::cell::{CellDirectory, CellEntry};
use crate::cvdata::CvData;
use crate::layout::{self, CellBlock, LayoutHeader};
use crate::strings;

/// Magic that opens the compressed block of the `Strings` stream.
pub const STRINGS_BLOCK_MAGIC: u32 = 0x0201_4B50; // "PK\x01\x02"

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInfo {
    pub name: String,
    pub len: usize,
}

/// Header of the `Strings` stream: 12 bytes then a compressed block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StringsHeader {
    /// u32 at 0x00 (2 in every corpus file).
    pub version: u32,
    /// u32 at 0x04: uncompressed pool size.
    pub uncompressed_len: u32,
    /// u32 at 0x08: string count.
    pub count: u32,
    /// u32 at 0x0C (0x02014B50 in every corpus file).
    pub block_magic: u32,
}

impl StringsHeader {
    pub fn parse(s: &[u8]) -> Option<StringsHeader> {
        if s.len() < 16 {
            return None;
        }
        let u = |o: usize| u32::from_le_bytes([s[o], s[o + 1], s[o + 2], s[o + 3]]);
        Some(StringsHeader {
            version: u(0),
            uncompressed_len: u(4),
            count: u(8),
            block_magic: u(12),
        })
    }

    pub fn compressed(&self) -> bool {
        self.block_magic == STRINGS_BLOCK_MAGIC
    }
}

/// Decoded view of one CaseView document.
pub struct Cvw {
    cfb: Cfb,
    pub streams: Vec<StreamInfo>,
    pub layout_header: LayoutHeader,
    pub cell_dir: CellDirectory,
    pub cell_blocks: Vec<CellBlock>,
    pub cvdata: CvData,
    pub strings: Option<StringsHeader>,
}

impl Cvw {
    pub fn parse(bytes: &[u8]) -> Result<Cvw> {
        // CaseView writes the final CFB sector short (e.g. 49091 bytes = 95
        // full sectors + 131 bytes). `Cfb` reads a short tail as the bytes the
        // file actually carries, so no padding is needed here: the old
        // `pad_to_sector` copy produced byte-identical streams on all 34
        // corpus documents (verified) and has been removed.
        let cfb = Cfb::parse(bytes)?;
        let streams = cfb
            .entries()?
            .into_iter()
            .map(|e| StreamInfo {
                name: e.name(),
                len: e.len,
            })
            .collect();
        let layout = cfb.stream("Form/Layout").unwrap_or_default();
        let cell_bytes = cfb.stream("Index/Cell").unwrap_or_default();
        let cvdata = cfb
            .stream("Index/CvData")
            .map(|b| CvData::parse(&b))
            .unwrap_or_default();
        let strings = cfb.stream("Form/Strings").ok().and_then(|b| StringsHeader::parse(&b));

        let layout_header = LayoutHeader::parse(&layout);
        let cell_dir = CellDirectory::parse(&cell_bytes, &layout);
        let cell_blocks = layout::blocks(&layout, &cell_dir.cells, crate::cell::NAME_WINDOW);

        Ok(Cvw {
            cfb,
            streams,
            layout_header,
            cell_dir,
            cell_blocks,
            cvdata,
            strings,
        })
    }

    /// Raw stream bytes by name (flat root paths).
    pub fn stream(&self, name: &str) -> Result<Vec<u8>> {
        self.cfb.stream(name)
    }

    /// Decode the `Form/Strings` text pool (F6).
    ///
    /// Reports [`strings::StringsError::NotPresent`] for documents that carry
    /// no pool, and [`strings::StringsError::Unreadable`] /
    /// [`strings::StringsError::TruncatedPayload`] for copies whose stream was
    /// cut by the F63 damaged-member frontier — never a fabricated pool.
    pub fn strings_pool(&self) -> std::result::Result<strings::StringsPool, strings::StringsError> {
        let stream = self
            .cfb
            .stream("Form/Strings")
            .map_err(|_| strings::StringsError::NotPresent)?;
        strings::StringsPool::parse(&stream)
    }

    pub fn cells(&self) -> &[CellEntry] {
        &self.cell_dir.cells
    }

    pub fn form(&self) -> Option<&str> {
        self.cvdata.form.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_header_shape() {
        let mut s = vec![0u8; 20];
        s[0..4].copy_from_slice(&2u32.to_le_bytes());
        s[4..8].copy_from_slice(&44757u32.to_le_bytes());
        s[8..12].copy_from_slice(&645u32.to_le_bytes());
        s[12..16].copy_from_slice(&STRINGS_BLOCK_MAGIC.to_le_bytes());
        let h = StringsHeader::parse(&s).unwrap();
        assert_eq!(h.version, 2);
        assert_eq!(h.count, 645);
        assert!(h.compressed());
        assert!(StringsHeader::parse(&s[..8]).is_none());
    }
}

