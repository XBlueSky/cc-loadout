use std::path::{Path, PathBuf};

/// Find git repos (dirs containing a `.git` entry) under `root`, up to `max_depth`.
/// A repo's `.git` is not descended into, but the repo's other subdirs ARE walked
/// (so nested repos are found) — matching `find -maxdepth N -name .git -prune`.
pub fn find_git_repos(root: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, 0, max_depth, &mut out);
    out.sort();
    out.dedup();
    out
}

fn walk(dir: &Path, depth: usize, max_depth: usize, out: &mut Vec<PathBuf>) {
    if depth > max_depth {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut subdirs = Vec::new();
    let mut is_repo = false;
    for entry in entries.flatten() {
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if entry.file_name() == ".git" {
                is_repo = true;
            } else {
                subdirs.push(entry.path());
            }
        }
    }
    if is_repo {
        out.push(dir.to_path_buf());
    }
    for sub in subdirs {
        walk(&sub, depth + 1, max_depth, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_git_repos_and_prunes_dot_git() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("repo1").join(".git")).unwrap();
        std::fs::create_dir_all(root.join("repo1").join("src")).unwrap();
        std::fs::create_dir_all(root.join("group").join("repo2").join(".git")).unwrap();
        std::fs::create_dir_all(root.join("plain")).unwrap();

        let mut repos = find_git_repos(root, 6);
        repos.sort();
        assert_eq!(
            repos,
            vec![root.join("group").join("repo2"), root.join("repo1")]
        );
    }

    #[test]
    fn missing_root_is_empty() {
        let repos = find_git_repos(std::path::Path::new("/no/such/dir"), 6);
        assert!(repos.is_empty());
    }
}
