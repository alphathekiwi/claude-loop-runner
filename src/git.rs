use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;
use tracing::{debug, info};

/// Represents the git state captured before starting the task runner
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GitState {
    /// The original branch we were on before starting
    pub original_branch: Option<String>,
    /// The branch created for this task
    pub task_branch: Option<String>,
    /// Files that were dirty (modified/untracked) before we started
    pub pre_existing_dirty_files: HashSet<PathBuf>,
    /// Whether git operations are enabled
    pub enabled: bool,
    /// Global allowlist patterns for all files being processed
    /// This prevents false "unauthorized" warnings when multiple workers run in parallel
    pub global_allowlist_patterns: Vec<String>,
}

impl GitState {
    /// Create a new GitState by capturing the current git status
    pub async fn capture(working_dir: &Path) -> Result<Self> {
        // Check if we're in a git repo
        if !is_git_repo(working_dir).await? {
            info!("Not a git repository, git features disabled");
            return Ok(Self {
                enabled: false,
                ..Default::default()
            });
        }

        let original_branch = get_current_branch(working_dir).await?;
        let dirty_files = get_dirty_files(working_dir).await?;

        if !dirty_files.is_empty() {
            info!(
                count = dirty_files.len(),
                "Captured pre-existing dirty files"
            );
            for file in &dirty_files {
                debug!(file = %file.display(), "Pre-existing dirty file");
            }
        }

        Ok(Self {
            original_branch: Some(original_branch),
            task_branch: None,
            pre_existing_dirty_files: dirty_files,
            enabled: true,
            global_allowlist_patterns: Vec::new(),
        })
    }

    /// Check if a file was dirty before we started (should be ignored for unauthorized checks)
    pub fn was_pre_existing_dirty(&self, path: &Path) -> bool {
        self.pre_existing_dirty_files.contains(path)
    }

    /// Get files that are newly modified (not pre-existing dirty)
    #[allow(dead_code)]
    pub fn filter_new_changes(&self, changed_files: &[PathBuf]) -> Vec<PathBuf> {
        changed_files
            .iter()
            .filter(|p| !self.pre_existing_dirty_files.contains(*p))
            .cloned()
            .collect()
    }

    /// Add an allowlist pattern for a file being processed
    pub fn add_allowlist_pattern(&mut self, pattern: String) {
        if !self.global_allowlist_patterns.contains(&pattern) {
            self.global_allowlist_patterns.push(pattern);
        }
    }

    /// Check if a path matches any of the global allowlist patterns
    pub fn matches_global_allowlist(&self, path: &Path) -> bool {
        use crate::process::matches_allowlist;
        self.global_allowlist_patterns
            .iter()
            .any(|pattern| matches_allowlist(path, pattern))
    }
}

/// Check if a directory is a git repository
pub async fn is_git_repo(working_dir: &Path) -> Result<bool> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(working_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .context("Failed to check if git repo")?;

    Ok(output.success())
}

/// Get the current branch name
pub async fn get_current_branch(working_dir: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("Failed to get current branch")?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to get current branch: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Get all dirty files (modified, added, deleted, untracked)
pub async fn get_dirty_files(working_dir: &Path) -> Result<HashSet<PathBuf>> {
    let output = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("Failed to run git status")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut files = HashSet::new();

    for line in stdout.lines() {
        if line.len() < 3 {
            continue;
        }
        let file_path = line[3..].trim();
        if !file_path.is_empty() {
            // Handle renamed files (format: "R  old -> new")
            if let Some(arrow_pos) = file_path.find(" -> ") {
                files.insert(PathBuf::from(&file_path[arrow_pos + 4..]));
            } else {
                files.insert(PathBuf::from(file_path));
            }
        }
    }

    Ok(files)
}

/// Create and checkout a new branch for the task
pub async fn create_task_branch(working_dir: &Path, task_id: &str) -> Result<String> {
    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let branch_name = format!("claude-loop/{}-{}", task_id, timestamp);

    let output = Command::new("git")
        .args(["checkout", "-b", &branch_name])
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("Failed to create task branch")?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to create branch '{}': {}",
            branch_name,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    info!(branch = %branch_name, "Created and checked out task branch");
    Ok(branch_name)
}

/// Checkout an existing branch
#[allow(dead_code)]
pub async fn checkout_branch(working_dir: &Path, branch: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["checkout", branch])
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("Failed to checkout branch")?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to checkout branch '{}': {}",
            branch,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    info!(branch = %branch, "Checked out branch");
    Ok(())
}

/// Stage specific files for commit
pub async fn stage_files(working_dir: &Path, files: &[PathBuf]) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }

    let file_args: Vec<&str> = files.iter().filter_map(|p| p.to_str()).collect();

    let output = Command::new("git")
        .arg("add")
        .args(&file_args)
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("Failed to stage files")?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to stage files: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    debug!(files = ?file_args, "Staged files");
    Ok(())
}

/// Commit staged changes with a message
pub async fn commit(working_dir: &Path, message: &str) -> Result<String> {
    let output = Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("Failed to commit")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // "nothing to commit" is not an error for our purposes
        if stderr.contains("nothing to commit") {
            debug!("Nothing to commit");
            return Ok(String::new());
        }
        anyhow::bail!("Failed to commit: {}", stderr);
    }

    // Get the commit hash
    let hash_output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .output()
        .await?;

    let commit_hash = String::from_utf8_lossy(&hash_output.stdout)
        .trim()
        .to_string();

    info!(hash = %commit_hash, "Created commit");
    Ok(commit_hash)
}

/// Stage and commit changes for a specific file
pub async fn commit_file_changes(
    working_dir: &Path,
    file_path: &Path,
    task_description: Option<&str>,
) -> Result<Option<String>> {
    // Get files that changed (related to this file path)
    let dirty_files = get_dirty_files(working_dir).await?;

    // Find files that match the file_path pattern (including related test files, etc.)
    let file_stem = file_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    let related_files: Vec<PathBuf> = dirty_files
        .into_iter()
        .filter(|p| {
            let p_str = p.to_string_lossy();
            p_str.contains(&file_stem) || p == file_path
        })
        .collect();

    if related_files.is_empty() {
        debug!(file = %file_path.display(), "No changes to commit for file");
        return Ok(None);
    }

    // Stage the files
    stage_files(working_dir, &related_files).await?;

    // Build commit message
    let file_name = file_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| file_path.display().to_string());

    let message = if let Some(desc) = task_description {
        format!("claude-loop: {} ({})", file_name, desc)
    } else {
        format!("claude-loop: {}", file_name)
    };

    let commit_hash = commit(working_dir, &message).await?;

    if commit_hash.is_empty() {
        Ok(None)
    } else {
        Ok(Some(commit_hash))
    }
}

/// Get the diff for staged files
#[allow(dead_code)]
pub async fn get_staged_diff(working_dir: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["diff", "--cached"])
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("Failed to get staged diff")?;

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Get the diff for a specific file (unstaged changes)
#[allow(dead_code)]
pub async fn get_file_diff(working_dir: &Path, file_path: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["diff", "--"])
        .arg(file_path)
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("Failed to get file diff")?;

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Check if there are any uncommitted changes
#[allow(dead_code)]
pub async fn has_uncommitted_changes(working_dir: &Path) -> Result<bool> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .output()
        .await
        .context("Failed to check for uncommitted changes")?;

    Ok(!output.stdout.is_empty())
}

/// Stash current changes
#[allow(dead_code)]
pub async fn stash(working_dir: &Path, message: Option<&str>) -> Result<()> {
    let mut args = vec!["stash", "push"];
    if let Some(msg) = message {
        args.push("-m");
        args.push(msg);
    }

    let output = Command::new("git")
        .args(&args)
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("Failed to stash changes")?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to stash: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    info!("Stashed changes");
    Ok(())
}

/// Pop the most recent stash
#[allow(dead_code)]
pub async fn stash_pop(working_dir: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["stash", "pop"])
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("Failed to pop stash")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // No stash is not an error
        if stderr.contains("No stash entries found") {
            return Ok(());
        }
        anyhow::bail!("Failed to pop stash: {}", stderr);
    }

    info!("Popped stash");
    Ok(())
}

/// Check git changes against allowlist, filtering out pre-existing dirty files
/// and files that match any task's global allowlist (for parallel worker support)
pub async fn check_git_changes_filtered(
    allowlist_pattern: &str,
    working_dir: &Path,
    git_state: &GitState,
) -> Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    use crate::process::matches_allowlist;

    let current_dirty = get_dirty_files(working_dir).await?;

    let mut allowed = Vec::new();
    let mut unauthorized = Vec::new();

    for path in current_dirty {
        // Skip files that were already dirty before we started
        if git_state.was_pre_existing_dirty(&path) {
            continue;
        }

        // Check against this worker's specific pattern OR the global allowlist
        // (global allowlist covers files being modified by other parallel workers)
        if matches_allowlist(&path, allowlist_pattern) || git_state.matches_global_allowlist(&path)
        {
            allowed.push(path);
        } else {
            unauthorized.push(path);
        }
    }

    Ok((allowed, unauthorized))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_state_filter_new_changes() {
        let mut git_state = GitState::default();
        git_state
            .pre_existing_dirty_files
            .insert(PathBuf::from("already_dirty.txt"));
        git_state.enabled = true;

        let changed = vec![
            PathBuf::from("already_dirty.txt"),
            PathBuf::from("new_change.txt"),
        ];

        let new_only = git_state.filter_new_changes(&changed);
        assert_eq!(new_only.len(), 1);
        assert_eq!(new_only[0], PathBuf::from("new_change.txt"));
    }

    #[test]
    fn test_was_pre_existing_dirty() {
        let mut git_state = GitState::default();
        git_state
            .pre_existing_dirty_files
            .insert(PathBuf::from("dirty.txt"));

        assert!(git_state.was_pre_existing_dirty(Path::new("dirty.txt")));
        assert!(!git_state.was_pre_existing_dirty(Path::new("clean.txt")));
    }
}
