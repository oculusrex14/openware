//! P5.E2b facade - leadsheet grouping assignment (`AM.GROUPn <- node MAP_NO`).
//!
//! The implementation lives in [`crate::groups`] (F37 write law: verbatim
//! `MAP_NO`, scheme `n` = `MP.ID`, assignable nodes only, atomic writes).
//! This module is the named entry point for the board's E2b item: assignment,
//! clearing, slot inventory, reverse lookup, lead-schedule linkage and the
//! `assignment_issues` audit (`xCR` twins stay raw `*CR` columns; the corpus
//! leaves them blank).

pub use crate::groups::{
    account_assignment, accounts_at, am_assignment_columns, assign_account, assignment_column,
    assignments, clear_assignment, dangling_lead_schedules, lead_schedule, lead_schedule_codes,
    set_lead_schedule, unassigned_rows, AssignmentIssue, AssignmentIssueKind, ASSIGNABLE_DIGITS,
};

