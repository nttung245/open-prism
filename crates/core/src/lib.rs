//! Core of Open Prism: project files, compilation, log parsing, SyncTeX, outline and history.
//!
//! This crate has no knowledge of HTTP so it can be tested on its own and reused by
//! other front ends (CLI, a future desktop shell).

pub mod engines;
