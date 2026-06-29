use crate::store::{Repo, RepoError, TimelineEntry};
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex};
use subtle::ConstantTimeEq;

fn map_repo<T>(result: Result<T, RepoError>) -> Result<T, String> {
    result.map_err(|e| e.to_string())
}

const API_PREFIX: &str = "/v1";

#[derive(Clone, Debug, Default)]
pub struct ServeOptions {
    pub token: Option<String>,
    pub public_read: bool,
}

pub fn serve_repo(repo: &Repo, bind: &str, port: u16, options: ServeOptions) -> Result<(), String> {
    let addr = format!("{bind}:{port}");
    let server = tiny_http::Server::http(&addr).map_err(|e| e.to_string())?;
    let repo = Arc::new(Mutex::new(map_repo(Repo::open(repo.root_path()))?));
    eprintln!("astvcs serve listening on http://{addr}/");

    for mut request in server.incoming_requests() {
        let response = match dispatch(&repo, &mut request, &options) {
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
    options: &ServeOptions,
) -> Result<tiny_http::Response<Cursor<Vec<u8>>>, String> {
    let method = request.method().clone();
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or(&url);

    if let Err(resp) = check_auth(options, &method, path, request) {
        return Ok(resp);
    }

    let force = request
        .headers()
        .iter()
        .any(|h| h.field.as_str().as_str() == "X-Astvcs-Force" && h.value.as_str() == "true");
    let body = read_body(request)?;

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

fn check_auth(
    options: &ServeOptions,
    method: &tiny_http::Method,
    path: &str,
    request: &tiny_http::Request,
) -> Result<(), tiny_http::Response<Cursor<Vec<u8>>>> {
    let Some(expected) = options.token.as_deref() else {
        return Ok(());
    };
    if !path.starts_with(API_PREFIX) {
        return Ok(());
    }

    let is_read = *method == tiny_http::Method::Get || *method == tiny_http::Method::Head;
    if is_read && options.public_read {
        return Ok(());
    }

    let provided = parse_bearer_token(request);
    let authorized = provided
        .as_deref()
        .is_some_and(|p| token_matches(expected, p));
    if authorized {
        Ok(())
    } else {
        Err(text_response(401, "unauthorized"))
    }
}

fn parse_bearer_token(request: &tiny_http::Request) -> Option<String> {
    for header in request.headers() {
        if !header
            .field
            .as_str()
            .as_str()
            .eq_ignore_ascii_case("authorization")
        {
            continue;
        }
        let value = header.value.as_str();
        if value.len() < 7 {
            continue;
        }
        if !value[..7].eq_ignore_ascii_case("bearer ") {
            continue;
        }
        return Some(value[7..].to_string());
    }
    None
}

fn token_matches(expected: &str, provided: &str) -> bool {
    expected.as_bytes().ct_eq(provided.as_bytes()).into()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Repo;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    struct TestServer {
        base_url: String,
        shutdown: Arc<AtomicBool>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl TestServer {
        fn start(repo: &Repo, options: ServeOptions) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let base_url = format!("http://127.0.0.1:{port}");
            let shutdown = Arc::new(AtomicBool::new(false));
            let shutdown_flag = Arc::clone(&shutdown);
            let repo_path = repo.root_path().to_path_buf();

            let server = tiny_http::Server::from_listener(listener, None).unwrap();
            let repo = Arc::new(Mutex::new(Repo::open(&repo_path).unwrap()));

            let handle = thread::spawn(move || {
                for mut request in server.incoming_requests() {
                    if shutdown_flag.load(Ordering::Relaxed) {
                        break;
                    }
                    let response = match dispatch(&repo, &mut request, &options) {
                        Ok(resp) => resp,
                        Err(e) => text_response(500, &e),
                    };
                    let _ = request.respond(response);
                }
            });
            thread::sleep(Duration::from_millis(50));

            Self {
                base_url,
                shutdown,
                handle: Some(handle),
            }
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::Relaxed);
            let client = reqwest::blocking::Client::builder()
                .timeout(Duration::from_millis(500))
                .build()
                .unwrap();
            let _ = client.get(format!("{}/", self.base_url)).send();
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn init_repo() -> (TempDir, Repo) {
        let dir = TempDir::new().unwrap();
        let repo = Repo::init_with_identity(dir.path()).unwrap();
        (dir, repo)
    }

    fn http_client() -> reqwest::blocking::Client {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    fn get(base: &str, path: &str, token: Option<&str>) -> (u16, String) {
        let client = http_client();
        let mut req = client.get(format!("{base}{path}"));
        if let Some(token) = token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        let resp = req.send().unwrap();
        (resp.status().as_u16(), resp.text().unwrap_or_default())
    }

    fn put(base: &str, path: &str, body: &str, token: Option<&str>) -> (u16, String) {
        let client = http_client();
        let mut req = client.put(format!("{base}{path}")).body(body.to_string());
        if let Some(token) = token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        let resp = req.send().unwrap();
        (resp.status().as_u16(), resp.text().unwrap_or_default())
    }

    #[test]
    fn serve_requires_token_for_mutations() {
        let (_dir, repo) = init_repo();
        let state_id = repo.head_state().unwrap();
        let server = TestServer::start(
            &repo,
            ServeOptions {
                token: Some("secret-token".into()),
                public_read: false,
            },
        );

        let (status, body) = put(&server.base_url, "/v1/refs/heads/main", &state_id, None);
        assert_eq!(status, 401);
        assert_eq!(body.trim(), "unauthorized");

        let (status, body) = put(
            &server.base_url,
            "/v1/refs/heads/main",
            &state_id,
            Some("secret-token"),
        );
        assert_eq!(status, 200);
        assert_eq!(body.trim(), "ok");
    }

    #[test]
    fn serve_read_requires_token_by_default() {
        let (_dir, repo) = init_repo();
        let server = TestServer::start(
            &repo,
            ServeOptions {
                token: Some("read-secret".into()),
                public_read: false,
            },
        );

        let (status, body) = get(&server.base_url, "/v1/config", None);
        assert_eq!(status, 401);
        assert_eq!(body.trim(), "unauthorized");

        let (status, _) = get(&server.base_url, "/v1/config", Some("read-secret"));
        assert_eq!(status, 200);
    }

    #[test]
    fn serve_public_read_allows_anonymous_get() {
        let (_dir, repo) = init_repo();
        let state_id = repo.head_state().unwrap();
        let server = TestServer::start(
            &repo,
            ServeOptions {
                token: Some("pub-secret".into()),
                public_read: true,
            },
        );

        let (status, _) = get(&server.base_url, "/v1/config", None);
        assert_eq!(status, 200);

        let (status, body) = put(&server.base_url, "/v1/refs/heads/main", &state_id, None);
        assert_eq!(status, 401);
        assert_eq!(body.trim(), "unauthorized");
    }
}
