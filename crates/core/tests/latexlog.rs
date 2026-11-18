//! Snapshot tests of the log parsers over real logs.
//!
//! Each directory under `tests/fixtures/logs` holds a small project and the `main.log` (and
//! `main.blg`) that the compile service's command wrote for it:
//!
//! ```text
//! latexmk -pdf|-xelatex|-lualatex -f -interaction=nonstopmode -file-line-error -synctex=1 \
//!   -outdir=/data/build/fixture main.tex
//! ```
//!
//! run in the `texlive/texlive` image with `max_print_line=10000`, the project mounted at
//! `/data/projects/fixture` (the cases starting with `xelatex-` and `lualatex-` use those
//! engines). Every file and line in the snapshots was checked against the sources.

use std::path::Path;

use insta::assert_yaml_snapshot;
use openprism_core::latexlog::{Diagnostic, parse_biber_log, parse_bibtex_log, parse_latex_log};

const PROJECT_ROOT: &str = "/data/projects/fixture";

fn read(case: &str, file: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/logs")
        .join(case)
        .join(file);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    String::from_utf8_lossy(&bytes).into_owned()
}

fn latex(case: &str) -> Vec<Diagnostic> {
    parse_latex_log(&read(case, "main.log"), Path::new(PROJECT_ROOT))
}

/// Diagnostics of the TeX log followed by those of biber.
fn latex_and_biber(case: &str) -> Vec<Diagnostic> {
    let mut diagnostics = latex(case);
    diagnostics.extend(parse_biber_log(
        &read(case, "main.blg"),
        Path::new(PROJECT_ROOT),
    ));
    diagnostics
}

/// Diagnostics of the TeX log followed by those of BibTeX.
fn latex_and_bibtex(case: &str) -> Vec<Diagnostic> {
    let mut diagnostics = latex(case);
    diagnostics.extend(parse_bibtex_log(
        &read(case, "main.blg"),
        Path::new(PROJECT_ROOT),
    ));
    diagnostics
}

#[test]
fn clean() {
    let diagnostics = latex("clean");
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    assert_yaml_snapshot!(diagnostics);
}

#[test]
fn undefined_control_sequence() {
    assert_yaml_snapshot!(latex("undefined-control-sequence"));
}

#[test]
fn error_in_subfile() {
    assert_yaml_snapshot!(latex("error-in-subfile"));
}

#[test]
fn missing_dollar() {
    assert_yaml_snapshot!(latex("missing-dollar"));
}

#[test]
fn undefined_references() {
    assert_yaml_snapshot!(latex("undefined-references"));
}

#[test]
fn bad_boxes() {
    assert_yaml_snapshot!(latex("bad-boxes"));
}

#[test]
fn missing_package() {
    assert_yaml_snapshot!(latex("missing-package"));
}

#[test]
fn missing_input() {
    assert_yaml_snapshot!(latex("missing-input"));
}

#[test]
fn package_messages() {
    assert_yaml_snapshot!(latex("package-messages"));
}

#[test]
fn runaway_argument() {
    assert_yaml_snapshot!(latex("runaway-argument"));
}

#[test]
fn xelatex_missing_font() {
    assert_yaml_snapshot!(latex("xelatex-missing-font"));
}

#[test]
fn lualatex_subfile() {
    assert_yaml_snapshot!(latex("lualatex-subfile"));
}

#[test]
fn spaces_in_filename() {
    assert_yaml_snapshot!(latex("spaces-in-filename"));
}

#[test]
fn multiple_errors() {
    assert_yaml_snapshot!(latex("multiple-errors"));
}

#[test]
fn biber_missing_entry() {
    assert_yaml_snapshot!(latex_and_biber("biber-missing-entry"));
}

#[test]
fn biber_syntax_error() {
    assert_yaml_snapshot!(latex_and_biber("biber-syntax-error"));
}

#[test]
fn bibtex_errors() {
    assert_yaml_snapshot!(latex_and_bibtex("bibtex-errors"));
}
