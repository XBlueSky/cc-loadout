use std::path::{Path, PathBuf};

/// Directory names never worth descending into, shared by every filesystem walk
/// in this crate (repo enumeration here, glob detection in `detect.rs`) so the
/// two can't drift apart.
///
/// Two reasons a name belongs here:
///   - **Tooling metadata** (`.git`, `.repo`): these hold real git repos that are
///     an implementation detail of the checkout, not the user's projects. An
///     Android `repo`-tool tree keeps `.repo/manifests` and `.repo/repo` under
///     `.repo/`; enumerating them would apply a loadout to the tool's own clones.
///   - **Build output and dependency/cache trees**: they can hold tens of
///     thousands of files, so a non-matching glob would walk all of them on every
///     detect (the profile-view perf hot path), and any `.git` inside them
///     belongs to a vendored dependency or a test fixture rather than to a repo
///     the user works in. Detection should classify a repo by its own source,
///     never by generated artifacts, so this does not change real matches.
pub fn is_pruned_dir(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".repo"
            | "node_modules"
            | "dist"
            | "build"
            | "target"
            | "vendor"
            | ".venv"
            | "venv"
            | "__pycache__"
            | ".next"
            | ".tox"
            | ".gradle"
            | ".mypy_cache"
            | ".pytest_cache"
    )
}

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
        // `.git` marks a repo in ALL three of its on-disk forms, and
        // `entry.file_type()` is the un-followed dirent type, so testing for
        // `is_dir()` recognised only the first:
        //   - directory: a plain `git clone`
        //   - symlink:   an Android `repo`-tool checkout, where every project
        //                links to `../.repo/projects/<name>.git`
        //   - file:      a git worktree or submodule (`gitdir: …`)
        // Match on the name first so all three count, and so a `.git` of any
        // form is never descended into.
        let name = entry.file_name();
        if name == ".git" {
            is_repo = true;
        } else if entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
            && !is_pruned_dir(&name.to_string_lossy())
        {
            subdirs.push(entry.path());
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

    /// An Android `repo`-tool checkout (`/synosrc/source`) links every project's
    /// `.git` into `.repo/projects/<name>.git`, and a git worktree/submodule
    /// writes `.git` as a `gitdir:` FILE. Neither is a directory, so classifying
    /// entries by dirent type alone made those repos invisible to `apply --all`.
    #[test]
    fn finds_repos_whose_dot_git_is_a_symlink_or_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // repo-tool layout: real bare dir under .repo, project links to it.
        std::fs::create_dir_all(root.join(".repo").join("projects").join("linked.git")).unwrap();
        std::fs::create_dir_all(root.join("linked")).unwrap();
        std::os::unix::fs::symlink(
            root.join(".repo").join("projects").join("linked.git"),
            root.join("linked").join(".git"),
        )
        .unwrap();

        // worktree/submodule layout: .git is a file.
        std::fs::create_dir_all(root.join("worktree")).unwrap();
        std::fs::write(
            root.join("worktree").join(".git"),
            "gitdir: /elsewhere/.git/worktrees/wt\n",
        )
        .unwrap();

        let repos = find_git_repos(root, 6);
        assert!(
            repos.contains(&root.join("linked")),
            "symlinked .git must count as a repo: {repos:?}"
        );
        assert!(
            repos.contains(&root.join("worktree")),
            "gitdir-file .git must count as a repo: {repos:?}"
        );
    }

    /// The repo tool's own clones live under `.repo/`, and build output can hold
    /// fixture or vendored clones. Both are real git repos, so only pruning by
    /// name keeps them out of the loadout.
    #[test]
    fn prunes_tooling_and_build_trees_that_contain_real_repos() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".repo").join("repo").join(".git")).unwrap();
        std::fs::create_dir_all(root.join(".repo").join("manifests").join(".git")).unwrap();
        std::fs::create_dir_all(
            root.join("proj")
                .join("target")
                .join("fixture")
                .join(".git"),
        )
        .unwrap();
        std::fs::create_dir_all(
            root.join("proj")
                .join("node_modules")
                .join("dep")
                .join(".git"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("proj").join(".git")).unwrap();

        assert_eq!(find_git_repos(root, 6), vec![root.join("proj")]);
    }

    #[test]
    fn missing_root_is_empty() {
        let repos = find_git_repos(std::path::Path::new("/no/such/dir"), 6);
        assert!(repos.is_empty());
    }
}
