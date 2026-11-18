//! The system prompt: instructions plus what the model should know about the project and the
//! editor (open file, cursor, selection, compile errors), within a size budget.

use serde::Deserialize;
use ts_rs::TS;

use crate::search_replace;
use crate::tools::{ToolContext, diagnostics_text, outline_text};

/// What the user is looking at, sent with every message.
#[derive(Debug, Clone, Default, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EditorContext {
    #[ts(optional)]
    pub active_file: Option<String>,
    /// 1-based line of the cursor.
    #[ts(optional)]
    pub cursor_line: Option<u32>,
    #[ts(optional)]
    pub selection: Option<String>,
}

/// How the model edits files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditMode {
    /// Through the `propose_edit` tool.
    Tools,
    /// By writing SEARCH/REPLACE blocks (models without tool calling).
    SearchReplace,
}

const MAX_FILES_LISTED: usize = 80;
const MAX_SELECTION: usize = 6_000;

pub struct PromptInput<'a> {
    pub project_name: &'a str,
    pub tools: &'a ToolContext,
    pub editor: &'a EditorContext,
    pub mode: EditMode,
    /// Rough size limit of the whole prompt in characters (about 4 per token).
    pub budget_chars: usize,
}

pub fn system_prompt(input: &PromptInput<'_>) -> String {
    let mut prompt = String::new();
    prompt.push_str(&format!(
        "You are the writing assistant of Open Prism, a LaTeX editor. You help the user with \
         their LaTeX project \"{}\".\n\
         - Answer in the language the user writes in. Be concise.\n\
         - Write LaTeX that compiles. Keep the author's style and change only what is needed.\n\
         - You cannot change files yourself: you propose edits and the user accepts or \
         rejects them. Never say a change was made before the user accepted it.\n\
         - The user may attach images, such as regions captured from the compiled PDF. Each \
         comes with its page and the source position SyncTeX gives for it, which can be a few \
         lines off: read the file around that position before proposing an edit.\n",
        input.project_name
    ));
    match input.mode {
        EditMode::Tools => prompt.push_str(
            "- Use the tools to look at the project. Before editing a file, read it, then call \
             propose_edit with `old` copied exactly from the file. Prefer a few small edits \
             to rewriting whole files.\n\
             - After the user decides on your edits you get their decision; continue from it.\n",
        ),
        EditMode::SearchReplace => {
            prompt.push_str(
                "- You cannot call tools. The relevant file content is included below.\n",
            );
            prompt.push_str(search_replace::INSTRUCTIONS);
            prompt.push('\n');
        }
    }

    let tools = input.tools;
    prompt.push_str(&format!("\n## Project\nMain file: {}\n", tools.main_file));
    if let Ok(entries) = openprism_core::files::list_tree(&tools.project_root) {
        let files: Vec<&str> = entries
            .iter()
            .filter(|entry| entry.kind == openprism_core::files::EntryKind::File)
            .map(|entry| entry.path.as_str())
            .collect();
        prompt.push_str("Files: ");
        prompt.push_str(
            &files
                .iter()
                .take(MAX_FILES_LISTED)
                .copied()
                .collect::<Vec<_>>()
                .join(", "),
        );
        if files.len() > MAX_FILES_LISTED {
            prompt.push_str(&format!(" … and {} more", files.len() - MAX_FILES_LISTED));
        }
        prompt.push('\n');
    }

    let editor = input.editor;
    if let Some(active) = &editor.active_file {
        prompt.push_str(&format!("\n## Editor\nThe user is editing {active}"));
        if let Some(line) = editor.cursor_line {
            prompt.push_str(&format!(" with the cursor on line {line}"));
        }
        prompt.push_str(".\n");
    }
    if let Some(selection) = editor.selection.as_deref().filter(|s| !s.trim().is_empty()) {
        prompt.push_str("Selected text (\"this\" usually refers to it):\n```latex\n");
        prompt.push_str(&truncate(selection, MAX_SELECTION));
        prompt.push_str("\n```\n");
    }

    if let Some(compile) = &tools.compile {
        prompt.push_str(&format!("\n## Last compilation: {}\n", compile.status));
        if !compile.diagnostics.is_empty() {
            prompt.push_str(&diagnostics_text(&compile.diagnostics, 15));
        }
    }

    // What remains of the budget goes to the outline and, without tools, the open file.
    let remaining = input.budget_chars.saturating_sub(prompt.len());
    match input.mode {
        EditMode::Tools => {
            if let Ok(outline) = outline_text(tools) {
                prompt.push_str("\n## Outline (file:line)\n");
                prompt.push_str(&truncate(&outline, remaining.saturating_sub(40)));
            }
        }
        EditMode::SearchReplace => {
            if let Some(active) = &editor.active_file {
                let full = tools.project_root.join(active);
                if let Ok(text) = std::fs::read_to_string(&full) {
                    prompt.push_str(&format!("\n## Content of {active}\n```latex\n"));
                    prompt.push_str(&window(
                        &text,
                        editor.cursor_line.unwrap_or(1),
                        remaining.saturating_sub(60),
                    ));
                    prompt.push_str("\n```\n");
                }
            }
        }
    }
    prompt
}

/// At most `max` characters, cut on a character boundary.
fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… (truncated)", &text[..end])
}

/// Whole lines of `text` around `line`, within `max` characters.
fn window(text: &str, line: u32, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let lines: Vec<&str> = text.lines().collect();
    let center = (line.max(1) as usize - 1).min(lines.len().saturating_sub(1));
    let (mut start, mut end) = (center, center);
    let mut size = lines.get(center).map_or(0, |l| l.len() + 1);
    loop {
        let mut grew = false;
        if start > 0 && size + lines[start - 1].len() < max {
            start -= 1;
            size += lines[start].len() + 1;
            grew = true;
        }
        if end + 1 < lines.len() && size + lines[end + 1].len() < max {
            end += 1;
            size += lines[end].len() + 1;
            grew = true;
        }
        if !grew {
            break;
        }
    }
    format!(
        "% lines {}-{} of {}\n{}",
        start + 1,
        end + 1,
        lines.len(),
        lines[start..=end].join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools() -> (tempfile::TempDir, ToolContext) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.tex"),
            "\\documentclass{article}\n\\begin{document}\n\\section{Intro}\nHello.\n\\end{document}\n",
        )
        .unwrap();
        let ctx = ToolContext {
            project_root: dir.path().to_path_buf(),
            main_file: "main.tex".into(),
            compile: None,
        };
        (dir, ctx)
    }

    #[test]
    fn tool_mode_prompt_has_project_editor_and_outline() {
        let (_dir, ctx) = tools();
        let editor = EditorContext {
            active_file: Some("main.tex".into()),
            cursor_line: Some(4),
            selection: Some("Hello.".into()),
        };
        let prompt = system_prompt(&PromptInput {
            project_name: "Paper",
            tools: &ctx,
            editor: &editor,
            mode: EditMode::Tools,
            budget_chars: 20_000,
        });
        assert!(prompt.contains("\"Paper\""));
        assert!(prompt.contains("propose_edit"));
        assert!(prompt.contains("editing main.tex with the cursor on line 4"));
        assert!(prompt.contains("Selected text"));
        assert!(prompt.contains("main.tex:3: \\section{Intro}"));
        assert!(!prompt.contains("SEARCH"));
    }

    #[test]
    fn search_replace_prompt_includes_the_file() {
        let (_dir, ctx) = tools();
        let editor = EditorContext {
            active_file: Some("main.tex".into()),
            ..EditorContext::default()
        };
        let prompt = system_prompt(&PromptInput {
            project_name: "Paper",
            tools: &ctx,
            editor: &editor,
            mode: EditMode::SearchReplace,
            budget_chars: 20_000,
        });
        assert!(prompt.contains("<<<<<<< SEARCH"));
        assert!(prompt.contains("## Content of main.tex"));
        assert!(prompt.contains("Hello."));
    }

    #[test]
    fn window_keeps_lines_around_the_cursor() {
        let text: String = (1..=100).map(|n| format!("line {n}\n")).collect();
        let part = window(&text, 50, 80);
        assert!(part.contains("line 50"));
        assert!(!part.contains("line 1\n"));
        assert!(part.len() <= 120);
    }
}
