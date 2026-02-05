use crate::types::{ParsedResult, ProcessOutput};
use anyhow::{Context, Result};
use glob::glob;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

/// Extract the file stem, stripping both the extension and common test suffixes (.test, .spec)
/// e.g., "parser.test.ts" -> "parser", "component.spec.tsx" -> "component"
pub fn extract_file_stem(file_path: &Path) -> String {
    let stem = file_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    // Strip common test suffixes
    stem.strip_suffix(".test")
        .or_else(|| stem.strip_suffix(".spec"))
        .map(|s| s.to_string())
        .unwrap_or(stem)
}

/// Expand pattern placeholders with file path components
/// Supports: {file}, {file_stem}, {file_dir}, {all_files}, {test_files}, {created_files}
pub fn expand_pattern(pattern: &str, file_path: &Path) -> String {
    expand_pattern_with_allowlist(pattern, file_path, "{file_stem}*")
}

/// Expand pattern placeholders with file path components and a custom allowlist
/// Supports: {file}, {file_stem}, {file_dir}, {all_files}, {test_files}, {created_files}
pub fn expand_pattern_with_allowlist(pattern: &str, file_path: &Path, allowlist: &str) -> String {
    let file_str = file_path.to_string_lossy();

    let file_stem = extract_file_stem(file_path);

    let file_dir = file_path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    // Only compute these if needed (they involve filesystem operations)
    let all_files = if pattern.contains("{all_files}") {
        find_all_files(file_path, allowlist).join(" ")
    } else {
        String::new()
    };

    let test_files = if pattern.contains("{test_files}") {
        find_test_files(file_path, allowlist).join(" ")
    } else {
        String::new()
    };

    let created_files = if pattern.contains("{created_files}") {
        find_created_files(file_path, allowlist).join(" ")
    } else {
        String::new()
    };

    pattern
        .replace("{file}", &file_str)
        .replace("{file_stem}", &file_stem)
        .replace("{file_dir}", &file_dir)
        .replace("{all_files}", &all_files)
        .replace("{test_files}", &test_files)
        .replace("{created_files}", &created_files)
}

/// Find all files matching the allowlist pattern (includes the source file)
/// Returns: {file} and any files that match the allowlist glob
pub fn find_all_files(file_path: &Path, allowlist_pattern: &str) -> Vec<String> {
    let glob_pattern = expand_allowlist_to_glob(file_path, allowlist_pattern);
    let mut files = collect_glob_matches(&glob_pattern);

    // Ensure the source file is included
    let file_str = file_path.to_string_lossy().to_string();
    if !files.contains(&file_str) {
        files.insert(0, file_str);
    }

    files
}

/// Find test files that likely correspond to the source file
/// Looks for files with common test patterns: *.test.*, *.spec.*, *_test.*, *_spec.*
pub fn find_test_files(file_path: &Path, allowlist_pattern: &str) -> Vec<String> {
    let all_files = find_all_files(file_path, allowlist_pattern);
    let file_str = file_path.to_string_lossy().to_string();

    all_files
        .into_iter()
        .filter(|f| {
            // Exclude the source file itself
            if f == &file_str {
                return false;
            }
            // Match common test file patterns
            let lower = f.to_lowercase();
            lower.contains(".test.")
                || lower.contains(".spec.")
                || lower.contains("_test.")
                || lower.contains("_spec.")
                || lower.contains("/test/")
                || lower.contains("/tests/")
                || lower.contains("/__tests__/")
        })
        .collect()
}

/// Find files that match the allowlist glob but are NOT the source file itself
/// These are likely files created by Claude during processing
pub fn find_created_files(file_path: &Path, allowlist_pattern: &str) -> Vec<String> {
    let glob_pattern = expand_allowlist_to_glob(file_path, allowlist_pattern);
    let files = collect_glob_matches(&glob_pattern);
    let file_str = file_path.to_string_lossy().to_string();

    files.into_iter().filter(|f| f != &file_str).collect()
}

/// Expand an allowlist pattern to a glob pattern for filesystem searching
fn expand_allowlist_to_glob(file_path: &Path, allowlist_pattern: &str) -> String {
    let file_str = file_path.to_string_lossy();

    let file_stem = extract_file_stem(file_path);

    let file_dir = file_path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());

    // Replace placeholders in the allowlist pattern
    let expanded = allowlist_pattern
        .replace("{file}", &file_str)
        .replace("{file_stem}", &file_stem)
        .replace("{file_dir}", &file_dir);

    // If the pattern doesn't contain a directory separator, search in the file's directory
    if !expanded.contains('/') && !expanded.contains('\\') {
        if file_dir.is_empty() || file_dir == "." {
            expanded
        } else {
            format!("{}/{}", file_dir, expanded)
        }
    } else {
        expanded
    }
}

/// Collect all files matching a glob pattern
fn collect_glob_matches(pattern: &str) -> Vec<String> {
    match glob(pattern) {
        Ok(paths) => paths
            .filter_map(|entry| entry.ok())
            .filter(|p| p.is_file())
            .map(|p| p.to_string_lossy().to_string())
            .collect(),
        Err(_) => Vec::new(),
    }
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
    fn test_extract_file_stem() {
        // Regular file - just strips extension
        assert_eq!(
            extract_file_stem(&PathBuf::from("src/utils/parser.ts")),
            "parser"
        );

        // Test file - strips both .test and extension
        assert_eq!(
            extract_file_stem(&PathBuf::from("src/utils/parser.test.ts")),
            "parser"
        );

        // Spec file - strips both .spec and extension
        assert_eq!(
            extract_file_stem(&PathBuf::from("src/utils/parser.spec.tsx")),
            "parser"
        );

        // File with multiple dots but no test/spec suffix
        assert_eq!(
            extract_file_stem(&PathBuf::from("src/config.dev.ts")),
            "config.dev"
        );
    }

    #[test]
    fn test_expand_pattern() {
        let path = PathBuf::from("src/reducer/teamsReducer.test.ts");

        assert_eq!(
            expand_pattern("{file}", &path),
            "src/reducer/teamsReducer.test.ts"
        );
        // file_stem now strips .test suffix
        assert_eq!(expand_pattern("{file_stem}*", &path), "teamsReducer*");
        assert_eq!(expand_pattern("{file_dir}/*.ts", &path), "src/reducer/*.ts");
        assert_eq!(
            expand_pattern("{file_dir}/{file_stem}*", &path),
            "src/reducer/teamsReducer*"
        );

        // Test with a non-test file
        let regular_path = PathBuf::from("src/reducer/teamsReducer.ts");
        assert_eq!(
            expand_pattern("{file_stem}*", &regular_path),
            "teamsReducer*"
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

    #[test]
    fn test_expand_allowlist_to_glob() {
        let path = PathBuf::from("src/reducer/teamsReducer.ts");

        // Default pattern {file_stem}* should become src/reducer/teamsReducer*
        assert_eq!(
            expand_allowlist_to_glob(&path, "{file_stem}*"),
            "src/reducer/teamsReducer*"
        );

        // Pattern with dir placeholder
        assert_eq!(
            expand_allowlist_to_glob(&path, "{file_dir}/*.ts"),
            "src/reducer/*.ts"
        );

        // Absolute pattern (already has directory)
        assert_eq!(
            expand_allowlist_to_glob(&path, "src/**/*.ts"),
            "src/**/*.ts"
        );
    }

    #[test]
    fn test_is_test_file_pattern() {
        // These should match test file patterns
        let test_patterns = vec![
            "src/component.test.ts",
            "src/component.spec.ts",
            "src/component_test.py",
            "src/component_spec.rb",
            "src/test/component.ts",
            "src/tests/component.ts",
            "src/__tests__/component.ts",
        ];

        for pattern in test_patterns {
            let lower = pattern.to_lowercase();
            let is_test = lower.contains(".test.")
                || lower.contains(".spec.")
                || lower.contains("_test.")
                || lower.contains("_spec.")
                || lower.contains("/test/")
                || lower.contains("/tests/")
                || lower.contains("/__tests__/");
            assert!(is_test, "Expected {} to match test pattern", pattern);
        }

        // These should NOT match test file patterns
        let non_test_patterns = vec!["src/component.ts", "src/testing.ts", "src/testUtils.ts"];

        for pattern in non_test_patterns {
            let lower = pattern.to_lowercase();
            let is_test = lower.contains(".test.")
                || lower.contains(".spec.")
                || lower.contains("_test.")
                || lower.contains("_spec.")
                || lower.contains("/test/")
                || lower.contains("/tests/")
                || lower.contains("/__tests__/");
            assert!(!is_test, "Expected {} to NOT match test pattern", pattern);
        }
    }
}
