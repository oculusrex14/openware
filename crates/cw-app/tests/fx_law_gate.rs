//! FX gate: the established foreign-exchange translation law.
//!
//! ## The law being pinned
//!
//! `X64CODEBASE.dll`'s translation routine computes a translated amount with
//! **one** multiply and **one** decimal re-render:
//!
//! ```text
//! if |rate| <= 1e-06:  result = render(amount, dec=2)        ; untranslated
//! else:                result = render(amount * rate, dec=2) ; single multiply
//! ```
//!
//! where `render` is the field renderer, whose `_fcvt` rounds **half away
//! from zero** at the field's declared decimal count.
//!
//! ## Why this gate exists
//!
//! The corpus cannot demonstrate the arithmetic: **no non-zero `C_RATE` exists
//! anywhere in the four engagements**, so every `FXCYTD` in the corpus is the
//! untranslated copy. A corpus-only test would therefore pass for *any*
//! implementation, including a wrong one. This gate pins the law on synthetic
//! vectors derived from the code, and separately pins the corpus-observable
//! consequence (zero rate ⇒ pass-through).
//!
//! ## The rounding distinction that matters
//!
//! `_fcvt` rounds half **away from zero**; Rust's `{:.2}` rounds half to
//! **even**. The two disagree exactly on ties that are representable as
//! doubles, e.g. `0.125 → 0.13` (vendor) vs `0.12` (formatter). A gate that only
//! tested `10.005` would miss this, because `10.005 * 100` is inexact and both
//! rules happen to agree; `0.125` is the discriminating case and is the one
//! pinned below.

use cw_app::fx::{round2, round_half_away, translate_amount, RATE_ZERO_EPSILON};

/// Ties that are *exact* doubles: `k/8` and `k/16` at two decimals.
///
/// `1/8 = 0.125` is the canonical one: binary-exact, exactly halfway, and the
/// two rounding conventions disagree. The vendor's `_fcvt` goes away from zero.
#[test]
fn exact_ties_round_away_from_zero_not_to_even() {
    // Formatter (half-to-even) would give 0.12 / 0.38 / -0.12; vendor gives these.
    assert_eq!(round2(0.125), 0.13);
    assert_eq!(round2(0.375), 0.38);
    assert_eq!(round2(-0.125), -0.13);
    assert_eq!(round2(-0.375), -0.38);
    assert_eq!(round2(2.625), 2.63);
    assert_eq!(round_half_away(0.0625, 3), 0.063);
}

#[test]
fn non_ties_round_normally() {
    assert_eq!(round2(10.005), 10.01);
    assert_eq!(round2(-10.005), -10.01);
    assert_eq!(round2(0.0), 0.0);
    assert_eq!(round2(1.234), 1.23);
    assert_eq!(round2(1.235), 1.24);
    assert_eq!(round2(12.344), 12.34);
    // `2.675` is *not* a tie: its exact double is 2.674999999999999822…, which
    // is below the midpoint, so `_fcvt` gives 2.67. A half-up-on-shortest-repr
    // implementation would wrongly give 2.68 — this vector discriminates.
    assert_eq!(round2(2.675), 2.67);
    assert_eq!(round2(-0.0), 0.0);
}

#[test]
fn zero_rate_passes_the_amount_through_untranslated() {
    // The established threshold: |rate| <= 1e-06 takes the untranslated branch.
    for rate in [0.0, 1e-7, -1e-7, RATE_ZERO_EPSILON, -RATE_ZERO_EPSILON] {
        assert_eq!(
            translate_amount(1234.567, rate),
            round2(1234.567),
            "rate {rate} must be treated as zero"
        );
    }
    // Just outside the window the multiply happens.
    assert_eq!(translate_amount(100.0, 2e-6), 0.0);
    assert_eq!(translate_amount(100.0, 0.5), 50.0);
}

#[test]
fn multiply_is_single_step_single_rounding() {
    // The rate is *not* rounded before the multiply: 250 * 1.23456 = 308.64,
    // whereas rounding the rate to 2dp first (1.23) would give 308.25.
    assert_eq!(translate_amount(250.0, 1.23456), 308.64);
    // `1.005`'s exact double is 1.004999999999999893…, so 3 * it is 3.0149999…
    // and the correct render is 3.01, not 3.02.
    assert_eq!(translate_amount(3.0, 1.005), 3.01);
    // And the *amount* is not pre-rounded either: `0.005` is exactly
    // 0.005000000000000000104…, but the *product* `3 × 0.005` lands at
    // 0.014999999999999999444… — below the midpoint — so the render is 0.01.
    // Pre-rounding the amount to 0.01 would give 0.03. This vector therefore
    // proves the multiply happens on the raw value.
    assert_eq!(translate_amount(0.005, 3.0), 0.01);
}

#[test]
fn synthetic_translation_matrix_matches_the_law() {
    // (amount, rate, expected) — all exact binary values so the expectation is
    // not itself a rounding artefact of the test.
    let cases: &[(f64, f64, f64)] = &[
        (100.0, 1.0, 100.0),
        (100.0, 1.5, 150.0),
        (100.0, 0.5, 50.0),
        (0.0, 2.0, 0.0),
        (-100.0, 1.5, -150.0),
        (1.0, 0.125, 0.13),
        (8.0, 0.125, 1.0),
        (1.0, 0.0625, 0.06),
        (1.0, 0.1875, 0.19),
        (250.0, 1.23456, 308.64),
        (1234.56, 1.0, 1234.56),
        (1234.565, 1.0, 1234.57),
    ];
    for (amt, rate, want) in cases {
        assert_eq!(
            translate_amount(*amt, *rate),
            *want,
            "translate_amount({amt}, {rate})"
        );
    }
}

#[test]
fn corpus_consequence_zero_rate_means_identity() {
    // The only corpus-observable consequence: with C_RATE blank (every row in
    // all four engagements), FXCYTD must equal CUR_YTD to the cent.
    for amount in [0.0, 1.0, -1.0, 1234.56, -9876.54, 0.01, 99_999.99] {
        assert_eq!(translate_amount(amount, 0.0), round2(amount), "amount {amount}");
    }
}
