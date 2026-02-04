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
            let list: TaskList = serde_json::from_str(&content).with_context(|| {
                format!("Failed to parse task list: {}", task_list_path.display())
            })?;
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

        let state_file = format!("state_{}.json", self.next_id - 1);

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

    /// Get state file path for a task
    #[allow(dead_code)]
    pub fn get_state_path(&self, tasks_dir: &Path, task_id: &str) -> Option<PathBuf> {
        self.tasks
            .get(task_id)
            .map(|entry| tasks_dir.join(&entry.state_file))
    }
}
