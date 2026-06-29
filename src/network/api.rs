use crate::store::timeline_ancestry;
use crate::store::{Repo, RepoError, StateId, TimelineEntry};
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
    pub query: HashMap<String, String>,
    pub body: Vec<u8>,
    pub headers: HashMap<String, String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AncestryResponse {
    pub states: Vec<String>,
    pub shallow_boundary: Option<String>,
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

/// True when the HTTP serve layer must take the in-process write lock and advisory lock.
pub fn is_write_request(request: &ApiRequest) -> bool {
    request.method.eq_ignore_ascii_case("PUT")
}

pub fn dispatch(repo: &Repo, request: &ApiRequest, auth: &AuthOptions) -> ApiResponse {
    dispatch_inner(repo, request, auth, false)
}

/// HTTP serve reads: no advisory `repo.lock`; uses unlocked repo paths so local CLI is not blocked.
pub fn dispatch_serve_read(repo: &Repo, request: &ApiRequest, auth: &AuthOptions) -> ApiResponse {
    dispatch_inner(repo, request, auth, true)
}

fn dispatch_inner(
    repo: &Repo,
    request: &ApiRequest,
    auth: &AuthOptions,
    serve_read: bool,
) -> ApiResponse {
    if let Some(resp) = check_auth(auth, request) {
        return resp;
    }

    let method = request.method.to_uppercase();
    let path = request.path.as_str();
    let force = request.force();
    let body = &request.body;

    if path == format!("{API_PREFIX}/config") && method == "GET" {
        let config = if serve_read {
            repo.load_config_unlocked()
        } else {
            repo.load_config()
        };
        return match map_repo(config) {
            Ok(config) => match serde_json::to_vec(&config) {
                Ok(bytes) => api_response(200, bytes),
                Err(e) => api_response(500, e.to_string().into_bytes()),
            },
            Err(e) => api_response(500, e.into_bytes()),
        };
    }
    if path == format!("{API_PREFIX}/refs/heads") && method == "GET" {
        let mut refs = HashMap::new();
        let branches = if serve_read {
            repo.list_branches_unlocked()
        } else {
            repo.list_branches()
        };
        return match map_repo(branches) {
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
        let tags = if serve_read {
            repo.list_tags_unlocked()
        } else {
            repo.list_tags()
        };
        return match map_repo(tags) {
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
        return handle_ref(repo, &method, branch, body, force, serve_read);
    }
    if let Some(name) = path.strip_prefix(&format!("{API_PREFIX}/refs/tags/")) {
        return handle_tag(repo, &method, name, body, serve_read);
    }
    if let Some(id) = path.strip_prefix(&format!("{API_PREFIX}/blobs/")) {
        return handle_blob(repo, &method, id, body, serve_read);
    }
    if let Some(id) = path.strip_prefix(&format!("{API_PREFIX}/states/")) {
        return handle_state(repo, &method, id, body, serve_read);
    }
    if let Some(rest) = path.strip_prefix(&format!("{API_PREFIX}/timeline/")) {
        if let Some((id, suffix)) = rest.split_once('/')
            && suffix == "ancestry"
        {
            return handle_ancestry(repo, &method, id, &request.query, serve_read);
        }
        return handle_timeline(repo, &method, rest, body, serve_read);
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

fn handle_ref(
    repo: &Repo,
    method: &str,
    branch: &str,
    body: &[u8],
    force: bool,
    serve_read: bool,
) -> ApiResponse {
    match method {
        "GET" => {
            let path = repo.astvcs_dir().join("refs/heads").join(branch);
            if !path.is_file() {
                return api_response(404, b"branch not found".to_vec());
            }
            let state = if serve_read {
                repo.read_branch_ref(branch)
            } else {
                repo.branch_state(branch)
            };
            match map_repo(state) {
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

fn handle_tag(repo: &Repo, method: &str, name: &str, body: &[u8], serve_read: bool) -> ApiResponse {
    match method {
        "GET" => {
            let path = repo.astvcs_dir().join("refs/tags").join(name);
            if !path.is_file() {
                return api_response(404, b"tag not found".to_vec());
            }
            let state = if serve_read {
                repo.read_tag_unlocked(name)
            } else {
                repo.read_tag(name)
            };
            match map_repo(state) {
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

fn handle_blob(repo: &Repo, method: &str, id: &str, body: &[u8], serve_read: bool) -> ApiResponse {
    match method {
        "GET" => {
            if !repo.has_blob(id) {
                return api_response(404, b"blob not found".to_vec());
            }
            let bytes = if serve_read {
                repo.blobs()
                    .read_bytes(&id.to_string())
                    .map_err(RepoError::from_message)
            } else {
                repo.read_blob_bytes(id)
            };
            match map_repo(bytes) {
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

fn handle_state(repo: &Repo, method: &str, id: &str, body: &[u8], serve_read: bool) -> ApiResponse {
    let state_id = id.to_string();
    match method {
        "GET" => {
            if !repo.has_state(&state_id) {
                return api_response(404, b"state not found".to_vec());
            }
            let manifest = if serve_read {
                repo.load_manifest_unlocked(&state_id)
            } else {
                repo.load_manifest(&state_id)
            };
            match map_repo(manifest) {
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

fn handle_ancestry(
    repo: &Repo,
    method: &str,
    id: &str,
    query: &HashMap<String, String>,
    serve_read: bool,
) -> ApiResponse {
    if method != "GET" {
        return api_response(405, b"method not allowed".to_vec());
    }
    let state_id = id.to_string();
    if !repo.has_timeline(&state_id) {
        return api_response(404, b"timeline entry not found".to_vec());
    }
    let depth = match query.get("depth") {
        None => None,
        Some(raw) => match raw.parse::<usize>() {
            Ok(0) => return api_response(400, b"depth must be at least 1".to_vec()),
            Ok(n) => Some(n),
            Err(_) => return api_response(400, b"invalid depth".to_vec()),
        },
    };
    let load_entry = |sid: &StateId| {
        if serve_read {
            map_repo(repo.load_timeline_entry_unlocked(sid))
        } else {
            map_repo(repo.load_timeline_entry(sid))
        }
    };
    match timeline_ancestry(&state_id, depth, load_entry) {
        Ok(result) => {
            let body = AncestryResponse {
                states: result.states,
                shallow_boundary: result.shallow_boundary,
            };
            match serde_json::to_vec(&body) {
                Ok(bytes) => api_response(200, bytes),
                Err(e) => api_response(500, e.to_string().into_bytes()),
            }
        }
        Err(e) => api_response(500, e.into_bytes()),
    }
}

fn handle_timeline(
    repo: &Repo,
    method: &str,
    id: &str,
    body: &[u8],
    serve_read: bool,
) -> ApiResponse {
    let state_id = id.to_string();
    match method {
        "GET" => {
            if !repo.has_timeline(&state_id) {
                return api_response(404, b"timeline entry not found".to_vec());
            }
            let entry = if serve_read {
                repo.load_timeline_entry_unlocked(&state_id)
            } else {
                repo.load_timeline_entry(&state_id)
            };
            match map_repo(entry) {
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

pub fn parse_query_string(query: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let trimmed = query.strip_prefix('?').unwrap_or(query);
    if trimmed.is_empty() {
        return out;
    }
    for pair in trimmed.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            if !key.is_empty() {
                out.insert(key.to_string(), value.to_string());
            }
        } else if !pair.is_empty() {
            out.insert(pair.to_string(), String::new());
        }
    }
    out
}

fn api_response(status: u16, body: Vec<u8>) -> ApiResponse {
    ApiResponse { status, body }
}
