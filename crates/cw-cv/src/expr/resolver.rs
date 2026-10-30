//! Resolution seam for [`crate::expr`].
//!
//! The placeholder language's *pure* tier is computed by the evaluator. Its
//! *data* tier — `act`, `map`, `field`, `rc`, `cvdata`, … — is resolved by
//! `x64CODEBASE.dll`'s `expr4*` family in the product, which is not part of the
//! staged artifact set. Rather than invent values, the evaluator asks a
//! [`Resolver`]; `None` means "not available offline" and propagates as
//! [`crate::expr::Value::Unknown`].
//!
//! A caller that *does* have the database (the `cw-app` side, or a renderer with
//! a decoded `am`/`gl`/`CV` set) implements this trait and gets real values with
//! no change to the grammar.

use crate::expr::Value;

/// Data-layer callbacks for the placeholder evaluator.
pub trait Resolver {
    /// Resolve a bare cell/field name such as `clp13`, `ac_no`, `CV`.
    ///
    /// `None` = unknown offline.
    fn name(&self, name: &str) -> Option<Value>;

    /// Resolve `base->field` / `base.field`, e.g. `AM->SCHEDULE`.
    fn field(&self, base: &str, field: &str) -> Option<Value>;

    /// Resolve a named function call with already-evaluated arguments.
    ///
    /// Returning `None` keeps the result unknown; returning `Some(Unknown)` is
    /// equivalent. The default implementation returns `None`, so an empty
    /// resolver still parses and type-checks an expression.
    fn call(&self, name: &str, args: &[Value]) -> Option<Value> {
        let _ = (name, args);
        None
    }
}

/// A resolver that knows nothing: every data reference is
/// [`Value::Unknown`]. This is what offline tooling uses when it only wants the
/// grammar and the pure sub-expressions.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnknownResolver;

impl Resolver for UnknownResolver {
    fn name(&self, _name: &str) -> Option<Value> {
        None
    }
    fn field(&self, _base: &str, _field: &str) -> Option<Value> {
        None
    }
}
