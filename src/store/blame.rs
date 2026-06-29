use crate::diff::{TextEdit, diff_text};
use crate::frontend::FileContent;
use crate::store::error::{RepoError, RepoResult};
use crate::store::manifest::FileMode;
use crate::store::repo::{
    LinearParentError, TimelineEntry, linear_timeline_parent, normalize_repo_path,
};
use crate::store::{Repo, StateId, TrackedFile};
use crate::unparser::unparse;
use similar::{ChangeTag, TextDiff};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameLine {
    pub state_id: StateId,
    pub author_name: String,
    pub author_email: String,
    pub timestamp: String,
    pub message: String,
    pub content: String,
}

impl BlameLine {
    pub fn short_state_id(&self) -> &str {
        short_state_id(&self.state_id)
    }
}

pub(crate) fn short_state_id(state_id: &StateId) -> &str {
    &state_id[..state_id.len().min(8)]
}

fn split_lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

fn file_text_at_path(files: &HashMap<String, TrackedFile>, path: &str) -> RepoResult<String> {
    let tracked = files
        .get(path)
        .ok_or_else(|| RepoError::invalid_input(format!("no such path in repository: {path}")))?;
    tracked_file_text(tracked)
}

fn optional_file_text_at_path(
    files: &HashMap<String, TrackedFile>,
    path: &str,
) -> RepoResult<Option<String>> {
    match files.get(path) {
        None => Ok(None),
        Some(tracked) => tracked_file_text(tracked).map(Some),
    }
}

fn tracked_file_text(tracked: &TrackedFile) -> RepoResult<String> {
    if tracked.mode == FileMode::Symlink {
        return Err(RepoError::invalid_input("blame does not support symlinks"));
    }
    match &tracked.content {
        FileContent::Binary(_) => Err(RepoError::invalid_input(
            "blame does not support binary files",
        )),
        FileContent::Symlink(_) => Err(RepoError::invalid_input("blame does not support symlinks")),
        FileContent::Text(blob) => Ok(blob.content.clone()),
        FileContent::Ast(graph) => Ok(unparse(graph)),
    }
}

/// Map line indices in `child` text to corresponding line indices in `parent` text.
pub(crate) fn child_to_parent_line_map(parent: &str, child: &str) -> HashMap<usize, usize> {
    let diff = TextDiff::from_lines(parent, child);
    let mut map = HashMap::new();
    let mut old_line = 0usize;
    let mut new_line = 0usize;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                map.insert(new_line, old_line);
                old_line += 1;
                new_line += 1;
            }
            ChangeTag::Delete => {
                old_line += 1;
            }
            ChangeTag::Insert => {
                new_line += 1;
            }
        }
    }
    map
}

/// Line indices in `child` that were inserted or modified relative to `parent`.
pub(crate) fn lines_changed_in_child(parent: &str, child: &str) -> HashSet<usize> {
    diff_text(parent, child)
        .into_iter()
        .filter_map(|edit| match edit {
            TextEdit::InsertLine { line, .. } => Some(line),
            TextEdit::ReplaceLine { line, .. } => Some(line),
            TextEdit::DeleteLine { .. } => None,
        })
        .collect()
}

#[derive(Clone)]
struct BlameAttribution {
    state_id: StateId,
    author_name: String,
    author_email: String,
    timestamp: String,
    message: String,
}

impl From<&TimelineEntry> for BlameAttribution {
    fn from(entry: &TimelineEntry) -> Self {
        Self {
            state_id: entry.id.clone(),
            author_name: entry.author_name.clone(),
            author_email: entry.author_email.clone(),
            timestamp: entry.timestamp.clone(),
            message: entry.message.clone(),
        }
    }
}

impl Repo {
    pub fn blame(&self, raw_path: &str) -> RepoResult<Vec<BlameLine>> {
        let _lock = self.repo_lock()?;
        self.blame_unlocked(raw_path)
    }

    fn blame_unlocked(&self, raw_path: &str) -> RepoResult<Vec<BlameLine>> {
        let path = normalize_repo_path(raw_path)?;
        let head = self.head_state_unlocked()?;
        let head_files = self.load_state_files_unlocked(&head)?;
        let head_text = file_text_at_path(&head_files, &path)?;
        let head_lines = split_lines(&head_text);
        if head_lines.is_empty() {
            return Ok(Vec::new());
        }

        let mut attributed = vec![None::<BlameAttribution>; head_lines.len()];
        let mut head_to_current: Vec<usize> = (0..head_lines.len()).collect();

        let mut current_id = head;
        loop {
            if attributed.iter().all(|a| a.is_some()) {
                break;
            }

            let entry = self.load_timeline_entry_unlocked(&current_id)?;
            let parent_id = match linear_timeline_parent(&entry) {
                Ok(id) => id,
                Err(LinearParentError::MergeCommit(id)) => {
                    return Err(RepoError::invalid_input(format!(
                        "cannot blame through merge state {id}; v1 requires linear history"
                    )));
                }
                Err(LinearParentError::NoParent(_)) => {
                    let info = BlameAttribution::from(&entry);
                    for slot in attributed.iter_mut().filter(|a| a.is_none()) {
                        *slot = Some(info.clone());
                    }
                    break;
                }
            };

            let child_files = self.load_state_files_unlocked(&current_id)?;
            let parent_files = self.load_state_files_unlocked(&parent_id)?;
            let child_text = file_text_at_path(&child_files, &path)?;
            let parent_text = optional_file_text_at_path(&parent_files, &path)?;

            let info = BlameAttribution::from(&entry);

            if parent_text.is_none() {
                for slot in attributed.iter_mut().filter(|a| a.is_none()) {
                    *slot = Some(info.clone());
                }
                break;
            }
            let parent_text = parent_text.unwrap();

            let changed = lines_changed_in_child(&parent_text, &child_text);
            for (h, slot) in attributed.iter_mut().enumerate() {
                if slot.is_some() {
                    continue;
                }
                let current_line = head_to_current[h];
                if changed.contains(&current_line) {
                    *slot = Some(info.clone());
                }
            }

            if attributed.iter().all(|a| a.is_some()) {
                break;
            }

            let child_to_parent = child_to_parent_line_map(&parent_text, &child_text);
            for (h, slot) in attributed.iter().enumerate() {
                if slot.is_some() {
                    continue;
                }
                let current_line = head_to_current[h];
                head_to_current[h] =
                    child_to_parent.get(&current_line).copied().ok_or_else(|| {
                        RepoError::other(format!(
                            "blame line mapping failed at state {} for line {current_line}",
                            entry.id
                        ))
                    })?;
            }

            current_id = parent_id;
        }

        Ok(head_lines
            .into_iter()
            .zip(attributed)
            .map(|(content, info)| {
                let info = info.expect("every blame line should be attributed");
                BlameLine {
                    state_id: info.state_id,
                    author_name: info.author_name,
                    author_email: info.author_email,
                    timestamp: info.timestamp,
                    message: info.message,
                    content,
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_to_parent_map_tracks_equal_lines() {
        let parent = "a\nb\nc\n";
        let child = "a\nB\nc\nd\n";
        let map = child_to_parent_line_map(parent, child);
        assert_eq!(map.get(&0), Some(&0));
        assert_eq!(map.get(&2), Some(&2));
        assert!(!map.contains_key(&1));
        assert!(!map.contains_key(&3));
    }

    #[test]
    fn lines_changed_in_child_detects_insert_and_modify() {
        let parent = "a\nb\nc\n";
        let child = "a\nB\nc\nd\n";
        let changed = lines_changed_in_child(parent, child);
        assert!(changed.contains(&1));
        assert!(changed.contains(&3));
        assert!(!changed.contains(&0));
        assert!(!changed.contains(&2));
    }
}
