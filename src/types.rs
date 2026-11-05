use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Status of a file in the processing pipeline
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    #[default]
    /// Not yet started
    Pending,
    /// Currently being processed by prompt worker
    PromptInProgress,
    /// Prompt complete, waiting for verification
    AwaitingVerification,
    /// Currently being verified
    VerifyInProgress,
    /// Verification failed, fixup in progress
    FixupInProgress,
    /// Successfully completed
    Completed,
    /// Failed after max retries
    Failed,
}

/// State of a single file being processed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileState {
    /// Current status in the pipeline
    pub status: FileStatus,
    /// Original metadata from input JSON
    pub original_data: serde_json::Value,
    /// Result data from Claude (parsed JSON or raw string)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_data: Option<serde_json::Value>,
    /// True if result_data is a raw unparsed string
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_data_raw: Option<bool>,
    /// Number of verification/fixup attempts
    #[serde(default)]
    pub attempts: u32,
    /// Last error message if failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl FileState {
    pub fn new(original_data: serde_json::Value) -> Self {
        Self {
            status: FileStatus::Pending,
            original_data,
            result_data: None,
            result_data_raw: None,
            attempts: 0,
            last_error: None,
        }
    }
}

/// A task to be processed by a worker
#[derive(Debug, Clone)]
pub struct FileTask {
    /// Path to the file
    pub path: PathBuf,
    /// Original metadata from input JSON
    pub original_data: serde_json::Value,
}

/// Output from running a subprocess
#[derive(Debug, Clone)]
pub struct ProcessOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Parsed result from Claude's output
#[derive(Debug, Clone)]
pub struct ParsedResult {
    /// The result value (JSON or string)
    pub value: serde_json::Value,
    /// True if value is a raw unparsed string
    pub is_raw: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn file_status_serializes_snake_case_and_round_trips() {
        let cases = [
            (FileStatus::Pending, "pending"),
            (FileStatus::PromptInProgress, "prompt_in_progress"),
            (FileStatus::AwaitingVerification, "awaiting_verification"),
            (FileStatus::VerifyInProgress, "verify_in_progress"),
            (FileStatus::FixupInProgress, "fixup_in_progress"),
            (FileStatus::Completed, "completed"),
            (FileStatus::Failed, "failed"),
        ];
        for (status, name) in cases {
            let encoded = serde_json::to_value(&status).unwrap();
            assert_eq!(encoded, json!(name));
            let decoded: FileStatus = serde_json::from_value(encoded).unwrap();
            assert_eq!(decoded, status);
        }
        assert_eq!(FileStatus::default(), FileStatus::Pending);
        assert!(serde_json::from_value::<FileStatus>(json!("PromptInProgress")).is_err());
    }

    #[test]
    fn new_file_state_is_pending_with_no_result() {
        let state = FileState::new(json!({"k": 1}));
        assert_eq!(state.status, FileStatus::Pending);
        assert_eq!(state.original_data, json!({"k": 1}));
        assert_eq!(state.attempts, 0);
        assert!(state.result_data.is_none());
        assert!(state.result_data_raw.is_none());
        assert!(state.last_error.is_none());
    }

    #[test]
    fn file_state_omits_unset_optionals_when_serialized() {
        let encoded = serde_json::to_value(FileState::new(json!(null))).unwrap();
        let obj = encoded.as_object().unwrap();
        let mut keys: Vec<_> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["attempts", "original_data", "status"]);
    }

    #[test]
    fn file_state_loads_minimal_older_json() {
        let decoded: FileState =
            serde_json::from_value(json!({"status": "completed", "original_data": [1, 2]}))
                .unwrap();
        assert_eq!(decoded.status, FileStatus::Completed);
        assert_eq!(decoded.original_data, json!([1, 2]));
        assert_eq!(decoded.attempts, 0);
        assert!(decoded.result_data.is_none());
        assert!(decoded.last_error.is_none());
    }

    #[test]
    fn file_state_requires_status_and_original_data() {
        assert!(serde_json::from_value::<FileState>(json!({"original_data": 1})).is_err());
        assert!(serde_json::from_value::<FileState>(json!({"status": "pending"})).is_err());
    }
}
