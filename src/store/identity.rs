use super::atomic::write_atomic_json;
use super::error::{RepoError, RepoResult};
use super::repo::Repo;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const GLOBAL_CONFIG_DIR: &str = ".astvcs";
const GLOBAL_CONFIG_FILE: &str = "config.json";

/// Author recorded on timeline entries (not part of content-addressed state ids).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorIdentity {
    pub name: String,
    pub email: String,
}

/// Optional author identity stored in repository or global config.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<AuthorIdentity>,
}

fn global_config_path() -> Option<PathBuf> {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE")?;
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(GLOBAL_CONFIG_DIR)
            .join(GLOBAL_CONFIG_FILE),
    )
}

fn read_identity_config(path: &Path) -> RepoResult<IdentityConfig> {
    if !path.is_file() {
        return Ok(IdentityConfig::default());
    }
    let text =
        fs::read_to_string(path).map_err(|e| RepoError::from_io("read identity config", e))?;
    serde_json::from_str(&text)
        .map_err(|e| RepoError::other(format!("parse identity config {}: {e}", path.display())))
}

fn write_identity_config(path: &Path, config: &IdentityConfig) -> RepoResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| RepoError::from_io("create config dir", e))?;
    }
    write_atomic_json(path, config).map_err(RepoError::from_message)
}

fn identity_from_env() -> Option<AuthorIdentity> {
    let name = std::env::var("ASTVCS_AUTHOR_NAME").ok()?;
    let email = std::env::var("ASTVCS_AUTHOR_EMAIL").ok()?;
    if name.is_empty() || email.is_empty() {
        return None;
    }
    Some(AuthorIdentity { name, email })
}

/// Resolve author identity: environment variables override repository config, which overrides global.
pub fn resolve_author_identity(repo: &Repo) -> RepoResult<AuthorIdentity> {
    if let Some(id) = identity_from_env() {
        return Ok(id);
    }
    let local = load_repo_identity_config(repo)?;
    if let Some(author) = local.author {
        return Ok(author);
    }
    if let Some(path) = global_config_path() {
        let global = read_identity_config(&path)?;
        if let Some(author) = global.author {
            return Ok(author);
        }
    }
    Err(RepoError::missing_identity())
}

pub fn load_repo_identity_config(repo: &Repo) -> RepoResult<IdentityConfig> {
    let path = repo.astvcs_dir().join("config.json");
    if !path.is_file() {
        return Ok(IdentityConfig::default());
    }
    let text = fs::read_to_string(&path).map_err(|e| RepoError::from_io("read config", e))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| RepoError::other(format!("parse config {}: {e}", path.display())))?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

fn save_repo_identity_config(repo: &Repo, config: &IdentityConfig) -> RepoResult<()> {
    let path = repo.astvcs_dir().join("config.json");
    let mut value: serde_json::Value = if path.is_file() {
        let text = fs::read_to_string(&path).map_err(|e| RepoError::from_io("read config", e))?;
        serde_json::from_str(&text)
            .map_err(|e| RepoError::other(format!("parse config {}: {e}", path.display())))?
    } else {
        serde_json::json!({
            "version": 2,
            "default_branch": "main",
            "format_version": crate::store::format::CURRENT_FORMAT_VERSION,
        })
    };
    if let Some(author) = &config.author {
        value["author"] = serde_json::to_value(author)
            .map_err(|e| RepoError::other(format!("serialize author: {e}")))?;
    } else {
        value.as_object_mut().and_then(|o| o.remove("author"));
    }
    write_atomic_json(&path, &value).map_err(RepoError::from_message)
}

fn save_global_identity_config(config: &IdentityConfig) -> RepoResult<()> {
    let path = global_config_path()
        .ok_or_else(|| RepoError::other("cannot determine home directory for global config"))?;
    write_identity_config(&path, config)
}

/// Read configured identity without environment override (for `identity get`).
pub fn configured_identity(repo: &Repo, global: bool) -> RepoResult<Option<AuthorIdentity>> {
    if global {
        let path = global_config_path()
            .ok_or_else(|| RepoError::other("cannot determine home directory for global config"))?;
        return Ok(read_identity_config(&path)?.author);
    }
    Ok(load_repo_identity_config(repo)?.author)
}

pub fn set_identity(repo: &Repo, name: &str, email: &str, global: bool) -> RepoResult<()> {
    let _lock = repo.repo_lock()?;
    if name.is_empty() {
        return Err(RepoError::invalid_input("author name cannot be empty"));
    }
    if email.is_empty() {
        return Err(RepoError::invalid_input("author email cannot be empty"));
    }
    let config = IdentityConfig {
        author: Some(AuthorIdentity {
            name: name.to_string(),
            email: email.to_string(),
        }),
    };
    if global {
        save_global_identity_config(&config)
    } else {
        save_repo_identity_config(repo, &config)
    }
}

#[allow(dead_code)]
pub fn clear_identity(repo: &Repo, global: bool) -> RepoResult<()> {
    let _lock = repo.repo_lock()?;
    let config = IdentityConfig::default();
    if global {
        save_global_identity_config(&config)
    } else {
        save_repo_identity_config(repo, &config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Repo;
    use tempfile::TempDir;

    fn setup_identity(repo: &Repo) {
        set_identity(repo, "Test User", "test@example.com", false).unwrap();
    }

    #[test]
    fn repo_local_identity_roundtrip() {
        let dir = TempDir::new().unwrap();
        let repo = Repo::init(dir.path()).unwrap();
        setup_identity(&repo);
        let repo2 = Repo::open(dir.path()).unwrap();
        let id = resolve_author_identity(&repo2).unwrap();
        assert_eq!(id.name, "Test User");
        assert_eq!(id.email, "test@example.com");
    }

    #[test]
    fn env_overrides_repo_config() {
        let dir = TempDir::new().unwrap();
        let repo = Repo::init(dir.path()).unwrap();
        setup_identity(&repo);
        unsafe {
            std::env::set_var("ASTVCS_AUTHOR_NAME", "Env User");
            std::env::set_var("ASTVCS_AUTHOR_EMAIL", "env@example.com");
        }
        let id = resolve_author_identity(&repo).unwrap();
        assert_eq!(id.name, "Env User");
        unsafe {
            std::env::remove_var("ASTVCS_AUTHOR_NAME");
            std::env::remove_var("ASTVCS_AUTHOR_EMAIL");
        }
    }

    #[test]
    fn missing_identity_errors() {
        let dir = TempDir::new().unwrap();
        let repo = Repo::init(dir.path()).unwrap();
        let err = resolve_author_identity(&repo).unwrap_err();
        assert_eq!(
            err.kind,
            super::super::error::RepoErrorKind::MissingIdentity
        );
    }
}
