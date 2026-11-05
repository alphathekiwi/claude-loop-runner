//! Dry runs, --max-files and --resume, asserting on the persisted task list and state.

mod common;

use common::{
    file_state, line_count, read_json, repo_with_files, run_err, run_ok, runner_command, task_list,
    task_state, write_input,
};
use serde_json::Value;

const TASKS: &str = "clr-tasks";

fn statuses(state: &Value) -> Vec<String> {
    let mut s: Vec<String> = state["files"]
        .as_object()
        .unwrap()
        .values()
        .map(|f| f["status"].as_str().unwrap().to_string())
        .collect();
    s.sort();
    s
}

#[test]
fn dry_run_persists_task_without_running_claude_then_resume_by_id_runs_it() {
    let files = ["src/Foo.ts", "src/Bar.ts"];
    let repo = repo_with_files(&files);
    let input = write_input(repo.path(), &files);

    let stdout = run_ok(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "FAKE_APPEND_TARGET\nFAKE_LOG=calls.log",
            "--verify",
            "grep -q edited {file}",
            "--dry-run",
            "--no-git",
            "--tasks-dir",
            TASKS,
        ],
    );
    assert!(stdout.contains("Dry run complete"), "{stdout}");
    assert!(stdout.contains("--resume task_0"), "{stdout}");
    assert!(!repo.path().join("calls.log").exists());

    let list = task_list(repo.path(), TASKS);
    let entry = &list["tasks"]["task_0"];
    assert_eq!(entry["completed"], false);
    assert_eq!(
        entry["description"],
        "FAKE_APPEND_TARGET\nFAKE_LOG=calls.log"
    );
    let state = task_state(repo.path(), TASKS, "task_0");
    assert_eq!(statuses(&state), ["pending", "pending"]);
    assert_eq!(
        state["config"]["prompt"],
        "FAKE_APPEND_TARGET\nFAKE_LOG=calls.log"
    );
    assert_eq!(state["config"]["verification_cmd"], "grep -q edited {file}");

    // Resume uses the saved prompt and verify command; none are passed again.
    run_ok(
        repo.path(),
        &["--resume", "task_0", "--no-git", "--tasks-dir", TASKS],
    );

    let state = task_state(repo.path(), TASKS, "task_0");
    assert_eq!(statuses(&state), ["completed", "completed"]);
    assert_eq!(line_count(&repo.path().join("calls.log")), 2);
    assert_eq!(
        task_list(repo.path(), TASKS)["tasks"]["task_0"]["completed"],
        true
    );
}

#[test]
fn bare_resume_picks_the_incomplete_task_then_errors_when_none_remain() {
    let repo = repo_with_files(&["src/Foo.ts"]);
    let input = write_input(repo.path(), &["src/Foo.ts"]);

    run_ok(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "FAKE_LOG=calls.log",
            "--dry-run",
            "--no-git",
            "--tasks-dir",
            TASKS,
        ],
    );

    let stdout = run_ok(repo.path(), &["--resume", "--no-git", "--tasks-dir", TASKS]);
    assert!(
        stdout.contains("Resuming first incomplete task"),
        "{stdout}"
    );
    let state = task_state(repo.path(), TASKS, "task_0");
    assert_eq!(file_state(&state, "src/Foo.ts")["status"], "completed");
    assert_eq!(line_count(&repo.path().join("calls.log")), 1);

    let out = run_err(repo.path(), &["--resume", "--no-git", "--tasks-dir", TASKS]);
    assert!(out.contains("No incomplete tasks to resume"), "{out}");
    assert_eq!(line_count(&repo.path().join("calls.log")), 1);
}

#[test]
fn resume_with_empty_tasks_dir_errors() {
    let repo = repo_with_files(&["src/Foo.ts"]);
    let out = run_err(repo.path(), &["--resume", "--tasks-dir", TASKS]);
    assert!(out.contains("No incomplete tasks to resume"), "{out}");
}

#[test]
fn resume_unknown_task_errors() {
    let repo = repo_with_files(&["src/Foo.ts"]);
    let out = run_err(repo.path(), &["--resume", "task_42", "--tasks-dir", TASKS]);
    assert!(out.contains("Task not found: task_42"), "{out}");
}

#[test]
fn max_files_limits_each_run_and_resume_finishes_the_rest() {
    let files = ["src/A.ts", "src/B.ts", "src/C.ts"];
    let repo = repo_with_files(&files);
    let input = write_input(repo.path(), &files);

    run_ok(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "FAKE_LOG=calls.log",
            "--max-files",
            "1",
            "--no-git",
            "--tasks-dir",
            TASKS,
        ],
    );
    let state = task_state(repo.path(), TASKS, "task_0");
    assert_eq!(statuses(&state), ["completed", "pending", "pending"]);
    assert_eq!(state["config"]["max_files"], 1);
    assert_eq!(line_count(&repo.path().join("calls.log")), 1);
    // Pending files remain, so the task stays resumable.
    assert_eq!(
        task_list(repo.path(), TASKS)["tasks"]["task_0"]["completed"],
        false
    );

    // The saved --max-files 1 still applies on a bare resume.
    run_ok(repo.path(), &["--resume", "--no-git", "--tasks-dir", TASKS]);
    let state = task_state(repo.path(), TASKS, "task_0");
    assert_eq!(statuses(&state), ["completed", "completed", "pending"]);

    // A CLI --max-files overrides the saved one.
    run_ok(
        repo.path(),
        &[
            "--resume",
            "task_0",
            "--max-files",
            "5",
            "--no-git",
            "--tasks-dir",
            TASKS,
        ],
    );
    let state = task_state(repo.path(), TASKS, "task_0");
    assert_eq!(statuses(&state), ["completed", "completed", "completed"]);
    // The resume override is saved back to the task's config.
    assert_eq!(state["config"]["max_files"], 5);

    // Every file was prompted exactly once across the three runs.
    let calls = std::fs::read_to_string(repo.path().join("calls.log")).unwrap();
    let mut calls: Vec<&str> = calls.lines().collect();
    calls.sort();
    assert_eq!(calls, ["src/A.ts", "src/B.ts", "src/C.ts"]);
    assert_eq!(
        task_list(repo.path(), TASKS)["tasks"]["task_0"]["completed"],
        true
    );
}

#[test]
fn resume_sends_awaiting_verification_files_straight_to_verify() {
    let repo = repo_with_files(&["src/Foo.ts"]);
    let input = write_input(repo.path(), &["src/Foo.ts"]);

    run_ok(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "FAKE_LOG=prompts.log",
            "--verify",
            "test -f {file}.fixed",
            "--fixup",
            "FAKE_WRITE=src/Foo.ts.fixed",
            "--dry-run",
            "--no-git",
            "--tasks-dir",
            TASKS,
        ],
    );

    // Simulate a run interrupted after the prompt phase.
    let state_path = repo.path().join(TASKS).join("tasks/state_0.json");
    let mut state = read_json(&state_path);
    state["files"]["src/Foo.ts"]["status"] = "awaiting_verification".into();
    std::fs::write(&state_path, serde_json::to_string(&state).unwrap()).unwrap();

    run_ok(
        repo.path(),
        &["--resume", "task_0", "--no-git", "--tasks-dir", TASKS],
    );

    let state = task_state(repo.path(), TASKS, "task_0");
    let fs = file_state(&state, "src/Foo.ts");
    assert_eq!(fs["status"], "completed");
    assert_eq!(fs["attempts"], 1);
    // The main prompt is not re-run for a file that already passed it.
    assert!(!repo.path().join("prompts.log").exists());
    assert!(repo.path().join("src/Foo.ts.fixed").exists());
}

#[test]
fn fixup_that_cannot_launch_claude_marks_file_failed() {
    let repo = repo_with_files(&["src/Foo.ts"]);
    let input = write_input(repo.path(), &["src/Foo.ts"]);

    run_ok(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "noop",
            "--verify",
            "false",
            "--dry-run",
            "--no-git",
            "--tasks-dir",
            TASKS,
        ],
    );
    let state_path = repo.path().join(TASKS).join("tasks/state_0.json");
    let mut state = read_json(&state_path);
    state["files"]["src/Foo.ts"]["status"] = "fixup_in_progress".into();
    std::fs::write(&state_path, serde_json::to_string(&state).unwrap()).unwrap();

    // Resume with a claude binary that does not exist; the saved fake is overridden.
    let output = runner_command(
        repo.path(),
        &[
            "--resume",
            "task_0",
            "--no-git",
            "--tasks-dir",
            TASKS,
            "--claude-bin",
            "/nonexistent/claude",
        ],
    )
    .output()
    .unwrap();
    assert!(output.status.success());

    let state = task_state(repo.path(), TASKS, "task_0");
    let fs = file_state(&state, "src/Foo.ts");
    assert_eq!(fs["status"], "failed");
    assert_eq!(fs["attempts"], 1);
    assert_eq!(fs["last_error"], "Failed to execute claude CLI");
    let log = std::fs::read_to_string(repo.path().join(TASKS).join("failures/Foo.ts.log")).unwrap();
    assert!(log.contains("FIXUP COMMAND FAILED"), "{log}");
}

#[test]
fn resume_flags_are_saved_including_explicit_defaults() {
    let repo = repo_with_files(&["src/Foo.ts"]);
    let input = write_input(repo.path(), &["src/Foo.ts"]);
    let saved = |key: &str| task_state(repo.path(), TASKS, "task_0")["config"][key].clone();

    run_ok(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "noop",
            "-c",
            "9",
            "--max-retries",
            "6",
            "--dry-run",
            "--no-git",
            "--tasks-dir",
            TASKS,
        ],
    );
    assert_eq!(
        (saved("concurrency"), saved("max_retries")),
        (9.into(), 6.into())
    );

    // A non-default override on resume is written back to the task's state.
    let resume = |extra: &[&str]| {
        let mut args = vec![
            "--resume",
            "task_0",
            "--dry-run",
            "--no-git",
            "--tasks-dir",
            TASKS,
        ];
        args.extend_from_slice(extra);
        run_ok(repo.path(), &args);
    };
    resume(&["-c", "7"]);
    assert_eq!(
        (saved("concurrency"), saved("max_retries")),
        (7.into(), 6.into())
    );

    // Passing the default value explicitly still overrides the saved one.
    resume(&["-c", "5", "--max-retries", "3"]);
    assert_eq!(
        (saved("concurrency"), saved("max_retries")),
        (5.into(), 3.into())
    );

    // A bare resume keeps what was saved.
    resume(&[]);
    assert_eq!(
        (saved("concurrency"), saved("max_retries")),
        (5.into(), 3.into())
    );
}
