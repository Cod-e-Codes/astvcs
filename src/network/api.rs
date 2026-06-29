use crate::store::{Repo, RepoError, TimelineEntry};
use std::collections::HashMap;
use subtle::ConstantTimeEq;

fn map_repo<T>(result: Result<T, RepoError>) -> Result<T, String> {
    result.map_err(|e| e.to_string())
}

pub const API_PREFIX: &str = "/v1";

#[derive(Clone, Debug, Default)]
pub struct AuthOptions {
    pub token: Option<String>,
    pub public_read: bool,
}

#[derive(Clone, Debug)]
pub struct ApiRequest {
    pub method: String,
    pub path: String,
    pub body: Vec<u8>,
    pub headers: HashMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct ApiResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl ApiRequest {
    pub fn force(&self) -> bool {
        self.headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("X-Astvcs-Force") && v == "true")
    }

    pub fn bearer_token(&self) -> Option<&str> {
        for (key, value) in &self.headers {
            if !key.eq_ignore_ascii_case("authorization") {
                continue;
            }
            if value.len() < 7 {
                continue;
            }
            if !value[..7].eq_ignore_ascii_case("bearer ") {
                continue;
            }
            return Some(&value[7..]);
        }
        None
    }
}

pub fn dispatch(repo: &Repo, request: &ApiRequest, auth: &AuthOptions) -> ApiResponse {
    if let Some(resp) = check_auth(auth, request) {
        return resp;
    }

    let method = request.method.to_uppercase();
    let path = request.path.as_str();
    let force = request.force();
    let body = &request.body;

    if path == format!("{API_PREFIX}/config") && method == "GET" {
        return match map_repo(repo.load_config()) {
            Ok(config) => match serde_json::to_vec(&config) {
                Ok(bytes) => api_response(200, bytes),
                Err(e) => api_response(500, e.to_string().into_bytes()),
            },
            Err(e) => api_response(500, e.into_bytes()),
        };
    }
    if path == format!("{API_PREFIX}/refs/heads") && method == "GET" {
        let mut refs = HashMap::new();
        return match map_repo(repo.list_branches()) {
            Ok(branches) => {
                for branch in branches {
                    refs.insert(branch.name, branch.state_id);
                }
                match serde_json::to_vec(&refs) {
                    Ok(bytes) => api_response(200, bytes),
                    Err(e) => api_response(500, e.to_string().into_bytes()),
                }
            }
            Err(e) => api_response(500, e.into_bytes()),
        };
    }
    if path == format!("{API_PREFIX}/refs/tags") && method == "GET" {
        let mut refs = HashMap::new();
        return match map_repo(repo.list_tags()) {
            Ok(tags) => {
                for tag in tags {
                    refs.insert(tag.name, tag.state_id);
                }
                match serde_json::to_vec(&refs) {
                    Ok(bytes) => api_response(200, bytes),
                    Err(e) => api_response(500, e.to_string().into_bytes()),
                }
            }
            Err(e) => api_response(500, e.into_bytes()),
        };
    }
    if let Some(branch) = path.strip_prefix(&format!("{API_PREFIX}/refs/heads/")) {
        return handle_ref(repo, &method, branch, body, force);
    }
    if let Some(name) = path.strip_prefix(&format!("{API_PREFIX}/refs/tags/")) {
        return handle_tag(repo, &method, name, body);
    }
    if let Some(id) = path.strip_prefix(&format!("{API_PREFIX}/blobs/")) {
        return handle_blob(repo, &method, id, body);
    }
    if let Some(id) = path.strip_prefix(&format!("{API_PREFIX}/states/")) {
        return handle_state(repo, &method, id, body);
    }
    if let Some(id) = path.strip_prefix(&format!("{API_PREFIX}/timeline/")) {
        return handle_timeline(repo, &method, id, body);
    }
    api_response(404, b"not found".to_vec())
}

fn check_auth(auth: &AuthOptions, request: &ApiRequest) -> Option<ApiResponse> {
    let expected = auth.token.as_deref()?;
    if !request.path.starts_with(API_PREFIX) {
        return None;
    }

    let method = request.method.to_uppercase();
    let is_read = method == "GET" || method == "HEAD";
    if is_read && auth.public_read {
        return None;
    }

    let authorized = request
        .bearer_token()
        .is_some_and(|p| token_matches(expected, p));
    if authorized {
        None
    } else {
        Some(api_response(401, b"unauthorized".to_vec()))
    }
}

fn token_matches(expected: &str, provided: &str) -> bool {
    expected.as_bytes().ct_eq(provided.as_bytes()).into()
}

fn handle_ref(repo: &Repo, method: &str, branch: &str, body: &[u8], force: bool) -> ApiResponse {
    match method {
        "GET" => {
            let path = repo.astvcs_dir().join("refs/heads").join(branch);
            if !path.is_file() {
                return api_response(404, b"branch not found".to_vec());
            }
            match map_repo(repo.branch_state(branch)) {
                Ok(state_id) => api_response(200, format!("{state_id}\n").into_bytes()),
                Err(e) => api_response(500, e.into_bytes()),
            }
        }
        "PUT" => {
            let state_id = match std::str::from_utf8(body) {
                Ok(text) => text.trim().to_string(),
                Err(e) => return api_response(400, e.to_string().into_bytes()),
            };
            if !force
                && let Ok(current) = map_repo(repo.branch_state(branch))
                && current != state_id
                && !map_repo(repo.is_ancestor_of(&current, &state_id)).unwrap_or(false)
            {
                return api_response(409, b"non-fast-forward update rejected".to_vec());
            }
            match map_repo(repo.write_branch_ref(branch, &state_id)) {
                Ok(()) => api_response(200, b"ok".to_vec()),
                Err(e) => api_response(500, e.into_bytes()),
            }
        }
        "HEAD" => {
            let path = repo.astvcs_dir().join("refs/heads").join(branch);
            let status = if path.is_file() { 200 } else { 404 };
            api_response(status, Vec::new())
        }
        _ => api_response(405, b"method not allowed".to_vec()),
    }
}

fn handle_tag(repo: &Repo, method: &str, name: &str, body: &[u8]) -> ApiResponse {
    match method {
        "GET" => {
            let path = repo.astvcs_dir().join("refs/tags").join(name);
            if !path.is_file() {
                return api_response(404, b"tag not found".to_vec());
            }
            match map_repo(repo.read_tag(name)) {
                Ok(state_id) => api_response(200, format!("{state_id}\n").into_bytes()),
                Err(e) => api_response(500, e.into_bytes()),
            }
        }
        "PUT" => {
            let state_id = match std::str::from_utf8(body) {
                Ok(text) => text.trim().to_string(),
                Err(e) => return api_response(400, e.to_string().into_bytes()),
            };
            match map_repo(repo.write_tag(name, &state_id)) {
                Ok(()) => api_response(200, b"ok".to_vec()),
                Err(e) => api_response(500, e.into_bytes()),
            }
        }
        "HEAD" => {
            let path = repo.astvcs_dir().join("refs/tags").join(name);
            let status = if path.is_file() { 200 } else { 404 };
            api_response(status, Vec::new())
        }
        _ => api_response(405, b"method not allowed".to_vec()),
    }
}

fn handle_blob(repo: &Repo, method: &str, id: &str, body: &[u8]) -> ApiResponse {
    match method {
        "GET" => {
            if !repo.has_blob(id) {
                return api_response(404, b"blob not found".to_vec());
            }
            match map_repo(repo.read_blob_bytes(id)) {
                Ok(bytes) => api_response(200, bytes),
                Err(e) => api_response(500, e.into_bytes()),
            }
        }
        "PUT" => {
            if repo.has_blob(id) {
                return api_response(409, b"blob already exists".to_vec());
            }
            match map_repo(repo.import_blob_bytes(id, body)) {
                Ok(()) => api_response(201, b"created".to_vec()),
                Err(e) => api_response(500, e.into_bytes()),
            }
        }
        "HEAD" => {
            let status = if repo.has_blob(id) { 200 } else { 404 };
            api_response(status, Vec::new())
        }
        _ => api_response(405, b"method not allowed".to_vec()),
    }
}

fn handle_state(repo: &Repo, method: &str, id: &str, body: &[u8]) -> ApiResponse {
    let state_id = id.to_string();
    match method {
        "GET" => {
            if !repo.has_state(&state_id) {
                return api_response(404, b"state not found".to_vec());
            }
            match map_repo(repo.load_manifest(&state_id)) {
                Ok(manifest) => match serde_json::to_vec(&manifest) {
                    Ok(bytes) => api_response(200, bytes),
                    Err(e) => api_response(500, e.to_string().into_bytes()),
                },
                Err(e) => api_response(500, e.into_bytes()),
            }
        }
        "PUT" => {
            if repo.has_state(&state_id) {
                return api_response(409, b"state already exists".to_vec());
            }
            let manifest: crate::store::ManifestMap = match serde_json::from_slice(body) {
                Ok(m) => m,
                Err(e) => return api_response(400, e.to_string().into_bytes()),
            };
            match map_repo(repo.import_state_manifest(&state_id, &manifest)) {
                Ok(()) => api_response(201, b"created".to_vec()),
                Err(e) => api_response(500, e.into_bytes()),
            }
        }
        "HEAD" => {
            let status = if repo.has_state(&state_id) { 200 } else { 404 };
            api_response(status, Vec::new())
        }
        _ => api_response(405, b"method not allowed".to_vec()),
    }
}

fn handle_timeline(repo: &Repo, method: &str, id: &str, body: &[u8]) -> ApiResponse {
    let state_id = id.to_string();
    match method {
        "GET" => {
            if !repo.has_timeline(&state_id) {
                return api_response(404, b"timeline entry not found".to_vec());
            }
            match map_repo(repo.load_timeline_entry(&state_id)) {
                Ok(entry) => match serde_json::to_vec(&entry) {
                    Ok(bytes) => api_response(200, bytes),
                    Err(e) => api_response(500, e.to_string().into_bytes()),
                },
                Err(e) => api_response(500, e.into_bytes()),
            }
        }
        "PUT" => {
            if repo.has_timeline(&state_id) {
                return api_response(409, b"timeline entry already exists".to_vec());
            }
            let entry: TimelineEntry = match serde_json::from_slice(body) {
                Ok(e) => e,
                Err(e) => return api_response(400, e.to_string().into_bytes()),
            };
            if entry.id != state_id {
                return api_response(400, b"timeline id mismatch".to_vec());
            }
            match map_repo(repo.import_timeline_entry(&entry)) {
                Ok(()) => api_response(201, b"created".to_vec()),
                Err(e) => api_response(500, e.into_bytes()),
            }
        }
        "HEAD" => {
            let status = if repo.has_timeline(&state_id) {
                200
            } else {
                404
            };
            api_response(status, Vec::new())
        }
        _ => api_response(405, b"method not allowed".to_vec()),
    }
}

fn api_response(status: u16, body: Vec<u8>) -> ApiResponse {
    ApiResponse { status, body }
}
