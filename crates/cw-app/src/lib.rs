//! cw-app — the engagement domain layer on top of [`cw_core`].
//!
//! Phases P5.D (lifecycle), P5.E (trial-balance app layer), P5.G (review),
//! P5.H (entities/consolidation/FX) land here; behaviors port 1:1 from the
//! COM fixtures (`docs\specs\*.md`) against the golden corpus.

pub mod accounts;
pub mod adjustments;
pub mod backup;
pub mod consolidation;
pub mod documents;
pub mod entities;
pub mod fund;
pub mod fx;
pub mod grouping_assign;
pub mod groups;
pub mod history;
pub mod import;
pub mod lifecycle;
pub mod lockdown;
pub mod mappings;
pub mod program;
pub mod protection;
pub mod recalc;
pub mod renumber;
pub mod review;
pub mod signoff;
pub mod tax;
pub mod taxexport;
pub mod tb;
pub mod yearend;

/// Domain smoke — the app layer is alive only while the kernel is.
pub fn healthy() -> bool {
    cw_core::healthy()
}

#[cfg(test)]
mod tests {
    #[test]
    fn app_boots_on_kernel() {
        assert!(super::healthy());
    }
}
// iter50 relink (spawn-deny stubborn case, see tools/rust/README.md)

