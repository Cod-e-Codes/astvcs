use crate::trace;
use std::path::Path;

/// Raw text blob for files that cannot be parsed into an AST.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TextBlob {
    pub content: String,
}

impl TextBlob {
    pub fn new(content: String) -> Self {
        Self { content }
    }

    pub fn from_path(path: &Path) -> Result<Self, String> {
        std::fs::read_to_string(path)
            .map(Self::new)
            .map_err(|e| e.to_string())
    }
}

/// Parse as AST when supported; fall back to text blob and emit a warning.
pub fn parse_text_or_blob(path: &str, source: &str) -> crate::frontend::FileContent {
    match crate::frontend::parse_source(path, source) {
        Ok(graph) => {
            trace::notice(format!("{path}: parsed as AST"));
            crate::frontend::FileContent::Ast(graph)
        }
        Err(reason) => {
            if reason.starts_with("no AST frontend") {
                if crate::frontend::is_text_only_path(path) {
                    trace::notice(format!("{path}: stored as text blob"));
                } else {
                    trace::warn_once(format!(
                        "{path}: AST parse unavailable ({reason}); using text blob"
                    ));
                    trace::notice(format!("{path}: text fallback ({reason})"));
                }
            } else {
                trace::warn(format!(
                    "{path}: AST parse failed ({reason}); using text blob"
                ));
                trace::notice(format!("{path}: text fallback ({reason})"));
            }
            crate::frontend::FileContent::Text(TextBlob::new(source.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::FileContent;
    use crate::trace;

    #[test]
    fn ast_parse_emits_notice() {
        trace::clear_log();
        trace::set_verbose(true);
        let content = parse_text_or_blob("main.rs", "fn main() {}\n");
        assert!(matches!(content, FileContent::Ast(_)));
        assert!(
            trace::take_log()
                .iter()
                .any(|l| l.contains("parsed as AST"))
        );
        trace::set_verbose(false);
    }

    #[test]
    fn new_language_extension_parses_as_ast() {
        trace::clear_log();
        trace::set_verbose(true);
        let content = parse_text_or_blob("app.ts", "function main(): void {}\n");
        assert!(matches!(content, FileContent::Ast(_)));
        assert!(
            trace::take_log()
                .iter()
                .any(|l| l.contains("app.ts: parsed as AST"))
        );
        trace::set_verbose(false);
    }

    #[test]
    fn unsupported_extension_emits_warning() {
        trace::clear_log();
        trace::clear_warned();
        let content = parse_text_or_blob("notes.md", "# hello\n");
        assert!(matches!(content, FileContent::Text(_)));
        let log = trace::take_log();
        assert!(!log.iter().any(|l| l.contains("warning:")));
        trace::clear_warned();
    }

    #[test]
    fn text_only_paths_are_quiet() {
        trace::clear_log();
        trace::clear_warned();
        trace::set_verbose(true);
        for path in [".gitignore", "notes.txt", "README.md", "go.sum", "run.ps1"] {
            let content = parse_text_or_blob(path, "data\n");
            assert!(matches!(content, FileContent::Text(_)));
        }
        let log = trace::take_log();
        assert!(!log.iter().any(|l| l.contains("warning:")));
        assert!(log.iter().any(|l| l.contains("stored as text blob")));
        trace::set_verbose(false);
        trace::clear_warned();
    }

    #[test]
    fn unknown_extension_warns_once() {
        trace::clear_log();
        trace::clear_warned();
        let src = "content\n";
        parse_text_or_blob("widget.foo", src);
        parse_text_or_blob("widget.foo", src);
        let log = trace::take_log();
        assert_eq!(
            log.iter()
                .filter(|l| l.contains("widget.foo") && l.contains("warning:"))
                .count(),
            1
        );
        trace::clear_warned();
    }

    #[test]
    fn syntax_error_emits_warning() {
        trace::clear_log();
        trace::clear_warned();
        let content = parse_text_or_blob("main.rs", "fn {{{\n");
        assert!(matches!(content, FileContent::Text(_)));
        let log = trace::take_log();
        assert!(log.iter().any(|l| l.contains("syntax errors")));
    }

    #[test]
    fn syntax_error_verbose_emits_notice() {
        trace::clear_log();
        trace::clear_warned();
        trace::set_verbose(true);
        parse_text_or_blob("main.rs", "fn {{{\n");
        let log = trace::take_log();
        assert!(
            log.iter()
                .any(|l| l.contains("notice:") && l.contains("text fallback")),
            "verbose parse failure should notice fallback: {log:?}"
        );
        trace::set_verbose(false);
    }
}
