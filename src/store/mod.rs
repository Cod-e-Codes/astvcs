pub(crate) mod atomic;
mod blobs;
mod history;
mod integrity;
mod lock;
mod merge_resolve;
mod reachability;
mod repo;
mod walk;

pub use integrity::{FsckFinding, FsckKind, FsckReport, GcReport};
pub use lock::RepoLockGuard;
pub use reachability::{ROOT_STATE_ID, Reachability};

pub use blobs::{BlobId, BlobStore, hash_manifest};
pub use history::{ancestors, merge_base, walk_history};
pub use merge_resolve::{
    MergeResolution, MergeResolveSide, apply_merge_resolutions, parse_merge_resolution,
    parse_merge_resolutions,
};
pub use repo::{
    BranchInfo, CommitOutcome, FileStatus, MergePlan, Repo, RepoConfig, RevertOutcome, RevertPlan,
    StateEntry, StateId, TimelineEntry, WorkingStatus,
};
pub use walk::{ASTVCS_DIR, ScanReport, SkippedPath};
