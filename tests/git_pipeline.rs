//! --git / --git-branch / --git-commit runs, asserting on the resulting git history.

mod common;

use common::{
    fake_claude_path, file_state, git, repo_with_files, run_ok, runner_command, task_state,
    write_file, write_input,
};
use std::path::Path;

const TASKS: &str = "clr-tasks";

/// Run with `GIT_OPTIONAL_LOCKS=0` so the prompt pool's `git status` cannot
/// briefly hold index.lock while a verify worker commits another file.
fn run_ok_no_optional_locks(dir: &Path, args: &[&str]) -> String {
    let output = runner_command(dir, args)
        .arg("--claude-bin")
        .arg(fake_claude_path())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(output.status.success(), "{stdout}");
    stdout
}

/// Files touched by `rev`, sorted.
fn commit_files(repo: &Path, rev: &str) -> Vec<String> {
    let out = git(repo, &["show", "--name-only", "--format=", rev]);
    let mut files: Vec<String> = out.lines().map(str::to_string).collect();
    files.sort();
    files
}

/// Commit subjects on HEAD since `base`, oldest first.
fn subjects_since(repo: &Path, base: &str) -> Vec<String> {
    git(
        repo,
        &["log", "--reverse", "--format=%s", &format!("{base}..HEAD")],
    )
    .lines()
    .map(str::to_string)
    .collect()
}

#[test]
fn git_branch_and_commit_make_one_commit_per_file_on_a_task_branch() {
    let files = ["src/Foo.ts", "src/Bar.ts"];
    let repo = repo_with_files(&files);
    let input = write_input(repo.path(), &files);
    // Keep the input file out of the dirty set the runner sees.
    git(repo.path(), &["add", "input.json"]);
    git(repo.path(), &["commit", "-q", "-m", "add input"]);

    // One verify worker: concurrent auto-commits race (see the ignored test below).
    run_ok_no_optional_locks(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "FAKE_APPEND_TARGET",
            "--verify",
            "grep -q edited {file}",
            "--concurrency",
            "2",
            "--verify-concurrency",
            "1",
            "--git-branch",
            "--git-commit",
            "--tasks-dir",
            TASKS,
        ],
    );

    let branch = git(repo.path(), &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert!(branch.starts_with("claude-loop/task_0-"), "{branch}");

    let state = task_state(repo.path(), TASKS, "task_0");
    assert_eq!(state["git_state"]["enabled"], true);
    assert_eq!(state["git_state"]["original_branch"], "main");
    assert_eq!(state["git_state"]["task_branch"], branch.as_str());
    let patterns = state["git_state"]["global_allowlist_patterns"]
        .as_array()
        .unwrap();
    assert!(patterns.contains(&"Foo*".into()), "{patterns:?}");
    assert!(patterns.contains(&"Bar*".into()), "{patterns:?}");
    for file in files {
        assert_eq!(file_state(&state, file)["status"], "completed");
    }

    // main is untouched; the branch carries exactly one commit per file.
    let mut subjects = subjects_since(repo.path(), "main");
    subjects.sort();
    assert_eq!(subjects, ["claude-loop: Bar.ts", "claude-loop: Foo.ts"]);
    for rev in ["HEAD", "HEAD~1"] {
        let subject = git(repo.path(), &["log", "-1", "--format=%s", rev]);
        let expected = if subject.ends_with("Foo.ts") {
            "src/Foo.ts"
        } else {
            "src/Bar.ts"
        };
        assert_eq!(commit_files(repo.path(), rev), [expected], "{subject}");
    }
    let foo = git(repo.path(), &["show", "HEAD:src/Foo.ts"]);
    assert!(foo.contains("edited by fake-claude"));
}

#[test]
fn commit_includes_related_test_file_and_custom_message() {
    let repo = repo_with_files(&["src/Foo.ts", "src/Foo.test.ts"]);
    let input = write_input(repo.path(), &["src/Foo.ts"]);
    git(repo.path(), &["add", "input.json"]);
    git(repo.path(), &["commit", "-q", "-m", "add input"]);

    run_ok(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "FAKE_APPEND_TARGET\nFAKE_WRITE=src/Foo.test.ts",
            "--verify",
            "true",
            "--git-commit",
            "--git-commit-message",
            "add tests",
            "--tasks-dir",
            TASKS,
        ],
    );

    // No --git-branch: commits land on the current branch.
    assert_eq!(
        git(repo.path(), &["rev-parse", "--abbrev-ref", "HEAD"]),
        "main"
    );
    assert_eq!(
        git(repo.path(), &["log", "-1", "--format=%s"]),
        "claude-loop: Foo.ts (add tests)"
    );
    assert_eq!(
        commit_files(repo.path(), "HEAD"),
        ["src/Foo.test.ts", "src/Foo.ts"]
    );
    let state = task_state(repo.path(), TASKS, "task_0");
    assert!(state["git_state"]["task_branch"].is_null());
    // Foo.test.ts reduces to the same stem, so one pattern covers both.
    assert_eq!(
        state["git_state"]["global_allowlist_patterns"],
        serde_json::json!(["Foo*"])
    );
}

#[test]
fn out_of_allowlist_edit_is_warned_about_and_not_committed() {
    let repo = repo_with_files(&["src/Foo.ts"]);
    let input = write_input(repo.path(), &["src/Foo.ts"]);
    git(repo.path(), &["add", "input.json"]);
    git(repo.path(), &["commit", "-q", "-m", "add input"]);

    let stdout = run_ok(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "FAKE_APPEND_TARGET\nFAKE_WRITE=other.ts",
            "--verify",
            "true",
            "--git",
            "--git-commit",
            "--tasks-dir",
            TASKS,
        ],
    );

    let warning = stdout
        .lines()
        .find(|l| l.contains("Detected unauthorized file changes"))
        .unwrap_or_else(|| panic!("no unauthorized warning in:\n{stdout}"));
    assert!(warning.contains(r#"["other.ts"]"#), "{warning}");

    assert_eq!(commit_files(repo.path(), "HEAD"), ["src/Foo.ts"]);
    let status = git(repo.path(), &["status", "--porcelain", "--", "other.ts"]);
    assert_eq!(status, "?? other.ts");
}

#[test]
fn pre_existing_dirty_files_are_not_flagged_as_unauthorized() {
    let repo = repo_with_files(&["src/Foo.ts"]);
    let input = write_input(repo.path(), &["src/Foo.ts"]);
    git(repo.path(), &["add", "input.json"]);
    git(repo.path(), &["commit", "-q", "-m", "add input"]);
    write_file(repo.path(), "notes.txt", "my own work\n");

    let stdout = run_ok(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "FAKE_APPEND_TARGET",
            "--verify",
            "true",
            "--git-commit",
            "--tasks-dir",
            TASKS,
        ],
    );

    assert!(
        stdout.contains("Found pre-existing dirty files"),
        "{stdout}"
    );
    assert!(!stdout.contains("Detected unauthorized"), "{stdout}");
    let state = task_state(repo.path(), TASKS, "task_0");
    let dirty = state["git_state"]["pre_existing_dirty_files"]
        .as_array()
        .unwrap();
    assert!(dirty.contains(&"notes.txt".into()), "{dirty:?}");
    assert_eq!(commit_files(repo.path(), "HEAD"), ["src/Foo.ts"]);
    assert_eq!(
        git(repo.path(), &["status", "--porcelain", "--", "notes.txt"]),
        "?? notes.txt"
    );
}

#[test]
#[ignore = "bug: git.rs:465 commits every dirty path containing the file stem, sweeping in the runner's own failures/<file>.log"]
fn auto_commit_after_fixup_does_not_commit_the_failure_log() {
    let repo = repo_with_files(&["src/Foo.ts"]);
    let input = write_input(repo.path(), &["src/Foo.ts"]);
    git(repo.path(), &["add", "input.json"]);
    git(repo.path(), &["commit", "-q", "-m", "add input"]);

    run_ok(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "FAKE_APPEND_TARGET",
            "--verify",
            "test -f {file}.fixed",
            "--fixup",
            "FAKE_WRITE=src/Foo.ts.fixed",
            "--git-commit",
            "--tasks-dir",
            TASKS,
        ],
    );

    assert_eq!(
        commit_files(repo.path(), "HEAD"),
        ["src/Foo.ts", "src/Foo.ts.fixed"]
    );
}

#[test]
#[ignore = "bug: git.rs:465 commit_file_changes ignores pre_existing_dirty_files, so the user's uncommitted edits to a related file are committed"]
fn auto_commit_leaves_pre_existing_edits_to_related_files_alone() {
    let repo = repo_with_files(&["src/Foo.ts", "src/Foo.test.ts"]);
    let input = write_input(repo.path(), &["src/Foo.ts"]);
    git(repo.path(), &["add", "input.json"]);
    git(repo.path(), &["commit", "-q", "-m", "add input"]);
    write_file(
        repo.path(),
        "src/Foo.test.ts",
        "// user's work in progress\n",
    );

    run_ok(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "FAKE_APPEND_TARGET",
            "--verify",
            "true",
            "--git-commit",
            "--tasks-dir",
            TASKS,
        ],
    );

    assert_eq!(commit_files(repo.path(), "HEAD"), ["src/Foo.ts"]);
}

#[test]
#[ignore = "bug: prompt.rs:116 marks the file completed without committing when there is no --verify, so --git-commit never commits"]
fn git_commit_without_verify_still_commits() {
    let repo = repo_with_files(&["src/Foo.ts"]);
    let input = write_input(repo.path(), &["src/Foo.ts"]);
    git(repo.path(), &["add", "input.json"]);
    git(repo.path(), &["commit", "-q", "-m", "add input"]);

    run_ok(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "FAKE_APPEND_TARGET",
            "--git-commit",
            "--tasks-dir",
            TASKS,
        ],
    );

    assert_eq!(
        git(repo.path(), &["log", "-1", "--format=%s"]),
        "claude-loop: Foo.ts"
    );
}

#[test]
#[ignore = "bug: verify.rs:149 auto-commits from parallel verify workers without a lock; git.rs:415 `git commit` takes the whole index, so one file's commit sweeps in another's staged changes and the other fails on index.lock"]
fn parallel_verify_workers_commit_each_file_separately() {
    let files = ["src/Foo.ts", "src/Bar.ts"];
    let repo = repo_with_files(&files);
    let input = write_input(repo.path(), &files);
    git(repo.path(), &["add", "input.json"]);
    git(repo.path(), &["commit", "-q", "-m", "add input"]);
    let base = git(repo.path(), &["rev-parse", "HEAD"]);

    // Both verifications wait for each other so the two auto-commits overlap.
    run_ok_no_optional_locks(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "FAKE_APPEND_TARGET",
            "--verify",
            "mkdir -p .vb; : > .vb/$$; i=0; \
             while [ $(ls .vb | wc -l) -lt 2 ] && [ $i -lt 100 ]; do sleep 0.05; i=$((i+1)); done",
            "--concurrency",
            "2",
            "--git-commit",
            "--tasks-dir",
            TASKS,
        ],
    );

    let mut subjects = subjects_since(repo.path(), &base);
    subjects.sort();
    assert_eq!(subjects, ["claude-loop: Bar.ts", "claude-loop: Foo.ts"]);
    assert_eq!(commit_files(repo.path(), "HEAD").len(), 1);
}

#[test]
fn no_git_overrides_git_flags() {
    let repo = repo_with_files(&["src/Foo.ts"]);
    let input = write_input(repo.path(), &["src/Foo.ts"]);
    let head = git(repo.path(), &["rev-parse", "HEAD"]);

    let stdout = run_ok(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "FAKE_APPEND_TARGET",
            "--git-branch",
            "--git-commit",
            "--no-git",
            "--tasks-dir",
            TASKS,
        ],
    );

    assert!(stdout.contains("--no-git specified"), "{stdout}");
    assert_eq!(
        git(repo.path(), &["rev-parse", "--abbrev-ref", "HEAD"]),
        "main"
    );
    assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]), head);
    let state = task_state(repo.path(), TASKS, "task_0");
    assert_eq!(file_state(&state, "src/Foo.ts")["status"], "completed");
    assert_eq!(state["git_state"]["enabled"], false);
}

#[test]
fn missing_git_identity_without_a_tty_disables_git_features() {
    let repo = repo_with_files(&["src/Foo.ts"]);
    let input = write_input(repo.path(), &["src/Foo.ts"]);
    git(repo.path(), &["config", "--unset", "user.name"]);
    git(repo.path(), &["config", "--unset", "user.email"]);
    let head = git(repo.path(), &["rev-parse", "HEAD"]);
    let home = tempfile::tempdir().unwrap();

    let output = runner_command(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "FAKE_APPEND_TARGET",
            "--git-branch",
            "--git-commit",
            "--tasks-dir",
            TASKS,
        ],
    )
    .arg("--claude-bin")
    .arg(fake_claude_path())
    .env("HOME", home.path())
    .env("XDG_CONFIG_HOME", home.path())
    .env("GIT_CONFIG_NOSYSTEM", "1")
    .env_remove("GIT_AUTHOR_NAME")
    .env_remove("GIT_AUTHOR_EMAIL")
    .env_remove("GIT_COMMITTER_NAME")
    .env_remove("GIT_COMMITTER_EMAIL")
    .stdin(std::process::Stdio::null())
    .output()
    .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");

    assert!(stdout.contains("Git identity not configured"), "{stdout}");
    assert_eq!(
        git(repo.path(), &["rev-parse", "--abbrev-ref", "HEAD"]),
        "main"
    );
    assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]), head);
    let state = task_state(repo.path(), TASKS, "task_0");
    assert_eq!(file_state(&state, "src/Foo.ts")["status"], "completed");
}

#[test]
fn git_outside_a_repository_runs_without_git() {
    let dir = tempfile::tempdir().unwrap();
    write_file(dir.path(), "src/Foo.ts", "export {}\n");
    let input = write_input(dir.path(), &["src/Foo.ts"]);

    let stdout = run_ok(
        dir.path(),
        &[
            "--input",
            input,
            "--prompt",
            "FAKE_APPEND_TARGET",
            "--git-commit",
            "--tasks-dir",
            TASKS,
        ],
    );

    assert!(stdout.contains("Not a git repository"), "{stdout}");
    let state = task_state(dir.path(), TASKS, "task_0");
    assert_eq!(state["git_state"]["enabled"], false);
    assert_eq!(file_state(&state, "src/Foo.ts")["status"], "completed");
}

#[test]
#[ignore = "bug: runner.rs:29 uses the process cwd, not --working-dir, so claude runs in the wrong directory"]
fn working_dir_flag_sets_where_claude_runs() {
    let repo = repo_with_files(&["src/Foo.ts"]);
    let input = write_input(repo.path(), &["src/Foo.ts"]);
    let elsewhere = tempfile::tempdir().unwrap();
    let input_abs = repo.path().join(input);
    let repo_str = repo.path().to_str().unwrap();

    run_ok(
        elsewhere.path(),
        &[
            "--input",
            input_abs.to_str().unwrap(),
            "--prompt",
            "FAKE_APPEND_TARGET",
            "-w",
            repo_str,
            "--no-git",
            "--tasks-dir",
            TASKS,
        ],
    );

    let contents = std::fs::read_to_string(repo.path().join("src/Foo.ts")).unwrap();
    assert!(contents.contains("edited by fake-claude"));
}
