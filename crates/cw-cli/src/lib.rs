//! `cw-cli` library - the OpenCW interface shell (P5.I I1 + I2).
//!
//! A thin, deterministic front end over the Rust kernel: it opens one expanded
//! engagement directory (the `<stem>AM.dbf` ... table family), and renders the
//! CaseWare surfaces this lane owns:
//!
//! * **TB grid** - `cw_app::tb::TbView` over the BL cube, with account titles
//!   joined from `AM` (presentation only; the view itself never writes).
//! * **Document manager** - `cw_app::documents::DocumentBook` tree/list/census.
//! * **Dialogs parity** - CaseWare dialog surfaces (account properties, the
//!   renumber cascade, grouping assignment preview, document properties) built
//!   from the same decoded models the editors write through.
//! * **Editing paths** - account properties/description, balanced AJE entries,
//!   grouping assignment/clear and the renumber cascade, all through the same
//!   typed writers the kernel gates already cover.
//!
//! Read surfaces never write. Edit paths follow preview-by-default, commit on
//! `--write`, and land through `cw_core::transact` (write-aside -> fsync ->
//! atomic rename).

use std::path::{Path, PathBuf};

pub mod docman;
pub mod web;

use cw_app::accounts::{
    group_slot, write_cell, AccountProperties, AmAccount, Chart, Flag, PropertiesPatch, Side,
    PROPERTY_COLUMNS,
};
use cw_app::adjustments::{AdjustmentEntry, AdjustmentLine, Adjustments, EntryType};
use cw_app::documents::{DocumentKind, ShDocument};
use cw_app::groups::{self, MpTable, Scheme};
use cw_app::renumber::{self, CascadeIssue, CascadeReport, RenumberPlan, RenumberScope};
use cw_app::tb::{Compare, TbColumnKey, TbOptions, TbRow, TbView};
use cw_core::cube::Cube;
use cw_core::dbf::Table;
use cw_core::{transact, CoreError, Result};

// ---------------------------------------------------------------------------
// Engagement handle
// ---------------------------------------------------------------------------

/// One expanded engagement directory plus its table stem.
#[derive(Clone, Debug)]
pub struct Engagement {
    pub dir: PathBuf,
    pub stem: String,
}

impl Engagement {
    /// Open a directory by discovering the `*AM.dbf` stem.
    pub fn open(dir: impl AsRef<Path>) -> Result<Engagement> {
        let dir = dir.as_ref().to_path_buf();
        let stem = detect_stem(&dir)?;
        Ok(Engagement { dir, stem })
    }

    /// Path of `<dir>\<stem><suffix>.dbf` (case-insensitive suffix match).
    pub fn table_path(&self, suffix: &str) -> Result<PathBuf> {
        let want = format!("{}{}.dbf", self.stem, suffix).to_ascii_lowercase();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.to_ascii_lowercase() == want {
                    return Ok(entry.path());
                }
            }
        }
        Err(CoreError::Format(format!(
            "missing {}{suffix}.dbf in {}",
            self.stem,
            self.dir.display()
        )))
    }

    /// Parse one table by suffix (`AM`, `MP`, `SH`, `BL`, ...).
    pub fn table(&self, suffix: &str) -> Result<Table> {
        Table::parse_path(self.table_path(suffix)?)
    }

    /// One-line engagement identity used as the shell header.
    pub fn header(&self) -> String {
        format!(
            "OpenCW {} | engagement: {} | {}",
            env!("CARGO_PKG_VERSION"),
            self.stem,
            self.dir.display()
        )
    }

    /// Dashboard / `open`: census of the kernel surfaces the shell fronts.
    pub fn dashboard(&self) -> Result<String> {
        let mut out = String::new();
        out.push_str(&self.header());
        out.push('\n');
        out.push_str(&"-".repeat(self.header().len().min(100)));
        out.push('\n');

        match Chart::from_table(&self.table("AM")?) {
            Ok(chart) => {
                let posting = chart.posting().count();
                out.push_str(&format!(
                    "  AM chart        : {:>6} rows / {:>6} posting accounts\n",
                    chart.len(),
                    posting
                ));
            }
            Err(e) => out.push_str(&format!("  AM chart        : {e}\n")),
        }
        match MpTable::from_table(&self.table("MP")?) {
            Ok(mp) => {
                let nodes: usize = mp.schemes().map(|s| s.len()).sum();
                out.push_str(&format!(
                    "  MP groupings    : {:>6} schemes / {:>5} nodes / {:>4} unassigned\n",
                    mp.schemes().count(),
                    nodes,
                    mp.unassigned().len()
                ));
            }
            Err(e) => out.push_str(&format!("  MP groupings    : {e}\n")),
        }
        match crate::docman::open_book(&self.dir, &self.stem) {
            Ok(book) => {
                let c = book.census();
                out.push_str(&format!(
                    "  SH documents    : {:>6} live / {:>6} deleted / {:>3} forms / {:>3} headings\n",
                    c.live, c.deleted, c.forms, c.headings
                ));
            }
            Err(e) => out.push_str(&format!("  SH documents    : {e}\n")),
        }
        match self.table("BL").and_then(|t| Cube::from_table(&t)) {
            Ok(cube) => out.push_str(&format!("  BL balance cube : {:>6} rows\n", cube.len())),
            Err(e) => out.push_str(&format!("  BL balance cube : {e}\n")),
        }
        out.push_str(
            "\n  Commands: tb | docs [tree|list|census] | account <AC_NO> | dialog <id> | dialogs\n           edit account|entry|grouping ... (preview; add --write)\n",
        );
        Ok(out)
    }
}

/// Locate `<stem>AM.dbf` and return the stem. Sorted pick keeps it stable.
pub fn detect_stem(dir: &Path) -> Result<String> {
    let mut hits: Vec<String> = Vec::new();
    let rd = std::fs::read_dir(dir).map_err(|e| {
        CoreError::Format(format!("cannot read engagement dir {}: {e}", dir.display()))
    })?;
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // macOS writes `._<name>` resource-fork companions beside files on
        // non-native volumes (exFAT/NTFS/SMB). They carry the same suffix but
        // are never engagement tables, and they sort *before* the real stem
        // ('._' < 'A'), so a copy of an engagement can otherwise be detected as
        // `._<stem>`.
        if name.starts_with('.') {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        if lower.ends_with("am.dbf") && name.len() > 6 {
            hits.push(name[..name.len() - 6].to_string());
        }
    }
    hits.sort();
    hits.into_iter().next().ok_or_else(|| {
        CoreError::Format(format!(
            "no *AM.dbf table in {} (expanded engagement expected)",
            dir.display()
        ))
    })
}

// ---------------------------------------------------------------------------
// TB grid
// ---------------------------------------------------------------------------

/// Shell request for one trial-balance render.
#[derive(Clone, Debug)]
pub struct TbRequest {
    pub year: i32,
    pub period: u8,
    pub group: String,
    pub entity: Option<String>,
    pub compare: Compare,
    pub synthetics: bool,
    /// Max account rows before a truncation footer (`0` = all).
    pub limit: usize,
}

impl Default for TbRequest {
    fn default() -> Self {
        TbRequest {
            year: 0,
            period: 12,
            group: "A".to_string(),
            entity: None,
            compare: Compare::Auto,
            synthetics: false,
            limit: 0,
        }
    }
}

impl TbRequest {
    fn options(&self) -> Result<TbOptions> {
        TbOptions {
            year: self.year,
            period: self.period,
            group: self.group.clone(),
            entity: self.entity.clone(),
            compare: self.compare,
            include_synthetics: self.synthetics,
        }
        .validate()
    }
}

/// Resolve an account by number, tolerating DBF right-justification.
///
/// `AM.AC_NO` (like `BL.NUMBER`) is space-padded on the left in the corpora;
/// CaseWare's own views compare the trimmed value, so the shell does too.
pub fn find_account<'a>(chart: &'a Chart, ac_no: &str) -> Option<&'a AmAccount> {
    let want = ac_no.trim();
    chart
        .posting()
        .find(|a| a.ac_no.trim() == want)
        .or_else(|| chart.accounts().iter().find(|a| a.ac_no.trim() == want))
}

/// Public helper: does this account number resolve in the chart?
pub fn account_exists(chart: &Chart, ac_no: &str) -> bool {
    find_account(chart, ac_no).is_some()
}

/// Render the TB grid (account number + AM description + view columns).
pub fn render_tb(eng: &Engagement, req: &TbRequest) -> Result<String> {
    let opts = req.options()?;
    let cube = Cube::from_table(&eng.table("BL")?)?;
    let view = TbView::from_cube(&cube, &opts)?;
    // Titles are presentation only; a missing/drifted AM must not hide the grid.
    let chart = Chart::from_table(&eng.table("AM")?).ok();
    Ok(render_tb_view(eng, &view, chart.as_ref(), req.limit))
}

fn render_tb_view(
    eng: &Engagement,
    view: &TbView,
    chart: Option<&Chart>,
    limit: usize,
) -> String {
    let cols = view.columns();
    let num_w = 10usize;
    let desc_w = 30usize;
    let mut widths: Vec<usize> = cols
        .iter()
        .map(|c| c.label.len().max(12))
        .collect::<Vec<_>>();

    let row_values = |r: &TbRow| -> Vec<String> {
        cols.iter()
            .map(|c| fmt_money(col_value(r, c.key)))
            .collect()
    };
    let totals = view.totals();
    let total_values: Vec<String> = cols
        .iter()
        .map(|c| fmt_money(totals_value(&totals, c.key)))
        .collect();
    for (i, v) in total_values.iter().enumerate() {
        widths[i] = widths[i].max(v.len());
    }

    let mut out = String::new();
    out.push_str(&eng.header());
    out.push('\n');
    out.push_str(&format!(
        "Trial Balance | Y{} P{} | basis {} | entity {} | compare {}\n",
        view.year,
        view.period,
        view.group,
        view.entity.as_deref().unwrap_or("<all>"),
        match view.prior_year {
            Some(y) => format!("Y{y}"),
            None => "off".to_string(),
        }
    ));

    // Header
    out.push_str(&format!("{:<num_w$}  {:<desc_w$}", "ACCT", "DESCRIPTION"));
    for (c, w) in cols.iter().zip(widths.iter()) {
        out.push_str(&format!(" {:>w$}", c.label, w = *w));
    }
    out.push('\n');
    let rule = "-".repeat(
        num_w + 2 + desc_w + widths.iter().map(|w| 1 + w).sum::<usize>(),
    );
    out.push_str(&rule);
    out.push('\n');

    let shown = if limit > 0 {
        view.rows().len().min(limit)
    } else {
        view.rows().len()
    };
    for r in &view.rows()[..shown] {
        let desc = chart
            .and_then(|c| find_account(c, &r.number))
            .map(|a| a.desc.clone())
            .unwrap_or_default();
        out.push_str(&format!(
            "{:<num_w$}  {:<desc_w$}",
            r.number,
            truncate(&desc, desc_w)
        ));
        for (v, w) in row_values(r).iter().zip(widths.iter()) {
            out.push_str(&format!(" {:>w$}", v, w = *w));
        }
        out.push('\n');
    }
    if shown < view.rows().len() {
        out.push_str(&format!(
            "  ... {} more rows (raise --limit)\n",
            view.rows().len() - shown
        ));
    }
    out.push_str(&rule);
    out.push('\n');
    out.push_str(&format!(
        "{:<num_w$}  {:<desc_w$}",
        "TOTAL",
        format!("{} rows", view.rows().len())
    ));
    for (v, w) in total_values.iter().zip(widths.iter()) {
        out.push_str(&format!(" {:>w$}", v, w = *w));
    }
    out.push('\n');
    out
}

fn col_value(r: &TbRow, key: TbColumnKey) -> f64 {
    match key {
        TbColumnKey::Opening => r.opening,
        TbColumnKey::Period => r.period,
        TbColumnKey::YtdMovement => r.ytd_movement,
        TbColumnKey::Ytd => r.ytd,
        TbColumnKey::PriorOpening => r.prior_opening,
        TbColumnKey::PriorPeriod => r.prior_period,
        TbColumnKey::PriorYtd => r.prior_ytd,
        TbColumnKey::VariancePeriod => r.variance_period,
        TbColumnKey::VarianceYtd => r.variance_ytd,
    }
}

fn totals_value(t: &cw_app::tb::TbTotals, key: TbColumnKey) -> f64 {
    match key {
        TbColumnKey::Opening => t.opening,
        TbColumnKey::Period => t.period,
        TbColumnKey::YtdMovement => t.ytd_movement,
        TbColumnKey::Ytd => t.ytd,
        TbColumnKey::PriorOpening => t.prior_opening,
        TbColumnKey::PriorPeriod => t.prior_period,
        TbColumnKey::PriorYtd => t.prior_ytd,
        TbColumnKey::VariancePeriod => t.variance_period,
        TbColumnKey::VarianceYtd => t.variance_ytd,
    }
}

/// TB grid as JSON (rows + totals; account titles joined when AM decodes).
pub fn tb_json(eng: &Engagement, req: &TbRequest) -> Result<String> {
    tb_json_impl(eng, req, 0, 0, false)
}

/// TB grid as JSON over a row window, plus the unpaged row count.
///
/// The web shell (`GET /api/tb?offset=&limit=`) needs both halves of one view;
/// the plain [`tb_json`] output stays byte-identical (no extra keys).
pub fn tb_json_window(
    eng: &Engagement,
    req: &TbRequest,
    offset: usize,
    limit: usize,
) -> Result<String> {
    tb_json_impl(eng, req, offset, limit, true)
}

fn tb_json_impl(
    eng: &Engagement,
    req: &TbRequest,
    offset: usize,
    limit: usize,
    paged: bool,
) -> Result<String> {
    let opts = req.options()?;
    let cube = Cube::from_table(&eng.table("BL")?)?;
    let view = TbView::from_cube(&cube, &opts)?;
    let chart = Chart::from_table(&eng.table("AM")?).ok();
    let cols = view.columns();
    let mut out = String::from("{");
    out.push_str(&format!(
        "\"engagement\":{},\"year\":{},\"period\":{},\"group\":{},\"entity\":{},\"prior_year\":{},",
        json_str(&eng.stem),
        view.year,
        view.period,
        json_str(&view.group),
        match view.entity.as_deref() {
            Some(e) => json_str(e),
            None => "null".to_string(),
        },
        match view.prior_year {
            Some(y) => y.to_string(),
            None => "null".to_string(),
        }
    ));
    out.push_str("\"columns\":[");
    for (i, c) in cols.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!("\"{}\"", json_escape(&c.label)));
    }
    out.push_str("],\"rows\":[");
    let all = view.rows();
    // The window is the whole view unless a caller asked for a page; `total`
    // is added only for paged callers so `tb_json` stays byte-stable.
    let start = if paged { offset.min(all.len()) } else { 0 };
    let end = if paged {
        match limit {
            0 => all.len(),
            n => (start + n).min(all.len()),
        }
    } else {
        all.len()
    };
    for (i, r) in all[start..end].iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let desc = chart
            .as_ref()
            .and_then(|c| find_account(c, &r.number))
            .map(|a| a.desc.clone())
            .unwrap_or_default();
        out.push_str(&format!(
            "{{\"acct\":{},\"desc\":{},\"opening\":{:.2},\"period\":{:.2},\"ytd_movement\":{:.2},\"ytd\":{:.2},\"prior_opening\":{:.2},\"prior_period\":{:.2},\"prior_ytd\":{:.2},\"variance_period\":{:.2},\"variance_ytd\":{:.2}}}",
            json_str(&r.number),
            json_str(&desc),
            r.opening,
            r.period,
            r.ytd_movement,
            r.ytd,
            r.prior_opening,
            r.prior_period,
            r.prior_ytd,
            r.variance_period,
            r.variance_ytd
        ));
    }
    let t = view.totals();
    out.push_str(&format!(
        "],\"totals\":{{\"opening\":{:.2},\"period\":{:.2},\"ytd_movement\":{:.2},\"ytd\":{:.2},\"prior_opening\":{:.2},\"prior_period\":{:.2},\"prior_ytd\":{:.2},\"variance_period\":{:.2},\"variance_ytd\":{:.2}}}",
        t.opening,
        t.period,
        t.ytd_movement,
        t.ytd,
        t.prior_opening,
        t.prior_period,
        t.prior_ytd,
        t.variance_period,
        t.variance_ytd
    ));
    if paged {
        out.push_str(&format!(",\"total\":{},\"offset\":{}", all.len(), start));
    }
    out.push('}');
    Ok(out)
}

// ---------------------------------------------------------------------------
// Document manager
// ---------------------------------------------------------------------------

/// Document-manager presentation modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocsMode {
    Tree,
    List,
    Census,
}

impl DocsMode {
    pub fn parse(s: &str) -> Result<DocsMode> {
        match s {
            "tree" | "" => Ok(DocsMode::Tree),
            "list" => Ok(DocsMode::List),
            "census" | "stats" => Ok(DocsMode::Census),
            other => Err(CoreError::Format(format!(
                "docs mode {other:?} is not tree|list|census"
            ))),
        }
    }
}

/// Render the document manager.
pub fn render_docs(eng: &Engagement, mode: DocsMode, limit: usize) -> Result<String> {
    let book = crate::docman::open_book(&eng.dir, &eng.stem)?;
    let mut out = String::new();
    out.push_str(&eng.header());
    out.push('\n');
    match mode {
        DocsMode::Tree => {
            out.push_str(&format!(
                "Document Manager | tree | {} live rows\n",
                book.count()
            ));
            let mut shown = 0usize;
            for node in book.tree() {
                render_node(&node, &mut out, &mut shown, limit);
            }
            if limit > 0 && shown >= limit {
                out.push_str("  ... tree truncated (raise --limit)\n");
            }
        }
        DocsMode::List => {
            out.push_str(&format!(
                "Document Manager | list | {} live rows\n",
                book.count()
            ));
            let rows = book.preorder();
            let shown = if limit > 0 {
                rows.len().min(limit)
            } else {
                rows.len()
            };
            out.push_str(&format!(
                "{:<7} {:<4} {:<3} {:<10} {}\n",
                "LINENO", "PRG", "KND", "SCHEDULE", "TITLE"
            ));
            for (depth, doc) in &rows[..shown] {
                out.push_str(&format!(
                    "{:<7} {:<4} {:<3} {:<10} {}{}\n",
                    doc.lineno
                        .map(|l| l.to_string())
                        .unwrap_or_else(|| "-".into()),
                    doc.prg,
                    doc.kind.letter(),
                    doc.schedule,
                    "  ".repeat(*depth),
                    doc.title
                ));
            }
            if shown < rows.len() {
                out.push_str(&format!("  ... {} more rows\n", rows.len() - shown));
            }
        }
        DocsMode::Census => {
            let c = book.census();
            out.push_str("Document Manager | census\n");
            out.push_str(&format!("  live rows      : {}\n", c.live));
            out.push_str(&format!("  deleted rows   : {}\n", c.deleted));
            out.push_str(&format!("  headings       : {}\n", c.headings));
            out.push_str(&format!("  forms          : {}\n", c.forms));
            out.push_str(&format!("  analyses       : {}\n", c.analyses));
            out.push_str(&format!("  index rows     : {}\n", c.index_rows));
            out.push_str(&format!("  roots / parent : {} / {}\n", c.roots, c.with_parent));
            out.push_str(&format!(
                "  archived/rolled/locked : {} / {} / {}\n",
                c.archived, c.rolled, c.locked
            ));
            out.push_str(&format!("  blank LINENO   : {}\n", c.blank_lineno));
            out.push_str(&format!("  cycles         : {}\n", c.cycles));
            out.push_str("  by type        : ");
            let types: Vec<String> = c
                .by_type
                .iter()
                .map(|(k, n)| {
                    let label = if *k == ' ' { "blank".to_string() } else { k.to_string() };
                    format!("{label}={n}")
                })
                .collect();
            out.push_str(&types.join(" "));
            out.push('\n');
            out.push_str("  by PRG         : ");
            let prgs: Vec<String> = c
                .by_prg
                .iter()
                .map(|(k, n)| format!("{k}={n}"))
                .collect();
            out.push_str(&prgs.join(" "));
            out.push('\n');
        }
    }
    Ok(out)
}

fn render_node(
    node: &cw_app::documents::DocumentNode,
    out: &mut String,
    shown: &mut usize,
    limit: usize,
) {
    if limit > 0 && *shown >= limit {
        return;
    }
    *shown += 1;
    out.push_str(&format!(
        "{}[{}] {:<10} {}\n",
        "  ".repeat(node.depth),
        node.kind.letter(),
        node.schedule,
        if node.title.is_empty() {
            &node.guid
        } else {
            &node.title
        }
    ));
    for child in &node.children {
        render_node(child, out, shown, limit);
    }
}

/// Document manager census as JSON.
pub fn docs_json(eng: &Engagement, mode: DocsMode, limit: usize) -> Result<String> {
    let book = crate::docman::open_book(&eng.dir, &eng.stem)?;
    let mut out = String::from("{");
    out.push_str(&format!("\"engagement\":{},", json_str(&eng.stem)));
    match mode {
        DocsMode::Census => {
            let c = book.census();
            out.push_str(&format!(
                "\"mode\":\"census\",\"live\":{},\"deleted\":{},\"headings\":{},\"forms\":{},\"analyses\":{},\"index_rows\":{},\"roots\":{},\"with_parent\":{},\"archived\":{},\"rolled\":{},\"locked\":{},\"blank_lineno\":{},\"cycles\":{},\"by_type\":{{",
                c.live,
                c.deleted,
                c.headings,
                c.forms,
                c.analyses,
                c.index_rows,
                c.roots,
                c.with_parent,
                c.archived,
                c.rolled,
                c.locked,
                c.blank_lineno,
                c.cycles
            ));
            for (i, (k, n)) in c.by_type.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&format!("{}:{}", json_str(&k.to_string()), n));
            }
            out.push_str("},\"by_prg\":{");
            for (i, (k, n)) in c.by_prg.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&format!("{}:{}", json_str(k), n));
            }
            out.push_str("}}");
        }
        mode => {
            out.push_str(&format!(
                "\"mode\":\"{}\",\"rows\":[",
                if mode == DocsMode::Tree { "tree" } else { "list" }
            ));
            let rows = book.preorder();
            let shown = if limit > 0 {
                rows.len().min(limit)
            } else {
                rows.len()
            };
            for (i, (depth, d)) in rows[..shown].iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&format!(
                    "{{\"depth\":{},\"lineno\":{},\"prg\":{},\"kind\":{},\"schedule\":{},\"title\":{},\"guid\":{}}}",
                    depth,
                    match d.lineno {
                        Some(l) => l.to_string(),
                        None => "null".to_string(),
                    },
                    json_str(&d.prg),
                    json_str(&d.kind.letter().to_string()),
                    json_str(&d.schedule),
                    json_str(&d.title),
                    json_str(&d.cwguid)
                ));
            }
            out.push_str("]}");
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Dialogs parity (read-only surfaces; I2 adds the edit paths)
// ---------------------------------------------------------------------------

/// Account properties dialog, in `PROPERTY_COLUMNS` order.
pub fn render_account_dialog(eng: &Engagement, ac_no: &str) -> Result<String> {
    let chart = Chart::from_table(&eng.table("AM")?)?;
    let acct = find_account(&chart, ac_no).ok_or_else(|| {
        CoreError::Format(format!("account {ac_no:?} not found in AM posting index"))
    })?;
    Ok(render_account(eng, acct))
}

fn render_account(eng: &Engagement, a: &AmAccount) -> String {
    let mut out = String::new();
    out.push_str(&eng.header());
    out.push('\n');
    out.push_str(&format!(
        "Account Properties | {} {} | dialog order per specs\\accounts.md\n",
        a.ac_no.trim(),
        a.desc
    ));
    out.push_str(&format!(
        "  Identity : type {} class {} schedule {} entity {} div {} sub {}\n",
        a.kind.label(),
        a.class,
        if a.schedule.is_empty() { "-" } else { &a.schedule },
        if a.entity.is_empty() { "<blank>" } else { &a.entity },
        if a.div.is_empty() { "-" } else { &a.div },
        if a.sub.is_empty() { "-" } else { &a.sub },
    ));
    out.push_str(&format!(
        "  GUID     : {}  line {}\n",
        a.cwguid, a.line_no
    ));
    out.push_str("  Grouping : ");
    let mut grouped = Vec::new();
    for digit in [0u8, 2, 3, 4, 5, 6, 7, 8, 9] {
        if let Some(slot) = group_slot(digit) {
            if !a.groups[slot].is_empty() {
                grouped.push(format!("{digit}:{}", a.groups[slot].trim()));
            }
        }
    }
    let grouping = if grouped.is_empty() {
        "(unassigned)".to_string()
    } else {
        grouped.join(" ")
    };
    out.push_str(&grouping);
    out.push('\n');
    out.push_str("  Properties (dialog order):\n");
    for (name, value) in property_pairs(a) {
        out.push_str(&format!("    {:<10} : {}\n", name, value));
    }
    out.push_str(&format!(
        "  Balances : open {:.2} | mth {:.2} | ytd {:.2} | AJE {:.2} | EJE {:.2} | RJE {:.2}\n",
        a.open, a.cur_mth, a.cur_ytd, a.aje_bal, a.eje_bal, a.rje_bal
    ));
    out.push_str("  Surface  : account edit -> `edit account <AC_NO> --set COL=VAL --write`\n");
    out
}

/// `PROPERTY_COLUMNS`-ordered (column, rendered value) pairs.
pub fn property_pairs(a: &AmAccount) -> Vec<(&'static str, String)> {
    let p: &AccountProperties = &a.props;
    let mut vals: Vec<(&'static str, String)> = vec![
        ("POST", p.post.raw().to_string()),
        ("PRINT", p.print.raw().to_string()),
        ("CTL", p.ctl.clone()),
        ("TOTAL", p.total.raw().to_string()),
        ("SIGN", p.sign.as_str().to_string()),
        ("DC", p.dc.as_str().to_string()),
        ("COLUMN", p.column.raw().to_string()),
        ("DOLLAR", p.dollar.raw().to_string()),
        ("ULINE", p.uline.raw().to_string()),
        ("PAGE", p.page.raw().to_string()),
        ("DEN", p.den.clone()),
        ("HIDDEN", p.hidden.raw().to_string()),
    ];
    debug_assert_eq!(vals.len(), PROPERTY_COLUMNS.len());
    vals.sort_by_key(|(name, _)| {
        PROPERTY_COLUMNS
            .iter()
            .position(|c| c == name)
            .unwrap_or(usize::MAX)
    });
    vals
}

/// Account properties dialog as JSON.
pub fn account_json(eng: &Engagement, ac_no: &str) -> Result<String> {
    let chart = Chart::from_table(&eng.table("AM")?)?;
    let a = find_account(&chart, ac_no).ok_or_else(|| {
        CoreError::Format(format!("account {ac_no:?} not found in AM posting index"))
    })?;
    let mut out = String::from("{");
    out.push_str(&format!(
        "\"engagement\":{},\"acct\":{},\"desc\":{},\"type\":{},\"class\":{},\"schedule\":{},\"entity\":{},\"guid\":{},\"open\":{:.2},\"mth\":{:.2},\"ytd\":{:.2},\"aje\":{:.2},\"eje\":{:.2},\"rje\":{:.2},\"properties\":{{",
        json_str(&eng.stem),
        json_str(a.ac_no.trim()),
        json_str(&a.desc),
        json_str(a.kind.label()),
        json_str(&a.class),
        json_str(&a.schedule),
        json_str(&a.entity),
        json_str(&a.cwguid),
        a.open,
        a.cur_mth,
        a.cur_ytd,
        a.aje_bal,
        a.eje_bal,
        a.rje_bal
    ));
    for (i, (name, value)) in property_pairs(a).iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!("{}:{}", json_str(name), json_str(value)));
    }
    out.push_str("},\"groups\":[");
    let mut first = true;
    for digit in [0u8, 2, 3, 4, 5, 6, 7, 8, 9] {
        if let Some(slot) = group_slot(digit) {
            if a.groups[slot].is_empty() {
                continue;
            }
            if !first {
                out.push(',');
            }
            first = false;
            out.push_str(&format!(
                "{{\"scheme\":{},\"node\":{}}}",
                digit,
                json_str(a.groups[slot].trim())
            ));
        }
    }
    out.push_str("]}");
    Ok(out)
}

/// The dialog catalogue the shell implements (parity index).
pub fn dialog_catalog() -> String {
    let rows = [
        ("open-engagement", "open", "engagement dashboard/census", "kernel: AM/MP/SH/BL"),
        ("trial-balance", "tb", "TB grid (period/YTD/compare)", "cw_app::tb"),
        ("document-manager", "docs", "tree / list / census", "cw_app::documents"),
        ("account-properties", "account <AC_NO>", "12 property fields, dialog order", "cw_app::accounts"),
        ("renumber-accounts", "dialog renumber --delta N [--write]", "renumber cascade AM/BL/GL (preview|commit)", "cw_app::renumber"),
        ("assign-grouping", "dialog assign-grouping <scheme>", "scheme tree + assigned counts", "MP + AM.GROUPn"),
        ("document-properties", "dialog document <guid|schedule>", "SH row fields", "cw_app::documents"),
        ("edit-account", "edit account <AC_NO> [--set C=V] --write", "description + 12 property fields", "AM writer"),
        ("edit-entry", "edit entry --ref ID --line ACCT=AMT --write", "balanced AJE create/replace", "GL writer"),
        ("edit-grouping", "edit grouping <scheme> <AC_NO> --node CODE --write", "AM.GROUPn assign/clear", "MP + AM.GROUPn"),
    ];
    let mut out = String::from("CaseWare dialog parity map (I1 read surfaces + I2 edit paths)\n");
    out.push_str(&format!(
        "  {:<20} {:<38} {:<32} {}\n",
        "DIALOG", "SHELL COMMAND", "SURFACE", "KERNEL SOURCE"
    ));
    for (id, cmd, surface, source) in rows {
        out.push_str(&format!(
            "  {:<20} {:<38} {:<32} {}\n",
            id, cmd, surface, source
        ));
    }
    out.push_str("\n  Write paths: edit account|entry|grouping ... --write (atomic table commit).\n");
    out
}

/// Load the three cascade tables plus the `MP` map space of one engagement.
///
/// A **missing** `MP` is tolerated (the cascade then cannot see map-code
/// collisions and says so in the report); a `MP` that exists but does not parse
/// is an error, because skipping it would silently drop the map-space check.
pub fn renumber_scope(eng: &Engagement) -> Result<RenumberScope> {
    let scope = RenumberScope::new(eng.table("AM")?, eng.table("BL")?, eng.table("GL")?);
    match eng.table_path("MP") {
        Ok(path) => Ok(scope.with_mp(Table::parse_path(path)?)),
        Err(_) => Ok(scope),
    }
}

/// Renumber-accounts dialog: the real cascade, preview by default.
///
/// Every `AM.AC_NO`/`AC_NO2`/`AC_NO2CR`/`PAC_NO`, `BL.NUMBER` and
/// `GL.AC_NO`/`B_ACT` cell that carries a renamed account is reported as an
/// exact before/after pair per table, with the tombstone, skipped-number and
/// dangler audit underneath. `write=false` renders the preview only; `--write`
/// commits each changed table through `cw_core::transact` (atomic rename), and a
/// cascade that changes nothing touches no file at all.
pub fn renumber_dialog(
    eng: &Engagement,
    plan: &RenumberPlan,
    limit: usize,
    write: bool,
) -> Result<RenumberOutcome> {
    let scope = renumber_scope(eng)?;

    // Validate before touching a byte: a blocked plan is *reported* by the dialog
    // (it is a user-facing surface, not a library call) and writes nothing.
    let resolution = plan.resolve(&scope)?;
    if resolution.is_blocked() {
        return Ok(RenumberOutcome {
            report: CascadeReport::default(),
            changed_tables: vec![("AM", false), ("BL", false), ("GL", false)],
            written: Vec::new(),
            limit,
            plan: plan.clone(),
            issues: resolution.issues,
        });
    }

    let preview = renumber::cascade(&scope, plan)?;
    let changed_tables = preview.changed_tables(&scope);

    let mut written: Vec<&'static str> = Vec::new();
    if write {
        for (suffix, changed) in &changed_tables {
            if !changed {
                continue;
            }
            let path = eng.table_path(suffix)?;
            let table = match *suffix {
                "AM" => &preview.am,
                "BL" => &preview.bl,
                _ => &preview.gl,
            };
            transact::commit_if_changed(&path, &table.encode())?;
            written.push(suffix);
        }
    }

    Ok(RenumberOutcome {
        report: preview.report,
        changed_tables,
        written,
        limit,
        plan: plan.clone(),
        issues: resolution.issues,
    })
}

/// Result of a renumber dialog run.
#[derive(Clone, Debug)]
pub struct RenumberOutcome {
    pub report: CascadeReport,
    /// `(table, changed)` for AM/BL/GL in commit order.
    pub changed_tables: Vec<(&'static str, bool)>,
    pub written: Vec<&'static str>,
    pub limit: usize,
    pub plan: RenumberPlan,
    /// Every reason the cascade was refused (empty when it ran).
    pub issues: Vec<CascadeIssue>,
}

impl RenumberOutcome {
    pub fn changed(&self) -> bool {
        self.changed_tables.iter().any(|(_, c)| *c)
    }

    pub fn written_any(&self) -> bool {
        !self.written.is_empty()
    }

    /// The plan was refused; nothing ran and nothing was written.
    pub fn blocked(&self) -> bool {
        !self.issues.is_empty()
    }

    /// Render the dialog report.
    pub fn render(&self, header: &str, label: &str) -> String {
        let mut out = String::new();
        out.push_str(header);
        out.push('\n');
        out.push_str(&format!("Renumber Accounts | {label}\n"));

        let renames = self.plan.renames();
        out.push_str(&format!(
            "  {} renames proposed, {} cell edits across {} tables\n",
            renames.len(),
            self.report.cells(),
            self.report.table_stats().len()
        ));
        if !renames.is_empty() {
            let cap = if self.limit > 0 { self.limit } else { 40 };
            out.push_str(&format!("  {:<10} {:<10}\n", "CURRENT", "PROPOSED"));
            for r in renames.iter().take(cap) {
                out.push_str(&format!("  {:<10} {:<10}\n", r.from, r.to));
            }
            if renames.len() > cap {
                out.push_str(&format!("  ... {} more\n", renames.len() - cap));
            }
        }

        // Exact before/after per table: the cascade evidence the dialog owns.
        for (table, cells, rows) in self.report.table_stats() {
            out.push_str(&format!("\n  {table}: {cells} cells in {rows} rows\n"));
            let cap = if self.limit > 0 { self.limit } else { 20 };
            let mut shown = 0usize;
            let mut last: Option<(usize, &str)> = None;
            for e in self.report.edits.iter().filter(|e| e.table == table) {
                if shown >= cap {
                    let total = self.report.table_cells(table);
                    out.push_str(&format!("    ... {} more cells\n", total - cap));
                    break;
                }
                if last != Some((e.row, e.column)) {
                    out.push_str(&format!(
                        "    row {:<5} {:<8} {:?} -> {:?}\n",
                        e.row, e.column, e.before, e.after
                    ));
                }
                last = Some((e.row, e.column));
                shown += 1;
            }
        }

        if !self.report.tombstones.is_empty() {
            out.push_str(&format!(
                "\n  tombstones preserved: {} rows carrying a renamed account\n",
                self.report.tombstones.len()
            ));
            for (table, row) in self.report.tombstones.iter().take(8) {
                out.push_str(&format!("    {table} row {row}\n"));
            }
            if self.report.tombstones.len() > 8 {
                out.push_str(&format!(
                    "    ... {} more\n",
                    self.report.tombstones.len() - 8
                ));
            }
        }
        if !self.plan.skipped().is_empty() {
            let cap = if self.limit > 0 { self.limit } else { 8 };
            let skipped = self.plan.skipped();
            out.push_str(&format!(
                "\n  not renumbered (non-numeric): {} account ids\n",
                skipped.len()
            ));
            for s in skipped.iter().take(cap) {
                out.push_str(&format!("    {s}\n"));
            }
            if skipped.len() > cap {
                out.push_str(&format!("    ... {} more\n", skipped.len() - cap));
            }
        }
        if !self.report.already_applied.is_empty() {
            out.push_str(&format!(
                "\n  already in effect: {}\n",
                self.report.already_applied.join(", ")
            ));
        }
        out.push_str(&format!(
            "\n  danglers: {} before, {} after\n",
            self.report.dangling_before.len(),
            self.report.dangling_after.len()
        ));

        if self.blocked() {
            out.push_str(&format!(
                "\n  REFUSED: {} blocking issue(s); no table was written\n",
                self.issues.len()
            ));
            for issue in self.issues.iter().take(if self.limit > 0 { self.limit } else { 20 }) {
                out.push_str(&format!("    - {issue}\n"));
            }
            if self.issues.len() > self.limit.max(20) {
                out.push_str(&format!("    ... {} more\n", self.issues.len() - self.limit.max(20)));
            }
        } else if !self.changed() {
            out.push_str("Result   : no change (cascade is already in effect)\n");
        } else if self.written_any() {
            out.push_str(&format!(
                "Result   : WROTE {} via atomic rename\n",
                self.written
                    .iter()
                    .map(|t| format!("{t}.dbf"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        } else {
            out.push_str("Result   : preview only (add --write to commit)\n");
        }
        out
    }
}

/// Renumber-accounts dialog preview (read-only convenience wrapper).
pub fn render_renumber(eng: &Engagement, delta: i64, limit: usize) -> Result<String> {
    let scope = renumber_scope(eng)?;
    let plan = RenumberPlan::from_delta(&scope, delta)?;
    let outcome = renumber_dialog(eng, &plan, limit, false)?;
    Ok(outcome.render(
        &eng.header(),
        &format!("delta {delta:+} | preview (atomic cascade over AM/BL/GL)"),
    ))
}

/// Assign-grouping dialog preview for one scheme.
pub fn render_grouping_dialog(
    eng: &Engagement,
    scheme_raw: &str,
    node: Option<&str>,
    limit: usize,
) -> Result<String> {
    let scheme = Scheme::parse(scheme_raw)?;
    let mp = MpTable::from_table(&eng.table("MP")?)?;
    let s = mp
        .scheme(scheme)
        .ok_or_else(|| CoreError::Format(format!("grouping scheme {scheme_raw:?} is empty/unknown")))?;
    let mut out = String::new();
    out.push_str(&eng.header());
    out.push('\n');
    out.push_str(&format!(
        "Assign Grouping | scheme {} ({}) | {} nodes | `edit grouping ... --node` writes\n",
        scheme_raw,
        s.scheme().label(),
        s.len()
    ));
    let nodes: Vec<&cw_app::groups::MapNode> = match node {
        Some(code) => {
            if s.get(code).is_none() {
                return Err(CoreError::Format(format!(
                    "grouping node {code:?} not found in scheme {scheme_raw}"
                )));
            }
            s.descendants(code)
        }
        None => s.walk(),
    };
    let cap = if limit > 0 { limit } else { 40 };
    let mut shown = 0usize;
    for n in nodes {
        if shown >= cap {
            break;
        }
        let depth = s.depth(&n.map_no);
        let title = if n.desc.is_empty() { &n.title_raw } else { &n.desc };
        out.push_str(&format!(
            "  {}{:<12} {:<10} {}\n",
            "  ".repeat(depth),
            n.map_no.trim(),
            n.kind.label(),
            truncate(title, 48)
        ));
        shown += 1;
    }
    if shown < s.len() {
        out.push_str(&format!("  ... {} more nodes\n", s.len() - shown));
    }
    // Assigned account counts for this scheme (AM.GROUPn -> MP node MAP_NO).
    if let Some(digit) = scheme.digit() {
        if let Some(slot) = group_slot(digit) {
            if let Ok(chart) = Chart::from_table(&eng.table("AM")?) {
                let assigned = chart
                    .posting()
                    .filter(|a| !a.groups[slot].is_empty())
                    .count();
                out.push_str(&format!(
                    "  assigned accounts in AM.GROUP{digit}: {assigned}\n"
                ));
            }
        }
    }
    Ok(out)
}

/// Document-properties dialog for one SH row (GUID or schedule).
pub fn render_document_dialog(eng: &Engagement, key: &str) -> Result<String> {
    let book = crate::docman::open_book(&eng.dir, &eng.stem)?;
    let doc: &ShDocument = book
        .resolve(key)
        .or_else(|| {
            book.by_schedule(key)
                .into_iter()
                .find(|d| d.kind.is_heading() || !d.title.is_empty())
        })
        .ok_or_else(|| CoreError::Format(format!("no SH document with GUID/schedule {key:?}")))?;
    let mut out = String::new();
    out.push_str(&eng.header());
    out.push('\n');
    out.push_str(&format!(
        "Document Properties | {} | {}\n",
        kind_label(doc.kind),
        doc.title
    ));
    out.push_str(&format!("  GUID      : {}\n", doc.cwguid));
    out.push_str(&format!("  Schedule  : {}\n", doc.schedule));
    out.push_str(&format!("  Name/PRG  : {} / {}\n", doc.name, doc.prg));
    out.push_str(&format!(
        "  LINENO    : {}\n",
        doc.lineno
            .map(|l| l.to_string())
            .unwrap_or_else(|| "<blank>".into())
    ));
    out.push_str(&format!("  CVID      : {}\n", doc.cvid));
    out.push_str(&format!("  Entity    : {}\n", doc.entity));
    out.push_str(&format!(
        "  Flags     : archive={} roll={} lockdown={} hidden={} delivered={} sign={}\n",
        yn(doc.archive),
        yn(doc.roll),
        yn(doc.lockdown),
        yn(doc.hidden),
        yn(doc.delivered),
        yn(doc.sign)
    ));
    out.push_str(&format!(
        "  Created   : {} | CW date {}\n",
        doc.created_on, doc.cw_date
    ));
    out.push_str("  Surface   : document editing remains residual (no SH writer yet)\n");
    Ok(out)
}

fn kind_label(kind: DocumentKind) -> &'static str {
    match kind {
        DocumentKind::Index => "index",
        DocumentKind::Heading => "heading",
        DocumentKind::Analysis => "analysis",
        DocumentKind::Form => "form",
        DocumentKind::Misc => "misc",
        DocumentKind::Star => "star",
        DocumentKind::Other(c) => match c {
            'S' => "schedule",
            'J' => "journal",
            'R' => "report",
            other => "lettered",
        },
    }
}

fn yn(b: bool) -> &'static str {
    if b {
        "Y"
    } else {
        "-"
    }
}

// ---------------------------------------------------------------------------
// Interactive editing paths (I2)
// ---------------------------------------------------------------------------
//
// Every path follows the same contract: decode kernel truth, apply the edit to
// an in-memory `Table` through the typed cw-app writer, and either preview
// (default) or commit the re-encoded table through `cw_core::transact`
// (write-aside -> fsync -> atomic rename). A failed validation leaves both the
// in-memory table and the engagement bytes untouched.

/// Outcome of one edit path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditOutcome {
    /// Surface name (`account`, `entry`, `grouping`).
    pub surface: &'static str,
    /// Table suffix the edit targets (`AM` / `GL`).
    pub table: String,
    /// Whether the edit changes any byte.
    pub changed: bool,
    /// Whether the change was committed to disk.
    pub written: bool,
    /// Human-readable change detail (before/after values).
    pub detail: Vec<String>,
}

impl EditOutcome {
    /// Render the outcome as the shell's edit report.
    pub fn render(&self, header: &str) -> String {
        let mut out = String::new();
        out.push_str(header);
        out.push('\n');
        out.push_str(&format!(
            "Table    : {}.dbf\n",
            self.table
        ));
        for line in &self.detail {
            out.push_str(&format!("  {line}\n"));
        }
        if !self.changed {
            out.push_str("Result   : no change (already at requested state)\n");
        } else if self.written {
            out.push_str("Result   : WROTE table via atomic rename\n");
        } else {
            out.push_str("Result   : preview only (add --write to commit)\n");
        }
        out
    }
}

/// Shared commit step: compare bytes, optionally write atomically.
fn commit_table(eng: &Engagement, suffix: &str, t: &Table, write: bool) -> Result<(bool, bool)> {
    let path = eng.table_path(suffix)?;
    let bytes = t.encode();
    let changed = match std::fs::read(&path) {
        Ok(before) => before != bytes,
        Err(_) => true,
    };
    if write && changed {
        transact::commit_if_changed(&path, &bytes)?;
    }
    Ok((changed, write && changed))
}

fn parse_flag_value(raw: &str) -> Result<Flag> {
    Flag::parse(raw)
}

fn parse_side_value(raw: &str) -> Result<Side> {
    Side::parse(raw)
}

/// Build a properties patch from `(COLUMN, value)` pairs.
fn property_patch(sets: &[(String, String)]) -> Result<PropertiesPatch> {
    let mut patch = PropertiesPatch::default();
    for (name, value) in sets {
        let col = name.trim().to_ascii_uppercase();
        match col.as_str() {
            "COLUMN" => patch.column = Some(parse_flag_value(value)?),
            "DOLLAR" => patch.dollar = Some(parse_flag_value(value)?),
            "ULINE" => patch.uline = Some(parse_flag_value(value)?),
            "PAGE" => patch.page = Some(parse_flag_value(value)?),
            "POST" => patch.post = Some(parse_flag_value(value)?),
            "PRINT" => patch.print = Some(parse_flag_value(value)?),
            "TOTAL" => patch.total = Some(parse_flag_value(value)?),
            "HIDDEN" => patch.hidden = Some(parse_flag_value(value)?),
            "SIGN" => patch.sign = Some(parse_side_value(value)?),
            "DC" => patch.dc = Some(parse_side_value(value)?),
            "CTL" => patch.ctl = Some(value.clone()),
            "DEN" => patch.den = Some(value.clone()),
            other => {
                return Err(CoreError::Format(format!(
                    "--set {other}: account edit supports {} (use --desc for AC_DESC)",
                    PROPERTY_COLUMNS.join("|")
                )))
            }
        }
    }
    Ok(patch)
}

fn account_change_lines(before: &AmAccount, after: &AmAccount) -> Vec<String> {
    let mut lines = Vec::new();
    if before.desc != after.desc {
        lines.push(format!("desc     : {:?} -> {:?}", before.desc, after.desc));
    }
    let bp = property_pairs(before);
    let ap = property_pairs(after);
    for ((name, old), (_, new)) in bp.iter().zip(ap.iter()) {
        if old != new {
            lines.push(format!("{name:<9}: {old:?} -> {new:?}"));
        }
    }
    if before.schedule != after.schedule {
        lines.push(format!(
            "schedule : {:?} -> {:?}",
            before.schedule, after.schedule
        ));
    }
    if lines.is_empty() {
        lines.push("(values already match)".to_string());
    }
    lines
}

/// Interactive account edit: description + the 12 account-property columns.
///
/// Writes to `<stem>AM.dbf` only. `write=false` is a preview.
pub fn edit_account(
    eng: &Engagement,
    ac_no: &str,
    desc: Option<&str>,
    sets: &[(String, String)],
    write: bool,
) -> Result<EditOutcome> {
    let mut t = eng.table("AM")?;
    let chart = Chart::from_table(&t)?;
    let before_acct = find_account(&chart, ac_no)
        .ok_or_else(|| {
            CoreError::Format(format!("account {ac_no:?} not found in AM posting index"))
        })?
        .clone();
    let row = before_acct.row;

    if let Some(d) = desc {
        write_cell(&mut t, row, "AC_DESC", d)?;
    }
    let patch = property_patch(sets)?;
    patch.write_to(&mut t, row)?;

    let after_chart = Chart::from_table(&t)?;
    let after_acct = after_chart
        .accounts()
        .iter()
        .find(|a| a.row == row)
        .ok_or_else(|| CoreError::Format(format!("row {row} vanished after edit")))?;
    let (changed, written) = commit_table(eng, "AM", &t, write)?;
    Ok(EditOutcome {
        surface: "account",
        table: format!("{}{}", eng.stem, "AM"),
        changed,
        written,
        detail: {
            let mut d = vec![format!("account  : {}", after_acct.ac_no.trim())];
            d.extend(account_change_lines(&before_acct, after_acct));
            d
        },
    })
}

/// Parse an entry class (`N`/`R`/... storage letters or friendly names).
pub fn parse_entry_type(raw: &str) -> Result<EntryType> {
    let s = raw.trim().to_ascii_lowercase();
    match s.as_str() {
        "n" | "normal" | "aje" | "adjusting" => Ok(EntryType::Normal),
        "r" | "reclass" | "reclassifying" => Ok(EntryType::Reclassifying),
        "u" | "unrecorded" | "proposed" => Ok(EntryType::UnrecordedProposed),
        "e" | "eliminating" | "elimination" => Ok(EntryType::Eliminating),
        "t" | "taxfed" | "tax-federal" => Ok(EntryType::TaxFederal),
        "v" | "taxstate" | "tax-state" => Ok(EntryType::TaxState),
        "c" | "taxcity" | "tax-city" => Ok(EntryType::TaxCity),
        "b" | "otherbasis" | "other-basis" => Ok(EntryType::OtherBasis),
        "l" | "projected" | "unrecorded-projected" => Ok(EntryType::UnrecordedProjected),
        "g" | "judgmental" | "unrecorded-judgmental" => Ok(EntryType::UnrecordedJudgmental),
        "2" | "other2" => Ok(EntryType::Other2),
        "3" | "other3" => Ok(EntryType::Other3),
        "prior" | "priorperiod" | "prior-period" => Err(CoreError::Format(
            "prior-period entries have no GL.STATUS code (context required)".into(),
        )),
        other => Err(CoreError::Format(format!(
            "unknown entry type {other:?} (N/R/U/E/T/V/C/B/L/G/2/3)"
        ))),
    }
}

/// Interactive entry edit: create a balanced AJE or replace an existing entry
/// of the same row shape.
///
/// Writes to `<stem>GL.dbf` only. New rows are appended; an existing
/// `system_id` is rewritten in place when the header + line count matches.
pub fn edit_entry(
    eng: &Engagement,
    system_id: &str,
    description: &str,
    user_reference: &str,
    entry_type: EntryType,
    date: &str,
    period: Option<u8>,
    lines: &[(String, f64)],
    write: bool,
) -> Result<EditOutcome> {
    if system_id.trim().is_empty() {
        return Err(CoreError::Format("entry needs a --ref value".into()));
    }
    if lines.is_empty() {
        return Err(CoreError::Format(
            "entry needs at least one --line ACCT=AMOUNT".into(),
        ));
    }
    if let Some(p) = period {
        if !(cw_app::tb::PERIOD_MIN..=cw_app::tb::PERIOD_MAX).contains(&p) {
            return Err(CoreError::Format(format!(
                "--period {p} is outside 1..=12"
            )));
        }
    }
    let chart = Chart::from_table(&eng.table("AM")?)?;
    for (acct, _) in lines {
        if find_account(&chart, acct).is_none() {
            return Err(CoreError::Format(format!(
                "line account {acct:?} not found in AM"
            )));
        }
    }
    let adj_lines: Vec<AdjustmentLine> = lines
        .iter()
        .map(|(acct, amount)| AdjustmentLine::new(acct.trim(), *amount))
        .collect();
    let entry = AdjustmentEntry::render(
        system_id,
        user_reference,
        description,
        entry_type,
        date,
        period,
        &adj_lines,
    )?;

    let mut t = eng.table("GL")?;
    let existing = Adjustments::from_table(&t)?
        .get(system_id)
        .cloned();
    let new_rows = entry.encode_rows()?;

    let (mode, changed) = match existing {
        Some(old) => {
            let mut targets = Vec::with_capacity(old.gl_rows.len());
            if let Some(h) = old.header_row {
                targets.push(h);
            }
            for l in &old.lines {
                targets.push(l.row);
            }
            if targets.len() != new_rows.len() {
                return Err(CoreError::Format(format!(
                    "entry {system_id:?} exists with {} rows; replacement has {} (same shape only)",
                    targets.len(),
                    new_rows.len()
                )));
            }
            for (row, bytes) in targets.iter().zip(new_rows.iter()) {
                cw_app::adjustments::GlRow::parse_record(bytes)?.write_to(&mut t, *row)?;
            }
            ("replaced", true)
        }
        None => {
            for bytes in &new_rows {
                if bytes.len() != t.record_len as usize {
                    return Err(CoreError::Format(format!(
                        "GL row is {} bytes, table record length {}",
                        bytes.len(),
                        t.record_len
                    )));
                }
                t.records.push(bytes.clone());
            }
            ("created", true)
        }
    };

    let (changed, written) = if changed {
        commit_table(eng, "GL", &t, write)?
    } else {
        (false, false)
    };
    let balance = entry.balance();
    let mut detail = vec![
        format!("entry    : {system_id} ({mode})"),
        format!(
            "header   : {} | {} | {} | date {} | period {}",
            entry.entry_type.display_name(),
            if description.is_empty() { "<no description>" } else { description },
            if user_reference.is_empty() { "<no reference>" } else { user_reference },
            date,
            period.map(|p| p.to_string()).unwrap_or_else(|| "-".into())
        ),
        format!(
            "rows     : header + {} lines ({} physical)",
            entry.line_count(),
            entry.row_count()
        ),
        format!(
            "balance  : debit {:.2} / credit {:.2} / net {:.2}",
            balance.debits,
            balance.credits,
            balance.net()
        ),
    ];
    for line in &adj_lines {
        detail.push(format!(
            "line     : {:<10} {:>14.2}",
            line.account, line.amount
        ));
    }
    Ok(EditOutcome {
        surface: "entry",
        table: format!("{}{}", eng.stem, "GL"),
        changed,
        written,
        detail,
    })
}

/// Interactive grouping edit: assign `AM.GROUPn <- MP.MAP_NO` or clear it.
///
/// Writes to `<stem>AM.dbf` only; validation lives in `cw_app::groups`.
pub fn edit_grouping(
    eng: &Engagement,
    scheme_raw: &str,
    ac_no: &str,
    node: Option<&str>,
    write: bool,
) -> Result<EditOutcome> {
    let scheme = Scheme::parse(scheme_raw)?;
    let mp = MpTable::from_table(&eng.table("MP")?)?;
    if mp.scheme(scheme).is_none() {
        return Err(CoreError::Format(format!(
            "grouping scheme {scheme_raw:?} is empty/unknown"
        )));
    }
    let mut am = eng.table("AM")?;
    let chart = Chart::from_table(&am)?;
    let acct = find_account(&chart, ac_no)
        .ok_or_else(|| {
            CoreError::Format(format!("account {ac_no:?} not found in AM posting index"))
        })?
        .clone();
    let digit = scheme.digit().ok_or_else(|| {
        CoreError::Format(format!(
            "scheme {scheme_raw:?} has no AM.GROUPn column (not assignable)"
        ))
    })?;
    let slot = group_slot(digit).ok_or_else(|| {
        CoreError::Format(format!("scheme digit {digit} has no AM group slot"))
    })?;
    let old = acct.groups[slot].trim().to_string();
    match node {
        Some(code) => groups::assign_account(&mut am, acct.row, &mp, digit, code)?,
        None => groups::clear_assignment(&mut am, acct.row, digit)?,
    }
    let new = groups::account_assignment(&am, acct.row, digit)?;
    let (changed, written) = commit_table(eng, "AM", &am, write)?;
    Ok(EditOutcome {
        surface: "grouping",
        table: format!("{}{}", eng.stem, "AM"),
        changed,
        written,
        detail: vec![
            format!(
                "account  : {} {}",
                acct.ac_no.trim(),
                truncate(&acct.desc, 40)
            ),
            format!(
                "scheme   : {} ({}) -> AM.GROUP{digit}",
                scheme_raw,
                scheme.label()
            ),
            format!(
                "node     : {} -> {}",
                if old.is_empty() { "<clear>" } else { &old },
                if new.is_empty() { "<clear>" } else { &new }
            ),
        ],
    })
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

/// Cents-rounded money with thousands separators; negatives in parentheses.
pub fn fmt_money(v: f64) -> String {
    let cents = (v.abs() * 100.0).round() as i64;
    let whole = cents / 100;
    let frac = cents % 100;
    let digits = whole.to_string();
    let mut grouped = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let body = format!("{grouped}.{frac:02}");
    if v < -0.004 {
        format!("({body})")
    } else {
        body
    }
}

/// Truncate to a display width, appending `..` when clipped.
pub fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    if width <= 2 {
        return s.chars().take(width).collect();
    }
    let mut out: String = s.chars().take(width - 2).collect();
    out.push_str("..");
    out
}

pub(crate) fn json_escape(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

pub(crate) fn json_str(s: &str) -> String {
    format!("\"{}\"", json_escape(s))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_is_cents_rounded_and_signed() {
        assert_eq!(fmt_money(0.0), "0.00");
        assert_eq!(fmt_money(1234.5), "1,234.50");
        assert_eq!(fmt_money(-1234.5), "(1,234.50)");
        assert_eq!(fmt_money(1_234_567.891), "1,234,567.89");
        assert_eq!(fmt_money(-0.004), "0.00");
    }

    #[test]
    fn truncate_respects_width() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdefghij", 6), "abcd..");
        assert_eq!(truncate("abc", 2), "ab");
    }

    #[test]
    fn json_escape_handles_quotes_and_controls() {
        assert_eq!(json_escape("a\"b\\c\n"), "a\\\"b\\\\c\\n");
    }

    #[test]
    fn property_pairs_follow_dialog_order() {
        assert_eq!(PROPERTY_COLUMNS[0], "POST");
        assert_eq!(PROPERTY_COLUMNS[11], "HIDDEN");
    }

    #[test]
    fn docs_mode_parses() {
        assert_eq!(DocsMode::parse("tree").unwrap(), DocsMode::Tree);
        assert_eq!(DocsMode::parse("").unwrap(), DocsMode::Tree);
        assert_eq!(DocsMode::parse("census").unwrap(), DocsMode::Census);
        assert!(DocsMode::parse("nope").is_err());
    }

    #[test]
    fn entry_type_parses_letters_and_names() {
        assert_eq!(parse_entry_type("N").unwrap(), EntryType::Normal);
        assert_eq!(parse_entry_type("aje").unwrap(), EntryType::Normal);
        assert_eq!(parse_entry_type("reclass").unwrap(), EntryType::Reclassifying);
        assert_eq!(parse_entry_type("T").unwrap(), EntryType::TaxFederal);
        assert_eq!(parse_entry_type("2").unwrap(), EntryType::Other2);
        assert!(parse_entry_type("prior").is_err());
        assert!(parse_entry_type("nope").is_err());
    }

    /// Golden corpus smoke: the shell must render the real kernel.
    #[test]
    fn shell_renders_golden_corpus() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("tests")
            .join("golden")
            .join("dbf")
            .join("corp2026");
        if !dir.exists() {
            return; // corpus not shipped with this checkout
        }
        let eng = Engagement::open(&dir).expect("open corpus");
        assert!(eng.stem.contains("USI Compilation Training"));

        let tb = render_tb(&eng, &TbRequest { limit: 5, ..Default::default() })
            .expect("tb renders");
        assert!(tb.contains("Trial Balance"));
        assert!(tb.contains("TOTAL"));
        assert!(tb.contains("more rows"));

        let docs = render_docs(&eng, DocsMode::Census, 0).expect("docs census");
        assert!(docs.contains("Document Manager"));

        let account = render_account_dialog(&eng, "1000")
            .or_else(|_| {
                let chart = Chart::from_table(&eng.table("AM")?)?;
                let first = chart.posting().next().expect("posting").ac_no.clone();
                render_account_dialog(&eng, &first)
            })
            .expect("account dialog");
        assert!(account.contains("Properties (dialog order)"));

        let catalog = dialog_catalog();
        assert!(catalog.contains("assign-grouping"));
    }

    /// Every shell surface must render against the real corpus (no panics).
    #[test]
    fn shell_renders_every_surface() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("tests")
            .join("golden")
            .join("dbf")
            .join("corp2026");
        if !dir.exists() {
            return;
        }
        // corp2026 MP rows past the readable frontier are restore-fabricated
        // (F63 LZMA truncation; frontier per cw-app tests/support FRONTIER).
        // The shell decodes MP whole, so run every surface on a frontier-
        // truncated mirror; all other corp2026 tables are readable end to end.
        let mp_frontier = 786usize;
        let tmp = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("cw-cli-every-surface")
            .join(std::process::id().to_string());
        std::fs::create_dir_all(&tmp).expect("scratch dir");
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = entry.path();
            let is_table = p.extension().map(|e| {
                e.eq_ignore_ascii_case("dbf") || e.eq_ignore_ascii_case("fpt") || e.eq_ignore_ascii_case("cdx")
            });
            if is_table == Some(true) {
                std::fs::copy(&p, tmp.join(p.file_name().unwrap())).expect("copy table");
            }
        }
        let mp_name = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().to_uppercase().ends_with("MP.DBF"))
                    .unwrap_or(false)
            })
            .expect("MP table");
        let mut mp = cw_core::dbf::Table::parse(&std::fs::read(&mp_name).expect("read MP"))
            .expect("parse MP");
        mp.records.truncate(mp_frontier);
        std::fs::write(tmp.join(mp_name.file_name().unwrap()), mp.encode()).expect("write MP");

        let eng = Engagement::open(&tmp).expect("open truncated corpus");
        assert_eq!(detect_stem(&dir).unwrap(), eng.stem);

        let tree = render_docs(&eng, DocsMode::Tree, 8).expect("tree");
        assert!(tree.contains("Document Manager | tree"));
        let list = render_docs(&eng, DocsMode::List, 8).expect("list");
        assert!(list.contains("LINENO"));
        let docs_json = docs_json(&eng, DocsMode::Census, 0).expect("docs json");
        assert!(docs_json.contains("\"live\":"));

        let tb_json = tb_json(&eng, &TbRequest { limit: 0, ..Default::default() })
            .expect("tb json");
        assert!(tb_json.contains("\"totals\""));
        assert!(tb_json.contains("\"rows\""));

        let renumber = render_renumber(&eng, 100, 5).expect("renumber preview");
        assert!(renumber.contains("Renumber Accounts"));
        assert!(renumber.contains("preview"));

        let mp = MpTable::from_table(&eng.table("MP").expect("MP")).expect("decode MP");
        let scheme = mp.scheme_keys().first().copied().expect("scheme");
        let grouping = render_grouping_dialog(&eng, &scheme.as_str(), None, 5)
            .expect("grouping dialog");
        assert!(grouping.contains("Assign Grouping"));

        let book = crate::docman::open_book(&eng.dir, &eng.stem).expect("SH");
        let doc = book
            .preorder()
            .into_iter()
            .map(|(_, d)| d)
            .find(|d| !d.cwguid.is_empty() && !d.schedule.is_empty())
            .expect("a document");
        let props = render_document_dialog(&eng, &doc.cwguid).expect("document dialog");
        assert!(props.contains("Document Properties"));

        let a = Chart::from_table(&eng.table("AM").expect("AM"))
            .expect("chart")
            .posting()
            .next()
            .expect("posting")
            .ac_no
            .clone();
        let acc_json = account_json(&eng, &a).expect("account json");
        assert!(acc_json.contains("\"properties\""));
    }

    /// Not part of the gate: writes the I1 evidence snapshot on demand
    /// (`cargo test -p cw-cli write_ui_snapshot -- --ignored`).
    #[test]
    #[ignore = "writes the I1 UI-shell evidence snapshot"]
    fn write_ui_snapshot() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let dir = root.join("tests").join("golden").join("dbf").join("corp2026");
        if !dir.exists() {
            return;
        }
        let eng = Engagement::open(&dir).expect("open corpus");
        let cube = Cube::from_table(&eng.table("BL").expect("BL")).expect("cube");
        let view = TbView::from_cube(
            &cube,
            &TbRequest {
                period: 12,
                ..Default::default()
            }
            .options()
            .expect("tb options"),
        )
        .expect("tb view");
        let acct = view.rows().first().expect("tb row").number.clone();
        let mp = MpTable::from_table(&eng.table("MP").expect("MP")).expect("MP");
        let scheme = mp.scheme_keys().first().copied().expect("scheme").as_str();
        let book = crate::docman::open_book(&eng.dir, &eng.stem).expect("SH");
        let doc = book
            .preorder()
            .into_iter()
            .map(|(_, d)| d)
            .find(|d| !d.cwguid.is_empty() && !d.schedule.is_empty())
            .expect("document")
            .cwguid
            .clone();

        let mut out = String::new();
        out.push_str("===== dashboard =====\n");
        out.push_str(&eng.dashboard().expect("dashboard"));
        out.push_str("\n===== tb (limit 8) =====\n");
        out.push_str(
            &render_tb(
                &eng,
                &TbRequest {
                    period: 12,
                    limit: 8,
                    ..Default::default()
                },
            )
            .expect("tb"),
        );
        out.push_str("\n===== docs tree (limit 15) =====\n");
        out.push_str(&render_docs(&eng, DocsMode::Tree, 15).expect("tree"));
        out.push_str("\n===== docs census =====\n");
        out.push_str(&render_docs(&eng, DocsMode::Census, 0).expect("census"));
        out.push_str(&format!("\n===== account {acct} =====\n"));
        out.push_str(&render_account_dialog(&eng, &acct).expect("account"));
        out.push_str("\n===== renumber preview (delta +100) =====\n");
        out.push_str(&render_renumber(&eng, 100, 3).expect("renumber"));
        out.push_str(&format!("\n===== assign grouping (scheme {scheme}) =====\n"));
        out.push_str(&render_grouping_dialog(&eng, &scheme, None, 5).expect("grouping"));
        out.push_str(&format!("\n===== document {doc} =====\n"));
        out.push_str(&render_document_dialog(&eng, &doc).expect("document"));
        out.push_str("\n===== dialog catalogue =====\n");
        out.push_str(&dialog_catalog());

        let path = root.join("evidence").join("ui-shell-snapshot.txt");
        std::fs::write(&path, out).expect("write evidence snapshot");
        assert!(path.exists());
    }
}

