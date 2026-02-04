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
pub async fn run_claude(prompt: &str, working_dir: &Path) -> Result<ProcessOutput> {
    let output = Command::new("claude")
        .arg("-p")
        .arg(prompt)
        .arg("--dangerously-skip-permissions") // Non-interactive mode
        .current_dir(working_dir)
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
