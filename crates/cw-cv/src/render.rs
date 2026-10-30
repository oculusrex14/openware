//! Cell-number links + render pipeline (F4).
//!
//! CaseView lays a document out as a **leadsheet**: every cell block has a
//! number made of a column letter and a row (`C1`, `B7`, `AA12`). The number is
//! carried in the cell name, either bare (`C1`) or group-qualified
//! (`aEQ0.C1`, observed in the shipped corpus next to the 0x1C system prefix).
//! A cell *link* is a cell whose value is not its own literal text but the value
//! of another cell or of a linked-worksheet feed:
//!
//! ```text
//! link target := <column><row>            same document, leadsheet substitution
//!              | <WKS>.<CELLNO>           FP store feed  (F2)
//! ```
//!
//! The two forms are unambiguous: the token after the last `.` is a cell
//! number (`aEQ0.C1` -> cell `C1`) or an integer (`CLP.2` -> worksheet `CLP`
//! cell `2`), never both.
//!
//! What this module fixes is the **semantic layer**, not the still-undecoded
//! `Strings` codec: literal text is supplied by [`TextTable`] (the caller may
//! populate it from the string pool once F1 residual is closed) and links by
//! [`LinkTable`]. [`render_cells`] then resolves every cell depth-first with
//! cycle detection, reports missing targets/feeds, and produces a deterministic
//! [`RenderedDoc`] that can be dumped as plain text.
//!
//! Corpus law used by the tests: a cell number repeats legitimately across
//! groups (`aEQ0.C1`, `aER3.C1`, ...) because each group is its own table, so
//! the index is **group-scoped**: a bare `C1` resolves in the source cell's own
//! group first, an explicit `aER3.C1` targets that group, and only a repeat
//! inside one group is reported as a collision. Directory (Layout-offset) order
//! breaks any remaining tie.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use cw_core::error::{CoreError, Result};

use crate::cell::CellEntry;
use crate::cvw::Cvw;
use crate::wks::WksRegistry;

/// A leadsheet cell number: `column` is 1-based (`A` = 1) and `row` is 1-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CellNumber {
    pub column: u32,
    pub row: u32,
}

impl CellNumber {
    /// Parse `A1`, `c12`, `AA10`. Column letters are case-insensitive; the row
    /// must be a positive integer and at least one letter is required.
    pub fn parse(text: &str) -> Option<CellNumber> {
        let s = text.trim();
        let split = s.find(|c: char| c.is_ascii_digit())?;
        let (letters, digits) = s.split_at(split);
        if letters.is_empty() || digits.is_empty() {
            return None;
        }
        if !letters.chars().all(|c| c.is_ascii_alphabetic()) {
            return None;
        }
        if !digits.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let mut column = 0u32;
        for c in letters.chars() {
            column = column
                .checked_mul(26)?
                .checked_add(c.to_ascii_uppercase() as u32 - 'A' as u32 + 1)?;
        }
        let row: u32 = digits.parse().ok()?;
        if row == 0 {
            return None;
        }
        Some(CellNumber { column, row })
    }

    /// Column letters (`A`, `Z`, `AA`, ...).
    pub fn column_name(&self) -> String {
        let mut n = self.column.max(1);
        let mut out = String::new();
        while n > 0 {
            let rem = ((n - 1) % 26) as u8;
            out.insert(0, (b'A' + rem) as char);
            n = (n - 1) / 26;
        }
        out
    }
}

impl fmt::Display for CellNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.column_name(), self.row)
    }
}

/// Where a cell's value comes from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CellRef {
    /// Same document, identified by leadsheet number. A group qualifier
    /// (`aEQ0.C1`) scopes the number to that group/table; a bare `C1` resolves
    /// in the source cell's own group first, then globally.
    Cell {
        number: CellNumber,
        group: Option<String>,
    },
    /// Linked worksheet feed (`FP` store, F2).
    Leadsheet { wks: String, cellno: u32 },
}

impl CellRef {
    /// Parse a link target: `C1` / `aEQ0.C1` (cell) or `CLP.2` (feed).
    pub fn parse(text: &str) -> Option<CellRef> {
        let s = text.trim();
        if s.is_empty() {
            return None;
        }
        if let Some((lhs, rhs)) = s.rsplit_once('.') {
            if let Some(number) = CellNumber::parse(rhs) {
                // `aEQ0.C1` — the group prefix scopes the number.
                let group = lhs.trim_start_matches(|c: char| (c as u32) < 0x20);
                return Some(CellRef::Cell {
                    number,
                    group: if group.is_empty() {
                        None
                    } else {
                        Some(group.to_string())
                    },
                });
            }
            if !lhs.is_empty() && lhs.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                if let Ok(cellno) = rhs.parse::<u32>() {
                    return Some(CellRef::Leadsheet {
                        wks: lhs.to_string(),
                        cellno,
                    });
                }
            }
            return None;
        }
        CellNumber::parse(s).map(|number| CellRef::Cell { number, group: None })
    }

    pub fn is_cell(&self) -> bool {
        matches!(self, CellRef::Cell { .. })
    }

    pub fn is_leadsheet(&self) -> bool {
        matches!(self, CellRef::Leadsheet { .. })
    }

    pub fn number(&self) -> Option<CellNumber> {
        match self {
            CellRef::Cell { number, .. } => Some(*number),
            CellRef::Leadsheet { .. } => None,
        }
    }
}

impl fmt::Display for CellRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CellRef::Cell { number, group } => match group {
                Some(g) => write!(f, "{g}.{number}"),
                None => write!(f, "{number}"),
            },
            CellRef::Leadsheet { wks, cellno } => write!(f, "{wks}.{cellno}"),
        }
    }
}

/// Cell name -> link target. Keys are exact cell names from [`crate::cell`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkTable {
    links: BTreeMap<String, CellRef>,
}

impl LinkTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, source: impl Into<String>, target: CellRef) -> Option<CellRef> {
        self.links.insert(source.into(), target)
    }

    pub fn get(&self, source: &str) -> Option<&CellRef> {
        self.links.get(source)
    }

    /// Lookup that tolerates the corpus's control-byte name prefixes: an exact
    /// hit wins, else the name with leading bytes `< 0x20` stripped is tried
    /// (`\x1caEQ0.C2` matches the file key `aEQ0.C2`).
    pub fn get_for(&self, source: &str) -> Option<&CellRef> {
        if let Some(t) = self.links.get(source) {
            return Some(t);
        }
        let canon = source.trim_start_matches(|c: char| (c as u32) < 0x20);
        if canon != source {
            self.links.get(canon)
        } else {
            None
        }
    }

    pub fn remove(&mut self, source: &str) -> Option<CellRef> {
        self.links.remove(source)
    }

    pub fn len(&self) -> usize {
        self.links.len()
    }

    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &CellRef)> {
        self.links.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Parse `source=target` lines; blank lines and `#` comments are ignored.
    pub fn parse(text: &str) -> Result<LinkTable> {
        let mut out = LinkTable::new();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (src, dst) = line.split_once('=').ok_or_else(|| {
                CoreError::Format(format!("link line {}: expected `source=target`", i + 1))
            })?;
            let src = src.trim();
            if src.is_empty() {
                return Err(CoreError::Format(format!("link line {}: blank source", i + 1)));
            }
            let target = CellRef::parse(dst).ok_or_else(|| {
                CoreError::Format(format!("link line {}: bad target {:?}", i + 1, dst.trim()))
            })?;
            out.insert(src, target);
        }
        Ok(out)
    }
}

/// Cell name -> literal text. Populated by the caller (string pool once F1 is
/// complete, or a fixture); the renderer never invents text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextTable {
    texts: BTreeMap<String, String>,
}

impl TextTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, name: impl Into<String>, text: impl Into<String>) -> Option<String> {
        self.texts.insert(name.into(), text.into())
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.texts.get(name).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.texts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.texts.is_empty()
    }

    /// Parse `name=value` lines; blank lines and `#` comments are ignored.
    /// Blanks directly after `=` are dropped, the rest of the line is kept.
    pub fn parse(text: &str) -> Result<TextTable> {
        let mut out = TextTable::new();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim_end();
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let (name, value) = line.split_once('=').ok_or_else(|| {
                CoreError::Format(format!("text line {}: expected `name=value`", i + 1))
            })?;
            let name = name.trim();
            if name.is_empty() {
                return Err(CoreError::Format(format!("text line {}: blank name", i + 1)));
            }
            out.insert(name, value.trim_start());
        }
        Ok(out)
    }
}

/// Strip the group qualifier and control-byte prefix from a cell name and read
/// its leadsheet number: `aEQ0.C1` -> `C1`, `\x1caEQ0.C1` -> `C1`.
pub fn cell_number_of(name: &str) -> Option<CellNumber> {
    let tail = name.rsplit('.').next().unwrap_or(name);
    CellNumber::parse(tail)
}

/// `aEQ0.C1` -> `aEQ0`; `C1` -> `None`.
pub fn cell_group_of(name: &str) -> Option<&str> {
    let (group, _) = name.rsplit_once('.')?;
    let group = group.trim_start_matches(|c: char| (c as u32) < 0x20);
    if group.is_empty() {
        None
    } else {
        Some(group)
    }
}

/// Leadesheet index: number -> cell names in directory (Layout-offset) order.
///
/// Two views are kept: global (`C1`) and group-scoped (`aEQ0.C1`). Corpus law:
/// the same number legitimately repeats across groups (a document carries many
/// tables/leadsheets), so only a repeat **inside one group** is a collision.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CellIndex {
    by_number: BTreeMap<CellNumber, Vec<String>>,
    by_group: BTreeMap<(String, CellNumber), Vec<String>>,
}

impl CellIndex {
    pub fn build(cells: &[CellEntry]) -> CellIndex {
        let mut by_number: BTreeMap<CellNumber, Vec<String>> = BTreeMap::new();
        let mut by_group: BTreeMap<(String, CellNumber), Vec<String>> = BTreeMap::new();
        for c in cells {
            let Some(n) = cell_number_of(&c.name) else {
                continue;
            };
            by_number.entry(n).or_default().push(c.name.clone());
            let group = cell_group_of(&c.name).unwrap_or("").to_string();
            by_group
                .entry((group, n))
                .or_default()
                .push(c.name.clone());
        }
        CellIndex { by_number, by_group }
    }

    /// First cell (directory order) carrying the number, any group.
    pub fn first(&self, number: CellNumber) -> Option<&str> {
        self.by_number
            .get(&number)
            .and_then(|v| v.first())
            .map(String::as_str)
    }

    /// First cell carrying the number inside `group` (`""` = ungrouped).
    pub fn first_in_group(&self, number: CellNumber, group: &str) -> Option<&str> {
        self.by_group
            .get(&(group.to_string(), number))
            .and_then(|v| v.first())
            .map(String::as_str)
    }

    pub fn names_for(&self, number: CellNumber) -> &[String] {
        self.by_number.get(&number).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn group_names_for(&self, number: CellNumber, group: &str) -> &[String] {
        self.by_group
            .get(&(group.to_string(), number))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn len(&self) -> usize {
        self.by_number.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_number.is_empty()
    }

    /// Numbers claimed more than once **within the same group** (true
    /// ambiguity). Cross-group repeats are normal and not reported.
    pub fn duplicates(&self) -> Vec<(CellNumber, Vec<String>)> {
        self.by_group
            .iter()
            .filter(|(_, v)| v.len() > 1)
            .map(|((_, n), v)| (*n, v.clone()))
            .collect()
    }

    /// Numbers in leadsheet order (column, then row).
    pub fn numbers(&self) -> impl Iterator<Item = CellNumber> + '_ {
        self.by_number.keys().copied()
    }
}

/// Why a cell could not be substituted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderIssue {
    /// Internal target number absent from the document.
    MissingCell {
        target: CellNumber,
        group: Option<String>,
    },
    /// `FP` registry has no value for this feed.
    MissingFeed { wks: String, cellno: u32 },
    /// A leadsheet feed was requested but no registry was supplied.
    NoFeedRegistry { wks: String, cellno: u32 },
    /// Link chain revisits a cell.
    Cycle { at: String },
}

impl fmt::Display for RenderIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderIssue::MissingCell { target, group } => match group {
                Some(g) => write!(f, "missing cell {g}.{target}"),
                None => write!(f, "missing cell {target}"),
            },
            RenderIssue::MissingFeed { wks, cellno } => {
                write!(f, "no FP feed for {wks}.{cellno}")
            }
            RenderIssue::NoFeedRegistry { wks, cellno } => {
                write!(f, "no registry for {wks}.{cellno}")
            }
            RenderIssue::Cycle { at } => write!(f, "cycle at {at}"),
        }
    }
}

/// How a rendered cell got its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellKind {
    /// Own text from [`TextTable`] (possibly empty).
    Literal,
    /// Substituted from another cell number.
    CellLink,
    /// Substituted from a linked worksheet feed.
    LeadsheetLink,
}

impl CellKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            CellKind::Literal => "text",
            CellKind::CellLink => "cell",
            CellKind::LeadsheetLink => "feed",
        }
    }
}

/// One resolved cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedCell {
    pub name: String,
    pub number: Option<CellNumber>,
    pub group: Option<String>,
    pub kind: CellKind,
    pub text: String,
    /// `None` when the value resolved cleanly.
    pub issue: Option<RenderIssue>,
}

/// A resolved document, in directory order, plus the leadsheet index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedDoc {
    pub form: Option<String>,
    pub cells: Vec<RenderedCell>,
    pub index: CellIndex,
}

impl RenderedDoc {
    pub fn cell(&self, name: &str) -> Option<&RenderedCell> {
        self.cells.iter().find(|c| c.name == name)
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Cells whose value came from a link and resolved without an issue.
    pub fn substituted(&self) -> usize {
        self.cells
            .iter()
            .filter(|c| c.kind != CellKind::Literal && c.issue.is_none())
            .count()
    }

    pub fn issues(&self) -> impl Iterator<Item = &RenderedCell> {
        self.cells.iter().filter(|c| c.issue.is_some())
    }

    pub fn issue_count(&self) -> usize {
        self.issues().count()
    }

    /// Cells ordered by leadsheet number (unnumbered names last, by name).
    pub fn in_number_order(&self) -> Vec<&RenderedCell> {
        let mut out: Vec<&RenderedCell> = self.cells.iter().collect();
        out.sort_by(|a, b| match (a.number, b.number) {
            (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.name.cmp(&b.name)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.name.cmp(&b.name),
        });
        out
    }

    /// Deterministic plain-text render. Cell names are escaped (Latin-1 control
    /// bytes become `\xNN`), so the dump is ASCII-safe.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "form {}\ncells {} substituted {} issues {}\n",
            self.form.as_deref().unwrap_or("-"),
            self.cells.len(),
            self.substituted(),
            self.issue_count()
        ));
        for c in &self.cells {
            let number = c.number.map(|n| n.to_string()).unwrap_or_else(|| "-".into());
            out.push_str(&format!(
                "{}\t{}\t{}\t{}",
                escape_name(&c.name),
                number,
                c.kind.as_str(),
                c.text
            ));
            if let Some(issue) = &c.issue {
                out.push_str(&format!("\t!{issue}"));
            }
            out.push('\n');
        }
        for (n, names) in self.index.duplicates() {
            out.push_str(&format!(
                "!duplicate {n}: {}\n",
                names
                    .iter()
                    .map(|s| escape_name(s))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        out
    }
}

/// Render a document from its cell directory.
pub fn render(
    doc: &Cvw,
    texts: &TextTable,
    links: &LinkTable,
    registry: Option<&WksRegistry>,
) -> RenderedDoc {
    render_cells(doc.cells(), doc.form(), texts, links, registry)
}

/// Render an arbitrary cell directory (used directly by fixtures and gates).
///
/// Resolution is depth-first through [`LinkTable`] with cycle detection; the
/// first cell in directory order wins when a number is duplicated (the index
/// reports the collision either way).
pub fn render_cells(
    cells: &[CellEntry],
    form: Option<&str>,
    texts: &TextTable,
    links: &LinkTable,
    registry: Option<&WksRegistry>,
) -> RenderedDoc {
    let index = CellIndex::build(cells);
    let mut out = Vec::with_capacity(cells.len());

    for cell in cells {
        let mut stack = BTreeSet::new();
        let (kind, text, issue) =
            resolve(&cell.name, texts, links, registry, &index, &mut stack);
        out.push(RenderedCell {
            name: cell.name.clone(),
            number: cell_number_of(&cell.name),
            group: cell_group_of(&cell.name).map(str::to_string),
            kind,
            text,
            issue,
        });
    }

    RenderedDoc {
        form: form.map(str::to_string),
        cells: out,
        index,
    }
}

type Resolution = (CellKind, String, Option<RenderIssue>);

fn resolve(
    name: &str,
    texts: &TextTable,
    links: &LinkTable,
    registry: Option<&WksRegistry>,
    index: &CellIndex,
    stack: &mut BTreeSet<String>,
) -> Resolution {
    if !stack.insert(name.to_string()) {
        return (
            CellKind::Literal,
            String::new(),
            Some(RenderIssue::Cycle { at: name.to_string() }),
        );
    }
    let result = match links.get_for(name) {
        None => (
            CellKind::Literal,
            texts.get(name).unwrap_or_default().to_string(),
            None,
        ),
        Some(CellRef::Cell { number, group }) => {
            // Group-scoped first: a bare `C1` means "my table's C1", and an
            // explicit group always wins. Global lookup is the last resort.
            let target = match group {
                Some(g) => index.first_in_group(*number, g),
                None => cell_group_of(name)
                    .and_then(|g| index.first_in_group(*number, g))
                    .or_else(|| index.first(*number)),
            };
            match target {
                None => (
                    CellKind::CellLink,
                    String::new(),
                    Some(RenderIssue::MissingCell {
                        target: *number,
                        group: group.clone(),
                    }),
                ),
                Some(target) => {
                    if target == name {
                        return finish(
                            stack,
                            name,
                            CellKind::CellLink,
                            String::new(),
                            Some(RenderIssue::Cycle { at: name.to_string() }),
                        );
                    }
                    let (_, text, issue) =
                        resolve(target, texts, links, registry, index, stack);
                    (CellKind::CellLink, text, issue)
                }
            }
        }
        Some(CellRef::Leadsheet { wks, cellno }) => match registry {
            None => (
                CellKind::LeadsheetLink,
                String::new(),
                Some(RenderIssue::NoFeedRegistry {
                    wks: wks.clone(),
                    cellno: *cellno,
                }),
            ),
            Some(reg) => match reg.text(wks, *cellno) {
                Some(text) => (CellKind::LeadsheetLink, text, None),
                None => (
                    CellKind::LeadsheetLink,
                    String::new(),
                    Some(RenderIssue::MissingFeed {
                        wks: wks.clone(),
                        cellno: *cellno,
                    }),
                ),
            },
        },
    };
    finish(stack, name, result.0, result.1, result.2)
}

fn finish(
    stack: &mut BTreeSet<String>,
    name: &str,
    kind: CellKind,
    text: String,
    issue: Option<RenderIssue>,
) -> Resolution {
    stack.remove(name);
    (kind, text, issue)
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

    fn entry(name: &str) -> CellEntry {
        CellEntry {
            name: name.to_string(),
            layout_offset: 0,
        }
    }

    fn cref(s: &str) -> CellRef {
        CellRef::parse(s).unwrap()
    }

    /// Minimal `FP` table: WKS(8) CELLNO(3) SEQNO(2) TEXT(22).
    fn fp_bytes(rows: &[(&str, u32, u32, &str)]) -> Vec<u8> {
        let fields = [
            ("WKS", b'C', 8u8),
            ("CELLNO", b'N', 3),
            ("SEQNO", b'N', 2),
            ("TEXT", b'C', 22),
        ];
        let header_len = (32 + 32 * fields.len() + 1) as u16;
        let record_len = 1 + fields.iter().map(|f| f.2 as usize).sum::<usize>();
        let mut b = vec![0u8; 32];
        b[0] = 0x03;
        b[4..8].copy_from_slice(&(rows.len() as u32).to_le_bytes());
        b[8..10].copy_from_slice(&header_len.to_le_bytes());
        b[10..12].copy_from_slice(&(record_len as u16).to_le_bytes());
        for (name, kind, len) in fields {
            let mut d = [0u8; 32];
            d[..name.len()].copy_from_slice(name.as_bytes());
            d[11] = kind;
            d[16] = len;
            b.extend_from_slice(&d);
        }
        b.push(0x0D);
        for (wks, cell, seq, text) in rows {
            b.push(b' ');
            b.extend_from_slice(wks.as_bytes());
            b.resize(b.len() + 8 - wks.len(), b' ');
            let c = cell.to_string();
            b.extend_from_slice(c.as_bytes());
            b.resize(b.len() + 3 - c.len(), b' ');
            let s = seq.to_string();
            b.extend_from_slice(s.as_bytes());
            b.resize(b.len() + 2 - s.len(), b' ');
            b.extend_from_slice(text.as_bytes());
            b.resize(b.len() + 22 - text.len(), b' ');
        }
        b.push(0x1A);
        b
    }

    #[test]
    fn parses_leadsheet_numbers_and_columns() {
        let a1 = CellNumber::parse("A1").unwrap();
        assert_eq!((a1.column, a1.row), (1, 1));
        assert_eq!(a1.to_string(), "A1");
        assert_eq!(CellNumber::parse("c12").unwrap().to_string(), "C12");
        assert_eq!(CellNumber::parse("AA10").unwrap().to_string(), "AA10");
        assert_eq!(CellNumber::parse("ZZ3").unwrap().column, 702);
        assert!(CellNumber::parse("A0").is_none());
        assert!(CellNumber::parse("12").is_none());
        assert!(CellNumber::parse("A1B").is_none());
        assert!(CellNumber::parse("").is_none());
    }

    #[test]
    fn parses_cell_and_feed_links() {
        assert_eq!(
            CellRef::parse("C1"),
            Some(CellRef::Cell {
                number: CellNumber { column: 3, row: 1 },
                group: None
            })
        );
        assert_eq!(
            CellRef::parse("aEQ0.C1"),
            Some(CellRef::Cell {
                number: CellNumber { column: 3, row: 1 },
                group: Some("aEQ0".into())
            })
        );
        assert_eq!(
            CellRef::parse("CLP.2"),
            Some(CellRef::Leadsheet {
                wks: "CLP".into(),
                cellno: 2
            })
        );
        assert!(CellRef::parse("CLP").is_none());
        assert!(CellRef::parse("CLP.-1").is_none());
        assert!(CellRef::parse("CLP.x").is_none());
        assert_eq!(cref("aEQ0.C1").to_string(), "aEQ0.C1");
    }

    #[test]
    fn cell_name_parts() {
        assert_eq!(cell_number_of("aEQ0.C1"), CellNumber::parse("C1"));
        assert_eq!(cell_number_of("\u{1c}aEQ0.C1"), CellNumber::parse("C1"));
        assert_eq!(cell_number_of("1ML3"), None);
        assert_eq!(cell_group_of("aEQ0.C1"), Some("aEQ0"));
        assert_eq!(cell_group_of("\u{1c}aEQ0.C1"), Some("aEQ0"));
        assert_eq!(cell_group_of("C1"), None);
    }

    #[test]
    fn index_scopes_numbers_by_group() {
        let cells = vec![entry("aEQ0.C1"), entry("aER3.C1"), entry("C2")];
        let idx = CellIndex::build(&cells);
        let c1 = CellNumber::parse("C1").unwrap();
        assert_eq!(idx.first(c1), Some("aEQ0.C1"));
        assert_eq!(idx.first_in_group(c1, "aEQ0"), Some("aEQ0.C1"));
        assert_eq!(idx.first_in_group(c1, "aER3"), Some("aER3.C1"));
        assert_eq!(idx.first_in_group(c1, "nope"), None);
        // Cross-group repeats are normal, not collisions.
        assert!(idx.duplicates().is_empty());
        // Same group + same number is a real collision.
        let dup_cells = vec![entry("aEQ0.C1"), entry("aEQ0.C01")];
        let dup = CellIndex::build(&dup_cells).duplicates();
        assert_eq!(dup.len(), 1);
        assert_eq!(dup[0].1, vec!["aEQ0.C1", "aEQ0.C01"]);
    }

    #[test]
    fn substitutes_internal_cell_links() {
        let cells = vec![entry("C1"), entry("C2"), entry("C3")];
        let mut texts = TextTable::new();
        texts.insert("C1", "hello");
        texts.insert("C3", "own");
        let mut links = LinkTable::new();
        links.insert("C2", cref("C1"));
        let doc = render_cells(&cells, Some("01200RG"), &texts, &links, None);
        assert_eq!(doc.cell("C1").unwrap().text, "hello");
        assert_eq!(doc.cell("C1").unwrap().kind, CellKind::Literal);
        let c2 = doc.cell("C2").unwrap();
        assert_eq!(c2.kind, CellKind::CellLink);
        assert_eq!(c2.text, "hello");
        assert!(c2.issue.is_none());
        assert_eq!(doc.substituted(), 1);
    }

    #[test]
    fn bare_cell_links_resolve_in_the_source_group() {
        let cells = vec![entry("aEQ0.C1"), entry("aER3.C1"), entry("aEQ0.C2")];
        let mut texts = TextTable::new();
        texts.insert("aEQ0.C1", "eq");
        texts.insert("aER3.C1", "er");
        let mut links = LinkTable::new();
        links.insert("aEQ0.C2", cref("C1"));
        let out = render_cells(&cells, None, &texts, &links, None);
        assert_eq!(out.cell("aEQ0.C2").unwrap().text, "eq");
        // An explicit group overrides the source group.
        let mut links = LinkTable::new();
        links.insert("aEQ0.C2", cref("aER3.C1"));
        let out = render_cells(&cells, None, &texts, &links, None);
        assert_eq!(out.cell("aEQ0.C2").unwrap().text, "er");
        // A group with no such number is a miss even when another group has it.
        let mut links = LinkTable::new();
        links.insert("aEQ0.C2", cref("aEQ0.Z9"));
        let out = render_cells(&cells, None, &texts, &links, None);
        assert_eq!(
            out.cell("aEQ0.C2").unwrap().issue,
            Some(RenderIssue::MissingCell {
                target: CellNumber::parse("Z9").unwrap(),
                group: Some("aEQ0".into())
            })
        );
    }

    #[test]
    fn substitutes_leadsheet_feed() {        let cells = vec![entry("C1"), entry("aEQ0.C2")];
        let reg = WksRegistry::parse_fp_bytes(&fp_bytes(&[
            ("CLP", 2, 1, "Midco International   "),
            ("CLP", 2, 2, "Inc"),
        ]))
        .unwrap();
        let mut links = LinkTable::new();
        links.insert(
            "aEQ0.C2",
            CellRef::Leadsheet {
                wks: "CLP".into(),
                cellno: 2,
            },
        );
        let doc = render_cells(&cells, None, &TextTable::new(), &links, Some(&reg));
        let c = doc.cell("aEQ0.C2").unwrap();
        assert_eq!(c.kind, CellKind::LeadsheetLink);
        assert_eq!(c.text, "Midco International   Inc");
        assert!(c.issue.is_none());
        assert_eq!(c.group.as_deref(), Some("aEQ0"));
    }

    #[test]
    fn missing_targets_and_feeds_are_reported() {
        let cells = vec![entry("C1"), entry("C2")];
        let mut links = LinkTable::new();
        links.insert("C1", cref("Z9"));
        links.insert(
            "C2",
            CellRef::Leadsheet {
                wks: "UD".into(),
                cellno: 7,
            },
        );
        let reg = WksRegistry::parse_fp_bytes(&fp_bytes(&[("UD", 1, 1, "x")])).unwrap();
        let doc = render_cells(&cells, None, &TextTable::new(), &links, Some(&reg));
        assert_eq!(
            doc.cell("C1").unwrap().issue,
            Some(RenderIssue::MissingCell {
                target: CellNumber::parse("Z9").unwrap(),
                group: None
            })
        );
        assert_eq!(
            doc.cell("C2").unwrap().issue,
            Some(RenderIssue::MissingFeed {
                wks: "UD".into(),
                cellno: 7
            })
        );
        assert_eq!(doc.issue_count(), 2);
    }

    #[test]
    fn cycles_terminate_with_an_issue() {
        let cells = vec![entry("C1"), entry("C2")];
        let mut links = LinkTable::new();
        links.insert("C1", cref("C2"));
        links.insert("C2", cref("C1"));
        let doc = render_cells(&cells, None, &TextTable::new(), &links, None);
        assert_eq!(
            doc.cell("C1").unwrap().issue,
            Some(RenderIssue::Cycle { at: "C1".into() })
        );
        assert_eq!(doc.issue_count(), 2);
        // Self-link is the shortest cycle.
        let mut self_link = LinkTable::new();
        self_link.insert("C1", cref("C1"));
        let doc = render_cells(&cells[..1], None, &TextTable::new(), &self_link, None);
        assert_eq!(
            doc.cell("C1").unwrap().issue,
            Some(RenderIssue::Cycle { at: "C1".into() })
        );
    }

    #[test]
    fn number_order_and_text_dump_are_deterministic() {
        let mut cells = vec![entry("C2"), entry("A1"), entry("1ML3")];
        cells[0].layout_offset = 0;
        cells[1].layout_offset = 10;
        cells[2].layout_offset = 20;
        let mut texts = TextTable::new();
        texts.insert("C2", "two");
        texts.insert("A1", "one");
        let doc = render_cells(&cells, None, &TextTable::new(), &LinkTable::new(), None);
        let order: Vec<&str> = doc.in_number_order().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(order, vec!["A1", "C2", "1ML3"]);
        // Directory order is preserved in the dump, not number order.
        let text = doc.to_text();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "form -");
        assert_eq!(lines[1], "cells 3 substituted 0 issues 0");
        assert_eq!(lines[2], "C2\tC2\ttext\t");
        assert_eq!(lines[3], "A1\tA1\ttext\t");
        assert_eq!(lines[4], "1ML3\t-\ttext\t");
        assert_eq!(text, doc.to_text());
        assert_eq!(texts.get("A1"), Some("one"));
    }

    #[test]
    fn parsers_accept_assignments() {
        let links = LinkTable::parse("# comment\nC2=C1\nC3=aEQ0.C1\naEQ0.C4=CLP.2\n").unwrap();
        assert_eq!(links.len(), 3);
        assert_eq!(links.get("aEQ0.C4"), Some(&CellRef::Leadsheet { wks: "CLP".into(), cellno: 2 }));
        assert!(LinkTable::parse("C1=nope").is_err());
        let texts = TextTable::parse("# t\nC1=hello world \n").unwrap();
        assert_eq!(texts.get("C1"), Some("hello world"));
        assert!(TextTable::parse("missing equals").is_err());
        // Control-prefixed corpus names match canonical file keys.
        let links = LinkTable::parse("aEQ0.C2=C1\n").unwrap();
        assert!(links.get_for("\u{1c}aEQ0.C2").is_some());
        assert!(links.get_for("aEQ0.C3").is_none());
    }

    #[test]
    fn control_names_are_escaped_in_the_dump() {
        let doc = render_cells(&[entry("\u{1c}aEQ0.C1")], None, &TextTable::new(), &LinkTable::new(), None);
        assert!(doc.to_text().contains("\\x1caEQ0.C1"));
    }
}

