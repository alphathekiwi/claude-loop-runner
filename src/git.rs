use crate::process::extract_file_stem;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{self, BufRead, Write as IoWrite};
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

/// Result of checking git identity configuration
pub enum GitIdentityStatus {
    /// Both user.name and user.email are configured
    Configured { name: String, email: String },
    /// One or both are missing
    Missing {
        name: Option<String>,
        email: Option<String>,
    },
}

/// User's choice when git identity is missing
#[derive(Debug, PartialEq)]
pub enum GitIdentityAction {
    /// User provided name and email to configure
    Configure { name: String, email: String },
    /// User chose to disable git features for this run
    DisableGit,
}

/// Get a single git config value, returning None if not set
async fn get_git_config(working_dir: &Path, key: &str) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["config", "--get", key])
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| format!("Failed to check git config {}", key))?;

    if output.status.success() {
        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if value.is_empty() {
            Ok(None)
        } else {
            Ok(Some(value))
        }
    } else {
        Ok(None)
    }
}

/// Check if git user.name and user.email are configured
pub async fn check_git_identity(working_dir: &Path) -> Result<GitIdentityStatus> {
    let name = get_git_config(working_dir, "user.name").await?;
    let email = get_git_config(working_dir, "user.email").await?;

    match (&name, &email) {
        (Some(n), Some(e)) => Ok(GitIdentityStatus::Configured {
            name: n.clone(),
            email: e.clone(),
        }),
        _ => Ok(GitIdentityStatus::Missing { name, email }),
    }
}

/// Set git user.name and user.email at repository (local) scope
pub async fn set_git_identity(working_dir: &Path, name: &str, email: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["config", "user.name", name])
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("Failed to set git user.name")?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to set git user.name: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let output = Command::new("git")
        .args(["config", "user.email", email])
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("Failed to set git user.email")?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to set git user.email: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    info!(name = %name, email = %email, "Set git identity (local scope)");
    Ok(())
}

/// Interactively prompt the user to configure git identity or disable git features.
/// Output goes to stderr to avoid polluting piped stdout.
pub fn prompt_git_identity(missing_name: bool, missing_email: bool) -> Result<GitIdentityAction> {
    prompt_git_identity_with(
        &mut io::stdin().lock(),
        &mut io::stderr(),
        missing_name,
        missing_email,
    )
}

/// [`prompt_git_identity`] over any reader/writer, so the dialogue can be tested.
fn prompt_git_identity_with(
    reader: &mut impl BufRead,
    stderr: &mut impl IoWrite,
    missing_name: bool,
    missing_email: bool,
) -> Result<GitIdentityAction> {
    writeln!(stderr)?;
    writeln!(stderr, "Git identity is not fully configured.")?;
    if missing_name {
        writeln!(stderr, "  - user.name is not set")?;
    }
    if missing_email {
        writeln!(stderr, "  - user.email is not set")?;
    }
    writeln!(stderr)?;
    writeln!(stderr, "Git commits require both user.name and user.email.")?;
    writeln!(stderr, "You can:")?;
    writeln!(
        stderr,
        "  [1] Provide name and email now (saved to this repo's .git/config)"
    )?;
    writeln!(stderr, "  [2] Disable git features for this run")?;
    writeln!(stderr)?;

    let mut input = String::new();

    loop {
        write!(stderr, "Choose [1/2]: ")?;
        stderr.flush()?;
        read_answer(reader, &mut input)?;
        let choice = input.trim();

        match choice {
            "1" => {
                let name = if missing_name {
                    loop {
                        write!(stderr, "Enter user.name: ")?;
                        stderr.flush()?;
                        read_answer(reader, &mut input)?;
                        let val = input.trim().to_string();
                        if !val.is_empty() {
                            break val;
                        }
                        writeln!(stderr, "Name cannot be empty.")?;
                    }
                } else {
                    String::new()
                };

                let email = if missing_email {
                    loop {
                        write!(stderr, "Enter user.email: ")?;
                        stderr.flush()?;
                        read_answer(reader, &mut input)?;
                        let val = input.trim().to_string();
                        if !val.is_empty() {
                            break val;
                        }
                        writeln!(stderr, "Email cannot be empty.")?;
                    }
                } else {
                    String::new()
                };

                return Ok(GitIdentityAction::Configure { name, email });
            }
            "2" => {
                return Ok(GitIdentityAction::DisableGit);
            }
            _ => {
                writeln!(stderr, "Invalid choice. Please enter 1 or 2.")?;
            }
        }
    }
}

/// Read one line of input, failing on end of input so a closed stdin can't loop the prompt
fn read_answer(reader: &mut impl BufRead, input: &mut String) -> Result<()> {
    input.clear();
    if reader.read_line(input)? == 0 {
        anyhow::bail!("stdin closed before git identity was entered");
    }
    Ok(())
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
    // -z gives NUL-separated, unquoted paths, so spaces and non-ASCII survive intact
    let output = Command::new("git")
        .args(["status", "--porcelain", "-z", "--untracked-files=all"])
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("Failed to run git status")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut files = HashSet::new();

    let mut entries = stdout.split('\0');
    while let Some(entry) = entries.next() {
        let Some(file_path) = entry.get(3..).filter(|p| !p.is_empty()) else {
            continue;
        };
        files.insert(PathBuf::from(file_path));
        // Renames and copies ("R  new\0old\0") are followed by the original path; skip it
        if entry.starts_with(['R', 'C']) {
            entries.next();
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

/// Commit changes with a message.
///
/// With `paths`, only those paths are committed (`git commit -- <paths>`), so
/// anything else in the index is left staged. With no paths, commits the index.
pub async fn commit(working_dir: &Path, message: &str, paths: &[PathBuf]) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.args(["commit", "-m", message]);
    if !paths.is_empty() {
        cmd.arg("--").args(paths);
    }
    let output = cmd
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("Failed to commit")?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        // "nothing to commit" is not an error for our purposes (git prints it on stdout)
        if stdout.contains("nothing to commit") || stderr.contains("nothing to commit") {
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

/// Stage and commit changes for a specific file.
///
/// Dirty paths for which `skip` returns true are never staged or committed.
pub async fn commit_file_changes(
    working_dir: &Path,
    file_path: &Path,
    task_description: Option<&str>,
    skip: impl Fn(&Path) -> bool,
) -> Result<Option<String>> {
    // Get files that changed (related to this file path)
    let dirty_files = get_dirty_files(working_dir).await?;

    // Find files that match the file_path pattern (including related test files, etc.)
    let file_stem = extract_file_stem(file_path);

    let related_files: Vec<PathBuf> = dirty_files
        .into_iter()
        .filter(|p| {
            let p_str = p.to_string_lossy();
            (p_str.contains(&file_stem) || p == file_path) && !skip(p)
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

    let commit_hash = commit(working_dir, &message, &related_files).await?;

    if commit_hash.is_empty() {
        Ok(None)
    } else {
        Ok(Some(commit_hash))
    }
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
    fn test_was_pre_existing_dirty() {
        let mut git_state = GitState::default();
        git_state
            .pre_existing_dirty_files
            .insert(PathBuf::from("dirty.txt"));

        assert!(git_state.was_pre_existing_dirty(Path::new("dirty.txt")));
        assert!(!git_state.was_pre_existing_dirty(Path::new("clean.txt")));
    }

    use crate::test_support::{git, temp_git_repo, write_file};

    fn set(paths: &[&str]) -> HashSet<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    fn sorted(mut v: Vec<PathBuf>) -> Vec<PathBuf> {
        v.sort();
        v
    }

    #[test]
    fn add_allowlist_pattern_dedupes_and_matches() {
        let mut state = GitState::default();
        state.add_allowlist_pattern("foo*".to_string());
        state.add_allowlist_pattern("foo*".to_string());
        state.add_allowlist_pattern("bar.rs".to_string());
        assert_eq!(state.global_allowlist_patterns, vec!["foo*", "bar.rs"]);

        assert!(state.matches_global_allowlist(Path::new("src/foo.test.ts")));
        assert!(state.matches_global_allowlist(Path::new("src/bar.rs")));
        assert!(!state.matches_global_allowlist(Path::new("src/baz.rs")));
        assert!(!GitState::default().matches_global_allowlist(Path::new("foo.rs")));
    }

    #[tokio::test]
    async fn is_git_repo_true_and_false() {
        let repo = temp_git_repo();
        assert!(is_git_repo(repo.path()).await.unwrap());
        let plain = tempfile::tempdir().unwrap();
        assert!(!is_git_repo(plain.path()).await.unwrap());
    }

    #[tokio::test]
    async fn is_git_repo_errors_on_missing_dir() {
        let plain = tempfile::tempdir().unwrap();
        assert!(is_git_repo(&plain.path().join("nope")).await.is_err());
    }

    #[tokio::test]
    async fn capture_outside_repo_disables_git() {
        let plain = tempfile::tempdir().unwrap();
        let state = GitState::capture(plain.path()).await.unwrap();
        assert!(!state.enabled);
        assert!(state.original_branch.is_none());
        assert!(state.pre_existing_dirty_files.is_empty());
    }

    #[tokio::test]
    async fn capture_records_branch_and_pre_existing_dirty_files() {
        let repo = temp_git_repo();
        let root = repo.path();
        write_file(root, "README.md", "changed\n");
        write_file(root, "new/untracked.txt", "x\n");

        let state = GitState::capture(root).await.unwrap();
        assert!(state.enabled);
        assert_eq!(state.original_branch.as_deref(), Some("main"));
        assert!(state.task_branch.is_none());
        assert_eq!(
            state.pre_existing_dirty_files,
            set(&["README.md", "new/untracked.txt"])
        );
    }

    #[tokio::test]
    async fn capture_clean_repo_has_no_dirty_files() {
        let repo = temp_git_repo();
        let state = GitState::capture(repo.path()).await.unwrap();
        assert!(state.enabled);
        assert!(state.pre_existing_dirty_files.is_empty());
    }

    #[tokio::test]
    async fn check_git_identity_configured() {
        let repo = temp_git_repo();
        match check_git_identity(repo.path()).await.unwrap() {
            GitIdentityStatus::Configured { name, email } => {
                assert_eq!(name, "Test User");
                assert_eq!(email, "test@example.com");
            }
            GitIdentityStatus::Missing { .. } => panic!("expected Configured"),
        }
    }

    // A local empty value shadows any global/system value (last one wins) and
    // reads back as None, so this is hermetic regardless of the machine's config.
    #[tokio::test]
    async fn check_git_identity_missing_when_blank_locally() {
        let repo = temp_git_repo();
        git(repo.path(), &["config", "user.email", ""]);
        match check_git_identity(repo.path()).await.unwrap() {
            GitIdentityStatus::Missing { name, email } => {
                assert_eq!(name.as_deref(), Some("Test User"));
                assert_eq!(email, None);
            }
            GitIdentityStatus::Configured { .. } => panic!("expected Missing"),
        }

        git(repo.path(), &["config", "user.name", ""]);
        match check_git_identity(repo.path()).await.unwrap() {
            GitIdentityStatus::Missing { name, email } => {
                assert_eq!(name, None);
                assert_eq!(email, None);
            }
            GitIdentityStatus::Configured { .. } => panic!("expected Missing"),
        }
    }

    #[tokio::test]
    async fn set_git_identity_writes_local_config() {
        let repo = temp_git_repo();
        set_git_identity(repo.path(), "New Name", "new@example.com")
            .await
            .unwrap();
        assert_eq!(
            git(repo.path(), &["config", "--local", "user.name"]),
            "New Name"
        );
        assert_eq!(
            git(repo.path(), &["config", "--local", "user.email"]),
            "new@example.com"
        );
        assert!(matches!(
            check_git_identity(repo.path()).await.unwrap(),
            GitIdentityStatus::Configured { .. }
        ));
    }

    #[tokio::test]
    async fn set_git_identity_fails_outside_repo() {
        let plain = tempfile::tempdir().unwrap();
        let err = set_git_identity(plain.path(), "n", "e")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("user.name"), "{err}");
    }

    #[tokio::test]
    async fn get_current_branch_reports_branch_and_errors_outside_repo() {
        let repo = temp_git_repo();
        assert_eq!(get_current_branch(repo.path()).await.unwrap(), "main");
        git(repo.path(), &["checkout", "-q", "-b", "feature/x"]);
        assert_eq!(get_current_branch(repo.path()).await.unwrap(), "feature/x");

        let plain = tempfile::tempdir().unwrap();
        assert!(get_current_branch(plain.path()).await.is_err());
    }

    #[tokio::test]
    async fn get_dirty_files_covers_modified_untracked_deleted_and_renamed() {
        let repo = temp_git_repo();
        let root = repo.path();
        write_file(root, "old.txt", "rename me\n");
        write_file(root, "gone.txt", "delete me\n");
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "more"]);

        write_file(root, "README.md", "modified\n");
        write_file(root, "dir/sub/untracked.txt", "u\n");
        std::fs::remove_file(root.join("gone.txt")).unwrap();
        git(root, &["mv", "old.txt", "renamed.txt"]);

        assert_eq!(
            get_dirty_files(root).await.unwrap(),
            set(&[
                "README.md",
                "dir/sub/untracked.txt",
                "gone.txt",
                "renamed.txt"
            ])
        );
    }

    #[tokio::test]
    async fn get_dirty_files_empty_when_clean() {
        let repo = temp_git_repo();
        assert!(get_dirty_files(repo.path()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn get_dirty_files_unquotes_paths_with_spaces() {
        let repo = temp_git_repo();
        write_file(repo.path(), "my file.txt", "x\n");
        assert_eq!(
            get_dirty_files(repo.path()).await.unwrap(),
            set(&["my file.txt"])
        );
    }

    /// Run the identity prompt on scripted input; returns the result and what it printed.
    /// Runs on a thread with a timeout so a prompt that loops forever fails the test.
    fn prompt(
        input: &str,
        missing_name: bool,
        missing_email: bool,
    ) -> (Result<GitIdentityAction>, String) {
        let input = input.to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut out = Vec::new();
            let result = prompt_git_identity_with(
                &mut io::Cursor::new(input),
                &mut out,
                missing_name,
                missing_email,
            );
            let _ = tx.send((result, String::from_utf8(out).unwrap()));
        });
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .expect("prompt did not return (looping on closed stdin?)")
    }

    #[test]
    fn identity_prompt_collects_missing_name_and_email() {
        let (result, out) = prompt("1\nAda\nada@example.com\n", true, true);
        assert_eq!(
            result.unwrap(),
            GitIdentityAction::Configure {
                name: "Ada".into(),
                email: "ada@example.com".into()
            }
        );
        assert!(out.contains("user.name is not set"), "{out}");
        assert!(out.contains("user.email is not set"), "{out}");
    }

    #[test]
    fn identity_prompt_only_asks_for_what_is_missing() {
        let (result, out) = prompt("1\nada@example.com\n", false, true);
        assert_eq!(
            result.unwrap(),
            GitIdentityAction::Configure {
                name: String::new(),
                email: "ada@example.com".into()
            }
        );
        assert!(!out.contains("Enter user.name"), "{out}");
    }

    #[test]
    fn identity_prompt_reprompts_on_bad_choice_and_empty_values() {
        let (result, out) = prompt("3\n1\n\nAda\n  \nada@example.com\n", true, true);
        assert!(matches!(
            result.unwrap(),
            GitIdentityAction::Configure { .. }
        ));
        assert!(
            out.contains("Invalid choice. Please enter 1 or 2."),
            "{out}"
        );
        assert!(out.contains("Name cannot be empty."), "{out}");
        assert!(out.contains("Email cannot be empty."), "{out}");
    }

    #[test]
    fn identity_prompt_can_disable_git() {
        let (result, _) = prompt("2\n", true, false);
        assert_eq!(result.unwrap(), GitIdentityAction::DisableGit);
    }

    #[test]
    fn identity_prompt_errors_when_stdin_closes() {
        // Ctrl-D (EOF) at any question must end the prompt, not spin forever.
        for input in ["", "1\n", "1\nAda\n"] {
            let (result, _) = prompt(input, true, true);
            assert!(result.is_err(), "input {input:?} should error");
        }
    }

    #[tokio::test]
    async fn create_task_branch_creates_and_checks_out_a_timestamped_branch() {
        let repo = temp_git_repo();
        let root = repo.path();
        let branch = create_task_branch(root, "task1").await.unwrap();
        assert!(branch.starts_with("claude-loop/task1-"), "{branch}");
        // timestamp suffix: YYYYMMDD-HHMMSS
        let suffix = branch.strip_prefix("claude-loop/task1-").unwrap();
        assert_eq!(suffix.len(), 15);
        assert_eq!(get_current_branch(root).await.unwrap(), branch);

        git(root, &["checkout", "-q", "main"]);
        assert_eq!(get_current_branch(root).await.unwrap(), "main");

        // An invalid ref name is rejected.
        let err = create_task_branch(root, "bad..name").await.unwrap_err();
        assert!(err.to_string().contains("Failed to create branch"), "{err}");
    }

    #[tokio::test]
    async fn stage_and_commit_returns_short_hash() {
        let repo = temp_git_repo();
        let root = repo.path();
        write_file(root, "a.txt", "a\n");
        write_file(root, "b.txt", "b\n");

        stage_files(root, &[]).await.unwrap();
        stage_files(root, &[PathBuf::from("a.txt")]).await.unwrap();
        assert_eq!(git(root, &["diff", "--cached", "--name-only"]), "a.txt");

        let hash = commit(root, "add a", &[]).await.unwrap();
        assert_eq!(hash, git(root, &["rev-parse", "--short", "HEAD"]));
        assert_eq!(git(root, &["log", "-1", "--format=%s"]), "add a");
        // b.txt was not staged so it is still untracked.
        assert_eq!(get_dirty_files(root).await.unwrap(), set(&["b.txt"]));
    }

    #[tokio::test]
    async fn stage_files_errors_on_unknown_path() {
        let repo = temp_git_repo();
        let err = stage_files(repo.path(), &[PathBuf::from("missing.txt")])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Failed to stage files"), "{err}");
    }

    #[tokio::test]
    async fn commit_fails_outside_repo() {
        let plain = tempfile::tempdir().unwrap();
        assert!(commit(plain.path(), "msg", &[]).await.is_err());
    }

    #[tokio::test]
    async fn commit_with_nothing_staged_returns_empty_hash() {
        let repo = temp_git_repo();
        assert_eq!(commit(repo.path(), "noop", &[]).await.unwrap(), "");
    }

    #[tokio::test]
    async fn commit_file_changes_only_commits_related_files() {
        let repo = temp_git_repo();
        let root = repo.path();
        write_file(root, "src/widget.rs", "fn w() {}\n");
        write_file(root, "src/widget.test.rs", "#[test] fn t() {}\n");
        write_file(root, "src/other.rs", "fn o() {}\n");

        let hash = commit_file_changes(root, Path::new("src/widget.rs"), None, |_| false)
            .await
            .unwrap()
            .expect("expected a commit");
        assert_eq!(hash, git(root, &["rev-parse", "--short", "HEAD"]));
        assert_eq!(
            git(root, &["log", "-1", "--format=%s"]),
            "claude-loop: widget.rs"
        );
        let committed = git(root, &["show", "--name-only", "--format=", "HEAD"]);
        let mut committed: Vec<&str> = committed.lines().collect();
        committed.sort();
        assert_eq!(committed, vec!["src/widget.rs", "src/widget.test.rs"]);
        assert_eq!(get_dirty_files(root).await.unwrap(), set(&["src/other.rs"]));
    }

    #[tokio::test]
    async fn commit_file_changes_leaves_unrelated_staged_files_out() {
        let repo = temp_git_repo();
        let root = repo.path();
        write_file(root, "staged_by_user.txt", "s\n");
        git(root, &["add", "staged_by_user.txt"]);
        write_file(root, "src/widget.rs", "w\n");

        commit_file_changes(root, Path::new("src/widget.rs"), None, |_| false)
            .await
            .unwrap()
            .expect("expected a commit");
        assert_eq!(
            git(root, &["show", "--name-only", "--format=", "HEAD"]),
            "src/widget.rs"
        );
    }

    #[tokio::test]
    async fn commit_file_changes_message_includes_description() {
        let repo = temp_git_repo();
        let root = repo.path();
        write_file(root, "lib/thing.ts", "export {};\n");
        commit_file_changes(root, Path::new("lib/thing.ts"), Some("add tests"), |_| {
            false
        })
        .await
        .unwrap()
        .expect("expected a commit");
        assert_eq!(
            git(root, &["log", "-1", "--format=%s"]),
            "claude-loop: thing.ts (add tests)"
        );
    }

    #[tokio::test]
    async fn commit_file_changes_none_when_nothing_related_changed() {
        let repo = temp_git_repo();
        let root = repo.path();
        let head = git(root, &["rev-parse", "HEAD"]);
        assert!(
            commit_file_changes(root, Path::new("src/widget.rs"), None, |_| false)
                .await
                .unwrap()
                .is_none()
        );

        write_file(root, "src/unrelated.rs", "x\n");
        assert!(
            commit_file_changes(root, Path::new("src/widget.rs"), None, |_| false)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(git(root, &["rev-parse", "HEAD"]), head);
    }

    #[tokio::test]
    async fn check_git_changes_filtered_splits_allowed_and_unauthorized() {
        let repo = temp_git_repo();
        let root = repo.path();
        write_file(root, "README.md", "dirty before start\n");
        let mut state = GitState::capture(root).await.unwrap();
        state.add_allowlist_pattern("peer*".to_string());

        write_file(root, "src/widget.rs", "w\n");
        write_file(root, "src/widget.test.rs", "t\n");
        write_file(root, "src/peer.rs", "p\n");
        write_file(root, "src/rogue.rs", "r\n");
        // Pre-existing dirty file modified again is still ignored.
        write_file(root, "README.md", "dirty again\n");

        let (allowed, unauthorized) = check_git_changes_filtered("widget*", root, &state)
            .await
            .unwrap();
        assert_eq!(
            sorted(allowed),
            vec![
                PathBuf::from("src/peer.rs"),
                PathBuf::from("src/widget.rs"),
                PathBuf::from("src/widget.test.rs"),
            ]
        );
        assert_eq!(unauthorized, vec![PathBuf::from("src/rogue.rs")]);
    }

    #[tokio::test]
    async fn check_git_changes_filtered_clean_repo_is_empty() {
        let repo = temp_git_repo();
        let state = GitState::capture(repo.path()).await.unwrap();
        let (allowed, unauthorized) = check_git_changes_filtered("*", repo.path(), &state)
            .await
            .unwrap();
        assert!(allowed.is_empty());
        assert!(unauthorized.is_empty());
    }
}
