//! Throwaway corpus tool: expand .ac_ packages via the kernel (proof + fixture regeneration).
use cw_app::backup::restore_package;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    for arg in args.iter().skip(1) {
        let pkg = PathBuf::from(arg);
        let stem = pkg.file_stem().unwrap().to_string_lossy().to_string();
        let name: String = stem.replace(' ', "_");
        let dst = PathBuf::from("expanded").join(&name);
        std::fs::create_dir_all(&dst).unwrap();
        match restore_package(&pkg, &dst) {
            Ok(rep) => println!(
                "{} -> {} | members={} crc_mismatch={}",
                pkg.display(),
                dst.display(),
                rep.members.len(),
                rep.crc_mismatch.len()
            ),
            Err(e) => println!("{} FAILED: {e}", pkg.display()),
        }
    }
}
