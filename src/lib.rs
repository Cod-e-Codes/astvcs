pub mod diff;
pub mod frontend;
pub mod graph;
pub mod intent;
pub mod merge;
pub mod network;
pub mod store;
pub mod trace;
pub mod unparser;

pub use diff::TextEdit;
pub use diff::{DiffResult, diff_graphs, diff_text};
pub use frontend::{
    FileContent, SourceLanguage, TextBlob, parse_language, parse_rust, parse_source,
    parse_text_or_blob, supported_extensions, supported_special_paths,
};
pub use graph::{AstGraph, Mutation, Node, NodeId, NodeKind, TriviaSlot};
pub use merge::{
    MergeConflict, MergeOutcome, MutationOverlap, OverlapReason, PathMergeConflict,
    find_overlapping_mutations, merge_files,
};
pub use network::{
    RemoteConfig, add_remote, clone_repo, fetch, list_remotes, push, remove_remote, serve_repo,
};
pub use store::{
    BlobId, BlobStore, BranchInfo, CommitOutcome, FileStatus, MergePlan, MergeResolution,
    MergeResolveSide, Repo, StateEntry, StateId, TimelineEntry, WorkingStatus, ancestors,
    hash_manifest, merge_base, parse_merge_resolution, parse_merge_resolutions, walk_history,
};
pub use trace::{is_verbose, notice, set_verbose, warn};
pub use unparser::unparse;
