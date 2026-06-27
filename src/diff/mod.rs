mod ast_diff;
mod lcs;
mod text_diff;

pub use ast_diff::{DiffResult, diff_graphs};
pub use lcs::lcs_pairs;
pub use text_diff::{TextEdit, diff_text};
