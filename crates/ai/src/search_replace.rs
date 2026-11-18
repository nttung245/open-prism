//! Edits for models that cannot call tools: the model writes SEARCH/REPLACE blocks in its
//! answer, which are parsed into the same hunks `propose_edit` produces.
//!
//! ```text
//! chapters/intro.tex
//! <<<<<<< SEARCH
//! old text
//! =======
//! new text
//! >>>>>>> REPLACE
//! ```

use crate::patch::Hunk;

pub const INSTRUCTIONS: &str = "\
To change a file, answer with one sentence, then one block per change in exactly this format:

path/to/file.tex
<<<<<<< SEARCH
exact existing lines to replace, copied from the file
=======
the new lines
>>>>>>> REPLACE

Example. To fix \"Helo world.\" in main.tex, answer:

Fixed the typo.

main.tex
<<<<<<< SEARCH
Helo world.
=======
Hello world.
>>>>>>> REPLACE

Rules: SEARCH must match the file exactly and only once; copy only the lines that change \
(plus a line of context if needed), never the whole file; do not wrap the blocks in code \
fences.";

/// One parsed block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// File named before the block, if any.
    pub file: Option<String>,
    pub hunk: Hunk,
}

const SEARCH: &str = "<<<<<<< SEARCH";
const DIVIDER: &str = "=======";
const REPLACE: &str = ">>>>>>> REPLACE";

/// Every complete block in `text`, in order. Incomplete blocks are ignored.
pub fn parse(text: &str) -> Vec<Block> {
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() != SEARCH {
            i += 1;
            continue;
        }
        let file = file_before(&lines[..i]);
        let Some(divider) = (i + 1..lines.len()).find(|&j| lines[j].trim() == DIVIDER) else {
            break;
        };
        let Some(end) = (divider + 1..lines.len()).find(|&j| lines[j].trim() == REPLACE) else {
            break;
        };
        let old = join(&lines[i + 1..divider]);
        let new = join(&lines[divider + 1..end]);
        blocks.push(Block {
            file,
            hunk: Hunk { old, new },
        });
        i = end + 1;
    }
    blocks
}

fn join(lines: &[&str]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        let mut text = lines.join("\n");
        text.push('\n');
        text
    }
}

/// The closest preceding line that looks like a file path, skipping code fences and blanks.
fn file_before(lines: &[&str]) -> Option<String> {
    for line in lines.iter().rev().take(3) {
        let trimmed = line.trim().trim_matches(['`', '*']).trim();
        if trimmed.is_empty() || trimmed.starts_with("```") {
            continue;
        }
        let has_extension = trimmed.rsplit_once('.').is_some_and(|(stem, ext)| {
            !stem.is_empty()
                && (1..=8).contains(&ext.len())
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
        });
        let looks_like_path = has_extension
            && !trimmed.contains(' ')
            && !trimmed.contains(':')
            && trimmed.len() < 300;
        return looks_like_path.then(|| trimmed.trim_start_matches("./").to_owned());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_blocks_with_and_without_file_names() {
        let answer = "I fixed the typo.\n\n```latex\nchapters/intro.tex\n<<<<<<< SEARCH\nTeh result\n=======\nThe result\n>>>>>>> REPLACE\n```\n\nAnd:\n<<<<<<< SEARCH\na\nb\n=======\n>>>>>>> REPLACE\n";
        let blocks = parse(answer);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].file.as_deref(), Some("chapters/intro.tex"));
        assert_eq!(blocks[0].hunk.old, "Teh result\n");
        assert_eq!(blocks[0].hunk.new, "The result\n");
        assert_eq!(blocks[1].file, None);
        assert_eq!(blocks[1].hunk.old, "a\nb\n");
        assert_eq!(blocks[1].hunk.new, "");
    }

    #[test]
    fn sentences_are_not_file_names() {
        let blocks = parse("Fixed.\n<<<<<<< SEARCH\nx\n=======\ny\n>>>>>>> REPLACE");
        assert_eq!(blocks[0].file, None);
    }

    #[test]
    fn ignores_incomplete_blocks() {
        assert!(parse("<<<<<<< SEARCH\nx\n=======\ny\n").is_empty());
        assert!(parse("no blocks here").is_empty());
    }

    #[test]
    fn file_names_in_bold_or_backticks() {
        let blocks = parse("**`main.tex`**\n<<<<<<< SEARCH\nx\n=======\ny\n>>>>>>> REPLACE");
        assert_eq!(blocks[0].file.as_deref(), Some("main.tex"));
    }
}
