mod ast_diff;
mod lcs;
mod path_rename;
mod text_diff;

pub use ast_diff::{DiffResult, diff_graphs};
pub use lcs::lcs_pairs;
pub use path_rename::{
    PathRename, PathRenameKind, build_rename_map, detect_path_renames, rename_targets_conflict,
    side_path_for_base,
};
pub use text_diff::{TextEdit, diff_text};
