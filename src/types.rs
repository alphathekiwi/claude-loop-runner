use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Status of a file in the processing pipeline
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
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

impl Default for FileStatus {
    fn default() -> Self {
        Self::Pending
    }
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
