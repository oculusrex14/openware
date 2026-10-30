//! cw-app data import — the column-map model, the ASCII reader and the TB
//! write-back engine (FEAT.009 core / P5.B2+B3).
//!
//! Contract: `docs\specs\import.md` (F49 surface, **F50** column-map storage,
//! **F52a/F52b** the green `RunImport` grammar and the AM/BL diff laws). The COM
//! surface modelled here is `client.Import(package) -> ICWImport` with
//! `ImportPath` / `ASCIIDataFile` / `ASCIILayoutFile` / `ImportComponents` /
//! `SetSetting(i, v)` / `RunImport()`.
//!
//! Four layers, each independently observable:
//!
//! 1. **[`LayoutFile`] / [`ColumnMap`]** — the record-layout artifact. It is a
//!    Windows INI (`GetPrivateProfileA`) that persists a **column map**, and the
//!    shipped preset library is PE resource `RAWDATA` id 101 ([`PresetStore`],
//!    anatomy in [`preset_layout`]). Both round-trip byte-exactly: a parsed
//!    layout re-emits its input bytes, a parsed preset re-encodes to its record,
//!    and a programmatic map emits the minimal cover layout the F52a green run
//!    proved (`[FIXED]` + `dbfields_*`).
//! 2. **[`read_ascii`]** — the byte reader. Fixed layouts bind **1-based
//!    inclusive** ranges (`dbfields_<field>=start,end`; the shipped gate is
//!    `0 < start && start <= end`, then registers `start-1, end-1`); delimited
//!    layouts bind a **1-based column** (`editlist_<field>=col`; gate
//!    `0 < col`). The 0-based form every pre-F52 probe used registers *zero*
//!    fields, which is exactly the silent no-op the spec chases down; this
//!    reader refuses and reports instead. [`read_ascii_report`] is the same scan
//!    without the abort, so one malformed trailer cannot hide the rest of a
//!    file; [`read_grid`] runs the delimited law over an already-split grid,
//!    which is the seam the Excel lane plugs into.
//! 3. **[`preview`]** — the F52 diff plan, computed against the engagement
//!    without touching it: new accounts land in `AM`, and each imported account
//!    with a **non-zero** current-year balance claims a `BL` year-0 `O`/`X` pair.
//! 4. **[`apply`]** — the write. Both tables are staged on clones and swapped
//!    only after every cell write succeeded, so a refused row leaves the
//!    engagement byte-identical (the house atomic-refusal law).
//!
//! ## F52 write-back laws (as implemented)
//!
//! * **AM, new account** — one appended row. `LINENO` = the next multiple of 32
//!    above the live maximum (corp2026's grid: all 324 live deltas are 32; the
//!    `99999999` sentinel hull2024 puts on memo rows is never a base),
//!    `TYPE` from the mapped statement type else [`ImportOptions::default_type`],
//!    `POST='Y'`, `SIGN` from the mapped normal sign else `D`, `AC_DESC` from the
//!    description, `SCHEDULE` from the leadsheet, `TAX_ID`/`UNIT` from their
//!    columns, a fresh family-A `CWGUID`, `CWDATE`/`CREATEDON` stamped.
//!    Everything else stays blank — the corpus leaves `CLASS`, `DC`, `GROUPn`,
//!    `BALSTAT`, … blank on chart rows. Text keys are stored right-justified in
//!    their field width (`AM.AC_NO` is 325/325 right-justified in corp2026) and
//!    amounts use the `N19.2` form, so the new rows are byte-shaped like CW's
//!    own.
//! * **AM, existing account** — untouched, unless `AVOverwriteAccDescription`
//!    is set *and* the imported description differs, in which case `AC_DESC`
//!    (and `CWDATE`) is rewritten. F52a changed no other `AM` cell.
//! * **BL, pair churn** — for `YEAR=0, BUCKET=O, GROUP=A` of the imported
//!    number/entity, the `ID='O'` row takes `AMT` and blanks every fill cell,
//!    and the `ID='X'` row takes `AMT`, `AMT_YTD = AMT`, `FX = AMT`,
//!    `FX_YTD = AMT`, `AMT_DIST = FX_DIST = "0.00"` (cube L2/L3 plus the Repost
//!    normalization). Both stay `TYPE=''`, `PERIOD=' 0'` (right-justified in the
//!    C2 cell), `BOOKED`/`CUSTOM` blank, `CWDATE`-stamped. A missing half of the pair is appended; a complete pair
//!    is rewritten in place, so the live row count is unchanged for an
//!    unchanged number set (F52a: `AM 325->329`, `BL 490<->490 X/O-pair churn`).
//! * **A zero balance claims no row.** Only 163 of corp2026's 325 chart
//!    accounts carry a year-0 `O`/`X` pair, and F52a grew `AM` by four while
//!    `BL` stayed at 490: an imported account with no balance lands in `AM`
//!    only.
//!
//! ## Documented deviations and residuals (spec §5, §8.5)
//!
//! * **Amount-token subgrammar** — the shipped reader mis-scales some tokens
//!    (`125000.00`→`12500000.00`, `111.11`→`11111.00`, `123.4`→`123.00`,
//!    `125000.5`→`124999.75`). Those are recorded defects, not laws, so
//!    [`Cents::parse`] lands exact cents and the divergence is deliberate.
//! * **Prior-year / budget / forecast landing** — only `Current Year Balance`
//!    has an established destination (`BL` year 0). The other fourteen registry kinds
//!    decode and are carried; a file that binds one is warned about.
//! * **Fixed-layout 14-int balance rows** (`dbfields_<Kind>`) are carried
//!    verbatim ([`FixedBalanceRow`]): their slot semantics were never decoded.
//! * **`delm` off-by-one** — the assembled ten-char string is exposed whole
//!    ([`LayoutOptions::delimiter_string`]); [`LayoutOptions::delimiter`] applies
//!    the documented F52a reading (`multiple_specific` ⇒ slot 2).
//! * **`.imp` preset row semantics** — the `(a, b, d)` destination ids live in
//!    `DIALOG` resources that were never decoded, so a [`Preset`] is exposed as
//!    its raw anatomy, never as a [`ColumnMap`].
//! * **GL / AJE components** (`ImportComponents` 1 and 2) belong to the
//!    journals lane; both are refused here rather than half-landed.
//! * **xlsx** — not implemented; [`xlsx_plan`] states the parser plan and
//!    [`read_grid`] is the seam it plugs into.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use cw_core::canon::latin1;
use cw_core::cube::Cube;
use cw_core::dbf::{RawRecord, Table};
use cw_core::guid::{CwGuid, Rng};
use cw_core::{CoreError, Result};

use crate::accounts::{write_cell, Chart};

// ---------------------------------------------------------------------------
// COM surface (F49 §2)
// ---------------------------------------------------------------------------

/// `CWImportPackage` — the `client.Import(package)` argument.
///
/// Probed on the 2026 build: `1..=4` accepted, `0` and `5,6` `E_INVALIDARG`.
/// The refused ordinals are kept as errors so a stored value is reported rather
/// than silently coerced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportPackage {
    /// `1 ipAccountView`.
    AccountView,
    /// `2 ipASCII` — the lane this module implements.
    Ascii,
    /// `3 ipProSysEngagement`.
    ProSysEngagement,
    /// `4 ipXAF`.
    Xaf,
}

impl ImportPackage {
    pub fn parse(raw: i32) -> Result<ImportPackage> {
        Ok(match raw {
            1 => ImportPackage::AccountView,
            2 => ImportPackage::Ascii,
            3 => ImportPackage::ProSysEngagement,
            4 => ImportPackage::Xaf,
            other => {
                return Err(CoreError::Format(format!(
                    "ImportPackage {other} is E_INVALIDARG (legal: 1..=4)"
                )))
            }
        })
    }

    pub fn code(self) -> i32 {
        match self {
            ImportPackage::AccountView => 1,
            ImportPackage::Ascii => 2,
            ImportPackage::ProSysEngagement => 3,
            ImportPackage::Xaf => 4,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ImportPackage::AccountView => "account view",
            ImportPackage::Ascii => "ASCII",
            ImportPackage::ProSysEngagement => "ProSystem engagement",
            ImportPackage::Xaf => "XAF",
        }
    }
}

/// `CWImportComponents` — what the run imports.
///
/// The typelib declares `icTrialBalance=0, icGeneralLedger=1,
/// icAdjustingJournalEntry=2`; whether the member is a bitmask is an open
/// question (spec §5.1), so only the three enumerated values are accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportComponents {
    TrialBalance,
    GeneralLedger,
    AdjustingJournalEntry,
}

impl ImportComponents {
    pub fn parse(raw: i32) -> Result<ImportComponents> {
        Ok(match raw {
            0 => ImportComponents::TrialBalance,
            1 => ImportComponents::GeneralLedger,
            2 => ImportComponents::AdjustingJournalEntry,
            other => {
                return Err(CoreError::Format(format!(
                    "ImportComponents {other} is not 0/1/2 (bitmask semantics unprobed)"
                )))
            }
        })
    }

    pub fn code(self) -> i32 {
        match self {
            ImportComponents::TrialBalance => 0,
            ImportComponents::GeneralLedger => 1,
            ImportComponents::AdjustingJournalEntry => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ImportComponents::TrialBalance => "trial balance",
            ImportComponents::GeneralLedger => "general ledger",
            ImportComponents::AdjustingJournalEntry => "adjusting journal entries",
        }
    }
}

/// `CWImportSetting` — the shared 20-slot settings vector (`0..=19`; `20` is
/// `E_INVALIDARG`). Each package interprets its own bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Setting {
    /// 0 — `AVUsePartnerCode`.
    AvUsePartnerCode,
    /// 1 — `AVPriorYearData`.
    AvPriorYearData,
    /// 2 — `AVUseCostCenterCode`.
    AvUseCostCenterCode,
    /// 3 — `AVNonfinancialData`.
    AvNonfinancialData,
    /// 4 — `AVOverwriteExistingJournals`.
    AvOverwriteExistingJournals,
    /// 5 — `AVOverwriteAccDescription` (the trial-balance description overwrite).
    AvOverwriteAccDescription,
    /// 6 — `ProSysImportClientInfomation` (sic).
    ProSysImportClientInfomation,
    /// 7 — `ProSysImportClientDocuments`.
    ProSysImportClientDocuments,
    /// 8 — `ProSysImportGL`.
    ProSysImportGL,
    /// 9 — `ProSysImportAdjEntries`.
    ProSysImportAdjEntries,
    /// 10 — `ProSysAddDocNo`.
    ProSysAddDocNo,
    /// 11 — `ProSysCreateVirtualRoot`.
    ProSysCreateVirtualRoot,
    /// 12 — `XafUpdateYearEndDate`.
    XafUpdateYearEndDate,
    /// 13 — `XafUpdateClientInformation`.
    XafUpdateClientInformation,
    /// 14 — `XafVersion3OrAbove`.
    XafVersion3OrAbove,
    /// 15 — `XafLeadCodeAsXBRL`.
    XafLeadCodeAsXBRL,
    /// 16 — `XafImportJournalOutsideFiscalYear`.
    XafImportJournalOutsideFiscalYear,
    /// 17 — `AsciiClearExistingAJE`.
    AsciiClearExistingAje,
    /// 18 — `AsciiAccumulateAJE`.
    AsciiAccumulateAje,
    /// 19 — `XafOpeningBalances`.
    XafOpeningBalances,
}

impl Setting {
    /// Every legal slot, in dispid order.
    pub const ALL: [Setting; 20] = [
        Setting::AvUsePartnerCode,
        Setting::AvPriorYearData,
        Setting::AvUseCostCenterCode,
        Setting::AvNonfinancialData,
        Setting::AvOverwriteExistingJournals,
        Setting::AvOverwriteAccDescription,
        Setting::ProSysImportClientInfomation,
        Setting::ProSysImportClientDocuments,
        Setting::ProSysImportGL,
        Setting::ProSysImportAdjEntries,
        Setting::ProSysAddDocNo,
        Setting::ProSysCreateVirtualRoot,
        Setting::XafUpdateYearEndDate,
        Setting::XafUpdateClientInformation,
        Setting::XafVersion3OrAbove,
        Setting::XafLeadCodeAsXBRL,
        Setting::XafImportJournalOutsideFiscalYear,
        Setting::AsciiClearExistingAje,
        Setting::AsciiAccumulateAje,
        Setting::XafOpeningBalances,
    ];

    /// Slot index (`0..=19`).
    pub fn code(self) -> usize {
        Setting::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    /// Parse `SetSetting(i, …)`'s index; `20` and beyond are `E_INVALIDARG`.
    pub fn parse(raw: i32) -> Result<Setting> {
        if (0..20).contains(&raw) {
            Ok(Setting::ALL[raw as usize])
        } else {
            Err(CoreError::Format(format!(
                "CWImportSetting {raw} is E_INVALIDARG (domain is 0..=19)"
            )))
        }
    }

    /// The typelib member name.
    pub fn name(self) -> &'static str {
        match self {
            Setting::AvUsePartnerCode => "AVUsePartnerCode",
            Setting::AvPriorYearData => "AVPriorYearData",
            Setting::AvUseCostCenterCode => "AVUseCostCenterCode",
            Setting::AvNonfinancialData => "AVNonfinancialData",
            Setting::AvOverwriteExistingJournals => "AVOverwriteExistingJournals",
            Setting::AvOverwriteAccDescription => "AVOverwriteAccDescription",
            Setting::ProSysImportClientInfomation => "ProSysImportClientInfomation",
            Setting::ProSysImportClientDocuments => "ProSysImportClientDocuments",
            Setting::ProSysImportGL => "ProSysImportGL",
            Setting::ProSysImportAdjEntries => "ProSysImportAdjEntries",
            Setting::ProSysAddDocNo => "ProSysAddDocNo",
            Setting::ProSysCreateVirtualRoot => "ProSysCreateVirtualRoot",
            Setting::XafUpdateYearEndDate => "XafUpdateYearEndDate",
            Setting::XafUpdateClientInformation => "XafUpdateClientInformation",
            Setting::XafVersion3OrAbove => "XafVersion3OrAbove",
            Setting::XafLeadCodeAsXBRL => "XafLeadCodeAsXBRL",
            Setting::XafImportJournalOutsideFiscalYear => "XafImportJournalOutsideFiscalYear",
            Setting::AsciiClearExistingAje => "AsciiClearExistingAJE",
            Setting::AsciiAccumulateAje => "AsciiAccumulateAJE",
            Setting::XafOpeningBalances => "XafOpeningBalances",
        }
    }
}

/// `Setting(i)` value: a VARIANT bool or string. An unset slot reads `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingValue {
    Bool(bool),
    Text(String),
}

impl SettingValue {
    /// The VARIANT truthiness CW applies to a stored setting.
    pub fn truthy(&self) -> bool {
        match self {
            SettingValue::Bool(b) => *b,
            SettingValue::Text(s) => matches!(
                s.trim().to_ascii_lowercase().as_str(),
                "1" | "y" | "yes" | "true" | "t"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Destination fields (F52 §8.2 enum map)
// ---------------------------------------------------------------------------

/// A destination field a layout column can bind to.
///
/// The enum ids in the doc comments are the `CWAsciiImport*Field` registrations
/// recovered from the reader; they document the decoded grammar and are never
/// written to disk. On disk only the key spelling (`dbfields_<name>`) matters, so
/// spelling drift must never drop a binding: anything unrecognized survives as
/// [`ImportField::Other`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ImportField {
    /// 0 — account division.
    AccountDivision,
    /// 1 — account number.
    AccountNumber,
    /// 2 — split-up.
    SplitUp,
    /// 3 — sub-account.
    SubAccount,
    /// 4 — account description.
    AccountDescription,
    /// 5 — statement type.
    StatementType,
    /// 6 — normal sign.
    NormalSign,
    /// 7 — extended account description.
    ExtAcctDesc,
    /// 8 — mapping number.
    MappingNumber,
    /// 9 — mapping-number description.
    MapNumberDescription,
    /// 0xA — extended mapping-number description.
    ExtMapNoDesc,
    /// 0xB — ratio class.
    RatioClass,
    /// 0xC — leadsheet.
    Leadsheet,
    /// 0xD for group 1, then `0xF + 3*(N-2)`.
    GroupDescription(u8),
    /// 0x2A..=0x35 — flip 1..12.
    Flip(u8),
    /// 0x81 — tax identification.
    TaxIdent,
    /// 0x82 — unit.
    Unit,
    /// 0x84 — GIFI unit.
    GifiUnit,
    /// 0x85 — abbreviated entity (the reader also sets its `+0x818` flag).
    AbbrEntity,
    /// 0x88 — journal source.
    JournalSource,
    /// 0x301..=0x30D — AJE column 1..13.
    AjeColumn(u8),
    /// 0x273 — M3.
    M3,
    /// 0xCD — FX type.
    FxType,
    /// 0xCE — tax export.
    TaxExport,
    /// Any suffix this module has no name for (kept verbatim).
    Other(String),
}

impl ImportField {
    /// The layout-file key suffix, when the field has a canonical spelling.
    /// [`ImportField::Other`] returns `None`; its raw text is the suffix.
    pub fn key_name(&self) -> Option<String> {
        Some(match self {
            ImportField::AccountDivision => "account division".into(),
            ImportField::AccountNumber => "account number".into(),
            ImportField::SplitUp => "split up".into(),
            ImportField::SubAccount => "sub account".into(),
            ImportField::AccountDescription => "account description".into(),
            ImportField::StatementType => "statement type".into(),
            ImportField::NormalSign => "normal sign".into(),
            ImportField::ExtAcctDesc => "ext acct desc".into(),
            ImportField::MappingNumber => "mapping number".into(),
            ImportField::MapNumberDescription => "mapnumberdescription".into(),
            ImportField::ExtMapNoDesc => "ext mapno desc".into(),
            ImportField::RatioClass => "ratio class".into(),
            ImportField::Leadsheet => "leadsheet".into(),
            ImportField::GroupDescription(n) => format!("group{n}description"),
            ImportField::Flip(n) => format!("flip{n}"),
            ImportField::TaxIdent => "tax ident".into(),
            ImportField::Unit => "unit".into(),
            ImportField::GifiUnit => "gifi unit".into(),
            ImportField::AbbrEntity => "abbr entity".into(),
            ImportField::JournalSource => "journal source".into(),
            ImportField::AjeColumn(n) => format!("ajec{n}"),
            ImportField::M3 => "m3".into(),
            ImportField::FxType => "fx type".into(),
            ImportField::TaxExport => "tax export".into(),
            ImportField::Other(_) => return None,
        })
    }

    /// The key suffix this field writes (`Other` keeps its raw text).
    pub fn key_label(&self) -> String {
        match self {
            ImportField::Other(raw) => raw.clone(),
            other => other.key_name().unwrap_or_default(),
        }
    }

    /// Resolve a key suffix. `GetPrivateProfile` is case-insensitive and the
    /// shipped spellings are inconsistent about spaces, so both are folded;
    /// `group<n>description`, `flip<n>` and `ajec<n>` are parameterized.
    pub fn from_key_name(raw: &str) -> ImportField {
        let tight: String = raw
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '_' && *c != '.')
            .collect::<String>()
            .to_ascii_lowercase();
        match tight.as_str() {
            "accountdivision" => ImportField::AccountDivision,
            "accountnumber" | "accountno" => ImportField::AccountNumber,
            "splitup" => ImportField::SplitUp,
            "subaccount" => ImportField::SubAccount,
            "accountdescription" | "accountdesc" => ImportField::AccountDescription,
            "statementtype" => ImportField::StatementType,
            "normalsign" => ImportField::NormalSign,
            "extacctdesc" => ImportField::ExtAcctDesc,
            "mappingnumber" => ImportField::MappingNumber,
            "mapnumberdescription" => ImportField::MapNumberDescription,
            "extmapnodesc" => ImportField::ExtMapNoDesc,
            "ratioclass" => ImportField::RatioClass,
            "leadsheet" => ImportField::Leadsheet,
            "taxident" => ImportField::TaxIdent,
            "unit" => ImportField::Unit,
            "gifiunit" => ImportField::GifiUnit,
            "abzbrentity" | "abbrevientity" | "abbreventity" | "abzbrendity" => {
                ImportField::AbbrEntity
            }
            "journalsource" => ImportField::JournalSource,
            "m3" => ImportField::M3,
            "fxtype" => ImportField::FxType,
            "taxexport" => ImportField::TaxExport,
            other => {
                if let Some(rest) = other.strip_prefix("group") {
                    if let Some(num) = rest.strip_suffix("description") {
                        if let Ok(n) = num.parse::<u8>() {
                            return ImportField::GroupDescription(n);
                        }
                    }
                }
                if let Some(rest) = other.strip_prefix("flip") {
                    if let Ok(n) = rest.parse::<u8>() {
                        return ImportField::Flip(n);
                    }
                }
                if let Some(rest) = other.strip_prefix("ajec") {
                    if let Ok(n) = rest.parse::<u8>() {
                        return ImportField::AjeColumn(n);
                    }
                }
                ImportField::Other(raw.trim().to_string())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Balance kinds (F52 §8.2 registry, 15 entries)
// ---------------------------------------------------------------------------

/// A prior-year / future-year offset (`1..=4`, the `BL.YEAR` window).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum YearOffset {
    Y1,
    Y2,
    Y3,
    Y4,
}

impl YearOffset {
    /// The `BL.YEAR` value.
    pub fn year(self) -> i32 {
        match self {
            YearOffset::Y1 => 1,
            YearOffset::Y2 => 2,
            YearOffset::Y3 => 3,
            YearOffset::Y4 => 4,
        }
    }

    /// Build from `BL.YEAR` (`1..=4`).
    pub fn parse(year: u8) -> Option<YearOffset> {
        Some(match year {
            1 => YearOffset::Y1,
            2 => YearOffset::Y2,
            3 => YearOffset::Y3,
            4 => YearOffset::Y4,
            _ => return None,
        })
    }
}

impl Default for YearOffset {
    fn default() -> Self {
        YearOffset::Y1
    }
}

/// One of the fifteen balance columns a layout can bind.
///
/// Only [`BalanceKind::CurrentYear`] has an established destination (the `BL` year-0
/// `O`/`X` pair, F52b). The other fourteen decode so a layout round-trips, but
/// landing them is a documented residual.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BalanceKind {
    /// Current year balance.
    #[default]
    CurrentYear,
    /// Prior year `1..=4` balance (the `BL.YEAR` offset it would land in).
    Prior(YearOffset),
    /// Current year budget.
    BudgetCurrent,
    /// Prior year `1..=4` budget.
    BudgetPrior(YearOffset),
    /// Current year forecast.
    ForecastCurrent,
    /// Future year `1..=4` forecast.
    Forecast(YearOffset),
}

impl BalanceKind {
    /// The registry in table order.
    pub const ALL: [BalanceKind; 15] = [
        BalanceKind::CurrentYear,
        BalanceKind::Prior(YearOffset::Y1),
        BalanceKind::Prior(YearOffset::Y2),
        BalanceKind::Prior(YearOffset::Y3),
        BalanceKind::Prior(YearOffset::Y4),
        BalanceKind::BudgetCurrent,
        BalanceKind::BudgetPrior(YearOffset::Y1),
        BalanceKind::BudgetPrior(YearOffset::Y2),
        BalanceKind::BudgetPrior(YearOffset::Y3),
        BalanceKind::BudgetPrior(YearOffset::Y4),
        BalanceKind::ForecastCurrent,
        BalanceKind::Forecast(YearOffset::Y1),
        BalanceKind::Forecast(YearOffset::Y2),
        BalanceKind::Forecast(YearOffset::Y3),
        BalanceKind::Forecast(YearOffset::Y4),
    ];

    /// The registry name a layout key is built from (`editlist_<kind> db`).
    pub fn key_name(self) -> &'static str {
        match self {
            BalanceKind::CurrentYear => "Current Year Balance",
            BalanceKind::Prior(YearOffset::Y1) => "Prior Year 1 Balance",
            BalanceKind::Prior(YearOffset::Y2) => "Prior Year 2 Balance",
            BalanceKind::Prior(YearOffset::Y3) => "Prior Year 3 Balance",
            BalanceKind::Prior(YearOffset::Y4) => "Prior Year 4 Balance",
            BalanceKind::BudgetCurrent => "Current Year Budget",
            BalanceKind::BudgetPrior(YearOffset::Y1) => "Prior Year 1 Budget",
            BalanceKind::BudgetPrior(YearOffset::Y2) => "Prior Year 2 Budget",
            BalanceKind::BudgetPrior(YearOffset::Y3) => "Prior Year 3 Budget",
            BalanceKind::BudgetPrior(YearOffset::Y4) => "Prior Year 4 Budget",
            BalanceKind::ForecastCurrent => "Current Year Forecast",
            BalanceKind::Forecast(YearOffset::Y1) => "Future Year 1 Forecast",
            BalanceKind::Forecast(YearOffset::Y2) => "Future Year 2 Forecast",
            BalanceKind::Forecast(YearOffset::Y3) => "Future Year 3 Forecast",
            BalanceKind::Forecast(YearOffset::Y4) => "Future Year 4 Forecast",
        }
    }

    /// Resolve a kind name (case- and space-insensitive, like
    /// `GetPrivateProfile`). Short forms (`cy balance`, `py1 balance`) are
    /// tolerated because both spellings appear in the shipped presets.
    pub fn from_key_name(raw: &str) -> Option<BalanceKind> {
        let tight: String = raw
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '_' && *c != '.')
            .collect::<String>()
            .to_ascii_lowercase();
        for k in BalanceKind::ALL {
            let probe: String = k
                .key_name()
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
                .to_ascii_lowercase();
            if probe == tight {
                return Some(k);
            }
        }
        match tight.as_str() {
            "cybalance" | "currentbalance" | "currentyear" => Some(BalanceKind::CurrentYear),
            "cybudget" | "currentbudget" | "currentyearbudget" => Some(BalanceKind::BudgetCurrent),
            "cyforecast" | "currentforecast" => Some(BalanceKind::ForecastCurrent),
            other => {
                let rest = other.strip_prefix("py")?;
                let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                let year = YearOffset::parse(digits.parse().ok()?)?;
                if rest.ends_with("budget") {
                    Some(BalanceKind::BudgetPrior(year))
                } else if rest.ends_with("forecast") || rest.ends_with("fy") {
                    Some(BalanceKind::Forecast(year))
                } else {
                    Some(BalanceKind::Prior(year))
                }
            }
        }
    }

    /// The `BL.YEAR` offset this kind lands in, when landing is modelled.
    pub fn bl_year(self) -> Option<i32> {
        match self {
            BalanceKind::CurrentYear => Some(0),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Column-map model
// ---------------------------------------------------------------------------

/// How a layout addresses a record's cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutMode {
    /// `f??` — `dbfields_<field>=start,end`, 1-based inclusive.
    Fixed,
    /// `v??` — `editlist_<field>=col`, 1-based column.
    Delimited,
}

impl LayoutMode {
    /// The record-layout family the file-dialog masks imply (spec §4): `v??` =
    /// delimited, `f??` = fixed, suffix = area; `.crd` is the legacy fixed form.
    pub fn from_extension(ext: &str) -> Option<LayoutMode> {
        let e = ext.trim_start_matches('.').to_ascii_lowercase();
        if e == "crd" {
            return Some(LayoutMode::Fixed);
        }
        let mut chars = e.chars();
        let head = chars.next()?;
        let area: String = chars.collect();
        const AREAS: [&str; 14] = [
            "tb", "gl", "aj", "ct", "cc", "pj", "cl", "gr", "rb", "wb", "sl", "wl", "bal", "imp",
        ];
        if !AREAS.contains(&area.as_str()) {
            return None;
        }
        match head {
            'v' => Some(LayoutMode::Delimited),
            'f' => Some(LayoutMode::Fixed),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LayoutMode::Fixed => "fixed-length",
            LayoutMode::Delimited => "delimited",
        }
    }
}

/// A bound source cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    /// `dbfields_<field>=start,end` — **1-based inclusive** byte range (the
    /// shipped parser registers `start-1, end-1`).
    Fixed { start: u32, end: u32 },
    /// `editlist_<field>=col` — **1-based** column index.
    Delimited { column: u32 },
}

impl Column {
    /// The value as the layout file spells it.
    pub fn key_value(&self) -> String {
        match self {
            Column::Fixed { start, end } => format!("{start},{end},0,-1"),
            Column::Delimited { column } => column.to_string(),
        }
    }

    /// Width in bytes for a fixed binding, columns for a delimited one.
    pub fn width(&self) -> u32 {
        match self {
            Column::Fixed { start, end } => end - start + 1,
            Column::Delimited { .. } => 1,
        }
    }
}

/// One destination field bound to a source cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldBinding {
    pub field: ImportField,
    pub column: Column,
}

/// A delimited balance triple (`editlist_<Kind> db|cr|fx`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BalanceBinding {
    pub kind: BalanceKind,
    /// Debit column (1-based).
    pub debit: Option<u32>,
    /// Credit column (1-based).
    pub credit: Option<u32>,
    /// Foreign-amount column (1-based); carried into [`MappedRow::fx`].
    pub fx: Option<u32>,
}

/// A fixed-layout balance row (`dbfields_<Kind>` = up to 14 ints).
///
/// Slot semantics were never decoded (spec §8.5 residual), so the row is carried
/// verbatim instead of guessed at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixedBalanceRow {
    pub kind: BalanceKind,
    /// Missing slots read `-1`, the shipped default.
    pub ints: [i64; 14],
}

impl FixedBalanceRow {
    pub fn key_value(&self) -> String {
        self.ints
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(",")
    }

    fn parse(kind: BalanceKind, raw: &str) -> Result<FixedBalanceRow> {
        let parts: Vec<&str> = raw.split(',').collect();
        if parts.is_empty() || parts.len() > 14 {
            return Err(CoreError::Format(format!(
                "dbfields_{}: expected up to 14 ints, got {raw:?}",
                kind.key_name()
            )));
        }
        let mut ints = [-1i64; 14];
        for (i, p) in parts.iter().enumerate() {
            ints[i] = p.trim().parse::<i64>().map_err(|_| {
                CoreError::Format(format!(
                    "dbfields_{}: slot {} = {p:?} is not an integer",
                    kind.key_name(),
                    i + 1
                ))
            })?;
        }
        Ok(FixedBalanceRow { kind, ints })
    }
}

/// The general options block (§8.3). Keys not modelled here survive in
/// [`LayoutOptions::extra`], so a parsed layout never loses a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutOptions {
    pub skip_header_lines: usize,
    pub skip_footer_lines: usize,
    /// `COMMON multiple_specific` — use one specific delimiter of the ten.
    pub multiple_specific: bool,
    /// `delm.delm_0..9` as ten slots (`'\0'` = unset).
    pub delimiters: [char; 10],
    /// `TextEncapsulator` — the quote character, when declared.
    pub text_encapsulator: Option<char>,
    /// `acctype` — `B`/`I`/`P`/`S` (internal codes 0/1/3/2).
    pub acctype: Option<char>,
    pub clear_periods: bool,
    pub round_balances: bool,
    pub accumulate_balances: bool,
    /// `nozero` — drop rows whose bound balances are all zero.
    pub no_zero: bool,
    pub exclude_totalling_accounts: bool,
    /// `positiveascredit` — flip the landed balance sign.
    pub positive_as_credit: bool,
    /// `balanceascredit` — treat the balance column as credit-positive.
    pub balance_as_credit: bool,
    /// `zb_newascii` (sic — the shipped key).
    pub zb_new_ascii: bool,
    pub use_regional_set: bool,
    pub period_mv: bool,
    pub group_symbol: Option<char>,
    pub decimal_symbol: Option<char>,
    pub version: Option<String>,
    pub standard: Option<String>,
    /// `accdivsub.*` / `accenterity.*` / `profiled` / `advbalsettings*` / …
    pub profile: BTreeMap<String, String>,
    /// `clear{gl,cf,obc,obf,obp,pm}`.
    pub clear: BTreeSet<String>,
    /// Every other key (upper-cased key, trimmed value).
    pub extra: BTreeMap<String, String>,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        LayoutOptions {
            skip_header_lines: 0,
            skip_footer_lines: 0,
            multiple_specific: false,
            delimiters: ['\0'; 10],
            text_encapsulator: None,
            acctype: None,
            clear_periods: false,
            round_balances: false,
            accumulate_balances: false,
            no_zero: false,
            exclude_totalling_accounts: false,
            positive_as_credit: false,
            balance_as_credit: false,
            zb_new_ascii: false,
            use_regional_set: false,
            period_mv: false,
            group_symbol: None,
            decimal_symbol: None,
            version: None,
            standard: None,
            profile: BTreeMap::new(),
            clear: BTreeSet::new(),
            extra: BTreeMap::new(),
        }
    }
}

impl LayoutOptions {
    /// The assembled ten-char `delm` string (`'\0'` for unset slots).
    pub fn delimiter_string(&self) -> String {
        self.delimiters.iter().collect()
    }

    /// The effective field delimiter.
    ///
    /// F52a shape 4 sets `delm_0/1/2 = 44` with `multiple_specific=1`; the
    /// documented reading is "the specific slot", i.e. slot 2. Without
    /// `multiple_specific` the first populated slot is the delimiter. The
    /// off-by-one in the layout reader is a remaining gap (spec §8.5) — the raw
    /// string stays available through [`delimiter_string`](Self::delimiter_string).
    pub fn delimiter(&self) -> Result<char> {
        let slot = if self.multiple_specific { 2 } else { 0 };
        if self.delimiters[slot] != '\0' {
            return Ok(self.delimiters[slot]);
        }
        self.delimiters
            .iter()
            .copied()
            .find(|c| *c != '\0')
            .ok_or_else(|| {
                CoreError::Format(
                    "delimited layout declares no delimiter (delm.delm_N)".into(),
                )
            })
    }

    /// Set one `delm.delm_N` slot (`0..=9`).
    pub fn set_delimiter(&mut self, slot: usize, c: char) -> Result<()> {
        if slot >= 10 {
            return Err(CoreError::Format(format!(
                "delm slot {slot} out of range (0..=9)"
            )));
        }
        self.delimiters[slot] = c;
        Ok(())
    }
}

/// The decoded column map: field bindings, balance bindings and options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColumnMap {
    pub fields: Vec<FieldBinding>,
    pub balances: Vec<BalanceBinding>,
    pub fixed_balances: Vec<FixedBalanceRow>,
    pub options: LayoutOptions,
    /// Explicit mode (from the layout extension); `None` = infer from bindings.
    pub mode: Option<LayoutMode>,
}

impl ColumnMap {
    /// Bind a destination field (a repeat binding replaces the earlier one).
    pub fn bind(&mut self, field: ImportField, column: Column) {
        if let Some(existing) = self.fields.iter_mut().find(|b| b.field == field) {
            existing.column = column;
        } else {
            self.fields.push(FieldBinding { field, column });
        }
    }

    /// The binding for `field`, if any.
    pub fn binding(&self, field: &ImportField) -> Option<&FieldBinding> {
        self.fields.iter().find(|b| &b.field == field)
    }

    /// The resolved mode.
    ///
    /// Both binding families in one map is an error unless the extension picked
    /// one: the shipped reader dispatches on the extension, so guessing would be
    /// a silent misread.
    pub fn resolved_mode(&self) -> Result<LayoutMode> {
        let fixed = self
            .fields
            .iter()
            .any(|b| matches!(b.column, Column::Fixed { .. }));
        let delim = self
            .fields
            .iter()
            .any(|b| matches!(b.column, Column::Delimited { .. }));
        if let Some(m) = self.mode {
            if (m == LayoutMode::Fixed && delim) || (m == LayoutMode::Delimited && fixed) {
                return Err(CoreError::Format(format!(
                    "layout declares the {} family but binds a {} column",
                    m.label(),
                    if m == LayoutMode::Fixed {
                        "editlist_ (delimited)"
                    } else {
                        "dbfields_ (fixed)"
                    }
                )));
            }
            return Ok(m);
        }
        match (fixed, delim) {
            (true, false) => Ok(LayoutMode::Fixed),
            (false, true) => Ok(LayoutMode::Delimited),
            (false, false) => Err(CoreError::Format(
                "layout binds no fields (empty record layout)".into(),
            )),
            (true, true) => Err(CoreError::Format(
                "layout mixes dbfields_ (fixed) and editlist_ (delimited) bindings and \
                 carries no layout extension to dispatch on"
                    .into(),
            )),
        }
    }

    /// Structural validation — the refuse-and-report surface.
    ///
    /// The two shipped gates are reproduced verbatim: fixed bindings need
    /// `0 < start && start <= end`, delimited bindings need `0 < col`. A 0-based
    /// start (the pre-F52 mistake) is therefore a [`IssueKind::ZeroBase`]
    /// refusal rather than a silent no-op.
    pub fn validate(&self) -> Vec<ImportIssue> {
        let mut out = Vec::new();
        let mode = self.resolved_mode();
        if let Err(e) = &mode {
            out.push(ImportIssue {
                severity: Severity::Refuse,
                line: None,
                field: None,
                kind: IssueKind::NoFields,
                detail: e.to_string(),
            });
            return out;
        }
        let mut spans: Vec<(ImportField, u32, u32)> = Vec::new();
        for b in &self.fields {
            match b.column {
                Column::Fixed { start, end } => {
                    if start == 0 {
                        out.push(ImportIssue {
                            severity: Severity::Refuse,
                            line: None,
                            field: Some(b.field.clone()),
                            kind: IssueKind::ZeroBase,
                            detail: format!(
                                "dbfields_{} start is 0; the parser gate is 0 < start \
                                 (starts are 1-based and inclusive)",
                                b.field.key_label()
                            ),
                        });
                        continue;
                    }
                    if start > end {
                        out.push(ImportIssue {
                            severity: Severity::Refuse,
                            line: None,
                            field: Some(b.field.clone()),
                            kind: IssueKind::InvalidRange,
                            detail: format!(
                                "dbfields_{} has start {start} > end {end}",
                                b.field.key_label()
                            ),
                        });
                        continue;
                    }
                    spans.push((b.field.clone(), start, end));
                }
                Column::Delimited { column } => {
                    if column == 0 {
                        out.push(ImportIssue {
                            severity: Severity::Refuse,
                            line: None,
                            field: Some(b.field.clone()),
                            kind: IssueKind::ZeroBase,
                            detail: format!(
                                "editlist_{} column is 0; the parser gate is 0 < col \
                                 (columns are 1-based)",
                                b.field.key_label()
                            ),
                        });
                    }
                }
            }
        }
        if let Ok(mode) = mode {
            if mode == LayoutMode::Fixed && !self.balances.is_empty() {
                out.push(ImportIssue {
                    severity: Severity::Refuse,
                    line: None,
                    field: None,
                    kind: IssueKind::InvalidRange,
                    detail: "a delimited balance triple (editlist_<Kind> db|cr) cannot be \
                             read from a fixed-length layout; fixed layouts carry \
                             dbfields_<Kind> rows instead"
                        .into(),
                });
            }
            if mode == LayoutMode::Delimited && !self.fixed_balances.is_empty() {
                out.push(ImportIssue {
                    severity: Severity::Refuse,
                    line: None,
                    field: None,
                    kind: IssueKind::InvalidRange,
                    detail: "a fixed balance row (dbfields_<Kind>) cannot be read from a \
                             delimited layout; use editlist_<Kind> db|cr"
                        .into(),
                });
            }
        }
        for r in &self.fixed_balances {
            out.push(ImportIssue {
                severity: Severity::Warn,
                line: None,
                field: None,
                kind: IssueKind::UnsupportedBalanceKind,
                detail: format!(
                    "dbfields_{} (14-int fixed balance row) is carried verbatim; its slot \
                     semantics are undecoded (spec §8.5 residual) so it lands nothing",
                    r.kind.key_name()
                ),
            });
        }
        for b in &self.balances {
            if b.debit.is_none() && b.credit.is_none() && b.fx.is_none() {
                out.push(ImportIssue {
                    severity: Severity::Refuse,
                    line: None,
                    field: None,
                    kind: IssueKind::InvalidRange,
                    detail: format!(
                        "{} binds no cell (expected ` db`, ` cr` or ` fx`)",
                        b.kind.key_name()
                    ),
                });
            }
            if b.debit == Some(0) || b.credit == Some(0) || b.fx == Some(0) {
                out.push(ImportIssue {
                    severity: Severity::Refuse,
                    line: None,
                    field: None,
                    kind: IssueKind::ZeroBase,
                    detail: format!("{} binds column 0 (columns are 1-based)", b.kind.key_name()),
                });
            }
            if b.kind.bl_year().is_none() {
                out.push(ImportIssue {
                    severity: Severity::Warn,
                    line: None,
                    field: None,
                    kind: IssueKind::UnsupportedBalanceKind,
                    detail: format!(
                        "{} has no modelled destination (only Current Year Balance \
                         lands; F52b)",
                        b.kind.key_name()
                    ),
                });
            }
        }
        // Overlap is not a CW gate, but two fields reading the same bytes is
        // never a working layout; report the intent bug instead of misreading.
        spans.sort_by_key(|(_, s, _)| *s);
        for pair in spans.windows(2) {
            let (f0, s0, e0) = (&pair[0].0, pair[0].1, pair[0].2);
            let (f1, s1) = (&pair[1].0, pair[1].1);
            if s1 <= e0 {
                out.push(ImportIssue {
                    severity: Severity::Refuse,
                    line: None,
                    field: Some(f1.clone()),
                    kind: IssueKind::Overlapping,
                    detail: format!(
                        "{} ({s0}..={e0}) overlaps {} (starts {s1})",
                        f0.key_label(),
                        f1.key_label()
                    ),
                });
            }
        }
        if self.binding(&ImportField::AccountNumber).is_none() {
            out.push(ImportIssue {
                severity: Severity::Refuse,
                line: None,
                field: Some(ImportField::AccountNumber),
                kind: IssueKind::NoAccountNumber,
                detail: "layout binds no account number; every imported row would be \
                         unattributable"
                    .into(),
            });
        }
        if !self.fields.is_empty()
            && self
                .fields
                .iter()
                .all(|b| matches!(b.field, ImportField::Other(_)))
        {
            out.push(ImportIssue {
                severity: Severity::Warn,
                line: None,
                field: None,
                kind: IssueKind::UnknownFieldsOnly,
                detail: "every bound field name is unrecognized; nothing will be typed".into(),
            });
        }
        out
    }

    /// The refuse-worthy subset of [`validate`](Self::validate).
    pub fn refusals(&self) -> Vec<ImportIssue> {
        self.validate()
            .into_iter()
            .filter(|i| i.severity == Severity::Refuse)
            .collect()
    }

    /// The minimal cover layout the F52a green run proved.
    ///
    /// Fixed: `[FIXED]` + `dbfields_<field>=<start>,<end>,0,-1`. Delimited:
    /// `[COMMON]` / `[DELM]` + `editlist_<field>=<col>` + the `db/cr/fx` triples +
    /// `delm.delm_N`.
    pub fn to_ini(&self) -> Result<String> {
        let mode = self.resolved_mode()?;
        let mut out = String::new();
        match mode {
            LayoutMode::Fixed => {
                out.push_str("[FIXED]\r\n");
                for b in &self.fields {
                    let (start, end) = match b.column {
                        Column::Fixed { start, end } => (start, end),
                        Column::Delimited { column } => (column, column),
                    };
                    out.push_str(&format!(
                        "dbfields_{}={start},{end},0,-1\r\n",
                        b.field.key_label()
                    ));
                }
                for r in &self.fixed_balances {
                    out.push_str(&format!(
                        "dbfields_{}={}\r\n",
                        r.kind.key_name(),
                        r.key_value()
                    ));
                }
            }
            LayoutMode::Delimited => {
                out.push_str("[COMMON]\r\n");
                if self.options.multiple_specific {
                    out.push_str("multiple_specific=1\r\n");
                }
                if self.options.skip_header_lines > 0 {
                    out.push_str(&format!(
                        "skipheaderlines={}\r\n",
                        self.options.skip_header_lines
                    ));
                }
                if let Some(q) = self.options.text_encapsulator {
                    out.push_str(&format!("TextEncapsulator={}\r\n", q as u32));
                }
                out.push_str("\r\n[DELM]\r\n");
                for b in &self.fields {
                    let column = match b.column {
                        Column::Delimited { column } => column,
                        Column::Fixed { start, .. } => start,
                    };
                    out.push_str(&format!(
                        "editlist_{}={column}\r\n",
                        b.field.key_label()
                    ));
                }
                for b in &self.balances {
                    for (slot, col) in [("db", b.debit), ("cr", b.credit), ("fx", b.fx)] {
                        if let Some(c) = col {
                            out.push_str(&format!(
                                "editlist_{} {slot}={c}\r\n",
                                b.kind.key_name()
                            ));
                        }
                    }
                }
                for (slot, c) in self.options.delimiters.iter().enumerate() {
                    if *c != '\0' {
                        out.push_str(&format!("delm.delm_{slot}={}\r\n", *c as u32));
                    }
                }
            }
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// INI container (byte-stable)
// ---------------------------------------------------------------------------

/// A parsed layout file: the raw document plus the decoded map.
///
/// The document is kept line-for-line with each line's own terminator, so
/// `parse(text)?.emit() == text` for every input — the same no-op-save law the
/// kernel applies to tables. A mutation rewrites the value span only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutFile {
    lines: Vec<IniLine>,
    map: ColumnMap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IniLine {
    text: String,
    eol: String,
}

impl LayoutFile {
    /// Parse a layout file. `mode` normally comes from the extension
    /// ([`LayoutMode::from_extension`]); pass `None` to infer from the bindings.
    pub fn parse(text: &str, mode: Option<LayoutMode>) -> Result<LayoutFile> {
        let mut lines = Vec::new();
        let mut rest = text;
        while !rest.is_empty() {
            let (line, used, eol) = match rest.find('\n') {
                Some(i) => {
                    let raw = &rest[..i];
                    match raw.strip_suffix('\r') {
                        Some(body) => (body.to_string(), i + 1, "\r\n"),
                        None => (raw.to_string(), i + 1, "\n"),
                    }
                }
                None => (rest.to_string(), rest.len(), ""),
            };
            lines.push(IniLine { text: line, eol: eol.to_string() });
            rest = &rest[used..];
        }
        let mut map = ColumnMap { mode, ..Default::default() };
        decode_into(&collect_entries(&lines), &mut map)?;
        Ok(LayoutFile { lines, map })
    }

    /// Parse from disk; the extension selects the mode.
    pub fn parse_path(path: impl AsRef<Path>) -> Result<LayoutFile> {
        let path = path.as_ref();
        let mode = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(LayoutMode::from_extension);
        let bytes = std::fs::read(path)?;
        LayoutFile::parse(&latin1(&bytes), mode)
    }

    /// The decoded map.
    pub fn map(&self) -> &ColumnMap {
        &self.map
    }

    /// Mutable access to the decoded map.
    ///
    /// Editing the map does *not* touch the document: call
    /// [`rebuild`](Self::rebuild) to project it onto the file, so a caller can
    /// stage several changes and commit them in one minimal cover layout.
    pub fn map_mut(&mut self) -> &mut ColumnMap {
        &mut self.map
    }

    /// The document exactly as parsed (the no-op save).
    pub fn emit(&self) -> String {
        let mut out = String::new();
        for l in &self.lines {
            out.push_str(&l.text);
            out.push_str(&l.eol);
        }
        out
    }

    /// The text this file stores to disk.
    pub fn save_text(&self) -> String {
        self.emit()
    }

    /// Rebuild the document from the map (the "Save Layout Settings" surface).
    pub fn rebuild(&mut self) -> Result<()> {
        let text = self.map.to_ini()?;
        let parsed = LayoutFile::parse(&text, self.map.mode)?;
        self.lines = parsed.lines;
        self.map = parsed.map;
        Ok(())
    }

    /// Set `key` in `section` (case-insensitive), rewriting only the value span.
    /// A missing key is appended to that section (or the file, with a fresh
    /// section header).
    pub fn set(&mut self, section: &str, key: &str, value: &str) -> Result<()> {
        let target = section.trim().to_ascii_uppercase();
        let mut current = String::new();
        let mut insert_at: Option<usize> = None;
        for (i, l) in self.lines.iter().enumerate() {
            if let Some(sec) = section_of(&l.text) {
                current = sec.to_ascii_uppercase();
                if current == target {
                    insert_at = Some(i + 1);
                }
                continue;
            }
            if current != target {
                continue;
            }
            if let Some((k, span)) = key_of(&l.text) {
                if k.trim().eq_ignore_ascii_case(key) {
                    let line = &mut self.lines[i];
                    let mut new = String::with_capacity(line.text.len() + value.len());
                    new.push_str(&line.text[..span.0]);
                    new.push_str(value);
                    new.push_str(&line.text[span.1..]);
                    line.text = new;
                    self.resync();
                    return Ok(());
                }
            }
        }
        let eol = self
            .lines
            .iter()
            .find(|l| !l.eol.is_empty())
            .map(|l| l.eol.clone())
            .unwrap_or_default();
        let new = IniLine { text: format!("{key}={value}"), eol: eol.clone() };
        match insert_at {
            Some(i) => self.lines.insert(i, new),
            None => {
                if !target.is_empty() {
                    self.lines.push(IniLine {
                        text: format!("[{section}]"),
                        eol: eol.clone(),
                    });
                }
                self.lines.push(new);
            }
        }
        self.resync();
        Ok(())
    }

    /// Re-decode the map from the current document text.
    fn resync(&mut self) {
        let mut map = ColumnMap { mode: self.map.mode, ..Default::default() };
        if decode_into(&collect_entries(&self.lines), &mut map).is_ok() {
            self.map = map;
        }
    }
}

/// `[NAME]` → `NAME`.
fn section_of(text: &str) -> Option<&str> {
    let t = text.trim();
    if t.len() >= 2 && t.starts_with('[') && t.ends_with(']') {
        Some(t[1..t.len() - 1].trim())
    } else {
        None
    }
}

/// `key=value` → `(key, value span)`; comments, blanks and section headers are
/// not keys.
fn key_of(text: &str) -> Option<(&str, (usize, usize))> {
    let t = text.trim_start();
    if t.is_empty() || t.starts_with(';') || t.starts_with('#') || t.starts_with('[') {
        return None;
    }
    let eq = text.find('=')?;
    let after = &text[eq + 1..];
    let lead = after.len() - after.trim_start().len();
    let vstart = eq + 1 + lead;
    let vend = vstart + after.trim().len();
    Some((&text[..eq], (vstart, vend)))
}

/// One `key=value` occurrence, with the section it sits in.
#[derive(Clone, Debug)]
struct Entry {
    section: String,
    key: String,
    value: String,
}

fn collect_entries(lines: &[IniLine]) -> Vec<Entry> {
    let mut current = String::new();
    let mut out = Vec::new();
    for l in lines {
        if let Some(sec) = section_of(&l.text) {
            current = sec.to_string();
            continue;
        }
        if let Some((k, span)) = key_of(&l.text) {
            out.push(Entry {
                section: current.clone(),
                key: k.trim().to_string(),
                value: l.text[span.0..span.1].to_string(),
            });
        }
    }
    out
}

/// Sections a per-call reader may have been looking at (spec §8.1): the dispatch
/// section, the delimiter section, and the two cover spellings.
const COVER_SECTIONS: [&str; 6] = [
    "FIXED", "DELM", "COMMON", "COMMON:DELM", "COMMONDELM", "COMMON.DELM",
];

enum FieldFamily {
    Fixed,
    Delimited,
}

/// Fold the entries into the typed map.
///
/// Cover merge law: keys are collected from every cover section (plus
/// unsectioned keys) in file order and the **last** occurrence wins, so a later
/// cover section refines an earlier one. `dbfields_`/`editlist_` select the
/// family; the suffix is either a destination field or a balance kind followed
/// by a space-delimited `db`/`cr`/`fx` selector.
fn decode_into(entries: &[Entry], map: &mut ColumnMap) -> Result<()> {
    let cover = |s: &str| {
        let up = s.trim().to_ascii_uppercase();
        up.is_empty() || COVER_SECTIONS.contains(&up.as_str())
    };
    for e in entries {
        if !cover(&e.section) {
            continue;
        }
        let key = e.key.trim();
        let lower = key.to_ascii_lowercase();
        if let Some((family, suffix)) = split_field_key(key) {
            match family {
                FieldFamily::Fixed => match parse_fixed_binding(suffix, &e.value)? {
                    Some(col) => map.bind(ImportField::from_key_name(suffix), col),
                    None => {
                        let kind = BalanceKind::from_key_name(suffix).ok_or_else(|| {
                            CoreError::Format(format!(
                                "dbfields_{suffix}: neither a field nor a balance kind"
                            ))
                        })?;
                        let row = FixedBalanceRow::parse(kind, &e.value)?;
                        map.fixed_balances.retain(|r| r.kind != row.kind);
                        map.fixed_balances.push(row);
                    }
                },
                FieldFamily::Delimited => {
                    if let Some((kind_name, slot)) = split_balance_suffix(suffix) {
                        let kind = BalanceKind::from_key_name(kind_name).ok_or_else(|| {
                            CoreError::Format(format!(
                                "editlist_{suffix}: {kind_name:?} is not a balance kind"
                            ))
                        })?;
                        let col: u32 = e.value.trim().parse().map_err(|_| {
                            CoreError::Format(format!(
                                "editlist_{suffix}: {:?} is not a column index",
                                e.value
                            ))
                        })?;
                        let at = match map.balances.iter().position(|b| b.kind == kind) {
                            Some(i) => i,
                            None => {
                                map.balances
                                    .push(BalanceBinding { kind, ..Default::default() });
                                map.balances.len() - 1
                            }
                        };
                        match slot {
                            BalanceSlot::Debit => map.balances[at].debit = Some(col),
                            BalanceSlot::Credit => map.balances[at].credit = Some(col),
                            BalanceSlot::Fx => map.balances[at].fx = Some(col),
                        }
                        continue;
                    }
                    let col: u32 = e.value.trim().parse().map_err(|_| {
                        CoreError::Format(format!(
                            "editlist_{suffix}: {:?} is not a column index",
                            e.value
                        ))
                    })?;
                    map.bind(ImportField::from_key_name(suffix), Column::Delimited { column: col });
                }
            }
            continue;
        }
        if let Some(rest) = lower.strip_prefix("delm.delm_") {
            let slot: usize = rest
                .trim()
                .parse()
                .map_err(|_| CoreError::Format(format!("{key}: expected delm.delm_0..9")))?;
            let code: u32 = e.value.trim().parse().map_err(|_| {
                CoreError::Format(format!("{key}: {:?} is not a character code", e.value))
            })?;
            let c = char::from_u32(code)
                .ok_or_else(|| CoreError::Format(format!("{key}: invalid char code {code}")))?;
            map.options.set_delimiter(slot, c)?;
            continue;
        }
        apply_option(map, &lower, &e.value)?;
    }
    if map.mode.is_none() && !map.fields.is_empty() {
        map.mode = Some(
            if map
                .fields
                .iter()
                .any(|b| matches!(b.column, Column::Fixed { .. }))
            {
                LayoutMode::Fixed
            } else {
                LayoutMode::Delimited
            },
        );
    }
    Ok(())
}

fn apply_option(map: &mut ColumnMap, lower: &str, value: &str) -> Result<()> {
    fn flag(v: &str) -> bool {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "y" | "yes" | "true" | "t"
        )
    }
    fn int(key: &str, raw: &str) -> Result<i64> {
        raw.trim()
            .parse::<i64>()
            .map_err(|_| CoreError::Format(format!("{key}={raw:?} is not an integer")))
    }
    let o = &mut map.options;
    match lower {
        "skipheaderlines" => o.skip_header_lines = int(lower, value)?.max(0) as usize,
        "skipfooterlines" => o.skip_footer_lines = int(lower, value)?.max(0) as usize,
        "multiple_specific" => o.multiple_specific = flag(value),
        "textencapsulator" => {
            o.text_encapsulator = value.trim().chars().next().filter(|c| !c.is_whitespace())
        }
        "acctype" => o.acctype = value.trim().chars().next().map(|c| c.to_ascii_uppercase()),
        "clearperiods" => o.clear_periods = flag(value),
        "roundbalances" => o.round_balances = flag(value),
        "accubals" => o.accumulate_balances = flag(value),
        "nozero" => o.no_zero = flag(value),
        "excludetotallingaccounts" => o.exclude_totalling_accounts = flag(value),
        "positiveascredit" => o.positive_as_credit = flag(value),
        "balanceascredit" => o.balance_as_credit = flag(value),
        "zb_newascii" => o.zb_new_ascii = flag(value),
        "useregionalset" => o.use_regional_set = flag(value),
        "periodmv" => o.period_mv = flag(value),
        "grpsymbol" => o.group_symbol = value.trim().chars().next(),
        "decsymbol" => o.decimal_symbol = value.trim().chars().next(),
        "version" => o.version = Some(value.trim().to_string()),
        "standard" => o.standard = Some(value.trim().to_string()),
        other => {
            if let Some(rest) = other.strip_prefix("clear") {
                if matches!(rest, "gl" | "cf" | "obc" | "obf" | "obp" | "pm") {
                    o.clear.insert(rest.to_string());
                    return Ok(());
                }
            }
            if other.starts_with("accdivsub.")
                || other.starts_with("accenterity.")
                || other == "profiled"
                || other.starts_with("exchangerateperioddatesequence")
                || other.starts_with("advbalsettings")
                || other.starts_with("field_break")
                || other.starts_with("delmperiod")
                || other.starts_with("fixedperiod")
                || other.starts_with("zb_")
                || other.starts_with("custom_")
                || other.starts_with("aje_")
                || other.ends_with("ascredit")
            {
                o.profile.insert(other.to_string(), value.trim().to_string());
                return Ok(());
            }
            // Anything else is carried, never dropped.
            o.extra
                .insert(other.to_string(), value.trim().to_string());
        }
    }
    Ok(())
}

fn split_field_key(key: &str) -> Option<(FieldFamily, &str)> {
    const FIXED: &str = "dbfields_";
    const DELIM: &str = "editlist_";
    let lower = key.to_ascii_lowercase();
    if lower.starts_with(FIXED) {
        return Some((FieldFamily::Fixed, &key[FIXED.len()..]));
    }
    if lower.starts_with(DELIM) {
        return Some((FieldFamily::Delimited, &key[DELIM.len()..]));
    }
    None
}

/// Parse `dbfields_<field>=a,b,c,d`.
///
/// Returns `None` when the row is the multi-int balance form instead (a
/// balance-kind suffix, or more than four slots).
fn parse_fixed_binding(suffix: &str, raw: &str) -> Result<Option<Column>> {
    if BalanceKind::from_key_name(suffix).is_some() {
        return Ok(None);
    }
    let parts: Vec<&str> = raw.split(',').collect();
    if parts.len() > 4 {
        return Ok(None);
    }
    if parts.len() < 2 {
        return Err(CoreError::Format(format!(
            "dbfields_{suffix}: expected start,end (1-based inclusive), got {raw:?}"
        )));
    }
    let start: u32 = parts[0]
        .trim()
        .parse()
        .map_err(|_| CoreError::Format(format!("dbfields_{suffix}: start {:?} is not an integer", parts[0])))?;
    let end: u32 = parts[1]
        .trim()
        .parse()
        .map_err(|_| CoreError::Format(format!("dbfields_{suffix}: end {:?} is not an integer", parts[1])))?;
    Ok(Some(Column::Fixed { start, end }))
}

enum BalanceSlot {
    Debit,
    Credit,
    Fx,
}

/// `"Current Year Balance db"` → `("Current Year Balance", Debit)`.
fn split_balance_suffix(suffix: &str) -> Option<(&str, BalanceSlot)> {
    let lower = suffix.to_ascii_lowercase();
    for (tail, slot) in [
        (" db", BalanceSlot::Debit),
        (" cr", BalanceSlot::Credit),
        (" fx", BalanceSlot::Fx),
    ] {
        if lower.ends_with(tail) {
            let cut = suffix.len() - tail.len();
            debug_assert!(suffix.is_char_boundary(cut));
            return Some((&suffix[..cut], slot));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// RAWDATA id-101 preset store (F50 §4b.2)
// ---------------------------------------------------------------------------

/// Byte offsets of the shipped preset record anatomy (`RAWDATA` id 101).
pub mod preset_layout {
    /// `name[24]`.
    pub const NAME: usize = 0x000;
    /// `product[24]`.
    pub const PRODUCT: usize = 0x018;
    /// `desc[32]`.
    pub const DESC: usize = 0x030;
    /// Zero pad up to the float64.
    pub const PAD: usize = 0x050;
    /// Family default `f64` (`9.0` Dynacom, `1.0` ACV, `6.0` QB, `8.0` SIM,
    /// `6.1` GP, `0` for a TB layout).
    pub const DEFAULT: usize = 0x148;
    /// Three `u32` header slots (`0x150`, `0x154`, `0x158`).
    pub const HEADER: usize = 0x150;
    /// `u32` row count (`0x15C`, the fourth header dword).
    pub const COUNT: usize = 0x15C;
    /// First of `count` 24-byte rows.
    pub const ROWS: usize = 0x160;
    /// Bytes per row: `(a, 0, b, c, d, 0)`.
    pub const ROW_LEN: usize = 24;
    /// Bytes of a zero-row record.
    pub const PROLOGUE: usize = 0x160;
}

/// One 24-byte preset row: `(a, 0, b, c, d, 0)`.
///
/// `c` is the source offset — ascending by 5/10 in a fixed-length layout
/// (Dynacom GL: 5, 15, 25, …), `0` throughout a delimited one (TBPLUSCH). `b` is
/// the small destination/role enum (1, 2, 3, 4, 7, 10, 28, 52, 53, …). `a`/`d`
/// are flags/parameters. Their meaning lives in map-dialog `DIALOG` resources
/// that were never decoded, hence the raw anatomy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct PresetRow {
    pub a: u32,
    pub b: u32,
    /// Source offset (ascending in fixed-length layouts, 0 in delimited ones).
    pub c: u32,
    pub d: u32,
}

impl PresetRow {
    /// Decode 24 bytes; slots 2 and 6 must be zero, per the anatomy.
    pub fn parse(raw: &[u8]) -> PresetRow {
        let u = |o: usize| u32::from_le_bytes([raw[o], raw[o + 1], raw[o + 2], raw[o + 3]]);
        PresetRow { a: u(0), b: u(8), c: u(12), d: u(16) }
    }

    pub fn encode(&self) -> [u8; preset_layout::ROW_LEN] {
        let mut out = [0u8; preset_layout::ROW_LEN];
        out[0..4].copy_from_slice(&self.a.to_le_bytes());
        out[8..12].copy_from_slice(&self.b.to_le_bytes());
        out[12..16].copy_from_slice(&self.c.to_le_bytes());
        out[16..20].copy_from_slice(&self.d.to_le_bytes());
        out
    }
}

/// One `<PRODUCT>.IMP` preset record.
///
/// Text cells are held byte-for-byte (NUL padding included) so a parse/encode
/// cycle is exact. `header` keeps three raw dwords: five header variants ship
/// (PEGOP\*\BUD/FP/GL, VANTAGE), so nothing here is validated.
#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    /// `name[24]`.
    pub name: String,
    /// `product[24]`.
    pub product: String,
    /// `desc[32]`.
    pub description: String,
    /// The `0x50..0x148` pad, carried verbatim.
    pub pad: Vec<u8>,
    pub default: f64,
    pub header: [u32; 3],
    pub rows: Vec<PresetRow>,
}

impl Preset {
    /// Parse one record at `offset`; returns the record and the next offset.
    pub fn parse_at(bytes: &[u8], offset: usize) -> Result<(Preset, usize)> {
        use preset_layout::*;
        if bytes.len() < offset + PROLOGUE {
            return Err(CoreError::Format(format!(
                "preset at {offset:#x}: truncated prologue ({} bytes left)",
                bytes.len().saturating_sub(offset)
            )));
        }
        let rec = &bytes[offset..];
        let text = |o: usize, n: usize| latin1(&rec[o..o + n]);
        let u = |o: usize| u32::from_le_bytes([rec[o], rec[o + 1], rec[o + 2], rec[o + 3]]);
        let default = f64::from_le_bytes([
            rec[DEFAULT],
            rec[DEFAULT + 1],
            rec[DEFAULT + 2],
            rec[DEFAULT + 3],
            rec[DEFAULT + 4],
            rec[DEFAULT + 5],
            rec[DEFAULT + 6],
            rec[DEFAULT + 7],
        ]);
        let count = u(COUNT) as usize;
        let end = count
            .checked_mul(ROW_LEN)
            .and_then(|n| n.checked_add(PROLOGUE))
            .ok_or_else(|| CoreError::Format("preset row count overflows".into()))?;
        if rec.len() < end {
            return Err(CoreError::Format(format!(
                "preset at {offset:#x}: {count} rows need {end:#x} bytes, {} available",
                rec.len()
            )));
        }
        let rows = (0..count)
            .map(|i| {
                let at = ROWS + i * ROW_LEN;
                PresetRow::parse(&rec[at..at + ROW_LEN])
            })
            .collect();
        Ok((
            Preset {
                name: text(NAME, 24),
                product: text(PRODUCT, 24),
                description: text(DESC, 32),
                pad: rec[PAD..DEFAULT].to_vec(),
                default,
                header: [u(HEADER), u(HEADER + 4), u(HEADER + 8)],
                rows,
            },
            offset + end,
        ))
    }

    /// The product name without NUL padding.
    pub fn product_name(&self) -> &str {
        self.product.trim_end_matches('\0').trim()
    }

    /// The layout name without NUL padding.
    pub fn layout_name(&self) -> &str {
        self.name.trim_end_matches('\0').trim()
    }

    /// Encode the full record (prologue + rows) — byte-identical to anything
    /// [`parse_at`](Self::parse_at) accepted.
    pub fn encode(&self) -> Vec<u8> {
        use preset_layout::*;
        let mut out = vec![0u8; PAD];
        out.resize(DEFAULT, 0);
        put_text(&mut out, NAME, 24, &self.name);
        put_text(&mut out, PRODUCT, 24, &self.product);
        put_text(&mut out, DESC, 32, &self.description);
        let pad = self.pad.len().min(DEFAULT - PAD);
        out[PAD..PAD + pad].copy_from_slice(&self.pad[..pad]);
        out.extend_from_slice(&self.default.to_le_bytes());
        for h in self.header {
            out.extend_from_slice(&h.to_le_bytes());
        }
        out.extend_from_slice(&(self.rows.len() as u32).to_le_bytes());
        debug_assert_eq!(out.len(), ROWS);
        for r in &self.rows {
            out.extend_from_slice(&r.encode());
        }
        out
    }

    /// The record **body** the layout writer emits (`hdr` + `count` + rows),
    /// without the product prologue (spec §4b.3).
    pub fn encode_body(&self) -> Vec<u8> {
        let full = self.encode();
        full[preset_layout::HEADER..].to_vec()
    }
}

fn put_text(out: &mut [u8], at: usize, len: usize, s: &str) {
    for (i, b) in s.bytes().take(len).enumerate() {
        out[at + i] = b;
    }
}

/// The shipped preset library (PE resource `RAWDATA` id 101: 470 records, 332
/// unique layouts).
///
/// The resource container is not part of the corpus, so this parses a bare
/// record stream; a PE resource wrapper would be a byte prefix
/// (`&bytes[header..]`) away.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PresetStore {
    pub presets: Vec<Preset>,
}

impl PresetStore {
    pub fn parse(bytes: &[u8]) -> Result<PresetStore> {
        let mut presets = Vec::new();
        let mut at = 0usize;
        while at < bytes.len() {
            let (p, next) = Preset::parse_at(bytes, at)?;
            presets.push(p);
            at = next;
        }
        Ok(PresetStore { presets })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for p in &self.presets {
            out.extend_from_slice(&p.encode());
        }
        out
    }

    /// Distinct `<PRODUCT>.IMP` names — the 332 of F50's 470 records.
    pub fn unique_layouts(&self) -> usize {
        self.products().len()
    }

    /// Every distinct product name, sorted.
    pub fn products(&self) -> BTreeSet<String> {
        self.presets
            .iter()
            .map(|p| p.product_name().to_ascii_uppercase())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// Look a preset up by product **or** layout name (case-insensitive, `.IMP`
    /// optional). Several records can share a product (the 470/332 ratio), so the
    /// first match wins and [`PresetStore::all_for`] returns the rest.
    pub fn find(&self, name: &str) -> Option<&Preset> {
        self.all_for(name).into_iter().next()
    }

    /// Every preset whose product or layout name matches.
    pub fn all_for(&self, name: &str) -> Vec<&Preset> {
        let probe = name.trim().to_ascii_uppercase();
        let probe = probe.strip_suffix(".IMP").unwrap_or(&probe).to_string();
        let key = |s: &str| {
            let up = s.trim().to_ascii_uppercase();
            up.strip_suffix(".IMP").unwrap_or(&up).to_string()
        };
        self.presets
            .iter()
            .filter(|p| key(p.product_name()) == probe || key(p.layout_name()) == probe)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Money
// ---------------------------------------------------------------------------

/// Exact cents — the only money type the import lane writes.
///
/// ## The vendor amount tokenizer — exact law
///
/// The amount scan is **not** in `x64CWAscii` and **not** in `x64cw42`: it lives
/// in the vendor's import path, reached as `importASCII`'s conversion routine.
/// Its behavior, characterized exactly:
///
/// First the whole token is classified: a character is a *candidate* when
/// `c - 0x2B < 0x3B` (i.e. `0x2B..0x65`) **and** the bit
/// `0x400000004000009 >> (c - 0x2B) & 1` is set. That mask has bits
/// `{0, 3, 26, 58}` → exactly `'+'` (`0x2B`), `'.'` (`0x2E`), `'E'` (`0x45`) and
/// `'e'` (`0x65`) — both exponent spellings are candidates. `'-'` is a candidate
/// only at index 0, and only when it leads the token. There is **no**
/// thousands separator, and note that this classifier only decides
/// "is this a number at all"; the *lane* (integer vs `%lf`) is decided separately
/// by the digit loop below, so `E`/`e`/`+`/`.` all end up in the double lane.
///
/// The scan itself, as a single clean loop:
///
/// ```text
/// first = s[0]; p = (first == '-') ? s + 1 : s;   /* '-' only leads */
/// gate  = (first == '-') ? 0x0CCCCCCCCCCCCCCC     /* u64::MAX/10, neg */
///                        : 0x1999999999999999;    /* u64::MAX/10, pos */
/// acc = 0;
/// loop {
///     if p == end {
///         if first == '-' { acc = -acc; }
///         return Integer(acc);
///     }
///     c = *p; p += 1;
///     if !c.is_ascii_digit() { return Double(lf_parse(s)); }  /* "%lf" */
///     if acc > gate           { return Double(lf_parse(s)); }  /* overflow */
///     acc = acc * 10 + (c - '0');
/// }
/// ```
///
/// The double lane is the C `sscanf(buf, "%lf")` converter, verbatim.
///
/// So the grammar is exactly two lanes:
///
/// | token | lane | arithmetic |
/// |---|---|---|
/// | `[-]digits` (no other char) | integer | `acc = acc*10 + digit`, i64, negate at the end |
/// | `digits` overflowing the gate | double | `sscanf("%lf")` |
/// | anything with `.`/`e`/`+`/inner `-` | double | `sscanf("%lf")` |
///
/// **There is no `×100`, no rounding and no fraction drop in the tokenizer.**
/// The `×100` / one-decimal fraction drop / `−0.75` seen in vendor output are
/// produced *downstream*, when the scanner's `i64` or `double` is written into a
/// fixed-decimal DBF cell — not by this scan. That is a **defect of the store
/// step, not of the grammar**, and it is why [`Cents::parse`] stays exact: it
/// implements the intended law (the integer lane, extended to explicit
/// decimals) rather than reproducing a storage bug.
///
/// The one behavioural consequence worth keeping: CW's double lane routes any
/// token containing `.` through binary floating point, so `111.11` is not
/// exactly 111.11. [`CwAmountGrammar`] models the *grammar* (which lane a token
/// takes) so the divergence is testable, while [`Cents::parse`] keeps producing
/// exact cents.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cents(pub i64);

/// Which lane the vendor's amount scanner puts a token in. See [`Cents`] for
/// the full law.
///
/// This is a **model of the vendor's branch**, not of its storage step: it
/// answers "would CW parse this as an exact integer or through `sscanf("%lf")`",
/// which is what makes the `111.11` divergence explainable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CwAmountLane {
    /// Digit-only (optionally leading `-`), accumulated as an exact i64.
    Integer,
    /// Contains `.`/`e`/`+`/inner `-`, or overflows the i64 gate: goes through
    /// `sscanf("%lf")` and therefore binary floating point.
    Double,
    /// Empty token — CW's scanner returns without storing.
    Empty,
}

/// The vendor scanner's accepted character class, derived from its classifier.
pub struct CwAmountGrammar;

impl CwAmountGrammar {
    /// Positive-lane overflow gate: `u64::MAX / 10` (`0x1999999999999999`).
    pub const GATE_POSITIVE: i64 = 0x1999_9999_9999_9999;
    /// Negative-lane overflow gate (`0x0CCCCCCCCCCCCCCC`).
    pub const GATE_NEGATIVE: i64 = 0x0CCC_CCCC_CCCC_CCCC;
    /// The classifier bitmask: bits {0, 3, 26, 58} ↔ `'+'`, `'.'`, `'E'`, `'e'`.
    pub const CLASS_MASK: u64 = 0x0400_0000_0400_0009;

    /// True when `c` is a candidate character in the scanner's classifier.
    ///
    /// `c - 0x2B < 0x3B` **and** bit `c - 0x2B` of [`Self::CLASS_MASK`].
    ///
    /// The accepted set is exactly `{'+', '.', 'E', 'e'}`.
    pub fn class_accepts(c: u8) -> bool {
        if c < 0x2B || c.wrapping_sub(0x2B) >= 0x3B {
            return false;
        }
        (Self::CLASS_MASK >> (c - 0x2B)) & 1 != 0
    }

    /// The lane CaseWare's scanner would take for `raw` (trimmed of padding).
    pub fn lane(raw: &str) -> CwAmountLane {
        let s = raw.trim();
        if s.is_empty() {
            return CwAmountLane::Empty;
        }
        let body = s.strip_prefix('-').unwrap_or(s);
        if body.is_empty() {
            // a lone '-' is not a number; CW falls to the sscanf lane
            return CwAmountLane::Double;
        }
        let negative = s.starts_with('-');
        let gate = if negative {
            Self::GATE_NEGATIVE
        } else {
            Self::GATE_POSITIVE
        };
        let mut acc: i64 = 0;
        for b in body.bytes() {
            if !b.is_ascii_digit() {
                return CwAmountLane::Double;
            }
            // The vendor compares `acc` against the gate *before* multiplying;
            // a run long enough to overflow i64 anyway also takes the double
            // lane, so a checked multiply models the same partition.
            if acc >= gate {
                return CwAmountLane::Double;
            }
            match acc
                .checked_mul(10)
                .and_then(|v| v.checked_add((b - b'0') as i64))
            {
                Some(v) => acc = v,
                None => return CwAmountLane::Double,
            }
        }
        CwAmountLane::Integer
    }

    /// True when the token round-trips through CW's integer lane exactly, i.e.
    /// the vendor and [`Cents::parse`] agree bit for bit.
    pub fn is_exact_integer(raw: &str) -> bool {
        matches!(Self::lane(raw), CwAmountLane::Integer)
    }
}

impl Cents {
    pub const ZERO: Cents = Cents(0);

    pub fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// Parse a trial-balance amount. Blank and `.` are zero.
    pub fn parse(raw: &str) -> Result<Cents> {
        let s = raw.trim();
        if s.is_empty() || s == "." || s == "-" || s == "+" {
            return Ok(Cents::ZERO);
        }
        let (neg, body) = match s.as_bytes()[0] {
            b'-' => (true, &s[1..]),
            b'+' => (false, &s[1..]),
            _ => (false, s),
        };
        let (int_part, frac) = match body.split_once('.') {
            Some((i, f)) => (i, f),
            None => (body, ""),
        };
        if int_part.is_empty() && frac.is_empty() {
            return Ok(Cents::ZERO);
        }
        if !int_part.bytes().all(|b| b.is_ascii_digit())
            || !frac.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(CoreError::Format(format!(
                "amount {raw:?} is not a plain decimal (no separators, currency or parens)"
            )));
        }
        let mut whole: i64 = 0;
        for b in int_part.bytes() {
            whole = whole
                .checked_mul(10)
                .and_then(|v| v.checked_add((b - b'0') as i64))
                .ok_or_else(|| CoreError::Format(format!("amount {raw:?} overflows cents")))?;
        }
        let d0 = frac.as_bytes().first().map(|b| (b - b'0') as i64).unwrap_or(0);
        let d1 = frac.as_bytes().get(1).map(|b| (b - b'0') as i64).unwrap_or(0);
        let mut cents = whole
            .checked_mul(100)
            .and_then(|v| v.checked_add(d0 * 10 + d1))
            .ok_or_else(|| CoreError::Format(format!("amount {raw:?} overflows cents")))?;
        if frac.as_bytes().get(2).map(|b| *b >= b'5').unwrap_or(false) {
            cents += 1;
        }
        Ok(Cents(if neg { -cents } else { cents }))
    }

    /// CW money text (`-6806301.24`) — the `N19.2` form BL cells carry.
    pub fn money(self) -> String {
        let sign = if self.0 < 0 { "-" } else { "" };
        let a = self.0.unsigned_abs();
        format!("{sign}{}.{:02}", a / 100, a % 100)
    }

    pub fn to_f64(self) -> f64 {
        self.0 as f64 / 100.0
    }
}

impl std::fmt::Display for Cents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.money())
    }
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

/// Severity of an [`ImportIssue`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// The run must not proceed.
    Refuse,
    /// The run proceeds; the row or key is reduced or carried.
    Warn,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Refuse => "refuse",
            Severity::Warn => "warn",
        }
    }
}

/// What is wrong with a layout or a source line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueKind {
    /// No bindings, or two binding families without a dispatch extension.
    NoFields,
    /// A 0-based start/column (the pre-F52 root cause).
    ZeroBase,
    /// `start > end`, or a balance triple that binds nothing.
    InvalidRange,
    /// Two bindings read the same bytes.
    Overlapping,
    /// No account-number binding.
    NoAccountNumber,
    /// Every bound field name is unrecognized.
    UnknownFieldsOnly,
    /// The source line is shorter than a bound fixed range.
    LineTooShort,
    /// The source line has fewer columns than a bound index.
    ColumnMissing,
    /// A bound amount cell is not a plain decimal.
    BadAmount,
    /// Two source lines carry the same account number.
    DuplicateAccount,
    /// An account number wider than `AM.AC_NO` (40).
    AccountTooWide,
    /// A description that is not Latin-1 or is wider than `AM.AC_DESC` (50).
    DescriptionTooWide,
    /// The layout binds a balance kind with no modelled destination.
    UnsupportedBalanceKind,
    /// A row with no account number.
    BlankAccount,
    /// The engagement tables are not the canonical schema.
    Schema,
}

impl IssueKind {
    pub fn label(self) -> &'static str {
        match self {
            IssueKind::NoFields => "no fields",
            IssueKind::ZeroBase => "zero-based binding",
            IssueKind::InvalidRange => "invalid range",
            IssueKind::Overlapping => "overlapping bindings",
            IssueKind::NoAccountNumber => "no account number",
            IssueKind::UnknownFieldsOnly => "unknown fields only",
            IssueKind::LineTooShort => "line too short",
            IssueKind::ColumnMissing => "column missing",
            IssueKind::BadAmount => "bad amount",
            IssueKind::DuplicateAccount => "duplicate account",
            IssueKind::AccountTooWide => "account too wide",
            IssueKind::DescriptionTooWide => "description too wide",
            IssueKind::UnsupportedBalanceKind => "unsupported balance kind",
            IssueKind::BlankAccount => "blank account",
            IssueKind::Schema => "schema",
        }
    }
}

/// One finding, addressable to a layout binding or a source line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportIssue {
    pub severity: Severity,
    /// 1-based source line, when the finding is about data.
    pub line: Option<usize>,
    pub field: Option<ImportField>,
    pub kind: IssueKind,
    pub detail: String,
}

impl std::fmt::Display for ImportIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.severity.label(), self.detail)?;
        if let Some(l) = self.line {
            write!(f, " (line {l})")?;
        }
        Ok(())
    }
}

/// One source record decoded through the map.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MappedRow {
    /// 1-based line in the source.
    pub source_line: usize,
    /// Bound destination fields in layout order, raw text.
    pub fields: Vec<(ImportField, String)>,
    /// Landed balances, signed debit-positive.
    pub balances: BTreeMap<BalanceKind, Cents>,
    /// Foreign-amount cells, carried but not landed.
    pub fx: BTreeMap<BalanceKind, Cents>,
}

impl MappedRow {
    pub fn get(&self, field: &ImportField) -> Option<&str> {
        self.fields
            .iter()
            .find(|(f, _)| f == field)
            .map(|(_, v)| v.as_str())
    }

    pub fn account_number(&self) -> &str {
        self.get(&ImportField::AccountNumber).unwrap_or("").trim()
    }

    pub fn description(&self) -> &str {
        self.get(&ImportField::AccountDescription).unwrap_or("").trim()
    }

    pub fn normal_sign(&self) -> Option<char> {
        self.get(&ImportField::NormalSign)
            .and_then(|s| s.trim().chars().next())
            .map(|c| c.to_ascii_uppercase())
    }

    pub fn statement_type(&self) -> Option<char> {
        self.get(&ImportField::StatementType)
            .and_then(|s| s.trim().chars().next())
            .map(|c| c.to_ascii_uppercase())
    }

    pub fn leadsheet(&self) -> &str {
        self.get(&ImportField::Leadsheet).unwrap_or("").trim()
    }

    pub fn tax_ident(&self) -> &str {
        self.get(&ImportField::TaxIdent).unwrap_or("").trim()
    }

    pub fn unit(&self) -> &str {
        self.get(&ImportField::Unit).unwrap_or("").trim()
    }

    /// The balance of `kind`, when bound.
    pub fn balance(&self, kind: BalanceKind) -> Option<Cents> {
        self.balances.get(&kind).copied()
    }
}

/// A decoded source file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportData {
    pub rows: Vec<MappedRow>,
    /// Findings that did not stop the read (skipped rows, unmodelled kinds).
    pub issues: Vec<ImportIssue>,
    pub mode: Option<LayoutMode>,
    /// The effective delimiter for a delimited read.
    pub delimiter: Option<char>,
    pub skipped_header: usize,
    pub skipped_footer: usize,
    pub source_lines: usize,
}

impl ImportData {
    /// The refusals carried by the read (normally empty: a refusal aborts).
    pub fn refusals(&self) -> Vec<&ImportIssue> {
        self.issues
            .iter()
            .filter(|i| i.severity == Severity::Refuse)
            .collect()
    }
}

/// Read a source file through `map`.
///
/// Refuses (`Err`) on any structural [`ColumnMap`] refusal and on the first line
/// that does not satisfy a binding — that is the "layout mismatch" path, and the
/// message names the binding, the required range/column and the line. Per-row
/// problems that leave the record meaningful (blank account, duplicate number,
/// unmodelled balance kind) are reported in [`ImportData::issues`].
///
/// [`read_ascii_report`] is the same read without the abort: it keeps every
/// readable row and records each failure as a [`Severity::Refuse`] issue.
pub fn read_ascii(map: &ColumnMap, bytes: &[u8]) -> Result<ImportData> {
    read_impl(map, bytes, false)
}

/// [`read_ascii`] without the abort.
///
/// Structural layout refusals still fail (no reading is possible), but a bad
/// *record* becomes a refusal issue and the scan continues, so one malformed
/// trailer cannot hide the rest of a file. The returned data must not be
/// applied while [`ImportData::refusals`] is non-empty.
pub fn read_ascii_report(map: &ColumnMap, bytes: &[u8]) -> Result<ImportData> {
    read_impl(map, bytes, true)
}

fn read_impl(map: &ColumnMap, bytes: &[u8], lenient: bool) -> Result<ImportData> {
    let refusals = map.refusals();
    if !refusals.is_empty() {
        return Err(CoreError::Format(format!(
            "layout refused ({} issue(s)): {}",
            refusals.len(),
            refusals
                .iter()
                .map(|i| i.detail.clone())
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }
    let mode = map.resolved_mode()?;
    let delim = match mode {
        LayoutMode::Delimited => Some(map.options.delimiter()?),
        LayoutMode::Fixed => None,
    };
    let text = latin1(bytes);
    let mut lines: Vec<(usize, &str)> = text
        .split('\n')
        .enumerate()
        .map(|(i, raw)| (i + 1, raw.strip_suffix('\r').unwrap_or(raw)))
        .collect();
    // A trailing newline yields one empty logical line; it is not a record.
    if lines.last().map(|(_, l)| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    let total = lines.len();
    let skip_head = map.options.skip_header_lines.min(total);
    let skip_tail = map.options.skip_footer_lines.min(total - skip_head);
    let body = &lines[skip_head..total - skip_tail];

    let mut data = ImportData {
        mode: Some(mode),
        delimiter: delim,
        skipped_header: skip_head,
        skipped_footer: skip_tail,
        source_lines: total,
        ..Default::default()
    };
    for (line_no, line) in body {
        if line.trim().is_empty() {
            continue;
        }
        let parsed = match mode {
            LayoutMode::Fixed => read_fixed_row(map, *line_no, line),
            LayoutMode::Delimited => {
                let cells = split_delimited(
                    line,
                    delim.expect("delimited"),
                    map.options.text_encapsulator,
                );
                read_delimited_row(map, *line_no, &cells)
            }
        };
        match parsed {
            Ok(row) => ingest_row(map, &mut data, row),
            Err(e) if lenient => data.issues.push(e.issue(Some(*line_no))),
            Err(e) => return Err(CoreError::Format(e.message())),
        }
    }
    Ok(data)
}

/// Read an already-split grid — the xlsx/CSV seam.
///
/// The delimited reader is defined over a grid of cells, so a future xlsx
/// adapter only has to produce `Vec<Vec<String>>`; no column-map, refusal or
/// write-back work is duplicated. See [`xlsx_plan`].
pub fn read_grid(map: &ColumnMap, grid: &[Vec<String>]) -> Result<ImportData> {
    let refusals = map.refusals();
    if !refusals.is_empty() {
        return Err(CoreError::Format(format!(
            "layout refused ({} issue(s)): {}",
            refusals.len(),
            refusals
                .iter()
                .map(|i| i.detail.clone())
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }
    let mode = map.resolved_mode()?;
    let total = grid.len();
    let skip_head = map.options.skip_header_lines.min(total);
    let skip_tail = map.options.skip_footer_lines.min(total - skip_head);
    let mut data = ImportData {
        mode: Some(mode),
        source_lines: total,
        skipped_header: skip_head,
        skipped_footer: skip_tail,
        ..Default::default()
    };
    for (i, cells) in grid.iter().enumerate() {
        if i < skip_head || i >= total - skip_tail {
            continue;
        }
        match read_delimited_row(map, i + 1, cells) {
            Ok(row) => ingest_row(map, &mut data, row),
            Err(e) => return Err(CoreError::Format(e.message())),
        }
    }
    Ok(data)
}

/// Common per-row finalization: duplicate collapse, `nozero`, unmodelled-kind
/// warnings.
fn ingest_row(map: &ColumnMap, data: &mut ImportData, row: MappedRow) {
    if row.account_number().is_empty() {
        data.issues.push(ImportIssue {
            severity: Severity::Warn,
            line: Some(row.source_line),
            field: Some(ImportField::AccountNumber),
            kind: IssueKind::BlankAccount,
            detail: "row has no account number; skipped".into(),
        });
        return;
    }
    let no = row.account_number().to_string();
    if let Some(prev) = data
        .rows
        .iter()
        .position(|r: &MappedRow| r.account_number() == no)
    {
        let prev_line = data.rows[prev].source_line;
        data.issues.push(ImportIssue {
            severity: Severity::Warn,
            line: Some(row.source_line),
            field: Some(ImportField::AccountNumber),
            kind: IssueKind::DuplicateAccount,
            detail: format!(
                "account {no:?} already read on line {prev_line}; this line replaces it"
            ),
        });
        data.rows.remove(prev);
    }
    if map.options.no_zero
        && !row.balances.is_empty()
        && row.balances.values().all(|c| c.is_zero())
    {
        return;
    }
    let mut warned: BTreeSet<BalanceKind> = BTreeSet::new();
    for kind in row.balances.keys() {
        if kind.bl_year().is_none() && warned.insert(*kind) {
            data.issues.push(ImportIssue {
                severity: Severity::Warn,
                line: Some(row.source_line),
                field: None,
                kind: IssueKind::UnsupportedBalanceKind,
                detail: format!(
                    "{} has no modelled destination (only Current Year Balance lands, F52b)",
                    kind.key_name()
                ),
            });
        }
    }
    data.rows.push(row);
}

/// A row-level reading failure, carrying the typed issue the strict reader
/// turns into an error and the lenient scanner records.
#[derive(Clone, Debug)]
struct RowError {
    kind: IssueKind,
    field: Option<ImportField>,
    detail: String,
}

impl RowError {
    fn message(&self) -> String {
        self.detail.clone()
    }

    fn issue(&self, line: Option<usize>) -> ImportIssue {
        ImportIssue {
            severity: Severity::Refuse,
            line,
            field: self.field.clone(),
            kind: self.kind,
            detail: self.detail.clone(),
        }
    }
}

type CellReader<'a> = dyn Fn(&Column, &ImportField) -> std::result::Result<Option<String>, RowError> + 'a;

fn fixed_read(line_no: usize, line: &str) -> impl Fn(&Column, &ImportField) -> std::result::Result<Option<String>, RowError> + '_ {
    let bytes = line.as_bytes();
    move |col, field| match *col {
        Column::Fixed { start, end } => {
            let (s, e) = (start as usize, end as usize);
            debug_assert!(s >= 1 && s <= e);
            if bytes.len() < e {
                return Err(RowError {
                    kind: IssueKind::LineTooShort,
                    field: Some(field.clone()),
                    detail: format!(
                        "layout mismatch at line {line_no}: {} needs bytes {start}..={end} \
                         (1-based inclusive), line has {}",
                        field.key_label(),
                        bytes.len()
                    ),
                });
            }
            Ok(Some(latin1(&bytes[s - 1..e])))
        }
        Column::Delimited { column } => Err(RowError {
            kind: IssueKind::ColumnMissing,
            field: Some(field.clone()),
            detail: format!(
                "line {line_no}: {} is bound to delimited column {column} inside a fixed \
                 layout",
                field.key_label()
            ),
        }),
    }
}

fn read_fixed_row(map: &ColumnMap, line_no: usize, line: &str) -> std::result::Result<MappedRow, RowError> {
    let read = fixed_read(line_no, line);
    build_row(map, line_no, &read)
}

fn read_delimited_row(
    map: &ColumnMap,
    line_no: usize,
    cells: &[String],
) -> std::result::Result<MappedRow, RowError> {
    let read = |col: &Column, field: &ImportField| -> std::result::Result<Option<String>, RowError> {
        let column = match *col {
            Column::Delimited { column } => column,
            Column::Fixed { start, .. } => {
                return Err(RowError {
                    kind: IssueKind::ColumnMissing,
                    field: Some(field.clone()),
                    detail: format!(
                        "line {line_no}: {} is bound to fixed bytes {start}.. inside a \
                         delimited layout",
                        field.key_label()
                    ),
                })
            }
        };
        match cells.get(column as usize - 1) {
            Some(v) => Ok(Some(v.clone())),
            None => Err(RowError {
                kind: IssueKind::ColumnMissing,
                field: Some(field.clone()),
                detail: format!(
                    "layout mismatch at line {line_no}: {} needs column {column} (1-based), \
                     line has {}",
                    field.key_label(),
                    cells.len()
                ),
            }),
        }
    };
    build_row(map, line_no, &read)
}

/// Resolve every binding of `map` into one [`MappedRow`].
fn build_row(
    map: &ColumnMap,
    line_no: usize,
    read: &CellReader<'_>,
) -> std::result::Result<MappedRow, RowError> {
    let mut row = MappedRow { source_line: line_no, ..Default::default() };
    for b in &map.fields {
        let value = read(&b.column, &b.field)?.ok_or_else(|| RowError {
            kind: IssueKind::ColumnMissing,
            field: Some(b.field.clone()),
            detail: format!(
                "layout mismatch at line {line_no}: {} is unreadable",
                b.field.key_label()
            ),
        })?;
        row.fields.push((b.field.clone(), value));
    }
    for b in &map.balances {
        let cell = |col: u32, slot: &str| -> std::result::Result<Cents, RowError> {
            let field = ImportField::Other(format!("{} {slot}", b.kind.key_name()));
            let raw = read(&Column::Delimited { column: col }, &field)?.unwrap_or_default();
            Cents::parse(&raw).map_err(|_| RowError {
                kind: IssueKind::BadAmount,
                field: Some(ImportField::Other(b.kind.key_name().to_string())),
                detail: format!(
                    "layout mismatch at line {line_no}: {} {slot} cell {raw:?} is not a \
                     plain decimal",
                    b.kind.key_name()
                ),
            })
        };
        let mut debit = 0i64;
        let mut credit = 0i64;
        if let Some(c) = b.debit {
            debit = cell(c, "db")?.0;
        }
        if let Some(c) = b.credit {
            credit = cell(c, "cr")?.0;
        }
        let mut cents = Cents(debit - credit);
        if map.options.positive_as_credit || map.options.balance_as_credit {
            cents = Cents(-cents.0);
        }
        row.balances.insert(b.kind, cents);
        if let Some(c) = b.fx {
            row.fx.insert(b.kind, cell(c, "fx")?);
        }
    }
    Ok(row)
}

/// Split one delimited record, honouring the declared quote character.
///
/// A doubled quote inside a quoted cell is one literal quote (the CSV/Excel
/// convention the wizard produces).
pub fn split_delimited(line: &str, delim: char, quote: Option<char>) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) if c == q => {
                if in_quote {
                    if chars.peek() == Some(&q) {
                        chars.next();
                        cur.push(q);
                    } else {
                        in_quote = false;
                    }
                } else if cur.is_empty() {
                    in_quote = true;
                } else {
                    cur.push(c);
                }
            }
            _ if c == delim && !in_quote => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

// ---------------------------------------------------------------------------
// Import plan and write-back
// ---------------------------------------------------------------------------

/// `AM.LINENO` spacing (corp2026: every one of the 324 live deltas is 32).
pub const AM_LINE_STEP: i64 = 32;
/// The `LINENO` sentinel hull2024 uses on its 50 memo rows; never a base.
pub const AM_LINE_SENTINEL: i64 = 99_999_999;
/// `AM.AC_NO` width.
pub const AM_AC_NO_WIDTH: usize = 40;
/// `AM.AC_DESC` width.
pub const AM_DESC_WIDTH: usize = 50;
/// `BL` official-layer id.
pub const BL_OPEN_ID: &str = "O";
/// `BL` computed-overlay id.
pub const BL_COMPUTED_ID: &str = "X";
/// `BL` opening bucket.
pub const BL_OPEN_BUCKET: &str = "O";
/// The reporting basis an import writes (`GROUP='A'`).
pub const BL_IMPORT_GROUP: &str = "A";

/// Caller intent for one run — the `ICWImport` knobs the trial-balance lane
/// honours, plus the deterministic stamp/seed knobs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportOptions {
    /// `ImportComponents` — only [`ImportComponents::TrialBalance`] lands here.
    pub components: ImportComponents,
    /// The shared 20-slot settings vector (`SetSetting(i, v)`).
    pub settings: BTreeMap<Setting, SettingValue>,
    /// `BL.ENTITY` for every landed pair (the corpus is single-blank-entity).
    pub entity: String,
    /// `AM.TYPE` for a new account the file does not type. F52 lists `TYPE`
    /// defaulting as a residual, so it is an explicit knob, not a hidden guess.
    pub default_type: char,
    /// 14-char `YYYYMMDDhhmmss` stamp for new/changed rows.
    pub stamp: String,
    /// Seed for the deterministic `CWGUID` allocator.
    pub seed: u64,
}

impl Default for ImportOptions {
    fn default() -> Self {
        ImportOptions {
            components: ImportComponents::TrialBalance,
            settings: BTreeMap::new(),
            entity: String::new(),
            default_type: 'B',
            stamp: "20260101000000".to_string(),
            seed: 0x1_4D_50_52,
        }
    }
}

impl ImportOptions {
    /// `SetSetting(i, v)` — the dispid-3 surface. Every legal slot is stored,
    /// exactly as the shared vector behaves; the lane reads the bits it owns.
    pub fn set(&mut self, setting: Setting, value: SettingValue) {
        self.settings.insert(setting, value);
    }

    /// `Setting(i)` — `None` when unset, as the COM getter reports.
    pub fn get(&self, setting: Setting) -> Option<&SettingValue> {
        self.settings.get(&setting)
    }

    fn flag(&self, setting: Setting) -> bool {
        self.get(setting).map(|v| v.truthy()).unwrap_or(false)
    }

    /// `AVOverwriteAccDescription` (setting 5).
    pub fn overwrite_description(&self) -> bool {
        self.flag(Setting::AvOverwriteAccDescription)
    }

    /// `AVPriorYearData` (setting 1).
    pub fn prior_year_data(&self) -> bool {
        self.flag(Setting::AvPriorYearData)
    }

    /// `AVNonfinancialData` (setting 3).
    pub fn nonfinancial_data(&self) -> bool {
        self.flag(Setting::AvNonfinancialData)
    }

    /// `AsciiClearExistingAJE` (setting 17).
    pub fn clear_existing_aje(&self) -> bool {
        self.flag(Setting::AsciiClearExistingAje)
    }

    /// `AsciiAccumulateAJE` (setting 18).
    pub fn accumulate_aje(&self) -> bool {
        self.flag(Setting::AsciiAccumulateAje)
    }

    /// Settings this run carries but does not act on, for the report.
    pub fn notes(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.prior_year_data() {
            out.push("AVPriorYearData: prior-year layers are not landed (F52 residual)".into());
        }
        if self.nonfinancial_data() {
            out.push("AVNonfinancialData: non-financial columns are carried, not landed".into());
        }
        if self.clear_existing_aje() {
            out.push("AsciiClearExistingAJE: this lane writes no AJE rows".into());
        }
        if self.accumulate_aje() {
            out.push("AsciiAccumulateAJE: this lane writes no AJE rows".into());
        }
        out
    }
}

/// One `AM` row to append.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AmInsert {
    pub account: String,
    pub description: String,
    /// `AM.TYPE` letter.
    pub kind: char,
    /// `AM.SIGN` letter (`D`/`C`).
    pub sign: char,
    pub leadsheet: String,
    pub tax_ident: String,
    pub unit: String,
    /// `AM.LINENO` value.
    pub line_no: i64,
    /// 1-based source line the account came from.
    pub source_line: usize,
}

/// One existing `AM` row whose description the run would replace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AmUpdate {
    /// Physical `AM` row.
    pub row: usize,
    pub account: String,
    pub old_description: String,
    pub new_description: String,
    pub source_line: usize,
}

/// One `BL` year-0 pair the run claims.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlPair {
    pub number: String,
    pub entity: String,
    pub cents: Cents,
    /// Existing live `ID='O'` row, if any.
    pub open_row: Option<usize>,
    /// Existing live `ID='X'` row, if any.
    pub computed_row: Option<usize>,
    pub source_line: usize,
}

/// Account totals over the source file (cents, signed debit-positive).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportTotals {
    /// Sum of debit balances (positive cents).
    pub debits: Cents,
    /// Sum of credit balances (negative cents, as stored).
    pub credits: Cents,
    /// Signed sum.
    pub net: Cents,
}

/// The F52 diff plan: what a run *would* change, computed without writing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportPreview {
    pub mode: Option<LayoutMode>,
    /// Accounts in the source file (after duplicate collapse).
    pub accounts_in_file: usize,
    pub inserts: Vec<AmInsert>,
    pub updates: Vec<AmUpdate>,
    /// Existing accounts the run leaves alone.
    pub kept: Vec<String>,
    pub bl_pairs: Vec<BlPair>,
    /// Accounts whose current-year balance is exactly zero: they claim no `BL`
    /// row.
    pub zero_balance: Vec<String>,
    pub totals: ImportTotals,
    pub issues: Vec<ImportIssue>,
}

impl ImportPreview {
    /// True when applying cannot change a byte.
    pub fn is_noop(&self) -> bool {
        self.inserts.is_empty()
            && self.updates.is_empty()
            && self.bl_pairs.iter().all(|p| {
                p.cents.is_zero() && p.open_row.is_some() && p.computed_row.is_some()
            })
    }

    /// Refusals carried by the preview.
    pub fn refusals(&self) -> Vec<&ImportIssue> {
        self.issues
            .iter()
            .filter(|i| i.severity == Severity::Refuse)
            .collect()
    }
}

/// What [`apply`] actually did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub inserted: usize,
    pub updated: usize,
    /// Pairs rewritten in place (both halves existed).
    pub bl_rewritten: usize,
    /// `BL` rows appended (a missing half of a pair).
    pub bl_appended: usize,
    /// Accounts that claimed no `BL` row (zero balance).
    pub bl_skipped: usize,
    pub totals: ImportTotals,
    pub notes: Vec<String>,
}

/// Compute the F52 diff against the engagement without writing.
pub fn preview(
    am: &Table,
    bl: &Table,
    data: &ImportData,
    opts: &ImportOptions,
) -> Result<ImportPreview> {
    if opts.components != ImportComponents::TrialBalance {
        return Err(CoreError::Format(format!(
            "{} import needs the journals lane and is not implemented (FEAT.009 scope)",
            opts.components.label()
        )));
    }
    // Schema gate: a drifted table must never be written through typed cells.
    Chart::from_table(am)?;
    Cube::from_table(bl)?;

    let existing = am_rows_by_account(am);
    let mut next_line = next_line_no(am);
    let mut out = ImportPreview {
        mode: data.mode,
        issues: data.issues.clone(),
        ..Default::default()
    };
    // Existing BL year-0 O/X rows, keyed by (number, entity).
    let mut bl_index: BTreeMap<(String, String), (Option<usize>, Option<usize>)> = BTreeMap::new();
    for r in 0..bl.count() {
        if bl.is_deleted(r)
            || trim_cell(bl, r, "YEAR") != "0"
            || trim_cell(bl, r, "BUCKET") != BL_OPEN_BUCKET
            || trim_cell(bl, r, "GROUP") != BL_IMPORT_GROUP
        {
            continue;
        }
        let slot = bl_index
            .entry((trim_cell(bl, r, "NUMBER"), trim_cell(bl, r, "ENTITY")))
            .or_default();
        match trim_cell(bl, r, "ID").as_str() {
            BL_OPEN_ID if slot.0.is_none() => slot.0 = Some(r),
            BL_COMPUTED_ID if slot.1.is_none() => slot.1 = Some(r),
            _ => {}
        }
    }

    let mut totals = ImportTotals::default();
    for row in &data.rows {
        let no = row.account_number().to_string();
        let desc = row.description().to_string();
        if no.chars().count() > AM_AC_NO_WIDTH {
            out.issues.push(ImportIssue {
                severity: Severity::Refuse,
                line: Some(row.source_line),
                field: Some(ImportField::AccountNumber),
                kind: IssueKind::AccountTooWide,
                detail: format!("account {no:?} is longer than AM.AC_NO ({AM_AC_NO_WIDTH})"),
            });
            continue;
        }
        if desc.chars().any(|c| c as u32 > 0xFF) {
            out.issues.push(ImportIssue {
                severity: Severity::Refuse,
                line: Some(row.source_line),
                field: Some(ImportField::AccountDescription),
                kind: IssueKind::DescriptionTooWide,
                detail: format!(
                    "line {}: description is not Latin-1 (CW text codepage)",
                    row.source_line
                ),
            });
            continue;
        }
        if desc.chars().count() > AM_DESC_WIDTH {
            out.issues.push(ImportIssue {
                severity: Severity::Refuse,
                line: Some(row.source_line),
                field: Some(ImportField::AccountDescription),
                kind: IssueKind::DescriptionTooWide,
                detail: format!(
                    "description {desc:?} is longer than AM.AC_DESC ({AM_DESC_WIDTH})"
                ),
            });
            continue;
        }
        let kind = match row.statement_type().unwrap_or(opts.default_type) {
            c @ ('B' | 'I' | 'M') => c,
            other => {
                out.issues.push(ImportIssue {
                    severity: Severity::Refuse,
                    line: Some(row.source_line),
                    field: Some(ImportField::StatementType),
                    kind: IssueKind::Schema,
                    detail: format!("statement type {other:?} is not B/I/M"),
                });
                continue;
            }
        };

        match existing.get(&no) {
            Some(&r) => {
                let old = trim_cell(am, r, "AC_DESC");
                if opts.overwrite_description() && !desc.is_empty() && desc != old.trim() {
                    out.updates.push(AmUpdate {
                        row: r,
                        account: no.clone(),
                        old_description: old,
                        new_description: desc,
                        source_line: row.source_line,
                    });
                } else {
                    out.kept.push(no.clone());
                }
            }
            None => {
                out.inserts.push(AmInsert {
                    account: no.clone(),
                    description: desc,
                    kind,
                    sign: row
                        .normal_sign()
                        .filter(|c| matches!(c, 'D' | 'C'))
                        .unwrap_or('D'),
                    leadsheet: row.leadsheet().to_string(),
                    tax_ident: row.tax_ident().to_string(),
                    unit: row.unit().to_string(),
                    line_no: next_line,
                    source_line: row.source_line,
                });
                next_line += AM_LINE_STEP;
            }
        }

        let bal = row
            .balance(BalanceKind::CurrentYear)
            .unwrap_or(Cents::ZERO);
        if bal.is_zero() {
            out.zero_balance.push(no);
            continue;
        }
        totals.net = Cents(totals.net.0 + bal.0);
        if bal.0 > 0 {
            totals.debits = Cents(totals.debits.0 + bal.0);
        } else {
            totals.credits = Cents(totals.credits.0 + bal.0);
        }
        let slot = bl_index
            .get(&(no.clone(), opts.entity.clone()))
            .copied()
            .unwrap_or((None, None));
        out.bl_pairs.push(BlPair {
            number: no,
            entity: opts.entity.clone(),
            cents: bal,
            open_row: slot.0,
            computed_row: slot.1,
            source_line: row.source_line,
        });
    }
    out.accounts_in_file = data.rows.len();
    out.totals = totals;
    if let Some(r) = out.refusals().first() {
        return Err(CoreError::Format(format!("import refused: {}", r.detail)));
    }
    Ok(out)
}

/// Apply a preview.
///
/// Both tables are written on clones and swapped only when every write
/// succeeded, so a refused cell leaves the engagement byte-identical.
pub fn apply(
    am: &mut Table,
    bl: &mut Table,
    plan: &ImportPreview,
    opts: &ImportOptions,
) -> Result<ImportReport> {
    let mut am2 = am.clone();
    let mut bl2 = bl.clone();
    let mut rng = Rng::new_seeded(opts.seed);
    let mut report = ImportReport {
        totals: plan.totals.clone(),
        notes: opts.notes(),
        ..Default::default()
    };

    for ins in &plan.inserts {
        let r = am2.records.len();
        am2.records.push(blank_row(&am2));
        write_cell(&mut am2, r, "LINENO", &ins.line_no.to_string())?;
        write_cell(&mut am2, r, "TYPE", &ins.kind.to_string())?;
        write_cell(&mut am2, r, "AC_NO", &right_justified(&ins.account, AM_AC_NO_WIDTH))?;
        write_cell(&mut am2, r, "AC_DESC", &ins.description)?;
        write_cell(&mut am2, r, "SIGN", &ins.sign.to_string())?;
        write_cell(&mut am2, r, "POST", "Y")?;
        write_cell(&mut am2, r, "SCHEDULE", &ins.leadsheet)?;
        write_cell(&mut am2, r, "TAX_ID", &ins.tax_ident)?;
        write_cell(&mut am2, r, "UNIT", &ins.unit)?;
        write_cell(
            &mut am2,
            r,
            "CWGUID",
            &CwGuid::allocate_for_table("AM", &mut rng).encode(),
        )?;
        write_cell(&mut am2, r, "CWDATE", &opts.stamp)?;
        write_cell(&mut am2, r, "CREATEDON", &opts.stamp)?;
        report.inserted += 1;
    }

    for up in &plan.updates {
        write_cell(&mut am2, up.row, "AC_DESC", &up.new_description)?;
        write_cell(&mut am2, up.row, "CWDATE", &opts.stamp)?;
        report.updated += 1;
    }

    for pair in &plan.bl_pairs {
        match pair.open_row {
            Some(r) => write_open_row(&mut bl2, r, pair, &opts.stamp)?,
            None => {
                let r = append_pair_base(&mut bl2, pair, BL_OPEN_ID, &opts.stamp, &mut rng)?;
                write_open_row(&mut bl2, r, pair, &opts.stamp)?;
                report.bl_appended += 1;
            }
        }
        match pair.computed_row {
            Some(r) => {
                write_computed_row(&mut bl2, r, pair, &opts.stamp)?;
                if pair.open_row.is_some() {
                    report.bl_rewritten += 1;
                }
            }
            None => {
                let r =
                    append_pair_base(&mut bl2, pair, BL_COMPUTED_ID, &opts.stamp, &mut rng)?;
                write_computed_row(&mut bl2, r, pair, &opts.stamp)?;
                report.bl_appended += 1;
            }
        }
    }
    report.bl_skipped = plan.zero_balance.len();

    // Post-conditions: the engagement must still decode under its own laws.
    Cube::from_table(&bl2)?;
    Chart::from_table(&am2)?;
    *am = am2;
    *bl = bl2;
    Ok(report)
}

// ---------------------------------------------------------------------------
// Table helpers
// ---------------------------------------------------------------------------

/// Right-justify a text key inside its field width, the way the corpus stores
/// `AM.AC_NO` (325/325 rows) and `BL.NUMBER`.
fn right_justified(value: &str, width: usize) -> String {
    let pad = width.saturating_sub(value.chars().count());
    format!("{}{value}", " ".repeat(pad))
}

fn blank_row(t: &Table) -> RawRecord {
    let mut r = vec![b' '; t.record_len as usize];
    r[0] = b' ';
    r
}

fn trim_cell(t: &Table, r: usize, col: &str) -> String {
    t.cell_str(r, col).unwrap_or_default().trim().to_string()
}

/// Live `AM` rows by `AC_NO` (first live row wins, memo rows included, so an
/// existing number is never duplicated).
fn am_rows_by_account(am: &Table) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for r in 0..am.count() {
        if am.is_deleted(r) {
            continue;
        }
        let no = trim_cell(am, r, "AC_NO");
        if no.is_empty() {
            continue;
        }
        out.entry(no).or_insert(r);
    }
    out
}

/// `max(LINENO) + 32`, ignoring the `99999999` sentinel and blank/zero cells.
fn next_line_no(am: &Table) -> i64 {
    let mut max = 0i64;
    for r in 0..am.count() {
        if am.is_deleted(r) {
            continue;
        }
        let Ok(v) = trim_cell(am, r, "LINENO").parse::<i64>() else {
            continue;
        };
        if v <= 0 || v >= AM_LINE_SENTINEL {
            continue;
        }
        max = max.max(v);
    }
    max + AM_LINE_STEP
}

fn write_open_row(bl: &mut Table, r: usize, pair: &BlPair, stamp: &str) -> Result<()> {
    write_cell(bl, r, "AMT", &pair.cents.money())?;
    // Cube L2: `O` carries AMT only; every fill cell stays empty.
    for c in ["AMT_YTD", "AMT_DIST", "FX", "FX_YTD", "FX_DIST"] {
        write_cell(bl, r, c, "")?;
    }
    write_cell(bl, r, "TYPE", "")?;
    write_cell(bl, r, "PERIOD", &right_justified("0", 2))?;
    write_cell(bl, r, "CWDATE", stamp)?;
    Ok(())
}

fn write_computed_row(bl: &mut Table, r: usize, pair: &BlPair, stamp: &str) -> Result<()> {
    let v = pair.cents.money();
    write_cell(bl, r, "AMT", &v)?;
    write_cell(bl, r, "AMT_YTD", &v)?;
    write_cell(bl, r, "FX", &v)?;
    write_cell(bl, r, "FX_YTD", &v)?;
    // Repost normalization (F44/F65): the DIST cells are `0.00`, never blank.
    write_cell(bl, r, "AMT_DIST", "0.00")?;
    write_cell(bl, r, "FX_DIST", "0.00")?;
    write_cell(bl, r, "TYPE", "")?;
    write_cell(bl, r, "PERIOD", &right_justified("0", 2))?;
    write_cell(bl, r, "CWDATE", stamp)?;
    Ok(())
}

fn append_pair_base(
    bl: &mut Table,
    pair: &BlPair,
    id: &str,
    stamp: &str,
    rng: &mut Rng,
) -> Result<usize> {
    let r = bl.records.len();
    bl.records.push(blank_row(bl));
    write_cell(bl, r, "ID", id)?;
    write_cell(bl, r, "GROUP", BL_IMPORT_GROUP)?;
    write_cell(bl, r, "NUMBER", &right_justified(&pair.number, 40))?;
    write_cell(bl, r, "ENTITY", &pair.entity)?;
    write_cell(bl, r, "YEAR", "0")?;
    write_cell(bl, r, "BUCKET", BL_OPEN_BUCKET)?;
    write_cell(bl, r, "TYPE", "")?;
    write_cell(bl, r, "BOOKED", "")?;
    write_cell(bl, r, "PERIOD", &right_justified("0", 2))?;
    write_cell(bl, r, "CUSTOM", "")?;
    write_cell(
        bl,
        r,
        "CWGUID",
        &CwGuid::allocate_for_table("BL", rng).encode(),
    )?;
    write_cell(bl, r, "CWDATE", stamp)?;
    write_cell(bl, r, "CREATEDON", stamp)?;
    Ok(r)
}

// ---------------------------------------------------------------------------
// xlsx — planned, deliberately not implemented
// ---------------------------------------------------------------------------

/// The precise parser plan for the Excel lane, which is **not** implemented.
///
/// The kernel already carries everything the container needs
/// ([`cw_core::pkg::Zip`] for the OPC central directory,
/// [`cw_core::inflate::inflate`] for method 8, `cw_core::lzma` for the CW
/// variant), and this module already carries the grid seam ([`read_grid`]), so
/// what remains is a hand-rolled XML pull parser plus the OPC wiring — a lane
/// of its own, not a stub here:
///
/// 1. **OPC resolution** — `Zip::parse`; then `_rels/.rels` →
///    `officeDocument` → `xl/workbook.xml`, `xl/_rels/workbook.xml.rels` → the
///    `worksheet` target for the first sheet's `r:id` (or a caller-chosen sheet
///    name). Each part's rels resolve against that part's own directory.
/// 2. **Shared strings** — `xl/sharedStrings.xml`: `<si>` items, concatenating
///    every `<t>` child (rich-text `<r><t>` runs included), `xml:space`
///    preserved. Cells with `t="s"` index this table.
/// 3. **Cell decode** — `t="s"` shared, `t="inlineStr"` inline `<is><t>`,
///    `t="str"` formula string, `t="b"` `0`/`1`, `t="e"` error → refused, no
///    `t` = number in its raw lexical form (never reformatted: the amount law is
///    [`Cents::parse`]).
/// 4. **Addressing** — A1 refs (`<c r="AB12">`) into (row, col); a missing `r`
///    means "next column". Rows emit dense up to the widest cell, so a trailing
///    gap is blank and the grid matches what the ASCII lane would see.
/// 5. **Dates** — `xl/styles.xml` `numFmtId` + `cellXfs`; a cell whose format is
///    a date format maps its serial to `YYYYMMDD`. **Refuse** when the style
///    table is missing rather than guessing an epoch.
/// 6. **Formula cells** — the cached `<v>` is used; a formula with no cached
///    value is a refusal (no evaluation).
/// 7. **Hand-off** — the grid goes to [`read_grid`], so the column map, the
///    refusal law and the write-back are shared with the ASCII lane.
///
/// Deferred beyond that: `xl/workbook.xml` defined names, multiple sheets, `.xls`
/// (BIFF8 is a different container entirely) and encrypted workbooks.
pub fn xlsx_plan() -> &'static str {
    "xlsx is wave 2: OPC parts via cw_core::pkg::Zip + inflate, a hand-rolled \
     sharedStrings/sheet/styles pull parser, then read_grid (see import.rs docs)"
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cents_parse_is_exact() {
        for (text, cents) in [
            ("125000.00", 12_500_000i64),
            ("111.11", 11_111),
            ("-6806301.24", -680_630_124),
            ("123.4", 12_340),
            ("125000.5", 12_500_050),
            ("", 0),
            (".", 0),
            ("0", 0),
            ("-0.005", -1),
        ] {
            assert_eq!(Cents::parse(text).unwrap().0, cents, "{text}");
        }
        assert_eq!(Cents(-680_630_124).money(), "-6806301.24");
        assert_eq!(Cents(0).money(), "0.00");
        assert!(Cents::parse("1,234.00").is_err());
        assert!(Cents::parse("(123.00)").is_err());
    }

    #[test]
    fn setting_domain_is_exactly_twenty() {
        assert_eq!(Setting::ALL.len(), 20);
        for i in 0..20 {
            assert_eq!(Setting::parse(i).unwrap().code(), i as usize);
        }
        assert!(Setting::parse(20).is_err());
        assert!(Setting::parse(-1).is_err());
        assert_eq!(
            Setting::AvOverwriteAccDescription.name(),
            "AVOverwriteAccDescription"
        );
    }

    #[test]
    fn package_and_component_codes_are_the_probed_set() {
        assert_eq!(ImportPackage::parse(2).unwrap(), ImportPackage::Ascii);
        assert!(ImportPackage::parse(0).is_err());
        assert!(ImportPackage::parse(5).is_err());
        assert_eq!(
            ImportComponents::parse(0).unwrap(),
            ImportComponents::TrialBalance
        );
        assert!(ImportComponents::parse(3).is_err());
    }

    #[test]
    fn field_names_resolve_both_spellings() {
        assert_eq!(
            ImportField::from_key_name("account number"),
            ImportField::AccountNumber
        );
        assert_eq!(
            ImportField::from_key_name("AccountNumber"),
            ImportField::AccountNumber
        );
        assert_eq!(
            ImportField::from_key_name("group3description"),
            ImportField::GroupDescription(3)
        );
        assert_eq!(ImportField::from_key_name("flip7"), ImportField::Flip(7));
        assert_eq!(
            ImportField::from_key_name("weird thing"),
            ImportField::Other("weird thing".into())
        );
    }

    #[test]
    fn balance_kind_registry_is_fifteen_and_round_trips() {
        assert_eq!(BalanceKind::ALL.len(), 15);
        assert_eq!(BalanceKind::CurrentYear.bl_year(), Some(0));
        assert_eq!(BalanceKind::Prior(YearOffset::Y2).bl_year(), None);
        for k in BalanceKind::ALL {
            assert_eq!(BalanceKind::from_key_name(k.key_name()), Some(k));
        }
        assert_eq!(
            BalanceKind::from_key_name("py3 balance"),
            Some(BalanceKind::Prior(YearOffset::Y3))
        );
    }

    #[test]
    fn split_delimited_honours_quotes() {
        assert_eq!(split_delimited("a,b,c", ',', None), ["a", "b", "c"]);
        assert_eq!(split_delimited("\"a,1\",b", ',', Some('"')), ["a,1", "b"]);
        assert_eq!(
            split_delimited("\"say \"\"hi\"\"\",b", ',', Some('"')),
            ["say \"hi\"", "b"]
        );
    }

    #[test]
    fn layout_mode_families_are_documented() {
        assert_eq!(LayoutMode::from_extension(".ftb"), Some(LayoutMode::Fixed));
        assert_eq!(
            LayoutMode::from_extension("vtb"),
            Some(LayoutMode::Delimited)
        );
        assert_eq!(LayoutMode::from_extension(".faj"), Some(LayoutMode::Fixed));
        assert_eq!(LayoutMode::from_extension(".crd"), Some(LayoutMode::Fixed));
        assert_eq!(LayoutMode::from_extension(".txt"), None);
    }

    #[test]
    fn zero_based_bindings_refuse() {
        let mut map = ColumnMap {
            mode: Some(LayoutMode::Fixed),
            ..Default::default()
        };
        map.bind(
            ImportField::AccountNumber,
            Column::Fixed { start: 0, end: 9 },
        );
        let refs = map.refusals();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].kind, IssueKind::ZeroBase);
        assert!(read_ascii(&map, b"1000-00\n").is_err());
    }
}

#[cfg(test)]
mod q8_grammar_tests {
    use super::{Cents, CwAmountGrammar, CwAmountLane};

    /// The classifier mask decodes to exactly `+`, `.` and `e`.
    #[test]
    fn classifier_accepts_plus_dot_and_e_only() {
        for c in [b'+', b'.', b'e', b'E'] {
            assert!(CwAmountGrammar::class_accepts(c), "{} must be accepted", c as char);
        }
        for c in [b'0', b'9', b',', b' ', b'/', b'$', b'(', b'-'] {
            assert!(
                !CwAmountGrammar::class_accepts(c),
                "{} must NOT be a candidate",
                c as char
            );
        }
        // '-' is handled separately (index-0 only), never by the mask
        assert!(!CwAmountGrammar::class_accepts(b'-'));
        // exactly four characters, no more
        let accepted: Vec<u8> = (0u8..=255).filter(|&c| CwAmountGrammar::class_accepts(c)).collect();
        assert_eq!(accepted, vec![b'+', b'.', b'E', b'e']);
    }

    /// Two lanes: digit-only is exact integer, anything else goes to `%lf`.
    #[test]
    fn lanes_match_the_vendor_branch() {
        for tok in ["0", "1", "125000", "-125000", "  42  "] {
            assert_eq!(
                CwAmountGrammar::lane(tok),
                CwAmountLane::Integer,
                "{tok:?} must use the integer lane"
            );
        }
        for tok in ["125000.00", "111.11", "123.4", "125000.5", "-0.75", "1e5", "+5"] {
            assert_eq!(
                CwAmountGrammar::lane(tok),
                CwAmountLane::Double,
                "{tok:?} must use the sscanf(\"%lf\") lane"
            );
        }
        assert_eq!(CwAmountGrammar::lane(""), CwAmountLane::Empty);
        assert_eq!(CwAmountGrammar::lane("   "), CwAmountLane::Empty);
    }

    /// The i64 overflow gate sends an over-long digit run to the double lane.
    #[test]
    fn overflow_gate_switches_lane() {
        // a digit run that trips `acc >= gate` -> double. 19 nines exceeds the
        // positive gate (0x1999999999999999 = 1844674407370955161).
        let over = "9".repeat(19);
        assert_eq!(CwAmountGrammar::lane(&over), CwAmountLane::Double);
        // 18 nines stays inside -> integer
        let under = "9".repeat(18);
        assert_eq!(CwAmountGrammar::lane(&under), CwAmountLane::Integer);
        // comfortably inside -> integer
        assert_eq!(CwAmountGrammar::lane("12345"), CwAmountLane::Integer);
        // negative gate is the smaller magnitude
        assert!(CwAmountGrammar::GATE_NEGATIVE < CwAmountGrammar::GATE_POSITIVE);
    }

    /// The vendor's double lane explains the `111.11` divergence without making
    /// it a law: `Cents::parse` stays exact, and the two differ only for tokens
    /// CW routes through floating point.
    #[test]
    fn exact_cents_diverges_only_on_the_double_lane() {
        // integer lane: vendor and OpenCW agree exactly
        for tok in ["125000", "-125000", "0"] {
            assert!(CwAmountGrammar::is_exact_integer(tok));
            assert_eq!(
                Cents::parse(tok).unwrap().0,
                tok.trim().parse::<i64>().unwrap() * 100
            );
        }
        // double lane: OpenCW is exact where the vendor is not
        for (tok, cents) in [("111.11", 11_111i64), ("125000.00", 12_500_000), ("-0.75", -75)] {
            assert!(!CwAmountGrammar::is_exact_integer(tok));
            assert_eq!(Cents::parse(tok).unwrap().0, cents, "{tok} must be exact");
        }
    }
}
