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
    /// Preserves existing global_allowlist_patterns from a prior run
    pub fn set_git_state(&mut self, mut git_state: GitState) {
        if git_state.global_allowlist_patterns.is_empty()
            && !self.git_state.global_allowlist_patterns.is_empty()
        {
            git_state.global_allowlist_patterns =
                std::mem::take(&mut self.git_state.global_allowlist_patterns);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::write_file;
    use serde_json::json;
    use tempfile::tempdir;

    fn config() -> Config {
        Config {
            input_file: PathBuf::from("input.json"),
            prompt: "do it".into(),
            fixup_prompt: None,
            verification_cmd: None,
            allowlist_pattern: "{file}".into(),
            concurrency: 2,
            verify_concurrency: None,
            max_files: None,
            max_retries: 3,
            git: Default::default(),
            claude_bin: "claude".into(),
        }
    }

    fn state_with(files: &[(&str, FileStatus)]) -> State {
        let mut state = State::new(config());
        for (path, status) in files {
            let mut fs = FileState::new(json!({ "path": path }));
            fs.status = status.clone();
            state.files.insert(PathBuf::from(path), fs);
        }
        state
    }

    fn sorted(mut v: Vec<PathBuf>) -> Vec<PathBuf> {
        v.sort();
        v
    }

    #[test]
    fn new_state_is_empty() {
        let state = State::new(config());
        assert!(state.files.is_empty());
        assert!(state.started_at <= state.updated_at);
        assert!(!state.git_state.enabled);
        assert_eq!(state.get_summary().total, 0);
    }

    #[test]
    fn walks_the_full_pipeline_for_one_file() {
        let p = Path::new("a.rs");
        let mut state = state_with(&[("a.rs", FileStatus::Pending)]);
        let pending_like = [
            FileStatus::Pending,
            FileStatus::PromptInProgress,
            FileStatus::AwaitingVerification,
        ];
        let steps = [
            FileStatus::PromptInProgress,
            FileStatus::AwaitingVerification,
            FileStatus::VerifyInProgress,
            FileStatus::FixupInProgress,
            FileStatus::VerifyInProgress,
            FileStatus::Completed,
        ];
        for status in steps {
            state.update_status(p, status.clone());
            assert_eq!(state.files[p].status, status);
            assert_eq!(
                state.get_pending_files().len(),
                usize::from(pending_like.contains(&status))
            );
            assert_eq!(
                state.get_awaiting_verification().len(),
                usize::from(status == FileStatus::AwaitingVerification)
            );
        }
        let summary = state.get_summary();
        assert_eq!((summary.total, summary.completed), (1, 1));
    }

    #[test]
    fn pending_and_awaiting_filters_select_expected_statuses() {
        let state = state_with(&[
            ("p", FileStatus::Pending),
            ("pi", FileStatus::PromptInProgress),
            ("av", FileStatus::AwaitingVerification),
            ("vi", FileStatus::VerifyInProgress),
            ("fi", FileStatus::FixupInProgress),
            ("c", FileStatus::Completed),
            ("f", FileStatus::Failed),
        ]);
        assert_eq!(
            sorted(state.get_pending_files()),
            vec![PathBuf::from("av"), PathBuf::from("p"), PathBuf::from("pi")]
        );
        assert_eq!(state.get_awaiting_verification(), vec![PathBuf::from("av")]);
    }

    #[test]
    fn summary_counts_every_status() {
        let state = state_with(&[
            ("p1", FileStatus::Pending),
            ("p2", FileStatus::Pending),
            ("pi", FileStatus::PromptInProgress),
            ("av", FileStatus::AwaitingVerification),
            ("vi", FileStatus::VerifyInProgress),
            ("fi", FileStatus::FixupInProgress),
            ("c1", FileStatus::Completed),
            ("c2", FileStatus::Completed),
            ("c3", FileStatus::Completed),
            ("f", FileStatus::Failed),
        ]);
        let s = state.get_summary();
        assert_eq!(s.total, 10);
        assert_eq!(s.pending, 2);
        assert_eq!(s.prompt_in_progress, 1);
        assert_eq!(s.awaiting_verification, 1);
        assert_eq!(s.verify_in_progress, 1);
        assert_eq!(s.fixup_in_progress, 1);
        assert_eq!(s.completed, 3);
        assert_eq!(s.failed, 1);
    }

    #[test]
    fn mutators_ignore_unknown_paths() {
        let mut state = state_with(&[("a", FileStatus::Pending)]);
        let ghost = Path::new("ghost");
        state.update_status(ghost, FileStatus::Completed);
        state.increment_attempts(ghost);
        state.set_error(ghost, "x".into());
        state.set_result(
            ghost,
            ParsedResult {
                value: json!(1),
                is_raw: false,
            },
        );
        assert_eq!(state.files.len(), 1);
        assert_eq!(state.get_attempts(ghost), 0);
        assert!(state.get_original_data(ghost).is_none());
        assert_eq!(state.files[Path::new("a")].status, FileStatus::Pending);
    }

    #[test]
    fn attempts_errors_and_original_data() {
        let p = Path::new("a");
        let mut state = state_with(&[("a", FileStatus::Pending)]);
        assert_eq!(state.get_attempts(p), 0);
        state.increment_attempts(p);
        state.increment_attempts(p);
        assert_eq!(state.get_attempts(p), 2);

        state.set_error(p, "first".into());
        state.set_error(p, "second".into());
        assert_eq!(state.files[p].last_error.as_deref(), Some("second"));
        assert_eq!(state.get_original_data(p), Some(json!({"path": "a"})));
    }

    #[test]
    fn set_result_tracks_raw_flag_and_clears_it() {
        let p = Path::new("a");
        let mut state = state_with(&[("a", FileStatus::Pending)]);
        state.set_result(
            p,
            ParsedResult {
                value: json!("not json"),
                is_raw: true,
            },
        );
        assert_eq!(state.files[p].result_data, Some(json!("not json")));
        assert_eq!(state.files[p].result_data_raw, Some(true));

        state.set_result(
            p,
            ParsedResult {
                value: json!({"ok": true}),
                is_raw: false,
            },
        );
        assert_eq!(state.files[p].result_data, Some(json!({"ok": true})));
        assert_eq!(state.files[p].result_data_raw, None);
    }

    #[test]
    fn set_git_state_preserves_prior_allowlist_only_when_new_is_empty() {
        let mut state = State::new(config());
        state.set_git_state(GitState {
            enabled: true,
            global_allowlist_patterns: vec!["src/a.rs".into()],
            ..Default::default()
        });

        // New capture without patterns keeps the prior ones but takes other fields.
        state.set_git_state(GitState {
            enabled: true,
            task_branch: Some("b".into()),
            ..Default::default()
        });
        assert_eq!(state.git_state.global_allowlist_patterns, vec!["src/a.rs"]);
        assert_eq!(state.git_state.task_branch.as_deref(), Some("b"));

        // New non-empty patterns replace the prior ones.
        state.set_git_state(GitState {
            global_allowlist_patterns: vec!["src/b.rs".into()],
            ..Default::default()
        });
        assert_eq!(state.git_state.global_allowlist_patterns, vec!["src/b.rs"]);
        assert!(!state.git_state.enabled);
    }

    #[test]
    fn save_and_load_round_trip() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("deep/dir/state.json");
        let mut state = state_with(&[
            ("a", FileStatus::Completed),
            ("b", FileStatus::FixupInProgress),
        ]);
        state.set_result(
            Path::new("a"),
            ParsedResult {
                value: json!("raw"),
                is_raw: true,
            },
        );
        state.increment_attempts(Path::new("b"));
        state.set_error(Path::new("b"), "boom".into());
        state.git_state.global_allowlist_patterns = vec!["x".into()];
        let before = state.updated_at;

        state.save(&path).unwrap();
        assert!(state.updated_at >= before);
        assert!(!path.with_extension("json.tmp").exists());

        let loaded = State::load(&path).unwrap();
        assert_eq!(loaded.files.len(), 2);
        let a = &loaded.files[Path::new("a")];
        assert_eq!(a.status, FileStatus::Completed);
        assert_eq!(a.result_data, Some(json!("raw")));
        assert_eq!(a.result_data_raw, Some(true));
        let b = &loaded.files[Path::new("b")];
        assert_eq!(b.status, FileStatus::FixupInProgress);
        assert_eq!(b.attempts, 1);
        assert_eq!(b.last_error.as_deref(), Some("boom"));
        assert_eq!(loaded.started_at, state.started_at);
        assert_eq!(loaded.updated_at, state.updated_at);
        assert_eq!(loaded.git_state.global_allowlist_patterns, vec!["x"]);
        assert_eq!(loaded.config.prompt, "do it");
        assert_eq!(loaded.config.max_retries, 3);
    }

    #[test]
    fn load_older_json_without_git_state_or_optional_fields() {
        let tmp = tempdir().unwrap();
        let path = write_file(
            tmp.path(),
            "state.json",
            r#"{
                "config": {
                    "input_file": "in.json",
                    "prompt": "p",
                    "allowlist_pattern": "{file}",
                    "concurrency": 1,
                    "max_retries": 2
                },
                "files": {
                    "a.rs": {"status": "awaiting_verification", "original_data": {"n": 1}}
                },
                "started_at": "2025-01-01T00:00:00Z",
                "updated_at": "2025-01-02T00:00:00Z"
            }"#,
        );
        let state = State::load(&path).unwrap();
        assert!(!state.git_state.enabled);
        assert!(state.git_state.global_allowlist_patterns.is_empty());
        assert_eq!(state.config.claude_bin, "claude");
        assert!(!state.config.git.enabled);
        assert_eq!(
            state.get_awaiting_verification(),
            vec![PathBuf::from("a.rs")]
        );
        assert_eq!(state.get_attempts(Path::new("a.rs")), 0);
    }

    #[test]
    fn load_reports_missing_and_malformed_files() {
        let tmp = tempdir().unwrap();
        let missing = State::load(&tmp.path().join("nope.json")).unwrap_err();
        assert!(format!("{missing:#}").contains("Failed to read state file"));

        let bad = write_file(tmp.path(), "bad.json", "{\"config\": 1}");
        let err = State::load(&bad).unwrap_err();
        assert!(format!("{err:#}").contains("Failed to parse state file"));
    }

    #[test]
    fn save_reports_filesystem_failures() {
        let tmp = tempdir().unwrap();
        let mut state = State::new(config());

        let blocker = write_file(tmp.path(), "blocker", "");
        let err = state.save(&blocker.join("state.json")).unwrap_err();
        assert!(format!("{err:#}").contains("Failed to create state directory"));

        fs::create_dir_all(tmp.path().join("w/state.json.tmp")).unwrap();
        let err = state.save(&tmp.path().join("w/state.json")).unwrap_err();
        assert!(format!("{err:#}").contains("Failed to write temp state file"));

        fs::create_dir_all(tmp.path().join("r/state.json/inner")).unwrap();
        let err = state.save(&tmp.path().join("r/state.json")).unwrap_err();
        assert!(format!("{err:#}").contains("Failed to rename state file"));
    }

    #[test]
    fn merge_input_adds_new_files_and_keeps_existing_progress() {
        let tmp = tempdir().unwrap();
        let mut state = state_with(&[("a", FileStatus::Completed)]);
        state.increment_attempts(Path::new("a"));
        let input = write_file(
            tmp.path(),
            "input.json",
            r#"{"a": {"new": "data"}, "b": {"n": 2}, "c": null}"#,
        );

        state.merge_input_file(&input).unwrap();
        assert_eq!(state.files.len(), 3);
        let a = &state.files[Path::new("a")];
        assert_eq!(a.status, FileStatus::Completed);
        assert_eq!(a.attempts, 1);
        assert_eq!(a.original_data, json!({"path": "a"}));
        let b = &state.files[Path::new("b")];
        assert_eq!(b.status, FileStatus::Pending);
        assert_eq!(b.original_data, json!({"n": 2}));
        assert_eq!(state.get_original_data(Path::new("c")), Some(json!(null)));

        // Merging again is idempotent.
        state.merge_input_file(&input).unwrap();
        assert_eq!(state.files.len(), 3);
        assert_eq!(state.get_summary().pending, 2);
    }

    #[test]
    fn merge_input_rejects_bad_input_without_mutating() {
        let tmp = tempdir().unwrap();
        let mut state = state_with(&[("a", FileStatus::Pending)]);

        let err = state
            .merge_input_file(&tmp.path().join("missing.json"))
            .unwrap_err();
        assert!(format!("{err:#}").contains("Failed to read input file"));

        for (name, body) in [("bad.json", "{oops"), ("array.json", r#"["a", "b"]"#)] {
            let path = write_file(tmp.path(), name, body);
            let err = state.merge_input_file(&path).unwrap_err();
            assert!(format!("{err:#}").contains("Failed to parse input file"));
        }
        assert_eq!(state.files.len(), 1);
    }
}
