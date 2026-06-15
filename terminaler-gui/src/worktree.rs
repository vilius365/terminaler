//! Git worktree helpers for the New Claude Agent flow.
//!
//! All functions shell out to `git` (or walk the filesystem) synchronously;
//! callers must not invoke them on the GUI thread. The overlay closure runs
//! on a dedicated thread, which is where these are used.

use anyhow::{anyhow, bail, Context};
use std::path::{Path, PathBuf};

/// Resolve a pane's working-directory URL to a LOCAL filesystem path, or
/// return a clear error for non-local panes (WSL / SSH / remote).
///
/// WSL and remote cwds arrive as `file://` URLs whose authority is the remote
/// hostname (e.g. `file://Home/home/vilius`). On Windows `Url::to_file_path()`
/// turns a non-`localhost` authority into a bogus UNC path
/// (`\\Home\home\vilius`) instead of erroring, so the worktree flow — which
/// shells out to Windows-host git — would walk the wrong path and report a
/// misleading "not a git repository". Reject those explicitly with guidance.
pub fn local_path_from_cwd_url(url: &url::Url) -> anyhow::Result<PathBuf> {
    if url.scheme() != "file" {
        bail!(
            "this pane's working directory isn't local (scheme `{}`) — run this \
             from a local pane in a Windows git repository",
            url.scheme()
        );
    }
    match url.host_str() {
        None | Some("") | Some("localhost") => {}
        Some(host) => bail!(
            "this pane runs on `{host}` (WSL or remote); worktree actions currently \
             support local panes only — run from a local pane in a Windows git \
             repository (WSL support is coming)"
        ),
    }
    url.to_file_path()
        .map_err(|_| anyhow!("could not resolve {url} to a local path"))
}

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

/// Where git runs for a worktree action: on the local host, or inside a WSL
/// distro. For `Wsl`, all paths are Linux paths and git is invoked through
/// `wsl.exe --distribution <distro> --exec git …` (so `std::fs` is never used
/// against the distro's filesystem; repo discovery and directory creation go
/// through git itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitEnv {
    Local,
    Wsl { distro: String },
}

impl GitEnv {
    fn base_command(&self) -> std::process::Command {
        let mut cmd = match self {
            GitEnv::Local => std::process::Command::new("git"),
            GitEnv::Wsl { distro } => {
                let mut c = std::process::Command::new("wsl.exe");
                c.args(["--distribution", distro, "--exec", "git"]);
                c
            }
        };
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(winapi::um::winbase::CREATE_NO_WINDOW);
        }
        cmd
    }

    fn run(&self, args: &[&str]) -> anyhow::Result<String> {
        let output = self
            .base_command()
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
}

/// Directory name for a branch's worktree (slashes flattened to dashes).
fn branch_dir_slug(branch: &str) -> String {
    branch.replace('/', "-")
}

/// Resolve the WSL distro for a pane's domain name, given the configured WSL
/// domains. Returns None for non-WSL (local) domains. Domains named
/// `WSL:<distro>` are treated as WSL even when not explicitly configured.
pub fn wsl_distro_from_domain_name(
    domain_name: &str,
    wsl_domains: &[config::WslDomain],
) -> Option<String> {
    if let Some(d) = wsl_domains.iter().find(|d| d.name == domain_name) {
        return Some(
            d.distribution
                .clone()
                .unwrap_or_else(|| domain_name.strip_prefix("WSL:").unwrap_or(domain_name).to_string()),
        );
    }
    domain_name.strip_prefix("WSL:").map(|s| s.to_string())
}

/// Find the git repo root containing `cwd`, in the given env. Returns a path
/// string in the env's space (native for Local, Linux for WSL).
pub fn find_repo_root_in(env: &GitEnv, cwd: &str) -> anyhow::Result<String> {
    match env {
        GitEnv::Local => find_git_repo_root(Path::new(cwd))
            .map(|p| p.to_string_lossy().into_owned())
            .ok_or_else(|| anyhow!("{cwd} is not inside a git repository")),
        GitEnv::Wsl { .. } => {
            let top = env
                .run(&["-C", cwd, "rev-parse", "--show-toplevel"])
                .map_err(|_| anyhow!("{cwd} is not inside a git repository"))?;
            let top = top.trim();
            if top.is_empty() {
                bail!("{cwd} is not inside a git repository");
            }
            Ok(top.to_string())
        }
    }
}

/// Linux worktree path for a WSL repo: `<repo-parent>/<repo>-worktrees/<slug>`.
fn wsl_worktree_path(repo_root: &str, branch: &str) -> anyhow::Result<String> {
    let repo_root = repo_root.trim_end_matches('/');
    let (parent, name) = repo_root
        .rsplit_once('/')
        .ok_or_else(|| anyhow!("cannot derive a worktree path from {repo_root}"))?;
    Ok(format!(
        "{parent}/{name}-worktrees/{}",
        branch_dir_slug(branch)
    ))
}

/// Create (or reuse) a worktree, dispatching on env. Returns the worktree path
/// in the env's space (native string for Local, Linux string for WSL).
pub fn create_worktree_in(
    env: &GitEnv,
    repo_root: &str,
    branch: &str,
    worktree_root: Option<&Path>,
) -> anyhow::Result<String> {
    match env {
        GitEnv::Local => create_worktree(Path::new(repo_root), branch, worktree_root)
            .map(|p| p.to_string_lossy().into_owned()),
        GitEnv::Wsl { .. } => create_worktree_wsl(env, repo_root, branch),
    }
}

fn create_worktree_wsl(env: &GitEnv, repo_root: &str, branch: &str) -> anyhow::Result<String> {
    validate_branch_name(branch)?;
    let path = wsl_worktree_path(repo_root, branch)?;
    // git creates the directory inside the distro. Try a new branch first,
    // then an existing branch; if a worktree already lives at that path,
    // treat it as a reuse.
    match env.run(&["-C", repo_root, "worktree", "add", &path, "-b", branch]) {
        Ok(_) => Ok(path),
        Err(first_err) => match env.run(&["-C", repo_root, "worktree", "add", &path, branch]) {
            Ok(_) => Ok(path),
            Err(_) => {
                if env.run(&["-C", &path, "rev-parse", "--git-dir"]).is_ok() {
                    Ok(path)
                } else {
                    Err(first_err)
                }
            }
        },
    }
}

/// A worktree belonging to a repository, as reported by
/// `git worktree list --porcelain`. Paths are strings in the env's space
/// (native for Local, Linux for WSL) — never `PathBuf`, so WSL Linux paths
/// aren't mangled by `\`-separator handling on a Windows host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub path: String,
    /// Short branch name (e.g. "agent/foo"), or None for a detached HEAD.
    pub branch: Option<String>,
    /// The primary checkout (first entry); cannot be removed.
    pub is_main: bool,
    /// A bare repository entry (has no working tree).
    pub is_bare: bool,
    /// Whether the working tree has uncommitted changes.
    pub dirty: bool,
}

/// Whether a worktree's working tree has uncommitted changes. Returns false
/// if git can't be queried — only suitable for non-destructive display (the
/// list). Destructive callers must use `ensure_clean`, which fails closed.
pub fn is_dirty(env: &GitEnv, worktree_path: &str) -> bool {
    env.run(&["-C", worktree_path, "status", "--porcelain"])
        .map(|out| !out.trim().is_empty())
        .unwrap_or(false)
}

/// Verify a worktree is clean, failing closed: an error here means git could
/// not be queried, so the state is unknown and a destructive action must NOT
/// proceed (unlike `is_dirty`, which treats unknown as clean for display).
fn ensure_clean(env: &GitEnv, worktree_path: &str) -> anyhow::Result<()> {
    let out = env
        .run(&["-C", worktree_path, "status", "--porcelain"])
        .map_err(|e| anyhow!("could not determine worktree state: {e:#}"))?;
    if !out.trim().is_empty() {
        bail!("worktree has uncommitted changes; commit or discard them before merging");
    }
    Ok(())
}

/// List all worktrees of the repository, with dirty state computed for each.
/// Works when invoked from any worktree of the repo. The first entry is the
/// main worktree.
pub fn list_worktrees(env: &GitEnv, repo_root: &str) -> anyhow::Result<Vec<WorktreeInfo>> {
    let out = env.run(&["-C", repo_root, "worktree", "list", "--porcelain"])?;
    let mut result = Vec::new();

    for block in out.split("\n\n") {
        let block = block.trim();
        if block.is_empty() {
            continue;
        }
        let mut path: Option<String> = None;
        let mut branch: Option<String> = None;
        let mut is_bare = false;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(p.to_string());
            } else if let Some(b) = line.strip_prefix("branch ") {
                branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
            } else if line == "bare" {
                is_bare = true;
            }
            // "HEAD <sha>", "detached", "locked", "prunable" are ignored.
        }
        if let Some(path) = path {
            result.push(WorktreeInfo {
                path,
                branch,
                is_main: false,
                is_bare,
                dirty: false,
            });
        }
    }

    if let Some(first) = result.first_mut() {
        first.is_main = true;
    }
    for wt in &mut result {
        if !wt.is_bare {
            wt.dirty = is_dirty(env, &wt.path);
        }
    }
    Ok(result)
}

/// Remove a worktree. `force` is required when the worktree is dirty or locked.
pub fn remove_worktree(
    env: &GitEnv,
    repo_root: &str,
    worktree_path: &str,
    force: bool,
) -> anyhow::Result<()> {
    let mut args = vec!["-C", repo_root, "worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(worktree_path);
    env.run(&args)?;
    Ok(())
}

/// Delete a branch. `force` uses `-D` (delete even if unmerged) instead of `-d`.
pub fn delete_branch(
    env: &GitEnv,
    repo_root: &str,
    branch: &str,
    force: bool,
) -> anyhow::Result<()> {
    let flag = if force { "-D" } else { "-d" };
    env.run(&["-C", repo_root, "branch", flag, branch])?;
    Ok(())
}

/// Merge `branch` into whatever the main worktree currently has checked out,
/// running the merge in `main_worktree`. On failure the merge is aborted so the
/// main checkout is never left mid-merge.
pub fn merge_branch(env: &GitEnv, main_worktree: &str, branch: &str) -> anyhow::Result<()> {
    match env.run(&["-C", main_worktree, "merge", "--no-ff", branch]) {
        Ok(_) => Ok(()),
        Err(err) => {
            let _ = env.run(&["-C", main_worktree, "merge", "--abort"]);
            Err(err)
        }
    }
}

/// Force-remove a worktree and force-delete its branch — discards the work.
/// Returns a human-readable summary of what happened.
pub fn discard_worktree(
    env: &GitEnv,
    repo_root: &str,
    wt: &WorktreeInfo,
) -> anyhow::Result<String> {
    if wt.is_main {
        bail!("cannot remove the main worktree");
    }
    remove_worktree(env, repo_root, &wt.path, true)?;
    let mut msg = format!("Removed worktree {}", wt.path);
    if let Some(branch) = &wt.branch {
        match delete_branch(env, repo_root, branch, true) {
            Ok(_) => msg.push_str(&format!(", deleted branch `{branch}`")),
            Err(e) => {
                msg.push_str(&format!(", but branch `{branch}` was not deleted: {e:#}"))
            }
        }
    }
    Ok(msg)
}

/// Merge the worktree's branch into the main worktree, then remove the
/// worktree and delete the (now-merged) branch. Refuses if the worktree is
/// dirty. Returns a human-readable summary.
pub fn merge_and_remove(
    env: &GitEnv,
    repo_root: &str,
    main_worktree: &str,
    wt: &WorktreeInfo,
) -> anyhow::Result<String> {
    if wt.is_main {
        bail!("cannot merge/remove the main worktree");
    }
    let branch = wt
        .branch
        .as_deref()
        .ok_or_else(|| anyhow!("worktree has a detached HEAD; nothing to merge"))?;
    ensure_clean(env, &wt.path)?;

    merge_branch(env, main_worktree, branch)?;
    remove_worktree(env, repo_root, &wt.path, false)?;
    delete_branch(env, repo_root, branch, false)?;
    Ok(format!(
        "Merged `{branch}` into the main worktree, removed {} and deleted the branch",
        wt.path
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wsl_worktree_path() {
        assert_eq!(
            wsl_worktree_path("/home/v/projects/repo", "agent/foo").unwrap(),
            "/home/v/projects/repo-worktrees/agent-foo"
        );
        assert_eq!(
            wsl_worktree_path("/home/v/projects/repo/", "fix").unwrap(),
            "/home/v/projects/repo-worktrees/fix"
        );
    }

    #[test]
    fn test_wsl_distro_from_domain_name() {
        // "WSL:<distro>" fallback with no config.
        assert_eq!(
            wsl_distro_from_domain_name("WSL:Ubuntu", &[]).as_deref(),
            Some("Ubuntu")
        );
        // Non-WSL domain.
        assert_eq!(wsl_distro_from_domain_name("local", &[]), None);
        // Config entry takes its explicit distribution.
        let domains = vec![config::WslDomain {
            name: "WSL:dev".to_string(),
            distribution: Some("Ubuntu-22.04".to_string()),
            ..Default::default()
        }];
        assert_eq!(
            wsl_distro_from_domain_name("WSL:dev", &domains).as_deref(),
            Some("Ubuntu-22.04")
        );
    }

    #[test]
    fn test_local_path_from_cwd_url() {
        use url::Url;
        // Local file URL (empty authority) resolves.
        let local = Url::parse("file:///home/u/proj").unwrap();
        assert!(local_path_from_cwd_url(&local).is_ok());
        // localhost authority is also local.
        let lh = Url::parse("file://localhost/home/u").unwrap();
        assert!(local_path_from_cwd_url(&lh).is_ok());
        // WSL/remote authority (hostname) is rejected with guidance.
        let wsl = Url::parse("file://Home/home/vilius").unwrap();
        let err = local_path_from_cwd_url(&wsl).unwrap_err().to_string();
        assert!(
            err.contains("WSL or remote") || err.contains("local panes only"),
            "unexpected error: {err}"
        );
        // Non-file scheme is rejected.
        let ssh = Url::parse("ssh://host/path").unwrap();
        assert!(local_path_from_cwd_url(&ssh).is_err());
    }

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

    /// Run git in `dir`, asserting success.
    fn git_in(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git -C {dir:?} {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo() -> (PathBuf, PathBuf) {
        let tmp = std::env::temp_dir().join(format!(
            "terminaler-wt-lifecycle-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let repo = tmp.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_in(&repo, &["init", "-b", "main"]);
        git_in(&repo, &["config", "user.email", "t@t"]);
        git_in(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("base.txt"), "base").unwrap();
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-m", "init"]);
        (tmp, repo)
    }

    #[test]
    fn test_list_worktrees() {
        let (tmp, repo) = init_repo();
        let wt = create_worktree(&repo, "agent/foo", None).unwrap();
        // Make the worktree dirty.
        std::fs::write(wt.join("dirty.txt"), "x").unwrap();

        let list = list_worktrees(&GitEnv::Local, repo.to_str().unwrap()).unwrap();
        assert_eq!(list.len(), 2);
        assert!(list[0].is_main, "first entry is the main worktree");
        assert_eq!(list[0].branch.as_deref(), Some("main"));

        let foo = list.iter().find(|w| w.path == wt.to_str().unwrap()).unwrap();
        assert_eq!(foo.branch.as_deref(), Some("agent/foo"));
        assert!(!foo.is_main);
        assert!(foo.dirty, "untracked file makes it dirty");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_merge_and_remove() {
        let (tmp, repo) = init_repo();
        let wt = create_worktree(&repo, "agent/feat", None).unwrap();
        std::fs::write(wt.join("feature.txt"), "new feature").unwrap();
        git_in(&wt, &["add", "."]);
        git_in(&wt, &["commit", "-m", "add feature"]);

        let repo_str = repo.to_str().unwrap();
        let list = list_worktrees(&GitEnv::Local, repo_str).unwrap();
        let main_path = list.iter().find(|w| w.is_main).unwrap().path.clone();
        let feat = list.iter().find(|w| w.path == wt.to_str().unwrap()).unwrap().clone();

        let msg = merge_and_remove(&GitEnv::Local, repo_str, &main_path, &feat).unwrap();
        assert!(msg.contains("agent/feat"), "summary mentions the branch");

        // The feature file is now in the main worktree.
        assert!(Path::new(&main_path).join("feature.txt").exists());
        // Worktree directory is gone and no longer listed.
        let after = list_worktrees(&GitEnv::Local, repo_str).unwrap();
        assert_eq!(after.len(), 1);
        assert!(after[0].is_main);

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_merge_and_remove_refuses_dirty() {
        let (tmp, repo) = init_repo();
        let wt = create_worktree(&repo, "agent/dirty", None).unwrap();
        std::fs::write(wt.join("uncommitted.txt"), "wip").unwrap();

        let repo_str = repo.to_str().unwrap();
        let list = list_worktrees(&GitEnv::Local, repo_str).unwrap();
        let main_path = list.iter().find(|w| w.is_main).unwrap().path.clone();
        let dirty = list.iter().find(|w| w.path == wt.to_str().unwrap()).unwrap().clone();

        let err = merge_and_remove(&GitEnv::Local, repo_str, &main_path, &dirty).unwrap_err();
        assert!(err.to_string().contains("uncommitted"));
        // Worktree is untouched.
        assert!(wt.exists());

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_discard_worktree() {
        let (tmp, repo) = init_repo();
        let wt = create_worktree(&repo, "agent/junk", None).unwrap();
        // Dirty, uncommitted — discard must force through it.
        std::fs::write(wt.join("scratch.txt"), "garbage").unwrap();

        let repo_str = repo.to_str().unwrap();
        let list = list_worktrees(&GitEnv::Local, repo_str).unwrap();
        let junk = list.iter().find(|w| w.path == wt.to_str().unwrap()).unwrap().clone();

        discard_worktree(&GitEnv::Local, repo_str, &junk).unwrap();
        assert!(!wt.exists(), "worktree directory removed");

        let after = list_worktrees(&GitEnv::Local, repo_str).unwrap();
        assert_eq!(after.len(), 1);
        // Branch is gone too.
        let branches = GitEnv::Local
            .run(&["-C", repo_str, "branch", "--list", "agent/junk"])
            .unwrap();
        assert!(branches.trim().is_empty(), "branch deleted");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_discard_refuses_main() {
        let (tmp, repo) = init_repo();
        let list = list_worktrees(&GitEnv::Local, repo.to_str().unwrap()).unwrap();
        let main = list[0].clone();
        assert!(discard_worktree(&GitEnv::Local, repo.to_str().unwrap(), &main).is_err());
        std::fs::remove_dir_all(&tmp).ok();
    }
}
