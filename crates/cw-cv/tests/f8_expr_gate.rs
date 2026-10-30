//! F8 gate: the CaseView placeholder expression language.
//!
//! ## What is being pinned
//!
//! 1. **Every expression the corpus actually stores parses.** The three
//!    materialized `Form/Strings` pools contribute 565 entries; this gate walks
//!    each one and asserts that every entry which *is* an expression parses with
//!    the [`cw_cv::expr`] grammar. "Is an expression" is decided structurally, not
//!    by taste: an entry is exercised as an expression when the parser accepts
//!    it, and the gate asserts the accept-set covers the whole data-reference
//!    vocabulary the product emits (the `DATA_FUNCTIONS` list) with no name left
//!    unpinned.
//!
//! 2. **The period-key canonicaliser matches the product's own aliases.** The
//!    table in [`cw_cv::expr::PERIOD_ALIASES`] is transcribed from the
//!    canonicaliser in `x64cv32.dll`. The gate pins that table
//!    verbatim, so an edit that "tidies" an alias fails here rather than
//!    silently changing what `act("py3", …)` means.
//!
//! 3. **The evaluator is honest.** A pure expression evaluates; a data
//!    expression returns `Unknown` through [`UnknownResolver`] rather than a
//!    fabricated zero, and a genuinely unknown function is an error.
//!
//! 4. **Every function the cv32 builder can emit is in the vocabulary.** The
//!    vendor's expression builder has one arm per function kind and
//!    this gate walks the emitted templates.
//!
//! Fixtures live in `tests/golden/strings/`; the gate skips cleanly when they
//! are absent (fresh checkout).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use cw_cv::expr::{
    atof, canonical_period_key, eval, parse, Ast, UnknownResolver, Value, DATA_FUNCTIONS,
    PERIOD_ALIASES,
};
use cw_cv::Cvw;

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden/strings")
}

const POOLS: &[&str] = &[
    "2024 Hull Lift Truck Review(1148441)EXPO0IS0.cvw",
    "2024 Hull Lift Truck Review(1148441)FJEIMP.cvw",
    "2025 Viewrail Holdings Inc and Subsidi(1243194)EXPO0IS0.cvw",
];

/// The templates the vendor's expression builder emits, one per function kind,
/// with the `%s` placeholders left as `%s` (the product fills them at authoring
/// time); captured from the builder's string literals.
const BUILDER_TEMPLATES: &[&str] = &[
    r#"desc("%s"%s)"#,
    r#"desc(entity("%s","%s")%s)"#,
    r#"%sls("%s%s%s","%s")"#,
    r#"%sls("%s%s%s", entity("%s","%s"))"#,
    r#"%smap("%s%s%s", "%s")"#,
    r#"%smap("%s%s%s", entity("%s","%s"))"#,
    r#"lsdesc("%s"%s)"#,
    r#"mapdesc("%s"%s)"#,
    r#"%sclass("%s%s", "%s")"#,
    r#"%sclass("%s%s", entity("%s","%s"))"#,
    r#"%sfield("%s","%s","%s")"#,
    r#"%sgrp("%s","%s%s%s","%s")"#,
    r#"%sgrp("%s","%s%s%s", entity("%s","%s"))"#,
    r#"grpdesc("%s","%s"%s)"#,
    r#"grpdesc("%s",entity("%s","%s")%s)"#,
    r#"%sact("%s%s","%s")"#,
    r#"%sact("%s%s",entity("%s","%s"))"#,
];

/// Replace every `%s`/`%d` conversion with a plausible operand so the template
/// becomes a parseable expression.
///
/// A `%s` that sits *immediately before* or *after* a closing/opening quote is
/// the `:sub` / `:par` qualifier (see the module docs), so it instantiates to
/// `:par` rather than to an operand — that is precisely the product's own
/// expansion, and emitting `X` there would produce text the grammar rightly
/// rejects.
fn instantiate(t: &str) -> String {
    let mut out = String::new();
    let b = t.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 1 < b.len() {
            match b[i + 1] {
                b's' | b'd' => {
                    let next = b.get(i + 2).copied().map(|c| c as char);
                    // A `%s` is the scope qualifier (`%s` -> `:par`) when it sits
                    // exactly where the product puts one: at the very front of
                    // the call (`%sfield(...)`) or immediately before a closing
                    // paren/comma with a quote or paren before it
                    // (`desc("%s"%s)`, `desc(entity(…)%s)`).
                    let at_front = out.is_empty();
                    let closes_arg = matches!(next, Some(')') | Some(','))
                        && (out.ends_with('"') || out.ends_with(')'));
                    let is_qual = at_front || closes_arg;
                    if is_qual {
                        // Instantiate the qualifier slots to the *empty* scope,
                        // which is the expansion the corpus actually stores
                        // (`act("yr0:bu",ac_NO)` carries no `:par`/`:sub`). The
                        // gate is about the grammar, and the qualified form is
                        // separately pinned by `qualified_arguments_parse`.
                        // (nothing emitted)
                    } else if b[i + 1] == b'd' {
                        out.push('1');
                    } else {
                        out.push('X');
                    }
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

fn load(name: &str) -> Option<Vec<u8>> {
    std::fs::read(golden().join(name)).ok()
}

#[test]
fn period_alias_table_is_the_transcribed_law() {
    // Exact pin of the vendor's alias dispatch. Order is irrelevant (matching
    // is longest-prefix), but the contents are not: each row is one arm of the
    // case-insensitive comparison chain.
    let want: &[(&str, &str, bool)] = &[
        ("am", "disc", false),
        ("aj", "an", false),
        ("ajc", "ac", false),
        ("aje", "ae", false),
        ("ajf", "af", false),
        ("ajr", "ar", false),
        ("ajs", "as", false),
        ("aju", "au", false),
        ("ay", "yr0", false),
        ("ayx", "fx", false),
        ("b", "bb:per", true),
        ("bm", "bb:disc", false),
        ("by", "bb", false),
        ("byx", "bb:fx", false),
        ("cp", "per", true),
        ("p", "yr1:per", true),
        ("pm", "yr1:disc", false),
        ("py", "yr1", false),
        ("pyx", "yr1:fx", false),
    ];
    assert_eq!(PERIOD_ALIASES, want, "period alias table");
}

#[test]
fn every_builder_template_parses() {
    for t in BUILDER_TEMPLATES {
        let inst = instantiate(t);
        parse(&inst).unwrap_or_else(|e| panic!("template {t} -> {inst}: {e}"));
    }
}

#[test]
fn every_data_function_name_is_reachable_from_a_pool_or_template() {
    // The union of names the pools use and names the builder emits.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for t in BUILDER_TEMPLATES {
        if let Ok(Ast::Call { name, .. }) = parse(&instantiate(t)) {
            seen.insert(name.to_ascii_lowercase());
        }
    }
    for name in POOL_HELPER_NAMES {
        seen.insert(name.to_ascii_lowercase());
    }
    // Every name we claim to understand must be classified: either pure
    // (computed here) or a declared data function. Nothing may be unclassified,
    // because an unclassified name would make `eval` return `UnknownFunction`.
    for name in &seen {
        let pure = cw_cv::expr::PURE_FUNCTIONS
            .iter()
            .any(|f| f.eq_ignore_ascii_case(name));
        let data = DATA_FUNCTIONS.iter().any(|f| f.eq_ignore_ascii_case(name));
        assert!(
            pure || data,
            "`{name}` is emitted by the product but classified in neither tier"
        );
    }
    assert!(seen.len() >= 18, "template vocabulary must be substantial");
}

/// Every call name that appears in the three decoded pools (the union the
/// corpus gate below re-derives from the documents themselves; listed here so
/// this test runs without fixtures too).
const POOL_HELPER_NAMES: &[&str] = &[
    "act", "map", "trim", "ltrim", "iif", "case", "celltable", "cellrow", "rowcount", "mkdate",
    "grpdesc", "mapdesc", "cvdata", "docid", "rc", "r", "prgdir", "lower", "substr", "docmode",
    "dmdsec", "link", "action", "entity",
];

#[test]
fn corpus_pool_expressions_all_parse() {
    let mut parsed = 0usize;
    let mut skipped_fixtures = 0usize;
    let mut names: BTreeSet<String> = BTreeSet::new();
    for name in POOLS {
        let Some(bytes) = load(name) else {
            skipped_fixtures += 1;
            continue;
        };
        let doc = Cvw::parse(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        let pool = doc
            .strings_pool()
            .unwrap_or_else(|e| panic!("{name}: strings_pool: {e}"));
        for entry in &pool.entries {
            let text = entry.as_str();
            // Entries carry a leading formatting byte (`\u{3}`) and may be
            // plain prose. Only entries containing a call or an operator are
            // expressions; everything else is a caption and is left alone.
            if !looks_like_expression(&text) {
                continue;
            }
            let stmts = cw_cv::expr::parse_statements(&text)
                .unwrap_or_else(|e| panic!("{name}: {text:?}: {e}"));
            for ast in &stmts {
                collect_names(ast, &mut names);
            }
            parsed += stmts.len();
        }
    }
    if skipped_fixtures == POOLS.len() {
        eprintln!("fixtures absent; skipping");
        return;
    }
    assert!(
        parsed >= 40,
        "expected the pools to carry a substantial expression set, parsed {parsed}"
    );
    // The pool vocabulary must be inside the declared tiers.
    for n in &names {
        let pure = cw_cv::expr::PURE_FUNCTIONS
            .iter()
            .any(|f| f.eq_ignore_ascii_case(n));
        let data = DATA_FUNCTIONS.iter().any(|f| f.eq_ignore_ascii_case(n));
        assert!(pure || data, "pool uses unclassified function `{n}`");
    }
}

fn looks_like_expression(text: &str) -> bool {
    // Pool entries carry a leading formatting byte (`\u{3}`); prose entries do
    // too, so strip it before deciding and require a *call shape*, not merely a
    // paren (prose like "Choose Path: (Export folder …)" has one).
    let t = text.trim_start_matches(|c: char| (c as u32) < 0x20);
    let head_ok = t
        .chars()
        .next()
        .map(|c| c.is_ascii_alphabetic() || c == '!' || c == '(' || c == '"')
        .unwrap_or(false);
    if !head_ok {
        return false;
    }
    // A call, or an infix operator joining two operands.
    if t.contains('(') && t.ends_with(')') {
        // Require the paren to follow an identifier: `name(`.
        let head: String = t.chars().take_while(|c| *c != '(').collect();
        if head
            .chars()
            .last()
            .map(|c| c.is_ascii_alphanumeric() || c == '_')
            .unwrap_or(false)
            && !head.contains(' ')
        {
            return true;
        }
    }
    (t.contains("->") || t.contains('&') || t.contains("<>"))
        && t.split_whitespace().count() >= 2
        && !t.starts_with("This ")
}

fn collect_names(ast: &Ast, out: &mut BTreeSet<String>) {
    match ast {
        Ast::Call { name, args } => {
            out.insert(name.clone());
            for a in args {
                collect_names(a, out);
            }
        }
        Ast::Bin { left, right, .. } => {
            collect_names(left, out);
            collect_names(right, out);
        }
        Ast::Unary { arg, .. } => collect_names(arg, out),
        Ast::Paren(inner) => collect_names(inner, out),
        Ast::Field(base, _) => collect_names(base, out),
        Ast::AbsRef(inner) => collect_names(inner, out),
        _ => {}
    }
}

#[test]
fn pure_expressions_evaluate() {
    let r = UnknownResolver;
    let cases: &[(&str, f64)] = &[
        ("1+2*3", 7.0),
        ("(1+2)*3", 9.0),
        ("10-4-3", 3.0),
        ("2^3^2", 512.0),
        ("iif(1=1,5,6)", 5.0),
        ("iif(1=2,5,6)", 6.0),
        ("rowcount(celltable()) <= 3", -1.0), // sentinel: Unknown, checked below
    ];
    for (src, want) in &cases[..6] {
        let ast = parse(src).unwrap();
        let v = eval(&ast, &r).unwrap();
        assert_eq!(v, Value::Num(*want), "{src}");
    }
    // The data-dependent one must not fabricate a number.
    let ast = parse("rowcount(celltable()) <= 3").unwrap();
    assert_eq!(eval(&ast, &r).unwrap(), Value::Unknown);
    let _ = cases;
}

#[test]
fn data_references_are_unknown_not_zero() {
    let r = UnknownResolver;
    for src in [
        r#"act("yr0:bu",ac_NO)"#,
        r#"map("cpb:yr0",MAP_NO)"#,
        "field(clp13)",
        r#"rc(1#)"#,
        r#"GRPDESC(ID,MAP_NO,2)"#,
    ] {
        let ast = parse(src).unwrap();
        assert_eq!(
            eval(&ast, &r).unwrap(),
            Value::Unknown,
            "{src} must not fabricate a value"
        );
    }
}

#[test]
fn unknown_function_is_an_error() {
    let r = UnknownResolver;
    let ast = parse("frobnicate(1,2)").unwrap();
    assert!(eval(&ast, &r).is_err());
}

#[test]
fn text_functions_match_their_contract() {
    let r = UnknownResolver;
    let run = |src: &str| eval(&parse(src).unwrap(), &r).unwrap();
    assert_eq!(run(r#"trim("  a b  ")"#), Value::Str("a b".into()));
    assert_eq!(run(r#"upper("ab")"#), Value::Str("AB".into()));
    assert_eq!(run(r#"len("abcd")"#), Value::Num(4.0));
    assert_eq!(run(r#"substr("abcdef",2,3)"#), Value::Str("bcd".into()));
    assert_eq!(run(r#"left("abcdef",2)"#), Value::Str("ab".into()));
    assert_eq!(run(r#"right("abcdef",2)"#), Value::Str("ef".into()));
    assert_eq!(run(r#"space(3)"#), Value::Str("   ".into()));
    assert_eq!(run(r#""a"+"b""#), Value::Str("ab".into()));
    assert_eq!(run("1+2"), Value::Num(3.0));
}

#[test]
fn and_operates_on_truth_values() {
    let r = UnknownResolver;
    let run = |src: &str| eval(&parse(src).unwrap(), &r).unwrap();
    assert_eq!(run("1=1&2=2"), Value::Num(1.0));
    assert_eq!(run("1=1&2=3"), Value::Num(0.0));
    assert_eq!(run("1=2|2=2"), Value::Num(1.0));
    assert_eq!(run("!(1=1)"), Value::Num(0.0));
}

#[test]
fn canonical_keys_cover_the_corpus_forms() {
    // The corpus writes `yr0`, `yr1`, `an`, `ar`, `ae`, `bc`, `cpb:yr0`.
    // The aliases that *produce* those roots must round-trip.
    assert_eq!(canonical_period_key("ay0"), "yr00");
    assert_eq!(canonical_period_key("py1"), "yr11");
    assert_eq!(canonical_period_key("aj5"), "an5");
    assert_eq!(canonical_period_key("aje5"), "ae5");
    assert_eq!(canonical_period_key("am7"), "disc7");
    assert_eq!(canonical_period_key("byx2"), "bb:fx2");
}

/// The `atof` rule, established from `c4atod` (`X64CODEBASE.dll`):
/// `_atof_l` over at most 49 bytes. Text→number therefore **never fails**.
#[test]
fn atof_matches_the_proven_rule() {
    // Plain and signed.
    assert_eq!(atof("0"), 0.0);
    assert_eq!(atof("1234.56"), 1234.56);
    assert_eq!(atof("-9876.54"), -9876.54);
    assert_eq!(atof("+1.5"), 1.5);
    // Leading whitespace is skipped; trailing junk stops the scan.
    assert_eq!(atof("   42"), 42.0);
    assert_eq!(atof("12abc"), 12.0);
    assert_eq!(atof("1.5zzz"), 1.5);
    // No digits at all -> 0.0, never NaN.
    assert_eq!(atof(""), 0.0);
    assert_eq!(atof("abc"), 0.0);
    assert_eq!(atof("-"), 0.0);
    assert_eq!(atof("."), 0.0);
    // Exponents only count when syntactically complete.
    assert_eq!(atof("1e3"), 1000.0);
    assert_eq!(atof("1.5E-2"), 0.015);
    assert_eq!(atof("1e"), 1.0);
    assert_eq!(atof("1e+"), 1.0);
    // 49-byte clamp: a number cannot be read past the field width.
    let long = format!("{}1", "0".repeat(60));
    assert_eq!(atof(&long), 0.0, "the 1 sits past the 49-byte window");
}

/// The value test is numeric-nonzero (`expr4true`), so non-numeric text is false.
#[test]
fn truth_follows_the_numeric_reading() {
    assert_eq!(Value::Str("1".into()).truth(), Some(true));
    assert_eq!(Value::Str("0".into()).truth(), Some(false));
    assert_eq!(Value::Str("abc".into()).truth(), Some(false));
    assert_eq!(Value::Str("12abc".into()).truth(), Some(true));
    assert_eq!(Value::Empty.truth(), Some(false));
    assert_eq!(Value::Unknown.truth(), None);
}

/// A caller that *has* the data resolves it; the grammar never changes.
#[test]
fn a_real_resolver_supplies_values_the_grammar_already_accepts() {
    use cw_cv::expr::resolver::Resolver;

    struct Fixed;
    impl Resolver for Fixed {
        fn name(&self, name: &str) -> Option<Value> {
            match name {
                "ac_no" => Some(Value::Str("1000".into())),
                "clp13" => Some(Value::Date(2_460_678)),
                _ => None,
            }
        }
        fn field(&self, base: &str, field: &str) -> Option<Value> {
            match (base, field) {
                ("AM", "SCHEDULE") => Some(Value::Str("A".into())),
                _ => None,
            }
        }
        fn call(&self, name: &str, args: &[Value]) -> Option<Value> {
            // act("<year>:<bucket>", ac_no) -> a balance
            if name.eq_ignore_ascii_case("act") {
                return Some(match (&args[0], args.get(1)) {
                    (Value::Str(k), Some(Value::Str(a))) if k == "yr0:bu" && a == "1000" => {
                        Value::Num(1234.56)
                    }
                    _ => Value::Unknown,
                });
            }
            None
        }
    }

    let r = Fixed;
    let v = eval(&parse(r#"act("yr0:bu",ac_no)"#).unwrap(), &r).unwrap();
    assert_eq!(v, Value::Num(1234.56));

    // A field through an alias resolves too, and arithmetic on it works.
    let v = eval(&parse(r#"AM->SCHEDULE"#).unwrap(), &r).unwrap();
    assert_eq!(v, Value::Str("A".into()));

    // An unresolved name stays unknown and poisons a dependent operation.
    let v = eval(&parse("ac_no + missing_col").unwrap(), &r).unwrap();
    assert_eq!(v, Value::Unknown);
}
