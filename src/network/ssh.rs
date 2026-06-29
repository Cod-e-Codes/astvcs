use crate::network::api::{API_PREFIX, ApiResponse};
use crate::network::remote_serve::{RemoteRequest, RemoteResponse};
use crate::store::{ManifestMap, StateId, TimelineEntry};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshTarget {
    pub user: Option<String>,
    pub host: String,
    pub repo_path: String,
}

pub fn parse_ssh_remote(url: &str) -> Result<SshTarget, String> {
    if let Some(rest) = url.strip_prefix("ssh://") {
        return parse_ssh_scheme(rest);
    }
    parse_scp_style(url)
}

pub fn is_ssh_remote(url: &str) -> bool {
    parse_ssh_remote(url).is_ok()
}

fn parse_ssh_scheme(rest: &str) -> Result<SshTarget, String> {
    let (authority, path) = rest
        .split_once('/')
        .ok_or_else(|| format!("ssh url missing repository path: ssh://{rest}"))?;
    if path.is_empty() {
        return Err(format!("ssh url missing repository path: ssh://{rest}"));
    }
    let repo_path = format!("/{path}");
    let (user, host) = split_user_host(authority)?;
    Ok(SshTarget {
        user,
        host,
        repo_path,
    })
}

fn parse_scp_style(url: &str) -> Result<SshTarget, String> {
    let colon = url
        .find(':')
        .ok_or_else(|| format!("not an ssh remote url: {url}"))?;
    let host_part = &url[..colon];
    let path_part = &url[colon + 1..];
    if host_part.is_empty() || path_part.is_empty() {
        return Err(format!("not an ssh remote url: {url}"));
    }
    if !host_part.contains('@') {
        return Err(format!("ambiguous scp-style url (missing user@): {url}"));
    }
    if path_part.contains('\\') {
        return Err(format!(
            "scp-style ssh path must be absolute on remote: {url}"
        ));
    }
    if !path_part.starts_with('/') {
        return Err(format!(
            "scp-style ssh path must be absolute on remote: {url}"
        ));
    }
    let (user, host) = split_user_host(host_part)?;
    Ok(SshTarget {
        user,
        host,
        repo_path: path_part.to_string(),
    })
}

fn split_user_host(authority: &str) -> Result<(Option<String>, String), String> {
    if let Some((user, host)) = authority.rsplit_once('@') {
        if user.is_empty() || host.is_empty() {
            return Err(format!("invalid ssh authority: {authority}"));
        }
        Ok((Some(user.to_string()), host.to_string()))
    } else if authority.is_empty() {
        Err(format!("invalid ssh authority: {authority}"))
    } else {
        Ok((None, authority.to_string()))
    }
}

pub struct SshSession {
    stdin: Mutex<Box<dyn Write + Send>>,
    stdout: Mutex<Box<dyn BufRead + Send>>,
    token: Option<String>,
    _child: Mutex<Child>,
}

impl SshSession {
    pub fn connect(target: &SshTarget, token: Option<&str>) -> Result<Self, String> {
        let mut child = spawn_ssh(target)?;
        let stdin = child.stdin.take().ok_or("ssh stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("ssh stdout unavailable")?;
        Ok(Self {
            stdin: Mutex::new(Box::new(BufWriter::new(stdin))),
            stdout: Mutex::new(Box::new(BufReader::new(stdout))),
            token: token.map(str::to_string),
            _child: Mutex::new(child),
        })
    }

    #[cfg(test)]
    pub fn from_io(
        stdin: impl Write + Send + 'static,
        stdout: impl BufRead + Send + 'static,
        token: Option<String>,
    ) -> Self {
        Self {
            stdin: Mutex::new(Box::new(BufWriter::new(stdin))),
            stdout: Mutex::new(Box::new(stdout)),
            token,
            _child: Mutex::new(spawn_placeholder_child()),
        }
    }

    pub fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
        extra_headers: HashMap<String, String>,
    ) -> Result<RemoteResponse, String> {
        let mut headers = extra_headers;
        if let Some(token) = &self.token {
            headers.insert("Authorization".into(), format!("Bearer {token}"));
        }
        let req = RemoteRequest {
            method: method.to_string(),
            path: path.to_string(),
            body: body.map(|b| STANDARD.encode(b)),
            headers,
        };
        let mut stdin = self.stdin.lock().map_err(|e| e.to_string())?;
        let line = serde_json::to_string(&req).map_err(|e| e.to_string())?;
        stdin
            .write_all(line.as_bytes())
            .and_then(|_| stdin.write_all(b"\n"))
            .and_then(|_| stdin.flush())
            .map_err(|e| e.to_string())?;

        let mut stdout = self.stdout.lock().map_err(|e| e.to_string())?;
        let mut response_line = String::new();
        stdout
            .read_line(&mut response_line)
            .map_err(|e| e.to_string())?;
        if response_line.trim().is_empty() {
            return Err("empty response from remote-serve".into());
        }
        serde_json::from_str(&response_line).map_err(|e| e.to_string())
    }

    fn api_path(&self, suffix: &str) -> String {
        format!("{API_PREFIX}{suffix}")
    }

    pub fn has_blob(&self, id: &str) -> Result<bool, String> {
        let resp = self.request(
            "HEAD",
            &self.api_path(&format!("/blobs/{id}")),
            None,
            HashMap::new(),
        )?;
        Ok(resp.status == 200)
    }

    pub fn get_blob(&self, id: &str) -> Result<Vec<u8>, String> {
        let resp = self.request(
            "GET",
            &self.api_path(&format!("/blobs/{id}")),
            None,
            HashMap::new(),
        )?;
        decode_response_body(&resp, &format!("blob {id}"))
    }

    pub fn put_blob(&self, id: &str, bytes: &[u8]) -> Result<(), String> {
        let resp = self.request(
            "PUT",
            &self.api_path(&format!("/blobs/{id}")),
            Some(bytes),
            HashMap::new(),
        )?;
        if resp.status == 200 || resp.status == 201 || resp.status == 409 {
            return Ok(());
        }
        Err(remote_error(&resp, &format!("put blob {id}")))
    }

    pub fn has_state(&self, id: &StateId) -> Result<bool, String> {
        let resp = self.request(
            "HEAD",
            &self.api_path(&format!("/states/{id}")),
            None,
            HashMap::new(),
        )?;
        Ok(resp.status == 200)
    }

    pub fn get_state(&self, id: &StateId) -> Result<ManifestMap, String> {
        let resp = self.request(
            "GET",
            &self.api_path(&format!("/states/{id}")),
            None,
            HashMap::new(),
        )?;
        let bytes = decode_response_body(&resp, &format!("state {id}"))?;
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    }

    pub fn put_state(&self, id: &StateId, manifest: &ManifestMap) -> Result<(), String> {
        let body = serde_json::to_vec(manifest).map_err(|e| e.to_string())?;
        let resp = self.request(
            "PUT",
            &self.api_path(&format!("/states/{id}")),
            Some(&body),
            HashMap::new(),
        )?;
        if resp.status == 200 || resp.status == 201 || resp.status == 409 {
            return Ok(());
        }
        Err(remote_error(&resp, &format!("put state {id}")))
    }

    pub fn has_timeline(&self, id: &StateId) -> Result<bool, String> {
        let resp = self.request(
            "HEAD",
            &self.api_path(&format!("/timeline/{id}")),
            None,
            HashMap::new(),
        )?;
        Ok(resp.status == 200)
    }

    pub fn get_timeline(&self, id: &StateId) -> Result<TimelineEntry, String> {
        let resp = self.request(
            "GET",
            &self.api_path(&format!("/timeline/{id}")),
            None,
            HashMap::new(),
        )?;
        let bytes = decode_response_body(&resp, &format!("timeline {id}"))?;
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    }

    pub fn put_timeline(&self, entry: &TimelineEntry) -> Result<(), String> {
        let body = serde_json::to_vec(entry).map_err(|e| e.to_string())?;
        let resp = self.request(
            "PUT",
            &self.api_path(&format!("/timeline/{}", entry.id)),
            Some(&body),
            HashMap::new(),
        )?;
        if resp.status == 200 || resp.status == 201 || resp.status == 409 {
            return Ok(());
        }
        Err(remote_error(&resp, &format!("put timeline {}", entry.id)))
    }

    pub fn list_refs(&self) -> Result<HashMap<String, StateId>, String> {
        let resp = self.request("GET", &self.api_path("/refs/heads"), None, HashMap::new())?;
        let bytes = decode_response_body(&resp, "list refs")?;
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    }

    pub fn get_ref(&self, branch: &str) -> Result<Option<StateId>, String> {
        let resp = self.request(
            "GET",
            &self.api_path(&format!("/refs/heads/{branch}")),
            None,
            HashMap::new(),
        )?;
        if resp.status == 404 {
            return Ok(None);
        }
        let bytes = decode_response_body(&resp, &format!("get ref {branch}"))?;
        Ok(Some(
            std::str::from_utf8(&bytes)
                .map_err(|e| e.to_string())?
                .trim()
                .to_string(),
        ))
    }

    pub fn set_ref(&self, branch: &str, state_id: &StateId, force: bool) -> Result<(), String> {
        let mut headers = HashMap::new();
        if force {
            headers.insert("X-Astvcs-Force".into(), "true".into());
        }
        let resp = self.request(
            "PUT",
            &self.api_path(&format!("/refs/heads/{branch}")),
            Some(state_id.as_bytes()),
            headers,
        )?;
        if resp.status == 200 {
            return Ok(());
        }
        Err(remote_error(&resp, &format!("set ref {branch}")))
    }

    pub fn default_branch(&self) -> Result<String, String> {
        let resp = self.request("GET", &self.api_path("/config"), None, HashMap::new())?;
        if resp.status != 200 {
            return Ok("main".into());
        }
        let bytes = decode_response_body(&resp, "config")?;
        let config: crate::store::RepoConfig =
            serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        Ok(config.default_branch)
    }

    pub fn list_tags(&self) -> Result<HashMap<String, StateId>, String> {
        let resp = self.request("GET", &self.api_path("/refs/tags"), None, HashMap::new())?;
        let bytes = decode_response_body(&resp, "list tags")?;
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    }

    pub fn get_tag(&self, name: &str) -> Result<Option<StateId>, String> {
        let resp = self.request(
            "GET",
            &self.api_path(&format!("/refs/tags/{name}")),
            None,
            HashMap::new(),
        )?;
        if resp.status == 404 {
            return Ok(None);
        }
        let bytes = decode_response_body(&resp, &format!("get tag {name}"))?;
        Ok(Some(
            std::str::from_utf8(&bytes)
                .map_err(|e| e.to_string())?
                .trim()
                .to_string(),
        ))
    }

    pub fn set_tag(&self, name: &str, state_id: &StateId) -> Result<(), String> {
        let resp = self.request(
            "PUT",
            &self.api_path(&format!("/refs/tags/{name}")),
            Some(state_id.as_bytes()),
            HashMap::new(),
        )?;
        if resp.status == 200 {
            return Ok(());
        }
        Err(remote_error(&resp, &format!("set tag {name}")))
    }
}

fn decode_response_body(resp: &RemoteResponse, context: &str) -> Result<Vec<u8>, String> {
    if resp.status < 200 || resp.status >= 300 {
        return Err(remote_error(resp, context));
    }
    match &resp.body {
        Some(encoded) => STANDARD.decode(encoded).map_err(|e| e.to_string()),
        None => Ok(Vec::new()),
    }
}

fn remote_error(resp: &RemoteResponse, context: &str) -> String {
    if let Some(err) = &resp.error {
        format!("{context}: remote error ({}) {err}", resp.status)
    } else {
        format!("{context}: remote status {}", resp.status)
    }
}

fn spawn_ssh(target: &SshTarget) -> Result<Child, String> {
    let ssh_target = match &target.user {
        Some(user) => format!("{user}@{}", target.host),
        None => target.host.clone(),
    };
    let remote_cmd = format!(
        "astvcs remote-serve --repo {}",
        shell_quote(&target.repo_path)
    );
    Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=accept-new",
            &ssh_target,
            &remote_cmd,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("failed to spawn ssh: {e}"))
}

fn shell_quote(path: &str) -> String {
    if path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '/' || c == '_' || c == '-' || c == '.')
    {
        path.to_string()
    } else {
        format!("'{}'", path.replace('\'', "'\\''"))
    }
}

#[cfg(all(test, unix))]
mod unix_ssh_integration {
    use super::*;
    use crate::store::Repo;
    use std::process::{Command, Stdio};
    use tempfile::TempDir;

    #[test]
    fn ssh_localhost_remote_serve_optional() {
        if Command::new("ssh")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("ConnectTimeout=2")
            .arg("localhost")
            .arg("true")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_err()
        {
            return;
        }
        let status = Command::new("ssh")
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=2",
                "localhost",
                "true",
            ])
            .status()
            .expect("ssh status");
        if !status.success() {
            return;
        }

        let dir = TempDir::new().unwrap();
        let _repo = Repo::init_with_identity(dir.path()).unwrap();
        let repo_path = dir.path().to_string_lossy().replace('\\', "/");
        let target = parse_ssh_remote(&format!("ssh://localhost{repo_path}")).unwrap();
        let session = match SshSession::connect(&target, None) {
            Ok(session) => session,
            Err(_) => return,
        };
        let resp = session
            .request("GET", "/v1/config", None, std::collections::HashMap::new())
            .expect("config over ssh");
        assert_eq!(resp.status, 200);
    }
}

#[cfg(test)]
fn spawn_placeholder_child() -> Child {
    if cfg!(windows) {
        Command::new("cmd")
            .args(["/C", "exit", "0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    } else {
        Command::new("true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }
}

pub fn api_response_to_remote(resp: ApiResponse) -> RemoteResponse {
    if resp.status >= 400 {
        let error = String::from_utf8_lossy(&resp.body).into_owned();
        RemoteResponse {
            status: resp.status,
            body: None,
            error: Some(error),
        }
    } else if resp.body.is_empty() {
        RemoteResponse {
            status: resp.status,
            body: None,
            error: None,
        }
    } else {
        RemoteResponse {
            status: resp.status,
            body: Some(STANDARD.encode(&resp.body)),
            error: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::remote_serve::RemoteRequest;
    use std::io::{BufReader, Cursor, Write};
    use std::sync::Arc;

    #[test]
    fn parse_ssh_scheme_url() {
        let target = parse_ssh_remote("ssh://alice@example.com/var/repos/demo").unwrap();
        assert_eq!(target.user.as_deref(), Some("alice"));
        assert_eq!(target.host, "example.com");
        assert_eq!(target.repo_path, "/var/repos/demo");
    }

    #[test]
    fn parse_ssh_scheme_without_user() {
        let target = parse_ssh_remote("ssh://example.com/home/repo").unwrap();
        assert!(target.user.is_none());
        assert_eq!(target.host, "example.com");
        assert_eq!(target.repo_path, "/home/repo");
    }

    #[test]
    fn parse_scp_style_url() {
        let target = parse_ssh_remote("bob@host.example:/srv/astvcs.git").unwrap();
        assert_eq!(target.user.as_deref(), Some("bob"));
        assert_eq!(target.host, "host.example");
        assert_eq!(target.repo_path, "/srv/astvcs.git");
    }

    #[test]
    fn reject_scp_style_without_user() {
        assert!(parse_ssh_remote("host.example:/srv/repo").is_err());
    }

    #[test]
    fn reject_scp_style_relative_path() {
        assert!(parse_ssh_remote("user@host:repo").is_err());
    }

    #[test]
    fn reject_non_ssh_urls() {
        assert!(parse_ssh_remote("https://example.com/repo").is_err());
        assert!(parse_ssh_remote("/local/path").is_err());
    }

    #[test]
    fn ssh_session_sends_bearer_token() {
        let captured = Arc::new(Mutex::new(String::new()));
        let session = SshSession::from_io(
            CaptureWriter(Arc::clone(&captured)),
            BufReader::new(Cursor::new(b"{\"status\":200,\"body\":null}\n".to_vec())),
            Some("ssh-secret".into()),
        );
        session
            .request("GET", "/v1/config", None, HashMap::new())
            .unwrap();
        let lines = captured.lock().unwrap();
        let req: RemoteRequest = serde_json::from_str(lines.trim()).unwrap();
        assert_eq!(
            req.headers.get("Authorization").map(String::as_str),
            Some("Bearer ssh-secret")
        );
    }

    struct CaptureWriter(Arc<Mutex<String>>);

    impl Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(buf));
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
