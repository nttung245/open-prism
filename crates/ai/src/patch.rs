//! Edits proposed by the AI. A patch is a list of hunks, each replacing one exact piece of
//! text (`old`) by another (`new`). Anchoring on content rather than line numbers lets a patch
//! still apply after the user typed elsewhere in the file, as long as `old` is still there.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Hunk {
    /// Exact text to replace; empty only when creating a file.
    pub old: String,
    pub new: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum PatchStatus {
    Pending,
    Accepted,
    Rejected,
    /// Some hunks accepted, some rejected.
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Patch {
    pub id: String,
    /// Project-relative path.
    pub file: String,
    /// One-line explanation from the model.
    pub summary: String,
    /// The file does not exist yet; the single hunk holds its content.
    pub creates_file: bool,
    pub hunks: Vec<Hunk>,
    pub status: PatchStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PatchError {
    #[error("the edit has no changes")]
    Empty,
    #[error(
        "change {index}: the `old` text was not found in {file}. Read the file again and copy \
         the text to replace exactly, including line breaks"
    )]
    NotFound { index: usize, file: String },
    #[error(
        "change {index}: the `old` text appears {count} times in {file}. Include more \
         surrounding text so that it matches exactly one place"
    )]
    Ambiguous {
        index: usize,
        count: usize,
        file: String,
    },
    #[error("changes {first} and {second} overlap; merge them into one change")]
    Overlap { first: usize, second: usize },
    #[error("change {index}: `old` is empty; give the exact text to replace")]
    EmptyOld { index: usize },
    #[error("change {index}: `old` and `new` are identical")]
    NoOp { index: usize },
}

/// Where each hunk applies in `content`, after validating that every `old` text matches
/// exactly one place and that the places do not overlap. Hunks whose exact text is missing
/// are matched again ignoring trailing spaces and `\r`, which small models often get wrong.
pub fn locate(
    content: &str,
    hunks: &[Hunk],
    file: &str,
) -> Result<Vec<(usize, usize)>, PatchError> {
    if hunks.is_empty() {
        return Err(PatchError::Empty);
    }
    let mut spans = Vec::with_capacity(hunks.len());
    for (i, hunk) in hunks.iter().enumerate() {
        let index = i + 1;
        if hunk.old.is_empty() {
            return Err(PatchError::EmptyOld { index });
        }
        if hunk.old == hunk.new {
            return Err(PatchError::NoOp { index });
        }
        let exact: Vec<usize> = content.match_indices(&hunk.old).map(|(at, _)| at).collect();
        let span = match exact.len() {
            1 => (exact[0], exact[0] + hunk.old.len()),
            0 => match loose_matches(content, &hunk.old).as_slice() {
                [only] => *only,
                [] => {
                    return Err(PatchError::NotFound {
                        index,
                        file: file.to_owned(),
                    });
                }
                many => {
                    return Err(PatchError::Ambiguous {
                        index,
                        count: many.len(),
                        file: file.to_owned(),
                    });
                }
            },
            count => {
                return Err(PatchError::Ambiguous {
                    index,
                    count,
                    file: file.to_owned(),
                });
            }
        };
        spans.push(span);
    }
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by_key(|&i| spans[i].0);
    for pair in order.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if spans[a].1 > spans[b].0 {
            return Err(PatchError::Overlap {
                first: a.min(b) + 1,
                second: a.max(b) + 1,
            });
        }
    }
    Ok(spans)
}

/// Spans of `content` whose lines equal `needle`'s lines once trailing whitespace and `\r`
/// are ignored. Only whole-line needles are matched this way.
fn loose_matches(content: &str, needle: &str) -> Vec<(usize, usize)> {
    let wanted: Vec<&str> = needle
        .trim_end_matches(['\n', '\r'])
        .split('\n')
        .map(|line| line.trim_end())
        .collect();
    if wanted.iter().all(|line| line.is_empty()) {
        return Vec::new();
    }
    // Byte offset of the start of every line of `content`.
    let mut starts = vec![0];
    starts.extend(content.match_indices('\n').map(|(at, _)| at + 1));
    let lines: Vec<&str> = content.split('\n').collect();
    let mut found = Vec::new();
    for first in 0..lines.len() {
        if first + wanted.len() > lines.len() {
            break;
        }
        let same = wanted
            .iter()
            .zip(&lines[first..first + wanted.len()])
            .all(|(want, have)| *want == have.trim_end());
        if same {
            let last = first + wanted.len() - 1;
            let end = starts[last] + lines[last].trim_end_matches('\r').len();
            // Keep the needle's own trailing newline, if it had one and the text has one.
            let end = if needle.ends_with('\n') && content[end..].starts_with('\n') {
                end + 1
            } else if needle.ends_with('\n') && content[end..].starts_with("\r\n") {
                end + 2
            } else {
                end
            };
            found.push((starts[first], end));
        }
    }
    found
}

/// Applies the `accepted` hunks (same order as `hunks`) to `content`.
pub fn apply(
    content: &str,
    hunks: &[Hunk],
    accepted: &[bool],
    file: &str,
) -> Result<String, PatchError> {
    let spans = locate(content, hunks, file)?;
    let mut chosen: Vec<(usize, usize, &str)> = spans
        .iter()
        .zip(hunks)
        .zip(accepted.iter().chain(std::iter::repeat(&false)))
        .filter(|(_, accept)| **accept)
        .map(|(((start, end), hunk), _)| (*start, *end, hunk.new.as_str()))
        .collect();
    chosen.sort_by_key(|(start, _, _)| *start);
    let mut out = String::with_capacity(content.len());
    let mut cursor = 0;
    for (start, end, new) in chosen {
        out.push_str(&content[cursor..start]);
        out.push_str(new);
        cursor = end;
    }
    out.push_str(&content[cursor..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunk(old: &str, new: &str) -> Hunk {
        Hunk {
            old: old.into(),
            new: new.into(),
        }
    }

    #[test]
    fn applies_unique_hunks_in_any_order() {
        let text = "alpha\nbeta\ngamma\n";
        let hunks = [hunk("gamma", "GAMMA"), hunk("alpha", "ALPHA")];
        assert_eq!(
            apply(text, &hunks, &[true, true], "f").unwrap(),
            "ALPHA\nbeta\nGAMMA\n"
        );
        assert_eq!(
            apply(text, &hunks, &[false, true], "f").unwrap(),
            "ALPHA\nbeta\ngamma\n"
        );
    }

    #[test]
    fn reports_missing_ambiguous_and_overlapping_text() {
        let text = "a b a\nc d\n";
        assert!(matches!(
            locate(text, &[hunk("zzz", "y")], "f"),
            Err(PatchError::NotFound { index: 1, .. })
        ));
        assert!(matches!(
            locate(text, &[hunk("a", "y")], "f"),
            Err(PatchError::Ambiguous { count: 2, .. })
        ));
        assert!(matches!(
            locate(text, &[hunk("a b", "x"), hunk("b a", "y")], "f"),
            Err(PatchError::Overlap {
                first: 1,
                second: 2
            })
        ));
        assert!(matches!(
            locate(text, &[hunk("", "x")], "f"),
            Err(PatchError::EmptyOld { .. })
        ));
        assert!(matches!(
            locate(text, &[hunk("c d", "c d")], "f"),
            Err(PatchError::NoOp { .. })
        ));
        assert!(matches!(locate(text, &[], "f"), Err(PatchError::Empty)));
    }

    #[test]
    fn tolerates_trailing_whitespace_and_crlf() {
        let text = "\\section{Intro}  \r\nSome text.\r\nEnd\r\n";
        let hunks = [hunk(
            "\\section{Intro}\nSome text.\n",
            "\\section{Intro}\nBetter text.\n",
        )];
        assert_eq!(
            apply(text, &hunks, &[true], "f").unwrap(),
            "\\section{Intro}\nBetter text.\nEnd\r\n"
        );
    }

    #[test]
    fn error_messages_guide_the_model() {
        let err = locate("x", &[hunk("y", "z")], "main.tex").unwrap_err();
        assert!(err.to_string().contains("Read the file again"));
        assert!(err.to_string().contains("main.tex"));
    }
}
