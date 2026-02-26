use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use std::path::Path;
use std::process::Command;
use tracing::{debug, info, warn};

// ─── Public types ─────────────────────────────────────────────────────────────

/// Outcome of a single file commit operation.
#[derive(Debug, Clone)]
#[allow(dead_code)] // fields are used by callers via serde_json::json!
pub struct CommitResult {
    /// Repository slug, e.g. `"my-org/evo-king"`.
    pub repo: String,
    /// Path of the file that was committed, e.g. `"Cargo.toml"`.
    pub file_path: String,
    /// Strategy that succeeded.
    pub strategy: CommitStrategy,
    /// Commit SHA or a brief description of the local push.
    pub sha: String,
}

/// Which commit mechanism was used.
#[derive(Debug, Clone, PartialEq)]
pub enum CommitStrategy {
    /// GitHub CLI (`gh api`) — remote commit, no local checkout needed.
    GhCli,
    /// Local `git add / commit / push` — used as fallback when gh CLI fails.
    LocalGit,
}

// ─── Main commit entry-point ──────────────────────────────────────────────────

/// Commits `content` to `file_path` in `{org}/{repo}` with `message`.
///
/// Strategy order:
/// 1. **`gh` CLI** — uses the GitHub API via `gh api` to create/update the file
///    entirely in-memory; no local clone required.
/// 2. **Local git** — writes the file to `local_base/file_path`, then runs
///    `git add`, `git commit`, and `git push`.  Only attempted when
///    `local_base` is `Some(_)` and the gh CLI attempt fails (or when
///    `GITHUB_TOKEN` is not set).
///
/// Returns `Err` only if *both* strategies fail.
pub async fn commit_file(
    org: &str,
    repo: &str,
    file_path: &str,
    content: &str,
    message: &str,
    local_base: Option<&Path>,
) -> Result<CommitResult> {
    let slug = format!("{org}/{repo}");

    // ── Attempt 1: gh CLI ──────────────────────────────────────────────────
    match commit_via_gh_cli(&slug, file_path, content, message) {
        Ok(sha) => {
            info!(repo = %slug, file = file_path, sha = %sha, "committed via gh CLI");
            return Ok(CommitResult {
                repo: slug,
                file_path: file_path.to_string(),
                strategy: CommitStrategy::GhCli,
                sha,
            });
        }
        Err(e) => {
            warn!(
                repo = %slug,
                file = file_path,
                error = %e,
                "gh CLI commit failed — will try local git fallback"
            );
        }
    }

    // ── Attempt 2: local git ───────────────────────────────────────────────
    let base = local_base.with_context(|| {
        format!("gh CLI failed and no local_base provided for {slug}/{file_path}")
    })?;

    let sha = commit_via_local_git(base, file_path, content, message)
        .with_context(|| format!("local git commit failed for {slug}/{file_path}"))?;

    info!(repo = %slug, file = file_path, "committed via local git");
    Ok(CommitResult {
        repo: slug,
        file_path: file_path.to_string(),
        strategy: CommitStrategy::LocalGit,
        sha,
    })
}

// ─── gh CLI strategy ──────────────────────────────────────────────────────────

/// Commits `content` to `file_path` in `repo` (e.g. `"org/name"`) using the
/// GitHub REST API via `gh api`.
///
/// Uses `gh api repos/{repo}/contents/{file_path}` (PUT).  The current file
/// SHA is fetched first so GitHub can confirm we're updating the right blob.
fn commit_via_gh_cli(repo: &str, file_path: &str, content: &str, message: &str) -> Result<String> {
    // ── Fetch current blob SHA ──
    let sha_output = Command::new("gh")
        .args([
            "api",
            &format!("repos/{repo}/contents/{file_path}"),
            "--jq",
            ".sha",
        ])
        .output()
        .context("gh CLI not found or failed to run")?;

    if !sha_output.status.success() {
        let stderr = String::from_utf8_lossy(&sha_output.stderr);
        anyhow::bail!("gh api GET failed: {stderr}");
    }

    let blob_sha = String::from_utf8_lossy(&sha_output.stdout)
        .trim()
        .trim_matches('"')
        .to_string();

    debug!(file = file_path, blob_sha = %blob_sha, "fetched current blob SHA");

    // ── PUT updated content ──
    let encoded = BASE64.encode(content.as_bytes());

    let put_output = Command::new("gh")
        .args([
            "api",
            "--method",
            "PUT",
            &format!("repos/{repo}/contents/{file_path}"),
            "--field",
            &format!("message={message}"),
            "--field",
            &format!("content={encoded}"),
            "--field",
            &format!("sha={blob_sha}"),
            "--jq",
            ".commit.sha",
        ])
        .output()
        .context("gh api PUT failed")?;

    if !put_output.status.success() {
        let stderr = String::from_utf8_lossy(&put_output.stderr);
        anyhow::bail!("gh api PUT returned non-zero: {stderr}");
    }

    let commit_sha = String::from_utf8_lossy(&put_output.stdout)
        .trim()
        .trim_matches('"')
        .to_string();

    Ok(commit_sha)
}

// ─── Local git strategy ───────────────────────────────────────────────────────

/// Writes `content` to `base/file_path`, then runs `git add`, `git commit`,
/// and `git push` in `base`.
fn commit_via_local_git(
    base: &Path,
    file_path: &str,
    content: &str,
    message: &str,
) -> Result<String> {
    let full_path = base.join(file_path);

    // Ensure parent directory exists
    if let Some(parent) = full_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent dirs for {}", full_path.display()))?;
    }

    std::fs::write(&full_path, content)
        .with_context(|| format!("write {}", full_path.display()))?;

    // git add
    run_git(base, &["add", file_path]).with_context(|| format!("git add {file_path}"))?;

    // git commit
    run_git(base, &["commit", "-m", message]).with_context(|| "git commit")?;

    // git push
    run_git(base, &["push"]).with_context(|| "git push")?;

    // Return short SHA of HEAD
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(base)
        .output()
        .context("git rev-parse HEAD")?;

    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(sha)
}

/// Runs a git subcommand in `dir`, returns `Err` if it exits non-zero.
fn run_git(dir: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .with_context(|| format!("spawn git {:?}", args))?;

    if !status.success() {
        anyhow::bail!("git {:?} exited with {status}", args);
    }
    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Creates a working repo backed by a local bare remote so `git push` works.
    ///
    /// Returns `(working_dir, bare_dir)` — both must stay alive for the duration
    /// of the test so the temp dirs are not deleted.
    fn make_git_repo_with_remote() -> (TempDir, TempDir) {
        // 1. Bare "remote"
        let bare = TempDir::new().expect("bare tempdir");
        run_git(bare.path(), &["init", "--bare"]).expect("git init --bare");

        // 2. Working checkout
        let repo = TempDir::new().expect("repo tempdir");
        let path = repo.path();
        run_git(path, &["init"]).expect("git init");
        run_git(path, &["config", "user.email", "test@test.com"]).expect("git config email");
        run_git(path, &["config", "user.name", "Test"]).expect("git config name");

        // Point origin at the bare repo (local path is fine)
        let origin = bare.path().to_str().expect("bare path");
        run_git(path, &["remote", "add", "origin", origin]).expect("git remote add");

        // Create an initial commit and push it so origin/HEAD is set
        let readme = path.join("README.md");
        fs::write(&readme, "# test").expect("write readme");
        run_git(path, &["add", "README.md"]).expect("git add");
        run_git(path, &["commit", "-m", "init"]).expect("git commit");

        // Push; use -u to set upstream tracking
        run_git(path, &["push", "-u", "origin", "HEAD"]).expect("git push init");

        (repo, bare)
    }

    #[test]
    fn test_local_git_commit_new_file() {
        let (repo, _bare) = make_git_repo_with_remote();
        let result = commit_via_local_git(
            repo.path(),
            "Cargo.toml",
            "[package]\nname=\"x\"\n",
            "chore: update Cargo.toml",
        );
        assert!(
            result.is_ok(),
            "local git commit should succeed: {result:?}"
        );
        let sha = result.unwrap();
        assert!(!sha.is_empty());
    }

    #[test]
    fn test_local_git_commit_existing_file() {
        let (repo, _bare) = make_git_repo_with_remote();

        // Write initial version
        commit_via_local_git(repo.path(), "Cargo.toml", "version = \"0.1\"", "init Cargo").unwrap();

        // Update it
        let result = commit_via_local_git(
            repo.path(),
            "Cargo.toml",
            "version = \"0.2\"",
            "bump version",
        );
        assert!(result.is_ok());
    }
}
