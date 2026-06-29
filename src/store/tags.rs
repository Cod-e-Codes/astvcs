use crate::store::atomic::write_atomic_text;
use crate::store::error::{RepoError, RepoResult};
use crate::store::{Repo, StateId};
use crate::trace;
use std::fs;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagInfo {
    pub name: String,
    pub state_id: StateId,
}

pub(crate) fn validate_tag_name(name: &str) -> RepoResult<()> {
    if name.is_empty() {
        return Err(RepoError::invalid_input("tag name cannot be empty"));
    }
    if name.contains('/') {
        return Err(RepoError::invalid_input("tag name cannot contain '/'"));
    }
    if name.contains("..") {
        return Err(RepoError::invalid_input("tag name cannot contain '..'"));
    }
    Ok(())
}

impl Repo {
    pub fn create_tag(&self, name: &str, reference: &str) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        validate_tag_name(name)?;
        let ref_path = self.astvcs_dir().join("refs/tags").join(name);
        if ref_path.exists() {
            return Err(RepoError::already_exists(format!(
                "tag already exists: {name}"
            )));
        }
        let state = self.resolve_state_ref_unlocked(reference)?;
        self.write_tag_unlocked(name, &state)?;
        trace::notice(format!("tag: created {name} at state {state}"));
        Ok(())
    }

    pub fn list_tags(&self) -> RepoResult<Vec<TagInfo>> {
        let _lock = self.repo_lock()?;
        self.list_tags_unlocked()
    }

    pub(crate) fn list_tags_unlocked(&self) -> RepoResult<Vec<TagInfo>> {
        let dir = self.astvcs_dir().join("refs/tags");
        let mut tags = Vec::new();
        if !dir.is_dir() {
            return Ok(tags);
        }
        for entry in fs::read_dir(&dir).map_err(|e| RepoError::from_io("read tags", e))? {
            let entry = entry.map_err(|e| RepoError::from_io("read tag entry", e))?;
            let name = entry.file_name().to_string_lossy().to_string();
            tags.push(TagInfo {
                name: name.clone(),
                state_id: self.read_tag_unlocked(&name)?,
            });
        }
        tags.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(tags)
    }

    pub fn read_tag(&self, name: &str) -> RepoResult<StateId> {
        let _lock = self.repo_lock()?;
        self.read_tag_unlocked(name)
    }

    pub(crate) fn read_tag_unlocked(&self, name: &str) -> RepoResult<StateId> {
        let path = self.astvcs_dir().join("refs/tags").join(name);
        if !path.is_file() {
            return Err(RepoError::not_found(format!("tag not found: {name}")));
        }
        let text = fs::read_to_string(&path).map_err(|e| RepoError::from_io("read tag ref", e))?;
        Ok(text.trim().to_string())
    }

    pub fn remove_tag(&self, name: &str) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        let ref_path = self.astvcs_dir().join("refs/tags").join(name);
        if !ref_path.is_file() {
            return Err(RepoError::not_found(format!("tag not found: {name}")));
        }
        fs::remove_file(&ref_path).map_err(|e| RepoError::from_io("remove tag", e))?;
        trace::notice(format!("tag: removed {name}"));
        Ok(())
    }

    pub(crate) fn write_tag_unlocked(&self, name: &str, state_id: &StateId) -> RepoResult<()> {
        validate_tag_name(name)?;
        write_atomic_text(
            &self.astvcs_dir().join("refs/tags").join(name),
            &format!("{state_id}\n"),
        )?;
        Ok(())
    }

    pub fn write_tag(&self, name: &str, state_id: &StateId) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        self.write_tag_unlocked(name, state_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn sample_repo() -> (TempDir, Repo) {
        let dir = TempDir::new().unwrap();
        let repo = Repo::init_with_identity(dir.path()).unwrap();
        (dir, repo)
    }

    #[test]
    fn tag_create_list_remove() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        let state = repo.commit("v1").unwrap().state_id;

        repo.create_tag("v1.0", "main").unwrap();
        let tags = repo.list_tags().unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name, "v1.0");
        assert_eq!(tags[0].state_id, state);

        repo.remove_tag("v1.0").unwrap();
        assert!(repo.list_tags().unwrap().is_empty());
    }

    #[test]
    fn resolve_tag_ref() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        repo.commit("v1").unwrap();
        repo.create_tag("release", "main").unwrap();

        let resolved = repo.resolve_state_ref("release").unwrap();
        assert_eq!(resolved, repo.head_state().unwrap());
    }

    #[test]
    fn tag_name_validation() {
        let (_, repo) = sample_repo();
        assert!(repo.create_tag("", "main").is_err());
        assert!(repo.create_tag("bad/name", "main").is_err());
        assert!(repo.create_tag("bad..name", "main").is_err());
    }
}
