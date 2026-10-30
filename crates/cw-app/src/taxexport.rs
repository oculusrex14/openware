//! cw-app tax-export writers: the vendor-neutral record model plus the two
//! formats whose grammar is recoverable.
//!
//! ## What the product actually does (established, `x64cw42` + `X64CWEXPORT`)
//!
//! Tax export is **not** a set of per-vendor DLLs and **not** a template set on
//! disk. Three layers, each verified:
//!
//! 1. **The mapping is data.** `CWTaxExportCode` rows live in the client DB
//!    (`TP.dbf`) and the vendor selector is the typelib enum `CWTaxVendorType`
//!    (`tvtTaxPrep`=0, `tvtGenericRSI`=1, `tvtUSAProSystemfx`=2, `tvtUSALacerte`=3,
//!    `tvtUSAGoSystem`=4, `tvtUSAUtraTax`=5, `tvtUSAOther`=6). So *which* of our
//!    accounts goes to *which* vendor line is a row lookup, not code.
//! 2. **The writer is a separate module.** Every export verb is an
//!    `X64CWEXPORT.dll` import: `__expacc`, `__expbl`, `__expat`, `__expra`,
//!    `__expwp`, `__expgf`, `comExport`, `callCWExportDll`, `callCWExportTaxCycle`,
//!    `callCWExportPayroll`, `callCWExportIDEA`, `callCWExportAdjustments`,
//!    `ExportGreenPoint`. `x64cw42` orchestrates; the formats live in that
//!    unstaged DLL.
//! 3. **Per-vendor *layouts* are therefore neither in code we have nor on disk.**
//!    The install manifest (Appendix A) contains **no** tax template files — no
//!    `.cgt`, no `.tlb`, no per-vendor `.csv`/`.txt` in `Document Library\`,
//!    `Styles\` or `Template\`. The only vendor-layout *data* the product ships
//!    is the `TP.dbf` code table itself.
//!
//! ## What is recoverable, and what is implemented here
//!
//! | target | status |
//! |---|---|
//! | **TaxCycle** | **Implemented.** The handoff is `callCWExportTaxCycle(hwnd, path, gifiPath, outPath)`; the vendor's own documented ingest format is a two-column sheet whose first column is a **field code** and whose second is the value, on a sheet named `Export`, with the first row reserved for titles (`taxcycle.com` help topic *Excel® Import Forms*). For the GIFI/T2 lane the exchange file is `.GFI` (established: `x64cw42` strings `.GFI`, `CWGIFI`, `TaxCycle GIFI Standard File`, `TaxCycle T2 Files (*.*T2)|*.*T2||`). Implemented as [`TaxCycleLayout::field_code_rows`] over GIFI codes, and `.GFI` as the filename contract. |
//! | **ProSystem fx** | **Not implemented — documented.** The product writes `COMPFILE.txt` and Caseware's own doc states the file is consumed by ProSystem fx Tax's *G/L Bridge* with the accounting system set to `ProSystem fx Engagement` (and for ≤2006, `CaseWare`). Nothing published specifies the record grammar, and `X64CWEXPORT.dll` is not staged. Shipping an invented `COMPFILE.txt` would be a half-format, so this stays a documented gap with the exact target contract recorded. |
//! | **Vendor-neutral CSV** | **Implemented.** A stable, documented column set over `TP`+`AM`+`BL`, so any downstream consumer can read it without our schema. |
//!
//! ## Design rules
//!
//! * The neutral writer emits **one row per account**, not per tax code: the
//!   account is the entity that carries balances, and a code may be blank.
//! * A row whose `TAC_NO` does not resolve in the `TP` table is **reported**, not
//!   silently exported — the same "atomic refusal" rule the rest of `cw-app`
//!   follows.
//! * `SIGN = 'P'` flips the exported amount (established column semantics: the COM
//!   property is `SwitchSign`, and the corpus stores `''`/`P`).
//! * Amounts are formatted with the established `%.2f` convention (`N 19.2` fields);
//!   see [`crate::fx`]'s rounding law for why this is `_fcvt`, not Rust `{:.2}`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use cw_core::dbf::Table;
use cw_core::{CoreError, Result};

use crate::accounts::AmAccount;
use crate::fx::round_half_away;
use crate::tax::{TaxExportCode, TaxExportTable};

/// `CWTaxVendorType` — the typelib enum, verbatim.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TaxVendor {
    /// 0 — `tvtTaxPrep`.
    TaxPrep,
    /// 1 — `tvtGenericRSI`.
    GenericRsi,
    /// 2 — `tvtUSAProSystemfx`.
    ProSystemFx,
    /// 3 — `tvtUSALacerte`.
    Lacerte,
    /// 4 — `tvtUSAGoSystem`.
    GoSystem,
    /// 5 — `tvtUSAUtraTax` (the vendor's own spelling keeps the typo).
    UltraTax,
    /// 6 — `tvtUSAOther`.
    Other,
}

impl TaxVendor {
    /// Decode the enum value exactly as the typelib stores it.
    pub fn from_code(n: i64) -> Option<TaxVendor> {
        Some(match n {
            0 => TaxVendor::TaxPrep,
            1 => TaxVendor::GenericRsi,
            2 => TaxVendor::ProSystemFx,
            3 => TaxVendor::Lacerte,
            4 => TaxVendor::GoSystem,
            5 => TaxVendor::UltraTax,
            6 => TaxVendor::Other,
            _ => return None,
        })
    }

    /// The numeric value the typelib assigns.
    pub fn code(self) -> i64 {
        match self {
            TaxVendor::TaxPrep => 0,
            TaxVendor::GenericRsi => 1,
            TaxVendor::ProSystemFx => 2,
            TaxVendor::Lacerte => 3,
            TaxVendor::GoSystem => 4,
            TaxVendor::UltraTax => 5,
            TaxVendor::Other => 6,
        }
    }

    /// The display name the product itself uses.
    pub fn display(self) -> &'static str {
        match self {
            TaxVendor::TaxPrep => "TaxPrep",
            TaxVendor::GenericRsi => "Generic RSI",
            TaxVendor::ProSystemFx => "ProSystem fx Tax (USA)",
            TaxVendor::Lacerte => "Lacerte Tax (USA)",
            TaxVendor::GoSystem => "GoSystem Tax (USA)",
            TaxVendor::UltraTax => "UltraTax (USA)",
            TaxVendor::Other => "Other",
        }
    }

    /// Whether a machine-readable writer exists for this vendor.
    ///
    /// Only TaxCycle has a *documented* ingest grammar; the rest are declared so
    /// the enum is complete, and refuse to render rather than emit a guess.
    pub fn writer(self) -> Option<VendorWriter> {
        match self {
            TaxVendor::TaxPrep => Some(VendorWriter::TaxCycleFieldCode),
            _ => None,
        }
    }
}

/// A machine-readable writer this crate can produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VendorWriter {
    /// TaxCycle's documented two-column `Export` sheet: `FieldCode, Data`.
    TaxCycleFieldCode,
}

/// Which balance to export.
///
/// The corpus's `AM` carries `CUR_YTD`, `AJE_BAL`, `EJE_BAL`, `RJE_BAL`, `OPEN`
/// and the derived `act()` buckets. These three are the ones a tax package
/// consumes: unadjusted, adjusted (after AJE/RJE/EJE), and prior-year opening.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BalanceBasis {
    /// `CUR_YTD`.
    Unadjusted,
    /// `CUR_YTD + AJE_BAL + RJE_BAL + EJE_BAL`.
    Adjusted,
    /// `OPEN` (prior-year opening).
    PriorOpen,
}

impl BalanceBasis {
    pub fn label(self) -> &'static str {
        match self {
            BalanceBasis::Unadjusted => "Unadjusted",
            BalanceBasis::Adjusted => "Adjusted",
            BalanceBasis::PriorOpen => "Prior Open",
        }
    }
}

/// One account's exportable fact set — the shared record model.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportRow {
    /// `AM.AC_NO`, trimmed as the product trims it for display.
    pub ac_no: String,
    /// `AM.SCHEDULE` (the report/lead-sheet reference).
    pub schedule: String,
    /// `AM.DESC`.
    pub description: String,
    /// `AM.TAC_NO` — the tax code that selects the `TP` row.
    pub tac_no: String,
    /// `AM.GIFI` — the Canadian GIFI code.
    pub gifi: String,
    /// `TP.FORM` for the resolved code (blank when unresolved).
    pub form: String,
    /// `TP.SUB` for the resolved code.
    pub sub: String,
    /// The amount for the requested basis, already sign-switched.
    pub amount: f64,
    /// True when the row's `TAC_NO` matched a `TP` row.
    pub code_resolved: bool,
}

impl ExportRow {
    /// Build one row from an `AM` row and the decoded `TP` table.
    ///
    /// `SIGN = 'P'` flips the amount; that is the `SwitchSign` COM property and
    /// the only sign rule the corpus stores.
    pub fn build(
        a: &AmAccount,
        tp: &TaxExportTable,
        basis: BalanceBasis,
        form: &str,
        entity: &str,
        year: &str,
    ) -> ExportRow {
        let code = tp.resolve(&a.tac_no, form, entity, year);
        let raw = match basis {
            BalanceBasis::Unadjusted => a.cur_ytd,
            BalanceBasis::Adjusted => a.cur_ytd + a.aje_bal + a.rje_bal + a.eje_bal,
            BalanceBasis::PriorOpen => a.open,
        };
        let amount = match code {
            Some(c) if flips_sign(c) => -raw,
            _ => raw,
        };
        ExportRow {
            ac_no: a.ac_no.trim().to_string(),
            schedule: a.schedule.trim().to_string(),
            description: a.desc.trim().to_string(),
            tac_no: a.tac_no.trim().to_string(),
            gifi: a.gifi.trim().to_string(),
            form: code.map(|c| c.form.trim().to_string()).unwrap_or_default(),
            sub: code.map(|c| c.sub.trim().to_string()).unwrap_or_default(),
            amount: round_half_away(amount, 2),
            code_resolved: code.is_some(),
        }
    }
}

/// `TP.SIGN = 'P'` means "switch the sign on export" (`SwitchSign`).
fn flips_sign(c: &TaxExportCode) -> bool {
    c.sign.trim().eq_ignore_ascii_case("P")
}

/// Why an export could not be produced.
#[derive(Clone, Debug, PartialEq)]
pub enum ExportError {
    /// One or more rows reference a tax code absent from `TP`.
    UnresolvedCodes { codes: Vec<String> },
    /// The requested vendor has no documented writer.
    NoWriter { vendor: TaxVendor },
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExportError::UnresolvedCodes { codes } => write!(
                f,
                "{} account(s) reference tax codes absent from TP: {}",
                codes.len(),
                codes.join(", ")
            ),
            ExportError::NoWriter { vendor } => write!(
                f,
                "no documented writer for {} (see the Q23 dossier: X64CWEXPORT.dll is unstaged)",
                vendor.display()
            ),
        }
    }
}

impl std::error::Error for ExportError {}

/// Render an amount the way an `N x.2` field renders: two decimals, half away
/// from zero, `-` for negatives, no thousands separator.
pub fn amount_text(v: f64) -> String {
    let r = round_half_away(v, 2);
    // `-0.00` is normalised to `0.00`: the field renderer places the sign only
    // when the value is non-zero.
    if r == 0.0 {
        "0.00".to_string()
    } else {
        format!("{:.2}", r.abs()).pipe_negative(r)
    }
}

trait PipeNegative {
    fn pipe_negative(self, r: f64) -> String;
}

impl PipeNegative for String {
    fn pipe_negative(self, r: f64) -> String {
        if r < 0.0 {
            format!("-{self}")
        } else {
            self
        }
    }
}

/// CSV-quote one field per RFC 4180: quote when it contains a comma, quote,
/// CR or LF; double embedded quotes.
pub fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// The vendor-neutral CSV column order. Frozen: downstream consumers key on it.
pub const NEUTRAL_COLUMNS: [&str; 8] = [
    "Account",
    "Schedule",
    "Description",
    "TaxCode",
    "Form",
    "Sub",
    "GIFI",
    "Amount",
];

/// Render the vendor-neutral CSV.
///
/// One header row in [`NEUTRAL_COLUMNS`] order, then one row per account.
/// Unresolved tax codes are exported with blank `Form`/`Sub` **and** cause
/// [`ExportError::UnresolvedCodes`] when [`render_neutral_csv`] is called with
/// `strict = true`.
pub fn render_neutral_csv(rows: &[ExportRow], strict: bool) -> std::result::Result<String, ExportError> {
    if strict {
        let missing = unresolved(rows);
        if !missing.is_empty() {
            return Err(ExportError::UnresolvedCodes { codes: missing });
        }
    }
    let mut out = String::new();
    out.push_str(&NEUTRAL_COLUMNS.join(","));
    out.push('\n');
    for r in rows {
        let cells = [
            csv_field(&r.ac_no),
            csv_field(&r.schedule),
            csv_field(&r.description),
            csv_field(&r.tac_no),
            csv_field(&r.form),
            csv_field(&r.sub),
            csv_field(&r.gifi),
            amount_text(r.amount),
        ];
        out.push_str(&cells.join(","));
        out.push('\n');
    }
    Ok(out)
}

fn unresolved(rows: &[ExportRow]) -> Vec<String> {
    let mut v: Vec<String> = rows
        .iter()
        .filter(|r| !r.code_resolved && !r.tac_no.is_empty())
        .map(|r| r.tac_no.clone())
        .collect();
    v.sort();
    v.dedup();
    v
}

/// The TaxCycle `Export`-sheet grammar, as documented by the vendor.
///
/// Proven constraints (taxcycle.com → *Excel® Import Forms*):
///
/// * the data must be on a worksheet named exactly `Export`;
/// * row 1 is reserved for column titles and is not imported;
/// * **column 1 is the field code** — the same codes used in templates, with the
///   `{{ }}` braces and the `CurrentClient.` prefix stripped, and without the
///   parent form (`T2125.Income.M[1]`, not `{{CurrentClient.T2125Forms[1].T2125.Income.M[1]}}`);
/// * **column 2 is the value**;
/// * further columns are ignored, and further sheets are ignored.
///
/// So the payload is a (code, value) pair list, not a fixed schema — which is
/// exactly why nothing about it needs to be guessed: we emit the pairs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaxCycleLayout;

impl TaxCycleLayout {
    /// The spreadsheet/worksheet name the vendor requires.
    pub const SHEET: &'static str = "Export";
    /// Column 1 title, exactly as the vendor's own generated template names it.
    pub const COL_CODE: &'static str = "FieldCode";
    /// Column 2 title.
    pub const COL_DATA: &'static str = "Data";

    /// The filename extension the GIFI/T2 lane exchanges (established: `x64cw42`
    /// strings `.GFI`, `CWGIFI`, `TaxCycle GIFI Standard File`).
    pub const GIFI_EXTENSION: &'static str = ".GFI";
    /// The T2 return file extension the export dialog filters on.
    pub const T2_FILTER: &'static str = "*.*T2";

    /// Build the (field-code, value) pairs for the GIFI lane.
    ///
    /// The field code used is the GIFI code itself — that is the identifier
    /// TaxCycle's T2 GIFI schedule keys on, and it is what our `AM.GIFI` column
    /// stores. Rows without a GIFI code are skipped: TaxCycle's import is
    /// code-addressed, so a row with no code has no destination.
    pub fn field_code_rows(rows: &[ExportRow]) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for r in rows {
            if r.gifi.is_empty() {
                continue;
            }
            out.push((r.gifi.clone(), amount_text(r.amount)));
        }
        out
    }

    /// Render the pairs as a CSV standing in for the `Export` sheet.
    ///
    /// This is the *sheet content*, not an `.xlsx`: writing a real workbook
    /// needs a spreadsheet writer, and inventing one here would be a second
    /// format to maintain. A caller that has one can drop these two columns into
    /// it unchanged.
    pub fn render_sheet(rows: &[ExportRow]) -> String {
        let mut out = String::new();
        // Row 1 is the title row (not imported by the vendor).
        let _ = writeln!(out, "{},{}", Self::COL_CODE, Self::COL_DATA);
        for (code, value) in Self::field_code_rows(rows) {
            let _ = writeln!(out, "{},{}", csv_field(&code), value);
        }
        out
    }

    /// The `.GFI`-named file the GIFI lane produces, with its sheet content.
    pub fn render_gifi(rows: &[ExportRow], stem: &str) -> (String, String) {
        (
            format!("{stem}{}", Self::GIFI_EXTENSION),
            Self::render_sheet(rows),
        )
    }
}

/// Build [`ExportRow`]s for every live account in an `AM` table.
///
/// `form`/`entity`/`year` scope the `TP` lookup, exactly as
/// [`TaxExportTable::resolve`] does.
pub fn collect_rows(
    am: &Table,
    tp: &TaxExportTable,
    basis: BalanceBasis,
    form: &str,
    entity: &str,
    year: &str,
) -> Result<Vec<ExportRow>> {
    let mut out = Vec::new();
    for r in 0..am.count() {
        if am.is_deleted(r) {
            continue;
        }
        let a = match AmAccount::from_table(am, r) {
            Ok(a) => a,
            Err(e) => return Err(e),
        };
        // Non-posting rows never reach a tax package.
        if !a.props.post.is_yes() {
            continue;
        }
        out.push(ExportRow::build(&a, tp, basis, form, entity, year));
    }
    Ok(out)
}

/// Per-code aggregation for the neutral "one row per code" view.
///
/// Some consumers want the *tax codes*, not the accounts; this groups by
/// `TAC_NO` and sums the amounts. Kept separate so the account-level CSV stays
/// lossless.
pub fn aggregate_by_code(rows: &[ExportRow]) -> BTreeMap<String, f64> {
    let mut out: BTreeMap<String, f64> = BTreeMap::new();
    for r in rows {
        let key = if r.tac_no.is_empty() {
            "(unassigned)".to_string()
        } else {
            r.tac_no.clone()
        };
        *out.entry(key).or_insert(0.0) += r.amount;
    }
    for v in out.values_mut() {
        *v = round_half_away(*v, 2);
    }
    out
}

/// Guard: refuse a vendor whose writer is not implemented rather than emit a
/// partial file.
pub fn require_writer(vendor: TaxVendor) -> std::result::Result<VendorWriter, ExportError> {
    vendor.writer().ok_or(ExportError::NoWriter { vendor })
}

/// A resolved tax-code summary for one account, used by the gates.
pub fn code_for(
    tp: &TaxExportTable,
    tac_no: &str,
    form: &str,
    entity: &str,
    year: &str,
) -> Option<TaxExportCode> {
    tp.resolve(tac_no, form, entity, year).cloned()
}

/// Convenience: a `CoreError` carrying an [`ExportError`], so the writer composes
/// with the rest of `cw-app`.
pub fn as_core(e: ExportError) -> CoreError {
    CoreError::Format(e.to_string())
}
