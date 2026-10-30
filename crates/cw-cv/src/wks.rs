//! Linked-worksheet registry: the `FP` cell store + the template index (F2).
//!
//! CaseView documents never carry their linked-worksheet values. Those live in
//! the engagement's `FP.dbf` table as a **worksheet cell store**:
//!
//! ```text
//! FP := { WKS(C,8) CELLNO(N,3) SEQNO(N,2) TEXT(C,22) CWGUID CWDATE CREATEDON }*
//! ```
//!
//! `WKS` is the worksheet code — `CLP` = client profile, `UD` = user-defined,
//! `PRG` = program sheet in every corpus engagement — and one worksheet cell is
//! a run of `SEQNO` 1..n whose `TEXT` chunks concatenate **without separator**
//! (each record is a fixed 22-byte slice of the cell value; the last slice is
//! space-padded).
//!
//! Corpus law (F2, `tests\golden\dbf\{corp2026,hull2024,mwgh2025}`): the
//! `(WKS, CELLNO, SEQNO)` key set is **identical in all three engagements**
//! (620 rows, `CLP` 441 cells / 571 rows, `UD` 15 / 48, `PRG` 1 / 1) even
//! though the three template families differ (`01200RG…` vs `RSMAUDIT`/
//! `RSMSEC` vs the `USI` training pack) ⇒ the registry is **template-frozen**.
//! That is why [`WksRegistry::signature`] is the frozen identity and
//! [`LinkedWorksheets`] carries both halves: registry (values) + template set
//! (which `.cvw` documents the engagement actually ships).
//!
//! Not decoded here (still open): the reverse pointer — which `.cvw` cell
//! reads which `(WKS, CELLNO)`. The reference text lives in the compressed
//! `Strings` pool (F1 residual), so binding is reported as unknown (`[?]`),
//! not invented.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cw_core::dbf::Table;
use cw_core::error::{CoreError, Result};

use crate::cvw::Cvw;

/// Worksheet codes observed in every corpus engagement, in registry order.
pub const BUILTIN_WKS: [&str; 3] = ["CLP", "PRG", "UD"];

/// Field names of the `FP` linked-worksheet store.
pub const FIELDS: [&str; 4] = ["WKS", "CELLNO", "SEQNO", "TEXT"];

/// One linked-worksheet cell: its `SEQNO` chunks in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WksCell {
    cellno: u32,
    /// `SEQNO` 1..n, each the raw 22-byte `TEXT` slice (space-padded).
    parts: Vec<String>,
}

impl WksCell {
    pub fn cellno(&self) -> u32 {
        self.cellno
    }

    /// Faithful value: chunk concatenation with no separator, trailing cell
    /// padding removed. Interior spaces are content (a 22-byte slice boundary
    /// keeps them), so this is the exact text CaseView stored.
    pub fn text(&self) -> String {
        let mut s = String::new();
        for p in &self.parts {
            s.push_str(p);
        }
        s.trim_end_matches(' ').to_string()
    }

    /// Raw chunk concatenation, padding included (byte-faithful).
    pub fn raw(&self) -> String {
        self.parts.concat()
    }

    pub fn part_count(&self) -> usize {
        self.parts.len()
    }

    pub fn parts(&self) -> &[String] {
        &self.parts
    }
}

/// One linked worksheet (`WKS` code) and its cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worksheet {
    code: String,
    rows: usize,
    cells: BTreeMap<u32, WksCell>,
}

impl Worksheet {
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Physical `FP` records for this worksheet (all `SEQNO` chunks).
    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }

    pub fn cells(&self) -> impl Iterator<Item = &WksCell> {
        self.cells.values()
    }

    pub fn cell(&self, cellno: u32) -> Option<&WksCell> {
        self.cells.get(&cellno)
    }

    pub fn text(&self, cellno: u32) -> Option<String> {
        self.cells.get(&cellno).map(WksCell::text)
    }

    /// Lowest / highest `CELLNO` present (not necessarily dense).
    pub fn cell_range(&self) -> Option<(u32, u32)> {
        let mut it = self.cells.keys();
        let first = *it.next()?;
        let last = *self.cells.keys().next_back()?;
        Some((first, last))
    }
}

/// `FP.dbf` decoded into linked worksheets, keyed by `WKS` code.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WksRegistry {
    worksheets: BTreeMap<String, Worksheet>,
    rows: usize,
    source: Option<PathBuf>,
}

/// One frozen-key mismatch between two registries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryDelta {
    pub wks: String,
    pub cellno: u32,
    pub seqno: u32,
    /// `true` when the key is in `self` but not in the other registry.
    pub missing_other: bool,
}

impl WksRegistry {
    /// Decode `FP.dbf` bytes. Live records only; deleted rows are skipped.
    pub fn parse_fp_bytes(bytes: &[u8]) -> Result<WksRegistry> {
        let table = Table::parse(bytes)?;
        Self::from_table(&table)
    }

    pub fn parse_fp_path(path: impl AsRef<Path>) -> Result<WksRegistry> {
        let path = path.as_ref();
        let bytes = std::fs::read(path)?;
        let mut reg = Self::parse_fp_bytes(&bytes)?;
        reg.source = Some(path.to_path_buf());
        Ok(reg)
    }

    /// Decode an already-parsed `FP` table (kernel `dbf::Table`).
    pub fn from_table(table: &Table) -> Result<WksRegistry> {
        for f in FIELDS {
            if table.field_index(f).is_none() {
                return Err(CoreError::Format(format!(
                    "FP table missing field {f} (fields: {:?})",
                    table.fields.iter().map(|x| &x.name).collect::<Vec<_>>()
                )));
            }
        }
        // (wks, cellno) -> (seqno -> text)
        let mut acc: BTreeMap<String, BTreeMap<u32, BTreeMap<u32, String>>> = BTreeMap::new();
        let mut rows = 0usize;
        for r in 0..table.count() {
            if table.is_deleted(r) {
                continue;
            }
            let wks = text_cell(table, r, "WKS");
            if wks.is_empty() {
                return Err(CoreError::Format(format!("FP record {r}: blank WKS")));
            }
            let cellno = num_cell(table, r, "CELLNO")?;
            let seqno = num_cell(table, r, "SEQNO")?;
            let text = table.cell_str(r, "TEXT").unwrap_or_default();
            let by_cell = acc.entry(wks.clone()).or_default();
            let by_seq = by_cell.entry(cellno).or_default();
            if by_seq.insert(seqno, text).is_some() {
                return Err(CoreError::Format(format!(
                    "FP record {r}: duplicate ({wks},{cellno},{seqno})"
                )));
            }
            rows += 1;
        }

        let mut worksheets = BTreeMap::new();
        for (code, cells) in acc {
            let mut out = BTreeMap::new();
            let mut ws_rows = 0usize;
            for (cellno, mut by_seq) in cells {
                let n = by_seq.len() as u32;
                // Law: SEQNO is exactly 1..n, no gaps and no out-of-range keys.
                if let Some(bad) = (1..=n).find(|s| !by_seq.contains_key(s)) {
                    return Err(CoreError::Format(format!(
                        "FP ({code},{cellno}): SEQNO gap at {bad} (n={n})"
                    )));
                }
                if let Some(&extra) = by_seq.keys().find(|&&s| s == 0 || s > n) {
                    return Err(CoreError::Format(format!(
                        "FP ({code},{cellno}): SEQNO {extra} outside 1..={n}"
                    )));
                }
                let parts: Vec<String> = (1..=n)
                    .map(|s| by_seq.remove(&s).unwrap_or_default())
                    .collect();
                ws_rows += parts.len();
                out.insert(
                    cellno,
                    WksCell {
                        cellno,
                        parts,
                    },
                );
            }
            worksheets.insert(
                code.clone(),
                Worksheet {
                    code,
                    rows: ws_rows,
                    cells: out,
                },
            );
        }
        Ok(WksRegistry {
            worksheets,
            rows,
            source: None,
        })
    }

    /// Find `<stem>FP.dbf` (any case) directly inside `dir`.
    pub fn find_fp(dir: &Path) -> Option<PathBuf> {
        let rd = std::fs::read_dir(dir).ok()?;
        for e in rd.flatten() {
            let p = e.path();
            let name = p.file_name()?.to_string_lossy().to_string();
            let lower = name.to_ascii_lowercase();
            if lower.ends_with("fp.dbf") {
                return Some(p);
            }
        }
        None
    }

    /// Load the `FP` table of one engagement directory (templates alongside).
    pub fn from_engagement_dir(dir: impl AsRef<Path>) -> Result<WksRegistry> {
        let dir = dir.as_ref();
        let path = Self::find_fp(dir).ok_or_else(|| {
            CoreError::Format(format!("no *FP.dbf in {}", dir.display()))
        })?;
        Self::parse_fp_path(path)
    }

    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    pub fn codes(&self) -> Vec<&str> {
        self.worksheets.keys().map(String::as_str).collect()
    }

    pub fn worksheets(&self) -> impl Iterator<Item = &Worksheet> {
        self.worksheets.values()
    }

    pub fn get(&self, wks: &str) -> Option<&Worksheet> {
        self.worksheets.get(wks)
    }

    /// All `FP` records (all `SEQNO` chunks) in the registry.
    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cell_count(&self) -> usize {
        self.worksheets.values().map(Worksheet::cell_count).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.worksheets.is_empty()
    }

    /// Resolve one linked-worksheet cell by `(WKS, CELLNO)`.
    pub fn text(&self, wks: &str, cellno: u32) -> Option<String> {
        self.worksheets.get(wks)?.text(cellno)
    }

    /// Frozen identity: the sorted `(WKS, CELLNO, SEQNO)` key set. Values are
    /// engagement data and therefore excluded.
    pub fn signature(&self) -> Vec<(String, u32, u32)> {
        let mut keys = Vec::with_capacity(self.rows);
        for ws in self.worksheets.values() {
            for c in ws.cells() {
                for seq in 1..=c.part_count() as u32 {
                    keys.push((ws.code.clone(), c.cellno(), seq));
                }
            }
        }
        keys.sort();
        keys
    }

    /// Keys in `self` not in `other`, plus keys in `other` not in `self`.
    /// Empty ⇒ the two registries are template-frozen against each other.
    pub fn delta(&self, other: &WksRegistry) -> Vec<RegistryDelta> {
        let mine: std::collections::BTreeSet<_> =
            self.signature().into_iter().collect();
        let theirs: std::collections::BTreeSet<_> =
            other.signature().into_iter().collect();
        let mut out = Vec::new();
        for (wks, cellno, seqno) in mine.difference(&theirs) {
            out.push(RegistryDelta {
                wks: wks.clone(),
                cellno: *cellno,
                seqno: *seqno,
                missing_other: true,
            });
        }
        for (wks, cellno, seqno) in theirs.difference(&mine) {
            out.push(RegistryDelta {
                wks: wks.clone(),
                cellno: *cellno,
                seqno: *seqno,
                missing_other: false,
            });
        }
        out
    }
}

/// One `.cvw`/`.cvv` template shipped by an engagement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateRef {
    /// `CvData` FORM code, else the code embedded in the filename.
    pub form: String,
    pub path: PathBuf,
    pub cells: usize,
}

/// The `.cvw`/`.cvv` set next to an engagement's `FP.dbf`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TemplateSet {
    templates: BTreeMap<String, TemplateRef>,
}

impl TemplateSet {
    /// Scan `dir` recursively for CaseView documents and index them by FORM.
    /// Decode failures are returned as errors (the caller sees the file).
    pub fn scan(dir: impl AsRef<Path>) -> Result<TemplateSet> {
        let mut files = Vec::new();
        collect_templates(dir.as_ref(), &mut files);
        files.sort();
        let mut templates = BTreeMap::new();
        for path in files {
            let bytes = std::fs::read(&path)?;
            let doc = Cvw::parse(&bytes)?;
            let form = doc
                .form()
                .map(str::to_string)
                .or_else(|| form_from_filename(&path))
                .ok_or_else(|| {
                    CoreError::Format(format!(
                        "{}: no CvData FORM and no filename code",
                        path.display()
                    ))
                })?;
            let cells = doc.cells().len();
            templates.entry(form.clone()).or_insert(TemplateRef {
                form,
                path,
                cells,
            });
        }
        Ok(TemplateSet { templates })
    }

    pub fn forms(&self) -> Vec<&str> {
        self.templates.keys().map(String::as_str).collect()
    }

    pub fn get(&self, form: &str) -> Option<&TemplateRef> {
        self.templates.get(form)
    }

    pub fn len(&self) -> usize {
        self.templates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.templates.is_empty()
    }

    pub fn cells_total(&self) -> usize {
        self.templates.values().map(|t| t.cells).sum()
    }
}

/// Both halves of the F2 layer for one engagement directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedWorksheets {
    pub dir: PathBuf,
    pub registry: WksRegistry,
    pub templates: TemplateSet,
}

impl LinkedWorksheets {
    /// Load `FP.dbf` + the template set from `dir`; `None` when `dir` has no
    /// `FP` table (a plain document folder).
    pub fn load(dir: impl AsRef<Path>) -> Result<Option<LinkedWorksheets>> {
        let dir = dir.as_ref();
        if WksRegistry::find_fp(dir).is_none() {
            return Ok(None);
        }
        Ok(Some(LinkedWorksheets {
            dir: dir.to_path_buf(),
            registry: WksRegistry::from_engagement_dir(dir)?,
            templates: TemplateSet::scan(dir)?,
        }))
    }
}

fn text_cell(table: &Table, r: usize, name: &str) -> String {
    table
        .cell_str(r, name)
        .unwrap_or_default()
        .trim_end()
        .to_string()
}

fn num_cell(table: &Table, r: usize, name: &str) -> Result<u32> {
    // dBase numerics are right-justified (`"  2"`).
    let raw = text_cell(table, r, name).trim().to_string();
    raw.parse::<u32>().map_err(|_| {
        CoreError::Format(format!("FP record {r}: {name} = {raw:?} is not a u32"))
    })
}

/// Filename code: text after the last `)` with the worksheet-variant digit
/// removed (`…(1148441)01200RG0.cvw` → `01200RG`); bare stems like `rm` and
/// template ids like `EXPO0IS0` survive.
fn form_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy().to_string();
    let mut code = match stem.rfind(')') {
        Some(i) => stem[i + 1..].to_string(),
        None => stem,
    };
    // Variant suffix = 5 digits + 2 letters + 1 digit (`01200RG0`).
    let b = code.as_bytes();
    if b.len() >= 8
        && b[b.len() - 8..b.len() - 3].iter().all(u8::is_ascii_digit)
        && b[b.len() - 3..b.len() - 1].iter().all(u8::is_ascii_alphabetic)
        && b[b.len() - 1].is_ascii_digit()
    {
        code.pop();
    }
    if code.is_empty() {
        None
    } else {
        Some(code)
    }
}

fn collect_templates(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_templates(&p, out);
        } else if matches!(
            p.extension().and_then(|x| x.to_str()),
            Some("cvw") | Some("cvv")
        ) {
            out.push(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal dBase III `FP` table: WKS(8) CELLNO(3) SEQNO(2) TEXT(22).
    fn fp_bytes(rows: &[(&str, &str, &str, &str)]) -> Vec<u8> {
        let fields = [("WKS", b'C', 8u8), ("CELLNO", b'N', 3), ("SEQNO", b'N', 2), ("TEXT", b'C', 22)];
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
            b.extend_from_slice(cell.as_bytes());
            b.resize(b.len() + 3 - cell.len(), b' ');
            b.extend_from_slice(seq.as_bytes());
            b.resize(b.len() + 2 - seq.len(), b' ');
            b.extend_from_slice(text.as_bytes());
            b.resize(b.len() + 22 - text.len(), b' ');
        }
        b.push(0x1A);
        b
    }

    #[test]
    fn parses_fp_and_joins_seqno_chunks() {
        let bytes = fp_bytes(&[
            ("CLP", "2", "1", "Midco International   "),
            ("CLP", "2", "2", "Inc"),
            ("CLP", "3", "1", "4140 W Victoria St"),
            ("UD", "1", "1", "x"),
        ]);
        let reg = WksRegistry::parse_fp_bytes(&bytes).unwrap();
        assert_eq!(reg.codes(), vec!["CLP", "UD"]);
        assert_eq!(reg.rows(), 4);
        assert_eq!(reg.cell_count(), 3);
        let clp = reg.get("CLP").unwrap();
        assert_eq!(clp.cell_count(), 2);
        assert_eq!(clp.cell_range(), Some((2, 3)));
        assert_eq!(clp.text(2).unwrap(), "Midco International   Inc");
        assert_eq!(clp.cell(2).unwrap().part_count(), 2);
        assert_eq!(reg.text("UD", 1).as_deref(), Some("x"));
        assert_eq!(reg.text("UD", 9), None);
    }

    #[test]
    fn seqno_gap_is_a_format_error() {
        let bytes = fp_bytes(&[("CLP", "2", "1", "a"), ("CLP", "2", "3", "c")]);
        let err = WksRegistry::parse_fp_bytes(&bytes).unwrap_err().to_string();
        assert!(err.contains("SEQNO gap at 2"), "{err}");
    }

    #[test]
    fn signature_is_sorted_and_delta_empty_for_same_keys() {
        let a = WksRegistry::parse_fp_bytes(&fp_bytes(&[
            ("UD", "1", "1", "one"),
            ("CLP", "9", "1", "x"),
        ]))
        .unwrap();
        let b = WksRegistry::parse_fp_bytes(&fp_bytes(&[
            ("CLP", "9", "1", "different value"),
            ("UD", "1", "1", "also different"),
        ]))
        .unwrap();
        assert_eq!(
            a.signature(),
            vec![
                ("CLP".into(), 9, 1),
                ("UD".into(), 1, 1),
            ]
        );
        assert!(a.delta(&b).is_empty(), "values must not affect identity");

        let c = WksRegistry::parse_fp_bytes(&fp_bytes(&[("CLP", "9", "1", "x")])).unwrap();
        let d = a.delta(&c);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].wks, "UD");
        assert!(d[0].missing_other);
    }

    #[test]
    fn filename_form_extraction() {
        assert_eq!(
            form_from_filename(Path::new("Engagement(1148441)01200RG0.cvw")).as_deref(),
            Some("01200RG")
        );
        assert_eq!(
            form_from_filename(Path::new("Engagement(1239263)rm.cvw")).as_deref(),
            Some("rm")
        );
        assert_eq!(
            form_from_filename(Path::new("EXPO0IS0.cvw")).as_deref(),
            Some("EXPO0IS0")
        );
        assert_eq!(
            form_from_filename(Path::new("Engagement(1)22.cvw")).as_deref(),
            Some("22")
        );
    }
}

