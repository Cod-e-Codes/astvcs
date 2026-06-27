use similar::{ChangeTag, TextDiff};

/// Line-oriented text diff using the Myers algorithm via `similar`.
pub fn diff_text(old: &str, new: &str) -> Vec<TextEdit> {
    let diff = TextDiff::from_lines(old, new);
    let mut edits = Vec::new();
    let mut old_line = 0usize;
    let mut new_line = 0usize;

    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                old_line += 1;
                new_line += 1;
            }
            ChangeTag::Delete => {
                edits.push(TextEdit::DeleteLine {
                    line: old_line,
                    content: change.value().trim_end_matches('\n').to_string(),
                });
                old_line += 1;
            }
            ChangeTag::Insert => {
                edits.push(TextEdit::InsertLine {
                    line: new_line,
                    content: change.value().trim_end_matches('\n').to_string(),
                });
                new_line += 1;
            }
        }
    }
    edits
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TextEdit {
    ReplaceLine {
        line: usize,
        old: String,
        new: String,
    },
    DeleteLine {
        line: usize,
        content: String,
    },
    InsertLine {
        line: usize,
        content: String,
    },
}

impl TextEdit {
    pub fn line(&self) -> usize {
        match self {
            Self::ReplaceLine { line, .. }
            | Self::DeleteLine { line, .. }
            | Self::InsertLine { line, .. } => *line,
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Self::ReplaceLine { line, old, new } => {
                format!("line {line}: '{old}' -> '{new}'")
            }
            Self::DeleteLine { line, content } => format!("line {line}: delete '{content}'"),
            Self::InsertLine { line, content } => format!("line {line}: insert '{content}'"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_line_change() {
        let edits = diff_text("a\nb\n", "a\nc\n");
        assert!(!edits.is_empty());
    }

    #[test]
    fn detects_insertion() {
        let edits = diff_text("a\n", "a\nb\n");
        assert!(
            edits
                .iter()
                .any(|e| matches!(e, TextEdit::InsertLine { .. }))
        );
    }

    #[test]
    fn identical_has_no_edits() {
        assert!(diff_text("same\n", "same\n").is_empty());
    }
}
