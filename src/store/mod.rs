pub(crate) mod atomic;
mod blobs;
mod cherry_pick;
pub mod error;
mod format;
mod history;
pub(crate) mod hooks;
mod identity;
mod integrity;
mod lock;
mod manifest;
mod merge_resolve;
mod pack;
mod reachability;
mod rebase;
mod repo;
mod scan_cache;
mod staging;
mod stash;
mod tags;
mod tracked;
mod walk;
mod working;

pub use integrity::{
    FsckFinding, FsckKind, FsckOptions, FsckRepair, FsckRepairKind, FsckReport, GcReport,
};
pub use lock::RepoLockGuard;
pub use pack::RepackReport;
pub use reachability::{ROOT_STATE_ID, Reachability};

pub use blobs::{BlobId, BlobStore};
pub use error::{RepoError, RepoErrorKind, RepoResult};
pub use history::{ancestors, merge_base, walk_history};
pub use identity::{
    AuthorIdentity, IdentityConfig, configured_identity, resolve_author_identity, set_identity,
};
pub use manifest::{FileMode, ManifestEntry, ManifestMap, hash_manifest};
pub use merge_resolve::{
    MergeResolution, MergeResolveSide, apply_merge_resolutions, parse_merge_resolution,
    parse_merge_resolutions,
};
pub use repo::{
    BranchInfo, ChangeColumn, CommitOptions, CommitOutcome, FileStatus, MergePlan, Repo,
    RepoConfig, RevertOutcome, RevertPlan, ScanOptions, StateEntry, StateId, TimelineEntry,
    WorkingStatus,
};
pub use scan_cache::ScanCache;
pub use staging::{StagedEntry, StagingIndex};
pub use stash::{StashId, StashInfo};
pub use tags::TagInfo;
pub use tracked::{TrackedFile, tracked_eq};
pub use walk::{ASTVCS_DIR, ScanMetrics, ScanMode, ScanReport, SkippedPath, last_scan_metrics};
