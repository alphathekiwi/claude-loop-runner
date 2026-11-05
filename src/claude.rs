use crate::process::expand_pattern;
use crate::types::ProcessOutput;
use anyhow::{Context, Result};
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

/// Instruction appended to prompts to get structured result output
pub const RESULT_INSTRUCTION: &str = r#"

When you have finished the task, output your result data as JSON on a single line starting with "RESULT:"
Example: RESULT: {"coverage": 78.5}
If you have no structured data to report, output: RESULT: "done"
"#;

/// Build the full prompt with file context and result instruction
pub fn build_prompt(
    base_prompt: &str,
    file_path: &Path,
    original_data: &serde_json::Value,
    allowlist_pattern: &str,
) -> String {
    let allowlist = expand_pattern(allowlist_pattern, file_path);
    let original_data_str =
        serde_json::to_string(original_data).unwrap_or_else(|_| "null".to_string());

    format!(
        "{base_prompt}

IMPORTANT: You may ONLY read and modify files matching the pattern: {allowlist}
Do not edit any other files.

File: {file}
Original data: {original_data}
{result_instruction}",
        base_prompt = base_prompt,
        allowlist = allowlist,
        file = file_path.display(),
        original_data = original_data_str,
        result_instruction = RESULT_INSTRUCTION,
    )
}

/// Build fixup prompt with error context
pub fn build_fixup_prompt(
    fixup_prompt: &str,
    file_path: &Path,
    error_output: &str,
    allowlist_pattern: &str,
) -> String {
    let allowlist = expand_pattern(allowlist_pattern, file_path);

    format!(
        "{fixup_prompt}

IMPORTANT: You may ONLY read and modify files matching the pattern: {allowlist}
Do not edit any other files.

File: {file}

Verification failed with the following error:
```
{error}
```

Please fix the issues and try again.
{result_instruction}",
        fixup_prompt = fixup_prompt,
        allowlist = allowlist,
        file = file_path.display(),
        error = error_output,
        result_instruction = RESULT_INSTRUCTION,
    )
}

/// Run the Claude CLI with the given prompt
pub async fn run_claude(
    claude_bin: &str,
    prompt: &str,
    working_dir: &Path,
) -> Result<ProcessOutput> {
    let output = Command::new(claude_bin)
        .arg("-p")
        .arg(prompt)
        .arg("--dangerously-skip-permissions") // Non-interactive mode
        .current_dir(working_dir)
        // If the worker is cancelled (Ctrl-C), don't leave claude editing files unsupervised
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("Failed to execute claude CLI")?;

    Ok(ProcessOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code().unwrap_or(-1),
    })
}

/// Treat a non-zero Claude exit as an error, so a crashed run is never marked done
pub fn require_success(output: ProcessOutput) -> Result<ProcessOutput> {
    if output.exit_code == 0 {
        return Ok(output);
    }
    let detail = if output.stderr.trim().is_empty() {
        output.stdout.trim()
    } else {
        output.stderr.trim()
    };
    anyhow::bail!("claude exited with code {}: {}", output.exit_code, detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fake_claude_path, git, temp_git_repo, write_file};

    #[test]
    fn build_prompt_includes_allowlist_file_and_result_instruction() {
        let prompt = build_prompt(
            "Add tests",
            Path::new("src/Foo.tsx"),
            &serde_json::json!({"coverage": 10}),
            "{file_stem}*",
        );
        assert!(prompt.starts_with("Add tests"));
        assert!(prompt.contains("matching the pattern: Foo*"));
        assert!(prompt.contains("File: src/Foo.tsx"));
        assert!(prompt.contains(r#"Original data: {"coverage":10}"#));
        assert!(prompt.contains("RESULT:"));
    }

    #[test]
    fn build_fixup_prompt_includes_error_output() {
        let prompt = build_fixup_prompt(
            "Fix it",
            Path::new("src/Foo.tsx"),
            "expected 1, got 2",
            "{file_stem}*",
        );
        assert!(prompt.starts_with("Fix it"));
        assert!(prompt.contains("expected 1, got 2"));
        assert!(prompt.contains("File: src/Foo.tsx"));
    }

    #[tokio::test]
    async fn run_claude_runs_binary_in_working_dir() {
        let repo = temp_git_repo();
        write_file(repo.path(), "src/Foo.tsx", "export {}\n");
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-q", "-m", "add Foo"]);
        let prompt = build_prompt(
            "FAKE_APPEND_TARGET\nFAKE_RESULT={\"coverage\": 90}",
            Path::new("src/Foo.tsx"),
            &serde_json::Value::Null,
            "{file_stem}*",
        );

        let fake = fake_claude_path();
        let output = run_claude(fake.to_str().unwrap(), &prompt, repo.path())
            .await
            .unwrap();

        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.contains(r#"RESULT: {"coverage": 90}"#));
        assert_eq!(
            git(repo.path(), &["status", "--porcelain"]),
            "M src/Foo.tsx"
        );
    }

    #[tokio::test]
    async fn run_claude_reports_exit_code_and_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let fake = fake_claude_path();
        let output = run_claude(
            fake.to_str().unwrap(),
            "FAKE_EXIT=3\nFAKE_STDERR=boom",
            dir.path(),
        )
        .await
        .unwrap();

        assert_eq!(output.exit_code, 3);
        assert!(output.stderr.contains("boom"));
    }

    #[test]
    fn require_success_passes_zero_exit_through() {
        let output = ProcessOutput {
            stdout: "ok".into(),
            stderr: String::new(),
            exit_code: 0,
        };
        assert_eq!(require_success(output).unwrap().stdout, "ok");
    }

    #[test]
    fn require_success_rejects_non_zero_exit_with_stderr() {
        let output = ProcessOutput {
            stdout: "partial".into(),
            stderr: "boom\n".into(),
            exit_code: 2,
        };
        let err = require_success(output).unwrap_err().to_string();
        assert_eq!(err, "claude exited with code 2: boom");
    }

    #[tokio::test]
    async fn run_claude_errors_when_binary_missing() {
        let dir = tempfile::tempdir().unwrap();
        let err = run_claude("/nonexistent/claude", "hi", dir.path())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Failed to execute claude CLI"));
    }
}
