use crate::types::{ParsedResult, ProcessOutput};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

/// Expand pattern placeholders with file path components
/// Supports: {file}, {file_stem}, {file_dir}
pub fn expand_pattern(pattern: &str, file_path: &Path) -> String {
    let file_str = file_path.to_string_lossy();

    let file_stem = file_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    let file_dir = file_path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    pattern
        .replace("{file}", &file_str)
        .replace("{file_stem}", &file_stem)
        .replace("{file_dir}", &file_dir)
}

/// Run a shell command and capture output
pub async fn run_command(command: &str) -> Result<ProcessOutput> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("Failed to execute command")?;

    Ok(ProcessOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code().unwrap_or(-1),
    })
}

/// Parse result from Claude's output
/// Looks for lines starting with "RESULT:" and tries to parse as JSON
pub fn parse_result(stdout: &str) -> ParsedResult {
    // Search from the end for the last RESULT: line
    for line in stdout.lines().rev() {
        let trimmed = line.trim();
        if let Some(json_str) = trimmed.strip_prefix("RESULT:") {
            let json_str = json_str.trim();
            if json_str.is_empty() {
                continue;
            }
            // Try to parse as JSON
            match serde_json::from_str(json_str) {
                Ok(value) => {
                    return ParsedResult {
                        value,
                        is_raw: false,
                    };
                }
                Err(_) => {
                    // Store as raw string
                    return ParsedResult {
                        value: serde_json::Value::String(json_str.to_string()),
                        is_raw: true,
                    };
                }
            }
        }
    }

    // No result found
    ParsedResult {
        value: serde_json::Value::Null,
        is_raw: false,
    }
}

/// Check if a file path matches the allowed pattern (glob-style)
pub fn matches_allowlist(path: &Path, pattern: &str) -> bool {
    let path_str = path.to_string_lossy();

    // Handle patterns ending with *
    if let Some(prefix) = pattern.strip_suffix('*') {
        // Check if any component of the path starts with the prefix
        for component in path.components() {
            if let std::path::Component::Normal(s) = component {
                if s.to_string_lossy().starts_with(prefix) {
                    return true;
                }
            }
        }
        // Also check the full path
        path_str.contains(prefix)
    } else {
        // Exact match or contains
        path_str.contains(pattern)
    }
}

/// Get list of files modified since last commit (or all uncommitted changes)
/// Returns (allowed_files, unauthorized_files) based on the allowlist pattern
#[allow(dead_code)]
pub async fn check_git_changes(
    allowlist_pattern: &str,
    working_dir: &Path,
) -> Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    // Get list of modified/added/deleted files
    let output = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("Failed to run git status")?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut allowed = Vec::new();
    let mut unauthorized = Vec::new();

    for line in stdout.lines() {
        // git status --porcelain format: XY filename
        // First two chars are status, then space, then filename
        if line.len() < 3 {
            continue;
        }
        let file_path = line[3..].trim();
        if file_path.is_empty() {
            continue;
        }

        let path = PathBuf::from(file_path);

        if matches_allowlist(&path, allowlist_pattern) {
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
    use std::path::PathBuf;

    #[test]
    fn test_expand_pattern() {
        let path = PathBuf::from("src/reducer/teamsReducer.test.ts");

        assert_eq!(
            expand_pattern("{file}", &path),
            "src/reducer/teamsReducer.test.ts"
        );
        assert_eq!(expand_pattern("{file_stem}*", &path), "teamsReducer.test*");
        assert_eq!(expand_pattern("{file_dir}/*.ts", &path), "src/reducer/*.ts");
        assert_eq!(
            expand_pattern("{file_dir}/{file_stem}*", &path),
            "src/reducer/teamsReducer.test*"
        );
    }

    #[test]
    fn test_matches_allowlist() {
        // Pattern: teamsReducer*
        assert!(matches_allowlist(
            &PathBuf::from("src/reducer/teamsReducer.ts"),
            "teamsReducer*"
        ));
        assert!(matches_allowlist(
            &PathBuf::from("src/reducer/teamsReducer.test.ts"),
            "teamsReducer*"
        ));
        assert!(!matches_allowlist(
            &PathBuf::from("src/reducer/userReducer.ts"),
            "teamsReducer*"
        ));

        // Exact pattern
        assert!(matches_allowlist(
            &PathBuf::from("src/reducer/teamsReducer.ts"),
            "teamsReducer.ts"
        ));
        assert!(!matches_allowlist(
            &PathBuf::from("src/reducer/teamsReducer.test.ts"),
            "teamsReducer.ts"
        ));
    }

    #[test]
    fn test_parse_result_json() {
        let stdout = r#"
Some output
RESULT: {"coverage": 78.5, "lines": 100}
More output
"#;
        let result = parse_result(stdout);
        assert!(!result.is_raw);
        assert_eq!(result.value["coverage"], 78.5);
    }

    #[test]
    fn test_parse_result_string() {
        let stdout = r#"
Some output
RESULT: "done"
"#;
        let result = parse_result(stdout);
        assert!(!result.is_raw);
        assert_eq!(result.value, "done");
    }

    #[test]
    fn test_parse_result_raw() {
        let stdout = r#"
Some output
RESULT: not valid json
"#;
        let result = parse_result(stdout);
        assert!(result.is_raw);
        assert_eq!(result.value, "not valid json");
    }

    #[test]
    fn test_parse_result_none() {
        let stdout = "Some output without result";
        let result = parse_result(stdout);
        assert!(!result.is_raw);
        assert!(result.value.is_null());
    }

    #[test]
    fn test_parse_result_last_wins() {
        let stdout = r#"
RESULT: {"first": 1}
RESULT: {"second": 2}
"#;
        let result = parse_result(stdout);
        assert_eq!(result.value["second"], 2);
    }
}
