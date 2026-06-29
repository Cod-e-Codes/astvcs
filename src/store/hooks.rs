use super::atomic::write_atomic_text;
use super::error::{RepoError, RepoErrorKind, RepoResult};
use super::lock::{lock_held, suspend_repo_lock};
use super::repo::StateId;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const HOOKS_DIR: &str = "hooks";
const COMMIT_MSG_INPUT: &str = "commit-msg-input";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookName {
    PreCommit,
    CommitMsg,
    PreMerge,
    PrePush,
}

impl HookName {
    fn file_name(self) -> &'static str {
        match self {
            Self::PreCommit => "pre-commit",
            Self::CommitMsg => "commit-msg",
            Self::PreMerge => "pre-merge",
            Self::PrePush => "pre-push",
        }
    }
}

pub struct HookContext<'a> {
    pub branch: Option<&'a str>,
    pub head_state_id: &'a StateId,
    pub commit_msg_file: Option<&'a Path>,
    pub merge_branch: Option<&'a str>,
    pub remote: Option<&'a str>,
}

pub fn run_hook(
    repo_root: &Path,
    astvcs_dir: &Path,
    name: HookName,
    ctx: &HookContext<'_>,
) -> RepoResult<()> {
    let hook_path = resolve_hook_path(astvcs_dir, name.file_name())?;
    let Some(hook_path) = hook_path else {
        return Ok(());
    };

    let mut cmd = build_hook_command(&hook_path)?;
    cmd.current_dir(repo_root);
    cmd.env("ASTVCS_ROOT", repo_root);
    cmd.env("ASTVCS_BRANCH", ctx.branch.unwrap_or_default());
    cmd.env("ASTVCS_HEAD_STATE_ID", ctx.head_state_id.as_str());
    if let Some(path) = ctx.commit_msg_file {
        cmd.env("ASTVCS_COMMIT_MSG_FILE", path);
    }
    if let Some(branch) = ctx.merge_branch {
        cmd.env("ASTVCS_MERGE_BRANCH", branch);
    }
    if let Some(remote) = ctx.remote {
        cmd.env("ASTVCS_REMOTE", remote);
    }
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());

    let status = cmd
        .status()
        .map_err(|e| RepoError::from_io("run hook", e))?;
    if status.success() {
        Ok(())
    } else {
        let code = status.code().unwrap_or(-1);
        Err(RepoError::new(
            RepoErrorKind::HookFailed,
            format!("hook {} failed with exit code {code}", name.file_name()),
        ))
    }
}

fn resolve_hook_path(astvcs_dir: &Path, name: &str) -> RepoResult<Option<PathBuf>> {
    let hooks_dir = astvcs_dir.join(HOOKS_DIR);
    let base = hooks_dir.join(name);
    if base.is_file() {
        return Ok(Some(base));
    }
    #[cfg(windows)]
    {
        for ext in [".cmd", ".bat", ".ps1"] {
            let candidate = hooks_dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Ok(Some(candidate));
            }
        }
    }
    Ok(None)
}

fn build_hook_command(hook_path: &Path) -> RepoResult<Command> {
    build_hook_command_impl(hook_path)
}

#[cfg(windows)]
fn build_hook_command_impl(hook_path: &Path) -> RepoResult<Command> {
    let ext = hook_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let cmd = match ext.as_str() {
        "cmd" | "bat" => {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(hook_path);
            c
        }
        "ps1" => {
            let mut c = Command::new("powershell");
            c.arg("-NoProfile").arg("-File").arg(hook_path);
            c
        }
        _ => Command::new(hook_path),
    };
    Ok(cmd)
}

#[cfg(unix)]
fn build_hook_command_impl(hook_path: &Path) -> RepoResult<Command> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(hook_path).map_err(|e| RepoError::from_io("stat hook", e))?;
    let cmd = if meta.permissions().mode() & 0o111 != 0 {
        Command::new(hook_path)
    } else {
        let mut c = Command::new("sh");
        c.arg(hook_path);
        c
    };
    Ok(cmd)
}

#[cfg(not(any(unix, windows)))]
fn build_hook_command_impl(hook_path: &Path) -> RepoResult<Command> {
    Ok(Command::new(hook_path))
}

pub fn commit_msg_input_path(astvcs_dir: &Path) -> PathBuf {
    astvcs_dir.join(HOOKS_DIR).join(COMMIT_MSG_INPUT)
}

pub fn write_commit_msg_input(astvcs_dir: &Path, message: &str) -> RepoResult<()> {
    write_atomic_text(&commit_msg_input_path(astvcs_dir), message).map_err(RepoError::from_message)
}

pub fn read_commit_msg_input(astvcs_dir: &Path) -> RepoResult<String> {
    let path = commit_msg_input_path(astvcs_dir);
    let content =
        std::fs::read_to_string(&path).map_err(|e| RepoError::from_io("read commit message", e))?;
    Ok(content.trim_end().to_string())
}

pub fn run_commit_hooks(
    repo: &super::repo::Repo,
    head: &StateId,
    message: &str,
    no_verify: bool,
) -> RepoResult<String> {
    if no_verify {
        return Ok(message.to_string());
    }
    if !lock_held() {
        return Err(RepoError::other(
            "internal error: commit hooks require held repository lock",
        ));
    }

    let branch = repo.head_branch_unlocked()?;
    let ctx = HookContext {
        branch: branch.as_deref(),
        head_state_id: head,
        commit_msg_file: None,
        merge_branch: None,
        remote: None,
    };

    suspend_repo_lock()?;
    let hook_result = (|| {
        run_hook(
            repo.root_path(),
            &repo.astvcs_dir(),
            HookName::PreCommit,
            &ctx,
        )?;

        let astvcs_dir = repo.astvcs_dir();
        write_commit_msg_input(&astvcs_dir, message)?;
        let msg_path = commit_msg_input_path(&astvcs_dir);
        let msg_ctx = HookContext {
            branch: branch.as_deref(),
            head_state_id: head,
            commit_msg_file: Some(&msg_path),
            merge_branch: None,
            remote: None,
        };
        run_hook(repo.root_path(), &astvcs_dir, HookName::CommitMsg, &msg_ctx)?;
        read_commit_msg_input(&astvcs_dir)
    })();

    let _guard = super::lock::resume_repo_lock(&repo.astvcs_dir())?;
    hook_result
}

pub fn run_pre_merge_hook(
    repo: &super::repo::Repo,
    head: &StateId,
    merge_branch: &str,
    no_verify: bool,
) -> RepoResult<()> {
    if no_verify {
        return Ok(());
    }
    if !lock_held() {
        return Err(RepoError::other(
            "internal error: pre-merge hook requires held repository lock",
        ));
    }

    let branch = repo.head_branch_unlocked()?;
    let ctx = HookContext {
        branch: branch.as_deref(),
        head_state_id: head,
        commit_msg_file: None,
        merge_branch: Some(merge_branch),
        remote: None,
    };

    suspend_repo_lock()?;
    let hook_result = run_hook(
        repo.root_path(),
        &repo.astvcs_dir(),
        HookName::PreMerge,
        &ctx,
    );
    let _guard = super::lock::resume_repo_lock(&repo.astvcs_dir())?;
    hook_result
}

pub fn run_pre_push_hook(
    repo: &super::repo::Repo,
    head: &StateId,
    remote: &str,
    no_verify: bool,
) -> RepoResult<()> {
    if no_verify {
        return Ok(());
    }
    if !lock_held() {
        return Err(RepoError::other(
            "internal error: pre-push hook requires held repository lock",
        ));
    }

    let branch = repo.head_branch_unlocked()?;
    let ctx = HookContext {
        branch: branch.as_deref(),
        head_state_id: head,
        commit_msg_file: None,
        merge_branch: None,
        remote: Some(remote),
    };

    suspend_repo_lock()?;
    let hook_result = run_hook(
        repo.root_path(),
        &repo.astvcs_dir(),
        HookName::PrePush,
        &ctx,
    );
    let _guard = super::lock::resume_repo_lock(&repo.astvcs_dir())?;
    hook_result
}
