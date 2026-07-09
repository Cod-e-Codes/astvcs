use crate::diff::{AlignEdge, PathRename, PathRenameKind, diff_graphs_detailed, diff_text};
use crate::frontend::{FileContent, is_ast_capable_path};
use crate::graph::{AstGraph, AstGraphSnapshot, Mutation};
use crate::intent::{classify_mutations, classify_path_rename, format_intent};
use crate::unparser::unparse;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// A complete diff view: one or more groups of per-file diffs.
#[derive(Clone, Debug, serde::Serialize)]
pub struct DiffViewDocument {
    pub left_label: String,
    pub right_label: String,
    pub groups: Vec<DiffViewGroup>,
}

/// A named group of files. Title is empty for a plain pairwise diff and set to
/// something like `base -> left` for three-way presentations.
#[derive(Clone, Debug, serde::Serialize)]
pub struct DiffViewGroup {
    pub title: String,
    pub files: Vec<DiffViewFile>,
}

/// How a single file changed, which decides how the viewer renders it.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffViewMode {
    Ast,
    Text,
    Binary,
    Symlink,
    KindChanged,
    Rename,
    Added,
    Deleted,
    Unchanged,
}

/// A classified edit intent paired with the mutation index it came from.
#[derive(Clone, Debug, serde::Serialize)]
pub struct IntentView {
    pub index: usize,
    pub label: String,
}

/// Everything the viewer needs to render one file diff honestly.
#[derive(Clone, Debug, serde::Serialize)]
pub struct DiffViewFile {
    pub path: String,
    pub path_from: Option<String>,
    pub mode: DiffViewMode,
    pub old: Option<AstGraphSnapshot>,
    pub new: Option<AstGraphSnapshot>,
    pub alignment: Vec<AlignEdge>,
    pub mutations: Vec<Mutation>,
    pub intents: Vec<IntentView>,
    pub source_old: Option<String>,
    pub source_new: Option<String>,
    pub text_edits: Vec<String>,
    pub note: Option<String>,
}

impl DiffViewFile {
    fn empty(path: &str, mode: DiffViewMode) -> Self {
        Self {
            path: path.to_string(),
            path_from: None,
            mode,
            old: None,
            new: None,
            alignment: Vec::new(),
            mutations: Vec::new(),
            intents: Vec::new(),
            source_old: None,
            source_new: None,
            text_edits: Vec::new(),
            note: None,
        }
    }
}

fn source_of(content: &FileContent) -> Option<String> {
    match content {
        FileContent::Ast(graph) => Some(unparse(graph)),
        FileContent::Text(blob) => Some(blob.content.clone()),
        FileContent::Symlink(blob) => Some(blob.target.clone()),
        FileContent::Binary(_) => None,
    }
}

fn intents_view(base: Option<&AstGraph>, mutations: &[Mutation]) -> Vec<IntentView> {
    classify_mutations(base, mutations)
        .into_iter()
        .map(|(index, intent)| IntentView {
            index,
            label: format_intent(base, &intent),
        })
        .collect()
}

fn fill_ast_diff(file: &mut DiffViewFile, old: &AstGraph, new: &AstGraph) {
    let detailed = diff_graphs_detailed(old, new);
    file.mode = DiffViewMode::Ast;
    file.old = Some(old.to_snapshot());
    file.new = Some(new.to_snapshot());
    file.intents = intents_view(Some(old), &detailed.mutations);
    file.alignment = detailed.alignment;
    file.mutations = detailed.mutations;
    file.source_old = Some(unparse(old));
    file.source_new = Some(unparse(new));
}

fn fill_text_diff(file: &mut DiffViewFile, old_src: &str, new_src: &str) {
    file.text_edits = diff_text(old_src, new_src)
        .iter()
        .map(|edit| edit.summary())
        .collect();
    file.source_old = Some(old_src.to_string());
    file.source_new = Some(new_src.to_string());
}

fn fill_changed(file: &mut DiffViewFile, path: &str, old: &FileContent, new: &FileContent) {
    match (old, new) {
        (FileContent::Ast(o), FileContent::Ast(n)) => fill_ast_diff(file, o, n),
        (FileContent::Text(a), FileContent::Text(b)) => {
            file.mode = DiffViewMode::Text;
            fill_text_diff(file, &a.content, &b.content);
            if is_ast_capable_path(path) {
                file.note = Some("(text fallback - structural diff unavailable)".to_string());
            }
        }
        (FileContent::Ast(_), FileContent::Text(_))
        | (FileContent::Text(_), FileContent::Ast(_))
            if is_ast_capable_path(path) =>
        {
            file.mode = DiffViewMode::Text;
            let old_src = source_of(old).unwrap_or_default();
            let new_src = source_of(new).unwrap_or_default();
            fill_text_diff(file, &old_src, &new_src);
            file.note = Some("(text fallback - structural diff unavailable)".to_string());
        }
        (FileContent::Binary(_), FileContent::Binary(_)) => {
            file.mode = DiffViewMode::Binary;
            file.note = Some("(binary file - content diff omitted)".to_string());
        }
        (FileContent::Symlink(a), FileContent::Symlink(b)) => {
            file.mode = DiffViewMode::Symlink;
            file.note = Some(format!("(symlink target: {} -> {})", a.target, b.target));
        }
        _ => {
            file.mode = DiffViewMode::KindChanged;
            file.source_old = source_of(old);
            file.source_new = source_of(new);
            file.note = Some(format!(
                "(file kind changed: {} -> {})",
                old.display_kind(),
                new.display_kind()
            ));
        }
    }
}

fn fill_added(file: &mut DiffViewFile, new: &FileContent) {
    file.source_new = source_of(new);
    match new {
        FileContent::Binary(_) => {
            file.note = Some("(binary file - content diff omitted)".to_string());
        }
        FileContent::Symlink(blob) => {
            file.note = Some(format!("(symlink -> {})", blob.target));
        }
        _ => {}
    }
}

fn fill_deleted(file: &mut DiffViewFile, old: &FileContent) {
    file.source_old = source_of(old);
    match old {
        FileContent::Binary(_) => {
            file.note = Some("(binary file - content diff omitted)".to_string());
        }
        FileContent::Symlink(blob) => {
            file.note = Some(format!("(symlink -> {})", blob.target));
        }
        _ => {}
    }
}

/// Build a per-file diff view from optional old and new content.
pub fn file_from_contents(
    path: &str,
    old: Option<&FileContent>,
    new: Option<&FileContent>,
) -> DiffViewFile {
    match (old, new) {
        (None, None) => DiffViewFile::empty(path, DiffViewMode::Unchanged),
        (None, Some(new)) => {
            let mut file = DiffViewFile::empty(path, DiffViewMode::Added);
            fill_added(&mut file, new);
            file
        }
        (Some(old), None) => {
            let mut file = DiffViewFile::empty(path, DiffViewMode::Deleted);
            fill_deleted(&mut file, old);
            file
        }
        (Some(old), Some(new)) => {
            let mut file = DiffViewFile::empty(path, DiffViewMode::Unchanged);
            if old.semantic_eq(new) {
                file.source_old = source_of(old);
                file.source_new = source_of(new);
                return file;
            }
            fill_changed(&mut file, path, old, new);
            file
        }
    }
}

/// Build a per-file diff view for a detected path rename.
pub fn file_from_rename(rename: &PathRename, old: &FileContent, new: &FileContent) -> DiffViewFile {
    let mut file = file_from_contents(&rename.to, Some(old), Some(new));
    file.path_from = Some(rename.from.clone());
    file.mode = DiffViewMode::Rename;

    let rename_intent = classify_path_rename(rename);
    let mut intents = vec![IntentView {
        index: 0,
        label: format_intent(None, &rename_intent),
    }];
    if rename.kind == PathRenameKind::WithEdits {
        for existing in file.intents.drain(..) {
            intents.push(IntentView {
                index: existing.index + 1,
                label: existing.label,
            });
        }
    }
    file.intents = intents;
    file
}

/// Inline the serialized document into the self-contained HTML viewer template.
pub fn render_diff_view_html(doc: &DiffViewDocument) -> Result<String, String> {
    let template = include_str!("view/viewer.html");
    let json = serde_json::to_string(doc).map_err(|e| e.to_string())?;
    // Neutralize any `<` (in particular `</script`) so the JSON is safe to inline
    // inside a <script> element. `\u003c` decodes back to `<` in JS.
    let safe_json = json.replace('<', "\\u003c");
    if !template.contains("/*__ASTVCS_DIFF_JSON__*/null") {
        return Err("viewer template is missing the diff JSON placeholder".to_string());
    }
    Ok(template.replace("/*__ASTVCS_DIFF_JSON__*/null", &safe_json))
}

static VIEW_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Render the viewer HTML and write it to a unique file in the temp directory.
pub fn write_diff_view_html(doc: &DiffViewDocument) -> Result<PathBuf, String> {
    let html = render_diff_view_html(doc)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = VIEW_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "astvcs-diff-{}-{}-{}.html",
        std::process::id(),
        nanos,
        seq
    ));
    std::fs::write(&path, html).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Open a file in the OS default browser. Skips opening when `CI` or
/// `ASTVCS_NO_BROWSER` is set (integration tests and headless runs).
pub fn open_in_browser(path: &Path) -> Result<(), String> {
    if std::env::var_os("CI").is_some() || std::env::var_os("ASTVCS_NO_BROWSER").is_some() {
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        // The empty string is the window title argument that `start` expects
        // before the path so a quoted path is not swallowed as the title.
        Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::{BinaryBlob, SymlinkBlob, TextBlob, parse_rust, parse_text_or_blob};

    fn ast(src: &str) -> FileContent {
        FileContent::Ast(parse_rust(src).unwrap())
    }

    #[test]
    fn added_file_previews_source_without_trees() {
        let new = ast("fn foo() {}\n");
        let file = file_from_contents("a.rs", None, Some(&new));
        assert!(matches!(file.mode, DiffViewMode::Added));
        assert!(file.old.is_none() && file.new.is_none());
        assert!(file.source_new.is_some());
        assert!(file.alignment.is_empty());
    }

    #[test]
    fn deleted_file_previews_old_source() {
        let old = ast("fn foo() {}\n");
        let file = file_from_contents("a.rs", Some(&old), None);
        assert!(matches!(file.mode, DiffViewMode::Deleted));
        assert!(file.source_old.is_some());
    }

    #[test]
    fn unchanged_file_reports_unchanged() {
        let old = ast("fn foo() {}\n");
        let new = ast("fn foo() {}\n");
        let file = file_from_contents("a.rs", Some(&old), Some(&new));
        assert!(matches!(file.mode, DiffViewMode::Unchanged));
    }

    #[test]
    fn ast_change_fills_snapshots_and_alignment() {
        let old = ast("fn foo() {\n    let x = 1;\n}\n");
        let new = ast("fn foo() {\n    let y = 1;\n}\n");
        let file = file_from_contents("a.rs", Some(&old), Some(&new));
        assert!(matches!(file.mode, DiffViewMode::Ast));
        assert!(file.old.is_some() && file.new.is_some());
        assert!(!file.alignment.is_empty());
        assert!(!file.mutations.is_empty());
        assert!(!file.intents.is_empty());
    }

    #[test]
    fn text_change_uses_text_mode() {
        let old = FileContent::Text(TextBlob::new("a\nb\n".to_string()));
        let new = FileContent::Text(TextBlob::new("a\nc\n".to_string()));
        let file = file_from_contents("notes.txt", Some(&old), Some(&new));
        assert!(matches!(file.mode, DiffViewMode::Text));
        assert!(!file.text_edits.is_empty());
        assert!(file.note.is_none());
    }

    #[test]
    fn text_fallback_on_ast_path_adds_note() {
        let old = parse_text_or_blob("a.rs", "fn foo(");
        let new = parse_text_or_blob("a.rs", "fn bar(");
        assert!(!old.is_ast() && !new.is_ast());
        let file = file_from_contents("a.rs", Some(&old), Some(&new));
        assert!(matches!(file.mode, DiffViewMode::Text));
        assert!(file.note.is_some());
    }

    #[test]
    fn binary_change_omits_content() {
        let old = FileContent::Binary(BinaryBlob::new(vec![0, 1, 2]));
        let new = FileContent::Binary(BinaryBlob::new(vec![0, 1, 3]));
        let file = file_from_contents("blob.bin", Some(&old), Some(&new));
        assert!(matches!(file.mode, DiffViewMode::Binary));
        assert!(file.note.is_some());
    }

    #[test]
    fn symlink_change_records_targets() {
        let old = FileContent::Symlink(SymlinkBlob::new("a".to_string()));
        let new = FileContent::Symlink(SymlinkBlob::new("b".to_string()));
        let file = file_from_contents("link", Some(&old), Some(&new));
        assert!(matches!(file.mode, DiffViewMode::Symlink));
        assert!(file.note.unwrap().contains("->"));
    }

    #[test]
    fn kind_change_is_reported() {
        let old = FileContent::Text(TextBlob::new("hi\n".to_string()));
        let new = FileContent::Binary(BinaryBlob::new(vec![0, 1, 2]));
        let file = file_from_contents("thing", Some(&old), Some(&new));
        assert!(matches!(file.mode, DiffViewMode::KindChanged));
        assert!(file.note.is_some());
    }

    #[test]
    fn rename_with_edits_includes_rename_intent_first() {
        let old = ast("fn foo() { 1 }\n");
        let new = ast("fn foo() { 2 }\n");
        let rename = PathRename {
            from: "a.rs".to_string(),
            to: "b.rs".to_string(),
            kind: PathRenameKind::WithEdits,
        };
        let file = file_from_rename(&rename, &old, &new);
        assert!(matches!(file.mode, DiffViewMode::Rename));
        assert_eq!(file.path, "b.rs");
        assert_eq!(file.path_from.as_deref(), Some("a.rs"));
        assert_eq!(file.intents[0].index, 0);
        assert!(file.intents[0].label.contains("rename path"));
        assert!(file.intents.len() > 1);
    }

    #[test]
    fn render_inlines_json_and_escapes_angle_brackets() {
        let old = ast("fn foo() {\n    let x = 1;\n}\n");
        let new = ast("fn foo() {\n    let y = 1;\n}\n");
        let file = file_from_contents("a.rs", Some(&old), Some(&new));
        let doc = DiffViewDocument {
            left_label: "old".to_string(),
            right_label: "new".to_string(),
            groups: vec![DiffViewGroup {
                title: String::new(),
                files: vec![file],
            }],
        };
        let html = render_diff_view_html(&doc).unwrap();
        assert!(!html.contains("/*__ASTVCS_DIFF_JSON__*/null"));
        assert!(html.contains("astvcs diff"));
        assert!(!html.contains("</script\": "));
    }

    #[test]
    fn open_in_browser_skips_under_ci() {
        // SAFETY: single-threaded test process; restores prior value.
        let prior = std::env::var_os("CI");
        unsafe { std::env::set_var("CI", "1") };
        let result = open_in_browser(Path::new("nonexistent.html"));
        match prior {
            Some(value) => unsafe { std::env::set_var("CI", value) },
            None => unsafe { std::env::remove_var("CI") },
        }
        assert!(result.is_ok());
    }
}
