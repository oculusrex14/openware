//! Property bag + per-instance overrides (`CV OVR` semantics) — F3.
//!
//! CaseView presentation state is layered: a named style (or document default)
//! supplies a **base property bag**, and each cell carries a **sparse override
//! record** that changes only what the user touched. The shipped `Layout` cell
//! block stores that override record as opaque bytes ahead of the cell name
//! (ahead of the cell name); the bytes are dense and per-instance, which is
//! exactly what an override set is.
//!
//! This module fixes the *semantics* independently of the still-undecoded bit
//! packing:
//!
//! * [`PropBag`] — ordered (`BTreeMap`) typed property bag: `Bool`, `Int`,
//!   `Real`, `Text` (Latin-1/UTF-8 byte preserving), `Rgb`, `Ref`, `Raw`.
//! * [`OverrideSet`] — **sparse** per-key override map. Absence of a key means
//!   *inherit*; [`Override::Set`] replaces the base value; [`Override::Reset`]
//!   explicitly clears it (the cell shows the default, it does not inherit).
//!   That three-state law (inherit / set / reset) is the `CV OVR` contract.
//! * [`resolve`] / [`resolve_bag`] — effective value plus [`Origin`] provenance,
//!   so renderers and diff tools can say where a property came from.
//! * [`OverrideSet::layered`] — nearest-wins chaining for document → style →
//!   cell (and later table/paragraph) layers.
//! * [`CellAttrs`] — byte-preserving split of a `Layout` cell block into the
//!   attribute prefix / cell name / trailing attribute bytes. Reassembly is
//!   byte-identical, so decoded overrides can be written back without guessing
//!   at the undecoded bits.
//!
//! Provenance/scope: the `Styles`/`Para` streams are not decoded yet, so no
//! property *names* are invented here. Callers supply keys from the style
//! dictionary; the bag and override laws are the stable layer F4+ builds on.

use std::collections::BTreeMap;

/// Typed property value. `Raw` keeps undecoded attribute payloads verbatim.
#[derive(Debug, Clone, PartialEq)]
pub enum PropValue {
    Bool(bool),
    Int(i64),
    Real(f64),
    /// Text is stored as Unicode; the binary codecs use Latin-1, so the
    /// 1:1 byte round-trip holds for every `U+0000..=U+00FF` char.
    Text(String),
    /// 0x00RRGGBB.
    Rgb(u32),
    /// Reference into a document table (style/font/format index).
    Ref(u32),
    /// Opaque attribute bytes (cell override payload, unknown chunks).
    Raw(Vec<u8>),
}

impl PropValue {
    /// Stable type tag used by [`PropBag::to_text`] and diff output.
    pub fn type_name(&self) -> &'static str {
        match self {
            PropValue::Bool(_) => "bool",
            PropValue::Int(_) => "int",
            PropValue::Real(_) => "real",
            PropValue::Text(_) => "text",
            PropValue::Rgb(_) => "rgb",
            PropValue::Ref(_) => "ref",
            PropValue::Raw(_) => "raw",
        }
    }

    /// Canonical, loss-free text rendering (`text` values are quoted).
    pub fn to_text(&self) -> String {
        match self {
            PropValue::Bool(v) => v.to_string(),
            PropValue::Int(v) => v.to_string(),
            PropValue::Real(v) => {
                if v.is_finite() {
                    format!("{v}")
                } else {
                    format!("#{v}")
                }
            }
            PropValue::Text(v) => format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\"")),
            PropValue::Rgb(v) => format!("#{v:06X}"),
            PropValue::Ref(v) => format!("@{v}"),
            PropValue::Raw(v) => v.iter().map(|b| format!("{b:02X}")).collect(),
        }
    }
}

impl From<bool> for PropValue {
    fn from(v: bool) -> Self {
        PropValue::Bool(v)
    }
}
impl From<i64> for PropValue {
    fn from(v: i64) -> Self {
        PropValue::Int(v)
    }
}
impl From<f64> for PropValue {
    fn from(v: f64) -> Self {
        PropValue::Real(v)
    }
}
impl From<&str> for PropValue {
    fn from(v: &str) -> Self {
        PropValue::Text(v.to_string())
    }
}
impl From<String> for PropValue {
    fn from(v: String) -> Self {
        PropValue::Text(v)
    }
}

/// Deterministic typed property bag (`BTreeMap` ⇒ stable iteration order).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PropBag {
    props: BTreeMap<String, PropValue>,
}

impl PropBag {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<PropValue>) -> Option<PropValue> {
        self.props.insert(key.into(), value.into())
    }

    pub fn get(&self, key: &str) -> Option<&PropValue> {
        self.props.get(key)
    }

    pub fn remove(&mut self, key: &str) -> Option<PropValue> {
        self.props.remove(key)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.props.contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.props.len()
    }

    pub fn is_empty(&self) -> bool {
        self.props.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &PropValue)> {
        self.props.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.props.keys().map(|k| k.as_str())
    }

    /// Copy every property from `other`, overwriting on collision.
    pub fn merge(&mut self, other: &PropBag) {
        for (k, v) in other.iter() {
            self.props.insert(k.to_string(), v.clone());
        }
    }

    /// Canonical multi-line dump (`key<TAB>type<TAB>value`, sorted by key).
    /// Stable across runs and platforms; used by the F3 gate.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for (k, v) in self.iter() {
            out.push_str(k);
            out.push('\t');
            out.push_str(v.type_name());
            out.push('\t');
            out.push_str(&v.to_text());
            out.push('\n');
        }
        out
    }
}

/// One override entry. Missing key = inherit; the other two states are explicit.
#[derive(Debug, Clone, PartialEq)]
pub enum Override {
    /// Replace the inherited value.
    Set(PropValue),
    /// Explicitly clear the inherited value (show default, do not inherit).
    Reset,
}

/// Resolution state of a single key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// No base value and no override.
    Unset,
    /// Value comes from the base bag.
    Base,
    /// Value comes from an override layer.
    Override,
    /// An override explicitly reset the key.
    Reset,
}

/// Effective value of one key.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved<'a> {
    pub value: Option<&'a PropValue>,
    pub origin: Origin,
}

impl<'a> Resolved<'a> {
    pub fn is_set(&self) -> bool {
        self.value.is_some()
    }
}

/// Sparse per-instance override map (`CV OVR`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OverrideSet {
    entries: BTreeMap<String, Override>,
}

impl OverrideSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// `key := value` override.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<PropValue>) -> &mut Self {
        self.entries.insert(key.into(), Override::Set(value.into()));
        self
    }

    /// `key := reset` (clear the inherited value).
    pub fn reset(&mut self, key: impl Into<String>) -> &mut Self {
        self.entries.insert(key.into(), Override::Reset);
        self
    }

    /// Drop the override so the key inherits again.
    pub fn inherit(&mut self, key: &str) -> Option<Override> {
        self.entries.remove(key)
    }

    pub fn get(&self, key: &str) -> Option<&Override> {
        self.entries.get(key)
    }

    /// All entries in deterministic key order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Override)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// Resolve one key against a base bag (None = no style/default layer).
    pub fn resolve<'a>(&'a self, key: &str, base: Option<&'a PropValue>) -> Resolved<'a> {
        match self.entries.get(key) {
            Some(Override::Set(v)) => Resolved {
                value: Some(v),
                origin: Origin::Override,
            },
            Some(Override::Reset) => Resolved {
                value: None,
                origin: Origin::Reset,
            },
            None => match base {
                Some(v) => Resolved {
                    value: Some(v),
                    origin: Origin::Base,
                },
                None => Resolved {
                    value: None,
                    origin: Origin::Unset,
                },
            },
        }
    }

    /// Materialize the effective bag: base, then set/reset this layer.
    pub fn apply(&self, base: &PropBag) -> PropBag {
        let mut out = base.clone();
        for (k, ovr) in self.iter() {
            match ovr {
                Override::Set(v) => {
                    out.insert(k, v.clone());
                }
                Override::Reset => {
                    out.remove(k);
                }
            }
        }
        out
    }

    /// Effective bag plus per-key origins (base keys first, then overridden
    /// keys that are not in the base, in deterministic order).
    pub fn apply_with_origins(&self, base: &PropBag) -> (PropBag, BTreeMap<String, Origin>) {
        let mut values = base.clone();
        let mut origins: BTreeMap<String, Origin> =
            base.keys().map(|k| (k.to_string(), Origin::Base)).collect();
        for (k, ovr) in self.iter() {
            match ovr {
                Override::Set(v) => {
                    values.insert(k, v.clone());
                    origins.insert(k.to_string(), Origin::Override);
                }
                Override::Reset => {
                    values.remove(k);
                    origins.insert(k.to_string(), Origin::Reset);
                }
            }
        }
        (values, origins)
    }

    /// Nearest-wins chain. `layers[0]` is the closest to the instance (cell),
    /// the last is the furthest (document). A `Reset` in a nearer layer stops
    /// the walk for that key: farther layers must not resurrect a cleared value.
    pub fn layered<'a>(layers: &[&'a OverrideSet]) -> OverrideSet {
        let mut out = OverrideSet::new();
        for layer in layers {
            for (k, ovr) in layer.iter() {
                if out.entries.contains_key(k) {
                    continue; // nearer layer already decided this key
                }
                out.entries.insert(k.to_string(), ovr.clone());
            }
        }
        out
    }

    /// Effective bag over a base and a nearest-wins chain (see [`Self::layered`]).
    pub fn layered_apply(base: &PropBag, layers: &[&OverrideSet]) -> PropBag {
        Self::layered(layers).apply(base)
    }

    /// Keys this layer actually touches (set or reset), sorted.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(|k| k.as_str())
    }

    /// True when both layers resolve every key to the same outcome.
    pub fn same_effect(&self, other: &OverrideSet, base: &PropBag) -> bool {
        self.apply(base) == other.apply(base)
    }

    /// Canonical dump: `key<TAB>set<TAB>type<TAB>value` / `key<TAB>reset`.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for (k, ovr) in self.iter() {
            out.push_str(k);
            out.push('\t');
            match ovr {
                Override::Set(v) => {
                    out.push_str("set\t");
                    out.push_str(v.type_name());
                    out.push('\t');
                    out.push_str(&v.to_text());
                }
                Override::Reset => out.push_str("reset"),
            }
            out.push('\n');
        }
        out
    }
}

/// A cell block split into `prefix | name | suffix`.
///
/// Both attribute parts are opaque bytes; the split only relies on the established
/// name location from [`crate::layout`]. `reassemble` is byte-identical, which
/// is the write-back law for F3 overrides until the bit packing is decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellAttrs {
    pub name: String,
    pub prefix: Vec<u8>,
    pub suffix: Vec<u8>,
}

impl CellAttrs {
    /// Split the layout block for `block` (requires a located name).
    pub fn from_layout(layout: &[u8], block: &crate::layout::CellBlock) -> Option<CellAttrs> {
        let start = block.offset as usize;
        let end = start.checked_add(block.len as usize)?;
        if end > layout.len() || block.len == 0 {
            return None;
        }
        let name_off = block.name_offset? as usize;
        let name_len = block.name.chars().count();
        if name_off < start || name_off + name_len > end {
            return None;
        }
        Some(CellAttrs {
            name: block.name.clone(),
            prefix: layout[start..name_off].to_vec(),
            suffix: layout[name_off + name_len..end].to_vec(),
        })
    }

    /// The attribute prefix that precedes the name (the per-instance override
    /// record ahead of the cell name).
    pub fn prefix(&self) -> &[u8] {
        &self.prefix
    }

    /// Trailing attribute bytes (run/layout data that follows the name).
    pub fn suffix(&self) -> &[u8] {
        &self.suffix
    }

    pub fn block_len(&self) -> usize {
        self.prefix.len() + self.name_bytes().len() + self.suffix.len()
    }

    /// Latin-1 encoding of the name (one byte per char, byte-preserving).
    pub fn name_bytes(&self) -> Vec<u8> {
        self.name.chars().map(|c| c as u32 as u8).collect()
    }

    /// Byte-identical block reconstruction.
    pub fn reassemble(&self) -> Vec<u8> {
        let name = self.name_bytes();
        let mut out = Vec::with_capacity(self.prefix.len() + name.len() + self.suffix.len());
        out.extend_from_slice(&self.prefix);
        out.extend_from_slice(&name);
        out.extend_from_slice(&self.suffix);
        out
    }

    /// Expose the prefix as an opaque override payload.
    pub fn override_blob(&self) -> PropValue {
        PropValue::Raw(self.prefix.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> PropBag {
        let mut b = PropBag::new();
        b.insert("font", PropValue::Text("Arial".into()));
        b.insert("size", PropValue::Real(10.0));
        b.insert("bold", PropValue::Bool(false));
        b
    }

    #[test]
    fn bag_is_deterministic_and_typed() {
        let mut b = PropBag::new();
        b.insert("z", PropValue::Int(1));
        b.insert("a", PropValue::Text("x".into()));
        assert_eq!(
            b.to_text(),
            "a\ttext\t\"x\"\nz\tint\t1\n",
            "BTreeMap iteration must be sorted"
        );
        assert_eq!(b.keys().collect::<Vec<_>>(), vec!["a", "z"]);
    }

    #[test]
    fn override_states_inherit_set_reset() {
        let base = base();
        let mut ovr = OverrideSet::new();

        // absent key => inherit
        assert_eq!(ovr.resolve("font", base.get("font")).origin, Origin::Base);
        assert_eq!(ovr.resolve("missing", None).origin, Origin::Unset);

        // set => replace
        ovr.set("font", "Times".to_string());
        let r = ovr.resolve("font", base.get("font"));
        assert_eq!(r.origin, Origin::Override);
        assert_eq!(r.value, Some(&PropValue::Text("Times".into())));

        // reset => cleared even though the base has a value
        ovr.reset("size");
        let r = ovr.resolve("size", base.get("size"));
        assert_eq!(r.origin, Origin::Reset);
        assert!(r.value.is_none());

        // back to inherit
        ovr.inherit("size");
        assert_eq!(ovr.resolve("size", base.get("size")).origin, Origin::Base);
    }

    #[test]
    fn apply_and_origins() {
        let base = base();
        let mut ovr = OverrideSet::new();
        ovr.set("bold", true);
        ovr.reset("size");
        ovr.set("align", "center".to_string());

        let (eff, origins) = ovr.apply_with_origins(&base);
        assert_eq!(eff.get("bold"), Some(&PropValue::Bool(true)));
        assert!(!eff.contains("size"), "reset must remove the base value");
        assert_eq!(eff.get("align"), Some(&PropValue::Text("center".into())));
        assert_eq!(origins["bold"], Origin::Override);
        assert_eq!(origins["size"], Origin::Reset);
        assert_eq!(origins["font"], Origin::Base);
        assert_eq!(origins["align"], Origin::Override);
    }

    #[test]
    fn layered_chain_is_nearest_wins_and_reset_blocks_farther_layers() {
        let mut doc = OverrideSet::new();
        doc.set("font", "Georgia".to_string());
        doc.set("size", 12.0);
        doc.set("color", PropValue::Rgb(0x00_00_00u32));

        let mut style = OverrideSet::new();
        style.set("size", 9.0);
        style.set("bold", true);

        let mut cell = OverrideSet::new();
        cell.set("bold", false);
        cell.reset("size"); // cell explicitly clears size

        // cell is nearest, then style, then document
        let chain = OverrideSet::layered(&[&cell, &style, &doc]);
        let eff = chain.apply(&PropBag::new());
        assert_eq!(eff.get("font"), Some(&PropValue::Text("Georgia".into())));
        assert_eq!(
            eff.get("bold"),
            Some(&PropValue::Bool(false)),
            "cell beats style"
        );
        assert!(
            !eff.contains("size"),
            "a nearer Reset must not be resurrected by style/doc"
        );
        assert_eq!(eff.get("color"), Some(&PropValue::Rgb(0)));
    }

    #[test]
    fn layered_apply_equals_sequential_apply() {
        let base = base();
        let mut a = OverrideSet::new();
        a.set("size", 14.0);
        let mut b = OverrideSet::new();
        b.set("size", 8.0);
        b.reset("font");
        // nearest layer a must win for size; b must still reset font only if
        // the nearer layers do not decide it.
        let stepwise = a.apply(&b.apply(&base));
        let chained = OverrideSet::layered_apply(&base, &[&a, &b]);
        assert_eq!(stepwise, chained);
    }

    #[test]
    fn same_effect_ignores_redundant_overrides() {
        let base = base();
        let mut a = OverrideSet::new();
        a.set("font", "Arial".to_string()); // same as base: effect equal
        let b = OverrideSet::new();
        assert!(a.same_effect(&b, &base));
        assert!(!a.same_effect(&OverrideSet::new().set("font", "Times").clone(), &base));
    }

    #[test]
    fn cell_attrs_round_trip_preserves_opaque_bytes() {
        // prefix has control bytes, name has class bytes, suffix is arbitrary.
        let prefix = [0x01u8, 0x09, 0x00, 0xE0, 0x00, 0x10];
        let name = b"\x1caEXR.C9";
        let suffix = [0u8, 1, 0x80, 0xFF];
        let mut layout = vec![0u8; 32];
        layout[4..4 + prefix.len()].copy_from_slice(&prefix);
        layout[4 + prefix.len()..4 + prefix.len() + name.len()].copy_from_slice(name);
        let block = crate::layout::CellBlock {
            name: name.iter().map(|&b| b as char).collect(),
            offset: 4,
            len: (prefix.len() + name.len() + suffix.len()) as u32,
            name_offset: Some(4 + prefix.len() as u32),
            trailer: None,
        };
        layout[4 + prefix.len() + name.len()..4 + prefix.len() + name.len() + suffix.len()]
            .copy_from_slice(&suffix);

        let attrs = CellAttrs::from_layout(&layout, &block).expect("split");
        assert_eq!(attrs.prefix, prefix);
        assert_eq!(attrs.suffix, suffix);
        assert_eq!(
            attrs.reassemble(),
            layout[4..4 + block.len as usize],
            "write-back must be byte-identical"
        );
        assert_eq!(attrs.override_blob(), PropValue::Raw(prefix.to_vec()));
    }
}

