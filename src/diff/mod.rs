mod align;
mod ast_diff;
mod lcs;
mod path_rename;
mod text_diff;
mod view;

pub use ast_diff::{
    AlignEdge, AlignKind, AlignMethod, DetailedDiffResult, DiffResult, diff_graphs,
    diff_graphs_detailed,
};
pub use lcs::lcs_pairs;
pub use path_rename::{
    PathRename, PathRenameKind, build_rename_map, detect_path_renames, rename_targets_conflict,
    side_path_for_base,
};
pub use text_diff::{TextEdit, diff_text};
pub use view::{
    DiffViewDocument, DiffViewFile, DiffViewGroup, DiffViewMode, IntentView, file_from_contents,
    file_from_rename, open_in_browser, render_diff_view_html, write_diff_view_html,
};
