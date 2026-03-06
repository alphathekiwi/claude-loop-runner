pub mod prompt;
pub mod verify;

use crate::config::Config;
use crate::memory::MemoryHandle;
use crate::state::State;
use crate::usage::UsageHandle;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

pub use prompt::spawn_prompt_pool;
pub use verify::spawn_verify_pool;

/// Shared context passed to all pool workers
#[derive(Clone)]
pub struct WorkerContext {
    pub state: Arc<Mutex<State>>,
    pub state_path: PathBuf,
    pub config: Arc<Config>,
    pub working_dir: PathBuf,
    pub memory: MemoryHandle,
    pub usage: UsageHandle,
}
