//! F5 gate: structural statement/leadsheet view over real corpus documents.
//!
//! Skips when `a sample copy` is absent (fresh checkout). Read-only over
//! the user-approved mock engagement copy; nothing here touches CaseWare.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use cw_cv::{
    cell_group_of, cell_number_of, render, sheet, sheet_cells, CellEntry, LinkTable, SheetKind,
    TextTable, Cvw,
};

fn corpus() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sandbox/bridge-work");
    let mut out = Vec::new();
    let Ok(dirs) = std::fs::read_dir(&root) else {
        return out;
    };
    for d in dirs.flatten() {
        let Ok(files) = std::fs::read_dir(d.path()) else {
            continue;
        };
        for f in files.flatten() {
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) == Some("cvw") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn docs() -> Vec<Cvw> {
    corpus()
        .into_iter()
        .filter_map(|p| std::fs::read(p).ok())
        .filter_map(|b| Cvw::parse(&b).ok())
        .collect()
}

fn entry(name: &str) -> CellEntry {
    CellEntry {
        name: name.to_string(),
        layout_offset: 0,
    }
}

#[test]
fn sparse_fixture_keeps_its_rectangle_and_gaps() {
    let cells = vec![
        entry("aEQ0.A1"),
        entry("aEQ0.C1"),
        entry("aEQ0.I9"),
        entry("aEQ1.A1"),
        entry("@@FIRM"),
    ];
    let mut texts = TextTable::new();
    texts.insert("aEQ0.A1", "label");
    texts.insert("aEQ0.C1", "value");
    let out = sheet_cells(&cells, Some("01200RG"), &texts, &LinkTable::new(), None);
    assert_eq!(out.kind, SheetKind::Statement);
    assert_eq!(out.numbered, 4);
    assert_eq!(out.unnumbered, 1);
    assert_eq!((out.max_column, out.max_row), (9, 9));
    let eq0 = out.block(Some("aEQ0")).unwrap();
    assert_eq!(eq0.columns, vec![1, 3, 9]);
    assert_eq!(eq0.rows, vec![1, 9]);
    assert!(eq0.cell(cw_cv::CellNumber { column: 2, row: 1 }).is_none());
    assert_eq!(eq0.grid_text(), out.block(Some("aEQ0")).unwrap().grid_text());
    assert_eq!(out.to_text(), out.to_text());
}

#[test]
fn real_statement_and_leadsheet_shapes_are_classified() {
    let docs = docs();
    if docs.is_empty() {
        eprintln!("corpus not present; skipping");
        return;
    }
    let mut statements = 0;
    let mut leadsheets = 0;
    for doc in docs.iter().take(24) {
        let rendered = render(doc, &TextTable::new(), &LinkTable::new(), None);
        let model = sheet(&rendered);
        if model.numbered == 0 {
            continue;
        }
        match model.kind {
            SheetKind::Statement => {
                statements += 1;
                assert!(model.max_row < cw_cv::LEADSHEET_BODY_ROW);
            }
            SheetKind::Leadsheet => {
                leadsheets += 1;
                assert!(model.max_row >= cw_cv::LEADSHEET_BODY_ROW);
            }
        }
        // Every numbered cell lands in exactly one group block and one position.
        let mut placed = 0usize;
        for cell in rendered.cells.iter() {
            let Some(number) = cell.number else { continue };
            placed += 1;
            let group = cell_group_of(&cell.name);
            let block = model
                .block(group)
                .unwrap_or_else(|| panic!("no block for {:?}", cell.name));
            assert_eq!(block.cell(number).unwrap().name, cell.name);
            assert!(number.column >= block.first_column && number.column <= block.last_column);
            assert!(number.row >= block.first_row && number.row <= block.last_row);
            assert!(block.columns.binary_search(&number.column).is_ok());
            assert!(block.rows.binary_search(&number.row).is_ok());
        }
        assert_eq!(placed, model.numbered);
        assert!(model.summary().contains(model.kind.as_str()));
    }
    assert!(statements > 0, "expected at least one statement-shaped document");
    assert!(leadsheets > 0, "expected at least one leadsheet-shaped document");
}

#[test]
fn real_leadsheet_body_is_a_rectangular_header_plus_body() {
    let docs = docs();
    if docs.is_empty() {
        eprintln!("corpus not present; skipping");
        return;
    }
    let mut found = None;
    for doc in &docs {
        let rendered = render(doc, &TextTable::new(), &LinkTable::new(), None);
        let model = sheet(&rendered);
        if model.kind == SheetKind::Leadsheet {
            found = Some((rendered, model));
            break;
        }
    }
    let Some((rendered, model)) = found else {
        eprintln!("no leadsheet-shaped document; skipping");
        return;
    };
    assert_eq!(model.numbered, rendered.cells.iter().filter(|c| c.number.is_some()).count());
    let block = model
        .blocks
        .iter()
        .find(|b| b.has_body())
        .expect("leadsheet has a body block");
    // Header band must be present in the same document (rows 1..9 observed).
    assert!(
        model.blocks.iter().any(|b| b.first_row <= 9),
        "leadsheet has header-band blocks"
    );
    // Body coordinates are dense in the rectangle sense: every used row and
    // column is explicitly listed even when the intersection is empty.
    for row in &block.rows {
        assert!(*row >= block.first_row && *row <= block.last_row);
    }
    for column in &block.columns {
        assert!(*column >= block.first_column && *column <= block.last_column);
    }
    assert_eq!(block.rows.len() as usize, block.rows.iter().collect::<BTreeSet<_>>().len());
    assert_eq!(
        block.columns.len() as usize,
        block.columns.iter().collect::<BTreeSet<_>>().len()
    );
    assert!(!block.grid_text().is_empty());
    assert_eq!(model.to_text(), model.to_text());
}

#[test]
fn real_cell_names_round_trip_into_blocks() {
    let docs = docs();
    if docs.is_empty() {
        eprintln!("corpus not present; skipping");
        return;
    }
    for doc in docs.iter().take(8) {
        let rendered = render(doc, &TextTable::new(), &LinkTable::new(), None);
        let model = sheet(&rendered);
        for cell in &rendered.cells {
            if let Some(number) = cell_number_of(&cell.name) {
                assert_eq!(Some(number), cell.number);
            }
        }
        let groups: BTreeSet<Option<&str>> = rendered
            .cells
            .iter()
            .filter(|c| c.number.is_some())
            .map(|c| cell_group_of(&c.name))
            .collect();
        assert_eq!(groups.len(), model.blocks.len());
    }
}

