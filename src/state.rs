use crate::config::Config;
use crate::git::GitState;
use crate::types::{FileState, FileStatus, ParsedResult};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Persistent state for the runner
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    /// Configuration for this run
    pub config: Config,
    /// State of each file being processed
    pub files: HashMap<PathBuf, FileState>,
    /// When this run started
    pub started_at: DateTime<Utc>,
    /// Last update time
    pub updated_at: DateTime<Utc>,
    /// Git state (dirty files, branch info)
    #[serde(default)]
    pub git_state: GitState,
}

impl State {
    /// Create a new state with the given config
    pub fn new(config: Config) -> Self {
        Self {
            config,
            files: HashMap::new(),
            started_at: Utc::now(),
            updated_at: Utc::now(),
            git_state: GitState::default(),
        }
    }

    /// Set the git state (called after capturing initial git status)
    pub fn set_git_state(&mut self, git_state: GitState) {
        self.git_state = git_state;
    }

    /// Load state from a file
    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read state file: {}", path.display()))?;
        let state: State = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse state file: {}", path.display()))?;
        Ok(state)
    }

    /// Save state to a file atomically (write to temp, then rename)
    pub fn save(&mut self, path: &Path) -> Result<()> {
        self.updated_at = Utc::now();

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create state directory: {}", parent.display())
            })?;
        }

        let temp_path = path.with_extension("json.tmp");
        let content = serde_json::to_string_pretty(self).context("Failed to serialize state")?;

        fs::write(&temp_path, &content)
            .with_context(|| format!("Failed to write temp state file: {}", temp_path.display()))?;

        fs::rename(&temp_path, path)
            .with_context(|| format!("Failed to rename state file to: {}", path.display()))?;

        Ok(())
    }

    /// Load files from input JSON and merge with existing state
    /// New files are added as pending, existing files keep their status
    pub fn merge_input_file(&mut self, input_path: &Path) -> Result<()> {
        let content = fs::read_to_string(input_path)
            .with_context(|| format!("Failed to read input file: {}", input_path.display()))?;

        let input: HashMap<PathBuf, serde_json::Value> = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse input file: {}", input_path.display()))?;

        for (path, original_data) in input {
            self.files
                .entry(path)
                .or_insert_with(|| FileState::new(original_data));
        }

        Ok(())
    }

    /// Get files that need processing (pending or in-progress states)
    #[allow(dead_code)]
    pub fn get_pending_files(&self) -> Vec<PathBuf> {
        self.files
            .iter()
            .filter(|(_, state)| {
                matches!(
                    state.status,
                    FileStatus::Pending
                        | FileStatus::PromptInProgress
                        | FileStatus::AwaitingVerification
                )
            })
            .map(|(path, _)| path.clone())
            .collect()
    }

    /// Get files awaiting verification
    #[allow(dead_code)]
    pub fn get_awaiting_verification(&self) -> Vec<PathBuf> {
        self.files
            .iter()
            .filter(|(_, state)| state.status == FileStatus::AwaitingVerification)
            .map(|(path, _)| path.clone())
            .collect()
    }

    /// Update status for a file
    pub fn update_status(&mut self, path: &Path, status: FileStatus) {
        if let Some(state) = self.files.get_mut(path) {
            state.status = status;
        }
    }

    /// Set result data for a file
    pub fn set_result(&mut self, path: &Path, result: ParsedResult) {
        if let Some(state) = self.files.get_mut(path) {
            state.result_data = Some(result.value);
            if result.is_raw {
                state.result_data_raw = Some(true);
            } else {
                state.result_data_raw = None;
            }
        }
    }

    /// Increment attempts for a file
    pub fn increment_attempts(&mut self, path: &Path) {
        if let Some(state) = self.files.get_mut(path) {
            state.attempts += 1;
        }
    }

    /// Get attempts for a file
    pub fn get_attempts(&self, path: &Path) -> u32 {
        self.files.get(path).map(|s| s.attempts).unwrap_or(0)
    }

    /// Set error message for a file
    pub fn set_error(&mut self, path: &Path, error: String) {
        if let Some(state) = self.files.get_mut(path) {
            state.last_error = Some(error);
        }
    }

    /// Get original data for a file
    #[allow(dead_code)]
    pub fn get_original_data(&self, path: &Path) -> Option<serde_json::Value> {
        self.files.get(path).map(|s| s.original_data.clone())
    }

    /// Get summary counts
    pub fn get_summary(&self) -> StateSummary {
        let mut summary = StateSummary::default();
        for state in self.files.values() {
            match state.status {
                FileStatus::Pending => summary.pending += 1,
                FileStatus::PromptInProgress => summary.prompt_in_progress += 1,
                FileStatus::AwaitingVerification => summary.awaiting_verification += 1,
                FileStatus::VerifyInProgress => summary.verify_in_progress += 1,
                FileStatus::FixupInProgress => summary.fixup_in_progress += 1,
                FileStatus::Completed => summary.completed += 1,
                FileStatus::Failed => summary.failed += 1,
            }
        }
        summary.total = self.files.len();
        summary
    }
}

/// Summary of file statuses
#[derive(Debug, Default)]
pub struct StateSummary {
    pub total: usize,
    pub pending: usize,
    pub prompt_in_progress: usize,
    pub awaiting_verification: usize,
    pub verify_in_progress: usize,
    pub fixup_in_progress: usize,
    pub completed: usize,
    pub failed: usize,
}
