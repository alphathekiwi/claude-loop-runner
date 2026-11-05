//! Shared helpers for integration tests that drive the built binary.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

/// Path to the scripted stand-in for the claude CLI (see the script header for directives).
pub fn fake_claude_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-claude.sh")
}

/// Run git in `dir`, panicking with stderr on failure.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Create a temp git repo on branch `main` with an identity and one initial commit.
pub fn temp_git_repo() -> TempDir {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let path = dir.path();
    git(path, &["init", "-q", "-b", "main"]);
    git(path, &["config", "user.name", "Test User"]);
    git(path, &["config", "user.email", "test@example.com"]);
    git(path, &["config", "commit.gpgsign", "false"]);
    write_file(path, "README.md", "test repo\n");
    git(path, &["add", "."]);
    git(path, &["commit", "-q", "-m", "initial"]);
    dir
}

/// Write `contents` to `rel` under `root`, creating parent directories.
pub fn write_file(root: &Path, rel: &str, contents: &str) -> PathBuf {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("failed to create parent dir");
    }
    std::fs::write(&path, contents).expect("failed to write file");
    path
}

/// Build a command for the binary in `working_dir` with no tmux and the usage limit disabled.
/// The caller picks the claude binary.
pub fn runner_command(working_dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_claude-loop-runner"));
    cmd.args(args)
        .args(["--no-tmux", "--limit", "0"])
        .current_dir(working_dir);
    cmd
}

/// Run the built binary in `working_dir` with the fake claude, no tmux, and the usage limit disabled.
pub fn run_runner(working_dir: &Path, args: &[&str]) -> Output {
    runner_command(working_dir, args)
        .arg("--claude-bin")
        .arg(fake_claude_path())
        .output()
        .expect("failed to run claude-loop-runner")
}

/// Run the binary and assert it succeeded; returns stdout (where tracing logs go).
pub fn run_ok(working_dir: &Path, args: &[&str]) -> String {
    let output = run_runner(working_dir, args);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        output.status.success(),
        "runner failed\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

/// Run the binary and assert it failed; returns stdout + stderr combined.
pub fn run_err(working_dir: &Path, args: &[&str]) -> String {
    let output = run_runner(working_dir, args);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.status.success(),
        "runner unexpectedly succeeded: {combined}"
    );
    combined
}

/// Create a temp git repo with `files` committed on top of the initial commit.
pub fn repo_with_files(files: &[&str]) -> TempDir {
    let repo = temp_git_repo();
    for file in files {
        write_file(repo.path(), file, "export {}\n");
    }
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "add sources"]);
    repo
}

/// Write an input JSON mapping each file to `{"index": i}` and return its file name.
pub fn write_input(root: &Path, files: &[&str]) -> &'static str {
    let entries: Vec<String> = files
        .iter()
        .enumerate()
        .map(|(i, f)| format!("{f:?}: {{\"index\": {i}}}"))
        .collect();
    write_file(root, "input.json", &format!("{{{}}}", entries.join(", ")));
    "input.json"
}

pub fn read_json(path: &Path) -> serde_json::Value {
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_json::from_str(&content).expect("invalid JSON")
}

/// The persisted task list under `tasks_dir`.
pub fn task_list(root: &Path, tasks_dir: &str) -> serde_json::Value {
    read_json(&root.join(tasks_dir).join("task_list.json"))
}

/// The persisted state for `task_id`, located via the task list's `state_file`.
pub fn task_state(root: &Path, tasks_dir: &str, task_id: &str) -> serde_json::Value {
    let list = task_list(root, tasks_dir);
    let state_file = list["tasks"][task_id]["state_file"]
        .as_str()
        .unwrap_or_else(|| panic!("task {task_id} missing from task list: {list}"));
    read_json(&root.join(tasks_dir).join(state_file))
}

/// The persisted per-file state for `file` within `state`.
pub fn file_state<'a>(state: &'a serde_json::Value, file: &str) -> &'a serde_json::Value {
    let entry = &state["files"][file];
    assert!(!entry.is_null(), "{file} missing from state: {state}");
    entry
}

/// Count of non-empty lines in `path` (0 if it does not exist).
pub fn line_count(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .map(|s| s.lines().filter(|l| !l.is_empty()).count())
        .unwrap_or(0)
}
