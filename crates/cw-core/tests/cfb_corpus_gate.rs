//! G1 gate: the CFB reader against **every** shipped `.cvw` in the expanded
//! corpus.
//!
//! ## What was broken
//!
//! `cw_core::cfb` used to abort a document's directory sweep the moment a slot
//! failed MS-CFB validation, and it aborted any sector chain whose FAT link ran
//! past the FAT table. Caseware's shipped `.cvw` files break both assumptions:
//!
//! * the directory chain links the real directory sectors as a **prefix** of a
//!   longer chain that then runs into ordinary data (hull2024 `03250RG1`:
//!   `1 → 2 → 5 → 310 → 348 → 381`, and sector 381 carries packed cell bytes,
//!   so its slots read `kind = 68 / 58 / 248`);
//! * the FAT's last sector is short of the file's sector count, so streams at
//!   the end of a document (and the chain that lists the directory itself) end
//!   at an index past the FAT.
//!
//! Measured on the shipped bytes: **29 of 34** documents failed to parse
//! (`cfb: dir id 20 missing`, `cfb: fat miss …`). Even the five that "parsed"
//! lost `Form/Layout`, the largest stream in every document, because the
//! Layout sector chain ran past the FAT.
//!
//! ## What this gate pins
//!
//! 1. all 34 corpus `.cvw` parse;
//! 2. every directory entry that a document's tree reaches resolves and every
//!    one of those streams reads;
//! 3. the corpus-wide entry-name inventory is **closed** — exactly the 28 names
//!    the directory tree holds, with no invented 29th;
//! 4. the hull2024 `03250RG1` document — the file whose trailing partial
//!    directory sector is the measured root cause — reaches its 17 entries and
//!    reads its `Form/Layout`.
//!
//! The corpus lives outside the frozen fixture set, so the gate skips cleanly
//! when `expanded/` is absent (the same contract the `cw-cv` corpus gates use).
//! Nothing here writes: it only reads.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn expanded() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../expanded")
}

/// Every `.cvw` under `expanded/<engagement>/`, sorted for determinism.
fn corpus() -> Vec<PathBuf> {
    let root = expanded();
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

fn find(suffix: &str) -> Option<PathBuf> {
    corpus()
        .into_iter()
        .find(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(suffix)))
}

/// The closed `.cvw` directory inventory: every node name the corpus tree
/// reaches, across all 34 documents. Root excluded.
///
/// `attachments/graph-data-source.md` §1 enumerates the same set and calls it
/// "32 names"; its own list totals **28** (21 leaf names + six `Scripts/*` +
/// `On Opening`), which is what the bytes hold. The 32 came from a
/// whole-file scan that also counted `Root Entry` and three 4-byte junk
/// "names" (`%`, `&`, `2`) sitting at `kind = 0` inside stream payloads. This
/// gate pins the directory truth, not the scan artefact.
///
/// 22 streams + 6 storages: there is no `Chart`, `OLEObject`, `Shapes`,
/// `Series`, `DataRange` or `Graph` entry in any shipped document, and the
/// `Media` storage is childless in all of them.
const INVENTORY: &[&str] = &[
    "AJEAdd",
    "AJECSV",
    "AJEDelete",
    "AJEUpdate",
    "Annotate",
    "Bookmarks",
    "Cell",
    "CheckTCodes",
    "CheckTaxCodes",
    "CvData",
    "Data",
    "Features",
    "Form",
    "Index",
    "Layout",
    "Media",
    "Note",
    "NoteNumStyles",
    "On Opening",
    "Para",
    "Scripts",
    "Sect",
    "Strings",
    "Styles",
    "Table",
    "XData",
    "XMLInfo",
    "XMLStore",
];

#[test]
fn all_corpus_cvw_parse() {
    let files = corpus();
    if files.is_empty() {
        eprintln!("expanded/ corpus absent; skipping");
        return;
    }
    assert_eq!(
        files.len(),
        34,
        "corpus shrank or grew; the reader contract is pinned on 34 documents"
    );

    let mut streams_total = 0usize;
    let mut storages_total = 0usize;
    for p in &files {
        let bytes = std::fs::read(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
        let ctx = p.file_name().unwrap().to_string_lossy().into_owned();
        let cfb = cw_core::cfb::Cfb::parse(&bytes)
            .unwrap_or_else(|e| panic!("{ctx}: parse: {e}"));
        let nodes = cfb
            .nodes()
            .unwrap_or_else(|e| panic!("{ctx}: nodes: {e}"));
        assert!(
            !nodes.is_empty(),
            "{ctx}: a CaseView document always carries a directory tree"
        );
        // Every stream in the tree must resolve to bytes. A stream whose chain
        // is cut by the file's short tail yields the bytes present, never an
        // error and never fabricated padding.
        for e in &nodes {
            match e.kind {
                cw_core::cfb::EntryKind::Stream => {
                    let data = cfb
                        .stream(&e.name())
                        .unwrap_or_else(|er| panic!("{ctx}/{}: {er}", e.name()));
                    assert!(
                        data.len() <= e.len,
                        "{ctx}/{}: read {} > declared {}",
                        e.name(),
                        data.len(),
                        e.len
                    );
                    streams_total += 1;
                }
                cw_core::cfb::EntryKind::Storage => {
                    assert_eq!(e.len, 0, "{ctx}/{}: a storage has no length", e.name());
                    storages_total += 1;
                }
            }
        }
    }
    eprintln!(
        "parsed {} files: {streams_total} streams, {storages_total} storages",
        files.len()
    );
    // Six root storages per document: Form, Data, Scripts, Index, Media,
    // XMLStore. They are present even when their `child` link is a hole.
    assert_eq!(storages_total, 6 * 34, "six root storages per document");
}

#[test]
fn corpus_entry_inventory_is_closed() {
    let files = corpus();
    if files.is_empty() {
        eprintln!("expanded/ corpus absent; skipping");
        return;
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for p in &files {
        let bytes = std::fs::read(&p).unwrap();
        let cfb = cw_core::cfb::Cfb::parse(&bytes)
            .unwrap_or_else(|e| panic!("{}: parse: {e}", p.display()));
        for e in cfb.nodes().unwrap() {
            for part in e.path {
                seen.insert(part);
            }
        }
    }
    let want: BTreeSet<String> = INVENTORY.iter().map(|s| s.to_string()).collect();
    assert_eq!(seen, want, "the .cvw directory inventory moved");
    assert_eq!(seen.len(), 28, "inventory size");
}

/// The document whose trailing directory sector aborts the old sweep, pinned
/// end to end: full tree, `Form/Layout` and `Index/Cell` both readable.
#[test]
fn hull_03250rg1_trailing_partial_dir_sector() {
    let Some(p) = find("03250RG1.cvw").filter(|p| p.to_string_lossy().contains("Hull")) else {
        eprintln!("hull2024 03250RG1.cvw absent; skipping");
        return;
    };
    let bytes = std::fs::read(&p).unwrap();
    let cfb = cw_core::cfb::Cfb::parse(&bytes).unwrap_or_else(|e| panic!("parse: {e}"));

    // The header names 4 FAT sectors and the first directory sector is 1; the
    // directory chain is 1→2→5→310→348→381 and 381 is not a directory sector.
    assert_eq!(&bytes[0..8], &cw_core::cfb::SIGNATURE, "signature");
    assert_eq!(u32::from_le_bytes([bytes[44], bytes[45], bytes[46], bytes[47]]), 4, "num_fat");
    assert_eq!(u32::from_le_bytes([bytes[48], bytes[49], bytes[50], bytes[51]]), 1, "first_dir");

    let names: Vec<String> = cfb.nodes().unwrap().into_iter().map(|e| e.name()).collect();
    // 17 nodes = 6 storages (Form Data Scripts Index Media XMLStore) + 11 leaves.
    assert_eq!(names.len(), 17, "node count: {names:?}");
    let set: BTreeSet<&str> = names.iter().map(|s| s.as_str()).collect();
    for want in [
        "Form",
        "Data",
        "Scripts",
        "Index",
        "Media",
        "XMLStore",
        "Form/Layout",
        "Form/Styles",
        "Form/XData",
        "Index/Cell",
        "Index/CvData",
        "Index/Para",
        "Index/Sect",
        "Index/Table",
        "Index/Note",
        "Index/Bookmarks",
        "XMLStore/XMLInfo",
    ] {
        assert!(set.contains(want), "missing {want} in {names:?}");
    }

    // The stream the document cannot be decoded without.
    let cell = cfb.stream("Index/Cell").expect("Index/Cell");
    assert_eq!(cell.len(), 1_325, "Cell length");
    // Cell directory header: version 4, marker 114 (the F1 laws).
    assert_eq!(&cell[0..4], &4u32.to_le_bytes(), "cell version");
    assert_eq!(&cell[4..8], &114u32.to_le_bytes(), "cell marker");

    // `Form/Layout` is declared 184,013 bytes but the file's FAT stops at index
    // 401: the 4th DIFAT sector (384) holds stream bytes, not a FAT, so the last
    // ~1.5 sectors of Layout's chain are unrecoverable. The reader returns the
    // 183,808 bytes the container actually addresses — short, never padded. The
    // shortfall is exactly the file's partial tail (206,541 = 403*512 + 205).
    let layout = cfb.stream("Form/Layout").expect("Form/Layout");
    assert_eq!(layout.len(), 183_808, "Layout readable length");
    assert_eq!(bytes.len() - 512 * 403, 205, "the file's partial tail");

    // The garbage directory sector's slots are holes: the id space covers them,
    // the entry census does not.
    assert_eq!(cfb.dir_len(), 20, "slots swept before the first all-hole sector");
    assert_eq!(cfb.dir_entries(), 20, "all 20 swept slots are valid entries");
    assert_eq!(cfb.entries().unwrap().len(), 11, "leaf entries");
}

/// A second, independent document whose directory chain also runs into data,
/// so the fix is not tuned to one file: hull2024 `01200RG0` has the dangling-link
/// shape (`XMLStore → 18` and `Layout → 19` point at holes).
#[test]
fn hull_01200rg0_dangling_dir_links() {
    let Some(p) = find("01200RG0.cvw").filter(|p| p.to_string_lossy().contains("Hull")) else {
        eprintln!("hull2024 01200RG0.cvw absent; skipping");
        return;
    };
    let bytes = std::fs::read(&p).unwrap();
    let cfb = cw_core::cfb::Cfb::parse(&bytes).unwrap_or_else(|e| panic!("parse: {e}"));
    let names: BTreeSet<String> = cfb.nodes().unwrap().into_iter().map(|e| e.name()).collect();
    let want: BTreeSet<String> = [
        "Data",
        "Form",
        "Form/Layout",
        "Form/Styles",
        "Index",
        "Index/Bookmarks",
        "Index/Cell",
        "Index/CvData",
        "Index/Para",
        "Index/Sect",
        "Media",
        "Scripts",
        "XMLStore",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(names, want, "tree shape");
    // Layout's chain legitimately ends one sector before the FAT does; the read
    // returns the 237,568 bytes the container holds, not 238,192 fabricated ones.
    let layout = cfb.stream("Form/Layout").unwrap();
    assert_eq!(layout.len(), 237_568, "short tail is reported as read, not padded");
}

// ---------------------------------------------------------------------------
// Byte-identity pin for every stream that read BEFORE the tolerant sweep.
//
// The defect analysis was measured against the pre-change reader: five
// documents parsed and 76 of their streams read; every other document errored
// out. The fix must not move a single byte of those 76. Each tuple is
// `(document file name, stream path, length, sha256)`, captured from the reader
// as it stood before `cfb.rs` was made tolerant and re-measured after.
//
// `Form/Layout` is absent from this pin on purpose: it is the one stream the
// old reader could not resolve even in the five "parsing" documents
// (`cfb: sector 94 out of range`), because its chain ran past the FAT.
// ---------------------------------------------------------------------------
const PRE_CHANGE_STREAMS: &[(&str, &str, usize, &str)] = &[
    ("2024 Hull Lift Truck Review(1148441)EXPO0IS0.cvw", "Form/Annotate", 4, "DF3F619804A92FDB4057192DC43DD748EA778ADC52BC498CE80524C014B81119"),
    ("2024 Hull Lift Truck Review(1148441)EXPO0IS0.cvw", "Form/Features", 152, "ADA4DB449EBC01B71590496DA79A0B62C956FFBF49CD2B25F099C13C25749099"),
    ("2024 Hull Lift Truck Review(1148441)EXPO0IS0.cvw", "Form/NoteNumStyles", 6, "287A3387A6A65E0B5483D02992ED2649EF4A9F48D79C7C1D3484ACFAEC7367D3"),
    ("2024 Hull Lift Truck Review(1148441)EXPO0IS0.cvw", "Form/Strings", 2591, "0F3E78D5F1847B728EE6A43D787487D8F5893D4496E92B6B674026A57AF1CECC"),
    ("2024 Hull Lift Truck Review(1148441)EXPO0IS0.cvw", "Form/Styles", 894, "C699CDB9A2FAD94A2F2686D76332B69C0144176E51ABD1264C89C799999B5810"),
    ("2024 Hull Lift Truck Review(1148441)EXPO0IS0.cvw", "Form/XData", 12, "31190090408F837AA1DC44BF75404AF354A0E5D5C965F1D407A97DD627554DA5"),
    ("2024 Hull Lift Truck Review(1148441)EXPO0IS0.cvw", "Index/Bookmarks", 46, "E79CA10FE802C68F47646A214D7A87B1EAF341BD49C8BC52C0E5115889DFAA66"),
    ("2024 Hull Lift Truck Review(1148441)EXPO0IS0.cvw", "Index/Cell", 486, "889C0DE439ADF0C2046B66B13CF552CFF6B39A6A5102D44D5597F97E21B54633"),
    ("2024 Hull Lift Truck Review(1148441)EXPO0IS0.cvw", "Index/CvData", 0, "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855"),
    ("2024 Hull Lift Truck Review(1148441)EXPO0IS0.cvw", "Index/Note", 12, "15EC7BF0B50732B49F8228E07D24365338F9E3AB994B00AF08E5A3BFFE55FD8B"),
    ("2024 Hull Lift Truck Review(1148441)EXPO0IS0.cvw", "Index/Para", 236, "C1651AAA740A8B9958BE69CF7C577C0F6D4D0CE1EC42D6C6C0AA11ED5955293E"),
    ("2024 Hull Lift Truck Review(1148441)EXPO0IS0.cvw", "Index/Sect", 92, "BAAD1B5C7BC3373046A10ACF53C104234722DAC91AF621D54E5B9FF5C4E590CA"),
    ("2024 Hull Lift Truck Review(1148441)EXPO0IS0.cvw", "Index/Table", 60, "5C4BD18F4EECB89820293D649439E39C8F5F778432C4A68E56D811083C221198"),
    ("2024 Hull Lift Truck Review(1148441)EXPO0IS0.cvw", "XMLStore/XMLInfo", 8, "7C9FA136D4413FA6173637E883B6998D32E1D675F88CDDFF9DCBCF331820F4B8"),
    ("2024 Hull Lift Truck Review(1148441)FJEIMP.cvw", "Form/Annotate", 4, "DF3F619804A92FDB4057192DC43DD748EA778ADC52BC498CE80524C014B81119"),
    ("2024 Hull Lift Truck Review(1148441)FJEIMP.cvw", "Form/Features", 152, "EF2BA0663924B08E132A3CA49FF96451587D22CA7E216C44F662CC27D1168E5A"),
    ("2024 Hull Lift Truck Review(1148441)FJEIMP.cvw", "Form/NoteNumStyles", 6, "287A3387A6A65E0B5483D02992ED2649EF4A9F48D79C7C1D3484ACFAEC7367D3"),
    ("2024 Hull Lift Truck Review(1148441)FJEIMP.cvw", "Form/Strings", 756, "34C529F72F5599FE71667A309ABA64A46C21ED43E2468BC6C1B2352712054ABC"),
    ("2024 Hull Lift Truck Review(1148441)FJEIMP.cvw", "Form/Styles", 10071, "F400F45924FD5584A3AB0309F3FD87AC47103380C5179E19C555D07A4A491D3D"),
    ("2024 Hull Lift Truck Review(1148441)FJEIMP.cvw", "Form/XData", 12, "31190090408F837AA1DC44BF75404AF354A0E5D5C965F1D407A97DD627554DA5"),
    ("2024 Hull Lift Truck Review(1148441)FJEIMP.cvw", "Index/Bookmarks", 12, "CA888F40C3CACA805B37A5434C75DE5550616E0795E7602FB91156F22DD90851"),
    ("2024 Hull Lift Truck Review(1148441)FJEIMP.cvw", "Index/Cell", 84, "2D7584F5A3FA031A974A226538E9DF590248A994F8594DFC0BF399654104E232"),
    ("2024 Hull Lift Truck Review(1148441)FJEIMP.cvw", "Index/CvData", 80, "698F7EAA2E500613708A3747171D6C8476B412CC595B9E471A50BD2BA83313BC"),
    ("2024 Hull Lift Truck Review(1148441)FJEIMP.cvw", "Index/Note", 12, "15EC7BF0B50732B49F8228E07D24365338F9E3AB994B00AF08E5A3BFFE55FD8B"),
    ("2024 Hull Lift Truck Review(1148441)FJEIMP.cvw", "Index/Para", 104, "8C4131CD61AE918CB64A305513686744C431F0788964E520092ACC77E09A5EEE"),
    ("2024 Hull Lift Truck Review(1148441)FJEIMP.cvw", "Index/Sect", 12, "607F1FE7C0FE6DCA9ED44045F8EED531539D9EF8F0FF30A041F0C89B1024F0CA"),
    ("2024 Hull Lift Truck Review(1148441)FJEIMP.cvw", "Index/Table", 28, "BCEE2AE19D86EACCD203AB5952C32E25F60725463887AA0E63833AFC1EB8F55A"),
    ("2024 Hull Lift Truck Review(1148441)FJEIMP.cvw", "Scripts/AJEAdd", 1156, "837F2838FB730E09E0E843EA7435EDA7C0A20DFE4A0E046F1BA6DE8E1388D479"),
    ("2024 Hull Lift Truck Review(1148441)FJEIMP.cvw", "Scripts/AJECSV", 794, "10DA9AC17F4E7C3E8FDF9D39ECA99EF369ED1D0C790DB3A1040D7B6C4516622A"),
    ("2024 Hull Lift Truck Review(1148441)FJEIMP.cvw", "Scripts/AJEDelete", 457, "FEF415E60306D2131D4DE43EB947A7CCF7C4E079D4790D72EE6E053F468B195D"),
    ("2024 Hull Lift Truck Review(1148441)FJEIMP.cvw", "Scripts/AJEUpdate", 917, "62FEDB4EEB0168BFECD367E38BD5DBAE2AFAA4BC323D6D1860D45DD25EE031D8"),
    ("2024 Hull Lift Truck Review(1148441)FJEIMP.cvw", "Scripts/CheckTCodes", 1011, "E8F81C3216108E55CB7EFA758F2CE8CCCC7FE1F38FEAC4AAD3EFBFC906543811"),
    ("2024 Hull Lift Truck Review(1148441)FJEIMP.cvw", "Scripts/CheckTaxCodes", 947, "7257D5F8A4C7F192A5CF4D2C735D9452B35256AC763661345CA4D0707651BB76"),
    ("2024 Hull Lift Truck Review(1148441)FJEIMP.cvw", "XMLStore/XMLInfo", 8, "7C9FA136D4413FA6173637E883B6998D32E1D675F88CDDFF9DCBCF331820F4B8"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)EXPO0IS0.cvw", "Form/Annotate", 4, "DF3F619804A92FDB4057192DC43DD748EA778ADC52BC498CE80524C014B81119"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)EXPO0IS0.cvw", "Form/Features", 152, "ADA4DB449EBC01B71590496DA79A0B62C956FFBF49CD2B25F099C13C25749099"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)EXPO0IS0.cvw", "Form/NoteNumStyles", 6, "287A3387A6A65E0B5483D02992ED2649EF4A9F48D79C7C1D3484ACFAEC7367D3"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)EXPO0IS0.cvw", "Form/Strings", 2588, "6DD09286C1F0202A324D0882FF61E69D5D4E9C41784031E04440EA7E79A124FD"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)EXPO0IS0.cvw", "Form/Styles", 894, "15018B37494B3F105C294F8B0F5A00571ADF3D95C28F33C52EB319F5A5B6E1B0"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)EXPO0IS0.cvw", "Form/XData", 12, "31190090408F837AA1DC44BF75404AF354A0E5D5C965F1D407A97DD627554DA5"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)EXPO0IS0.cvw", "Index/Bookmarks", 46, "E79CA10FE802C68F47646A214D7A87B1EAF341BD49C8BC52C0E5115889DFAA66"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)EXPO0IS0.cvw", "Index/Cell", 486, "058E15FA205F7D0B929715944AA4746839BD1DB8D20966251655477BC88CB19C"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)EXPO0IS0.cvw", "Index/CvData", 0, "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)EXPO0IS0.cvw", "Index/Note", 12, "15EC7BF0B50732B49F8228E07D24365338F9E3AB994B00AF08E5A3BFFE55FD8B"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)EXPO0IS0.cvw", "Index/Para", 236, "C377A808C881527C34C4A50BA6CDF3CB7C906D99657D22D4653C49BABAD45D6E"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)EXPO0IS0.cvw", "Index/Sect", 92, "082559408022D6F2B521F79F005F54143D424C131EF15C4F405353C35FD96D73"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)EXPO0IS0.cvw", "Index/Table", 60, "B4C947EB311123F7CC583C54DBD6B63442CD4128F7DFCD097C525A5BA6F1A2FB"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)EXPO0IS0.cvw", "XMLStore/XMLInfo", 8, "7C9FA136D4413FA6173637E883B6998D32E1D675F88CDDFF9DCBCF331820F4B8"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)990GG0.cvw", "Form/Annotate", 4, "DF3F619804A92FDB4057192DC43DD748EA778ADC52BC498CE80524C014B81119"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)990GG0.cvw", "Form/Features", 152, "ADA4DB449EBC01B71590496DA79A0B62C956FFBF49CD2B25F099C13C25749099"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)990GG0.cvw", "Form/NoteNumStyles", 6, "287A3387A6A65E0B5483D02992ED2649EF4A9F48D79C7C1D3484ACFAEC7367D3"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)990GG0.cvw", "Form/Styles", 6009, "9353267CD72A8434D3E3A5A5A1C225FE74A28175199A63ECB6EF459D680DB7BC"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)990GG0.cvw", "Form/XData", 12, "31190090408F837AA1DC44BF75404AF354A0E5D5C965F1D407A97DD627554DA5"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)990GG0.cvw", "Index/Bookmarks", 102, "F4DA4D8F9DB22B6F358F899ED2051FBAE64E4E84D003563FC38A696807992E9C"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)990GG0.cvw", "Index/Cell", 10536, "3C4EF396BE01486F80FACF3E25257208A1CD7734714536318302F369942D1E60"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)990GG0.cvw", "Index/CvData", 0, "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)990GG0.cvw", "Index/Note", 51, "F3F7405CF87B9C441EF9FEDAD46F8463BECCF7E9EE1D69A139BE38AB797165A1"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)990GG0.cvw", "Index/Para", 15816, "0951792738B0B931AEDCD6B1572BCCCF11101E95AA36C35AF94A5856E3625935"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)990GG0.cvw", "Index/Sect", 412, "50C8F6F2B9CB6314C8142B77E5CC541FFEEABB3232DB6B0605B29D12D69858E6"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)990GG0.cvw", "Index/Table", 364, "875984EC1AC836045C1B343052760FF0029603996BC6889A683AE95F34513082"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)990GG0.cvw", "Scripts/On Opening", 185, "29417D4DFDDC588F84F0B54C7FAB457D4514B8F70B4E5EFD28D29EB49B1FE73B"),
    ("2025 Viewrail Holdings Inc and Subsidi(1243194)990GG0.cvw", "XMLStore/XMLInfo", 8, "7C9FA136D4413FA6173637E883B6998D32E1D675F88CDDFF9DCBCF331820F4B8"),
    ("MWGH LLC 9-30-25 REVIEW(1239263)990GG0.cvw", "Form/Annotate", 4, "DF3F619804A92FDB4057192DC43DD748EA778ADC52BC498CE80524C014B81119"),
    ("MWGH LLC 9-30-25 REVIEW(1239263)990GG0.cvw", "Form/Features", 152, "ADA4DB449EBC01B71590496DA79A0B62C956FFBF49CD2B25F099C13C25749099"),
    ("MWGH LLC 9-30-25 REVIEW(1239263)990GG0.cvw", "Form/NoteNumStyles", 6, "287A3387A6A65E0B5483D02992ED2649EF4A9F48D79C7C1D3484ACFAEC7367D3"),
    ("MWGH LLC 9-30-25 REVIEW(1239263)990GG0.cvw", "Form/Styles", 6009, "15EC0A9B6EE48AAA560EEC5FB37064B5AF0CBF89D114C71CA1977019E483504E"),
    ("MWGH LLC 9-30-25 REVIEW(1239263)990GG0.cvw", "Form/XData", 12, "31190090408F837AA1DC44BF75404AF354A0E5D5C965F1D407A97DD627554DA5"),
    ("MWGH LLC 9-30-25 REVIEW(1239263)990GG0.cvw", "Index/Bookmarks", 102, "B1EF9196CA54FF9BA1FAD67888891F301305EB0A3104166B511EF2BDC1A28760"),
    ("MWGH LLC 9-30-25 REVIEW(1239263)990GG0.cvw", "Index/Cell", 9534, "06DE2D3CE57877E4AA8BE0E209B52C84CDE1E19B2FF0AC33CF7C63585A43A3A9"),
    ("MWGH LLC 9-30-25 REVIEW(1239263)990GG0.cvw", "Index/CvData", 0, "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855"),
    ("MWGH LLC 9-30-25 REVIEW(1239263)990GG0.cvw", "Index/Note", 51, "F3F7405CF87B9C441EF9FEDAD46F8463BECCF7E9EE1D69A139BE38AB797165A1"),
    ("MWGH LLC 9-30-25 REVIEW(1239263)990GG0.cvw", "Index/Para", 13840, "17751849D6B926571BD974C27310154BD55C64E0D54915266F00DF38642C1557"),
    ("MWGH LLC 9-30-25 REVIEW(1239263)990GG0.cvw", "Index/Sect", 412, "4E220FE9D5AA1ED02592943BD9AEF6D552982A2AFD9AB3829DD823C3CC3C0DAB"),
    ("MWGH LLC 9-30-25 REVIEW(1239263)990GG0.cvw", "Index/Table", 364, "9934B2FB0C0ECE87309B6F06AC75C5B6DC49DCFEDF3573C5DC28BD67876386EC"),
    ("MWGH LLC 9-30-25 REVIEW(1239263)990GG0.cvw", "Scripts/On Opening", 185, "B629C84827D3C17324D3441D71AA8B6C53A7222C682486EA060D0DC8AAB764B5"),
    ("MWGH LLC 9-30-25 REVIEW(1239263)990GG0.cvw", "XMLStore/XMLInfo", 8, "7C9FA136D4413FA6173637E883B6998D32E1D675F88CDDFF9DCBCF331820F4B8"),
];

#[test]
fn streams_that_read_before_are_byte_identical() {
    let files = corpus();
    if files.is_empty() {
        eprintln!("expanded/ corpus absent; skipping");
        return;
    }
    let mut checked = 0usize;
    for (doc_name, stream, want_len, want_sha) in PRE_CHANGE_STREAMS {
        let Some(p) = files
            .iter()
            .find(|p| p.file_name().and_then(|n| n.to_str()) == Some(doc_name))
        else {
            panic!("{doc_name}: pinned document missing");
        };
        let bytes = std::fs::read(p).unwrap();
        let cfb = cw_core::cfb::Cfb::parse(&bytes)
            .unwrap_or_else(|e| panic!("{doc_name}: {e}"));
        let data = cfb
            .stream(stream)
            .unwrap_or_else(|e| panic!("{doc_name}/{stream}: {e}"));
        assert_eq!(data.len(), *want_len, "{doc_name}/{stream}: length drifted");
        assert_eq!(
            cw_core::sha256::sha256_hex(&data).to_uppercase(),
            *want_sha,
            "{doc_name}/{stream}: bytes drifted"
        );
        checked += 1;
    }
    assert_eq!(checked, PRE_CHANGE_STREAMS.len(), "every pin must be exercised");
    assert_eq!(checked, 76, "the pre-change readable set was 76 streams");
}
