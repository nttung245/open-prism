//! Tools the agent can call. Reading tools run immediately; `propose_edit` only produces a
//! patch for the user to review: no tool writes to the project.

use std::path::Path;

use openprism_core::files::{self, EntryKind};
use openprism_core::latexlog::{Diagnostic, Severity};
use openprism_core::paths::{RelPath, resolve};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::llm::ToolSpec;
use crate::patch::{Hunk, Patch, PatchStatus, locate};

/// Longest file content returned by `read_file`.
const MAX_READ_BYTES: usize = 60_000;
const MAX_LIST_ENTRIES: usize = 400;
const MAX_SEARCH_HITS: usize = 80;
const MAX_OUTLINE_LINES: usize = 300;

/// What a tool may look at besides the project files.
#[derive(Debug, Clone, Default)]
pub struct ToolContext {
    /// Absolute project directory.
    pub project_root: std::path::PathBuf,
    pub main_file: String,
    /// The latest compilation, if any.
    pub compile: Option<CompileSummary>,
}

#[derive(Debug, Clone, Default)]
pub struct CompileSummary {
    /// e.g. "success", "failed".
    pub status: String,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutcome {
    Ok(String),
    Err(String),
    /// `propose_edit` produced a valid patch, awaiting the user's decision.
    Proposed(Patch),
}

pub const PROPOSE_EDIT: &str = "propose_edit";

pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_files",
            description: "List the files of the LaTeX project with their sizes.",
            schema: json!({"type": "object", "properties": {}}),
        },
        ToolSpec {
            name: "read_file",
            description: "Read a text file of the project. Long files are returned in parts: \
                          pass start_line/end_line (1-based, inclusive) to read more.",
            schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Project-relative path, e.g. chapters/intro.tex"},
                    "start_line": {"type": "integer", "minimum": 1},
                    "end_line": {"type": "integer", "minimum": 1}
                },
                "required": ["path"]
            }),
        },
        ToolSpec {
            name: "search",
            description: "Case-insensitive text search in the project's text files. Returns \
                          matching lines as path:line: text.",
            schema: json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"]
            }),
        },
        ToolSpec {
            name: "outline",
            description: "Structure of the document: parts, chapters, sections, labels, \
                          included files and bibliography, with file and line.",
            schema: json!({"type": "object", "properties": {}}),
        },
        ToolSpec {
            name: "compile_log",
            description: "Result of the latest compilation: errors and warnings with file \
                          and line.",
            schema: json!({"type": "object", "properties": {}}),
        },
        ToolSpec {
            name: PROPOSE_EDIT,
            description: "Propose changes to one file. The user reviews them before anything \
                          is written, so never claim a change is done before they accept it. \
                          Each edit replaces `old` (exact text copied from the file, matching \
                          exactly one place, a few whole lines) with `new`. To create a new \
                          file, use a single edit with an empty `old`.",
            schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "summary": {"type": "string", "description": "One sentence explaining the change"},
                    "edits": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old": {"type": "string"},
                                "new": {"type": "string"}
                            },
                            "required": ["old", "new"]
                        }
                    }
                },
                "required": ["path", "edits"]
            }),
        },
    ]
}

/// Runs one tool call (blocking file-system access).
pub fn run(name: &str, arguments: &Value, ctx: &ToolContext) -> ToolOutcome {
    let result = match name {
        "list_files" => list_files(ctx),
        "read_file" => parse(arguments).and_then(|args| read_file(ctx, args)),
        "search" => parse(arguments).and_then(|args| search(ctx, args)),
        "outline" => outline(ctx),
        "compile_log" => Ok(compile_log(ctx)),
        PROPOSE_EDIT => {
            return match parse(arguments).and_then(|args| propose_edit(ctx, args)) {
                Ok(patch) => ToolOutcome::Proposed(patch),
                Err(message) => ToolOutcome::Err(message),
            };
        }
        other => Err(format!("unknown tool `{other}`")),
    };
    match result {
        Ok(output) => ToolOutcome::Ok(output),
        Err(message) => ToolOutcome::Err(message),
    }
}

fn parse<T: for<'de> Deserialize<'de>>(arguments: &Value) -> Result<T, String> {
    // Some models send the arguments as a JSON string.
    let value = match arguments {
        Value::String(text) => serde_json::from_str(text).map_err(|err| err.to_string())?,
        other => other.clone(),
    };
    serde_json::from_value(value).map_err(|err| format!("invalid arguments: {err}"))
}

fn project_path(ctx: &ToolContext, path: &str) -> Result<(RelPath, std::path::PathBuf), String> {
    let rel = RelPath::parse(path.trim().trim_start_matches("./"))
        .map_err(|err| format!("invalid path `{path}`: {err}"))?;
    let full = resolve(&ctx.project_root, &rel).map_err(|err| err.to_string())?;
    Ok((rel, full))
}

fn read_text(path: &Path, rel: &RelPath) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|_| format!("`{rel}` does not exist"))?;
    if bytes.contains(&0) {
        return Err(format!("`{rel}` is a binary file"));
    }
    String::from_utf8(bytes).map_err(|_| format!("`{rel}` is not UTF-8 text"))
}

fn list_files(ctx: &ToolContext) -> Result<String, String> {
    let entries = files::list_tree(&ctx.project_root).map_err(|err| err.to_string())?;
    let total = entries.len();
    let mut out = String::new();
    for entry in entries.into_iter().take(MAX_LIST_ENTRIES) {
        match entry.kind {
            EntryKind::Dir => out.push_str(&format!("{}/\n", entry.path)),
            EntryKind::File => out.push_str(&format!("{} ({} bytes)\n", entry.path, entry.size)),
        }
    }
    if total > MAX_LIST_ENTRIES {
        out.push_str(&format!("… and {} more\n", total - MAX_LIST_ENTRIES));
    }
    if out.is_empty() {
        out.push_str("(the project is empty)\n");
    }
    Ok(out)
}

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
}

fn read_file(ctx: &ToolContext, args: ReadArgs) -> Result<String, String> {
    let (rel, full) = project_path(ctx, &args.path)?;
    let text = read_text(&full, &rel)?;
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let total = lines.len();
    let start = args.start_line.unwrap_or(1).max(1);
    if start > total.max(1) {
        return Err(format!("`{rel}` has only {total} lines"));
    }
    let wanted_end = args.end_line.unwrap_or(total).clamp(start, total.max(1));
    let mut out = String::new();
    let mut end = start - 1;
    for line in &lines[start - 1..wanted_end] {
        if out.len() + line.len() > MAX_READ_BYTES && end >= start {
            break;
        }
        out.push_str(line);
        end += 1;
    }
    if start == 1 && end == total {
        Ok(out)
    } else {
        Ok(format!(
            "[{rel}: lines {start}-{end} of {total}; read more with start_line={}]\n{out}",
            end + 1
        ))
    }
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
}

const SEARCHABLE: [&str; 9] = ["tex", "bib", "sty", "cls", "txt", "md", "bbx", "cbx", "ltx"];

fn text_files(ctx: &ToolContext) -> Result<Vec<String>, String> {
    let entries = files::list_tree(&ctx.project_root).map_err(|err| err.to_string())?;
    Ok(entries
        .into_iter()
        .filter(|entry| entry.kind == EntryKind::File)
        .filter(|entry| {
            entry
                .path
                .rsplit_once('.')
                .is_some_and(|(_, ext)| SEARCHABLE.contains(&ext.to_ascii_lowercase().as_str()))
        })
        .map(|entry| entry.path)
        .collect())
}

fn search(ctx: &ToolContext, args: SearchArgs) -> Result<String, String> {
    let needle = args.query.trim().to_lowercase();
    if needle.is_empty() {
        return Err("the query is empty".into());
    }
    let mut out = String::new();
    let mut hits = 0;
    'files: for path in text_files(ctx)? {
        let Ok((rel, full)) = project_path(ctx, &path) else {
            continue;
        };
        let Ok(text) = read_text(&full, &rel) else {
            continue;
        };
        for (number, line) in text.lines().enumerate() {
            if line.to_lowercase().contains(&needle) {
                out.push_str(&format!("{rel}:{}: {}\n", number + 1, line.trim()));
                hits += 1;
                if hits >= MAX_SEARCH_HITS {
                    out.push_str("… more matches not shown\n");
                    break 'files;
                }
            }
        }
    }
    if out.is_empty() {
        out = format!("no match for `{}`", args.query);
    }
    Ok(out)
}

const STRUCTURE: [&str; 13] = [
    "\\part",
    "\\chapter",
    "\\section",
    "\\subsection",
    "\\subsubsection",
    "\\paragraph",
    "\\label",
    "\\input",
    "\\include",
    "\\bibliography",
    "\\addbibresource",
    "\\begin{abstract}",
    "\\documentclass",
];

/// Lines of the project's `.tex` files that define its structure.
pub fn outline_text(ctx: &ToolContext) -> Result<String, String> {
    let mut out = String::new();
    let mut lines_out = 0;
    let mut paths: Vec<String> = text_files(ctx)?
        .into_iter()
        .filter(|path| path.ends_with(".tex"))
        .collect();
    // The main file first, then the others.
    paths.sort_by_key(|path| (path != &ctx.main_file, path.clone()));
    for path in paths {
        let Ok((rel, full)) = project_path(ctx, &path) else {
            continue;
        };
        let Ok(text) = read_text(&full, &rel) else {
            continue;
        };
        for (number, line) in text.lines().enumerate() {
            let code = line.split('%').next().unwrap_or("").trim();
            if STRUCTURE.iter().any(|marker| code.contains(marker)) {
                out.push_str(&format!("{rel}:{}: {code}\n", number + 1));
                lines_out += 1;
                if lines_out >= MAX_OUTLINE_LINES {
                    out.push_str("… outline truncated\n");
                    return Ok(out);
                }
            }
        }
    }
    if out.is_empty() {
        out.push_str("(no sections found)\n");
    }
    Ok(out)
}

fn outline(ctx: &ToolContext) -> Result<String, String> {
    outline_text(ctx)
}

/// Errors first, at most `limit` lines.
pub fn diagnostics_text(diagnostics: &[Diagnostic], limit: usize) -> String {
    let mut sorted: Vec<&Diagnostic> = diagnostics.iter().collect();
    sorted.sort_by_key(|d| match d.severity {
        Severity::Error => 0,
        Severity::Warning => 1,
        Severity::Info => 2,
    });
    let mut out = String::new();
    for diagnostic in sorted.iter().take(limit) {
        let place = match (&diagnostic.file, diagnostic.line) {
            (Some(file), Some(line)) => format!("{file}:{line}: "),
            (Some(file), None) => format!("{file}: "),
            _ => String::new(),
        };
        let severity = match diagnostic.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "note",
        };
        out.push_str(&format!("{place}{severity}: {}", diagnostic.message));
        if let Some(first) = diagnostic.details.lines().next() {
            out.push_str(&format!(" ({})", first.trim()));
        }
        out.push('\n');
    }
    if sorted.len() > limit {
        out.push_str(&format!("… {} more\n", sorted.len() - limit));
    }
    out
}

fn compile_log(ctx: &ToolContext) -> String {
    match &ctx.compile {
        None => "The project has not been compiled yet.".into(),
        Some(summary) if summary.diagnostics.is_empty() => {
            format!(
                "Last compilation: {}, no errors or warnings.",
                summary.status
            )
        }
        Some(summary) => format!(
            "Last compilation: {}.\n{}",
            summary.status,
            diagnostics_text(&summary.diagnostics, 60)
        ),
    }
}

#[derive(Deserialize)]
struct ProposeArgs {
    path: String,
    #[serde(default)]
    summary: String,
    edits: Vec<Hunk>,
}

fn propose_edit(ctx: &ToolContext, args: ProposeArgs) -> Result<Patch, String> {
    let (rel, full) = project_path(ctx, &args.path)?;
    let exists = full.exists();
    let creates_file = !exists;
    if creates_file {
        let [only] = args.edits.as_slice() else {
            return Err(format!(
                "`{rel}` does not exist. To create it, send one edit with an empty `old` and \
                 the whole content as `new`"
            ));
        };
        if !only.old.is_empty() {
            return Err(format!(
                "`{rel}` does not exist, so there is no `old` text to replace. Check the path \
                 with list_files, or create the file with an empty `old`"
            ));
        }
    } else {
        let text = read_text(&full, &rel)?;
        locate(&text, &args.edits, rel.as_str()).map_err(|err| err.to_string())?;
    }
    Ok(Patch {
        id: uuid::Uuid::now_v7().to_string(),
        file: rel.as_str().to_owned(),
        summary: args.summary.trim().to_owned(),
        creates_file,
        hunks: args.edits,
        status: PatchStatus::Pending,
    })
}

#[cfg(test)]
mod tests {
    use openprism_core::latexlog::DiagnosticSource;

    use super::*;

    fn project() -> (tempfile::TempDir, ToolContext) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("chapters")).unwrap();
        std::fs::write(
            root.join("main.tex"),
            "\\documentclass{article}\n\\begin{document}\n\\section{Intro}\\label{sec:intro}\n\\input{chapters/one}\n\\end{document}\n",
        )
        .unwrap();
        std::fs::write(root.join("chapters/one.tex"), "\\section{One}\nTeh text.\n").unwrap();
        std::fs::write(root.join("fig.png"), [0u8, 1, 2]).unwrap();
        let ctx = ToolContext {
            project_root: root,
            main_file: "main.tex".into(),
            compile: None,
        };
        (dir, ctx)
    }

    fn ok(outcome: ToolOutcome) -> String {
        match outcome {
            ToolOutcome::Ok(text) => text,
            other => panic!("expected output, got {other:?}"),
        }
    }

    fn err(outcome: ToolOutcome) -> String {
        match outcome {
            ToolOutcome::Err(text) => text,
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[test]
    fn reads_lists_searches_and_outlines() {
        let (_dir, ctx) = project();
        assert!(ok(run("list_files", &json!({}), &ctx)).contains("chapters/one.tex (24 bytes)"));
        assert_eq!(
            ok(run("read_file", &json!({"path": "chapters/one.tex"}), &ctx)),
            "\\section{One}\nTeh text.\n"
        );
        assert!(
            ok(run(
                "read_file",
                &json!({"path": "main.tex", "start_line": 3, "end_line": 3}),
                &ctx
            ))
            .starts_with("[main.tex: lines 3-3 of 5")
        );
        assert!(err(run("read_file", &json!({"path": "fig.png"}), &ctx)).contains("binary"));
        assert!(
            err(run("read_file", &json!({"path": "../etc/passwd"}), &ctx)).contains("invalid path")
        );
        assert_eq!(
            ok(run("search", &json!({"query": "teh"}), &ctx)),
            "chapters/one.tex:2: Teh text.\n"
        );
        let outline = ok(run("outline", &json!({}), &ctx));
        assert!(outline.starts_with("main.tex:1: \\documentclass{article}"));
        assert!(outline.contains("chapters/one.tex:1: \\section{One}"));
        // Arguments sent as a JSON string are accepted too.
        assert!(
            ok(run("read_file", &json!("{\"path\": \"main.tex\"}"), &ctx))
                .contains("documentclass")
        );
    }

    #[test]
    fn propose_edit_validates_without_writing() {
        let (_dir, ctx) = project();
        let outcome = run(
            PROPOSE_EDIT,
            &json!({"path": "chapters/one.tex", "summary": "Fix typo", "edits": [{"old": "Teh text.", "new": "The text."}]}),
            &ctx,
        );
        let ToolOutcome::Proposed(patch) = outcome else {
            panic!("expected a patch, got {outcome:?}");
        };
        assert_eq!(patch.file, "chapters/one.tex");
        assert!(!patch.creates_file);
        assert_eq!(
            std::fs::read_to_string(ctx.project_root.join("chapters/one.tex")).unwrap(),
            "\\section{One}\nTeh text.\n",
            "proposing must not write"
        );

        let missing = err(run(
            PROPOSE_EDIT,
            &json!({"path": "main.tex", "edits": [{"old": "nope", "new": "x"}]}),
            &ctx,
        ));
        assert!(missing.contains("not found"));

        let ToolOutcome::Proposed(new_file) = run(
            PROPOSE_EDIT,
            &json!({"path": "chapters/two.tex", "edits": [{"old": "", "new": "Two"}]}),
            &ctx,
        ) else {
            panic!("expected a file creation");
        };
        assert!(new_file.creates_file);
        assert!(!ctx.project_root.join("chapters/two.tex").exists());
    }

    #[test]
    fn compile_log_lists_errors_first() {
        let (_dir, mut ctx) = project();
        assert!(compile_log(&ctx).contains("not been compiled"));
        let diagnostic = |severity, message: &str| Diagnostic {
            severity,
            source: DiagnosticSource::Latex,
            file: Some("main.tex".into()),
            line: Some(3),
            message: message.into(),
            details: "l.3 \\foo".into(),
        };
        ctx.compile = Some(CompileSummary {
            status: "failed".into(),
            diagnostics: vec![
                diagnostic(Severity::Warning, "Reference undefined"),
                diagnostic(Severity::Error, "Undefined control sequence."),
            ],
        });
        let log = compile_log(&ctx);
        let error_at = log.find("error: Undefined").unwrap();
        let warning_at = log.find("warning: Reference").unwrap();
        assert!(error_at < warning_at);
        assert!(log.contains("main.tex:3: error: Undefined control sequence. (l.3 \\foo)"));
    }
}
