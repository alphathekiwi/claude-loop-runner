use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Entry in the task list
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEntry {
    /// Path to the state file for this task
    pub state_file: String,
    /// Working directory where the task runs
    pub working_dir: PathBuf,
    /// Human-readable description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether this task is complete
    #[serde(default)]
    pub completed: bool,
}

/// Task list tracking multiple independent task runs
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskList {
    /// Map of task ID to task entry
    pub tasks: HashMap<String, TaskEntry>,
    /// Counter for generating unique task IDs
    #[serde(default)]
    next_id: u32,
}

impl TaskList {
    /// Load task list from file, or create empty if doesn't exist
    pub fn load_or_create(tasks_dir: &Path) -> Result<Self> {
        let task_list_path = tasks_dir.join("task_list.json");

        if task_list_path.exists() {
            let content = fs::read_to_string(&task_list_path).with_context(|| {
                format!("Failed to read task list: {}", task_list_path.display())
            })?;
            let mut list: TaskList = serde_json::from_str(&content).with_context(|| {
                format!("Failed to parse task list: {}", task_list_path.display())
            })?;
            // Older or hand-edited lists may lack next_id; never hand out an ID already in use
            let next_free = list
                .tasks
                .keys()
                .filter_map(|id| id.strip_prefix("task_")?.parse::<u32>().ok())
                .map(|n| n + 1)
                .max()
                .unwrap_or(0);
            list.next_id = list.next_id.max(next_free);
            Ok(list)
        } else {
            // Create tasks directory if it doesn't exist
            fs::create_dir_all(tasks_dir).with_context(|| {
                format!("Failed to create tasks directory: {}", tasks_dir.display())
            })?;

            let list = TaskList::default();
            list.save(tasks_dir)?;
            Ok(list)
        }
    }

    /// Save task list to file
    pub fn save(&self, tasks_dir: &Path) -> Result<()> {
        fs::create_dir_all(tasks_dir).with_context(|| {
            format!("Failed to create tasks directory: {}", tasks_dir.display())
        })?;

        let task_list_path = tasks_dir.join("task_list.json");
        let temp_path = task_list_path.with_extension("json.tmp");

        let content =
            serde_json::to_string_pretty(self).context("Failed to serialize task list")?;

        fs::write(&temp_path, &content)
            .with_context(|| format!("Failed to write task list: {}", temp_path.display()))?;

        fs::rename(&temp_path, &task_list_path)
            .with_context(|| format!("Failed to rename task list: {}", task_list_path.display()))?;

        Ok(())
    }

    /// Create a new task and return its ID
    pub fn create_task(&mut self, working_dir: PathBuf, description: Option<String>) -> String {
        let task_id = format!("task_{}", self.next_id);
        self.next_id += 1;

        let state_file = format!("tasks/state_{}.json", self.next_id - 1);

        self.tasks.insert(
            task_id.clone(),
            TaskEntry {
                state_file,
                working_dir,
                description,
                completed: false,
            },
        );

        task_id
    }

    /// Get a task by ID
    pub fn get_task(&self, task_id: &str) -> Option<&TaskEntry> {
        self.tasks.get(task_id)
    }

    /// Mark a task as completed
    pub fn mark_completed(&mut self, task_id: &str) {
        if let Some(entry) = self.tasks.get_mut(task_id) {
            entry.completed = true;
        }
    }

    /// Get all incomplete tasks
    pub fn get_incomplete_tasks(&self) -> Vec<(&String, &TaskEntry)> {
        self.tasks
            .iter()
            .filter(|(_, entry)| !entry.completed)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn create_task_assigns_sequential_ids_and_state_files() {
        let mut list = TaskList::default();
        let a = list.create_task(PathBuf::from("/a"), Some("first".into()));
        let b = list.create_task(PathBuf::from("/b"), None);
        assert_eq!(a, "task_0");
        assert_eq!(b, "task_1");

        let entry_a = list.get_task(&a).unwrap();
        assert_eq!(entry_a.state_file, "tasks/state_0.json");
        assert_eq!(entry_a.working_dir, PathBuf::from("/a"));
        assert_eq!(entry_a.description.as_deref(), Some("first"));
        assert!(!entry_a.completed);

        let entry_b = list.get_task(&b).unwrap();
        assert_eq!(entry_b.state_file, "tasks/state_1.json");
        assert!(entry_b.description.is_none());
        assert!(list.get_task("task_2").is_none());
    }

    #[test]
    fn mark_completed_removes_task_from_incomplete_list() {
        let mut list = TaskList::default();
        let a = list.create_task(PathBuf::from("/a"), None);
        let b = list.create_task(PathBuf::from("/b"), None);
        assert_eq!(list.get_incomplete_tasks().len(), 2);

        list.mark_completed(&a);
        assert!(list.get_task(&a).unwrap().completed);
        let incomplete = list.get_incomplete_tasks();
        assert_eq!(incomplete.len(), 1);
        assert_eq!(incomplete[0].0, &b);

        // Unknown IDs are ignored rather than inserted.
        list.mark_completed("task_99");
        assert_eq!(list.tasks.len(), 2);
    }

    #[test]
    fn load_or_create_creates_dir_and_empty_file() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("nested/tasks");
        let list = TaskList::load_or_create(&dir).unwrap();
        assert!(list.tasks.is_empty());
        let file = dir.join("task_list.json");
        assert!(file.is_file());
        assert!(!dir.join("task_list.json.tmp").exists());
        let on_disk: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
        assert_eq!(on_disk, serde_json::json!({"tasks": {}, "next_id": 0}));
    }

    #[test]
    fn save_then_load_round_trips_and_keeps_counter() {
        let tmp = tempdir().unwrap();
        let mut list = TaskList::load_or_create(tmp.path()).unwrap();
        let a = list.create_task(PathBuf::from("/a"), Some("desc".into()));
        list.create_task(PathBuf::from("/b"), None);
        list.mark_completed(&a);
        list.save(tmp.path()).unwrap();

        let mut loaded = TaskList::load_or_create(tmp.path()).unwrap();
        assert_eq!(loaded.tasks.len(), 2);
        let entry = loaded.get_task(&a).unwrap();
        assert!(entry.completed);
        assert_eq!(entry.description.as_deref(), Some("desc"));
        assert_eq!(loaded.get_incomplete_tasks().len(), 1);
        // Counter persisted, so the next ID does not collide.
        assert_eq!(loaded.create_task(PathBuf::from("/c"), None), "task_2");
    }

    #[test]
    fn saved_json_omits_missing_description() {
        let tmp = tempdir().unwrap();
        let mut list = TaskList::default();
        list.create_task(PathBuf::from("/a"), None);
        list.save(tmp.path()).unwrap();
        let raw = fs::read_to_string(tmp.path().join("task_list.json")).unwrap();
        assert!(!raw.contains("description"));
    }

    #[test]
    fn load_accepts_entries_without_completed_field() {
        let tmp = tempdir().unwrap();
        fs::write(
            tmp.path().join("task_list.json"),
            r#"{"tasks": {"task_0": {"state_file": "tasks/state_0.json", "working_dir": "/w"}}, "next_id": 1}"#,
        )
        .unwrap();
        let list = TaskList::load_or_create(tmp.path()).unwrap();
        let entry = list.get_task("task_0").unwrap();
        assert!(!entry.completed);
        assert!(entry.description.is_none());
        assert_eq!(list.get_incomplete_tasks().len(), 1);
    }

    #[test]
    fn create_task_after_loading_list_without_next_id_does_not_overwrite() {
        let tmp = tempdir().unwrap();
        fs::write(
            tmp.path().join("task_list.json"),
            r#"{"tasks": {"task_0": {"state_file": "tasks/state_0.json", "working_dir": "/old"}}}"#,
        )
        .unwrap();
        let mut list = TaskList::load_or_create(tmp.path()).unwrap();
        let id = list.create_task(PathBuf::from("/new"), None);
        assert_ne!(id, "task_0");
        assert_eq!(
            list.get_task("task_0").unwrap().working_dir,
            PathBuf::from("/old")
        );
    }

    #[test]
    fn load_or_create_reports_bad_json() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("task_list.json"), "{not json").unwrap();
        let err = TaskList::load_or_create(tmp.path()).unwrap_err();
        assert!(format!("{err:#}").contains("Failed to parse task list"));
    }

    #[test]
    fn load_or_create_reports_unreadable_file() {
        let tmp = tempdir().unwrap();
        fs::create_dir(tmp.path().join("task_list.json")).unwrap();
        let err = TaskList::load_or_create(tmp.path()).unwrap_err();
        assert!(format!("{err:#}").contains("Failed to read task list"));
    }

    #[test]
    fn load_or_create_fails_when_dir_is_a_file() {
        let tmp = tempdir().unwrap();
        let blocker = tmp.path().join("blocker");
        fs::write(&blocker, "").unwrap();
        let err = TaskList::load_or_create(&blocker.join("tasks")).unwrap_err();
        assert!(format!("{err:#}").contains("Failed to create tasks directory"));
    }

    #[test]
    fn save_reports_write_and_rename_failures() {
        let tmp = tempdir().unwrap();
        let list = TaskList::default();

        // Temp path occupied by a directory: write fails.
        let write_dir = tmp.path().join("w");
        fs::create_dir_all(write_dir.join("task_list.json.tmp")).unwrap();
        let err = list.save(&write_dir).unwrap_err();
        assert!(format!("{err:#}").contains("Failed to write task list"));

        // Target occupied by a non-empty directory: rename fails.
        let rename_dir = tmp.path().join("r");
        fs::create_dir_all(rename_dir.join("task_list.json/inner")).unwrap();
        let err = list.save(&rename_dir).unwrap_err();
        assert!(format!("{err:#}").contains("Failed to rename task list"));
    }
}
