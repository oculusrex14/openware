//! Structural statement/leadsheet view (F5).
//!
//! [`crate::render`] resolves *what* each cell says. This module answers *where*
//! it sits. A CaseView document is a set of **blocks** (group-scoped tables);
//! inside a block every cell has a leadsheet coordinate (`column`, `row`). The
//! coordinate space is sparse: a table may use columns `A..G, I` and skip rows,
//! and CaseWare still lays the block out as a rectangle with real gaps. That
//! rectangle is the structural contract this module keeps.
//!
//! ```text
//! SheetDoc
//!   blocks: Vec<SheetBlock>        first-appearance (directory) order
//!     group: Option<String>        `aEQ0` for `aEQ0.C1`, None for bare `C1`
//!     first/last row + column      bounds of the rectangle
//!     columns: [1, 2, 3, 4, 9]      sorted unique, gaps are explicit
//!     rows:    [1, 2, ..., 9]       sorted unique
//!     cells:   coordinate -> cell  first cell wins on a same-group repeat
//!     duplicates: same-group coordinate collisions from `CellIndex`
//! ```
//!
//! CaseWare's standard **leadsheet** uses a nine-row header band and data from
//! row 10 down (the corpus carries blocks of nine header rows in many groups).
//! A document with any cell at or below [`LEADSHEET_BODY_ROW`] is therefore
//! classified [`SheetKind::Leadsheet`]; a document whose coordinates all live
//! in the header band is a [`SheetKind::Statement`]. The classification is a
//! shape law, not a text or filename guess, so it is stable across the mock
//! corpus.
//!
//! Cell text comes from the resolved [`RenderedDoc`](crate::render::RenderedDoc),
//! so feed substitution, cycles and missing targets keep their F4 meaning.
//! Unnumbered cells (system/header names such as `@@FIRM`) are counted but do
//! not enter the coordinate grid.

use std::collections::BTreeMap;

use crate::render::{cell_group_of, CellKind, CellNumber, RenderedCell, RenderedDoc};

/// First body row of a standard CaseView leadsheet (rows 1-9 are the header).
pub const LEADSHEET_BODY_ROW: u32 = 10;

/// Document shape inferred from cell coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SheetKind {
    /// All numbered cells sit in the nine-row header band.
    Statement,
    /// At least one numbered cell reaches the leadsheet body (`row >= 10`).
    Leadsheet,
}

impl SheetKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SheetKind::Statement => "statement",
            SheetKind::Leadsheet => "leadsheet",
        }
    }
}

/// One group-scoped table positioned on the leadsheet grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetBlock {
    /// Dotted prefix of the cell names (`aEQ0`), `None` for bare names.
    pub group: Option<String>,
    pub first_row: u32,
    pub last_row: u32,
    pub first_column: u32,
    pub last_column: u32,
    /// Sorted unique 1-based columns actually used (gaps preserved).
    pub columns: Vec<u32>,
    /// Sorted unique 1-based rows actually used (gaps preserved).
    pub rows: Vec<u32>,
    /// First cell (directory order) at each coordinate.
    pub cells: BTreeMap<CellNumber, RenderedCell>,
    /// Same-group coordinate repeats; first cell still wins in `cells`.
    pub duplicates: Vec<(CellNumber, Vec<String>)>,
}

impl SheetBlock {
    pub fn cell(&self, number: CellNumber) -> Option<&RenderedCell> {
        self.cells.get(&number)
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// `true` when the block reaches the leadsheet body band.
    pub fn has_body(&self) -> bool {
        self.last_row >= LEADSHEET_BODY_ROW
    }

    fn new(group: Option<String>) -> SheetBlock {
        SheetBlock {
            group,
            first_row: 0,
            last_row: 0,
            first_column: 0,
            last_column: 0,
            columns: Vec::new(),
            rows: Vec::new(),
            cells: BTreeMap::new(),
            duplicates: Vec::new(),
        }
    }

    fn insert(&mut self, cell: &RenderedCell) {
        let Some(number) = cell.number else { return };
        if self.cells.is_empty() {
            self.first_row = number.row;
            self.last_row = number.row;
            self.first_column = number.column;
            self.last_column = number.column;
        } else {
            self.first_row = self.first_row.min(number.row);
            self.last_row = self.last_row.max(number.row);
            self.first_column = self.first_column.min(number.column);
            self.last_column = self.last_column.max(number.column);
        }
        self.cells.entry(number).or_insert_with(|| cell.clone());
    }

    fn finish(&mut self) {
        self.columns = self.cells.keys().map(|n| n.column).collect();
        self.columns.sort_unstable();
        self.columns.dedup();
        self.rows = self.cells.keys().map(|n| n.row).collect();
        self.rows.sort_unstable();
        self.rows.dedup();
    }

    /// Structural ASCII grid: one line per used row, one column per used
    /// coordinate column. A missing coordinate prints as an empty field; an
    /// issue appends `!<issue>` to the cell text.
    pub fn grid_text(&self) -> String {
        if self.cells.is_empty() {
            return String::new();
        }
        let label_width = self.last_row.to_string().len().max(1);
        let mut widths: Vec<usize> = self
            .columns
            .iter()
            .map(|c| {
                self.cells
                    .iter()
                    .filter(|(n, _)| n.column == *c)
                    .map(|(_, cell)| display_cell(cell).chars().count())
                    .max()
                    .unwrap_or(0)
                    .max(column_name(*c).len())
            })
            .collect();
        for w in &mut widths {
            *w = (*w).max(1);
        }

        let mut out = String::new();
        out.push_str(&" ".repeat(label_width + 2));
        for (i, c) in self.columns.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            out.push_str(&pad(&column_name(*c), widths[i]));
        }
        out.push('\n');
        for row in &self.rows {
            out.push_str(&pad(&row.to_string(), label_width));
            out.push_str("  ");
            for (i, c) in self.columns.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                let text = self
                    .cells
                    .get(&n_c(*c, *row))
                    .map(display_cell)
                    .unwrap_or_default();
                out.push_str(&pad(&text, widths[i]));
            }
            out.truncate(out.trim_end_matches(' ').len());
            out.push('\n');
        }
        out
    }
}

/// Structural view over a resolved document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetDoc {
    pub form: Option<String>,
    pub kind: SheetKind,
    /// Blocks in first-appearance (cell directory) order.
    pub blocks: Vec<SheetBlock>,
    /// Numbered cells placed in a block.
    pub numbered: usize,
    /// Cells without a leadsheet coordinate (system/header names).
    pub unnumbered: usize,
    /// Maximum row and column over all blocks (0 when no numbered cells).
    pub max_row: u32,
    pub max_column: u32,
}

impl SheetDoc {
    pub fn block(&self, group: Option<&str>) -> Option<&SheetBlock> {
        self.blocks.iter().find(|b| b.group.as_deref() == group)
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// One-line shape digest used by the CLI and gates.
    pub fn summary(&self) -> String {
        format!(
            "form {} kind {} blocks {} cells {} numbered {} unnumbered {} max col {} row {}",
            self.form.as_deref().unwrap_or("-"),
            self.kind.as_str(),
            self.blocks.len(),
            self.numbered + self.unnumbered,
            self.numbered,
            self.unnumbered,
            column_name(self.max_column),
            self.max_row
        )
    }

    /// Deterministic block-by-block structural dump.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&self.summary());
        out.push('\n');
        for (i, block) in self.blocks.iter().enumerate() {
            out.push_str(&format!(
                "block {} group {} rows {}..{} cols {}..{} cells {} {}\n",
                i,
                escape_name(block.group.as_deref().unwrap_or("-")),
                block.first_row,
                block.last_row,
                column_name(block.first_column),
                column_name(block.last_column),
                block.len(),
                if block.has_body() { "body" } else { "header" }
            ));
            for (number, names) in &block.duplicates {
                out.push_str(&format!(
                    "  !duplicate {}: {}\n",
                    number,
                    names
                        .iter()
                        .map(|s| escape_name(s))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            out.push_str(&block.grid_text());
        }
        out
    }
}

/// Build the structural view from already-resolved cells.
pub fn sheet(doc: &RenderedDoc) -> SheetDoc {
    let mut blocks: Vec<SheetBlock> = Vec::new();
    let mut by_group: BTreeMap<Option<String>, usize> = BTreeMap::new();
    let mut numbered = 0usize;
    let mut unnumbered = 0usize;

    for cell in &doc.cells {
        if cell.number.is_none() {
            unnumbered += 1;
            continue;
        }
        numbered += 1;
        let group = cell_group_of(&cell.name).map(str::to_string);
        let slot = *by_group.entry(group.clone()).or_insert_with(|| {
            blocks.push(SheetBlock::new(group.clone()));
            blocks.len() - 1
        });
        blocks[slot].insert(cell);
    }

    for block in &mut blocks {
        block.finish();
        let group = block.group.clone().unwrap_or_default();
        for (number, names) in doc.index.duplicates() {
            let in_group: Vec<String> = names
                .into_iter()
                .filter(|name| cell_group_of(name).unwrap_or("") == group)
                .collect();
            if in_group.len() > 1 {
                block.duplicates.push((number, in_group));
            }
        }
    }

    let kind = if blocks.iter().any(SheetBlock::has_body) {
        SheetKind::Leadsheet
    } else {
        SheetKind::Statement
    };
    let max_row = blocks.iter().map(|b| b.last_row).max().unwrap_or(0);
    let max_column = blocks.iter().map(|b| b.last_column).max().unwrap_or(0);

    SheetDoc {
        form: doc.form.clone(),
        kind,
        blocks,
        numbered,
        unnumbered,
        max_row,
        max_column,
    }
}

/// Build the structural view directly from a cell directory (fixtures).
pub fn sheet_cells(
    cells: &[crate::cell::CellEntry],
    form: Option<&str>,
    texts: &crate::render::TextTable,
    links: &crate::render::LinkTable,
    registry: Option<&crate::wks::WksRegistry>,
) -> SheetDoc {
    sheet(&crate::render::render_cells(cells, form, texts, links, registry))
}

fn n_c(column: u32, row: u32) -> CellNumber {
    CellNumber { column, row }
}

fn column_name(n: u32) -> String {
    n_c(n, 1).column_name()
}

fn display_cell(cell: &RenderedCell) -> String {
    let mut out = escape_text(&cell.text);
    if let Some(issue) = &cell.issue {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&format!("!{issue}"));
    }
    match cell.kind {
        CellKind::Literal => {}
        CellKind::CellLink => out.push_str(" [cell]"),
        CellKind::LeadsheetLink => out.push_str(" [feed]"),
    }
    out
}

fn pad(text: &str, width: usize) -> String {
    let mut out = text.to_string();
    let len = text.chars().count();
    if len < width {
        out.push_str(&" ".repeat(width - len));
    }
    out
}

fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() || !c.is_ascii() => {
                out.push_str(&format!("\\x{:02x}", c as u32 & 0xFF))
            }
            c => out.push(c),
        }
    }
    out
}

fn escape_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_control() || !c.is_ascii() {
            out.push_str(&format!("\\x{:02x}", c as u32 & 0xFF));
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::CellEntry;
    use crate::render::{LinkTable, RenderIssue, TextTable};

    fn entry(name: &str) -> CellEntry {
        CellEntry {
            name: name.to_string(),
            layout_offset: 0,
        }
    }

    #[test]
    fn blocks_keep_sparse_coordinate_rectangle() {
        let cells = vec![
            entry("aEQ0.C1"),
            entry("aEQ0.C2"),
            entry("aEQ0.A1"),
            entry("aEQ0.I9"),
            entry("aEQ1.A1"),
        ];
        let mut texts = TextTable::new();
        texts.insert("aEQ0.C1", "header");
        texts.insert("aEQ0.I9", "total");
        let out = sheet_cells(&cells, Some("01200RG"), &texts, &LinkTable::new(), None);
        assert_eq!(out.kind, SheetKind::Statement);
        assert_eq!(out.blocks.len(), 2);
        let eq0 = out.block(Some("aEQ0")).unwrap();
        assert_eq!((eq0.first_row, eq0.last_row), (1, 9));
        assert_eq!((eq0.first_column, eq0.last_column), (1, 9));
        assert_eq!(eq0.columns, vec![1, 3, 9]);
        assert_eq!(eq0.rows, vec![1, 2, 9]);
        assert_eq!(eq0.cell(n_c(3, 1)).unwrap().text, "header");
        assert!(eq0.cell(n_c(2, 1)).is_none());
        assert_eq!(eq0.len(), 4);
        let text = out.to_text();
        assert!(text.starts_with("form 01200RG kind statement blocks 2"));
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[2].starts_with("   A"));
        assert!(lines[2].contains('C'));
        assert!(lines[2].trim_end().ends_with('I'));
        assert!(lines[3].contains("header"));
        assert!(lines[5].contains("total"));
    }

    #[test]
    fn body_row_marks_a_leadsheet_and_headers_stay_out() {
        let cells = vec![
            entry("rTB1.A10"),
            entry("rTB1.B10"),
            entry("rTB1.A11"),
            entry("rTB1.B11"),
            entry("@@FIRM"),
        ];
        let mut texts = TextTable::new();
        texts.insert("rTB1.A10", "cash");
        texts.insert("rTB1.B10", "100");
        let out = sheet_cells(&cells, None, &texts, &LinkTable::new(), None);
        assert_eq!(out.kind, SheetKind::Leadsheet);
        assert_eq!(out.numbered, 4);
        assert_eq!(out.unnumbered, 1);
        assert_eq!(out.max_column, 2);
        assert_eq!(out.max_row, 11);
        let block = out.block(Some("rTB1")).unwrap();
        assert!(block.has_body());
        assert_eq!(block.grid_text().lines().count(), 3); // header + two rows
        assert!(out.summary().contains("kind leadsheet"));
        assert!(out.summary().contains("numbered 4 unnumbered 1 max col B row 11"));
    }

    #[test]
    fn same_group_duplicate_is_reported_and_first_wins() {
        let cells = vec![entry("aEQ0.C1"), entry("aEQ0.C01")];
        let out = sheet_cells(&cells, None, &TextTable::new(), &LinkTable::new(), None);
        let block = out.block(Some("aEQ0")).unwrap();
        assert_eq!(block.duplicates.len(), 1);
        assert_eq!(block.duplicates[0].1, vec!["aEQ0.C1", "aEQ0.C01"]);
        assert_eq!(block.cell(n_c(3, 1)).unwrap().name, "aEQ0.C1");
    }

    #[test]
    fn issue_marker_is_structural_too() {
        let cells = vec![entry("C1")];
        let mut links = LinkTable::new();
        links.insert("C1", crate::render::CellRef::parse("Z9").unwrap());
        let out = sheet_cells(&cells, None, &TextTable::new(), &links, None);
        let block = out.blocks.first().unwrap();
        let rendered = block.cell(n_c(3, 1)).unwrap();
        assert_eq!(
            rendered.issue,
            Some(RenderIssue::MissingCell {
                target: CellNumber::parse("Z9").unwrap(),
                group: None
            })
        );
        assert!(block.grid_text().contains("!missing cell Z9"));
    }
}

