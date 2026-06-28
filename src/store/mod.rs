pub(crate) mod atomic;
mod blobs;
pub mod error;
mod history;
mod identity;
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
pub use error::{RepoError, RepoErrorKind, RepoResult};
pub use history::{ancestors, merge_base, walk_history};
pub use identity::{
    AuthorIdentity, IdentityConfig, configured_identity, resolve_author_identity, set_identity,
};
pub use merge_resolve::{
    MergeResolution, MergeResolveSide, apply_merge_resolutions, parse_merge_resolution,
    parse_merge_resolutions,
};
pub use repo::{
    BranchInfo, CommitOutcome, FileStatus, MergePlan, Repo, RepoConfig, RevertOutcome, RevertPlan,
    StateEntry, StateId, TimelineEntry, WorkingStatus,
};
pub use walk::{ASTVCS_DIR, ScanReport, SkippedPath};
