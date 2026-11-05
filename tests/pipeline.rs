//! End-to-end runs of the prompt -> verify -> fixup pipeline, asserting on persisted state.

mod common;

use common::{
    file_state, line_count, repo_with_files, run_err, run_ok, runner_command, task_list,
    task_state, write_input,
};
use serde_json::json;

const TASKS: &str = "clr-tasks";

#[test]
fn concurrent_prompts_verify_and_complete_every_file() {
    let files = ["src/Alpha.ts", "src/Beta.ts", "src/Gamma.ts"];
    let repo = repo_with_files(&files);
    let input = write_input(repo.path(), &files);

    // The barrier only releases once all three claude calls are running at once,
    // so this cannot pass with fewer than three prompt workers.
    run_ok(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "FAKE_BARRIER=3\nFAKE_APPEND_TARGET\nFAKE_RESULT={\"coverage\": 100}\nFAKE_LOG=calls.log",
            "--verify",
            "grep -q 'edited by fake-claude' {file}",
            "--concurrency",
            "3",
            "--no-git",
            "--tasks-dir",
            TASKS,
        ],
    );

    let state = task_state(repo.path(), TASKS, "task_0");
    for (i, file) in files.iter().enumerate() {
        let fs = file_state(&state, file);
        assert_eq!(fs["status"], "completed", "{file}: {fs}");
        assert_eq!(fs["attempts"], 0);
        assert_eq!(fs["original_data"], json!({ "index": i }));
        assert_eq!(fs["result_data"], json!({ "coverage": 100 }));
        assert!(fs.get("result_data_raw").is_none());
        assert!(fs.get("last_error").is_none());
        let contents = std::fs::read_to_string(repo.path().join(file)).unwrap();
        assert!(
            contents.contains("edited by fake-claude"),
            "{file} not edited"
        );
    }
    assert_eq!(line_count(&repo.path().join("calls.log")), 3);
    assert_eq!(state["config"]["concurrency"], 3);
    assert!(!repo.path().join(TASKS).join("failures").exists());

    let list = task_list(repo.path(), TASKS);
    assert_eq!(list["tasks"]["task_0"]["completed"], true);
    assert_eq!(list["tasks"]["task_0"]["state_file"], "tasks/state_0.json");
}

#[test]
fn failed_verification_runs_fixup_then_passes() {
    let repo = repo_with_files(&["src/Foo.ts"]);
    let input = write_input(repo.path(), &["src/Foo.ts"]);

    run_ok(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "FAKE_RESULT={\"stage\": \"prompt\"}",
            "--verify",
            "test -f {file}.fixed || { echo 'missing fixed marker' >&2; exit 1; }",
            "--fixup",
            "FAKE_WRITE=src/Foo.ts.fixed\nFAKE_RESULT={\"stage\": \"fixup\"}\nFAKE_LOG=fixups.log",
            "--no-git",
            "--tasks-dir",
            TASKS,
        ],
    );

    let state = task_state(repo.path(), TASKS, "task_0");
    let fs = file_state(&state, "src/Foo.ts");
    assert_eq!(fs["status"], "completed");
    assert_eq!(fs["attempts"], 1);
    // The fixup's RESULT replaces the prompt's.
    assert_eq!(fs["result_data"], json!({ "stage": "fixup" }));
    assert!(fs.get("last_error").is_none());
    assert_eq!(line_count(&repo.path().join("fixups.log")), 1);

    let log = std::fs::read_to_string(repo.path().join(TASKS).join("failures/Foo.ts.log")).unwrap();
    assert!(log.contains("VERIFICATION FAILED (attempt 1/4)"), "{log}");
    assert!(log.contains("missing fixed marker"), "{log}");
    // The fixup prompt carries the verify error output and the file under work.
    assert!(log.contains("FIXUP PROMPT SENT:"), "{log}");
    assert!(log.contains("File: src/Foo.ts"), "{log}");
    assert!(log.contains("CLAUDE FIXUP RESPONSE:"), "{log}");
    assert!(!log.contains("FINAL STATUS"), "{log}");
}

#[test]
fn verification_that_never_passes_marks_file_failed_after_max_retries() {
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
            "echo 'still broken' >&2; exit 2",
            "--fixup",
            "FAKE_LOG=fixups.log",
            "--max-retries",
            "3",
            "--no-git",
            "--tasks-dir",
            TASKS,
        ],
    );

    let state = task_state(repo.path(), TASKS, "task_0");
    let fs = file_state(&state, "src/Foo.ts");
    assert_eq!(fs["status"], "failed");
    assert_eq!(fs["attempts"], 4);
    assert_eq!(fs["last_error"], "still broken\n");
    // Three retries: four verifications with a fixup between each.
    assert_eq!(line_count(&repo.path().join("fixups.log")), 3);

    let log = std::fs::read_to_string(repo.path().join(TASKS).join("failures/Foo.ts.log")).unwrap();
    for attempt in 1..=4 {
        assert!(
            log.contains(&format!("VERIFICATION FAILED (attempt {attempt}/4)")),
            "{log}"
        );
    }
    assert!(log.contains("Exit code: 2"), "{log}");
    assert!(
        log.contains("FINAL STATUS: FAILED after max retries"),
        "{log}"
    );

    // A failed file still finishes the task.
    assert_eq!(
        task_list(repo.path(), TASKS)["tasks"]["task_0"]["completed"],
        true
    );
}

#[test]
fn max_retries_counts_fixup_attempts() {
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
            "--fixup",
            "FAKE_LOG=fixups.log",
            "--max-retries",
            "1",
            "--no-git",
            "--tasks-dir",
            TASKS,
        ],
    );
    // --help: "Maximum number of fixup retry attempts" -- 1 should allow one fixup.
    assert_eq!(line_count(&repo.path().join("fixups.log")), 1);
}

#[test]
fn verify_stdout_is_used_as_error_when_stderr_is_empty() {
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
            "echo 'stdout failure'; false",
            "--max-retries",
            "1",
            "--no-git",
            "--tasks-dir",
            TASKS,
        ],
    );

    let state = task_state(repo.path(), TASKS, "task_0");
    let fs = file_state(&state, "src/Foo.ts");
    assert_eq!(fs["status"], "failed");
    assert_eq!(fs["attempts"], 2);
    assert_eq!(fs["last_error"], "stdout failure\n");
}

#[test]
fn without_verify_files_complete_with_raw_result_captured() {
    let repo = repo_with_files(&["src/Foo.ts"]);
    let input = write_input(repo.path(), &["src/Foo.ts"]);

    run_ok(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "FAKE_RESULT=coverage went up",
            "--no-git",
            "--tasks-dir",
            TASKS,
        ],
    );

    let state = task_state(repo.path(), TASKS, "task_0");
    let fs = file_state(&state, "src/Foo.ts");
    assert_eq!(fs["status"], "completed");
    assert_eq!(fs["result_data"], "coverage went up");
    assert_eq!(fs["result_data_raw"], true);
    assert!(state["config"].get("verification_cmd").is_none());
}

#[test]
fn claude_non_zero_exit_marks_file_failed() {
    let repo = repo_with_files(&["src/Foo.ts"]);
    let input = write_input(repo.path(), &["src/Foo.ts"]);
    run_ok(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "FAKE_EXIT=1\nFAKE_STDERR=claude crashed",
            "--no-git",
            "--tasks-dir",
            TASKS,
        ],
    );
    let state = task_state(repo.path(), TASKS, "task_0");
    let fs = file_state(&state, "src/Foo.ts");
    assert_eq!(fs["status"], "failed");
    assert!(
        fs["last_error"]
            .as_str()
            .unwrap()
            .contains("claude crashed"),
        "{fs}"
    );
}

#[test]
fn missing_claude_binary_marks_file_failed() {
    let repo = repo_with_files(&["src/Foo.ts"]);
    let input = write_input(repo.path(), &["src/Foo.ts"]);

    let output = runner_command(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "noop",
            "--verify",
            "true",
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
    assert_eq!(fs["last_error"], "Failed to execute claude CLI");
    assert_eq!(state["config"]["claude_bin"], "/nonexistent/claude");
}

#[test]
fn input_and_prompt_are_required_for_new_tasks() {
    let repo = repo_with_files(&["src/Foo.ts"]);
    let input = write_input(repo.path(), &["src/Foo.ts"]);

    let out = run_err(repo.path(), &["--prompt", "noop", "--tasks-dir", TASKS]);
    assert!(
        out.contains("--input is required when not using --resume"),
        "{out}"
    );

    let out = run_err(repo.path(), &["--input", input, "--tasks-dir", TASKS]);
    assert!(
        out.contains("--prompt is required when not using --resume"),
        "{out}"
    );

    // Validation happens before anything is written.
    assert!(!repo.path().join(TASKS).exists());
}

#[test]
fn zero_concurrency_is_rejected() {
    let repo = repo_with_files(&["src/Foo.ts"]);
    let input = write_input(repo.path(), &["src/Foo.ts"]);
    let out = run_err(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "noop",
            "--concurrency",
            "0",
            "--tasks-dir",
            TASKS,
        ],
    );
    assert!(out.contains("--concurrency must be at least 1"), "{out}");
}

#[test]
fn unreadable_input_file_errors() {
    let repo = repo_with_files(&["src/Foo.ts"]);
    let out = run_err(
        repo.path(),
        &[
            "--input",
            "missing.json",
            "--prompt",
            "noop",
            "--no-git",
            "--tasks-dir",
            TASKS,
        ],
    );
    assert!(
        out.contains("Failed to load input file: missing.json"),
        "{out}"
    );
}

#[test]
fn empty_input_completes_with_nothing_to_do() {
    let repo = common::temp_git_repo();
    common::write_file(repo.path(), "input.json", "{}");
    let stdout = run_ok(
        repo.path(),
        &[
            "--input",
            "input.json",
            "--prompt",
            "FAKE_LOG=calls.log",
            "--no-git",
            "--tasks-dir",
            TASKS,
        ],
    );
    assert!(stdout.contains("No files to process"), "{stdout}");
    assert!(!repo.path().join("calls.log").exists());
    assert_eq!(
        task_list(repo.path(), TASKS)["tasks"]["task_0"]["completed"],
        true
    );
}

#[test]
fn agent_created_file_names_cannot_inject_into_the_verify_command() {
    // Claude creates a file whose name matches the allowlist and contains shell syntax.
    let repo = repo_with_files(&["src/Foo.ts"]);
    let input = write_input(repo.path(), &["src/Foo.ts"]);
    run_ok(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "FAKE_WRITE=src/Foo;touch INJECTED;.ts",
            "--verify",
            "ls {created_files}",
            "--no-git",
            "--tasks-dir",
            TASKS,
        ],
    );

    assert!(!repo.path().join("INJECTED").exists());
    // The quoted name reached ls as one argument, so verification passed.
    let state = task_state(repo.path(), TASKS, "task_0");
    assert_eq!(file_state(&state, "src/Foo.ts")["status"], "completed");
}

#[test]
fn verify_placeholders_resolve_files_in_the_working_dir() {
    let repo = repo_with_files(&["src/Foo.ts", "src/Foo.test.ts"]);
    let input = write_input(repo.path(), &["src/Foo.ts"]);
    let elsewhere = tempfile::tempdir().unwrap();
    let input_abs = repo.path().join(input);

    run_ok(
        elsewhere.path(),
        &[
            "--input",
            input_abs.to_str().unwrap(),
            "--prompt",
            "noop",
            "-w",
            repo.path().to_str().unwrap(),
            "--verify",
            "test \"{test_files}\" = src/Foo.test.ts",
            "--max-retries",
            "0",
            "--no-git",
            "--tasks-dir",
            TASKS,
        ],
    );

    let state = task_state(elsewhere.path(), TASKS, "task_0");
    assert_eq!(file_state(&state, "src/Foo.ts")["status"], "completed");
}
