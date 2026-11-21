//! Core of Open Prism: project files, compilation, log parsing, SyncTeX, outline and history.
//!
//! This crate has no knowledge of HTTP so it can be tested on its own and reused by
//! other front ends (CLI, a future desktop shell).

pub mod archive;
pub mod compile;
pub mod engines;
pub mod files;
pub mod history;
pub mod latexlog;
pub mod paths;
pub mod pdf_pages;
pub mod pdf_render;
pub mod synctex;
#[cfg(all(test, unix))]
mod test_support;
pub mod watch;
