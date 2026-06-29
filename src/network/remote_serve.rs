use crate::network::api::{ApiRequest, AuthOptions, dispatch, parse_query_string};
use crate::network::ssh::api_response_to_remote;
use crate::store::Repo;
use base64::{Engine, engine::general_purpose::STANDARD};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct ServeAuthOptions {
    pub token: Option<String>,
    pub public_read: bool,
}

#[derive(serde::Deserialize, serde::Serialize)]
pub struct RemoteRequest {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct RemoteResponse {
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn remote_serve_repo(repo_path: &Path, auth: ServeAuthOptions) -> Result<(), String> {
    let token = auth
        .token
        .or_else(|| std::env::var("ASTVCS_SERVE_TOKEN").ok());
    let auth = ServeAuthOptions {
        token,
        public_read: auth.public_read,
    };
    let repo = Repo::open(repo_path).map_err(|e| e.to_string())?;
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    remote_serve_io(&repo, &auth, &mut stdin.lock(), &mut stdout.lock())
}

pub fn remote_serve_io<R: BufRead, W: Write>(
    repo: &Repo,
    auth: &ServeAuthOptions,
    reader: &mut R,
    writer: &mut W,
) -> Result<(), String> {
    let mut buf = String::new();
    while reader.read_line(&mut buf).map_err(|e| e.to_string())? > 0 {
        let line = buf.trim();
        if !line.is_empty() {
            let request: RemoteRequest = serde_json::from_str(line).map_err(|e| e.to_string())?;
            let response = remote_request(repo, &request, auth);
            writeln!(
                writer,
                "{}",
                serde_json::to_string(&response).map_err(|e| e.to_string())?
            )
            .map_err(|e| e.to_string())?;
            writer.flush().map_err(|e| e.to_string())?;
        }
        buf.clear();
    }
    Ok(())
}

pub fn remote_request(
    repo: &Repo,
    request: &RemoteRequest,
    auth: &ServeAuthOptions,
) -> RemoteResponse {
    let body = match &request.body {
        Some(encoded) => STANDARD.decode(encoded).unwrap_or_default(),
        None => Vec::new(),
    };
    let (path, query) = match request.path.split_once('?') {
        Some((path, query)) => (path.to_string(), parse_query_string(query)),
        None => (request.path.clone(), std::collections::HashMap::new()),
    };
    let api_request = ApiRequest {
        method: request.method.clone(),
        path,
        query,
        body,
        headers: request.headers.clone(),
    };
    let auth_options = AuthOptions {
        token: auth.token.clone(),
        public_read: auth.public_read,
    };
    let api_response = dispatch(repo, &api_request, &auth_options);
    api_response_to_remote(api_response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use std::collections::HashMap;
    use std::io::Cursor;
    use tempfile::TempDir;

    fn init_repo() -> (TempDir, Repo) {
        let dir = TempDir::new().unwrap();
        let repo = Repo::init_with_identity(dir.path()).unwrap();
        (dir, repo)
    }

    #[test]
    fn remote_request_requires_token_when_configured() {
        let (_dir, repo) = init_repo();
        let auth = ServeAuthOptions {
            token: Some("secret".into()),
            public_read: false,
        };
        let request = RemoteRequest {
            method: "GET".into(),
            path: "/v1/config".into(),
            body: None,
            headers: HashMap::new(),
        };
        let resp = remote_request(&repo, &request, &auth);
        assert_eq!(resp.status, 401);

        let mut headers = HashMap::new();
        headers.insert("Authorization".into(), "Bearer secret".into());
        let authed = RemoteRequest { headers, ..request };
        let resp = remote_request(&repo, &authed, &auth);
        assert_eq!(resp.status, 200);
    }

    #[test]
    fn remote_serve_io_get_config_put_blob_head_404() {
        use sha2::{Digest, Sha256};

        let (_dir, repo) = init_repo();
        let payload = b"payload";
        let blob_id = hex::encode(Sha256::digest(payload));
        let input = format!(
            "{}\n{}\n{}\n",
            serde_json::to_string(&RemoteRequest {
                method: "GET".into(),
                path: "/v1/config".into(),
                body: None,
                headers: HashMap::new(),
            })
            .unwrap(),
            serde_json::to_string(&RemoteRequest {
                method: "PUT".into(),
                path: format!("/v1/blobs/{blob_id}"),
                body: Some(STANDARD.encode(payload)),
                headers: HashMap::new(),
            })
            .unwrap(),
            serde_json::to_string(&RemoteRequest {
                method: "HEAD".into(),
                path: "/v1/blobs/missing".into(),
                body: None,
                headers: HashMap::new(),
            })
            .unwrap(),
        );
        let mut output = Vec::new();
        remote_serve_io(
            &repo,
            &ServeAuthOptions::default(),
            &mut Cursor::new(input),
            &mut output,
        )
        .unwrap();
        let lines: Vec<&str> = std::str::from_utf8(&output)
            .unwrap()
            .lines()
            .filter(|l| !l.is_empty())
            .collect();
        assert_eq!(lines.len(), 3);
        let r0: RemoteResponse = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(r0.status, 200);
        let r1: RemoteResponse = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(r1.status, 201);
        let r2: RemoteResponse = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(r2.status, 404);
    }
}
