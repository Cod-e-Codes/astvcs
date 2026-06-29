use serde::Serialize;
use std::fmt;
use std::ops::Deref;

/// Category of repository operation failure for tooling and embedders.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoErrorKind {
    LockContention,
    DirtyWorkingTree,
    MergeConflict,
    RevertConflict,
    RevertPrecondition,
    UnknownRef,
    NotFound,
    MissingIdentity,
    BranchGuard,
    AlreadyExists,
    InvalidInput,
    IntegrityCheck,
    StateMismatch,
    HookFailed,
    Other,
}

/// Structured repository error with a human-readable message matching legacy string output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RepoError {
    pub kind: RepoErrorKind,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

impl RepoError {
    pub fn new(kind: RepoErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            path: None,
            reference: None,
        }
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn with_reference(mut self, reference: impl Into<String>) -> Self {
        self.reference = Some(reference.into());
        self
    }

    pub fn lock_contention(message: impl Into<String>) -> Self {
        Self::new(RepoErrorKind::LockContention, message)
    }

    pub fn dirty_working_tree(message: impl Into<String>) -> Self {
        Self::new(RepoErrorKind::DirtyWorkingTree, message)
    }

    pub fn merge_conflict(message: impl Into<String>) -> Self {
        Self::new(RepoErrorKind::MergeConflict, message)
    }

    pub fn revert_conflict(message: impl Into<String>) -> Self {
        Self::new(RepoErrorKind::RevertConflict, message)
    }

    pub fn revert_precondition(message: impl Into<String>) -> Self {
        Self::new(RepoErrorKind::RevertPrecondition, message)
    }

    pub fn unknown_ref(reference: impl Into<String>) -> Self {
        let reference = reference.into();
        Self::new(
            RepoErrorKind::UnknownRef,
            format!("unknown branch or state: {reference}"),
        )
        .with_reference(reference)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(RepoErrorKind::NotFound, message)
    }

    pub fn missing_identity() -> Self {
        Self::new(
            RepoErrorKind::MissingIdentity,
            "author identity not configured; run `astvcs identity set --name <name> --email <email>` \
             or set ASTVCS_AUTHOR_NAME and ASTVCS_AUTHOR_EMAIL",
        )
    }

    pub fn branch_guard(message: impl Into<String>) -> Self {
        Self::new(RepoErrorKind::BranchGuard, message)
    }

    pub fn already_exists(message: impl Into<String>) -> Self {
        Self::new(RepoErrorKind::AlreadyExists, message)
    }

    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::new(RepoErrorKind::InvalidInput, message)
    }

    pub fn integrity_check(message: impl Into<String>) -> Self {
        Self::new(RepoErrorKind::IntegrityCheck, message)
    }

    pub fn hook_failed(message: impl Into<String>) -> Self {
        Self::new(RepoErrorKind::HookFailed, message)
    }

    pub fn other(message: impl Into<String>) -> Self {
        Self::new(RepoErrorKind::Other, message)
    }

    pub fn from_io(context: &str, err: std::io::Error) -> Self {
        Self::other(format!("{context}: {err}"))
    }

    /// Wrap a legacy string error, inferring kind from message content when possible.
    pub fn from_message(message: impl Into<String>) -> Self {
        let message = message.into();
        let kind = if message.contains("repository is locked by another process") {
            RepoErrorKind::LockContention
        } else if message.contains("uncommitted changes") {
            RepoErrorKind::DirtyWorkingTree
        } else if message.starts_with("merge would conflict") {
            RepoErrorKind::MergeConflict
        } else if message.starts_with("revert would conflict") {
            RepoErrorKind::RevertConflict
        } else if message.contains("cannot revert") || message.contains("not an ancestor") {
            RepoErrorKind::RevertPrecondition
        } else if message.starts_with("unknown branch or state:") {
            RepoErrorKind::UnknownRef
        } else if message.contains("branch not found")
            || message.contains("remote not found")
            || message.contains("not an astvcs repository")
        {
            RepoErrorKind::NotFound
        } else if message.contains("author identity not configured") {
            RepoErrorKind::MissingIdentity
        } else if message.contains("checked-out branch") || message.contains("last branch") {
            RepoErrorKind::BranchGuard
        } else if message.contains("already exists") {
            RepoErrorKind::AlreadyExists
        } else if message.contains("integrity check failed") {
            RepoErrorKind::IntegrityCheck
        } else if message.starts_with("hook ") && message.contains(" failed with exit code ") {
            RepoErrorKind::HookFailed
        } else {
            RepoErrorKind::Other
        };
        let mut err = Self::new(kind, message.clone());
        if let Some(reference) = message.strip_prefix("unknown branch or state: ") {
            err.reference = Some(reference.to_string());
        }
        err
    }
}

impl fmt::Display for RepoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Deref for RepoError {
    type Target = str;

    fn deref(&self) -> &str {
        &self.message
    }
}

impl From<RepoError> for String {
    fn from(err: RepoError) -> String {
        err.message
    }
}

impl From<String> for RepoError {
    fn from(message: String) -> Self {
        Self::from_message(message)
    }
}

impl From<&str> for RepoError {
    fn from(message: &str) -> Self {
        Self::from_message(message)
    }
}

impl From<std::io::Error> for RepoError {
    fn from(err: std::io::Error) -> Self {
        Self::other(err.to_string())
    }
}

pub type RepoResult<T> = Result<T, RepoError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deref_allows_contains_on_error() {
        let err = RepoError::dirty_working_tree("working tree has uncommitted changes");
        assert!(err.contains("uncommitted changes"));
    }

    #[test]
    fn display_matches_message() {
        let err = RepoError::missing_identity();
        assert_eq!(
            err.to_string(),
            "author identity not configured; run `astvcs identity set --name <name> --email <email>` \
             or set ASTVCS_AUTHOR_NAME and ASTVCS_AUTHOR_EMAIL"
        );
    }
}
