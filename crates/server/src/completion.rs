//! Suggestions shown as grey "ghost text" at the editor cursor: the model continues the text
//! around the cursor. The writer accepts them with Tab; nothing is written before that.
//!
//! The suggestion is streamed as it is written, so the editor shows its start early. Only the
//! text around the cursor is sent, no tools are offered, and suggestions never contain citation
//! commands, not even for a moment: citations are the writer's job (D11).

use std::convert::Infallible;

use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::{Stream, StreamExt};
use openprism_ai::llm::{EventStream, LlmEvent, LlmRequest, Message, Usage};
use openprism_core::paths::RelPath;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::chat;
use crate::error::ApiError;
use crate::projects;
use crate::settings::{self, ModelRef};
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

/// What the editor receives while a suggestion is written (server-sent events).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export)]
pub enum CompletionEvent {
    /// The suggestion so far, cleaned: sent each time the part that is safe to show changes.
    Text { text: String },
    /// The whole suggestion, cleaned; empty when there is nothing useful to suggest. The last
    /// event of a stream that ends normally.
    Done { text: String },
    /// The provider failed while writing the suggestion.
    Error { message: String },
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

/// Asks the fast model (the agent model if there is none) for a suggestion and streams it.
///
/// Errors found before the model answers (bad input, no model, a provider that refuses the
/// request) are HTTP errors; a failure while it writes is an `error` event.
pub async fn complete(
    state: &AppState,
    request: CompletionRequest,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>> + use<>>, ApiError> {
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

    let mut stream = model.stream(llm_request).await.map_err(provider_error)?;
    // Some providers report a refused request (a bad key, an unknown model) as the first event.
    let first = match stream.next().await {
        Some(Err(err)) => return Err(provider_error(err)),
        first => first,
    };
    let run = Run {
        stream: Box::pin(futures_util::stream::iter(first).chain(stream)),
        raw: String::new(),
        shown: String::new(),
        usage: Usage::default(),
        prefix: request.prefix,
        suffix: request.suffix,
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

/// A suggestion being written.
struct Run {
    stream: EventStream,
    /// The answer so far.
    raw: String,
    /// The text of the last `text` event.
    shown: String,
    usage: Usage,
    prefix: String,
    suffix: String,
    state: AppState,
    model_ref: ModelRef,
}

impl Run {
    /// The next event to send, and the run to continue with (`None` after the last event).
    async fn next_event(mut self) -> (CompletionEvent, Option<Self>) {
        loop {
            match self.stream.next().await {
                Some(Ok(LlmEvent::Text(delta))) => {
                    self.raw.push_str(&delta);
                    // Enough lines: stop paying for more.
                    if self.raw.matches('\n').count() > MAX_LINES {
                        return (self.finish().await, None);
                    }
                    let visible = visible_completion(&self.raw, &self.prefix, &self.suffix);
                    if visible != self.shown {
                        self.shown.clone_from(&visible);
                        return (CompletionEvent::Text { text: visible }, Some(self));
                    }
                }
                Some(Ok(LlmEvent::Usage(more))) => self.usage.add(more),
                Some(Ok(_)) => {}
                Some(Err(err)) => {
                    record_usage(&self.state, &self.model_ref, self.usage).await;
                    let message = err.to_string();
                    return (CompletionEvent::Error { message }, None);
                }
                None => return (self.finish().await, None),
            }
        }
    }

    async fn finish(self) -> CompletionEvent {
        let text = clean_completion(&self.raw, &self.prefix, &self.suffix);
        record_usage(&self.state, &self.model_ref, self.usage).await;
        CompletionEvent::Done { text }
    }
}

async fn record_usage(state: &AppState, model_ref: &ModelRef, usage: Usage) {
    if let Err(err) = chat::record_usage(state, None, model_ref, usage).await {
        tracing::warn!(?err, "cannot record usage");
    }
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

/// The part of an unfinished answer that is safe to show while the model is still writing.
/// Cleaned like [`clean_completion`], but without what cleaning the whole answer could still
/// remove: a repeat of the text before the cursor in progress, a code fence or quotes around the
/// answer, and at the end the start of a citation command or of the text after the cursor. So it
/// is usually the start of the final suggestion, and never shows part of a citation.
pub fn visible_completion(raw: &str, prefix: &str, suffix: &str) -> String {
    let text = raw.replace(CURSOR, "");
    let text = without_start_of(&text, CURSOR);
    let start = text.trim_start();
    if start.starts_with(['"', '\u{201c}']) {
        // Quotes are removed only around the whole answer: wait for the end.
        return String::new();
    }
    let text = if start.starts_with("```") {
        match start.split_once('\n') {
            // Without the backticks at the end, which may be the closing fence.
            Some((_, inner)) => inner.trim_end_matches('`'),
            None => return String::new(),
        }
    } else if "``".starts_with(start) && !start.is_empty() {
        // Maybe the start of a fence.
        return String::new();
    } else {
        text
    };
    // The model may be repeating the text before the cursor.
    if prefix.contains(text) {
        return String::new();
    }
    let text = strip_prefix_echo(text, prefix);
    let text = cut_before_citation(text);
    let text = first_lines(text, MAX_LINES);
    let text = without_citation_start(text.trim_end());
    let text = without_start_of(text.trim_end(), suffix.trim_start());
    let text = text.trim_end();
    if text.trim().is_empty() {
        String::new()
    } else {
        text.to_owned()
    }
}

/// Removes the end of `text` when it is the start of `word`.
fn without_start_of<'a>(text: &'a str, word: &str) -> &'a str {
    let longest = word
        .char_indices()
        .map(|(index, c)| index + c.len_utf8())
        .take_while(|&end| end <= text.len())
        .filter(|&end| text.ends_with(&word[..end]))
        .max();
    longest.map_or(text, |end| &text[..text.len() - end])
}

/// Removes a backslash and letters at the end of `text` that may become a citation command.
fn without_citation_start(text: &str) -> &str {
    let Some(index) = text.rfind('\\') else {
        return text;
    };
    let name = text[index + 1..].to_ascii_lowercase();
    if name.chars().all(|c| c.is_ascii_alphabetic())
        && CITE_COMMANDS
            .iter()
            .any(|command| command.starts_with(&name))
    {
        &text[..index]
    } else {
        text
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

    /// Answers used to check what is shown while they stream: (answer, prefix, suffix).
    const SAMPLES: [(&str, &str, &str); 11] = [
        (
            " như đã chỉ ra trong \\cite{smith2020} và các nghiên cứu sau.",
            "Kết quả này",
            "",
        ),
        (
            "Chúng tôi đề xuất một phương pháp mới.",
            "\\section{Ý tưởng}\nChúng tôi đề xuất",
            "",
        ),
        (
            "\\item Third\n\\end{itemize}",
            "\\begin{itemize}\n\\item First\n\\item Second\n",
            "\n\\end{itemize}\n",
        ),
        ("```latex\n\\item Next\n```", "\\item First\n", ""),
        ("\"a quoted answer\"", "X", ""),
        (
            " gồm encoder, decoder và bộ nhớ.",
            "Mô hình của chúng tôi",
            "",
        ),
        (" see Section~\\ref{sec:intro}.", "As", ". More"),
        ("We use the data.", "Intro.\nWe use", ""),
        ("the end.", "over the", ""),
        (" shown by \\citet*{a} and \\noindent later.", "As", ""),
        (
            "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\n",
            "Start",
            "",
        ),
    ];

    #[test]
    fn what_is_shown_while_streaming_is_the_start_of_the_suggestion() {
        for (answer, prefix, suffix) in SAMPLES {
            let suggestion = clean_completion(answer, prefix, suffix);
            for end in answer
                .char_indices()
                .map(|(index, _)| index)
                .chain([answer.len()])
            {
                let shown = visible_completion(&answer[..end], prefix, suffix);
                assert!(
                    suggestion.starts_with(&shown),
                    "{:?} shows {shown:?}, not the start of {suggestion:?}",
                    &answer[..end]
                );
                assert!(!shown.contains("\\ci"), "{shown:?}");
            }
            // Once the answer is complete, something is shown (the `done` event has the rest).
            let shown = visible_completion(answer, prefix, suffix);
            assert!(
                !shown.is_empty() || suggestion.is_empty() || answer.starts_with('"'),
                "{answer:?}: {shown:?} vs {suggestion:?}"
            );
        }
    }

    #[test]
    fn unfinished_answers_show_only_what_cleaning_keeps() {
        // A text that cannot be a repeat is shown at once.
        assert_eq!(
            visible_completion(" gồm encoder", "Mô hình của chúng tôi", ""),
            " gồm encoder"
        );
        // A citation command being written is held back.
        assert_eq!(
            visible_completion(" như đã chỉ ra trong \\ci", "Kết quả này", ""),
            " như đã chỉ ra trong"
        );
        for start in ["\\", "\\C", "\\paren", "\\foot", "\\text", "\\no"] {
            assert_eq!(visible_completion(&format!(" a{start}"), "X", ""), " a");
        }
        // Commands that cannot become a citation are shown.
        assert_eq!(visible_completion(" a \\textbf", "X", ""), " a \\textbf");
        assert_eq!(visible_completion(" a \\ref", "X", ""), " a \\ref");
        // Maybe a repeat of the text before the cursor.
        assert_eq!(
            visible_completion("Chúng tôi đề", "Chúng tôi đề xuất", ""),
            ""
        );
        // Fences and quotes.
        assert_eq!(visible_completion("``", "X", ""), "");
        assert_eq!(visible_completion("```latex", "X", ""), "");
        assert_eq!(
            visible_completion("```latex\n\\item A\n``", "X", ""),
            "\\item A"
        );
        assert_eq!(visible_completion("\"quoted", "X", ""), "");
        assert_eq!(visible_completion(" ``quoted''", "X", ""), " ``quoted''");
        // Maybe the start of the text after the cursor.
        assert_eq!(
            visible_completion("\\item C\n\\en", "\\item B\n", "\n\\end{itemize}"),
            "\\item C"
        );
        // A cursor marker being written.
        assert_eq!(visible_completion(" end<CURS", "The", ""), " end");
    }
}
