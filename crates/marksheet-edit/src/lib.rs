//! Transactional, source-preserving edits for Marksheet documents.
//!
//! This crate translates semantic edit intent into exact, ordered byte patches.
//! It does not mutate the authored workbook IR or use canonical serialization
//! as an editing shortcut.

#![forbid(unsafe_code)]

pub mod csv;
pub mod diff;
pub mod history;
pub mod inverse;
pub mod patch;
pub mod transaction;
