mod common;

use common::{git, run_runner, temp_git_repo, write_file};

#[test]
fn end_to_end_with_fake_claude_completes() {
    let repo = temp_git_repo();
    write_file(repo.path(), "src/Foo.ts", "export {}\n");
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "add Foo"]);
    write_file(repo.path(), "input.json", r#"{"src/Foo.ts": {}}"#);

    let output = run_runner(
        repo.path(),
        &[
            "--input",
            "input.json",
            "--prompt",
            "FAKE_APPEND_TARGET",
            "--verify",
            "true",
            "--no-git",
        ],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    let contents = std::fs::read_to_string(repo.path().join("src/Foo.ts")).unwrap();
    assert!(contents.contains("edited by fake-claude"));
}
