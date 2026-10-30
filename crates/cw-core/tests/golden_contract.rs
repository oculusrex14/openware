//! Port contract check (Plan.md §10): the golden-fixture root must exist and
//! carry its contract README. C1+ freeze bridge/agent outputs here and every
//! kernel behavior change rides on a green diff against them.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("repo root resolvable from crates/cw-core")
}

fn golden_dir() -> PathBuf {
    repo_root().join("tests").join("golden")
}

#[test]
fn golden_contract_root_present() {
    let readme = golden_dir().join("README.md");
    assert!(
        readme.is_file(),
        "missing port-contract README: {}",
        readme.display()
    );
}

#[test]
fn fixture_areas_registered() {
    // Areas accumulate as C1+ freeze bridges outputs; the manifest lists them.
    let manifest = golden_dir().join("MANIFEST.toml");
    assert!(
        manifest.is_file(),
        "missing golden manifest: {}",
        manifest.display()
    );
}

