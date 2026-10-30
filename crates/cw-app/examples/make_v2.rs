//! V2 acceptance artifacts: (A) faithful repack, (B) same + one visible AC_DESC edit.
use cw_app::backup::{package_engagement, PackageOptions};
use cw_core::dbf::Table;
use std::path::Path;

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap().flatten() {
        let p = e.path();
        if p.is_file() {
            std::fs::copy(&p, dst.join(p.file_name().unwrap())).unwrap();
        }
    }
}

fn am_path(dir: &Path) -> std::path::PathBuf {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_uppercase().ends_with("AM.DBF"))
                .unwrap_or(false)
        })
        .expect("AM table")
}

fn main() {
    let src = Path::new("expanded/2024_Hull_Lift_Truck_Review(1148441)");
    let out = Path::new("v2");
    std::fs::create_dir_all(out).unwrap();

    // A — faithful repack
    let adir = out.join("A-repack");
    copy_tree(src, &adir);
    let opts = PackageOptions::default();
    let rep = package_engagement(&adir, &opts).unwrap_or_else(|e| panic!("package A: {e:?}"));
    println!("A: entries={} -> {}", rep.entries.len(), rep.pkg_path.display());

    // B — copy, edit one posting account's description, package
    let bdir = out.join("B-edit");
    copy_tree(src, &bdir);
    let am_path = am_path(&bdir);
    let bytes = std::fs::read(&am_path).unwrap();
    let table = Table::parse(&bytes).expect("parse AM");

    let acct_i = table
        .fields
        .iter()
        .position(|f| f.name.eq_ignore_ascii_case("AC_NO"))
        .expect("AC_NO");
    let desc_i = table
        .fields
        .iter()
        .position(|f| f.name.eq_ignore_ascii_case("AC_DESC"))
        .expect("AC_DESC");
    let off = |i: usize| -> usize { table.fields[..i].iter().map(|f| f.len as usize).sum() };
    let (a_off, alen) = (off(acct_i), table.fields[acct_i].len as usize);
    let (d_off, dlen) = (off(desc_i), table.fields[desc_i].len as usize);

    let type_i = table
        .fields
        .iter()
        .position(|f| f.name.eq_ignore_ascii_case("TYPE"))
        .expect("TYPE");
    let t_off: usize = table.fields[..type_i].iter().map(|f| f.len as usize).sum();
    let mut census = Vec::new();
    for r in 0..table.count() {
        if table.is_deleted(r) {
            continue;
        }
        let t: Vec<u8> = table.records[r][t_off..t_off + table.fields[type_i].len as usize].to_vec();
        if !t.iter().any(|&b| b == b'B' || b == b'I') {
            continue; // posting accounts only — the probe must be visible in CW
        }
        let raw: String = table.records[r][a_off..a_off + alen]
            .iter()
            .filter(|&&b| b.is_ascii_graphic())
            .map(|&b| b as char)
            .collect();
        census.push((r, raw.trim().to_string()));
    }
    let numeric: Vec<(usize, String)> = census
        .iter()
        .map(|(r, n)| (*r, n.clone()))
        .filter(|(_, n)| !n.is_empty())
        .collect();
    eprintln!("live={} numeric={} first5={:?}", census.len(), numeric.len(), &census[..census.len().min(8)]);
    let (row, which) = numeric.first().unwrap().clone();

    let mut table = table;
    let cell = &mut table.records[row][d_off..d_off + dlen];
    let text = b"V2 EDIT PROBE - OpenCW roundtrip";
    for (i, b) in cell.iter_mut().enumerate() {
        *b = if i < dlen.saturating_sub(1) && i < text.len() { text[i] } else { b' ' };
    }
    let encoded = table.encode();
    std::fs::write(&am_path, &encoded).unwrap();
    drop(table);
    println!("B: edited row {} AC_NO {}", row, which);

    let rep = package_engagement(&bdir, &opts).unwrap_or_else(|e| panic!("package B: {e:?}"));
    println!("B: entries={} -> {}", rep.entries.len(), rep.pkg_path.display());
}

// post-verification hook appended: none (see verify step in shell)
