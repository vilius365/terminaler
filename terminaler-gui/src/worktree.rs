//! Git worktree helpers for the New Claude Agent flow.
//!
//! All functions shell out to `git` (or walk the filesystem) synchronously;
//! callers must not invoke them on the GUI thread. The overlay closure runs
//! on a dedicated thread, which is where these are used.

use anyhow::{anyhow, bail, Context};
use std::path::{Path, PathBuf};

/// Walk up from `path` to find the root of a git checkout.
/// Handles both regular checkouts (`.git` directory) and worktrees
/// (`.git` file containing a `gitdir:` pointer).
pub fn find_git_repo_root(path: &Path) -> Option<PathBuf> {
    let mut dir = path.to_path_buf();
    for _ in 0..20 {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() || dot_git.is_file() {
            return Some(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// Validate a branch/worktree name typed by the user.
pub fn validate_branch_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        bail!("branch name is empty");
    }
    if name.starts_with('-') || name.ends_with('/') || name.ends_with(".lock") {
        bail!("invalid branch name: {name}");
    }
    if name.contains("..")
        || name
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '\\' | ':' | '~' | '^' | '?' | '*' | '['))
    {
        bail!("branch name contains invalid characters: {name}");
    }
    Ok(())
}

/// Compute the worktree directory for a branch name.
/// `worktree_root` overrides the default `<repo-parent>/<repo-name>-worktrees/`.
/// Slashes in the branch name are flattened to dashes for the directory name.
pub fn worktree_path_for(
    repo_root: &Path,
    branch: &str,
    worktree_root: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    let dir_name = branch.replace('/', "-");
    let root = match worktree_root {
        Some(root) => root.to_path_buf(),
        None => {
            let repo_name = repo_root
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| anyhow!("cannot determine repo name from {repo_root:?}"))?;
            repo_root
                .parent()
                .ok_or_else(|| anyhow!("repo {repo_root:?} has no parent directory"))?
                .join(format!("{repo_name}-worktrees"))
        }
    };
    Ok(root.join(dir_name))
}

fn git_command(repo_root: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(repo_root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(winapi::um::winbase::CREATE_NO_WINDOW);
    }
    cmd
}

fn run_git(repo_root: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = git_command(repo_root)
        .args(args)
        .output()
        .with_context(|| format!("failed to run git {args:?}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
}

/// Create (or reuse) a worktree for `branch` and return its path.
///
/// - If the target directory already exists it is assumed to be the
///   worktree for that branch and is reused.
/// - Tries `git worktree add <path> -b <branch>` first; if the branch
///   already exists, retries checking out the existing branch.
pub fn create_worktree(
    repo_root: &Path,
    branch: &str,
    worktree_root: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    validate_branch_name(branch)?;
    let path = worktree_path_for(repo_root, branch, worktree_root)?;

    if path.exists() {
        if path.join(".git").exists() {
            log::info!("reusing existing worktree at {path:?}");
            return Ok(path);
        }
        bail!("{path:?} exists but is not a git worktree");
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating worktree root {parent:?}"))?;
    }

    let path_str = path
        .to_str()
        .ok_or_else(|| anyhow!("worktree path {path:?} is not valid unicode"))?;

    match run_git(repo_root, &["worktree", "add", path_str, "-b", branch]) {
        Ok(_) => Ok(path),
        Err(first_err) => {
            // The branch may already exist; retry checking it out instead.
            match run_git(repo_root, &["worktree", "add", path_str, branch]) {
                Ok(_) => Ok(path),
                Err(_) => Err(first_err),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_branch_name() {
        assert!(validate_branch_name("agent/foo").is_ok());
        assert!(validate_branch_name("fix-123").is_ok());
        assert!(validate_branch_name("").is_err());
        assert!(validate_branch_name("a b").is_err());
        assert!(validate_branch_name("../evil").is_err());
        assert!(validate_branch_name("-flag").is_err());
        assert!(validate_branch_name("a:b").is_err());
    }

    #[test]
    fn test_worktree_path_for() {
        let path = worktree_path_for(Path::new("/home/u/proj/repo"), "agent/foo", None).unwrap();
        assert_eq!(path, PathBuf::from("/home/u/proj/repo-worktrees/agent-foo"));

        let path = worktree_path_for(
            Path::new("/home/u/proj/repo"),
            "fix",
            Some(Path::new("/tmp/wt")),
        )
        .unwrap();
        assert_eq!(path, PathBuf::from("/tmp/wt/fix"));
    }

    #[test]
    fn test_find_repo_root_and_create_worktree() {
        let tmp = std::env::temp_dir().join(format!("terminaler-wt-test-{}", std::process::id()));
        let repo = tmp.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(repo.join("f.txt"), "hi").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "init"]);

        let nested = repo.join("src");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(find_git_repo_root(&nested), Some(repo.clone()));

        let wt = create_worktree(&repo, "agent/test", None).unwrap();
        assert!(wt.join(".git").exists());
        assert_eq!(find_git_repo_root(&wt), Some(wt.clone()));

        // Re-running reuses the existing worktree.
        let wt2 = create_worktree(&repo, "agent/test", None).unwrap();
        assert_eq!(wt, wt2);

        std::fs::remove_dir_all(&tmp).ok();
    }
}
