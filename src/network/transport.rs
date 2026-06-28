use crate::store::{ManifestMap, Repo, RepoError, StateId, TimelineEntry};
use std::collections::HashMap;
use std::path::PathBuf;

fn map_repo<T>(result: Result<T, RepoError>) -> Result<T, String> {
    result.map_err(|e| e.to_string())
}

const API_PREFIX: &str = "/v1";

pub fn parse_remote_url(url: &str) -> Result<PathBuf, String> {
    if url.starts_with("http://") || url.starts_with("https://") {
        return Ok(PathBuf::from(url));
    }
    if let Some(rest) = url.strip_prefix("file://") {
        return Ok(normalize_file_path(rest));
    }
    let path = PathBuf::from(url);
    if path.join(".astvcs").is_dir() {
        return Ok(path);
    }
    Err(format!("not a valid astvcs remote url: {url}"))
}

fn normalize_file_path(path: &str) -> PathBuf {
    let p = path.trim_start_matches('/');
    if path.len() >= 3 && path.as_bytes()[2] == b':' {
        PathBuf::from(&path[1..])
    } else {
        PathBuf::from(p)
    }
}

pub enum Transport {
    File(Repo),
    Http {
        base: String,
        client: reqwest::blocking::Client,
    },
}

impl Transport {
    pub fn open(url: &str) -> Result<Self, String> {
        if url.starts_with("http://") || url.starts_with("https://") {
            let base = url.trim_end_matches('/').to_string();
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .map_err(|e| e.to_string())?;
            return Ok(Self::Http { base, client });
        }
        let path = parse_remote_url(url)?;
        let repo = map_repo(Repo::open(&path))?;
        Ok(Self::File(repo))
    }

    fn http_url(&self, path: &str) -> Result<String, String> {
        match self {
            Self::Http { base, .. } => Ok(format!("{base}{API_PREFIX}{path}")),
            Self::File(_) => Err("http url requested on file transport".into()),
        }
    }

    pub fn has_blob(&self, id: &str) -> Result<bool, String> {
        match self {
            Self::File(repo) => Ok(repo.has_blob(id)),
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/blobs/{id}"))?;
                let resp = client.head(&url).send().map_err(|e| e.to_string())?;
                Ok(resp.status().is_success())
            }
        }
    }

    pub fn get_blob(&self, id: &str) -> Result<Vec<u8>, String> {
        match self {
            Self::File(repo) => map_repo(repo.read_blob_bytes(id)),
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/blobs/{id}"))?;
                let resp = client.get(&url).send().map_err(|e| e.to_string())?;
                if !resp.status().is_success() {
                    return Err(format!("blob {id}: HTTP {}", resp.status()));
                }
                resp.bytes().map(|b| b.to_vec()).map_err(|e| e.to_string())
            }
        }
    }

    pub fn put_blob(&self, id: &str, bytes: &[u8]) -> Result<(), String> {
        match self {
            Self::File(repo) => map_repo(repo.import_blob_bytes(id, bytes)),
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/blobs/{id}"))?;
                let resp = client
                    .put(&url)
                    .body(bytes.to_vec())
                    .send()
                    .map_err(|e| e.to_string())?;
                if resp.status().is_success() || resp.status().as_u16() == 409 {
                    return Ok(());
                }
                Err(format!("put blob {id}: HTTP {}", resp.status()))
            }
        }
    }

    pub fn has_state(&self, id: &StateId) -> Result<bool, String> {
        match self {
            Self::File(repo) => Ok(repo.has_state(id)),
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/states/{id}"))?;
                let resp = client.head(&url).send().map_err(|e| e.to_string())?;
                Ok(resp.status().is_success())
            }
        }
    }

    pub fn get_state(&self, id: &StateId) -> Result<ManifestMap, String> {
        match self {
            Self::File(repo) => map_repo(repo.load_manifest(id)),
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/states/{id}"))?;
                let resp = client.get(&url).send().map_err(|e| e.to_string())?;
                if !resp.status().is_success() {
                    return Err(format!("state {id}: HTTP {}", resp.status()));
                }
                resp.json().map_err(|e| e.to_string())
            }
        }
    }

    pub fn put_state(&self, id: &StateId, manifest: &ManifestMap) -> Result<(), String> {
        match self {
            Self::File(repo) => map_repo(repo.import_state_manifest(id, manifest)),
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/states/{id}"))?;
                let resp = client
                    .put(&url)
                    .json(manifest)
                    .send()
                    .map_err(|e| e.to_string())?;
                if resp.status().is_success() || resp.status().as_u16() == 409 {
                    return Ok(());
                }
                Err(format!("put state {id}: HTTP {}", resp.status()))
            }
        }
    }

    pub fn has_timeline(&self, id: &StateId) -> Result<bool, String> {
        match self {
            Self::File(repo) => Ok(repo.has_timeline(id)),
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/timeline/{id}"))?;
                let resp = client.head(&url).send().map_err(|e| e.to_string())?;
                Ok(resp.status().is_success())
            }
        }
    }

    pub fn get_timeline(&self, id: &StateId) -> Result<TimelineEntry, String> {
        match self {
            Self::File(repo) => map_repo(repo.load_timeline_entry(id)),
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/timeline/{id}"))?;
                let resp = client.get(&url).send().map_err(|e| e.to_string())?;
                if !resp.status().is_success() {
                    return Err(format!("timeline {id}: HTTP {}", resp.status()));
                }
                resp.json().map_err(|e| e.to_string())
            }
        }
    }

    pub fn put_timeline(&self, entry: &TimelineEntry) -> Result<(), String> {
        match self {
            Self::File(repo) => map_repo(repo.import_timeline_entry(entry)),
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/timeline/{}", entry.id))?;
                let resp = client
                    .put(&url)
                    .json(entry)
                    .send()
                    .map_err(|e| e.to_string())?;
                if resp.status().is_success() || resp.status().as_u16() == 409 {
                    return Ok(());
                }
                Err(format!("put timeline {}: HTTP {}", entry.id, resp.status()))
            }
        }
    }

    pub fn list_refs(&self) -> Result<HashMap<String, StateId>, String> {
        match self {
            Self::File(repo) => {
                let mut out = HashMap::new();
                for branch in map_repo(repo.list_branches())? {
                    out.insert(branch.name, branch.state_id);
                }
                Ok(out)
            }
            Self::Http { client, .. } => {
                let url = self.http_url("/refs/heads")?;
                let resp = client.get(&url).send().map_err(|e| e.to_string())?;
                if !resp.status().is_success() {
                    return Err(format!("list refs: HTTP {}", resp.status()));
                }
                resp.json().map_err(|e| e.to_string())
            }
        }
    }

    pub fn get_ref(&self, branch: &str) -> Result<Option<StateId>, String> {
        match self {
            Self::File(repo) => {
                let path = repo.astvcs_dir().join("refs/heads").join(branch);
                if !path.is_file() {
                    return Ok(None);
                }
                Ok(Some(map_repo(repo.branch_state(branch))?))
            }
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/refs/heads/{branch}"))?;
                let resp = client.get(&url).send().map_err(|e| e.to_string())?;
                if resp.status().as_u16() == 404 {
                    return Ok(None);
                }
                if !resp.status().is_success() {
                    return Err(format!("get ref {branch}: HTTP {}", resp.status()));
                }
                let text = resp.text().map_err(|e| e.to_string())?;
                Ok(Some(text.trim().to_string()))
            }
        }
    }

    pub fn set_ref(&self, branch: &str, state_id: &StateId, force: bool) -> Result<(), String> {
        match self {
            Self::File(repo) => {
                if !force
                    && let Ok(current) = map_repo(repo.branch_state(branch))
                    && current != *state_id
                    && !map_repo(repo.is_ancestor_of(&current, state_id))?
                {
                    return Err(format!("non-fast-forward update for refs/heads/{branch}"));
                }
                map_repo(repo.write_branch_ref(branch, state_id))
            }
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/refs/heads/{branch}"))?;
                let mut req = client.put(&url).body(state_id.clone());
                if force {
                    req = req.header("X-Astvcs-Force", "true");
                }
                let resp = req.send().map_err(|e| e.to_string())?;
                if resp.status().is_success() {
                    return Ok(());
                }
                let status = resp.status();
                let body = resp.text().unwrap_or_default();
                Err(format!("set ref {branch}: HTTP {status} {body}"))
            }
        }
    }

    pub fn default_branch(&self) -> Result<String, String> {
        match self {
            Self::File(repo) => Ok(map_repo(repo.load_config())?.default_branch),
            Self::Http { client, .. } => {
                let url = self.http_url("/config")?;
                let resp = client.get(&url).send().map_err(|e| e.to_string())?;
                if !resp.status().is_success() {
                    return Ok("main".into());
                }
                let config: crate::store::RepoConfig = resp.json().map_err(|e| e.to_string())?;
                Ok(config.default_branch)
            }
        }
    }
}
