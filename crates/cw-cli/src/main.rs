//! `opencw` - the OpenCW interface shell (P5.I / I1).
//!
//! Usage:
//!   opencw                                  health probe
//!   opencw <engagement-dir>                 shell dashboard
//!   opencw <dir> tb [flags]                 TB grid
//!   opencw <dir> docs [tree|list|census]    document manager
//!   opencw <dir> account <AC_NO>            account-properties dialog
//!   opencw <dir> dialog <id> [args]         dialog-parity surfaces
//!   opencw dialogs                          dialog catalogue
//!
//! Read surfaces never write. The `edit` paths and `dialog renumber --write`
//! commit through `cw_core::transact` (write-aside -> fsync -> atomic rename).

use cw_app::tb::Compare;
use cw_cli::{
    account_json, detect_stem, dialog_catalog, docs_json, edit_account, edit_entry, edit_grouping,
    parse_entry_type, render_account_dialog, render_docs, render_document_dialog,
    render_grouping_dialog, render_tb, renumber_dialog, renumber_scope, tb_json, DocsMode,
    Engagement, TbRequest,
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        probe();
        return;
    }
    if let Err(e) = run(&args) {
        eprintln!("opencw: {e}");
        std::process::exit(2);
    }
}

fn run(args: &[String]) -> Result<(), String> {
    match args[0].as_str() {
        "health" => {
            probe();
            Ok(())
        }
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        "version" | "--version" | "-v" => {
            println!("opencw {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "dialogs" => {
            print!("{}", dialog_catalog());
            Ok(())
        }
        "serve" => serve_command(&args[1..]),
        cmd if cmd.starts_with('-') => Err(format!("unknown option {cmd:?}\n\n{}", HELP)),
        dir => {
            let eng = Engagement::open(dir).map_err(|e| e.to_string())?;
            let rest = &args[1..];
            let command = rest.first().map(String::as_str).unwrap_or("open");
            match command {
                "open" | "status" | "dashboard" => {
                    print!("{}", eng.dashboard().map_err(|e| e.to_string())?);
                    Ok(())
                }
                "tb" | "trial-balance" => tb_command(&eng, &rest[1..]),
                "docs" | "documents" | "tree" => docs_command(&eng, command, &rest[1..]),
                "account" | "acct" => {
                    let ac_no = rest.get(1).ok_or("account needs <AC_NO>")?;
                    if rest.iter().any(|a| a == "--json") {
                        println!(
                            "{}",
                            account_json(&eng, ac_no).map_err(|e| e.to_string())?
                        );
                    } else {
                        print!(
                            "{}",
                            render_account_dialog(&eng, ac_no).map_err(|e| e.to_string())?
                        );
                    }
                    Ok(())
                }
                "dialog" => dialog_command(&eng, &rest[1..]),
                "edit" | "set" => edit_command(&eng, &rest[1..]),
                other => Err(format!(
                    "unknown command {other:?} (try open|tb|docs|account|dialog|edit|dialogs)"
                )),
            }
        }
    }
}

fn tb_command(eng: &Engagement, args: &[String]) -> Result<(), String> {
    let mut req = TbRequest::default();
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--year" => req.year = int_arg(args, &mut i, "--year")? as i32,
            "--period" => req.period = int_arg(args, &mut i, "--period")? as u8,
            "--group" | "--basis" => req.group = str_arg(args, &mut i, "--group")?,
            "--entity" => req.entity = Some(str_arg(args, &mut i, "--entity")?),
            "--prior" => req.compare = Compare::Year(int_arg(args, &mut i, "--prior")? as i32),
            "--no-compare" => req.compare = Compare::Off,
            "--auto-compare" => req.compare = Compare::Auto,
            "--synthetics" => req.synthetics = true,
            "--limit" => req.limit = int_arg(args, &mut i, "--limit")? as usize,
            "--json" => json = true,
            other => return Err(format!("tb: unknown option {other:?}")),
        }
        i += 1;
    }
    if json {
        println!("{}", tb_json(eng, &req).map_err(|e| e.to_string())?);
    } else {
        print!("{}", render_tb(eng, &req).map_err(|e| e.to_string())?);
    }
    Ok(())
}

fn docs_command(eng: &Engagement, command: &str, args: &[String]) -> Result<(), String> {
    let mut mode = if command == "tree" {
        DocsMode::Tree
    } else {
        DocsMode::Tree
    };
    let mut json = false;
    let mut limit = 0usize;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "tree" => mode = DocsMode::Tree,
            "list" => mode = DocsMode::List,
            "census" | "stats" => mode = DocsMode::Census,
            "--json" => json = true,
            "--limit" => limit = int_arg(args, &mut i, "--limit")? as usize,
            other => mode = DocsMode::parse(other).map_err(|e| e.to_string())?,
        }
        i += 1;
    }
    if json {
        println!(
            "{}",
            docs_json(eng, mode, limit).map_err(|e| e.to_string())?
        );
    } else {
        print!(
            "{}",
            render_docs(eng, mode, limit).map_err(|e| e.to_string())?
        );
    }
    Ok(())
}

fn dialog_command(eng: &Engagement, args: &[String]) -> Result<(), String> {
    let id = args.first().map(String::as_str).unwrap_or("list");
    match id {
        "list" | "" => {
            print!("{}", dialog_catalog());
            Ok(())
        }
        "account-properties" | "account" => {
            let ac_no = args.get(1).ok_or("dialog account-properties needs <AC_NO>")?;
            print!(
                "{}",
                render_account_dialog(eng, ac_no).map_err(|e| e.to_string())?
            );
            Ok(())
        }
        "renumber-accounts" | "renumber" => {
            let delta = flag_value(args, "--delta")
                .map(|v| v.parse::<i64>())
                .transpose()
                .map_err(|_| "dialog renumber: --delta must be an integer".to_string())?
                .unwrap_or(100);
            let limit = flag_value(args, "--limit")
                .map(|v| v.parse::<usize>())
                .transpose()
                .map_err(|_| "dialog renumber: --limit must be an integer".to_string())?
                .unwrap_or(40);
            let write = args.iter().any(|a| a == "--write");
            let mut pairs: Vec<(String, String)> = Vec::new();
            let mut i = 0usize;
            while i < args.len() {
                if args[i] == "--map" {
                    let raw = args
                        .get(i + 1)
                        .ok_or("dialog renumber: --map needs FROM=TO")?;
                    let (from, to) = raw
                        .split_once('=')
                        .ok_or_else(|| format!("dialog renumber: --map {raw:?} is not FROM=TO"))?;
                    pairs.push((from.to_string(), to.to_string()));
                    i += 1;
                }
                i += 1;
            }
            let plan = if pairs.is_empty() {
                cw_app::renumber::RenumberPlan::from_delta(
                    &renumber_scope(eng).map_err(|e| e.to_string())?,
                    delta,
                )
                .map_err(|e| e.to_string())?
            } else {
                cw_app::renumber::RenumberPlan::from_pairs(&pairs).map_err(|e| e.to_string())?
            };
            let label = if pairs.is_empty() {
                format!(
                    "delta {delta:+} | {}",
                    if write { "commit" } else { "preview" }
                )
            } else {
                format!("map {} pair(s) | {}", pairs.len(), if write { "commit" } else { "preview" })
            };
            let outcome =
                renumber_dialog(eng, &plan, limit, write).map_err(|e| e.to_string())?;
            print!("{}", outcome.render(&eng.header(), &label));
            Ok(())
        }
        "assign-grouping" | "grouping" => {
            let scheme = args.get(1).ok_or("dialog assign-grouping needs <scheme>")?;
            let node = flag_value(args, "--node");
            let limit = flag_value(args, "--limit")
                .map(|v| v.parse::<usize>())
                .transpose()
                .map_err(|_| "dialog assign-grouping: --limit must be an integer".to_string())?
                .unwrap_or(40);
            print!(
                "{}",
                render_grouping_dialog(eng, scheme, node.as_deref(), limit)
                    .map_err(|e| e.to_string())?
            );
            Ok(())
        }
        "document-properties" | "document" => {
            let key = args.get(1).ok_or("dialog document needs <guid|schedule>")?;
            print!(
                "{}",
                render_document_dialog(eng, key).map_err(|e| e.to_string())?
            );
            Ok(())
        }
        other => Err(format!(
            "unknown dialog {other:?} (try account-properties|renumber-accounts|assign-grouping|document-properties)"
        )),
    }
}

fn edit_command(eng: &Engagement, args: &[String]) -> Result<(), String> {
    let sub = args
        .first()
        .map(String::as_str)
        .ok_or("edit needs a surface: account <AC_NO> | entry --ref ID | grouping <scheme> <AC_NO>")?;
    let write = args.iter().any(|a| a == "--write");
    let header = format!(
        "{} | edit {}",
        eng.header(),
        if write { "commit" } else { "preview" }
    );
    match sub {
        "account" | "acct" => {
            let ac_no = args.get(1).ok_or("edit account needs <AC_NO>")?;
            let desc = flag_value(args, "--desc");
            let mut sets: Vec<(String, String)> = Vec::new();
            let mut i = 0usize;
            while i < args.len() {
                if args[i] == "--set" {
                    let raw = args
                        .get(i + 1)
                        .ok_or("edit account: --set needs COLUMN=VALUE")?;
                    let (col, value) = raw
                        .split_once('=')
                        .ok_or_else(|| format!("edit account: --set {raw:?} is not COLUMN=VALUE"))?;
                    sets.push((col.to_string(), value.to_string()));
                    i += 1;
                }
                i += 1;
            }
            let outcome = edit_account(eng, ac_no, desc.as_deref(), &sets, write)
                .map_err(|e| e.to_string())?;
            print!("{}", outcome.render(&header));
            Ok(())
        }
        "entry" | "aje" => {
            let system_id = flag_value(args, "--ref").ok_or("edit entry needs --ref <ID>")?;
            let date = flag_value(args, "--date").ok_or("edit entry needs --date YYYYMMDD")?;
            let description = flag_value(args, "--desc").unwrap_or_default();
            let user_reference = flag_value(args, "--from-ref").unwrap_or_default();
            let entry_type = parse_entry_type(&flag_value(args, "--type").unwrap_or_else(|| "N".into()))
                .map_err(|e| e.to_string())?;
            let period = flag_value(args, "--period")
                .map(|v| {
                    v.parse::<u8>()
                        .map_err(|_| format!("edit entry: --period {v:?} is not 1..12"))
                })
                .transpose()?;
            let mut lines: Vec<(String, f64)> = Vec::new();
            let mut i = 0usize;
            while i < args.len() {
                if args[i] == "--line" {
                    let raw = args
                        .get(i + 1)
                        .ok_or("edit entry: --line needs ACCT=AMOUNT")?;
                    let (acct, amount) = raw
                        .split_once('=')
                        .ok_or_else(|| format!("edit entry: --line {raw:?} is not ACCT=AMOUNT"))?;
                    let amount = amount.parse::<f64>().map_err(|_| {
                        format!("edit entry: --line {raw:?} amount {amount:?} is not numeric")
                    })?;
                    lines.push((acct.to_string(), amount));
                    i += 1;
                }
                i += 1;
            }
            let outcome = edit_entry(
                eng,
                &system_id,
                &description,
                &user_reference,
                entry_type,
                &date,
                period,
                &lines,
                write,
            )
            .map_err(|e| e.to_string())?;
            print!("{}", outcome.render(&header));
            Ok(())
        }
        "grouping" | "group" => {
            let scheme = args.get(1).ok_or("edit grouping needs <scheme>")?;
            let ac_no = args.get(2).ok_or("edit grouping needs <AC_NO>")?;
            let node = flag_value(args, "--node");
            let clear = args.iter().any(|a| a == "--clear");
            if node.is_none() && !clear {
                return Err(
                    "edit grouping needs --node <MAP_NO> or --clear (preview/commit)".into(),
                );
            }
            let outcome = edit_grouping(eng, scheme, ac_no, node.as_deref(), write)
                .map_err(|e| e.to_string())?;
            print!("{}", outcome.render(&header));
            Ok(())
        }
        other => Err(format!(
            "unknown edit surface {other:?} (try account|entry|grouping)"
        )),
    }
}

/// `opencw serve [<engagement-dir>] [--port N] [--bind ADDR]`
fn serve_command(args: &[String]) -> Result<(), String> {
    let mut dir: Option<String> = None;
    let mut port = cw_cli::web::DEFAULT_PORT;
    let mut bind = cw_cli::web::DEFAULT_BIND.to_string();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--port" | "-p" => {
                i += 1;
                let raw = args.get(i).ok_or("serve: --port needs a value")?;
                port = raw
                    .parse::<u16>()
                    .map_err(|_| format!("serve: --port {raw:?} is not 1..=65535"))?;
            }
            "--bind" | "-b" => {
                i += 1;
                bind = args
                    .get(i)
                    .cloned()
                    .ok_or("serve: --bind needs an address")?;
            }
            other if other.starts_with('-') => {
                return Err(format!("serve: unknown option {other:?}"));
            }
            other => {
                if dir.is_some() {
                    return Err("serve: one engagement directory only".to_string());
                }
                dir = Some(other.to_string());
            }
        }
        i += 1;
    }
    let dir = dir.ok_or(
        "serve needs an engagement directory: opencw serve <dir> [--port 8078] [--bind 127.0.0.1]",
    )?;
    let cfg = cw_cli::web::Config {
        dir: std::path::PathBuf::from(dir),
        port,
        bind,
    };
    cw_cli::web::serve(cfg)
}

fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn int_arg(args: &[String], i: &mut usize, name: &str) -> Result<i64, String> {
    *i += 1;
    let raw = args.get(*i).ok_or(format!("{name} needs a value"))?;
    raw.parse::<i64>()
        .map_err(|_| format!("{name} must be an integer, got {raw:?}"))
}

fn str_arg(args: &[String], i: &mut usize, name: &str) -> Result<String, String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or(format!("{name} needs a value"))
}

fn probe() {
    let core = cw_core::healthy();
    let app = cw_app::healthy();
    let cv = cw_cv::healthy();
    let ok = core && app && cv;
    println!(
        "opencw v{} | core {} app {} cv {} | stack {}",
        env!("CARGO_PKG_VERSION"),
        if core { "ok" } else { "DEGRADED" },
        if app { "ok" } else { "DEGRADED" },
        if cv { "ok" } else { "DEGRADED" },
        if ok { "ok" } else { "DEGRADED" }
    );
    println!("usage: opencw <engagement-dir> [open|tb|docs|account|dialog]   (opencw help)");
    if !ok {
        std::process::exit(1);
    }
}

fn print_help() {
    println!("{HELP}");
}

const HELP: &str = "\
opencw - OpenCW interface shell (I1 read surfaces + I2 edit paths)

USAGE
  opencw                                     kernel/stack health probe
  opencw <engagement-dir>                    shell dashboard (AM/MP/SH/BL census)
  opencw <dir> tb [flags]                    trial-balance grid
  opencw <dir> docs [tree|list|census]       document manager
  opencw <dir> account <AC_NO> [--json]      account-properties dialog
  opencw <dir> dialog <id> [args]            dialog-parity surfaces
  opencw dialogs                             dialog catalogue
  opencw <dir> dialog renumber --delta 100   renumber cascade preview
  opencw <dir> dialog renumber --map 1001=1099 --write   commit a renumber
  opencw <dir> dialog assign-grouping 1 --node 100
  opencw <dir> dialog document <guid|schedule>
  opencw <dir> edit account <AC_NO> ...      account description/properties
  opencw <dir> edit entry --ref ID ...       create/replace a balanced AJE
  opencw <dir> edit grouping <scheme> <AC_NO> --node CODE | --clear
  opencw serve <dir> [--port 8078] [--bind 127.0.0.1]
                                             local web shell (read + preview/commit)

WEB SHELL
  opencw serve <dir>                         http://127.0.0.1:8078/
  GET  /api/overview                         {engagement, stem, path, census, healthy}
  GET  /api/tb?year=&period=&group=&entity=&limit=&offset=
  GET  /api/docs?mode=tree|list|census        SH register lens (repaired prefix)
  GET  /api/docman?q=&class=&internals=      Document Manager: every file in the
       &sha=&path=<dir>                      engagement folder LEFT-JOINed to SH
  GET  /api/docman/file?path=<rel>           raw document bytes, real MIME type
  GET  /api/docman/preview-cvw?path=<rel>    render a .cvw sheet in the browser
       [&format=json]                        (cw_cv Layout -> render -> sheet)
  GET  /api/account/<AC_NO>                  account properties + balances
  GET  /api/entries?acct=&limit=&offset=     GL entries, header + lines
  GET  /api/signoff?doc=<guid|schedule>      eight-level signoff ladder
  GET  /api/manifest?path=<dir>              {files:[{name,bytes,sha256}], total_bytes}
  POST /api/manifest/diff {a,b}              {only_a, only_b, changed}
  POST /api/{entries,renumber,yearend}/preview|commit
  POST /api/lifecycle/{expand,compress,backup}[/preview|commit]

  Every write is preview-first: a preview runs the real operation against a
  throwaway copy and returns the full before/after manifest diff; nothing is
  written. A commit snapshots the engagement into <parent>/work/ first, then
  lands through cw_core::transact. Smoke:
    curl -s http://127.0.0.1:8078/api/overview
    curl -s 'http://127.0.0.1:8078/api/tb?period=12&limit=5'
    curl -s -X POST http://127.0.0.1:8078/api/renumber/preview -d '{\"delta\":100}'

EDIT FLAGS
  --write          commit via atomic rename (default: preview only)
  renumber         --delta N | --map FROM=TO (repeatable)  --write
  account          --desc TEXT  --set POST=Y  --set HIDDEN=N  (12 property cols)
  entry            --ref ID --date YYYYMMDD [--desc TEXT] [--from-ref TEXT]
                   [--type N|R|U|E|T|V|C|B|L|G|2|3] [--period 1..12]
                   --line ACCT=AMOUNT (repeat; must balance)
  grouping         scheme digit/letter; --node MAP_NO or --clear

TB FLAGS
  --year <0..4>   BL year offset (default 0)
  --period <1..12> period cutoff (default 12)
  --group <A|T>   actual or tax basis (default A)
  --entity <e>    restrict to one entity (default all)
  --prior <n>     explicit comparison year offset (default auto)
  --no-compare    disable prior-year columns
  --synthetics    include NETINC/GROUP__* rows
  --limit <n>     cap rendered account rows (0 = all)
  --json          machine-readable output

EXAMPLES
  opencw 'sandbox\\extracted\\hull2024-z7' tb --period 12 --limit 25
  opencw 'sandbox\\extracted\\corp2026-z7' docs tree --limit 40
  opencw 'sandbox\\extracted\\hull2024-z7' account 1000

NOTES
  Engagement directories are expanded table sets (<stem>AM.dbf, <stem>BL.dbf, ...).
  Read surfaces never write. Edit paths write only the tables they target and only
  with --write; every commit is write-aside -> fsync -> atomic rename. A renumber
  writes AM, BL and GL together (the cascade), and a no-op touches no file.";

// Compile-time guard that the stem helper is part of the public shell API.
#[allow(dead_code)]
fn _stem_probe(dir: &str) -> Result<String, String> {
    detect_stem(std::path::Path::new(dir)).map_err(|e| e.to_string())
}

