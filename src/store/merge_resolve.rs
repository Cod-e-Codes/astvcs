use crate::store::repo::MergePlan;
use crate::store::tracked::TrackedFile;
use crate::trace;
use std::collections::{HashMap, HashSet};

/// Which side to keep for a conflicted path (`ours` = HEAD, `theirs` = merged branch).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeResolveSide {
    Ours,
    Theirs,
}

impl MergeResolveSide {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ours => "ours",
            Self::Theirs => "theirs",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeResolution {
    pub path: String,
    pub side: MergeResolveSide,
}

pub fn parse_merge_resolution(value: &str) -> Result<MergeResolution, String> {
    let Some((path, side_str)) = value.rsplit_once(':') else {
        return Err(format!(
            "invalid --resolve format (expected path:ours|theirs): {value}"
        ));
    };
    if path.is_empty() {
        return Err(format!("invalid --resolve path: {value}"));
    }
    let side = match side_str {
        "ours" => MergeResolveSide::Ours,
        "theirs" => MergeResolveSide::Theirs,
        other => {
            return Err(format!(
                "invalid resolve side {other:?} (expected ours or theirs)"
            ));
        }
    };
    Ok(MergeResolution {
        path: path.to_string(),
        side,
    })
}

pub fn parse_merge_resolutions(values: &[String]) -> Result<Vec<MergeResolution>, String> {
    let mut resolutions = Vec::with_capacity(values.len());
    let mut seen = HashSet::new();
    for value in values {
        let resolution = parse_merge_resolution(value)?;
        if !seen.insert(resolution.path.clone()) {
            return Err(format!("duplicate --resolve for path: {}", resolution.path));
        }
        resolutions.push(resolution);
    }
    Ok(resolutions)
}

pub fn apply_merge_resolutions(
    plan: &mut MergePlan,
    head_files: &HashMap<String, TrackedFile>,
    other_files: &HashMap<String, TrackedFile>,
    resolutions: &[MergeResolution],
) -> Result<(), String> {
    let conflict_paths: HashSet<&str> = plan.conflicts.iter().map(|c| c.path.as_str()).collect();

    for resolution in resolutions {
        if !conflict_paths.contains(resolution.path.as_str()) {
            return Err(format!(
                "path not in merge conflicts (nothing to resolve): {}",
                resolution.path
            ));
        }
    }

    for resolution in resolutions {
        let content = match resolution.side {
            MergeResolveSide::Ours => head_files.get(&resolution.path).ok_or_else(|| {
                format!("ours (HEAD) side has no file at path: {}", resolution.path)
            })?,
            MergeResolveSide::Theirs => other_files.get(&resolution.path).ok_or_else(|| {
                format!(
                    "theirs (merged branch) side has no file at path: {}",
                    resolution.path
                )
            })?,
        };
        plan.merged_files
            .insert(resolution.path.clone(), content.clone());
        trace::notice(format!(
            "merge resolve: {} {}",
            resolution.path,
            resolution.side.as_str()
        ));
        plan.conflicts.retain(|c| c.path != resolution.path);
    }

    Ok(())
}
