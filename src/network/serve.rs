use crate::network::api::{
    ApiRequest, AuthOptions, dispatch, dispatch_serve_read, is_write_request,
};
use crate::store::error::RepoErrorKind;
use crate::store::{Repo, RepoLockGuard};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

fn map_repo<T>(result: Result<T, crate::store::RepoError>) -> Result<T, String> {
    result.map_err(|e| e.to_string())
}

#[derive(Clone, Debug, Default)]
pub struct ServeOptions {
    pub token: Option<String>,
    pub public_read: bool,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
}

/// In-process serve state: concurrent reads, serialized writes.
#[derive(Clone)]
struct SharedServeRepo {
    repo: Arc<RwLock<Repo>>,
}

impl SharedServeRepo {
    fn open(path: &Path) -> Result<Self, String> {
        Ok(Self {
            repo: Arc::new(RwLock::new(map_repo(Repo::open(path))?)),
        })
    }
}

pub fn validate_tls_config(
    tls_cert: &Option<PathBuf>,
    tls_key: &Option<PathBuf>,
) -> Result<(), String> {
    match (tls_cert.as_ref(), tls_key.as_ref()) {
        (None, None) | (Some(_), Some(_)) => Ok(()),
        _ => Err("--tls-cert and --tls-key must be provided together".into()),
    }
}

pub fn serve_repo(repo: &Repo, bind: &str, port: u16, options: ServeOptions) -> Result<(), String> {
    validate_tls_config(&options.tls_cert, &options.tls_key)?;
    let addr = format!("{bind}:{port}");
    let tls_enabled = options.tls_cert.is_some();
    let server = if tls_enabled {
        let cert_path = options.tls_cert.as_ref().unwrap();
        let key_path = options.tls_key.as_ref().unwrap();
        let certificate = fs::read(cert_path).map_err(|e| {
            format!(
                "failed to read TLS certificate {}: {e}",
                cert_path.display()
            )
        })?;
        let private_key = fs::read(key_path)
            .map_err(|e| format!("failed to read TLS private key {}: {e}", key_path.display()))?;
        tiny_http::Server::https(
            &addr,
            tiny_http::SslConfig {
                certificate,
                private_key,
            },
        )
        .map_err(|e| e.to_string())?
    } else {
        tiny_http::Server::http(&addr).map_err(|e| e.to_string())?
    };
    let shared = SharedServeRepo::open(repo.root_path())?;
    let auth = auth_options(&options);
    let scheme = if tls_enabled { "https" } else { "http" };
    eprintln!("astvcs serve listening on {scheme}://{addr}/");

    for mut request in server.incoming_requests() {
        let response = match http_dispatch(&shared, &mut request, &auth) {
            Ok(resp) => resp,
            Err(e) => text_response(500, &e),
        };
        if let Err(e) = request.respond(response) {
            eprintln!("serve respond error: {e}");
        }
    }
    Ok(())
}

fn auth_options(options: &ServeOptions) -> AuthOptions {
    AuthOptions {
        token: options.token.clone(),
        public_read: options.public_read,
    }
}

fn http_dispatch(
    shared: &SharedServeRepo,
    request: &mut tiny_http::Request,
    auth: &AuthOptions,
) -> Result<tiny_http::Response<Cursor<Vec<u8>>>, String> {
    let method = request.method().as_str().to_string();
    let url = request.url().to_string();
    let (path_only, query) = match url.split_once('?') {
        Some((path, query)) => (
            path.to_string(),
            crate::network::api::parse_query_string(query),
        ),
        None => (url, std::collections::HashMap::new()),
    };
    let path = path_only;
    let headers = request
        .headers()
        .iter()
        .map(|h| {
            (
                h.field.as_str().as_str().to_string(),
                h.value.as_str().to_string(),
            )
        })
        .collect();
    let body = read_body(request)?;
    let api_request = ApiRequest {
        method,
        path,
        query,
        body,
        headers,
    };

    if is_write_request(&api_request) {
        let repo_guard = shared
            .repo
            .write()
            .map_err(|e| format!("serve write lock poisoned: {e}"))?;
        let astvcs_dir = repo_guard.astvcs_dir().to_path_buf();
        let advisory = match RepoLockGuard::acquire(&astvcs_dir) {
            Ok(guard) => guard,
            Err(e) if e.kind == RepoErrorKind::LockContention => {
                return Ok(text_response(503, "repository locked"));
            }
            Err(e) => return Err(e.to_string()),
        };
        let api_response = dispatch(&repo_guard, &api_request, auth);
        drop(advisory);
        Ok(to_http_response(api_response))
    } else {
        let repo_guard = shared
            .repo
            .read()
            .map_err(|e| format!("serve read lock poisoned: {e}"))?;
        let api_response = dispatch_serve_read(&repo_guard, &api_request, auth);
        Ok(to_http_response(api_response))
    }
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

fn to_http_response(api: crate::network::api::ApiResponse) -> tiny_http::Response<Cursor<Vec<u8>>> {
    if api.body.is_empty() {
        return text_response(api.status, "");
    }
    if api.status == 200 && api.body.first().is_some_and(|b| *b == b'{' || *b == b'[') {
        return json_response(api.status, &api.body);
    }
    if api.body.iter().all(|b| b.is_ascii()) {
        text_response(api.status, &String::from_utf8_lossy(&api.body))
    } else {
        bytes_response(api.status, &api.body)
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
    use sha2::{Digest, Sha256};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
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
            let auth = auth_options(&options);

            let server = tiny_http::Server::from_listener(listener, None).unwrap();
            let shared = SharedServeRepo::open(&repo_path).unwrap();

            let handle = thread::spawn(move || {
                for mut request in server.incoming_requests() {
                    if shutdown_flag.load(Ordering::Relaxed) {
                        break;
                    }
                    let response = match http_dispatch(&shared, &mut request, &auth) {
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

    fn put_bytes(base: &str, path: &str, body: &[u8], token: Option<&str>) -> u16 {
        let client = http_client();
        let mut req = client.put(format!("{base}{path}")).body(body.to_vec());
        if let Some(token) = token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        req.send().unwrap().status().as_u16()
    }

    #[test]
    fn validate_tls_config_requires_both_or_neither() {
        assert!(validate_tls_config(&None, &None).is_ok());
        assert!(
            validate_tls_config(
                &Some(PathBuf::from("cert.pem")),
                &Some(PathBuf::from("key.pem")),
            )
            .is_ok()
        );
        assert_eq!(
            validate_tls_config(&Some(PathBuf::from("cert.pem")), &None).unwrap_err(),
            "--tls-cert and --tls-key must be provided together"
        );
        assert_eq!(
            validate_tls_config(&None, &Some(PathBuf::from("key.pem"))).unwrap_err(),
            "--tls-cert and --tls-key must be provided together"
        );
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
                ..Default::default()
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
                ..Default::default()
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
                ..Default::default()
            },
        );

        let (status, _) = get(&server.base_url, "/v1/config", None);
        assert_eq!(status, 200);

        let (status, body) = put(&server.base_url, "/v1/refs/heads/main", &state_id, None);
        assert_eq!(status, 401);
        assert_eq!(body.trim(), "unauthorized");
    }

    #[test]
    fn serve_put_returns_503_when_advisory_lock_held() {
        let (dir, repo) = init_repo();
        let state_id = repo.head_state().unwrap();
        let astvcs = dir.path().join(".astvcs");
        let server = TestServer::start(&repo, ServeOptions::default());
        let barrier = Arc::new(Barrier::new(2));
        let barrier_holder = Arc::clone(&barrier);
        let astvcs_holder = astvcs.clone();

        let holder = thread::spawn(move || {
            let _guard = RepoLockGuard::acquire(&astvcs_holder).unwrap();
            barrier_holder.wait();
            thread::sleep(Duration::from_millis(200));
        });

        barrier.wait();
        let (status, body) = put(&server.base_url, "/v1/refs/heads/main", &state_id, None);
        holder.join().unwrap();
        assert_eq!(status, 503);
        assert_eq!(body.trim(), "repository locked");
    }

    #[test]
    fn serve_concurrent_reads_during_writes() {
        let (_dir, repo) = init_repo();
        let state_id = repo.head_state().unwrap();
        let seed = b"seed-blob-for-concurrent-reads";
        let existing_blob = hex::encode(Sha256::digest(seed));
        repo.import_blob_bytes(&existing_blob, seed).unwrap();

        let server = TestServer::start(
            &repo,
            ServeOptions {
                public_read: true,
                ..Default::default()
            },
        );
        let base = server.base_url.clone();
        let read_ok = Arc::new(AtomicUsize::new(0));
        let write_ok = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(9));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let base = base.clone();
            let blob_id = existing_blob.clone();
            let state_id = state_id.clone();
            let read_ok = Arc::clone(&read_ok);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                let client = http_client();
                for _ in 0..20 {
                    let blob_status = client
                        .get(format!("{base}/v1/blobs/{blob_id}"))
                        .send()
                        .unwrap()
                        .status()
                        .as_u16();
                    let state_status = client
                        .get(format!("{base}/v1/states/{state_id}"))
                        .send()
                        .unwrap()
                        .status()
                        .as_u16();
                    if blob_status == 200 && state_status == 200 {
                        read_ok.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }));
        }

        let base_writer = base.clone();
        let write_ok_writer = Arc::clone(&write_ok);
        let barrier_writer = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier_writer.wait();
            for i in 0..10 {
                let payload = format!("concurrent-payload-{i}");
                let blob_id = hex::encode(Sha256::digest(payload.as_bytes()));
                let status = put_bytes(
                    &base_writer,
                    &format!("/v1/blobs/{blob_id}"),
                    payload.as_bytes(),
                    None,
                );
                if status == 201 {
                    write_ok_writer.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(read_ok.load(Ordering::Relaxed), 160);
        assert_eq!(write_ok.load(Ordering::Relaxed), 10);
    }
}
