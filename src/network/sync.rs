use crate::network::remote::{ensure_remote_dir, remote_token, remote_url};
use crate::network::transport::Transport;
use crate::store::{Repo, RepoError, StateId};
use crate::trace;
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::Path;

fn map_repo<T>(result: Result<T, RepoError>) -> Result<T, String> {
    result.map_err(|e| e.to_string())
}

struct MissingStates {
    states: Vec<StateId>,
    shallow_boundary: Option<StateId>,
}

fn collect_missing_states(
    local: &Repo,
    transport: &Transport,
    tip: &StateId,
    depth: Option<usize>,
) -> Result<MissingStates, String> {
    if let Some(limit) = depth {
        let ancestry = transport.get_ancestry(tip, limit)?;
        let mut missing = Vec::new();
        for id in ancestry.states.iter().rev() {
            if !local.has_timeline(id) {
                missing.push(id.clone());
            }
        }
        return Ok(MissingStates {
            states: missing,
            shallow_boundary: ancestry.shallow_boundary,
        });
    }

    let mut missing = Vec::new();
    let mut queue = VecDeque::new();
    let mut seen = HashSet::new();
    queue.push_back(tip.clone());

    while let Some(id) = queue.pop_front() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if local.has_timeline(&id) {
            continue;
        }
        let entry = transport.get_timeline(&id)?;
        missing.push(id.clone());
        for parent in entry.parents.iter().chain(entry.parent.iter()) {
            if !seen.contains(parent) && !local.has_timeline(parent) {
                queue.push_back(parent.clone());
            }
        }
    }
    missing.reverse();
    Ok(MissingStates {
        states: missing,
        shallow_boundary: None,
    })
}

fn record_shallow_boundary(
    repo: &Repo,
    shallow_boundary: Option<&StateId>,
    fetched_states: &[StateId],
    depth: Option<usize>,
) -> Result<(), String> {
    map_repo(repo.update_shallow_boundaries(shallow_boundary, fetched_states, depth))
}

fn collect_upload_states(
    local: &Repo,
    transport: &Transport,
    tip: &StateId,
) -> Result<Vec<StateId>, String> {
    let mut upload = Vec::new();
    let mut queue = VecDeque::new();
    let mut seen = HashSet::new();
    queue.push_back(tip.clone());

    while let Some(id) = queue.pop_front() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if transport.has_timeline(&id)? {
            continue;
        }
        if !local.has_timeline(&id) {
            return Err(format!("missing local timeline entry: {id}"));
        }
        upload.push(id.clone());
        let entry = map_repo(local.load_timeline_entry(&id))?;
        for parent in entry.parents.iter().chain(entry.parent.iter()) {
            if !seen.contains(parent) {
                queue.push_back(parent.clone());
            }
        }
    }
    upload.reverse();
    Ok(upload)
}

fn import_state(local: &Repo, transport: &Transport, state_id: &StateId) -> Result<(), String> {
    let entry = transport.get_timeline(state_id)?;
    let manifest = transport.get_state(state_id)?;
    for entry in manifest.values() {
        let blob_id = &entry.blob;
        if !local.has_blob(blob_id) {
            let bytes = transport.get_blob(blob_id)?;
            map_repo(local.import_blob_bytes(blob_id, &bytes))?;
            trace::notice(format!("fetch: blob {blob_id}"));
        }
    }
    map_repo(local.import_state_manifest(state_id, &manifest))?;
    map_repo(local.import_timeline_entry(&entry))?;
    trace::notice(format!("fetch: state {state_id}"));
    Ok(())
}

fn upload_state(local: &Repo, transport: &Transport, state_id: &StateId) -> Result<(), String> {
    let entry = map_repo(local.load_timeline_entry(state_id))?;
    let manifest = map_repo(local.load_manifest(state_id))?;
    for entry in manifest.values() {
        let blob_id = &entry.blob;
        if !transport.has_blob(blob_id)? {
            let bytes = map_repo(local.read_blob_bytes(blob_id))?;
            transport.put_blob(blob_id, &bytes)?;
            trace::notice(format!("push: blob {blob_id}"));
        }
    }
    transport.put_state(state_id, &manifest)?;
    transport.put_timeline(&entry)?;
    trace::notice(format!("push: state {state_id}"));
    Ok(())
}

fn import_missing_history(
    repo: &Repo,
    transport: &Transport,
    tip: &StateId,
    depth: Option<usize>,
) -> Result<(), String> {
    let missing = collect_missing_states(repo, transport, tip, depth)?;
    for state_id in &missing.states {
        import_state(repo, transport, state_id)?;
    }
    record_shallow_boundary(
        repo,
        missing.shallow_boundary.as_ref(),
        &missing.states,
        depth,
    )
}

fn import_tag_tip(
    repo: &Repo,
    transport: &Transport,
    name: &str,
    tip: &StateId,
    depth: Option<usize>,
) -> Result<(), String> {
    import_missing_history(repo, transport, tip, depth)?;
    map_repo(repo.write_tag(name, tip))?;
    trace::notice(format!("fetch: tag {name} -> {tip}"));
    Ok(())
}

fn fetch_tags(repo: &Repo, transport: &Transport, depth: Option<usize>) -> Result<(), String> {
    for (name, tip) in transport.list_tags()? {
        import_tag_tip(repo, transport, &name, &tip, depth)?;
    }
    Ok(())
}

pub struct FetchOutcome {
    pub branches: Vec<(String, StateId)>,
}

pub fn fetch(
    repo: &Repo,
    remote_name: &str,
    branch: Option<&str>,
    depth: Option<usize>,
    insecure: bool,
) -> Result<FetchOutcome, String> {
    let _lock = map_repo(repo.repo_lock())?;
    let url = remote_url(repo, remote_name)?;
    let token = remote_token(repo, remote_name)?;
    let transport = Transport::open_with_options(&url, token.as_deref(), insecure)?;
    ensure_remote_dir(repo, remote_name)?;

    let refs = transport.list_refs()?;
    let targets: Vec<(String, StateId)> = match branch {
        Some(name) => {
            let tip = refs
                .get(name)
                .cloned()
                .ok_or_else(|| format!("remote branch not found: {name}"))?;
            vec![(name.to_string(), tip)]
        }
        None => refs.into_iter().collect(),
    };

    for (name, tip) in &targets {
        import_missing_history(repo, &transport, tip, depth)?;
        map_repo(repo.write_remote_ref(remote_name, name, tip))?;
        trace::notice(format!("fetch: {remote_name}/{name} -> {tip}"));
    }

    fetch_tags(repo, &transport, depth)?;

    Ok(FetchOutcome { branches: targets })
}

fn push_tags(repo: &Repo, transport: &Transport) -> Result<(), String> {
    for tag in map_repo(repo.list_tags())? {
        if transport.get_tag(&tag.name)?.as_ref() == Some(&tag.state_id) {
            continue;
        }
        let upload = collect_upload_states(repo, transport, &tag.state_id)?;
        for state_id in upload {
            upload_state(repo, transport, &state_id)?;
        }
        transport.set_tag(&tag.name, &tag.state_id)?;
        trace::notice(format!("push: tag {} -> {}", tag.name, tag.state_id));
    }
    Ok(())
}

pub struct PushOutcome {
    pub branch: String,
    pub state_id: StateId,
}

pub fn push(
    repo: &Repo,
    remote_name: &str,
    branch: Option<&str>,
    force: bool,
    no_verify: bool,
    insecure: bool,
) -> Result<PushOutcome, String> {
    let _lock = map_repo(repo.repo_lock())?;
    let url = remote_url(repo, remote_name)?;
    let token = remote_token(repo, remote_name)?;
    let transport = Transport::open_with_options(&url, token.as_deref(), insecure)?;

    let branch_name = match branch {
        Some(name) => name.to_string(),
        None => map_repo(repo.head_branch())?
            .ok_or_else(|| "detached HEAD; specify a branch to push".to_string())?,
    };

    let local_tip = map_repo(repo.branch_state(&branch_name))?;
    let remote_tip = transport.get_ref(&branch_name)?;

    if let Some(ref remote_id) = remote_tip {
        if remote_id == &local_tip {
            trace::notice(format!("push: {branch_name} already up to date"));
            return Ok(PushOutcome {
                branch: branch_name,
                state_id: local_tip,
            });
        }
        if !force && !map_repo(repo.is_ancestor_of(remote_id, &local_tip))? {
            return Err(format!(
                "non-fast-forward push for {branch_name}; use --force"
            ));
        }
    }

    map_repo(crate::store::hooks::run_pre_push_hook(
        repo,
        &local_tip,
        remote_name,
        no_verify,
    ))?;

    let upload = collect_upload_states(repo, &transport, &local_tip)?;
    for state_id in upload {
        upload_state(repo, &transport, &state_id)?;
    }
    transport.set_ref(&branch_name, &local_tip, force)?;
    trace::notice(format!("push: {branch_name} -> {local_tip}"));

    push_tags(repo, &transport)?;

    Ok(PushOutcome {
        branch: branch_name,
        state_id: local_tip,
    })
}

pub fn clone_repo(
    url: &str,
    path: &Path,
    token: Option<&str>,
    depth: Option<usize>,
    insecure: bool,
) -> Result<(Repo, String), String> {
    Transport::open_with_options(url, token, insecure)?;
    if path.exists() {
        if path.read_dir().map_err(|e| e.to_string())?.next().is_some() {
            return Err(format!("destination is not empty: {}", path.display()));
        }
    } else {
        fs::create_dir_all(path).map_err(|e| e.to_string())?;
    }

    let repo = map_repo(Repo::init(path))?;
    crate::network::remote::add_remote(&repo, "origin", url, token)?;

    let transport = Transport::open_with_options(url, token, insecure)?;
    let default_branch = transport.default_branch()?;
    let refs = transport.list_refs()?;
    let tip = refs
        .get(&default_branch)
        .cloned()
        .ok_or_else(|| format!("remote has no branch {default_branch}"))?;

    import_missing_history(&repo, &transport, &tip, depth)?;

    ensure_remote_dir(&repo, "origin")?;
    for (name, state_id) in &refs {
        map_repo(repo.write_remote_ref("origin", name, state_id))?;
    }

    map_repo(repo.write_branch_ref(&default_branch, &tip))?;
    map_repo(repo.checkout_branch(&default_branch))?;

    fetch_tags(&repo, &transport, depth)?;

    Ok((repo, default_branch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::remote::add_remote;
    use tempfile::TempDir;

    fn init_with_commit(dir: &Path, message: &str) -> Repo {
        let repo = Repo::init_with_identity(dir).unwrap();
        fs::write(dir.join("hello.txt"), "hello\n").unwrap();
        repo.commit(message).unwrap();
        repo
    }

    fn commit_file(repo: &Repo, dir: &Path, name: &str, content: &str, message: &str) {
        fs::write(dir.join(name), content).unwrap();
        repo.commit(message).unwrap();
    }

    fn timeline_count(repo: &Repo) -> usize {
        fs::read_dir(repo.astvcs_dir().join("timeline"))
            .unwrap()
            .count()
    }

    #[test]
    fn file_remote_fetch_and_push() {
        let upstream = TempDir::new().unwrap();
        let downstream = TempDir::new().unwrap();
        let upstream_repo = init_with_commit(upstream.path(), "upstream change");

        let downstream_repo = Repo::init_with_identity(downstream.path()).unwrap();
        add_remote(
            &downstream_repo,
            "origin",
            upstream.path().to_str().unwrap(),
            None,
        )
        .unwrap();

        let outcome = fetch(&downstream_repo, "origin", Some("main"), None, false).unwrap();
        assert_eq!(outcome.branches.len(), 1);

        let remote_tip = downstream_repo.read_remote_ref("origin", "main").unwrap();
        assert_eq!(remote_tip, Some(upstream_repo.head_state().unwrap()));

        downstream_repo
            .write_branch_ref("main", remote_tip.as_ref().unwrap())
            .unwrap();
        downstream_repo.checkout_branch("main").unwrap();

        fs::write(downstream.path().join("hello.txt"), "world\n").unwrap();
        downstream_repo.commit("downstream change").unwrap();
        push(
            &downstream_repo,
            "origin",
            Some("main"),
            false,
            false,
            false,
        )
        .unwrap();

        assert_eq!(
            upstream_repo.head_state().unwrap(),
            downstream_repo.head_state().unwrap()
        );
    }

    #[test]
    fn clone_from_file_remote() {
        let upstream = TempDir::new().unwrap();
        init_with_commit(upstream.path(), "initial");

        let clone_dir = TempDir::new().unwrap();
        let (repo, branch) = clone_repo(
            upstream.path().to_str().unwrap(),
            clone_dir.path(),
            None,
            None,
            false,
        )
        .unwrap();
        assert_eq!(branch, "main");
        assert!(repo.has_timeline(&repo.head_state().unwrap()));
        assert_eq!(
            fs::read_to_string(clone_dir.path().join("hello.txt")).unwrap(),
            "hello\n"
        );
    }

    #[test]
    fn clone_uses_remote_default_branch() {
        use crate::store::RepoConfig;

        let upstream = TempDir::new().unwrap();
        let upstream_repo = init_with_commit(upstream.path(), "initial");
        upstream_repo.create_branch("develop", None).unwrap();

        let config_path = upstream.path().join(".astvcs/config.json");
        let mut config: RepoConfig =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        config.default_branch = "develop".into();
        fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

        let clone_dir = TempDir::new().unwrap();
        let (repo, branch) = clone_repo(
            upstream.path().to_str().unwrap(),
            clone_dir.path(),
            None,
            None,
            false,
        )
        .unwrap();
        assert_eq!(branch, "develop");
        assert_eq!(repo.head_branch().unwrap(), Some("develop".into()));
        assert_eq!(
            fs::read_to_string(clone_dir.path().join("hello.txt")).unwrap(),
            "hello\n"
        );
    }

    #[test]
    fn shallow_clone_fetches_fewer_timeline_entries_than_full_clone() {
        let upstream = TempDir::new().unwrap();
        let upstream_repo = init_with_commit(upstream.path(), "v1");
        commit_file(&upstream_repo, upstream.path(), "hello.txt", "v2\n", "v2");
        commit_file(&upstream_repo, upstream.path(), "hello.txt", "v3\n", "v3");
        commit_file(&upstream_repo, upstream.path(), "hello.txt", "v4\n", "v4");
        commit_file(&upstream_repo, upstream.path(), "hello.txt", "v5\n", "v5");
        assert!(timeline_count(&upstream_repo) >= 5);

        let shallow_dir = TempDir::new().unwrap();
        let (shallow_repo, _) = clone_repo(
            upstream.path().to_str().unwrap(),
            shallow_dir.path(),
            None,
            Some(2),
            false,
        )
        .unwrap();
        let full_dir = TempDir::new().unwrap();
        let (full_repo, _) = clone_repo(
            upstream.path().to_str().unwrap(),
            full_dir.path(),
            None,
            None,
            false,
        )
        .unwrap();

        let shallow_count = timeline_count(&shallow_repo);
        let full_count = timeline_count(&full_repo);
        assert!(shallow_count < full_count);
        assert_eq!(shallow_count, 3);
        assert!(shallow_repo.load_shallow_boundaries().unwrap().len() == 1);
    }

    #[test]
    fn merge_base_fails_on_shallow_repo_when_lca_missing() {
        let upstream = TempDir::new().unwrap();
        let upstream_repo = init_with_commit(upstream.path(), "root");
        commit_file(
            &upstream_repo,
            upstream.path(),
            "hello.txt",
            "main2\n",
            "main2",
        );
        let main_tip = upstream_repo.head_state().unwrap();
        upstream_repo.create_branch("feature", None).unwrap();
        commit_file(
            &upstream_repo,
            upstream.path(),
            "hello.txt",
            "feature2\n",
            "feature2",
        );

        let shallow_dir = TempDir::new().unwrap();
        let (shallow_repo, _) = clone_repo(
            upstream.path().to_str().unwrap(),
            shallow_dir.path(),
            None,
            Some(1),
            false,
        )
        .unwrap();
        fetch(&shallow_repo, "origin", Some("feature"), Some(1), false).unwrap();
        let feature_tip = shallow_repo
            .read_remote_ref("origin", "feature")
            .unwrap()
            .unwrap();
        shallow_repo
            .write_branch_ref("feature", &feature_tip)
            .unwrap();

        let err = shallow_repo
            .merge_base_refs("main", "feature")
            .unwrap_err()
            .to_string();
        assert!(err.contains("shallow history"));
        assert!(err.contains("merge-base"));

        let merge_err = shallow_repo.plan_merge("feature").unwrap_err().to_string();
        assert!(merge_err.contains("shallow history"));

        let _ = main_tip;
    }
}
