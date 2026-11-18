//! Suggestions shown as grey "ghost text" at the editor cursor: the model continues the text
//! around the cursor. The writer accepts them with Tab; nothing is written before that.
//!
//! Only the text around the cursor is sent, no tools are offered, and suggestions never contain
//! citation commands: citations are the writer's job (D11).

use futures_util::StreamExt;
use openprism_ai::llm::{LlmEvent, LlmRequest, Message, Usage};
use openprism_core::paths::RelPath;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::chat;
use crate::error::ApiError;
use crate::projects;
use crate::settings;
use crate::state::AppState;

/// Most characters of text before the cursor.
pub const MAX_PREFIX_CHARS: usize = 3_000;
/// Most characters of text after the cursor.
pub const MAX_SUFFIX_CHARS: usize = 1_000;
/// Most lines in a suggestion.
const MAX_LINES: usize = 6;
const MAX_TOKENS: u32 = 128;
/// A repeat of the text before the cursor is removed from this many characters on.
const MIN_ECHO_CHARS: usize = 8;
/// A repeat of the text after the cursor is removed from this many characters on.
const MIN_OVERLAP_CHARS: usize = 3;
const CURSOR: &str = "<CURSOR>";

/// Commands that cite; matched after a backslash, in any letter case. Variants such as
/// `\citep`, `\citet`, `\cite*` or `\citeauthor` start with one of these.
const CITE_COMMANDS: [&str; 6] = [
    "cite",
    "nocite",
    "footcite",
    "autocite",
    "textcite",
    "parencite",
];

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CompletionRequest {
    pub project_id: String,
    /// The file being edited, relative to the project root.
    pub path: String,
    /// Text just before the cursor, at most 3,000 characters.
    #[serde(default)]
    pub prefix: String,
    /// Text just after the cursor, at most 1,000 characters.
    #[serde(default)]
    pub suffix: String,
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct CompletionResponse {
    /// Text to insert at the cursor; empty when there is nothing useful to suggest.
    pub text: String,
}

const SYSTEM: &str = "You help an author write a LaTeX document by continuing the text at the \
position marked <CURSOR>. Reply with only the text to insert there: no explanation, no quotes, \
no code fences, and do not repeat text that is already before or after <CURSOR>. Begin with \
a space or a line break whenever the inserted text needs one to follow the text before \
<CURSOR> (for example after a complete word). Usually finish the current sentence or line; \
never write more than 6 lines. Write in the language of \
the surrounding text. When the context calls for it, complete LaTeX syntax: close environments, \
add the next \\item, close braces. Never add citations or \\cite commands: the author adds \
references themselves. If nothing useful can be added, reply with nothing.";

/// Asks the fast model (the agent model if there is none) for a suggestion.
pub async fn complete(
    state: &AppState,
    request: CompletionRequest,
) -> Result<CompletionResponse, ApiError> {
    projects::get(state, &request.project_id).await?;
    let path = RelPath::parse(&request.path)?;
    if request.prefix.chars().count() > MAX_PREFIX_CHARS {
        return Err(ApiError::bad_request(format!(
            "the text before the cursor is longer than {MAX_PREFIX_CHARS} characters"
        )));
    }
    if request.suffix.chars().count() > MAX_SUFFIX_CHARS {
        return Err(ApiError::bad_request(format!(
            "the text after the cursor is longer than {MAX_SUFFIX_CHARS} characters"
        )));
    }
    if request.prefix.trim().is_empty() && request.suffix.trim().is_empty() {
        return Err(ApiError::bad_request("there is no text around the cursor"));
    }
    let roles = settings::roles(state).await?;
    let model_ref = roles.fast.or(roles.agent).ok_or_else(|| {
        ApiError::bad_request("no model is set up: add a provider and choose a model in Settings")
    })?;
    let model = settings::model(state, &model_ref).await?;
    let llm_request = LlmRequest {
        system: SYSTEM.to_owned(),
        messages: vec![Message::User {
            text: format!(
                "File: {path}\n<document>\n{}{CURSOR}{}\n</document>\nWrite the text to insert at {CURSOR}.",
                request.prefix, request.suffix
            ),
        }],
        tools: Vec::new(),
        max_tokens: Some(MAX_TOKENS),
        reasoning: model_ref.reasoning,
        session_id: Some(Uuid::now_v7().to_string()),
    };

    let mut text = String::new();
    let mut usage = Usage::default();
    let mut stream = model.stream(llm_request).await.map_err(provider_error)?;
    while let Some(event) = stream.next().await {
        match event.map_err(provider_error)? {
            LlmEvent::Text(delta) => {
                text.push_str(&delta);
                // Enough lines: stop paying for more.
                if text.matches('\n').count() > MAX_LINES {
                    break;
                }
            }
            LlmEvent::Usage(more) => usage.add(more),
            _ => {}
        }
    }
    drop(stream);
    if let Err(err) = chat::record_usage(state, None, &model_ref, usage).await {
        tracing::warn!(?err, "cannot record usage");
    }
    Ok(CompletionResponse {
        text: clean_completion(&text, &request.prefix, &request.suffix),
    })
}

fn provider_error(err: openprism_ai::error::AiError) -> ApiError {
    ApiError::new(
        axum::http::StatusCode::BAD_GATEWAY,
        "ai_provider",
        err.to_string(),
    )
}

/// Makes a raw model answer usable as a suggestion: strips code fences, quotes and repeats of
/// the surrounding text, cuts it before the first citation command and keeps at most
/// [`MAX_LINES`] lines. Leading whitespace is kept: it is part of the continuation.
pub fn clean_completion(raw: &str, prefix: &str, suffix: &str) -> String {
    let text = raw.replace(CURSOR, "");
    let text = strip_wrapping(&text);
    let text = strip_prefix_echo(text, prefix);
    let text = cut_before_citation(text);
    let text = first_lines(text, MAX_LINES);
    let text = strip_suffix_overlap(text.trim_end(), suffix);
    let text = text.trim_end();
    if text.trim().is_empty() {
        String::new()
    } else {
        text.to_owned()
    }
}

/// Removes a code fence or quotes around the whole answer.
fn strip_wrapping(text: &str) -> &str {
    let trimmed = text.trim();
    if trimmed.starts_with("```") {
        let inner = trimmed.split_once('\n').map_or("", |(_, rest)| rest);
        let inner = inner.strip_suffix("```").unwrap_or(inner);
        return inner.strip_suffix('\n').unwrap_or(inner);
    }
    for (open, close) in [('"', '"'), ('\u{201c}', '\u{201d}')] {
        if trimmed.chars().count() >= 2 && trimmed.starts_with(open) && trimmed.ends_with(close) {
            return &trimmed[open.len_utf8()..trimmed.len() - close.len_utf8()];
        }
    }
    text
}

/// Removes the start of `text` when it repeats the end of `prefix`: a long repeat, or exactly
/// the current line so far. A short accidental match (a word) is kept.
fn strip_prefix_echo<'a>(text: &'a str, prefix: &str) -> &'a str {
    let longest = text
        .char_indices()
        .map(|(index, c)| index + c.len_utf8())
        .filter(|&end| prefix.ends_with(&text[..end]))
        .max();
    if let Some(end) = longest
        && text[..end].chars().count() >= MIN_ECHO_CHARS
    {
        return &text[end..];
    }
    let line = prefix.rsplit('\n').next().unwrap_or_default();
    if !line.trim().is_empty() && text.starts_with(line) {
        return &text[line.len()..];
    }
    text
}

/// Cuts `text` just before the first citation command.
fn cut_before_citation(text: &str) -> &str {
    for (index, _) in text.match_indices('\\') {
        let rest = text[index + 1..].to_ascii_lowercase();
        if CITE_COMMANDS
            .iter()
            .any(|command| rest.starts_with(command))
        {
            return &text[..index];
        }
    }
    text
}

fn first_lines(text: &str, count: usize) -> &str {
    match text.match_indices('\n').nth(count - 1) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}

/// Removes the end of `text` when it repeats the start of what follows the cursor.
fn strip_suffix_overlap<'a>(text: &'a str, suffix: &str) -> &'a str {
    let next = suffix.trim_start();
    let longest = next
        .char_indices()
        .map(|(index, c)| index + c.len_utf8())
        .filter(|&end| text.ends_with(&next[..end]))
        .max();
    match longest {
        Some(end) if next[..end].chars().count() >= MIN_OVERLAP_CHARS => &text[..text.len() - end],
        _ => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn citations_are_cut_off() {
        assert_eq!(
            clean_completion(
                " như đã chỉ ra trong \\cite{smith2020} và các nghiên cứu sau.",
                "Kết quả này",
                ""
            ),
            " như đã chỉ ra trong"
        );
        for command in [
            "\\cite{a}",
            "\\Cite{a}",
            "\\citep{a}",
            "\\citet{a}",
            "\\cite*{a}",
            "\\citeauthor{a}",
            "\\parencite{a}",
            "\\textcite{a}",
            "\\autocite{a}",
            "\\footcite{a}",
            "\\nocite{a}",
        ] {
            assert_eq!(
                clean_completion(&format!(" shown before {command} in"), "As", ""),
                " shown before",
                "{command}"
            );
        }
        // Other commands stay.
        assert_eq!(
            clean_completion(" see Section~\\ref{sec:intro}.", "As", ""),
            " see Section~\\ref{sec:intro}."
        );
        // Only a citation: nothing to suggest.
        assert_eq!(clean_completion("\\cite{x}", "Text", ""), "");
    }

    #[test]
    fn repeats_of_the_text_before_the_cursor_are_removed() {
        assert_eq!(
            clean_completion(
                "Chúng tôi đề xuất một phương pháp mới.",
                "\\section{Ý tưởng}\nChúng tôi đề xuất",
                ""
            ),
            " một phương pháp mới."
        );
        // The current line repeated, even when short.
        assert_eq!(
            clean_completion("We use the data.", "Intro.\nWe use", ""),
            " the data."
        );
        // A short accidental match is not a repeat.
        assert_eq!(clean_completion("the end.", "over the", ""), "the end.");
    }

    #[test]
    fn repeats_of_the_text_after_the_cursor_are_removed() {
        assert_eq!(
            clean_completion(
                "\\item Third\n\\end{itemize}",
                "\\begin{itemize}\n\\item First\n\\item Second\n",
                "\n\\end{itemize}\n"
            ),
            "\\item Third"
        );
        // A short coincidence is kept.
        assert_eq!(
            clean_completion(" in 2020.", "Published", ". More"),
            " in 2020."
        );
    }

    #[test]
    fn fences_quotes_cursor_markers_and_long_answers_are_cleaned() {
        assert_eq!(
            clean_completion("```latex\n\\item Next\n```", "\\item First\n", ""),
            "\\item Next"
        );
        assert_eq!(
            clean_completion("\"a quoted answer\"", "X", ""),
            "a quoted answer"
        );
        assert_eq!(clean_completion(" end<CURSOR>", "The", ""), " end");
        let long: String = (1..=10).map(|n| format!("line {n}\n")).collect();
        assert_eq!(
            clean_completion(&long, "Start", ""),
            "line 1\nline 2\nline 3\nline 4\nline 5\nline 6"
        );
        assert_eq!(clean_completion("   \n  ", "Text", ""), "");
        // Leading whitespace and line breaks are part of the continuation.
        assert_eq!(
            clean_completion("\n\\item Next", "\\item First", ""),
            "\n\\item Next"
        );
    }
}
