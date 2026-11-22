//! Turning an image (a formula, a table, printed or handwritten text) into LaTeX that the
//! writer inserts in the editor after looking at it.
//!
//! The image is used only for the request and is never stored. The answer is streamed as it is
//! written, and the final one is cleaned: no code fences, no `\usepackage` lines (packages the
//! main file does not load yet are only named, never added to the document) and no citation
//! commands, which become `[?]`: citations are the writer's job (D11).

use std::convert::Infallible;

use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::{Stream, StreamExt};
use openprism_ai::llm::{EventStream, LlmEvent, LlmRequest, Message, Usage};
use openprism_core::files;
use openprism_core::paths::RelPath;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::completion::{self, CITE_COMMANDS};
use crate::error::ApiError;
use crate::images::{self, ImageUpload};
use crate::projects::{self, Project};
use crate::settings::{self, ModelRef};
use crate::state::AppState;

/// Most characters of text before the insertion point.
pub const MAX_BEFORE_CHARS: usize = 1_500;
/// Most characters of text after the insertion point.
pub const MAX_AFTER_CHARS: usize = 500;
const MAX_TOKENS: u32 = 4_096;
const INSERT: &str = "<INSERT>";
/// What replaces a citation command.
const NO_CITATION: &str = "[?]";

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImageLatexRequest {
    pub project_id: String,
    /// The file the LaTeX goes into, relative to the project root.
    pub path: String,
    /// The image; its `page` and `source` are ignored.
    pub image: ImageUpload,
    /// Text just before the insertion point, at most 1,500 characters.
    #[serde(default)]
    pub before: String,
    /// Text just after the insertion point, at most 500 characters.
    #[serde(default)]
    pub after: String,
}

/// What the editor receives while the LaTeX is written (server-sent events).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export)]
pub enum ImageLatexEvent {
    /// The answer so far, without code fences, `\usepackage` lines and citations.
    Text { text: String },
    /// The cleaned result: the last event of a stream that ends normally.
    Done {
        /// The LaTeX to insert; empty when the model wrote nothing usable.
        latex: String,
        /// Packages the LaTeX needs that the main file does not load.
        packages: Vec<String>,
        /// Whether citation commands were replaced with `[?]`.
        #[serde(rename = "citationsRemoved")]
        citations_removed: bool,
    },
    /// The provider failed while writing.
    Error { message: String },
}

/// A finished answer, cleaned (see [`clean_image_latex`]).
#[derive(Debug, PartialEq, Eq)]
pub struct ImageLatex {
    pub latex: String,
    pub packages: Vec<String>,
    pub citations_removed: bool,
}

const SYSTEM: &str = "You transcribe the content of an image into LaTeX that will be inserted \
into the author's document at the position marked <INSERT>. Reply with only the LaTeX to \
insert: no explanation, no code fences.\n\
- A formula becomes an equation environment (one line) or an align environment (several \
lines). If <INSERT> is inside a math environment or math delimiters, write only the math, \
without any environment or delimiter.\n\
- A table becomes a tabular environment.\n\
- Text, printed or handwritten, becomes paragraphs in the language of the text in the image, \
with formulas in it as inline math.\n\
- Use only the packages already loaded. If the LaTeX needs another package, start your reply \
with one \\usepackage{...} line per package.\n\
- Never write \\cite or any other citation command: keep reference marks such as [12] as \
plain text.\n\
Transcribe exactly what the image shows: do not add, complete or correct its content.";

/// Asks the fast model (the agent model if there is none) to transcribe the image and streams
/// its answer.
///
/// Errors found before the model answers (bad input, no model, a provider that refuses the
/// request, for example because the model does not take images) are HTTP errors; a failure
/// while it writes is an `error` event.
pub async fn image_to_latex(
    state: &AppState,
    user_id: Option<&str>,
    request: ImageLatexRequest,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>> + use<>>, ApiError> {
    let project = projects::get(state, &request.project_id).await?;
    let path = RelPath::parse(&request.path)?;
    if request.before.chars().count() > MAX_BEFORE_CHARS {
        return Err(ApiError::bad_request(format!(
            "the text before the insertion point is longer than {MAX_BEFORE_CHARS} characters"
        )));
    }
    if request.after.chars().count() > MAX_AFTER_CHARS {
        return Err(ApiError::bad_request(format!(
            "the text after the insertion point is longer than {MAX_AFTER_CHARS} characters"
        )));
    }
    let image = images::check(vec![request.image])?
        .pop()
        .map(images::unstored)
        .ok_or_else(|| ApiError::bad_request("there is no image"))?;
    let roles = settings::roles_for(state, user_id).await?;
    let model_ref = roles.fast.or(roles.agent).ok_or_else(|| {
        ApiError::bad_request("no model is set up: add a provider and choose a model in Settings")
    })?;
    let model = settings::model(state, &model_ref).await?;
    let loaded = main_file_packages(state, &project).await;
    let packages = if loaded.is_empty() {
        "none".to_owned()
    } else {
        loaded.join(", ")
    };
    let llm_request = LlmRequest {
        system: SYSTEM.to_owned(),
        messages: vec![Message::User {
            text: format!(
                "File: {path}\nPackages loaded: {packages}\n<document>\n{}{INSERT}{}\n</document>\nTranscribe the image into LaTeX to insert at {INSERT}.",
                request.before, request.after
            ),
            images: vec![image],
        }],
        tools: Vec::new(),
        max_tokens: Some(MAX_TOKENS),
        reasoning: model_ref.reasoning,
        session_id: Some(Uuid::now_v7().to_string()),
    };

    let mut stream = model
        .stream(llm_request)
        .await
        .map_err(completion::provider_error)?;
    // Some providers report a refused request (a model without image input, a bad key) as the
    // first event.
    let first = match stream.next().await {
        Some(Err(err)) => return Err(completion::provider_error(err)),
        first => first,
    };
    let run = Run {
        stream: Box::pin(futures_util::stream::iter(first).chain(stream)),
        raw: String::new(),
        shown: String::new(),
        usage: Usage::default(),
        loaded,
        state: state.clone(),
        model_ref,
    };
    // Closing the connection drops this stream, and with it the stream from the provider.
    let events = futures_util::stream::unfold(Some(run), |run| async move {
        let event = run?.next_event().await;
        Some(event)
    })
    .map(|event| {
        Ok(Event::default()
            .json_data(&event)
            .unwrap_or_else(|_| Event::default().comment("unserialisable event")))
    });
    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
}

/// The packages the project's main file loads; none when it cannot be read.
async fn main_file_packages(state: &AppState, project: &Project) -> Vec<String> {
    let Ok(main_file) = RelPath::parse(&project.main_file) else {
        return Vec::new();
    };
    let root = state.paths.project_dir(&project.id);
    match tokio::task::spawn_blocking(move || files::read(&root, &main_file)).await {
        Ok(Ok((bytes, _))) => loaded_packages(&String::from_utf8_lossy(&bytes)),
        _ => Vec::new(),
    }
}

/// An answer being written.
struct Run {
    stream: EventStream,
    /// The answer so far.
    raw: String,
    /// The text of the last `text` event.
    shown: String,
    usage: Usage,
    /// The packages the main file loads.
    loaded: Vec<String>,
    state: AppState,
    model_ref: ModelRef,
}

impl Run {
    /// The next event to send, and the run to continue with (`None` after the last event).
    async fn next_event(mut self) -> (ImageLatexEvent, Option<Self>) {
        loop {
            match self.stream.next().await {
                Some(Ok(LlmEvent::Text(delta))) => {
                    self.raw.push_str(&delta);
                    let visible = visible_image_latex(&self.raw);
                    if visible != self.shown {
                        self.shown.clone_from(&visible);
                        return (ImageLatexEvent::Text { text: visible }, Some(self));
                    }
                }
                Some(Ok(LlmEvent::Usage(more))) => self.usage.add(more),
                Some(Ok(_)) => {}
                Some(Err(err)) => {
                    completion::record_usage(&self.state, &self.model_ref, self.usage).await;
                    let message = err.to_string();
                    return (ImageLatexEvent::Error { message }, None);
                }
                None => {
                    let result = clean_image_latex(&self.raw, &self.loaded);
                    completion::record_usage(&self.state, &self.model_ref, self.usage).await;
                    let event = ImageLatexEvent::Done {
                        latex: result.latex,
                        packages: result.packages,
                        citations_removed: result.citations_removed,
                    };
                    return (event, None);
                }
            }
        }
    }
}

/// Cleans a finished answer: keeps only what is inside code fences when there are some, takes
/// out the `\usepackage` lines (naming those of their packages that are not in `loaded`, in
/// order and once each), replaces each citation command with `[?]` and removes blank lines at
/// the start and the end.
pub fn clean_image_latex(raw: &str, loaded: &[String]) -> ImageLatex {
    let mut packages: Vec<String> = Vec::new();
    let mut lines = Vec::new();
    for line in unfenced_lines(raw) {
        match package_line(line) {
            Some(names) => {
                for name in names {
                    if !loaded.contains(&name) && !packages.contains(&name) {
                        packages.push(name);
                    }
                }
            }
            None => lines.push(line),
        }
    }
    let (latex, citations_removed) = replace_citations(&lines.join("\n"), false);
    ImageLatex {
        latex: without_blank_ends(&latex).to_owned(),
        packages,
        citations_removed,
    }
}

/// The part of an unfinished answer to show while the model is still writing: cleaned like
/// [`clean_image_latex`], but without a last line that may still become a code fence or a
/// `\usepackage` line, and without a citation command still being written.
pub fn visible_image_latex(raw: &str) -> String {
    let last_start = raw.rfind('\n').map_or(0, |index| index + 1);
    let last = raw[last_start..].trim_start();
    let raw = if !last.is_empty() && ("```".starts_with(last) || "\\usepackage".starts_with(last)) {
        &raw[..last_start]
    } else {
        raw
    };
    let lines: Vec<&str> = unfenced_lines(raw)
        .into_iter()
        .filter(|line| package_line(line).is_none())
        .collect();
    let (text, _) = replace_citations(&lines.join("\n"), true);
    without_blank_ends(&text).to_owned()
}

/// The packages a LaTeX source loads before `\begin{document}` with `\usepackage` or
/// `\RequirePackage`, in order and once each. Comments are ignored. Packages loaded by a file
/// the preamble includes are not seen.
pub fn loaded_packages(source: &str) -> Vec<String> {
    let mut preamble = String::new();
    for line in source.lines() {
        let line = without_comment(line);
        if let Some(index) = line.find("\\begin{document}") {
            preamble.push_str(&line[..index]);
            break;
        }
        preamble.push_str(line);
        preamble.push('\n');
    }
    let mut packages: Vec<String> = Vec::new();
    let mut index = 0;
    while let Some(offset) = preamble[index..].find('\\') {
        let start = index + offset;
        let rest = &preamble[start + 1..];
        index = start + 1;
        if rest.starts_with('\\') {
            // `\\`: a line break, not a command.
            index += 1;
            continue;
        }
        let Some(args) = ["usepackage", "RequirePackage"]
            .into_iter()
            .find_map(|command| rest.strip_prefix(command))
        else {
            continue;
        };
        if args.starts_with(|c: char| c.is_ascii_alphabetic()) {
            continue;
        }
        if let Some((names, _)) = package_names(args) {
            for name in names {
                if !packages.contains(&name) {
                    packages.push(name);
                }
            }
        }
    }
    packages
}

/// The lines of an answer without code fences. When it has some, only the lines inside them are
/// kept, and the `\usepackage` lines outside them: anything else outside is talk around the code.
fn unfenced_lines(raw: &str) -> Vec<&str> {
    let is_fence = |line: &str| line.trim_start().starts_with("```");
    if !raw.lines().any(is_fence) {
        return raw.lines().collect();
    }
    let mut inside = false;
    raw.lines()
        .filter(|line| {
            if is_fence(line) {
                inside = !inside;
                false
            } else {
                inside || package_line(line).is_some()
            }
        })
        .collect()
}

/// The package names of a `\usepackage` line; `None` for any other line.
fn package_line(line: &str) -> Option<Vec<String>> {
    let args = line.trim_start().strip_prefix("\\usepackage")?;
    if args.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    Some(package_names(args).map_or_else(Vec::new, |(names, _)| names))
}

/// The names in the `[options]{a, b}` arguments that start `args`, and where they end. `None`
/// when the names are missing or not closed.
fn package_names(args: &str) -> Option<(Vec<String>, usize)> {
    let mut index = skip_space(args, 0);
    if args[index..].starts_with('[') {
        index = skip_space(args, group_end(args, index)? + 1);
    }
    if !args[index..].starts_with('{') {
        return None;
    }
    let end = group_end(args, index)?;
    let names = args[index + 1..end]
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect();
    Some((names, end + 1))
}

/// A citation command found at a backslash.
enum Citation {
    /// Not a citation command.
    No,
    /// The text ends with what may still become a citation command.
    Maybe,
    /// A citation command and where it ends; not `finished` when the text ends before its
    /// arguments do.
    Command { end: usize, finished: bool },
}

/// The citation command at `text[start]`, a backslash: its name (any command starting with one
/// of [`CITE_COMMANDS`], in any letter case), an optional `*`, then its `[…]` and `{…}`
/// arguments.
fn citation_at(text: &str, start: usize) -> Citation {
    let name_start = start + 1;
    let name_end = text[name_start..]
        .find(|c: char| !c.is_ascii_alphabetic())
        .map_or(text.len(), |offset| name_start + offset);
    let name = text[name_start..name_end].to_ascii_lowercase();
    if !CITE_COMMANDS
        .iter()
        .any(|command| name.starts_with(command))
    {
        let may_grow = name_end == text.len()
            && CITE_COMMANDS
                .iter()
                .any(|command| command.starts_with(&name));
        return if may_grow {
            Citation::Maybe
        } else {
            Citation::No
        };
    }
    let mut end = name_end;
    if text[end..].starts_with('*') {
        end += 1;
    }
    let args = skip_space(text, end);
    let mut index = args;
    let mut has_key = false;
    while let Some(open) = text[index..]
        .chars()
        .next()
        .filter(|c| matches!(c, '[' | '{'))
    {
        match group_end(text, index) {
            Some(close) => {
                has_key |= open == '{';
                index = close + 1;
            }
            None => {
                return Citation::Command {
                    end: text.len(),
                    finished: false,
                };
            }
        }
    }
    if index == args {
        // No arguments yet: they may follow.
        return Citation::Command {
            end,
            finished: args < text.len(),
        };
    }
    Citation::Command {
        end: index,
        finished: has_key || index < text.len(),
    }
}

/// Replaces each citation command with `[?]`; also says whether there was any. While
/// `streaming`, the text is cut before a citation command that is still being written.
fn replace_citations(text: &str, streaming: bool) -> (String, bool) {
    let mut out = String::with_capacity(text.len());
    let mut removed = false;
    let mut copied = 0;
    let mut index = 0;
    while let Some(offset) = text[index..].find('\\') {
        let start = index + offset;
        if text[start + 1..].starts_with('\\') {
            index = start + 2;
            continue;
        }
        match citation_at(text, start) {
            Citation::No => index = start + 1,
            Citation::Maybe if !streaming => index = start + 1,
            Citation::Command { end, finished } if finished || !streaming => {
                out.push_str(&text[copied..start]);
                out.push_str(NO_CITATION);
                removed = true;
                copied = end;
                index = end;
            }
            Citation::Maybe | Citation::Command { .. } => {
                out.push_str(&text[copied..start]);
                return (out, removed);
            }
        }
    }
    out.push_str(&text[copied..]);
    (out, removed)
}

/// The index of the `]` or `}` that closes the `[` or `{` at `open`, past nested braces and
/// escaped characters.
fn group_end(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let close = match bytes.get(open)? {
        b'[' => b']',
        b'{' => b'}',
        _ => return None,
    };
    let mut depth = 0_usize;
    let mut index = open + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 1,
            byte if byte == close && depth == 0 => return Some(index),
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        index += 1;
    }
    None
}

/// `index` moved past the whitespace that follows it.
fn skip_space(text: &str, index: usize) -> usize {
    let rest = &text[index..];
    index + rest.len() - rest.trim_start().len()
}

/// A line without its comment: from the first `%` that is not escaped.
fn without_comment(line: &str) -> &str {
    for (index, _) in line.match_indices('%') {
        let backslashes = line[..index]
            .bytes()
            .rev()
            .take_while(|&byte| byte == b'\\')
            .count();
        if backslashes % 2 == 0 {
            return &line[..index];
        }
    }
    line
}

/// `text` without blank lines at its start and whitespace at its end.
fn without_blank_ends(text: &str) -> &str {
    let text = text.trim_end();
    match text.find(|c: char| !c.is_whitespace()) {
        Some(first) => {
            let line_start = text[..first].rfind('\n').map_or(0, |index| index + 1);
            &text[line_start..]
        }
        None => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn code_fences_and_the_talk_around_them_are_removed() {
        let result = clean_image_latex(
            "```latex\n\\begin{equation}\n  E = mc^2\n\\end{equation}\n```",
            &[],
        );
        assert_eq!(
            result.latex,
            "\\begin{equation}\n  E = mc^2\n\\end{equation}"
        );
        let result = clean_image_latex(
            "Here is the LaTeX:\n\n```\n\\[ x^2 \\]\n```\nHope it helps.",
            &[],
        );
        assert_eq!(result.latex, "\\[ x^2 \\]");
        // Without fences, everything is kept; blank lines at the ends go, indentation stays.
        let result = clean_image_latex("\n\n  a & b \\\\\n  c & d\n\n", &[]);
        assert_eq!(result.latex, "  a & b \\\\\n  c & d");
        assert!(!result.citations_removed);
        assert!(result.packages.is_empty());
    }

    #[test]
    fn usepackage_lines_are_taken_out_and_only_missing_packages_named() {
        // The example of the spec: a table that needs booktabs.
        let raw = "\\usepackage{booktabs}\n\\begin{tabular}{ll}\n\\toprule\na & b \\\\\n\\bottomrule\n\\end{tabular}";
        let result = clean_image_latex(raw, &loaded(&["amsmath"]));
        assert_eq!(
            result.latex,
            "\\begin{tabular}{ll}\n\\toprule\na & b \\\\\n\\bottomrule\n\\end{tabular}"
        );
        assert_eq!(result.packages, ["booktabs"]);
        // Already loaded: nothing to name, the line still goes.
        let result = clean_image_latex(raw, &loaded(&["booktabs"]));
        assert!(result.packages.is_empty());
        assert!(!result.latex.contains("usepackage"));
        // Options, several names, repeats, and lines outside a fence.
        let raw = "\\usepackage[table]{xcolor}\n\\usepackage{amsmath, booktabs}\n```latex\n\\usepackage{booktabs}\nx\n```";
        let result = clean_image_latex(raw, &loaded(&["amsmath"]));
        assert_eq!(result.packages, ["xcolor", "booktabs"]);
        assert_eq!(result.latex, "x");
    }

    #[test]
    fn every_citation_command_becomes_a_question_mark() {
        // The example of the spec.
        let result = clean_image_latex("như đã chỉ ra trong \\cite{smith2020}.", &[]);
        assert_eq!(result.latex, "như đã chỉ ra trong [?].");
        assert!(result.citations_removed);
        for command in [
            "\\cite{a}",
            "\\Cite{a}",
            "\\cite*{a}",
            "\\citep{a}",
            "\\citep[p.~5]{a}",
            "\\citet*{a,b}",
            "\\citeauthor{a}",
            "\\parencite[see][12]{a}",
            "\\textcite{a}",
            "\\autocite{a}",
            "\\footcite{a}",
            "\\nocite{*}",
            "\\cite {a}",
            "\\cites{a}{b}",
        ] {
            let result = clean_image_latex(&format!("as in {command}, shown"), &[]);
            assert_eq!(result.latex, "as in [?], shown", "{command}");
            assert!(result.citations_removed, "{command}");
        }
        // Other commands and line breaks stay.
        let result = clean_image_latex("see Section~\\ref{sec:intro}\\\\ cited", &[]);
        assert_eq!(result.latex, "see Section~\\ref{sec:intro}\\\\ cited");
        assert!(!result.citations_removed);
        // A citation cut off by the end of the answer is still removed.
        assert_eq!(
            clean_image_latex("as in \\cite{smi", &[]).latex,
            "as in [?]"
        );
    }

    #[test]
    fn packages_are_read_from_the_preamble() {
        let source = "\\documentclass{article}\n\
\\usepackage[utf8]{inputenc} % encoding\n\
\\usepackage{amsmath,\n  amssymb}\n\
% \\usepackage{booktabs}\n\
\\usepackage[\n  margin=1in, % wide\n]{geometry}\n\
\\RequirePackage{xcolor}\n\
\\usepackage{graphicx} \\usepackage{amsmath}\n\
50\\% \\usepackage{hyperref}\n\
\\usepackagex{nope}\n\
\\begin{document}\n\
\\usepackage{tikz}\n\
\\end{document}\n";
        assert_eq!(
            loaded_packages(source),
            [
                "inputenc", "amsmath", "amssymb", "geometry", "xcolor", "graphicx", "hyperref"
            ]
        );
        // A commented-out \begin{document} does not end the preamble.
        assert_eq!(
            loaded_packages("% \\begin{document}\n\\usepackage{booktabs}\n\\begin{document}"),
            ["booktabs"]
        );
        assert!(loaded_packages("no preamble at all").is_empty());
    }

    #[test]
    fn what_is_shown_while_streaming_is_clean() {
        for (raw, shown) in [
            ("`", ""),
            ("``", ""),
            ("```latex", ""),
            ("```latex\n\\begin{equ", "\\begin{equ"),
            ("\\usepack", ""),
            ("\\usepackage{booktabs}", ""),
            (
                "\\usepackage{booktabs}\n\\begin{tabular}",
                "\\begin{tabular}",
            ),
            ("\\begin{tabular}\n``", "\\begin{tabular}"),
            ("x\n```", "x"),
            ("see \\", "see"),
            ("see \\cit", "see"),
            ("see \\cite", "see"),
            ("see \\cite[p.~5]", "see"),
            ("see \\cite{smi", "see"),
            ("see \\cite{smith} and", "see [?] and"),
            ("see \\ref{a} and \\c", "see \\ref{a} and"),
            ("see \\cdot", "see \\cdot"),
        ] {
            assert_eq!(visible_image_latex(raw), shown, "{raw}");
        }
    }
}
