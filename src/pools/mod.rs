pub mod prompt;
pub mod verify;

use crate::config::Config;
use crate::git::commit_file_changes;
use crate::memory::MemoryHandle;
use crate::state::State;
use crate::types::FileTask;
use crate::usage::UsageHandle;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

pub use prompt::spawn_prompt_pool;
pub use verify::spawn_verify_pool;

/// Shared context passed to all pool workers
#[derive(Clone)]
pub struct WorkerContext {
    pub state: Arc<Mutex<State>>,
    pub state_path: PathBuf,
    pub config: Arc<Config>,
    pub working_dir: PathBuf,
    pub tasks_dir: PathBuf,
    pub memory: MemoryHandle,
    pub usage: UsageHandle,
    /// Serialises git index access so parallel workers don't race on staging and commits
    pub git_lock: Arc<Mutex<()>>,
}

/// Commit the task's changes if auto-commit is enabled.
///
/// Leaves out files that were dirty before the run started and anything the
/// runner itself wrote under the tasks directory (state, failure logs).
pub async fn auto_commit(ctx: &WorkerContext, worker_id: usize, task: &FileTask) {
    if !ctx.config.git.auto_commit {
        return;
    }

    let git_state = ctx.state.lock().await.git_state.clone();
    let tasks_dir = absolute(&ctx.tasks_dir);
    let working_dir = absolute(&ctx.working_dir);
    let skip = |path: &Path| {
        git_state.was_pre_existing_dirty(path) || working_dir.join(path).starts_with(&tasks_dir)
    };

    let file_display = task.path.display().to_string();
    let description = ctx.config.git.commit_message_template.as_deref();
    let _guard = ctx.git_lock.lock().await;
    match commit_file_changes(&ctx.working_dir, &task.path, description, skip).await {
        Ok(Some(hash)) => {
            info!(worker = worker_id, file = %file_display, commit = %hash, "Auto-committed changes");
        }
        Ok(None) => {
            debug!(worker = worker_id, file = %file_display, "No changes to commit");
        }
        Err(e) => {
            warn!(
                worker = worker_id,
                file = %file_display,
                error = %e,
                "Failed to auto-commit (continuing anyway)"
            );
        }
    }
}

/// Resolve a path against the process cwd and canonicalise it where possible
fn absolute(path: &Path) -> PathBuf {
    let joined = std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf());
    joined.canonicalize().unwrap_or(joined)
}
