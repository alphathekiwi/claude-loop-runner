//! Ctrl-C handling: what the runner leaves behind when interrupted mid-run.

mod common;

use common::{
    fake_claude_path, file_state, repo_with_files, run_ok, runner_command, task_list, task_state,
    write_input,
};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

const TASKS: &str = "clr-tasks";

fn wait_for(path: &Path, timeout: Duration) {
    let start = Instant::now();
    while !path.exists() {
        assert!(
            start.elapsed() < timeout,
            "timed out waiting for {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn ctrl_c_stops_in_flight_claude_and_leaves_the_task_resumable() {
    let repo = repo_with_files(&["src/Foo.ts"]);
    let input = write_input(repo.path(), &["src/Foo.ts"]);

    // Claude signals it has started, then works for 2s before writing its output.
    let mut child = runner_command(
        repo.path(),
        &[
            "--input",
            input,
            "--prompt",
            "FAKE_WRITE=started\nFAKE_SLEEP=2\nFAKE_WRITE=finished\nFAKE_LOG=calls.log",
            "--no-git",
            "--tasks-dir",
            TASKS,
        ],
    )
    .arg("--claude-bin")
    .arg(fake_claude_path())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .unwrap();

    wait_for(&repo.path().join("started"), Duration::from_secs(10));
    let status = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let exit = child.wait().unwrap();
    assert!(
        exit.success(),
        "runner should exit cleanly on Ctrl-C: {exit:?}"
    );

    // Outlive the fake claude's 2s of work: an orphaned process would finish now.
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !repo.path().join("finished").exists(),
        "claude kept running after the runner exited"
    );

    // The interrupted file is left for a resume, not marked done.
    let state = task_state(repo.path(), TASKS, "task_0");
    assert_eq!(
        file_state(&state, "src/Foo.ts")["status"],
        "prompt_in_progress"
    );
    assert_eq!(
        task_list(repo.path(), TASKS)["tasks"]["task_0"]["completed"],
        false
    );

    // Resuming re-runs the interrupted prompt and completes the task.
    run_ok(
        repo.path(),
        &[
            "--resume",
            "task_0",
            "--prompt",
            "FAKE_LOG=calls.log",
            "--no-git",
            "--tasks-dir",
            TASKS,
        ],
    );
    let state = task_state(repo.path(), TASKS, "task_0");
    assert_eq!(file_state(&state, "src/Foo.ts")["status"], "completed");
    assert_eq!(
        task_list(repo.path(), TASKS)["tasks"]["task_0"]["completed"],
        true
    );
}
