use crate::store::{Repo, RepoError, TimelineEntry};
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex};

fn map_repo<T>(result: Result<T, RepoError>) -> Result<T, String> {
    result.map_err(|e| e.to_string())
}

const API_PREFIX: &str = "/v1";

pub fn serve_repo(repo: &Repo, bind: &str, port: u16) -> Result<(), String> {
    let addr = format!("{bind}:{port}");
    let server = tiny_http::Server::http(&addr).map_err(|e| e.to_string())?;
    let repo = Arc::new(Mutex::new(map_repo(Repo::open(repo.root_path()))?));
    eprintln!("astvcs serve listening on http://{addr}/");

    for mut request in server.incoming_requests() {
        let response = match dispatch(&repo, &mut request) {
            Ok(resp) => resp,
            Err(e) => text_response(500, &e),
        };
        if let Err(e) = request.respond(response) {
            eprintln!("serve respond error: {e}");
        }
    }
    Ok(())
}

fn dispatch(
    repo: &Arc<Mutex<Repo>>,
    request: &mut tiny_http::Request,
) -> Result<tiny_http::Response<Cursor<Vec<u8>>>, String> {
    let method = request.method().clone();
    let url = request.url().to_string();
    let force = request
        .headers()
        .iter()
        .any(|h| h.field.as_str().as_str() == "X-Astvcs-Force" && h.value.as_str() == "true");
    let body = read_body(request)?;
    let path = url.split('?').next().unwrap_or(&url);

    if path == format!("{API_PREFIX}/config") && method == tiny_http::Method::Get {
        let repo = repo.lock().map_err(|e| e.to_string())?;
        let config = map_repo(repo.load_config())?;
        return Ok(json_response(
            200,
            &serde_json::to_vec(&config).map_err(|e| e.to_string())?,
        ));
    }
    if path == format!("{API_PREFIX}/refs/heads") && method == tiny_http::Method::Get {
        let repo = repo.lock().map_err(|e| e.to_string())?;
        let mut refs = HashMap::new();
        for branch in map_repo(repo.list_branches())? {
            refs.insert(branch.name, branch.state_id);
        }
        return Ok(json_response(
            200,
            &serde_json::to_vec(&refs).map_err(|e| e.to_string())?,
        ));
    }
    if path == format!("{API_PREFIX}/refs/tags") && method == tiny_http::Method::Get {
        let repo = repo.lock().map_err(|e| e.to_string())?;
        let mut refs = HashMap::new();
        for tag in map_repo(repo.list_tags())? {
            refs.insert(tag.name, tag.state_id);
        }
        return Ok(json_response(
            200,
            &serde_json::to_vec(&refs).map_err(|e| e.to_string())?,
        ));
    }
    if let Some(branch) = path.strip_prefix(&format!("{API_PREFIX}/refs/heads/")) {
        return handle_ref(repo, &method, branch, &body, force);
    }
    if let Some(name) = path.strip_prefix(&format!("{API_PREFIX}/refs/tags/")) {
        return handle_tag(repo, &method, name, &body);
    }
    if let Some(id) = path.strip_prefix(&format!("{API_PREFIX}/blobs/")) {
        return handle_blob(repo, &method, id, &body);
    }
    if let Some(id) = path.strip_prefix(&format!("{API_PREFIX}/states/")) {
        return handle_state(repo, &method, id, &body);
    }
    if let Some(id) = path.strip_prefix(&format!("{API_PREFIX}/timeline/")) {
        return handle_timeline(repo, &method, id, &body);
    }
    Ok(text_response(404, "not found"))
}

fn read_body(request: &mut tiny_http::Request) -> Result<Vec<u8>, String> {
    if request.method() == &tiny_http::Method::Get || request.method() == &tiny_http::Method::Head {
        return Ok(Vec::new());
    }
    let mut buf = Vec::new();
    request
        .as_reader()
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    Ok(buf)
}

fn handle_ref(
    repo: &Arc<Mutex<Repo>>,
    method: &tiny_http::Method,
    branch: &str,
    body: &[u8],
    force: bool,
) -> Result<tiny_http::Response<Cursor<Vec<u8>>>, String> {
    let repo = repo.lock().map_err(|e| e.to_string())?;
    match *method {
        tiny_http::Method::Get => {
            let path = repo.astvcs_dir().join("refs/heads").join(branch);
            if !path.is_file() {
                return Ok(text_response(404, "branch not found"));
            }
            let state_id = map_repo(repo.branch_state(branch))?;
            Ok(text_response(200, &format!("{state_id}\n")))
        }
        tiny_http::Method::Put => {
            let state_id = std::str::from_utf8(body)
                .map_err(|e| e.to_string())?
                .trim()
                .to_string();
            if !force
                && let Ok(current) = map_repo(repo.branch_state(branch))
                && current != state_id
                && !map_repo(repo.is_ancestor_of(&current, &state_id))?
            {
                return Ok(text_response(409, "non-fast-forward update rejected"));
            }
            map_repo(repo.write_branch_ref(branch, &state_id))?;
            Ok(text_response(200, "ok"))
        }
        tiny_http::Method::Head => {
            let path = repo.astvcs_dir().join("refs/heads").join(branch);
            let status = if path.is_file() { 200 } else { 404 };
            Ok(text_response(status, ""))
        }
        _ => Ok(text_response(405, "method not allowed")),
    }
}

fn handle_tag(
    repo: &Arc<Mutex<Repo>>,
    method: &tiny_http::Method,
    name: &str,
    body: &[u8],
) -> Result<tiny_http::Response<Cursor<Vec<u8>>>, String> {
    let repo = repo.lock().map_err(|e| e.to_string())?;
    match *method {
        tiny_http::Method::Get => {
            let path = repo.astvcs_dir().join("refs/tags").join(name);
            if !path.is_file() {
                return Ok(text_response(404, "tag not found"));
            }
            let state_id = map_repo(repo.read_tag(name))?;
            Ok(text_response(200, &format!("{state_id}\n")))
        }
        tiny_http::Method::Put => {
            let state_id = std::str::from_utf8(body)
                .map_err(|e| e.to_string())?
                .trim()
                .to_string();
            map_repo(repo.write_tag(name, &state_id))?;
            Ok(text_response(200, "ok"))
        }
        tiny_http::Method::Head => {
            let path = repo.astvcs_dir().join("refs/tags").join(name);
            let status = if path.is_file() { 200 } else { 404 };
            Ok(text_response(status, ""))
        }
        _ => Ok(text_response(405, "method not allowed")),
    }
}

fn handle_blob(
    repo: &Arc<Mutex<Repo>>,
    method: &tiny_http::Method,
    id: &str,
    body: &[u8],
) -> Result<tiny_http::Response<Cursor<Vec<u8>>>, String> {
    let repo = repo.lock().map_err(|e| e.to_string())?;
    match *method {
        tiny_http::Method::Get => {
            if !repo.has_blob(id) {
                return Ok(text_response(404, "blob not found"));
            }
            let bytes = map_repo(repo.read_blob_bytes(id))?;
            Ok(bytes_response(200, &bytes))
        }
        tiny_http::Method::Put => {
            if repo.has_blob(id) {
                return Ok(text_response(409, "blob already exists"));
            }
            map_repo(repo.import_blob_bytes(id, body))?;
            Ok(text_response(201, "created"))
        }
        tiny_http::Method::Head => {
            let status = if repo.has_blob(id) { 200 } else { 404 };
            Ok(text_response(status, ""))
        }
        _ => Ok(text_response(405, "method not allowed")),
    }
}

fn handle_state(
    repo: &Arc<Mutex<Repo>>,
    method: &tiny_http::Method,
    id: &str,
    body: &[u8],
) -> Result<tiny_http::Response<Cursor<Vec<u8>>>, String> {
    let repo = repo.lock().map_err(|e| e.to_string())?;
    let state_id = id.to_string();
    match *method {
        tiny_http::Method::Get => {
            if !repo.has_state(&state_id) {
                return Ok(text_response(404, "state not found"));
            }
            let manifest = map_repo(repo.load_manifest(&state_id))?;
            Ok(json_response(
                200,
                &serde_json::to_vec(&manifest).map_err(|e| e.to_string())?,
            ))
        }
        tiny_http::Method::Put => {
            if repo.has_state(&state_id) {
                return Ok(text_response(409, "state already exists"));
            }
            let manifest: crate::store::ManifestMap =
                serde_json::from_slice(body).map_err(|e| e.to_string())?;
            map_repo(repo.import_state_manifest(&state_id, &manifest))?;
            Ok(text_response(201, "created"))
        }
        tiny_http::Method::Head => {
            let status = if repo.has_state(&state_id) { 200 } else { 404 };
            Ok(text_response(status, ""))
        }
        _ => Ok(text_response(405, "method not allowed")),
    }
}

fn handle_timeline(
    repo: &Arc<Mutex<Repo>>,
    method: &tiny_http::Method,
    id: &str,
    body: &[u8],
) -> Result<tiny_http::Response<Cursor<Vec<u8>>>, String> {
    let repo = repo.lock().map_err(|e| e.to_string())?;
    let state_id = id.to_string();
    match *method {
        tiny_http::Method::Get => {
            if !repo.has_timeline(&state_id) {
                return Ok(text_response(404, "timeline entry not found"));
            }
            let entry = map_repo(repo.load_timeline_entry(&state_id))?;
            Ok(json_response(
                200,
                &serde_json::to_vec(&entry).map_err(|e| e.to_string())?,
            ))
        }
        tiny_http::Method::Put => {
            if repo.has_timeline(&state_id) {
                return Ok(text_response(409, "timeline entry already exists"));
            }
            let entry: TimelineEntry = serde_json::from_slice(body).map_err(|e| e.to_string())?;
            if entry.id != state_id {
                return Ok(text_response(400, "timeline id mismatch"));
            }
            map_repo(repo.import_timeline_entry(&entry))?;
            Ok(text_response(201, "created"))
        }
        tiny_http::Method::Head => {
            let status = if repo.has_timeline(&state_id) {
                200
            } else {
                404
            };
            Ok(text_response(status, ""))
        }
        _ => Ok(text_response(405, "method not allowed")),
    }
}

fn text_response(status: u16, body: &str) -> tiny_http::Response<Cursor<Vec<u8>>> {
    tiny_http::Response::from_string(body.to_string()).with_status_code(status)
}

fn json_response(status: u16, body: &[u8]) -> tiny_http::Response<Cursor<Vec<u8>>> {
    tiny_http::Response::from_data(body.to_vec())
        .with_status_code(status)
        .with_header(tiny_http::Header::from_bytes(b"Content-Type", b"application/json").unwrap())
}

fn bytes_response(status: u16, body: &[u8]) -> tiny_http::Response<Cursor<Vec<u8>>> {
    tiny_http::Response::from_data(body.to_vec()).with_status_code(status)
}
