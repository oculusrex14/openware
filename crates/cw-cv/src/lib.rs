//! cw-cv ??CaseView document model and render pipeline.
//!
//! `.cvw`/`.cvv` binary formats (F20, checklist F1): the shipped files are CFB
//! containers carrying `Layout` (packed cell blocks), `Cell` (cell directory),
//! `CvData` (FORM identity) and a compressed `Strings` pool. Decoding lives in
//! [`cvw`], [`cell`], [`layout`], [`cvdata`]; `cw_core::cfb` supplies the
//! container reader (F20/F24 substrate).
//!
//! F3 (`props`) fixes the property-bag + override (`CV OVR`) semantics: a sparse
//! per-cell override set over a style/base bag with inherit / set / reset states,
//! nearest-wins layering and origin provenance. Cell attribute bytes are kept
//! verbatim by [`props::CellAttrs`], so write-back is byte-identical before the
//! bit packing is decoded.
//!
//! Still open: bit-level attribute packing, authored `Para`/`Sect`/`Table`
//! geometry, and glyph-level layout.
//!
//! F6 ([`strings`]) closes the `Form/Strings` text pool: the stream is a
//! DynaZip-framed raw LZMA1 payload (a 46-byte ZIP central-directory-shaped
//! frame, the 19-byte DynaZip prefix, then LZMA1 lc3/lp0/pb2 at offset 77) whose
//! decoded bytes form `u32 len / u8 kind / bytes / NUL` records closed by
//! `0xFFFFFFFF`. [`strings::StringsPool::parse`] validates the frame, checks the
//! header's CRC law, decodes to the declared length and walks the table exactly;
//! [`Cvw::strings_pool`] is the per-document entry point and reports typed
//! failures instead of a fabricated pool. Law:
//! `attachments/caseview-strings.md`; gate: `tests/f6_strings_gate.rs`. Reach is
//! limited to the three corpus documents whose stream survives the F63 damaged-
//! member frontier (hull2024 `EXPO0IS0`/`FJEIMP`, viewrail2025 `EXPO0IS0`); every
//! other document either has no pool or places it past the readable prefix.
//!
//! F4 ([`render`]) adds the leadsheet layer: cell numbers (`C1`, `aEQ0.C1`),
//! cell/feed links and a deterministic render pipeline. Literal text is supplied
//! by [`render::TextTable`]; wiring it to a decoded [`strings::StringsPool`] is
//! the remaining `Layout`-side index work, not a codec gap. Link resolution,
//! cycle detection, duplicate-number reporting and the plain-text dump are
//! independent of that residual.
//!
//! F5 ([`sheet`]) adds the structural view: one block per group, explicit
//! row/column bounds with gaps preserved, statement-vs-leadsheet classification
//! from the nine-row header band, and a deterministic grid dump.
//!
//! The deep-risk lane ??formats are undocumented; static analysis on
//! `cv32.dll`/`x64cv32.dll` is the fallback (Plan.md --3).

pub mod cell;
pub mod cvdata;
pub mod cvw;
pub mod expr;
pub mod index_stream;
pub mod layout;
pub mod props;
pub mod render;
pub mod sheet;
pub mod strings;
pub mod wks;

pub use cell::{CellDirectory, CellEntry};
pub use cvdata::CvData;
pub use cvw::{Cvw, StreamInfo, StringsHeader};
pub use expr::{canonical_period_key, parse as parse_expr, Ast, BinOp, EvalError, PeriodKey, Value};
pub use index_stream::{IndexHeader, IndexRecord, ParaIndex, RangeIndex};
pub use layout::{BlockTrailer, CellBlock, LayoutHeader};
pub use strings::{PoolEntry, PoolHeader, StringsError, StringsPool};
pub use props::{CellAttrs, Origin, Override, OverrideSet, PropBag, PropValue, Resolved};
pub use render::{
    cell_group_of, cell_number_of, render, render_cells, CellIndex, CellKind, CellNumber, CellRef,
    LinkTable, RenderIssue, RenderedCell, RenderedDoc, TextTable,
};
pub use sheet::{
    sheet, sheet_cells, SheetBlock, SheetDoc, SheetKind, LEADSHEET_BODY_ROW,
};
pub use wks::{
    LinkedWorksheets, RegistryDelta, TemplateRef, TemplateSet, WksCell, WksRegistry, Worksheet,
};

/// Kernel health probe (kept for the crate smoke contract).
pub fn healthy() -> bool {
    cw_core::healthy()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cv_boots_on_kernel() {
        assert!(super::healthy());
    }

    #[test]
    fn healthy_is_idempotent() {
        assert_eq!(super::healthy(), super::healthy());
    }

    /// End-to-end decode of a real shipped `.cvw` when the corpus is present.
    /// Skips silently in environments without `a sample copy`.
    #[test]
    fn decodes_real_corpus_document() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sandbox/bridge-work");
        let mut found = None;
        if let Ok(dirs) = std::fs::read_dir(&root) {
            'outer: for d in dirs.flatten() {
                if let Ok(files) = std::fs::read_dir(d.path()) {
                    for f in files.flatten() {
                        let p = f.path();
                        if p.extension().and_then(|e| e.to_str()) == Some("cvw") {
                            if let Ok(bytes) = std::fs::read(&p) {
                                if let Ok(doc) = Cvw::parse(&bytes) {
                                    if doc.cells().len() > 10 {
                                        found = Some(doc);
                                        break 'outer;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        let Some(doc) = found else {
            eprintln!("corpus not present; skipping real-document decode");
            return;
        };
        assert!(doc.layout_header.magic_ok);
        assert_eq!(doc.cell_dir.header[1], cell::MARKER);
        assert!(doc.cell_dir.complete, "cell walk must consume the stream");
        assert!(doc.cells().len() > 10);
        // Every directory entry must resolve to a layout block with its name.
        let named = doc.cell_blocks.iter().filter(|b| b.name_offset.is_some()).count();
        assert_eq!(named, doc.cell_blocks.len(), "every cell name must be locatable");
        assert!(doc.stream("Form/Layout").is_ok());
    }
}

// relink touch: cw-cv F1 decode (2026-10-23)

