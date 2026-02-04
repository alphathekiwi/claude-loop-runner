use crate::claude::{build_prompt, run_claude};
use crate::config::Config;
use crate::git::check_git_changes_filtered;
use crate::process::{expand_pattern, parse_result};
use crate::state::State;
use crate::types::{FileStatus, FileTask};
use async_channel::{Receiver, Sender};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

/// Spawn a pool of prompt workers
pub fn spawn_prompt_pool(
    concurrency: usize,
    rx: Receiver<FileTask>,
    verify_tx: Sender<FileTask>,
    state: Arc<Mutex<State>>,
    state_path: PathBuf,
    config: Arc<Config>,
    working_dir: PathBuf,
) -> Vec<JoinHandle<()>> {
    (0..concurrency)
        .map(|worker_id| {
            let rx = rx.clone();
            let verify_tx = verify_tx.clone();
            let state = Arc::clone(&state);
            let state_path = state_path.clone();
            let config = Arc::clone(&config);
            let working_dir = working_dir.clone();

            tokio::spawn(async move {
                prompt_worker(
                    worker_id,
                    rx,
                    verify_tx,
                    state,
                    state_path,
                    config,
                    working_dir,
                )
                .await;
            })
        })
        .collect()
}

async fn prompt_worker(
    worker_id: usize,
    rx: Receiver<FileTask>,
    verify_tx: Sender<FileTask>,
    state: Arc<Mutex<State>>,
    state_path: PathBuf,
    config: Arc<Config>,
    working_dir: PathBuf,
) {
    while let Ok(task) = rx.recv().await {
        let file_display = task.path.display().to_string();
        info!(worker = worker_id, file = %file_display, "Starting prompt task");

        // Update status to in progress
        {
            let mut state = state.lock().await;
            state.update_status(&task.path, FileStatus::PromptInProgress);
            if let Err(e) = state.save(&state_path) {
                error!(error = %e, "Failed to save state");
            }
        }

        // Build prompt
        let prompt = build_prompt(
            &config.prompt,
            &task.path,
            &task.original_data,
            &config.allowlist_pattern,
        );
        let allowlist = expand_pattern(&config.allowlist_pattern, &task.path);

        // Run Claude
        match run_claude(&prompt, &working_dir).await {
            Ok(output) => {
                // Check for unauthorized file changes (filtering out pre-existing dirty files)
                let git_state = {
                    let state = state.lock().await;
                    state.git_state.clone()
                };

                if git_state.enabled {
                    if let Ok((_, unauthorized)) =
                        check_git_changes_filtered(&allowlist, &working_dir, &git_state).await
                    {
                        if !unauthorized.is_empty() {
                            let unauthorized_list: Vec<_> = unauthorized
                                .iter()
                                .map(|p| p.display().to_string())
                                .collect();
                            warn!(
                                worker = worker_id,
                                file = %file_display,
                                unauthorized = ?unauthorized_list,
                                "Detected unauthorized file changes (excluding pre-existing dirty files)"
                            );
                            // Note: We log but don't fail - the verification step will catch issues
                        }
                    }
                }

                // Parse result from output
                let result = parse_result(&output.stdout);

                // Update state with result
                {
                    let mut state = state.lock().await;
                    state.set_result(&task.path, result);

                    if config.verification_cmd.is_some() {
                        // Queue for verification
                        state.update_status(&task.path, FileStatus::AwaitingVerification);
                    } else {
                        // No verification, mark as complete
                        state.update_status(&task.path, FileStatus::Completed);
                    }

                    if let Err(e) = state.save(&state_path) {
                        error!(error = %e, "Failed to save state");
                    }
                }

                if config.verification_cmd.is_some() {
                    // Send to verification queue
                    if let Err(e) = verify_tx.send(task.clone()).await {
                        error!(error = %e, file = %file_display, "Failed to queue for verification");
                    }
                }

                info!(worker = worker_id, file = %file_display, "Prompt task complete");
            }
            Err(e) => {
                error!(worker = worker_id, file = %file_display, error = %e, "Prompt task failed");

                // Mark as failed
                let mut state = state.lock().await;
                state.update_status(&task.path, FileStatus::Failed);
                state.set_error(&task.path, e.to_string());
                if let Err(e) = state.save(&state_path) {
                    error!(error = %e, "Failed to save state");
                }
            }
        }
    }

    info!(worker = worker_id, "Prompt worker shutting down");
}
