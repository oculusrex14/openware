//! cw-core ??the OpenCW data kernel.
//!
//! Owns every engagement file format, reconstructed from the shipped product on-disk formats
//! (see `docs\specs\`, `evidence\the reference notes`): the 22 DBF tables + memo/index
//! sidecars, the `.ac_` compressed container (F24), the CWGUID allocator
//! (22-char, F59), the interlink resolver (F32) and the BL balance cube
//! NUMBER x ENTITY x YEAR x BUCKET x PERIOD x TYPE.
//!
//! Port contract (Plan.md §10): every behavior here must reproduce the
//! Python-frozen golden fixtures under `tests\golden`; the bridge/agents
//! harness (`src\bridge`, `src\agents`) is the regression oracle.

pub mod canon;
pub mod cfb;
pub mod cube;
pub mod dbf;
pub mod error;
pub mod fpt;
pub mod guid;
pub mod inflate;
pub mod interlink;
pub mod json;
pub mod lzma;
pub mod pkg;
pub mod sha256;
pub mod transact;

pub use error::{CoreError, Result};

/// Version of the workspace this crate belongs to.
pub const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Health probe: the kernel parses when it can round-trip a synthetic table.
pub fn healthy() -> bool {
    let bytes = dbf::synthetic_fixture();
    dbf::Table::parse(&bytes)
        .map(|t| t.encode() == bytes)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    #[test]
    fn kernel_boots() {
        assert!(super::healthy());
        assert!(!super::CRATE_VERSION.is_empty());
    }

    #[test]
    fn version_is_semverish() {
        // Guards the workspace version plumbing (Cargo env re-export).
        let v = super::CRATE_VERSION;
        assert!(v.contains('.'), "got {v}");
    }
}
// re-key touch 0.1.3b (spawn identity refresh, the tooling scripts\README.md)

// re-key touch 0.1.3c (runner 21:57)

// 0.1.7 re-key (deny-window rotation; pkg/cfb/transact landed 0.1.6)

// 0.1.8 re-key (Iter-54b closeout: pkgread/cube_golden green-record attempt)

