use crate::merge::ConflictResolutionStyle;
use crate::store::error::{RepoError, RepoResult};
use crate::store::identity::resolve_author_identity;
use crate::store::repo::{
    HeadTarget, LinearParentError, MaterializeOptions, linear_timeline_parent,
};
use crate::store::{Repo, StateId};
use crate::trace;

impl Repo {
    pub fn cherry_pick(&self, reference: &str, message: &str, force: bool) -> RepoResult<StateId> {
        let _lock = self.repo_lock()?;
        self.cherry_pick_unlocked(reference, message, force)
    }

    fn cherry_pick_unlocked(
        &self,
        reference: &str,
        message: &str,
        force: bool,
    ) -> RepoResult<StateId> {
        let staging = self.load_staging_unlocked()?;
        if staging.staging_in_use() {
            return Err(RepoError::invalid_input(
                "cannot cherry-pick with staged changes; commit or reset --mixed to unstage",
            ));
        }

        let target_id = self.resolve_state_ref_unlocked(reference)?;
        let entry = self.load_timeline_entry_unlocked(&target_id)?;

        let parent = linear_timeline_parent(&entry).map_err(|e| match e {
            LinearParentError::MergeCommit(id) => RepoError::invalid_input(format!(
                "cannot cherry-pick merge state {id}; v1 requires a single-parent commit"
            )),
            LinearParentError::NoParent(id) => {
                RepoError::invalid_input(format!("cannot cherry-pick root state {id}"))
            }
        })?;

        let head = self.head_state_unlocked()?;
        let plan = self.plan_three_way_unlocked(&parent, &head, &target_id)?;

        if !plan.is_clean() {
            trace::warn("cherry-pick: aborted due to conflicts");
            return Err(
                RepoError::merge_conflict(plan.format_conflicts()).with_concise(
                    plan.format_conflicts_focused_for(
                        "cherry-pick",
                        "current HEAD",
                        "picked state",
                        ConflictResolutionStyle::None,
                    ),
                ),
            );
        }

        let materialize_opts = MaterializeOptions::new("cherry-pick").force(force);
        let clobbered = self.materialize_guard(&materialize_opts)?;

        let author = resolve_author_identity(self)?;
        let state_id = self.persist_state(
            &plan.merged_files,
            message,
            &author,
            Some(head.clone()),
            vec![head.clone()],
        )?;
        self.materialize_state_inner(&state_id, clobbered, &materialize_opts)?;

        match self.read_head_target()? {
            HeadTarget::Branch(branch) => {
                self.write_branch_ref_unlocked(&branch, &state_id)?;
                trace::notice(format!(
                    "cherry-pick: updated branch {branch} -> {state_id}"
                ));
            }
            HeadTarget::Detached(_) => {
                self.write_head_target(&HeadTarget::Detached(state_id.clone()))?;
                trace::notice(format!("cherry-pick: detached HEAD -> {state_id}"));
            }
        }
        trace::notice(format!(
            "cherry-pick: created state {state_id} from {target_id} onto {head}"
        ));
        Ok(state_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::repo::TimelineEntry;
    use std::collections::HashMap;
    use std::fs;
    use tempfile::TempDir;

    fn sample_repo() -> (TempDir, Repo) {
        let dir = TempDir::new().unwrap();
        let repo = Repo::init_with_identity(dir.path()).unwrap();
        (dir, repo)
    }

    fn entry(id: &str, parent: Option<&str>) -> TimelineEntry {
        TimelineEntry {
            id: id.to_string(),
            parent: parent.map(|p| p.to_string()),
            parents: parent.map(|p| vec![p.to_string()]).unwrap_or_default(),
            message: id.into(),
            timestamp: "0".into(),
            author_name: String::new(),
            author_email: String::new(),
            manifest: HashMap::new(),
            files: None,
        }
    }

    #[test]
    fn linear_timeline_parent_rejects_merge_commit() {
        let merge = "m".repeat(64);
        let timeline = entry(&merge, None);
        let timeline = TimelineEntry {
            parents: vec!["a".repeat(64), "b".repeat(64)],
            ..timeline
        };
        let err = linear_timeline_parent(&timeline).unwrap_err();
        assert_eq!(err, LinearParentError::MergeCommit(merge));
    }

    #[test]
    fn cherry_pick_rejects_merge_commit() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        repo.commit("v1").unwrap();
        repo.create_branch("feature", None).unwrap();
        repo.checkout_branch("feature").unwrap();
        fs::write(dir.path().join("note.txt"), "v2\n").unwrap();
        let feature_tip = repo.commit("feature").unwrap().state_id;
        repo.checkout_branch("main").unwrap();
        fs::write(dir.path().join("note.txt"), "v3\n").unwrap();
        let main_tip = repo.commit("main").unwrap().state_id;
        let merge_id = repo
            .persist_state(
                &repo.load_state_files(&main_tip).unwrap(),
                "merge",
                &crate::store::identity::AuthorIdentity {
                    name: "Test".into(),
                    email: "t@example.com".into(),
                },
                None,
                vec![main_tip.clone(), feature_tip.clone()],
            )
            .unwrap();

        let err = repo
            .cherry_pick(&merge_id, "pick merge", false)
            .unwrap_err();
        assert!(err.contains("cannot cherry-pick merge state"), "{err}");
    }
}
