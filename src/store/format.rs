//! On-disk repository format versioning and migrations.
//!
//! `config.json` `format_version` tracks layout migrations. Legacy repositories
//! without the field (or with `0`) are migrated on the first outermost `repo_lock`.

use super::atomic::write_atomic_json;
use super::error::{RepoError, RepoResult};
use super::repo::{CONFIG_FILE, Repo, RepoConfig};

/// Current on-disk format version written by `Repo::init` and targeted by migrations.
pub(crate) const CURRENT_FORMAT_VERSION: u32 = 1;

pub(crate) fn ensure_format_current(repo: &Repo) -> RepoResult<()> {
    let path = repo.astvcs_dir().join(CONFIG_FILE);
    let mut config: RepoConfig = super::repo::read_json_unlocked(&path)?;
    let stored = config.format_version;
    if stored > CURRENT_FORMAT_VERSION {
        return Ok(());
    }
    let mut version = stored;
    while version < CURRENT_FORMAT_VERSION {
        version = apply_migration(repo, version, &mut config)?;
    }
    Ok(())
}

fn apply_migration(repo: &Repo, from: u32, config: &mut RepoConfig) -> RepoResult<u32> {
    match from {
        0 => migrate_v0_to_v1(repo, config),
        other => Err(RepoError::other(format!(
            "no migration from on-disk format version {other}"
        ))),
    }
}

/// Records legacy repositories by stamping `format_version: 1` without changing data.
fn migrate_v0_to_v1(repo: &Repo, config: &mut RepoConfig) -> RepoResult<u32> {
    if config.format_version >= 1 {
        return Ok(1);
    }
    config.format_version = 1;
    write_atomic_json(&repo.astvcs_dir().join(CONFIG_FILE), config)
        .map_err(RepoError::from_message)?;
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn legacy_repo_without_format_version_migrates_on_lock() {
        let dir = TempDir::new().unwrap();
        let _repo = Repo::init_with_identity(dir.path()).unwrap();
        let config_path = dir.path().join(".astvcs").join(CONFIG_FILE);
        let mut value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        value.as_object_mut().unwrap().remove("format_version");
        fs::write(&config_path, serde_json::to_string_pretty(&value).unwrap()).unwrap();

        let reopened = Repo::open(dir.path()).unwrap();
        let _lock = reopened.repo_lock().unwrap();
        let config = reopened.load_config().unwrap();
        assert_eq!(config.format_version, CURRENT_FORMAT_VERSION);
    }

    #[test]
    fn format_version_zero_migrates_idempotently() {
        let dir = TempDir::new().unwrap();
        let _repo = Repo::init_with_identity(dir.path()).unwrap();
        let config_path = dir.path().join(".astvcs").join(CONFIG_FILE);
        let mut value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        value["format_version"] = serde_json::json!(0);
        fs::write(&config_path, serde_json::to_string_pretty(&value).unwrap()).unwrap();

        let reopened = Repo::open(dir.path()).unwrap();
        let _lock = reopened.repo_lock().unwrap();
        assert_eq!(
            reopened.load_config().unwrap().format_version,
            CURRENT_FORMAT_VERSION
        );
        let _lock = reopened.repo_lock().unwrap();
        assert_eq!(
            reopened.load_config().unwrap().format_version,
            CURRENT_FORMAT_VERSION
        );
    }
}
