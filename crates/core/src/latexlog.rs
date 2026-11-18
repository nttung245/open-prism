//! Turns the logs of TeX engines, biber and BibTeX into [`Diagnostic`]s.
//!
//! The parsers expect logs as the compile service produces them: `latexmk` with
//! `-file-line-error` and `max_print_line=10000`, so TeX never wraps a log line.
//!
//! TeX names the file of a message only for `-file-line-error` errors. Everything else is
//! located through the `(` and `)` TeX writes when it opens and closes a file. Other text has
//! parentheses too, so structured messages (errors, warnings, info, bad boxes) are recognised
//! first and never scanned for file markers, and a parenthesis opened by free text is
//! forgotten at the next blank line.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// How serious a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum Severity {
    Error,
    Warning,
    /// Typesetting notes such as overfull and underfull boxes, and requests to rerun.
    Info,
}

/// The tool whose log reported a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum DiagnosticSource {
    /// The TeX engine and the LaTeX packages it runs.
    Latex,
    Biber,
    Bibtex,
}

/// A problem reported by one of the tools of a compile.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Diagnostic {
    pub severity: Severity,
    pub source: DiagnosticSource,
    /// Project-relative path with `/` separators (no leading `./`) when the file is inside the
    /// project; otherwise the path as printed (e.g. a .sty in TeX Live). None if unknown.
    pub file: Option<String>,
    pub line: Option<u32>,
    /// One-line summary, e.g. "Undefined control sequence."
    pub message: String,
    /// Supporting lines (e.g. `l.12 \foo` context, package help text), trimmed; may be empty.
    pub details: String,
}

/// Parses the `.log` of a pdfTeX, XeTeX or LuaTeX run.
///
/// `project_root` is the directory TeX ran in; files under it are reported relative to it.
pub fn parse_latex_log(log: &str, project_root: &Path) -> Vec<Diagnostic> {
    let mut parser = LatexLog::new(log, project_root);
    parser.run();
    dedup(parser.out)
}

/// Parses the `.blg` written by biber.
pub fn parse_biber_log(blg: &str, project_root: &Path) -> Vec<Diagnostic> {
    let paths = ProjectPaths::new(project_root);
    // Biber parses a UTF-8 copy of each `.bib` in a temporary directory and names that copy
    // in its messages, so remember the data source being read.
    let mut data_source: Option<&str> = None;
    let mut out = Vec::new();
    for line in blg.lines() {
        let Some((level, message)) = biber_message(line) else {
            continue;
        };
        let severity = match level {
            "ERROR" => Severity::Error,
            "WARN" => Severity::Warning,
            _ => {
                if let Some(source) = biber_data_source(message) {
                    data_source = Some(source);
                }
                continue;
            }
        };
        let (file, line, message) = biber_location(message, data_source);
        out.push(Diagnostic {
            severity,
            source: DiagnosticSource::Biber,
            file: file.map(|file| paths.display(file)),
            line,
            message: message.trim().to_owned(),
            details: String::new(),
        });
    }
    dedup(out)
}

/// Parses the `.blg` written by BibTeX.
pub fn parse_bibtex_log(blg: &str, project_root: &Path) -> Vec<Diagnostic> {
    let paths = ProjectPaths::new(project_root);
    let lines: Vec<&str> = blg.lines().collect();
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(&line) = lines.get(pos) {
        pos += 1;
        if let Some(text) = line.strip_prefix("Warning--") {
            // A warning about the database is followed by `--line 5 of file refs.bib`.
            let location = lines
                .get(pos)
                .and_then(|next| next.strip_prefix("--line "))
                .and_then(bibtex_location);
            if location.is_some() {
                pos += 1;
            }
            out.push(bibtex_diagnostic(
                Severity::Warning,
                text,
                location,
                Vec::new(),
                &paths,
            ));
            continue;
        }

        // Errors end with `---line 17 of file refs.bib`, either on the same line or alone on
        // the next one, or with `---while reading file main.aux`.
        let (message, mut location) = if let Some((message, location)) = line.split_once("---line ")
        {
            (message, bibtex_location(location))
        } else if let Some(location) = lines.get(pos).and_then(|l| l.strip_prefix("---line ")) {
            pos += 1;
            (line, bibtex_location(location))
        } else if let Some((message, file)) = line.split_once("---while reading file ") {
            (message, Some((file, None)))
        } else {
            continue;
        };
        let mut details = Vec::new();
        while let Some(&next) = lines.get(pos) {
            if next == "(Error may have been on previous line)" {
                if let Some((_, Some(line))) = &mut location {
                    *line = line.saturating_sub(1).max(1);
                }
            } else if !(next.starts_with(" : ")
                || next.starts_with("I'm skipping whatever remains"))
            {
                break;
            }
            // ` : ` starts both lines of the context, split where BibTeX stopped reading.
            details.push(next.strip_prefix(" : ").unwrap_or(next));
            pos += 1;
        }
        out.push(bibtex_diagnostic(
            Severity::Error,
            message,
            location,
            details,
            &paths,
        ));
    }
    dedup(out)
}

// ---------------------------------------------------------------------------------------------
// TeX logs
// ---------------------------------------------------------------------------------------------

/// Lines of context or help an error may have before TeX's `l.<n>` line.
const MAX_CONTEXT_LINES: usize = 60;
/// Lines of box content printed after an overfull or underfull box.
const MAX_BOX_LINES: usize = 50;
/// Lines of context an engine warning (e.g. `pdfTeX warning`) may have.
const MAX_WARNING_CONTEXT_LINES: usize = 8;

/// An open parenthesis of the log.
enum Group {
    /// TeX opened this file; the name is as printed.
    File(String),
    /// A parenthesis in ordinary text.
    Text,
}

struct LatexLog<'a> {
    lines: Vec<&'a str>,
    pos: usize,
    paths: ProjectPaths,
    groups: Vec<Group>,
    /// The innermost file closed by the latest `)`, as long as nothing else was printed
    /// since. A paragraph or an argument that ran to the end of that file is reported only
    /// after TeX closed it.
    just_closed: Option<String>,
    /// The paragraph (`lines a--b`) of the box warnings attributed to `just_closed`.
    closed_box_range: Option<(u32, u32)>,
    /// `Runaway argument?` and the text that follows it, shown with the next error.
    runaway: Vec<&'a str>,
    /// An error printed without context (LaTeX's prompt for a missing file); it takes the
    /// location of the error that follows it (the `Emergency stop` in nonstop mode).
    unlocated_error: Option<usize>,
    /// Index in `out` of the `Missing character` warning of each font.
    missing_chars: HashMap<String, usize>,
    out: Vec<Diagnostic>,
}

impl<'a> LatexLog<'a> {
    fn new(log: &'a str, project_root: &Path) -> Self {
        Self {
            lines: log.lines().collect(),
            pos: 0,
            paths: ProjectPaths::new(project_root),
            groups: Vec::new(),
            just_closed: None,
            closed_box_range: None,
            runaway: Vec::new(),
            unlocated_error: None,
            missing_chars: HashMap::new(),
            out: Vec::new(),
        }
    }

    fn run(&mut self) {
        while let Some(&line) = self.lines.get(self.pos) {
            if line.trim().is_empty() {
                self.groups.retain(|group| matches!(group, Group::File(_)));
                self.pos += 1;
            } else if is_runaway(line) {
                self.runaway = self.lines[self.pos..].iter().take(2).copied().collect();
                self.pos += 2;
            } else if let Some(start) = parse_error_start(line) {
                self.error(start);
            } else {
                self.runaway.clear();
                self.unlocated_error = None;
                if let Some(header) = parse_header(line).filter(|h| h.kind != Kind::Error) {
                    self.message(header);
                } else if let Some(bad_box) = parse_bad_box(line) {
                    self.bad_box(bad_box);
                } else if is_engine_warning(line) {
                    self.engine_warning(line);
                } else if line.starts_with("Missing character: ") {
                    self.missing_character(line);
                } else if line.starts_with("No file ") && line.ends_with(".bbl.") {
                    // The bibliography was never built (BibTeX or biber failed).
                    self.pos += 1;
                    let file = self.current_file();
                    self.push(Severity::Warning, file, None, line.trim().to_owned(), "");
                    self.just_closed = None;
                } else {
                    self.scan_file_markers(line);
                    self.pos += 1;
                }
            }
        }
    }

    /// An error: `./main.tex:12: <message>` or `! <message>`, the context TeX shows (ending
    /// with `l.<n>`, the line it was reading) and help text up to a blank line.
    fn error(&mut self, start: ErrorStart<'a>) {
        let pending = self.unlocated_error.take();
        let runaway = std::mem::take(&mut self.runaway);
        self.pos += 1;
        if start.message.trim_start().starts_with("==>") {
            // `==> Fatal error occurred, no output PDF file produced!` sums up earlier errors.
            return;
        }

        let mut message = start.message.trim().to_owned();
        if let Some(header) = parse_header(start.message).filter(|h| h.kind == Kind::Error) {
            while let Some(text) = self
                .lines
                .get(self.pos)
                .and_then(|line| continuation(line, header.owner, false))
            {
                push_text(&mut message, text);
                self.pos += 1;
            }
        }

        let body = self.pos;
        let mut context: Option<(usize, Option<ContextLine<'a>>)> = None;
        let mut next_error = None;
        for (index, &line) in self.lines.iter().enumerate().skip(body) {
            if index - body >= MAX_CONTEXT_LINES {
                break;
            }
            if let Some(context_line) = parse_context_line(line) {
                context = Some((index, Some(context_line)));
                break;
            }
            if line.starts_with("<*>") {
                context = Some((index, None));
                break;
            }
            if parse_error_start(line).is_some() {
                next_error = Some(index);
                break;
            }
        }

        let mut details = runaway;
        match (context, next_error) {
            (Some((bottom, context_line)), _) => {
                // An error raised by a macro (`LaTeX Error`, `Package x Error`) shows the
                // macro's internals before the line TeX was reading: keep only that line.
                let from = if message.contains("Error:") {
                    bottom
                } else {
                    body
                };
                let pair_end = (bottom + 2).min(self.lines.len());
                // `l.12` alone, at a blank line, says nothing the diagnostic does not.
                let empty_bottom = context_line.is_some_and(|line| line.read_nothing(false))
                    && self
                        .lines
                        .get(bottom + 1)
                        .is_none_or(|rest| rest.trim().is_empty());
                for (index, &line) in self.lines[from..pair_end].iter().enumerate() {
                    if !(empty_bottom && from + index == bottom) {
                        details.push(line);
                    }
                }
                self.pos = pair_end;
                // LaTeX3 (expl3) errors leave a blank line before their help text.
                let latex3 = self.lines[body..bottom]
                    .iter()
                    .any(|line| line.trim() == "For immediate help type H <return>.");
                if latex3
                    && self
                        .lines
                        .get(self.pos)
                        .is_some_and(|l| l.trim().is_empty())
                    && self
                        .lines
                        .get(self.pos + 1)
                        .is_some_and(|l| !l.trim().is_empty())
                {
                    self.pos += 1;
                }
                while let Some(&line) = self.lines.get(self.pos) {
                    if line.trim().is_empty() || parse_error_start(line).is_some() {
                        break;
                    }
                    details.push(line);
                    self.pos += 1;
                }
            }
            (None, Some(next)) => {
                details.extend(&self.lines[body..next]);
                self.pos = next;
            }
            (None, None) => {
                let mut seen_text = false;
                while let Some(&line) = self.lines.get(self.pos) {
                    if self.pos - body >= MAX_CONTEXT_LINES || parse_error_start(line).is_some() {
                        break;
                    }
                    if line.trim().is_empty() {
                        if seen_text {
                            break;
                        }
                    } else {
                        seen_text = true;
                        details.push(line);
                    }
                    self.pos += 1;
                }
            }
        }

        let (mut file, mut line) = match start.location {
            Some((file, line)) => (Some(self.paths.display(file)), Some(line)),
            None => (
                self.current_file(),
                context.and_then(|(_, line)| line).map(|line| line.number),
            ),
        };
        if let Some((_, Some(context_line))) = context {
            let missing_file = is_missing_file_error(&message)
                || pending.is_some_and(|index| is_missing_file_error(&self.out[index].message));
            if line == Some(context_line.number) && context_line.read_nothing(missing_file) {
                line = Some(context_line.number.saturating_sub(1).max(1));
            }
        }
        let at_end_of_file =
            message.starts_with("File ended while scanning") || matches!(context, Some((_, None)));
        if at_end_of_file && let Some(closed) = self.just_closed.take() {
            file = Some(self.paths.display(&closed));
            line = None;
        }

        let index = self.push(
            Severity::Error,
            file,
            line,
            message,
            &join_details(&details),
        );
        if let Some(pending) = pending {
            let (file, line) = (self.out[index].file.clone(), self.out[index].line);
            self.out[pending].file = file;
            self.out[pending].line = line;
        }
        if context.is_none() && next_error.is_some() {
            self.unlocated_error = Some(index);
        }
        self.just_closed = None;
    }

    /// `Package x Warning: ...`, `LaTeX Font Info: ...` and the like, with their
    /// continuation lines. Info messages are skipped.
    fn message(&mut self, header: Header<'a>) {
        let mut text = self.lines[self.pos].trim().to_owned();
        self.pos += 1;
        while let Some(more) = self
            .lines
            .get(self.pos)
            .and_then(|line| continuation(line, header.owner, true))
        {
            push_text(&mut text, more);
            self.pos += 1;
        }
        if header.kind != Kind::Warning {
            return;
        }
        let severity = if is_rerun_notice(&text) {
            Severity::Info
        } else {
            Severity::Warning
        };
        let line = input_line(&text);
        let file = self.current_file();
        self.push(severity, file, line, text, "");
        self.just_closed = None;
    }

    /// `Overfull \hbox (12.0pt too wide) in paragraph at lines 10--12` and the box content
    /// printed after it, up to a blank line.
    fn bad_box(&mut self, bad_box: BadBox<'a>) {
        self.pos += 1;
        let mut content = Vec::new();
        if bad_box.hbox {
            // A line showing the text of the box comes first; it may be empty.
            if let Some(&line) = self.lines.get(self.pos) {
                content.push(line);
                self.pos += 1;
            }
        }
        while let Some(&line) = self.lines.get(self.pos) {
            if line.trim().is_empty() || content.len() >= MAX_BOX_LINES {
                break;
            }
            content.push(line);
            self.pos += 1;
        }
        // `[]` stands for boxes TeX does not show.
        content.retain(|line| !line.chars().all(|c| matches!(c, '[' | ']' | ' ')));

        let file = match (bad_box.paragraph, &self.just_closed) {
            (Some(paragraph), Some(closed))
                if self.closed_box_range.is_none_or(|range| range == paragraph) =>
            {
                // The paragraph started in the file TeX has just closed.
                self.closed_box_range = Some(paragraph);
                Some(self.paths.display(closed))
            }
            _ => {
                self.just_closed = None;
                self.current_file()
            }
        };
        self.push(
            Severity::Info,
            file,
            bad_box.line,
            bad_box.message.to_owned(),
            &join_details(&content),
        );
    }

    /// `pdfTeX warning (ext4): ...`, possibly followed by context down to `l.<n>`. Output
    /// continues on the line after `l.<n>`, so that line is scanned as usual.
    fn engine_warning(&mut self, warning: &'a str) {
        self.pos += 1;
        let mut context = None;
        for (index, &line) in self.lines.iter().enumerate().skip(self.pos) {
            if index - self.pos >= MAX_WARNING_CONTEXT_LINES
                || line.trim().is_empty()
                || parse_error_start(line).is_some()
            {
                break;
            }
            if let Some(context_line) = parse_context_line(line) {
                context = Some((index, context_line));
                break;
            }
        }
        let (line, details) = match context {
            Some((bottom, context_line)) => {
                let details = join_details(&self.lines[self.pos..=bottom]);
                self.pos = bottom + 1;
                (Some(context_line.number), details)
            }
            None => (None, String::new()),
        };
        let file = self.current_file();
        self.push(
            Severity::Warning,
            file,
            line,
            warning.trim().to_owned(),
            &details,
        );
        self.just_closed = None;
    }

    /// `Missing character: There is no X in font Y!`, one warning per font.
    fn missing_character(&mut self, line: &str) {
        self.pos += 1;
        let line = line.trim();
        let font = line
            .rsplit_once(" in font ")
            .map_or("", |(_, font)| font.trim_end_matches('!'));
        if let Some(&index) = self.missing_chars.get(font) {
            let diagnostic = &mut self.out[index];
            if diagnostic.message != line && !diagnostic.details.lines().any(|seen| seen == line) {
                if !diagnostic.details.is_empty() {
                    diagnostic.details.push('\n');
                }
                diagnostic.details.push_str(line);
            }
        } else {
            let file = self.current_file();
            let index = self.push(Severity::Warning, file, None, line.to_owned(), "");
            self.missing_chars.insert(font.to_owned(), index);
        }
        self.just_closed = None;
    }

    /// Follows the files TeX opens with `(<name>` and closes with `)`.
    fn scan_file_markers(&mut self, line: &str) {
        let bytes = line.as_bytes();
        let mut index = 0;
        while let Some(&byte) = bytes.get(index) {
            index += 1;
            match byte {
                b'(' => {
                    self.just_closed = None;
                    match file_name_after_paren(&line[index..]) {
                        Some((name, len)) => {
                            self.groups.push(Group::File(name.to_owned()));
                            index += len;
                        }
                        None => self.groups.push(Group::Text),
                    }
                }
                b')' => {
                    if let Some(Group::File(name)) = self.groups.pop()
                        && self.just_closed.is_none()
                        && !is_generated_file(&name)
                    {
                        self.just_closed = Some(name);
                        self.closed_box_range = None;
                    }
                }
                byte if byte.is_ascii_whitespace() => {}
                _ => self.just_closed = None,
            }
        }
    }

    /// The file TeX is reading, for display. A message raised while TeX reads a file LaTeX
    /// wrote (`Label multiply defined` from the `.aux`, a missing entry from the `.bbl`) has
    /// its cause elsewhere, so its location is unknown.
    fn current_file(&self) -> Option<String> {
        let name = self.groups.iter().rev().find_map(|group| match group {
            Group::File(name) => Some(name),
            Group::Text => None,
        })?;
        (!is_generated_file(name)).then(|| self.paths.display(name))
    }

    fn push(
        &mut self,
        severity: Severity,
        file: Option<String>,
        line: Option<u32>,
        message: String,
        details: &str,
    ) -> usize {
        // A line number means nothing without its file (e.g. one of a beamer `.vrb`).
        let line = file.as_ref().and(line);
        self.out.push(Diagnostic {
            severity,
            source: DiagnosticSource::Latex,
            file,
            line,
            message,
            details: details.to_owned(),
        });
        self.out.len() - 1
    }
}

struct ErrorStart<'a> {
    /// File and line of a `-file-line-error` message.
    location: Option<(&'a str, u32)>,
    message: &'a str,
}

/// Recognises `./chapters/intro.tex:7: Undefined control sequence.` and
/// `! Undefined control sequence.`
fn parse_error_start(line: &str) -> Option<ErrorStart<'_>> {
    if let Some(message) = line.strip_prefix('!') {
        return Some(ErrorStart {
            location: None,
            message: message.trim_start(),
        });
    }
    // The file name may contain spaces but not `:<digits>: `.
    for (colon, _) in line.match_indices(':') {
        let after = &line[colon + 1..];
        let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 {
            continue;
        }
        let rest = &after[digits..];
        let message = match rest.strip_prefix(": ") {
            Some(message) => message,
            None if rest == ":" => "",
            None => continue,
        };
        let file = &line[..colon];
        if !(looks_like_path_start(file) || has_extension(file)) {
            return None;
        }
        let number = after[..digits].parse().ok()?;
        return Some(ErrorStart {
            location: Some((file, number)),
            message,
        });
    }
    None
}

/// `l.12 \foo`: the bottom line of TeX's error context.
#[derive(Clone, Copy)]
struct ContextLine<'a> {
    number: u32,
    /// What TeX had read of that line when it stopped.
    read: &'a str,
}

impl ContextLine<'_> {
    /// Whether TeX had read nothing of the line yet, so what it complains about comes from
    /// an earlier line: the blank line ending a paragraph, or a command that looked ahead
    /// for an optional argument (`\usepackage{x}`, `\setmainfont{X}`). When LaTeX looks for
    /// a missing package, the next line's first command may already be read as well.
    fn read_nothing(&self, missing_file: bool) -> bool {
        self.read.is_empty()
            || self.read == "^^M"
            || (missing_file && is_single_control_word(self.read))
    }
}

fn parse_context_line(line: &str) -> Option<ContextLine<'_>> {
    let rest = line.strip_prefix("l.")?;
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let read = &rest[digits..];
    if digits == 0 || !(read.is_empty() || read.starts_with(' ')) {
        return None;
    }
    Some(ContextLine {
        number: rest[..digits].parse().ok()?,
        read: read.trim(),
    })
}

fn is_single_control_word(text: &str) -> bool {
    text.strip_prefix('\\').is_some_and(|name| {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphabetic() || c == '@')
    })
}

/// `LaTeX Error: File `foo.sty' not found.`
fn is_missing_file_error(message: &str) -> bool {
    message.starts_with("LaTeX Error: File `") && message.ends_with("' not found.")
}

fn is_runaway(line: &str) -> bool {
    matches!(
        line.trim_end(),
        "Runaway argument?" | "Runaway definition?" | "Runaway preamble?" | "Runaway text?"
    )
}

fn is_engine_warning(line: &str) -> bool {
    ["pdfTeX warning", "XeTeX warning", "LuaTeX warning"]
        .iter()
        .any(|prefix| line.starts_with(prefix))
}

/// Requests to compile again, which latexmk takes care of.
fn is_rerun_notice(text: &str) -> bool {
    text.contains("Rerun to get")
        || text.contains("Label(s) may have changed")
        || text.contains("Please rerun LaTeX")
        || text.ends_with("Rerun LaTeX.")
}

/// `... on input line 12.`
fn input_line(text: &str) -> Option<u32> {
    let (_, rest) = text.rsplit_once("on input line ")?;
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Warning,
    Info,
    Error,
}

/// Who wrote a message, which decides how its continuation lines look.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner<'a> {
    /// `Package hyperref Warning:`, `LaTeX Font Info:`: continued by `(hyperref)` lines.
    Named(&'a str),
    /// `LaTeX Warning:`: continued by indented lines.
    Kernel,
}

#[derive(Clone, Copy)]
struct Header<'a> {
    kind: Kind,
    owner: Owner<'a>,
}

/// Recognises `Package <name> Warning:`, `Class <name> Info:`, `LaTeX Warning:`,
/// `LaTeX Font Info:`, `Package <name> Error:` and so on.
fn parse_header(text: &str) -> Option<Header<'_>> {
    let (head, _) = text.split_once(':')?;
    let (head, kind) = [
        (" Warning", Kind::Warning),
        (" Info", Kind::Info),
        (" Error", Kind::Error),
    ]
    .into_iter()
    .find_map(|(suffix, kind)| head.strip_suffix(suffix).map(|head| (head, kind)))?;
    let mut words = head.split(' ');
    let owner = match (words.next(), words.next(), words.next()) {
        (Some("Package" | "Class" | "Module"), Some(name), None) if !name.is_empty() => {
            Owner::Named(name)
        }
        (Some("LaTeX" | "LaTeX3"), None, None) => Owner::Kernel,
        (Some("LaTeX"), Some(name), None) if !name.is_empty() => Owner::Named(name),
        _ => return None,
    };
    Some(Header { kind, owner })
}

/// The text of a continuation line of a message, written by `\MessageBreak`.
fn continuation<'l>(line: &'l str, owner: Owner<'_>, allow_indent: bool) -> Option<&'l str> {
    if let Some(rest) = line.strip_prefix('(') {
        let (name, text) = rest.split_once(')')?;
        let expected = match owner {
            Owner::Named(name) => name,
            Owner::Kernel => "LaTeX",
        };
        return (name == expected && (text.is_empty() || text.starts_with(' ')))
            .then(|| text.trim());
    }
    let indented = line.starts_with([' ', '\t']);
    let text = line.trim();
    (allow_indent
        && owner == Owner::Kernel
        && indented
        && !text.is_empty()
        && !text.starts_with('('))
    .then_some(text)
}

fn push_text(message: &mut String, text: &str) {
    if text.is_empty() {
        return;
    }
    if !message.is_empty() && !message.ends_with(char::is_whitespace) {
        message.push(' ');
    }
    message.push_str(text);
}

struct BadBox<'a> {
    message: &'a str,
    hbox: bool,
    /// First line of the material.
    line: Option<u32>,
    /// `lines a--b` of a paragraph or an alignment.
    paragraph: Option<(u32, u32)>,
}

fn parse_bad_box(line: &str) -> Option<BadBox<'_>> {
    let kind = line
        .strip_prefix("Overfull \\")
        .or_else(|| line.strip_prefix("Underfull \\"))?;
    let hbox = kind.starts_with("hbox");
    if !(hbox || kind.starts_with("vbox")) {
        return None;
    }
    let message = line.trim_end();
    let message = message.strip_suffix(" []").unwrap_or(message).trim_end();
    let mut bad_box = BadBox {
        message,
        hbox,
        line: None,
        paragraph: None,
    };
    if let Some((_, lines)) = message.split_once(" at lines ") {
        let (first, last) = lines.split_once("--")?;
        let first = leading_number(first)?;
        bad_box.line = Some(first);
        bad_box.paragraph = Some((first, leading_number(last)?));
    } else if let Some((_, line)) = message.split_once(" detected at line ") {
        bad_box.line = leading_number(line);
    }
    Some(bad_box)
}

fn leading_number(text: &str) -> Option<u32> {
    let end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    text[..end].parse().ok()
}

/// Reads the name TeX prints after `(` when it opens a file; returns it with its length in
/// bytes. TeX does not quote names with spaces, so a first word without an extension is
/// extended to the next word that has one.
fn file_name_after_paren(rest: &str) -> Option<(&str, usize)> {
    if let Some(quoted) = rest.strip_prefix('"') {
        let end = quoted.find('"')?;
        let name = &quoted[..end];
        return looks_like_path_start(name).then_some((name, end + 2));
    }
    let span_end = rest
        .find(['(', ')', '[', ']', '{', '}', '<', '>', '"'])
        .unwrap_or(rest.len());
    let span = &rest[..span_end];
    let first_end = span.find(char::is_whitespace).unwrap_or(span.len());
    let first = &span[..first_end];
    if !looks_like_path_start(first) {
        return None;
    }
    if has_extension(first) {
        return Some((first, first_end));
    }
    let mut end = first_end;
    while let Some(start) = span[end..].find(|c: char| !c.is_whitespace()) {
        let start = end + start;
        end = span[start..]
            .find(char::is_whitespace)
            .map_or(span.len(), |len| start + len);
        if has_extension(&span[..end]) {
            return Some((&span[..end], end));
        }
    }
    Some((first, first_end))
}

/// Extensions of the files TeX may print without a directory.
const TEX_EXTENSIONS: &[&str] = &[
    "aux", "bbl", "bbx", "bib", "cbx", "cfg", "cls", "clo", "dbx", "def", "dtx", "fd", "gls",
    "ind", "ldf", "lbx", "lof", "lot", "ltx", "lua", "mkii", "nav", "out", "snm", "sty", "tex",
    "toc", "vrb",
];

fn looks_like_path_start(name: &str) -> bool {
    let bytes = name.as_bytes();
    if name.starts_with("./") || name.starts_with("../") || name.starts_with("~/") {
        return true;
    }
    if let [b'/', next, ..] = bytes {
        return !next.is_ascii_whitespace();
    }
    if let [drive, b':', b'/' | b'\\', ..] = bytes {
        return drive.is_ascii_alphabetic();
    }
    // A bare relative name, e.g. `main.aux`.
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '+'))
        && extension(name).is_some_and(|ext| TEX_EXTENSIONS.contains(&ext))
}

/// Files written by LaTeX or by bibliography and index tools, read back on the next run.
fn is_generated_file(name: &str) -> bool {
    const GENERATED: &[&str] = &[
        "acr", "aux", "bbl", "brf", "gls", "idx", "ind", "loa", "lof", "lol", "lot", "nav", "out",
        "snm", "thm", "toc", "vrb",
    ];
    extension(name.trim_matches('"')).is_some_and(|ext| GENERATED.contains(&ext))
}

fn has_extension(name: &str) -> bool {
    extension(name).is_some()
}

fn extension(name: &str) -> Option<&str> {
    let base = name.rsplit(['/', '\\']).next()?;
    let (stem, ext) = base.rsplit_once('.')?;
    let valid = !stem.is_empty()
        && (1..=8).contains(&ext.len())
        && ext.starts_with(|c: char| c.is_ascii_alphabetic())
        && ext.chars().all(|c| c.is_ascii_alphanumeric());
    valid.then_some(ext)
}

/// Generic instructions TeX and LaTeX print with errors, useless outside a terminal.
fn is_boilerplate(line: &str) -> bool {
    const LINES: &[&str] = &[
        "...",
        "Type  H <return>  for immediate help.",
        "For immediate help type H <return>.",
        "Try typing  <return>  to proceed.",
        "If that doesn't work, type  X <return>  to quit.",
        "Type  I <command> <return>  to replace it with another command,",
        "or  <return>  to continue without it.",
        "Type X to quit or <RETURN> to proceed,",
        "Enter file name:",
    ];
    LINES.contains(&line)
        || (line.starts_with("See the ") && line.ends_with(" for explanation."))
        || line.starts_with("or enter new name. (Default extension:")
}

fn join_details(lines: &[&str]) -> String {
    let kept: Vec<&str> = lines
        .iter()
        .map(|line| line.trim_end())
        .filter(|line| !line.is_empty() && !is_boilerplate(line.trim()))
        .collect();
    kept.join("\n").trim().to_owned()
}

// ---------------------------------------------------------------------------------------------
// Biber and BibTeX
// ---------------------------------------------------------------------------------------------

/// `[177] Biber.pm:132> WARN - message` (the prefix is missing on the terminal).
fn biber_message(line: &str) -> Option<(&str, &str)> {
    let rest = if line.starts_with('[') {
        line.split_once("> ")?.1
    } else {
        line
    };
    let (level, message) = rest.split_once(" - ")?;
    matches!(level, "INFO" | "WARN" | "ERROR").then_some((level, message))
}

/// `Found BibTeX data source 'refs.bib'`
fn biber_data_source(message: &str) -> Option<&str> {
    let rest = message.strip_prefix("Found ")?;
    let (_, rest) = rest.split_once(" data source '")?;
    rest.strip_suffix('\'')
}

fn biber_location<'m>(
    message: &'m str,
    data_source: Option<&'m str>,
) -> (Option<&'m str>, Option<u32>, &'m str) {
    // `BibTeX subsystem: /tmp/biber_tmp_x/8fe4..._10.utf8, line 11, syntax error: ...`
    if let Some(rest) = message.strip_prefix("BibTeX subsystem: ")
        && let Some((path, rest)) = rest.split_once(", line ")
        && let Some((number, text)) = rest.split_once(", ")
        && let Ok(mut line) = number.parse::<u32>()
    {
        let copy = path.ends_with(".utf8") || path.contains("biber_tmp_");
        let file = if copy { data_source } else { Some(path) };
        // btparse stops at the token after a missing comma, usually on the next line.
        if text.starts_with("syntax error: found") && text.contains("expected end of entry") {
            line = line.saturating_sub(1).max(1);
        }
        return (file, Some(line), text);
    }
    // `Duplicate entry key: 'x' in file 'refs.bib', skipping ...`
    if let Some((_, rest)) = message.split_once(" in file '")
        && let Some((file, _)) = rest.split_once('\'')
    {
        return (Some(file), None, message);
    }
    // `Datamodel: Entry 'x' (refs.bib): Missing mandatory field 'author'`
    if let Some((_, rest)) = message.split_once("Entry '")
        && let Some((_, rest)) = rest.split_once("' (")
        && let Some((file, _)) = rest.split_once("):")
    {
        return (Some(file), None, message);
    }
    (None, None, message)
}

/// `17 of file refs.bib`
fn bibtex_location(text: &str) -> Option<(&str, Option<u32>)> {
    let (number, file) = text.split_once(" of file ")?;
    Some((file.trim(), number.trim().parse().ok()))
}

fn bibtex_diagnostic(
    severity: Severity,
    message: &str,
    location: Option<(&str, Option<u32>)>,
    details: Vec<&str>,
    paths: &ProjectPaths,
) -> Diagnostic {
    // BibTeX runs in the build directory and reads the `.aux` there; the `\bibdata` or
    // `\citation` behind such an error is not in the project.
    let location = location.filter(|(file, _)| !is_generated_file(file));
    Diagnostic {
        severity,
        source: DiagnosticSource::Bibtex,
        file: location.map(|(file, _)| paths.display(file)),
        line: location.and_then(|(_, line)| line),
        message: message.trim().to_owned(),
        details: join_details(&details),
    }
}

// ---------------------------------------------------------------------------------------------
// Shared
// ---------------------------------------------------------------------------------------------

/// Rewrites file names from logs relative to the project.
struct ProjectPaths {
    /// The project root with `/` separators and no trailing slash, unless it is empty.
    root: Option<String>,
}

impl ProjectPaths {
    fn new(project_root: &Path) -> Self {
        let root = project_root.to_string_lossy().replace('\\', "/");
        let root = (!root.is_empty()).then(|| root.trim_end_matches('/').to_owned());
        Self { root }
    }

    /// The project-relative path of `printed` if it is inside the project, else `printed`.
    fn display(&self, printed: &str) -> String {
        let printed = printed.trim();
        let printed = printed
            .strip_prefix('"')
            .and_then(|name| name.strip_suffix('"'))
            .unwrap_or(printed);
        let slashed = printed.replace('\\', "/");
        let absolute = slashed.starts_with('/')
            || matches!(slashed.as_bytes(), [drive, b':', b'/', ..] if drive.is_ascii_alphabetic());
        let relative = if absolute {
            let inside = self.root.as_deref().and_then(|root| {
                slashed
                    .strip_prefix(root)
                    .and_then(|rest| rest.strip_prefix('/'))
            });
            match inside {
                Some(rest) => rest,
                None => return printed.to_owned(),
            }
        } else {
            &slashed
        };
        normalize_relative(relative).unwrap_or_else(|| printed.to_owned())
    }
}

/// Removes `.` and resolves `..`; `None` if the path leaves the project or is empty.
fn normalize_relative(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Drops repeated diagnostics, keeping the first of each.
fn dedup(diagnostics: Vec<Diagnostic>) -> Vec<Diagnostic> {
    let mut seen = HashSet::new();
    diagnostics
        .into_iter()
        .filter(|diagnostic| seen.insert(diagnostic.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "/data/projects/fixture";

    fn latex(log: &str) -> Vec<Diagnostic> {
        parse_latex_log(log, Path::new(ROOT))
    }

    fn located(diagnostics: &[Diagnostic]) -> Vec<(Severity, Option<&str>, Option<u32>)> {
        diagnostics
            .iter()
            .map(|d| (d.severity, d.file.as_deref(), d.line))
            .collect()
    }

    /// Every file TeX opens in the fixture logs is closed again by the end, except the ones
    /// still open when an emergency stop aborted the run.
    #[test]
    fn file_markers_balance_in_every_fixture_log() {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/logs");
        let mut checked = 0;
        for case in std::fs::read_dir(&fixtures).unwrap() {
            let path = case.unwrap().path().join("main.log");
            let log = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
            let mut parser = LatexLog::new(&log, Path::new(ROOT));
            parser.run();
            let open: Vec<&str> = parser
                .groups
                .iter()
                .filter_map(|group| match group {
                    Group::File(name) => Some(name.as_str()),
                    Group::Text => None,
                })
                .collect();
            let expected: &[&str] = if log.contains("Emergency stop.") {
                &["./main.tex"]
            } else {
                &[]
            };
            assert_eq!(open, expected, "{}", path.display());
            checked += 1;
        }
        assert!(checked >= 12, "only {checked} fixture logs");
    }

    #[test]
    fn normalises_paths() {
        let paths = ProjectPaths::new(Path::new("/data/projects/fixture/"));
        assert_eq!(paths.display("./main.tex"), "main.tex");
        assert_eq!(
            paths.display("./chapters/./intro.tex"),
            "chapters/intro.tex"
        );
        assert_eq!(paths.display("chapters/../main.tex"), "main.tex");
        assert_eq!(
            paths.display("/data/projects/fixture/chapters/intro.tex"),
            "chapters/intro.tex"
        );
        assert_eq!(paths.display("\"./my intro.tex\""), "my intro.tex");
        // Outside the project: as printed.
        assert_eq!(
            paths.display("/usr/local/texlive/2026/texmf-dist/tex/latex/base/article.cls"),
            "/usr/local/texlive/2026/texmf-dist/tex/latex/base/article.cls"
        );
        assert_eq!(
            paths.display("/data/projects/fixture-old/main.tex"),
            "/data/projects/fixture-old/main.tex"
        );
        assert_eq!(
            paths.display("../shared/macros.tex"),
            "../shared/macros.tex"
        );

        let windows = ProjectPaths::new(Path::new(r"C:\Users\me\thesis"));
        assert_eq!(
            windows.display(r"C:\Users\me\thesis\chapters\intro.tex"),
            "chapters/intro.tex"
        );
    }

    #[test]
    fn reads_file_names_after_parentheses() {
        assert_eq!(
            file_name_after_paren("./main.tex"),
            Some(("./main.tex", 10))
        );
        assert_eq!(
            file_name_after_paren("/data/build/fixture/main.aux) [1]"),
            Some(("/data/build/fixture/main.aux", 28))
        );
        // Names with spaces are printed unquoted, or quoted by some engines.
        assert_eq!(
            file_name_after_paren("./chapters/my intro.tex [2]"),
            Some(("./chapters/my intro.tex", 23))
        );
        assert_eq!(
            file_name_after_paren("\"./my intro.tex\")"),
            Some(("./my intro.tex", 16))
        );
        assert_eq!(file_name_after_paren("main.toc)"), Some(("main.toc", 8)));
        for text in [
            "TeX Live 2026)",
            "see the transcript file)",
            "load luc: /x/y.luc)",
            "1 page, 7665 bytes).",
            "e.g., `\\hobx')",
            "v1.4n Standard)",
            "PK/MW)",
            "",
        ] {
            assert_eq!(file_name_after_paren(text), None, "{text}");
        }
    }

    #[test]
    fn parentheses_in_text_do_not_move_the_file_stack() {
        let log = "\
This is pdfTeX, Version 3.141592653-2.6-1.40.29 (TeX Live 2026) (preloaded format=pdflatex)
(./main.tex
LaTeX2e <2026-11-01>
Package: infwarerr 2019/12/03 v1.5 Providing info/warning/error messages (HO)
Package fontspec Info: Adjusting the maths setup (use [no-math] to avoid
(fontspec)             this).
Some free text (opened here
and closed on the next line) of the same paragraph.
(./chapters/intro.tex
Unbalanced free text (never closed

LaTeX Warning: Reference `x' on page 1 undefined on input line 5.

) [1] (/data/build/fixture/main.aux)
Output written on /data/build/fixture/main.pdf (1 page, 14971 bytes).

LaTeX Warning: There were undefined references.

)";
        assert_eq!(
            located(&latex(log)),
            [
                (Severity::Warning, Some("chapters/intro.tex"), Some(5)),
                (Severity::Warning, Some("main.tex"), None),
            ]
        );
    }

    #[test]
    fn classic_errors_take_the_file_from_the_stack_and_the_line_from_the_context() {
        let log = "\
(./main.tex (./chapters/intro.tex
! Undefined control sequence.
<argument> \\foo
               (unbalanced
l.7 \\textbf{\\foo}
                 bar (
The control sequence at the end of the top line
of your error message was never \\def'ed.

)
! Missing $ inserted.
<inserted text>
                $
l.12

I've inserted a begin-math/end-math symbol since I think
you left one out. Proceed, with fingers crossed.

)";
        let diagnostics = latex(log);
        assert_eq!(
            located(&diagnostics),
            [
                (Severity::Error, Some("chapters/intro.tex"), Some(7)),
                // Nothing of line 12 was read: the culprit is above it.
                (Severity::Error, Some("main.tex"), Some(11)),
            ]
        );
        assert_eq!(diagnostics[0].message, "Undefined control sequence.");
        assert_eq!(
            diagnostics[0].details,
            "<argument> \\foo\n               (unbalanced\nl.7 \\textbf{\\foo}\n                 \
             bar (\nThe control sequence at the end of the top line\nof your error message \
             was never \\def'ed."
        );
    }

    #[test]
    fn box_ending_after_a_subfile_points_into_the_subfile() {
        let log = "\
(./main.tex (./chapters/wide.tex)
Overfull \\hbox (64.5pt too wide) in paragraph at lines 2--15
[]\\OT1/cmr/m/n/10 Some (wide text
 []


Underfull \\hbox (badness 10000) in paragraph at lines 2--15

 []


Overfull \\hbox (3.0pt too wide) in paragraph at lines 17--18
[]\\OT1/cmr/m/n/10 Other text

)";
        let diagnostics = latex(log);
        assert_eq!(
            located(&diagnostics),
            [
                (Severity::Info, Some("chapters/wide.tex"), Some(2)),
                (Severity::Info, Some("chapters/wide.tex"), Some(2)),
                (Severity::Info, Some("main.tex"), Some(17)),
            ]
        );
        assert_eq!(diagnostics[0].details, "[]\\OT1/cmr/m/n/10 Some (wide text");
    }

    #[test]
    fn rerun_requests_are_info() {
        let log = "\
(./main.tex

LaTeX Warning: Label(s) may have changed. Rerun to get cross-references right.


Package rerunfilecheck Warning: File `main.out' has changed.
(rerunfilecheck)                Rerun to get outlines right
(rerunfilecheck)                or use package `bookmark'.


Package biblatex Warning: Please (re)run Biber on the file:
(biblatex)                main
(biblatex)                and rerun LaTeX afterwards.

)";
        let diagnostics = latex(log);
        assert_eq!(
            diagnostics
                .iter()
                .map(|d| (d.severity, d.message.as_str()))
                .collect::<Vec<_>>(),
            [
                (
                    Severity::Info,
                    "LaTeX Warning: Label(s) may have changed. Rerun to get cross-references \
                     right."
                ),
                (
                    Severity::Info,
                    "Package rerunfilecheck Warning: File `main.out' has changed. Rerun to get \
                     outlines right or use package `bookmark'."
                ),
                (
                    Severity::Warning,
                    "Package biblatex Warning: Please (re)run Biber on the file: main and rerun \
                     LaTeX afterwards."
                ),
            ]
        );
    }

    #[test]
    fn reports_a_missing_bbl_and_indented_kernel_warning_lines() {
        let log = "\
(./main.tex

LaTeX Warning: Unused global option(s):
    [a4paper].

No file main.bbl.
)";
        assert_eq!(
            latex(log)
                .iter()
                .map(|d| (d.message.as_str(), d.file.as_deref()))
                .collect::<Vec<_>>(),
            [
                (
                    "LaTeX Warning: Unused global option(s): [a4paper].",
                    Some("main.tex")
                ),
                ("No file main.bbl.", Some("main.tex")),
            ]
        );
    }

    #[test]
    fn missing_end_of_document_points_at_the_closed_file() {
        let log = "\
(./main.tex
LaTeX2e <2026-11-01>
)
! Emergency stop.
<*> main.tex

*** (job aborted, no legal \\end found)

";
        let diagnostics = latex(log);
        assert_eq!(
            located(&diagnostics),
            [(Severity::Error, Some("main.tex"), None)]
        );
        assert_eq!(
            diagnostics[0].details,
            "<*> main.tex\n*** (job aborted, no legal \\end found)"
        );
    }

    #[test]
    fn collapses_duplicates() {
        let warning = "\nLaTeX Warning: Citation `x' on page 1 undefined on input line 3.\n\n";
        let log = format!("(./main.tex\n{warning}{warning})");
        assert_eq!(latex(&log).len(), 1);
    }

    #[test]
    fn bibtex_cannot_open_a_database() {
        // Written by BibTeX 0.99e for `\bibliography{nosuchdb}`.
        let blg = "\
This is BibTeX, Version 0.99e (TeX Live 2026)
Capacity: max_strings=200000, hash_size=200000, hash_prime=170003
The top-level auxiliary file: main.aux
The style file: plain.bst
I couldn't open database file nosuchdb.bib
---line 4 of file main.aux
 : \\bibdata{nosuchdb
 :                  }
I'm skipping whatever remains of this command
I found no database files---while reading file main.aux
Warning--I didn't find a database entry for \"knuth1984\"
Warning--string name \"jan\" is undefined
--line 5 of file refs.bib
(There were 2 error messages)
";
        let diagnostics = parse_bibtex_log(blg, Path::new(ROOT));
        assert_eq!(
            diagnostics
                .iter()
                .map(|d| (d.severity, d.file.as_deref(), d.line, d.message.as_str()))
                .collect::<Vec<_>>(),
            [
                (
                    Severity::Error,
                    None,
                    None,
                    "I couldn't open database file nosuchdb.bib"
                ),
                (Severity::Error, None, None, "I found no database files"),
                (
                    Severity::Warning,
                    None,
                    None,
                    "I didn't find a database entry for \"knuth1984\""
                ),
                (
                    Severity::Warning,
                    Some("refs.bib"),
                    Some(5),
                    "string name \"jan\" is undefined"
                ),
            ]
        );
        assert_eq!(
            diagnostics[0].details,
            "\\bibdata{nosuchdb\n                 }\nI'm skipping whatever remains of this command"
        );
    }

    #[test]
    fn biber_maps_its_utf8_copies_back_to_each_data_source() {
        let blg = "\
[113] Biber.pm:4680> INFO - Looking for bibtex file 'a.bib' for section 0
[115] bibtex.pm:1519> INFO - Found BibTeX data source 'a.bib'
[116] Utils.pm:480> WARN - BibTeX subsystem: /tmp/biber_tmp_x/0cc1_10.utf8, line 4, warning: undefined macro \"jan\"
[117] bibtex.pm:1519> INFO - Found BibTeX data source '/data/projects/fixture/bib/b.bib'
[132] Utils.pm:480> ERROR - BibTeX subsystem: /tmp/biber_tmp_x/8fe4_10.utf8, line 11, syntax error: found \"year\", expected end of entry (\"}\" or \")\") (skipping to next \"@\")
[140] Biber.pm:132> WARN - Datamodel: Entry 'k' (b.bib): Missing mandatory field 'author'
";
        let diagnostics = parse_biber_log(blg, Path::new(ROOT));
        assert_eq!(
            diagnostics
                .iter()
                .map(|d| (d.severity, d.file.as_deref(), d.line))
                .collect::<Vec<_>>(),
            [
                (Severity::Warning, Some("a.bib"), Some(4)),
                (Severity::Error, Some("bib/b.bib"), Some(10)),
                (Severity::Warning, Some("b.bib"), None),
            ]
        );
        assert_eq!(diagnostics[0].message, "warning: undefined macro \"jan\"");
    }
}
