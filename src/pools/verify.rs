use super::{auto_commit, WorkerContext};
use crate::claude::{build_fixup_prompt, require_success, run_claude};
use crate::process::{expand_pattern_with_allowlist, parse_result, run_command};
use crate::types::{FileStatus, FileTask};
use async_channel::Receiver;
use chrono::Utc;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

/// Spawn a pool of verification workers
pub fn spawn_verify_pool(
    concurrency: usize,
    rx: Receiver<FileTask>,
    ctx: WorkerContext,
) -> Vec<JoinHandle<()>> {
    (0..concurrency)
        .map(|worker_id| {
            let rx = rx.clone();
            let ctx = ctx.clone();

            tokio::spawn(async move {
                verify_worker(worker_id, rx, ctx).await;
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

async fn verify_worker(worker_id: usize, rx: Receiver<FileTask>, ctx: WorkerContext) {
    let tasks_dir = ctx.tasks_dir.clone();
    let verification_cmd = match &ctx.config.verification_cmd {
        Some(cmd) => cmd.clone(),
        None => return,
    };

    while let Ok(task) = rx.recv().await {
        // Wait if memory pressure is high
        if ctx.memory.is_paused() {
            info!(worker = worker_id, "Waiting for memory pressure to ease...");
            ctx.memory.wait_if_paused().await;
            info!(worker = worker_id, "Resuming after memory recovery");
        }

        // Wait if API usage limit exceeded
        if ctx.usage.is_paused() {
            info!(
                worker = worker_id,
                "Waiting for API usage quota to reset..."
            );
            ctx.usage.wait_if_paused().await;
            info!(worker = worker_id, "Resuming after usage quota reset");
        }
        let file_display = task.path.display().to_string();
        let mut attempts = {
            let state = ctx.state.lock().await;
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
                let mut state = ctx.state.lock().await;
                state.update_status(&task.path, FileStatus::VerifyInProgress);
                if let Err(e) = state.save(&ctx.state_path) {
                    error!(error = %e, "Failed to save state");
                }
            }

            // Run verification command
            let cmd = expand_pattern_with_allowlist(
                &verification_cmd,
                &task.path,
                &ctx.config.allowlist_pattern,
            );
            let result = match run_command(&cmd, &ctx.working_dir).await {
                Ok(r) => r,
                Err(e) => {
                    error!(
                        worker = worker_id,
                        file = %file_display,
                        error = %e,
                        "Verification command failed to execute"
                    );
                    let mut state = ctx.state.lock().await;
                    state.update_status(&task.path, FileStatus::Failed);
                    state.set_error(&task.path, e.to_string());
                    if let Err(e) = state.save(&ctx.state_path) {
                        error!(error = %e, "Failed to save state");
                    }
                    break;
                }
            };

            if result.exit_code == 0 {
                info!(
                    worker = worker_id,
                    file = %file_display,
                    "Verification PASSED"
                );

                auto_commit(&ctx, worker_id, &task).await;

                let mut state = ctx.state.lock().await;
                state.update_status(&task.path, FileStatus::Completed);
                if let Err(e) = state.save(&ctx.state_path) {
                    error!(error = %e, "Failed to save state");
                }
                break;
            }

            // Verification failed
            attempts += 1;
            {
                let mut state = ctx.state.lock().await;
                state.increment_attempts(&task.path);
            }

            let error_output = if result.stderr.is_empty() {
                &result.stdout
            } else {
                &result.stderr
            };

            let failure_msg = format!(
                "VERIFICATION FAILED (attempt {}/{})\nCommand: {}\nExit code: {}\n\nOutput:\n{}",
                attempts, ctx.config.max_retries, cmd, result.exit_code, error_output
            );
            append_to_failure_log(&tasks_dir, &task.path, &failure_msg);

            if attempts >= ctx.config.max_retries {
                warn!(
                    worker = worker_id,
                    file = %file_display,
                    attempts = attempts,
                    "Verification FAILED after max retries"
                );

                append_to_failure_log(
                    &tasks_dir,
                    &task.path,
                    "FINAL STATUS: FAILED after max retries",
                );

                let mut state = ctx.state.lock().await;
                state.update_status(&task.path, FileStatus::Failed);
                state.set_error(&task.path, error_output.clone());
                if let Err(e) = state.save(&ctx.state_path) {
                    error!(error = %e, "Failed to save state");
                }
                break;
            }

            // Run fixup
            warn!(
                worker = worker_id,
                file = %file_display,
                attempt = attempts,
                max = ctx.config.max_retries,
                "Verification failed, running fixup"
            );

            {
                let mut state = ctx.state.lock().await;
                state.update_status(&task.path, FileStatus::FixupInProgress);
                if let Err(e) = state.save(&ctx.state_path) {
                    error!(error = %e, "Failed to save state");
                }
            }

            let fixup_prompt_base = ctx
                .config
                .fixup_prompt
                .as_deref()
                .unwrap_or("Fix the issues with the file");

            let fixup_prompt = build_fixup_prompt(
                fixup_prompt_base,
                &task.path,
                error_output,
                &ctx.config.allowlist_pattern,
            );

            append_to_failure_log(
                &tasks_dir,
                &task.path,
                &format!("FIXUP PROMPT SENT:\n{}", fixup_prompt),
            );

            // Wait if API usage limit exceeded before calling Claude for fixup
            if ctx.usage.is_paused() {
                info!(
                    worker = worker_id,
                    "Waiting for API usage quota to reset before fixup..."
                );
                ctx.usage.wait_if_paused().await;
                info!(worker = worker_id, "Resuming fixup after usage quota reset");
            }

            let outcome = run_claude(&ctx.config.claude_bin, &fixup_prompt, &ctx.working_dir)
                .await
                .and_then(require_success);
            match outcome {
                Ok(output) => {
                    let response_log = format!(
                        "CLAUDE FIXUP RESPONSE:\n\nSTDOUT:\n{}\n\nSTDERR:\n{}",
                        output.stdout, output.stderr
                    );
                    append_to_failure_log(&tasks_dir, &task.path, &response_log);

                    let parsed = parse_result(&output.stdout);
                    {
                        let mut state = ctx.state.lock().await;
                        state.set_result(&task.path, parsed);
                        if let Err(e) = state.save(&ctx.state_path) {
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

                    append_to_failure_log(
                        &tasks_dir,
                        &task.path,
                        &format!("FIXUP COMMAND FAILED: {}", e),
                    );

                    let mut state = ctx.state.lock().await;
                    state.update_status(&task.path, FileStatus::Failed);
                    state.set_error(&task.path, e.to_string());
                    if let Err(e) = state.save(&ctx.state_path) {
                        error!(error = %e, "Failed to save state");
                    }
                    break;
                }
            }
        }
    }

    info!(worker = worker_id, "Verify worker shutting down");
}
