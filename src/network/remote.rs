use crate::store::Repo;
use crate::store::atomic::write_atomic_json;
use std::collections::HashMap;
use std::fs;

const REMOTES_FILE: &str = "remotes.json";

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct RemoteEntry {
    pub url: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Default)]
pub struct RemoteConfig {
    pub remotes: HashMap<String, RemoteEntry>,
}

pub fn load_remotes(repo: &Repo) -> Result<RemoteConfig, String> {
    let path = repo.astvcs_dir().join(REMOTES_FILE);
    if !path.is_file() {
        return Ok(RemoteConfig::default());
    }
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn save_remotes(repo: &Repo, config: &RemoteConfig) -> Result<(), String> {
    let path = repo.astvcs_dir().join(REMOTES_FILE);
    write_atomic_json(&path, config)
}

pub fn add_remote(repo: &Repo, name: &str, url: &str) -> Result<(), String> {
    let _lock = repo.repo_lock().map_err(|e| e.to_string())?;
    if name.is_empty() {
        return Err("remote name cannot be empty".into());
    }
    crate::network::transport::parse_remote_url(url)?;
    let mut config = load_remotes(repo)?;
    if config.remotes.contains_key(name) {
        return Err(format!("remote already exists: {name}"));
    }
    config.remotes.insert(
        name.to_string(),
        RemoteEntry {
            url: url.to_string(),
        },
    );
    save_remotes(repo, &config)
}

pub fn remove_remote(repo: &Repo, name: &str) -> Result<(), String> {
    let _lock = repo.repo_lock().map_err(|e| e.to_string())?;
    let mut config = load_remotes(repo)?;
    if config.remotes.remove(name).is_none() {
        return Err(format!("remote not found: {name}"));
    }
    save_remotes(repo, &config)?;
    let remote_refs = repo.astvcs_dir().join("refs/remotes").join(name);
    if remote_refs.is_dir() {
        fs::remove_dir_all(&remote_refs).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn list_remotes(repo: &Repo) -> Result<Vec<(String, String)>, String> {
    let _lock = repo.repo_lock().map_err(|e| e.to_string())?;
    let config = load_remotes(repo)?;
    let mut out: Vec<_> = config
        .remotes
        .iter()
        .map(|(name, entry)| (name.clone(), entry.url.clone()))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

pub fn remote_url(repo: &Repo, name: &str) -> Result<String, String> {
    let config = load_remotes(repo)?;
    config
        .remotes
        .get(name)
        .map(|e| e.url.clone())
        .ok_or_else(|| format!("remote not found: {name}"))
}

pub fn ensure_remote_dir(repo: &Repo, name: &str) -> Result<(), String> {
    let dir = repo.astvcs_dir().join("refs/remotes").join(name);
    fs::create_dir_all(dir).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Repo;
    use tempfile::TempDir;

    #[test]
    fn add_list_remove_remote() {
        let dir = TempDir::new().unwrap();
        let repo = Repo::init_with_identity(dir.path()).unwrap();
        add_remote(&repo, "origin", dir.path().to_str().unwrap()).unwrap();
        let list = list_remotes(&repo).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].0, "origin");
        remove_remote(&repo, "origin").unwrap();
        assert!(list_remotes(&repo).unwrap().is_empty());
    }
}
