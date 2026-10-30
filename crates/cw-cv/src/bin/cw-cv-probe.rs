//! `cw-cv-probe <file.cvw|cvv> [--cells]` — offline CaseView container dump.
//!
//! Prints container shape, `CvData` identity, the cell directory walk and the
//! `Layout` block table. Read-only; never touches CaseWare.

use std::collections::BTreeMap;
use std::process::ExitCode;

use cw_cv::{
    render, CellAttrs, Cvw, LinkTable, LinkedWorksheets, TextTable, WksRegistry,
};

fn main() -> ExitCode {
    let mut path = None;
    let mut show_cells = false;
    let mut scan = None;
    let mut wks = None;
    let mut ovr = None;
    let mut render_doc = false;
    let mut sheet_doc = false;
    let mut text_file = None;
    let mut links_file = None;
    let mut selftest = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--cells" => show_cells = true,
            "--scan" => scan = args.next(),
            "--wks" => wks = args.next(),
            "--ovr" => ovr = args.next(),
            "--render" => render_doc = true,
            "--sheet" => sheet_doc = true,
            "--text" => text_file = args.next(),
            "--links" => links_file = args.next(),
            "--selftest" => selftest = true,
            "-h" | "--help" => {
                eprintln!(
                    "usage: cw-cv-probe <file.cvw|cvv> [--cells] [--render|--sheet [--text <f>] [--links <f>] [--wks <dir>]]\n       cw-cv-probe --scan <dir> | --wks <dir> | --ovr <file>"
                );
                return ExitCode::SUCCESS;
            }
            _ if path.is_none() => path = Some(a),
            _ => {
                eprintln!("unexpected argument: {a}");
                return ExitCode::from(2);
            }
        }
    }
    if let Some(dir) = wks {
        return wks_report(std::path::Path::new(&dir));
    }
    if let Some(file) = ovr {
        return ovr_report(std::path::Path::new(&file));
    }
    if let Some(dir) = scan {
        return scan_dir(std::path::Path::new(&dir));
    }
    if selftest {
        return run_selftest();
    }
    let Some(path) = path else {
        eprintln!("usage: cw-cv-probe <file.cvw|cvv> [--cells] | --scan <dir> | --wks <dir>");
        return ExitCode::from(2);
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("read {path}: {e}");
            return ExitCode::from(2);
        }
    };
    let doc = match Cvw::parse(&bytes) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("parse {path}: {e}");
            return ExitCode::from(1);
        }
    };

    println!("file        {}", path);
    println!("bytes       {}", bytes.len());
    println!("streams     {}", doc.streams.len());
    for s in &doc.streams {
        println!("  {:<22} {:>9}", s.name, s.len);
    }
    println!(
        "layout      magic={} version={} block_size={}",
        doc.layout_header.magic_ok, doc.layout_header.version, doc.layout_header.block_size
    );
    match &doc.strings {
        Some(h) => println!(
            "strings     v{} count={} unpacked={} block={:#010x} compressed={}",
            h.version,
            h.count,
            h.uncompressed_len,
            h.block_magic,
            h.compressed()
        ),
        None => println!("strings     <absent>"),
    }
    match doc.strings_pool() {
        Ok(pool) => {
            println!(
                "pool        decoded entries={} complete={} text0={:?}",
                pool.len(),
                pool.complete,
                pool.text(2).unwrap_or_default()
            );
        }
        Err(e) => println!("pool        <undecoded> {e}"),
    }
    match doc.form() {
        Some(f) => println!(
            "cvdata      FORM={} valid={} offsets={}",
            f, doc.cvdata.valid, doc.cvdata.offset_count
        ),
        None => println!("cvdata      <empty>"),
    }
    println!(
        "cells       {} (declared {}, header {:?}, consumed {} complete={})",
        doc.cells().len(),
        doc.cell_dir.declared_count(),
        doc.cell_dir.header,
        doc.cell_dir.consumed,
        doc.cell_dir.complete
    );
    if show_cells {
        for (c, b) in doc.cells().iter().zip(&doc.cell_blocks) {
            println!(
                "  {:<20} layout={:#010x} block_len={:<6} name_at={}",
                c.name,
                c.layout_offset,
                b.len,
                b.name_offset.map(|o| format!("{o:#x}")).unwrap_or_else(|| "-".into())
            );
        }
    }
    if render_doc || sheet_doc {
        return render_report(
            &doc,
            std::path::Path::new(&path),
            wks.as_deref().map(std::path::Path::new),
            text_file.as_deref().map(std::path::Path::new),
            links_file.as_deref().map(std::path::Path::new),
            sheet_doc,
        );
    }
    ExitCode::SUCCESS
}

/// `--render [--text <f>] [--links <f>] [--wks <dir>]`: F4 render pipeline.
///
/// Prints the deterministic plain-text render. Without `--wks`, the document's
/// own folder is probed for `*FP.dbf` so leadsheet feeds resolve in place.
fn render_report(
    doc: &Cvw,
    path: &std::path::Path,
    wks_dir: Option<&std::path::Path>,
    text_file: Option<&std::path::Path>,
    links_file: Option<&std::path::Path>,
    structural: bool,
) -> ExitCode {
    let texts = match text_file {
        None => TextTable::new(),
        Some(f) => {
            let Ok(s) = std::fs::read_to_string(f) else {
                eprintln!("read {}", f.display());
                return ExitCode::from(2);
            };
            match TextTable::parse(&s) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("text {}: {e}", f.display());
                    return ExitCode::from(1);
                }
            }
        }
    };
    let links = match links_file {
        None => LinkTable::new(),
        Some(f) => {
            let Ok(s) = std::fs::read_to_string(f) else {
                eprintln!("read {}", f.display());
                return ExitCode::from(2);
            };
            match LinkTable::parse(&s) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("links {}: {e}", f.display());
                    return ExitCode::from(1);
                }
            }
        }
    };
    let registry: Option<WksRegistry> = match wks_dir {
        Some(dir) => match WksRegistry::from_engagement_dir(dir) {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!("wks {}: {e}", dir.display());
                return ExitCode::from(1);
            }
        },
        None => path
            .parent()
            .and_then(|p| WksRegistry::from_engagement_dir(p).ok()),
    };
    if registry.is_none() && links.iter().any(|(_, r)| r.is_leadsheet()) {
        eprintln!("render: no FP registry found; leadsheet feeds stay unresolved");
    }
    let rendered = render::render(doc, &texts, &links, registry.as_ref());
    if structural {
        print!("{}", cw_cv::sheet::sheet(&rendered).to_text());
    } else {
        print!("{}", rendered.to_text());
    }
    ExitCode::SUCCESS
}

/// `--wks <dir>`: dump the F2 linked-worksheet registry of one engagement
/// directory (`FP.dbf` + the `.cvw` template set next to it).
fn wks_report(dir: &std::path::Path) -> ExitCode {
    let loaded = match LinkedWorksheets::load(dir) {
        Ok(Some(l)) => l,
        Ok(None) => {
            eprintln!("no *FP.dbf in {}", dir.display());
            return ExitCode::from(1);
        }
        Err(e) => {
            eprintln!("wks {}: {e}", dir.display());
            return ExitCode::from(1);
        }
    };
    let reg = &loaded.registry;
    let tpl = &loaded.templates;
    println!("dir         {}", loaded.dir.display());
    println!(
        "fp          {}",
        reg.source()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<memory>".into())
    );
    println!("rows        {} ({})", reg.rows(), reg.cell_count());
    for ws in reg.worksheets() {
        let range = ws
            .cell_range()
            .map(|(a, b)| format!("{a}..{b}"))
            .unwrap_or_else(|| "-".into());
        println!(
            "  {:<4} rows={:<4} cells={:<4} range={}",
            ws.code(),
            ws.rows(),
            ws.cell_count(),
            range
        );
    }
    println!(
        "templates   {} forms [{}] ({} cells)",
        tpl.len(),
        tpl.forms().join(" "),
        tpl.cells_total()
    );
    ExitCode::SUCCESS
}

/// `--scan <dir>`: decode every `.cvw`/`.cvv` below `dir` and check the F1
/// invariants. Exit 1 on any violation, 0 on clean (or empty) scan.
fn scan_dir(root: &std::path::Path) -> ExitCode {
    let mut files = Vec::new();
    collect(root, &mut files);
    files.sort();
    let (mut cells, mut forms, mut bad) = (0usize, 0usize, 0usize);
    for p in &files {
        let Ok(bytes) = std::fs::read(p) else {
            eprintln!("READ  {}", p.display());
            bad += 1;
            continue;
        };
        let doc = match Cvw::parse(&bytes) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("PARSE {}: {e}", p.display());
                bad += 1;
                continue;
            }
        };
        let count = doc.cells().len();
        let names_ok = doc.cell_blocks.iter().all(|b| b.name_offset.is_some());
        let ok = doc.layout_header.magic_ok
            && doc.cell_dir.header[0] == 4
            && doc.cell_dir.header[1] == cw_cv::cell::MARKER
            && doc.cell_dir.declared_count() as usize == count
            && doc.cell_dir.complete
            && names_ok;
        if !ok {
            eprintln!(
                "BAD   {} cells={} declared={} consumed={}/{} magic={} names_ok={}",
                p.display(),
                count,
                doc.cell_dir.declared_count(),
                doc.cell_dir.consumed,
                bytes.len(),
                doc.layout_header.magic_ok,
                names_ok
            );
            bad += 1;
        }
        cells += count;
        if doc.form().is_some() {
            forms += 1;
        }
    }
    println!(
        "scan {} files: {} cells, {} with FORM, {} bad",
        files.len(),
        cells,
        forms,
        bad
    );
    if bad == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// `--ovr <file>`: F3 property-bag / override view of one document.
///
/// Splits every cell block into `prefix|name|suffix`, proves write-back is
/// byte-identical, and reports the sparse per-cell attribute-record shapes
/// (the opaque `CV OVR` payload) so the bit packing can be decoded later.
fn ovr_report(path: &std::path::Path) -> ExitCode {
    let Ok(bytes) = std::fs::read(path) else {
        eprintln!("read {}", path.display());
        return ExitCode::from(2);
    };
    let doc = match Cvw::parse(&bytes) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("parse {}: {e}", path.display());
            return ExitCode::from(1);
        }
    };
    let layout = match doc.stream("Form/Layout") {
        Ok(l) => l,
        Err(e) => {
            eprintln!("layout {}: {e}", path.display());
            return ExitCode::from(1);
        }
    };

    let mut shapes: BTreeMap<Vec<u8>, usize> = BTreeMap::new();
    let mut min_prefix = usize::MAX;
    let mut max_prefix = 0usize;
    let mut mismatched = 0usize;
    let mut unnamed = 0usize;
    for block in &doc.cell_blocks {
        let Some(attrs) = CellAttrs::from_layout(&layout, block) else {
            unnamed += 1;
            continue;
        };
        let start = block.offset as usize;
        let end = start + block.len as usize;
        if attrs.reassemble() != layout[start..end] {
            mismatched += 1;
        }
        let n = attrs.prefix().len();
        min_prefix = min_prefix.min(n);
        max_prefix = max_prefix.max(n);
        *shapes.entry(attrs.prefix().to_vec()).or_default() += 1;
    }
    if unnamed > 0 {
        min_prefix = min_prefix.min(0);
    }

    let mut ranked: Vec<(&Vec<u8>, &usize)> = shapes.iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(a.1));
    println!("file        {}", path.display());
    println!(
        "cells       {} (prefix {}..={} bytes, {} distinct shapes)",
        doc.cell_blocks.len(),
        if min_prefix == usize::MAX { 0 } else { min_prefix },
        max_prefix,
        shapes.len()
    );
    println!(
        "write-back  {} ({} mismatched, {} unresolved)",
        if mismatched == 0 && unnamed == 0 { "byte-identical" } else { "FAILED" },
        mismatched,
        unnamed
    );
    for (shape, n) in ranked.iter().take(12) {
        let hexs: String = shape.iter().map(|b| format!("{b:02X}")).collect();
        println!("  {n:>6}x  {hexs}");
    }
    ExitCode::SUCCESS
}

/// `--selftest`: execute the synthetic decoder fixtures without a test harness
/// (the machine's AV filter sometimes denies freshly linked lib-test exes).
fn run_selftest() -> ExitCode {
    use cw_cv::cell::{CellDirectory, MARKER};
    use cw_cv::cvdata::CvData;
    use cw_cv::layout::LayoutHeader;

    let mut fail = 0;
    // Cell walk: header + 3 names, Layout oracle resolves boundaries.
    let mut cell = Vec::new();
    for w in [4u32, MARKER, 0, 3] {
        cell.extend_from_slice(&w.to_le_bytes());
    }
    for (name, off) in [("A1", 0x10u32), ("B2", 0x20), ("C3", 0x30)] {
        cell.extend_from_slice(name.as_bytes());
        cell.extend_from_slice(&off.to_le_bytes());
    }
    let mut layout = vec![0u8; 0x60];
    layout[0x10..0x12].copy_from_slice(b"A1");
    layout[0x20..0x22].copy_from_slice(b"B2");
    layout[0x30..0x32].copy_from_slice(b"C3");
    let dir = CellDirectory::parse(&cell, &layout);
    if dir.len() != 3 || !dir.complete || dir.declared_count() != 3 {
        eprintln!("FAIL cell walk: {:?} complete={}", dir.len(), dir.complete);
        fail += 1;
    }
    // CvData: real 77-byte fixture, FORM=01200RG.
    let cv = CvData::parse(&hex(CVDATA_01200RG));
    if cv.form.as_deref() != Some("01200RG") || !cv.valid {
        eprintln!("FAIL cvdata: {:?} valid={}", cv.form, cv.valid);
        fail += 1;
    }
    // Layout header constants.
    let mut lay = vec![0u8; 64];
    lay[0..4].copy_from_slice(&cw_cv::layout::MAGIC);
    lay[8..12].copy_from_slice(&22u32.to_le_bytes());
    lay[20..24].copy_from_slice(&179u32.to_le_bytes());
    let h = LayoutHeader::parse(&lay);
    if !h.magic_ok || h.version != 22 || h.block_size != 179 {
        eprintln!("FAIL layout header: {h:?}");
        fail += 1;
    }
    if fail == 0 {
        println!("selftest: 3/3 passed");
        ExitCode::SUCCESS
    } else {
        println!("selftest: {fail} failed");
        ExitCode::from(1)
    }
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

const CVDATA_01200RG: &str = "\
010000000000010000000000000000000000000000000100464f524d00303132303052470056455253494f4e\
00180000001d000000250000002d0000003c3c4376446174615f56616c69643e3e";

fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if matches!(
            p.extension().and_then(|x| x.to_str()),
            Some("cvw") | Some("cvv")
        ) {
            out.push(p);
        }
    }
}

