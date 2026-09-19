use std::path::Path;
use std::path::PathBuf;

use super::DiffPanelFile;
use crate::diff_model::FileChange;
use crate::diff_render::calculate_add_remove_from_diff;

pub(super) fn parse_git_diff(text: &str) -> Vec<DiffPanelFile> {
    let mut sections = Vec::new();
    let mut current = String::new();
    for line in text.split_inclusive('\n') {
        if line.starts_with("diff --git ") && !current.is_empty() {
            sections.push(std::mem::take(&mut current));
        }
        current.push_str(line);
    }
    if !current.is_empty() {
        sections.push(current);
    }

    sections
        .into_iter()
        .filter_map(|section| {
            let path = section
                .lines()
                .find_map(|line| line.strip_prefix("rename to ").map(PathBuf::from))
                .or_else(|| {
                    codex_git_utils::extract_paths_from_patch(&section)
                        .into_iter()
                        .rfind(|path| !path.eq_ignore_ascii_case("NUL"))
                        .map(PathBuf::from)
                })?;
            let patch_start = section
                .strip_prefix("--- ")
                .map(|_| 0)
                .or_else(|| section.find("\n--- ").map(|index| index.saturating_add(1)));
            let change = patch_start.map(|start| FileChange::Update {
                unified_diff: section[start..].to_string(),
                move_path: None,
            });
            let (added, removed) = change.as_ref().map_or((0, 0), |change| match change {
                FileChange::Update { unified_diff, .. } => {
                    calculate_add_remove_from_diff(unified_diff)
                }
                FileChange::Add { .. } | FileChange::Delete { .. } => (0, 0),
            });
            Some(DiffPanelFile {
                path,
                added,
                removed,
                change,
            })
        })
        .collect()
}

pub(super) fn paths_match(left: &Path, right: &Path) -> bool {
    let left = normalize_path(left);
    let right = normalize_path(right);
    left == right
        || left
            .strip_suffix(&right)
            .is_some_and(|prefix| prefix.ends_with('/'))
        || right
            .strip_suffix(&left)
            .is_some_and(|prefix| prefix.ends_with('/'))
}

fn normalize_path(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        path.to_lowercase()
    } else {
        path
    }
}
