//! F2 gate: the `FP` linked-worksheet registry is template-frozen.
//!
//! Two independent checks, both read-only on the frozen corpora:
//!
//! 1. **Golden DBF** (`tests\golden\dbf\{corp2026,hull2024,mwgh2025}`): the
//!    `(WKS, CELLNO, SEQNO)` key set must be identical across the three
//!    engagements, with the pinned counts (620 rows / 457 cells).
//! 2. **Template families** (`a sample copy`): every engagement folder
//!    that carries an `FP.dbf` must resolve to the same registry signature even
//!    though the `.cvw` template sets differ (that is what "template-frozen"
//!    means), and the template index must decode every shipped document.
//!
//! Skips when a corpus is absent (fresh checkout); nothing touches CaseWare.

use std::path::{Path, PathBuf};

use cw_cv::wks::{LinkedWorksheets, WksRegistry};

fn golden(corpus: &str) -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/dbf")
        .join(corpus);
    let ok = std::fs::read_dir(&dir).ok()?.flatten().any(|e| {
        e.file_name()
            .to_string_lossy()
            .to_ascii_lowercase()
            .ends_with("fp.dbf")
    });
    ok.then_some(dir)
}

fn bridge_dirs() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sandbox/bridge-work");
    let mut out = Vec::new();
    let Ok(dirs) = std::fs::read_dir(&root) else {
        return out;
    };
    for d in dirs.flatten() {
        let p = d.path();
        if p.is_dir() && WksRegistry::find_fp(&p).is_some() {
            out.push(p);
        }
    }
    out.sort();
    out
}

fn load(dir: &Path) -> WksRegistry {
    WksRegistry::from_engagement_dir(dir)
        .unwrap_or_else(|e| panic!("load {}: {e}", dir.display()))
}

#[test]
fn registry_is_frozen_across_golden_corpora() {
    let Some(base) = golden("corp2026") else {
        eprintln!("golden dbf corpus absent; skipping");
        return;
    };
    let corp = load(&base);
    let hull = load(&base.with_file_name("hull2024"));
    let mwgh = load(&base.with_file_name("mwgh2025"));

    // ---- shape law (F2) -------------------------------------------------
    for (name, reg) in [("corp2026", &corp), ("hull2024", &hull), ("mwgh2025", &mwgh)] {
        assert_eq!(reg.rows(), 620, "{name}: FP rows");
        assert_eq!(reg.cell_count(), 457, "{name}: worksheet cells");
        assert_eq!(reg.codes(), vec!["CLP", "PRG", "UD"], "{name}: WKS codes");
        let clp = reg.get("CLP").unwrap();
        assert_eq!((clp.rows(), clp.cell_count()), (571, 441), "{name}: CLP");
        assert_eq!(clp.cell_range(), Some((2, 444)), "{name}: CLP range");
        let ud = reg.get("UD").unwrap();
        assert_eq!((ud.rows(), ud.cell_count()), (48, 15), "{name}: UD");
        assert_eq!(ud.cell_range(), Some((1, 15)), "{name}: UD range");
        let prg = reg.get("PRG").unwrap();
        assert_eq!((prg.rows(), prg.cell_count()), (1, 1), "{name}: PRG");
        assert_eq!(reg.signature().len(), 620, "{name}: signature keys");
    }

    // ---- template-frozen identity: keys equal, values engagement-owned ---
    assert!(corp.delta(&hull).is_empty(), "corp2026 vs hull2024 key drift");
    assert!(corp.delta(&mwgh).is_empty(), "corp2026 vs mwgh2025 key drift");
    assert_eq!(corp.signature(), hull.signature());

    // ---- value reconstruction (SEQNO chunks join without separator) ------
    assert_eq!(
        corp.text("CLP", 2).as_deref(),
        Some("Midco International   Inc"),
        "corp2026 client name (22-byte chunk boundary)"
    );
    assert!(hull
        .text("CLP", 2)
        .unwrap()
        .starts_with("Hull Lift Truck"));
    // A cell whose chunks are all blank resolves to the empty string.
    let mut blank_cells = 0;
    for ws in corp.worksheets() {
        for c in ws.cells() {
            if c.text().is_empty() {
                blank_cells += 1;
            }
        }
    }
    assert!(blank_cells > 0, "expected blank CLP/UD cells in the corpus");
    // Unknown keys are None, never a panic.
    assert_eq!(corp.text("NOPE", 1), None);
    assert_eq!(corp.text("CLP", 9999), None);
}

#[test]
fn registry_resolves_across_every_template_family() {
    let dirs = bridge_dirs();
    if dirs.is_empty() {
        eprintln!("bridge-work corpus absent; skipping");
        return;
    }
    assert!(
        dirs.len() >= 3,
        "need >=3 engagement folders with FP, found {}",
        dirs.len()
    );

    let mut families: std::collections::BTreeSet<Vec<String>> = Default::default();
    let mut baseline: Option<Vec<(String, u32, u32)>> = None;
    let mut template_files = 0usize;
    for dir in &dirs {
        let loaded = LinkedWorksheets::load(dir)
            .unwrap_or_else(|e| panic!("load {}: {e}", dir.display()))
            .expect("folder has FP.dbf");
        assert_eq!(
            loaded.registry.rows(),
            620,
            "{}: unexpected FP row set",
            dir.display()
        );
        let sig = loaded.registry.signature();
        match &baseline {
            None => baseline = Some(sig),
            Some(base) => assert_eq!(*base, sig, "{}: registry drifted", dir.display()),
        }
        families.insert(loaded.templates.forms().iter().map(|s| s.to_string()).collect());
        template_files += loaded.templates.len();
    }

    // The point of the test: values are engagement-owned, the key registry is
    // not — so the same registry serves several different template packs.
    assert!(
        families.len() >= 2,
        "expected distinct template families, saw {:?}",
        families
    );
    assert!(template_files >= 3, "template index is empty");

    // Golden DBF signature and on-disk engagement signature agree.
    if let Some(base) = golden("corp2026") {
        let golden_sig = load(&base).signature();
        assert_eq!(baseline.unwrap(), golden_sig, "corpus vs golden signature");
    }
}

