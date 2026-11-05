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

/// Run the built binary in `working_dir` with the fake claude, no tmux, and the usage limit disabled.
pub fn run_runner(working_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_claude-loop-runner"))
        .args(args)
        .args(["--no-tmux", "--limit", "0", "--claude-bin"])
        .arg(fake_claude_path())
        .current_dir(working_dir)
        .output()
        .expect("failed to run claude-loop-runner")
}
