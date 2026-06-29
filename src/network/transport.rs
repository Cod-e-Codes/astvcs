use crate::network::ssh::{SshSession, parse_ssh_remote};
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
    if crate::network::ssh::is_ssh_remote(url) {
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
        token: Option<String>,
    },
    Ssh(SshSession),
}

fn build_http_client(insecure: bool) -> Result<reqwest::blocking::Client, String> {
    let mut builder =
        reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(60));
    if insecure {
        builder = builder.danger_accept_invalid_certs(true);
    }
    builder.build().map_err(|e| e.to_string())
}

impl Transport {
    pub fn open(url: &str) -> Result<Self, String> {
        Self::open_with_options(url, None, false)
    }

    pub fn open_with_token(url: &str, token: Option<&str>) -> Result<Self, String> {
        Self::open_with_options(url, token, false)
    }

    pub fn open_with_options(
        url: &str,
        token: Option<&str>,
        insecure: bool,
    ) -> Result<Self, String> {
        if url.starts_with("http://") || url.starts_with("https://") {
            let base = url.trim_end_matches('/').to_string();
            let client = build_http_client(insecure)?;
            return Ok(Self::Http {
                base,
                client,
                token: token.map(str::to_string),
            });
        }
        if let Ok(target) = parse_ssh_remote(url) {
            let _ = insecure;
            let session = SshSession::connect(&target, token)?;
            return Ok(Self::Ssh(session));
        }
        let path = parse_remote_url(url)?;
        let repo = map_repo(Repo::open(&path))?;
        Ok(Self::File(repo))
    }

    fn http_url(&self, path: &str) -> Result<String, String> {
        match self {
            Self::Http { base, .. } => Ok(format!("{base}{API_PREFIX}{path}")),
            Self::File(_) | Self::Ssh(_) => Err("http url requested on non-http transport".into()),
        }
    }

    fn authorize_request(
        &self,
        req: reqwest::blocking::RequestBuilder,
    ) -> reqwest::blocking::RequestBuilder {
        match self {
            Self::Http {
                token: Some(token), ..
            } => req.header("Authorization", format!("Bearer {token}")),
            _ => req,
        }
    }

    pub fn has_blob(&self, id: &str) -> Result<bool, String> {
        match self {
            Self::File(repo) => Ok(repo.has_blob(id)),
            Self::Ssh(session) => session.has_blob(id),
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/blobs/{id}"))?;
                let resp = self
                    .authorize_request(client.head(&url))
                    .send()
                    .map_err(|e| e.to_string())?;
                Ok(resp.status().is_success())
            }
        }
    }

    pub fn get_blob(&self, id: &str) -> Result<Vec<u8>, String> {
        match self {
            Self::File(repo) => map_repo(repo.read_blob_bytes(id)),
            Self::Ssh(session) => session.get_blob(id),
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/blobs/{id}"))?;
                let resp = self
                    .authorize_request(client.get(&url))
                    .send()
                    .map_err(|e| e.to_string())?;
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
            Self::Ssh(session) => session.put_blob(id, bytes),
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/blobs/{id}"))?;
                let resp = self
                    .authorize_request(client.put(&url))
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
            Self::Ssh(session) => session.has_state(id),
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/states/{id}"))?;
                let resp = self
                    .authorize_request(client.head(&url))
                    .send()
                    .map_err(|e| e.to_string())?;
                Ok(resp.status().is_success())
            }
        }
    }

    pub fn get_state(&self, id: &StateId) -> Result<ManifestMap, String> {
        match self {
            Self::File(repo) => map_repo(repo.load_manifest(id)),
            Self::Ssh(session) => session.get_state(id),
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/states/{id}"))?;
                let resp = self
                    .authorize_request(client.get(&url))
                    .send()
                    .map_err(|e| e.to_string())?;
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
            Self::Ssh(session) => session.put_state(id, manifest),
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/states/{id}"))?;
                let resp = self
                    .authorize_request(client.put(&url))
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
            Self::Ssh(session) => session.has_timeline(id),
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/timeline/{id}"))?;
                let resp = self
                    .authorize_request(client.head(&url))
                    .send()
                    .map_err(|e| e.to_string())?;
                Ok(resp.status().is_success())
            }
        }
    }

    pub fn get_timeline(&self, id: &StateId) -> Result<TimelineEntry, String> {
        match self {
            Self::File(repo) => map_repo(repo.load_timeline_entry(id)),
            Self::Ssh(session) => session.get_timeline(id),
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/timeline/{id}"))?;
                let resp = self
                    .authorize_request(client.get(&url))
                    .send()
                    .map_err(|e| e.to_string())?;
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
            Self::Ssh(session) => session.put_timeline(entry),
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/timeline/{}", entry.id))?;
                let resp = self
                    .authorize_request(client.put(&url))
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
            Self::Ssh(session) => session.list_refs(),
            Self::Http { client, .. } => {
                let url = self.http_url("/refs/heads")?;
                let resp = self
                    .authorize_request(client.get(&url))
                    .send()
                    .map_err(|e| e.to_string())?;
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
            Self::Ssh(session) => session.get_ref(branch),
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/refs/heads/{branch}"))?;
                let resp = self
                    .authorize_request(client.get(&url))
                    .send()
                    .map_err(|e| e.to_string())?;
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
            Self::Ssh(session) => session.set_ref(branch, state_id, force),
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/refs/heads/{branch}"))?;
                let mut req = self
                    .authorize_request(client.put(&url))
                    .body(state_id.clone());
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
            Self::Ssh(session) => session.default_branch(),
            Self::Http { client, .. } => {
                let url = self.http_url("/config")?;
                let resp = self
                    .authorize_request(client.get(&url))
                    .send()
                    .map_err(|e| e.to_string())?;
                if !resp.status().is_success() {
                    return Ok("main".into());
                }
                let config: crate::store::RepoConfig = resp.json().map_err(|e| e.to_string())?;
                Ok(config.default_branch)
            }
        }
    }

    pub fn list_tags(&self) -> Result<HashMap<String, StateId>, String> {
        match self {
            Self::File(repo) => {
                let mut out = HashMap::new();
                for tag in map_repo(repo.list_tags())? {
                    out.insert(tag.name, tag.state_id);
                }
                Ok(out)
            }
            Self::Ssh(session) => session.list_tags(),
            Self::Http { client, .. } => {
                let url = self.http_url("/refs/tags")?;
                let resp = self
                    .authorize_request(client.get(&url))
                    .send()
                    .map_err(|e| e.to_string())?;
                if !resp.status().is_success() {
                    return Err(format!("list tags: HTTP {}", resp.status()));
                }
                resp.json().map_err(|e| e.to_string())
            }
        }
    }

    pub fn get_tag(&self, name: &str) -> Result<Option<StateId>, String> {
        match self {
            Self::File(repo) => {
                let path = repo.astvcs_dir().join("refs/tags").join(name);
                if !path.is_file() {
                    return Ok(None);
                }
                Ok(Some(map_repo(repo.read_tag(name))?))
            }
            Self::Ssh(session) => session.get_tag(name),
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/refs/tags/{name}"))?;
                let resp = self
                    .authorize_request(client.get(&url))
                    .send()
                    .map_err(|e| e.to_string())?;
                if resp.status().as_u16() == 404 {
                    return Ok(None);
                }
                if !resp.status().is_success() {
                    return Err(format!("get tag {name}: HTTP {}", resp.status()));
                }
                let text = resp.text().map_err(|e| e.to_string())?;
                Ok(Some(text.trim().to_string()))
            }
        }
    }

    pub fn set_tag(&self, name: &str, state_id: &StateId) -> Result<(), String> {
        match self {
            Self::File(repo) => map_repo(repo.write_tag(name, state_id)),
            Self::Ssh(session) => session.set_tag(name, state_id),
            Self::Http { client, .. } => {
                let url = self.http_url(&format!("/refs/tags/{name}"))?;
                let resp = self
                    .authorize_request(client.put(&url))
                    .body(state_id.clone())
                    .send()
                    .map_err(|e| e.to_string())?;
                if resp.status().is_success() {
                    return Ok(());
                }
                let status = resp.status();
                let body = resp.text().unwrap_or_default();
                Err(format!("set tag {name}: HTTP {status} {body}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    #[test]
    fn parse_remote_url_accepts_ssh() {
        let url = "user@host.example:/srv/repo";
        let parsed = parse_remote_url(url).unwrap();
        assert_eq!(parsed, PathBuf::from(url));
    }

    #[test]
    fn parse_remote_url_accepts_https() {
        let url = "https://example.com:9443/repo";
        let parsed = parse_remote_url(url).unwrap();
        assert_eq!(parsed, PathBuf::from(url));
    }

    #[test]
    fn insecure_client_accepts_self_signed_cert() {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let certificate = cert.cert.pem().into_bytes();
        let private_key = cert.signing_key.serialize_pem().into_bytes();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let ssl_config = tiny_http::SslConfig {
            certificate,
            private_key,
        };
        let server = tiny_http::Server::from_listener(listener, Some(ssl_config)).unwrap();

        let handle = thread::spawn(move || {
            if let Some(request) = server.incoming_requests().next() {
                let response = tiny_http::Response::from_string("{}").with_status_code(200);
                let _ = request.respond(response);
            }
        });

        let base = format!("https://127.0.0.1:{port}");
        let strict = Transport::open_with_options(&base, None, false);
        assert!(strict.is_ok());
        assert!(strict.unwrap().list_refs().is_err());

        let transport = Transport::open_with_options(&base, None, true).expect("open insecure");
        transport
            .list_refs()
            .expect("fetch refs over self-signed https");

        let _ = handle.join();
    }

    #[test]
    fn http_transport_sends_bearer_token() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tiny_http::Server::from_listener(listener, None).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_flag = Arc::clone(&seen);

        let handle = thread::spawn(move || {
            if let Some(request) = server.incoming_requests().next() {
                let auth = request
                    .headers()
                    .iter()
                    .find(|h| {
                        h.field
                            .as_str()
                            .as_str()
                            .eq_ignore_ascii_case("authorization")
                    })
                    .map(|h| h.value.as_str().to_string());
                seen_flag.lock().unwrap().push(auth);
                let _ =
                    request.respond(tiny_http::Response::from_string("ok").with_status_code(200));
            }
        });

        let base = format!("http://127.0.0.1:{port}");
        let transport =
            Transport::open_with_token(&base, Some("client-secret")).expect("open transport");
        transport.list_refs().ok();

        let _ = handle.join();
        let auths = seen.lock().unwrap();
        assert_eq!(auths.len(), 1);
        assert_eq!(auths[0].as_deref(), Some("Bearer client-secret"));
    }
}
