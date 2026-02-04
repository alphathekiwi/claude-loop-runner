use crate::claude::{build_fixup_prompt, run_claude};
use crate::config::Config;
use crate::git::commit_file_changes;
use crate::process::{expand_pattern, parse_result, run_command};
use crate::state::State;
use crate::types::{FileStatus, FileTask};
use async_channel::Receiver;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

/// Spawn a pool of verification workers
pub fn spawn_verify_pool(
    concurrency: usize,
    rx: Receiver<FileTask>,
    state: Arc<Mutex<State>>,
    state_path: PathBuf,
    config: Arc<Config>,
    working_dir: PathBuf,
) -> Vec<JoinHandle<()>> {
    (0..concurrency)
        .map(|worker_id| {
            let rx = rx.clone();
            let state = Arc::clone(&state);
            let state_path = state_path.clone();
            let config = Arc::clone(&config);
            let working_dir = working_dir.clone();

            tokio::spawn(async move {
                verify_worker(worker_id, rx, state, state_path, config, working_dir).await;
            })
        })
        .collect()
}

async fn verify_worker(
    worker_id: usize,
    rx: Receiver<FileTask>,
    state: Arc<Mutex<State>>,
    state_path: PathBuf,
    config: Arc<Config>,
    working_dir: PathBuf,
) {
    let verification_cmd = match &config.verification_cmd {
        Some(cmd) => cmd.clone(),
        None => {
            // No verification configured, worker exits immediately
            return;
        }
    };

    while let Ok(task) = rx.recv().await {
        let file_display = task.path.display().to_string();
        let mut attempts = {
            let state = state.lock().await;
            state.get_attempts(&task.path)
        };

        loop {
            info!(
                worker = worker_id,
                file = %file_display,
                attempt = attempts + 1,
                "Starting verification"
            );

            // Update status
            {
                let mut state = state.lock().await;
                state.update_status(&task.path, FileStatus::VerifyInProgress);
                if let Err(e) = state.save(&state_path) {
                    error!(error = %e, "Failed to save state");
                }
            }

            // Run verification command
            let cmd = expand_pattern(&verification_cmd, &task.path);
            let result = match run_command(&cmd).await {
                Ok(r) => r,
                Err(e) => {
                    error!(
                        worker = worker_id,
                        file = %file_display,
                        error = %e,
                        "Verification command failed to execute"
                    );
                    // Mark as failed
                    let mut state = state.lock().await;
                    state.update_status(&task.path, FileStatus::Failed);
                    state.set_error(&task.path, e.to_string());
                    if let Err(e) = state.save(&state_path) {
                        error!(error = %e, "Failed to save state");
                    }
                    break;
                }
            };

            if result.exit_code == 0 {
                // Verification passed!
                info!(
                    worker = worker_id,
                    file = %file_display,
                    "Verification PASSED"
                );

                // Auto-commit if enabled
                if config.git.auto_commit {
                    let description = config.git.commit_message_template.as_deref();
                    match commit_file_changes(&working_dir, &task.path, description).await {
                        Ok(Some(hash)) => {
                            info!(
                                worker = worker_id,
                                file = %file_display,
                                commit = %hash,
                                "Auto-committed changes"
                            );
                        }
                        Ok(None) => {
                            debug!(
                                worker = worker_id,
                                file = %file_display,
                                "No changes to commit"
                            );
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

                let mut state = state.lock().await;
                state.update_status(&task.path, FileStatus::Completed);
                if let Err(e) = state.save(&state_path) {
                    error!(error = %e, "Failed to save state");
                }
                break;
            }

            // Verification failed
            attempts += 1;
            {
                let mut state = state.lock().await;
                state.increment_attempts(&task.path);
            }

            if attempts >= config.max_retries {
                // Max retries reached
                warn!(
                    worker = worker_id,
                    file = %file_display,
                    attempts = attempts,
                    "Verification FAILED after max retries"
                );

                let mut state = state.lock().await;
                state.update_status(&task.path, FileStatus::Failed);
                let error_msg = if result.stderr.is_empty() {
                    result.stdout.clone()
                } else {
                    result.stderr.clone()
                };
                state.set_error(&task.path, error_msg);
                if let Err(e) = state.save(&state_path) {
                    error!(error = %e, "Failed to save state");
                }
                break;
            }

            // Run fixup
            warn!(
                worker = worker_id,
                file = %file_display,
                attempt = attempts,
                max = config.max_retries,
                "Verification failed, running fixup"
            );

            {
                let mut state = state.lock().await;
                state.update_status(&task.path, FileStatus::FixupInProgress);
                if let Err(e) = state.save(&state_path) {
                    error!(error = %e, "Failed to save state");
                }
            }

            let fixup_prompt_base = config
                .fixup_prompt
                .as_deref()
                .unwrap_or("Fix the issues with the file");

            let error_output = if result.stderr.is_empty() {
                &result.stdout
            } else {
                &result.stderr
            };

            let fixup_prompt = build_fixup_prompt(
                fixup_prompt_base,
                &task.path,
                error_output,
                &config.allowlist_pattern,
            );

            // Run fixup
            match run_claude(&fixup_prompt, &working_dir).await {
                Ok(output) => {
                    // Parse and update result
                    let parsed = parse_result(&output.stdout);
                    {
                        let mut state = state.lock().await;
                        state.set_result(&task.path, parsed);
                        if let Err(e) = state.save(&state_path) {
                            error!(error = %e, "Failed to save state");
                        }
                    }
                    info!(
                        worker = worker_id,
                        file = %file_display,
                        "Fixup complete, re-verifying"
                    );
                }
                Err(e) => {
                    error!(
                        worker = worker_id,
                        file = %file_display,
                        error = %e,
                        "Fixup failed"
                    );
                    // Mark as failed
                    let mut state = state.lock().await;
                    state.update_status(&task.path, FileStatus::Failed);
                    state.set_error(&task.path, e.to_string());
                    if let Err(e) = state.save(&state_path) {
                        error!(error = %e, "Failed to save state");
                    }
                    break;
                }
            }

            // Loop continues to re-verify
        }
    }

    info!(worker = worker_id, "Verify worker shutting down");
}
