use crate::config::Config;
use crate::memory::MemoryMonitor;
use crate::pools::{spawn_prompt_pool, spawn_verify_pool};
use crate::process::expand_pattern;
use crate::state::State;
use crate::types::{FileStatus, FileTask};
use anyhow::Result;
use async_channel::{bounded, Sender};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, info};

/// Main orchestration function
pub async fn run(
    config: Config,
    state: State,
    state_path: PathBuf,
    tasks_dir: PathBuf,
    shutdown_rx: tokio::sync::broadcast::Receiver<()>,
) -> Result<()> {
    let config = Arc::new(config);
    let state = Arc::new(Mutex::new(state));

    // Get current working directory for ACP server
    let working_dir = std::env::current_dir()?;

    // Create memory monitor with hysteresis (85% high, 70% low)
    let memory_monitor = MemoryMonitor::new();
    let memory_handle = memory_monitor.handle();
    let _monitor_handle = memory_monitor.spawn_monitor(85.0, 70.0, Duration::from_secs(2));

    // Build global allowlist (no channel needed, just state mutation)
    let file_count = build_allowlist(
        &state,
        config.max_files,
        &config.allowlist_pattern,
        &state_path,
    )
    .await?;

    if file_count == 0 {
        info!("No files to process");
        return Ok(());
    }

    let verify_concurrency = config.verify_concurrency.unwrap_or(config.concurrency);

    info!(
        files = file_count,
        prompt_concurrency = config.concurrency,
        verify_concurrency = verify_concurrency,
        "Starting processing"
    );

    // Create channels sized to fit all files (avoids deadlock)
    let (prompt_tx, prompt_rx) = bounded::<FileTask>(file_count);
    let (verify_tx, verify_rx) = bounded::<FileTask>(file_count);

    // Spawn worker pools BEFORE queuing so consumers are ready
    let prompt_handles = spawn_prompt_pool(
        config.concurrency,
        prompt_rx.clone(),
        verify_tx.clone(),
        Arc::clone(&state),
        state_path.clone(),
        Arc::clone(&config),
        working_dir.clone(),
        memory_handle.clone(),
    );

    let verify_handles = spawn_verify_pool(
        verify_concurrency,
        verify_rx.clone(),
        Arc::clone(&state),
        state_path.clone(),
        Arc::clone(&config),
        working_dir.clone(),
        tasks_dir,
        memory_handle,
    );

    // Now queue files — workers are already consuming
    queue_files(
        &state,
        &prompt_tx,
        &verify_tx,
        config.max_files,
    )
    .await?;

    // Close senders so workers know when to stop
    drop(prompt_tx);
    drop(verify_tx);

    // Wait for shutdown signal or completion
    let mut shutdown_rx = shutdown_rx;
    tokio::select! {
        _ = async {
            for handle in prompt_handles {
                let _ = handle.await;
            }
            for handle in verify_handles {
                let _ = handle.await;
            }
        } => {
            info!("All workers completed");
        }
        _ = shutdown_rx.recv() => {
            info!("Shutdown signal received, saving state...");
            // State is automatically saved by workers, just need to wait a moment
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }
    }

    // Print summary
    let state = state.lock().await;
    let summary = state.get_summary();
    info!(
        total = summary.total,
        completed = summary.completed,
        failed = summary.failed,
        pending = summary.pending,
        "Processing complete"
    );

    Ok(())
}

/// Build the global allowlist and return the count of files to process.
/// Does NOT queue files to channels — just updates state.
async fn build_allowlist(
    state: &Arc<Mutex<State>>,
    max_files: Option<usize>,
    allowlist_pattern: &str,
    state_path: &Path,
) -> Result<usize> {
    let mut state = state.lock().await;

    // Collect files that need processing
    let mut files_to_process: Vec<_> = state
        .files
        .iter()
        .filter(|(_, file_state)| {
            !matches!(
                file_state.status,
                FileStatus::Completed | FileStatus::Failed
            )
        })
        .map(|(path, _)| path.clone())
        .collect();

    // Apply max files limit
    if let Some(max) = max_files {
        files_to_process.truncate(max);
    }

    // Build global allowlist (skip if already built)
    if state.git_state.enabled {
        if state.git_state.global_allowlist_patterns.is_empty() {
            let working_dir = std::env::current_dir().unwrap_or_default();
            for path in &files_to_process {
                let pattern = expand_pattern(allowlist_pattern, path);
                state.git_state.add_allowlist_pattern(pattern.clone());
                debug!(pattern = %pattern, "Added to global allowlist");

                // Discover related test/snapshot files and add their patterns
                for related in crate::process::find_related_files(path, &working_dir) {
                    let related_pattern = expand_pattern(allowlist_pattern, &related);
                    state.git_state.add_allowlist_pattern(related_pattern);
                }
            }

            if let Err(e) = state.save(state_path) {
                tracing::error!(error = %e, "Failed to save state with global allowlist");
            } else {
                info!(
                    patterns = state.git_state.global_allowlist_patterns.len(),
                    "Built global allowlist for parallel workers (with related file discovery)"
                );
            }
        } else {
            info!(
                patterns = state.git_state.global_allowlist_patterns.len(),
                "Using existing global allowlist from saved state"
            );
        }
    }

    Ok(files_to_process.len())
}

/// Queue files to the appropriate worker channels
async fn queue_files(
    state: &Arc<Mutex<State>>,
    prompt_tx: &Sender<FileTask>,
    verify_tx: &Sender<FileTask>,
    max_files: Option<usize>,
) -> Result<usize> {
    let state = state.lock().await;
    let mut queued = 0;

    for (path, file_state) in &state.files {
        if let Some(max) = max_files {
            if queued >= max {
                break;
            }
        }

        match file_state.status {
            FileStatus::Pending | FileStatus::PromptInProgress => {
                let task = FileTask {
                    path: path.clone(),
                    original_data: file_state.original_data.clone(),
                };
                prompt_tx.send(task).await?;
                queued += 1;
            }
            FileStatus::AwaitingVerification
            | FileStatus::VerifyInProgress
            | FileStatus::FixupInProgress => {
                let task = FileTask {
                    path: path.clone(),
                    original_data: file_state.original_data.clone(),
                };
                verify_tx.send(task).await?;
                queued += 1;
            }
            FileStatus::Completed | FileStatus::Failed => {}
        }
    }

    Ok(queued)
}
