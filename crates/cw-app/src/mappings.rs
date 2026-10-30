//! P5.E2a facade - `MP` mapping row model + tree walk.
//!
//! The implementation lives in [`crate::groups`] (one module for the MP model
//! and the `AM` assignment surface, so the two never drift). This module is the
//! named entry point the board's E2a item asks for: MP row model (`MAP_NO`,
//! scheme `ID`, `AC_DESC`, `MAPTYPE`, `LINENO`), lookup by `MAP_NO`, tree walk
//! and the node writer.

pub use crate::groups::{
    inferred_parent, mp_schema_drift, mp_schema_field, mp_schema_matches, rewrite_nodes, MapNode,
    MapProperties, MpField, MpIssue, MpIssueKind, MpScheme, MpTable, NodeKind, Scheme, MP_SCHEMA,
    MP_FIELD_COUNT, MP_RECORD_LEN,
};

