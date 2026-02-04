use crate::config::Config;
use crate::pools::{spawn_prompt_pool, spawn_verify_pool};
use crate::process::expand_pattern;
use crate::state::State;
use crate::types::{FileStatus, FileTask};
use anyhow::Result;
use async_channel::{bounded, Sender};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info};

/// Main orchestration function
pub async fn run(
    config: Config,
    state: State,
    state_path: PathBuf,
    shutdown_rx: tokio::sync::broadcast::Receiver<()>,
) -> Result<()> {
    let config = Arc::new(config);
    let state = Arc::new(Mutex::new(state));

    // Get current working directory for ACP server
    let working_dir = std::env::current_dir()?;

    // Create channels
    let (prompt_tx, prompt_rx) = bounded::<FileTask>(100);
    let (verify_tx, verify_rx) = bounded::<FileTask>(100);

    // Queue pending files and build global allowlist for parallel worker support
    let files_to_process = queue_pending_files(
        &state,
        &prompt_tx,
        &verify_tx,
        config.max_files,
        &config.allowlist_pattern,
        &state_path,
    )
    .await?;

    if files_to_process == 0 {
        info!("No files to process");
        return Ok(());
    }

    info!(
        files = files_to_process,
        concurrency = config.concurrency,
        "Starting processing"
    );

    // Spawn worker pools
    let prompt_handles = spawn_prompt_pool(
        config.concurrency,
        prompt_rx.clone(),
        verify_tx.clone(),
        Arc::clone(&state),
        state_path.clone(),
        Arc::clone(&config),
        working_dir.clone(),
    );

    let verify_handles = spawn_verify_pool(
        config.concurrency,
        verify_rx.clone(),
        Arc::clone(&state),
        state_path.clone(),
        Arc::clone(&config),
        working_dir.clone(),
    );

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

/// Queue files that need processing and build global allowlist
async fn queue_pending_files(
    state: &Arc<Mutex<State>>,
    prompt_tx: &Sender<FileTask>,
    verify_tx: &Sender<FileTask>,
    max_files: Option<usize>,
    allowlist_pattern: &str,
    state_path: &Path,
) -> Result<usize> {
    let mut state = state.lock().await;
    let mut queued = 0;

    // First pass: collect all files that will be processed and build global allowlist
    let files_to_queue: Vec<_> = state
        .files
        .iter()
        .filter(|(_, file_state)| {
            !matches!(
                file_state.status,
                FileStatus::Completed | FileStatus::Failed
            )
        })
        .map(|(path, file_state)| (path.clone(), file_state.clone()))
        .collect();

    // Build global allowlist from all files being processed
    if state.git_state.enabled {
        for (path, _) in &files_to_queue {
            let pattern = expand_pattern(allowlist_pattern, path);
            state.git_state.add_allowlist_pattern(pattern.clone());
            debug!(pattern = %pattern, "Added to global allowlist");
        }

        // Save state with updated allowlist
        if let Err(e) = state.save(state_path) {
            tracing::error!(error = %e, "Failed to save state with global allowlist");
        } else {
            info!(
                patterns = state.git_state.global_allowlist_patterns.len(),
                "Built global allowlist for parallel workers"
            );
        }
    }

    // Second pass: queue the files
    for (path, file_state) in files_to_queue {
        // Check max files limit
        if let Some(max) = max_files {
            if queued >= max {
                break;
            }
        }

        match file_state.status {
            FileStatus::Pending | FileStatus::PromptInProgress => {
                // Needs to go through prompt
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
                // Already prompted, needs verification
                let task = FileTask {
                    path: path.clone(),
                    original_data: file_state.original_data.clone(),
                };
                verify_tx.send(task).await?;
                queued += 1;
            }
            FileStatus::Completed | FileStatus::Failed => {
                // Already done, skip
            }
        }
    }

    Ok(queued)
}
