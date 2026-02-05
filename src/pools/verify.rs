use crate::claude::{build_fixup_prompt, run_claude};
use crate::config::Config;
use crate::git::commit_file_changes;
use crate::memory::MemoryHandle;
use crate::process::{expand_pattern_with_allowlist, parse_result, run_command};
use crate::state::State;
use crate::types::{FileStatus, FileTask};
use async_channel::Receiver;
use chrono::Utc;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
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
    tasks_dir: PathBuf,
    memory: MemoryHandle,
) -> Vec<JoinHandle<()>> {
    (0..concurrency)
        .map(|worker_id| {
            let rx = rx.clone();
            let state = Arc::clone(&state);
            let state_path = state_path.clone();
            let config = Arc::clone(&config);
            let working_dir = working_dir.clone();
            let tasks_dir = tasks_dir.clone();
            let memory = memory.clone();

            tokio::spawn(async move {
                verify_worker(
                    worker_id,
                    rx,
                    state,
                    state_path,
                    config,
                    working_dir,
                    tasks_dir,
                    memory,
                )
                .await;
            })
        })
        .collect()
}

/// Append a message to the failure log for a file
fn append_to_failure_log(tasks_dir: &Path, file_path: &Path, message: &str) {
    let failures_dir = tasks_dir.join("failures");
    if let Err(e) = fs::create_dir_all(&failures_dir) {
        error!(error = %e, "Failed to create failures directory");
        return;
    }

    // Create log filename from the source file path
    let log_name = file_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let log_path = failures_dir.join(format!("{}.log", log_name));

    let timestamp = Utc::now().format("%Y-%m-%d %H:%M:%S UTC");

    match OpenOptions::new().create(true).append(true).open(&log_path) {
        Ok(mut file) => {
            let separator = "=".repeat(80);
            if let Err(e) = writeln!(file, "\n{}\n[{}]\n{}", separator, timestamp, message) {
                error!(error = %e, "Failed to write to failure log");
            }
        }
        Err(e) => {
            error!(error = %e, path = %log_path.display(), "Failed to open failure log");
        }
    }
}

async fn verify_worker(
    worker_id: usize,
    rx: Receiver<FileTask>,
    state: Arc<Mutex<State>>,
    state_path: PathBuf,
    config: Arc<Config>,
    working_dir: PathBuf,
    tasks_dir: PathBuf,
    memory: MemoryHandle,
) {
    let verification_cmd = match &config.verification_cmd {
        Some(cmd) => cmd.clone(),
        None => {
            // No verification configured, worker exits immediately
            return;
        }
    };

    while let Ok(task) = rx.recv().await {
        // Wait if memory pressure is high
        if memory.is_paused() {
            info!(worker = worker_id, "Waiting for memory pressure to ease...");
            memory.wait_if_paused().await;
            info!(worker = worker_id, "Resuming after memory recovery");
        }
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
            let cmd = expand_pattern_with_allowlist(
                &verification_cmd,
                &task.path,
                &config.allowlist_pattern,
            );
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

            // Build error message for logging
            let error_output = if result.stderr.is_empty() {
                &result.stdout
            } else {
                &result.stderr
            };

            // Log verification failure
            let failure_msg = format!(
                "VERIFICATION FAILED (attempt {}/{})\nCommand: {}\nExit code: {}\n\nOutput:\n{}",
                attempts, config.max_retries, cmd, result.exit_code, error_output
            );
            append_to_failure_log(&tasks_dir, &task.path, &failure_msg);

            if attempts >= config.max_retries {
                // Max retries reached
                warn!(
                    worker = worker_id,
                    file = %file_display,
                    attempts = attempts,
                    "Verification FAILED after max retries"
                );

                // Log final failure
                append_to_failure_log(
                    &tasks_dir,
                    &task.path,
                    "FINAL STATUS: FAILED after max retries",
                );

                let mut state = state.lock().await;
                state.update_status(&task.path, FileStatus::Failed);
                state.set_error(&task.path, error_output.clone());
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

            let fixup_prompt = build_fixup_prompt(
                fixup_prompt_base,
                &task.path,
                error_output,
                &config.allowlist_pattern,
            );

            // Log the fixup prompt being sent
            append_to_failure_log(
                &tasks_dir,
                &task.path,
                &format!("FIXUP PROMPT SENT:\n{}", fixup_prompt),
            );

            // Run fixup
            match run_claude(&fixup_prompt, &working_dir).await {
                Ok(output) => {
                    // Log Claude's response
                    let response_log = format!(
                        "CLAUDE FIXUP RESPONSE:\n\nSTDOUT:\n{}\n\nSTDERR:\n{}",
                        output.stdout, output.stderr
                    );
                    append_to_failure_log(&tasks_dir, &task.path, &response_log);

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

                    // Log fixup failure
                    append_to_failure_log(
                        &tasks_dir,
                        &task.path,
                        &format!("FIXUP COMMAND FAILED: {}", e),
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
