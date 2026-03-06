mod claude;
mod cli;
mod config;
mod git;
mod memory;
mod pools;
mod process;
mod runner;
mod state;
mod task_list;
mod types;
mod usage;

use anyhow::{Context, Result};
use clap::Parser;
use cli::Cli;
use config::Config;
use git::GitState;
use state::State;
use task_list::TaskList;
use tokio::sync::broadcast;
use tracing::{error, info, warn, Level};
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_target(false)
        .with_thread_ids(false)
        .compact()
        .init();

    let cli = Cli::parse();
    cli.validate()?;

    // Validate concurrency
    if cli.concurrency == 0 {
        anyhow::bail!("--concurrency must be at least 1");
    }

    // Load or create task list
    let mut task_list = TaskList::load_or_create(&cli.tasks_dir)?;

    // Determine working directory
    let working_dir = cli
        .working_dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    let (mut config, state, state_path, task_id) = if cli.is_resume() {
        // Resume mode
        if let Some(specific_task_id) = cli.resume_task_id() {
            // Resume a specific task
            let entry = task_list
                .get_task(specific_task_id)
                .ok_or_else(|| anyhow::anyhow!("Task not found: {}", specific_task_id))?;

            let state_path = cli.tasks_dir.join(&entry.state_file);
            let state = State::load(&state_path)
                .with_context(|| format!("Failed to load state for task: {}", specific_task_id))?;

            info!(task_id = %specific_task_id, state_file = %entry.state_file, "Resuming task");

            let config = state.config.clone().merge_with_cli(&cli);
            (config, state, state_path, specific_task_id.to_string())
        } else {
            // Resume first incomplete task
            let incomplete = task_list.get_incomplete_tasks();
            if incomplete.is_empty() {
                anyhow::bail!(
                    "No incomplete tasks to resume. Use --input and --prompt to start a new task."
                );
            }

            let (task_id, entry) = incomplete.first().unwrap();
            let state_path = cli.tasks_dir.join(&entry.state_file);
            let state = State::load(&state_path)
                .with_context(|| format!("Failed to load state for task: {}", task_id))?;

            info!(task_id = %task_id, state_file = %entry.state_file, "Resuming first incomplete task");

            let config = state.config.clone().merge_with_cli(&cli);
            (config, state, state_path, task_id.to_string())
        }
    } else {
        // New task mode
        let config = Config::from_cli(&cli)?;
        let mut state = State::new(config.clone());

        // Create new task entry
        let description = cli.prompt.as_ref().map(|p| {
            if p.len() > 50 {
                format!("{}...", &p[..47])
            } else {
                p.clone()
            }
        });
        let task_id = task_list.create_task(working_dir.clone(), description);
        let state_path = cli
            .tasks_dir
            .join(task_list.get_task(&task_id).unwrap().state_file.clone());

        // Merge input file
        if let Some(ref input) = cli.input {
            state
                .merge_input_file(input)
                .with_context(|| format!("Failed to load input file: {}", input.display()))?;

            info!(input = %input.display(), files = state.files.len(), "Loaded input file");
        }

        // Build global allowlist from all files (+ related tests/snapshots) so it's available on resume
        if cli.git || cli.git_branch || cli.git_commit {
            let file_paths: Vec<_> = state.files.keys().cloned().collect();
            for path in &file_paths {
                let expanded = process::expand_pattern(&cli.allowlist, path);
                state.git_state.add_allowlist_pattern(expanded);

                // Discover related test/snapshot files and add their patterns too
                for related in process::find_related_files(path, &working_dir) {
                    let related_pattern = process::expand_pattern(&cli.allowlist, &related);
                    state.git_state.add_allowlist_pattern(related_pattern);
                }
            }
            if !state.git_state.global_allowlist_patterns.is_empty() {
                info!(
                    patterns = state.git_state.global_allowlist_patterns.len(),
                    "Built global allowlist during task creation (with related file discovery)"
                );
            }
        }

        // Save task list and initial state
        task_list.save(&cli.tasks_dir)?;
        state.config = config.clone();
        state
            .save(&state_path)
            .context("Failed to save initial state")?;

        info!(task_id = %task_id, "Created new task");

        (config, state, state_path, task_id)
    };

    // Auto-launch into tmux if not already in one
    if !cli.no_tmux && !cli.dry_run && std::env::var("TMUX").is_err() {
        if let Ok(output) = std::process::Command::new("tmux").arg("-V").output() {
            if output.status.success() {
                let session_name = build_tmux_session_name(&working_dir, &task_id);
                let exe = std::env::current_exe()
                    .unwrap_or_else(|_| "claude-loop-runner".into());
                let mut cmd = format!(
                    "{} --resume {} --no-tmux --tasks-dir {} -w {}",
                    exe.display(),
                    task_id,
                    cli.tasks_dir.display(),
                    working_dir.display(),
                );
                if cli.no_git {
                    cmd.push_str(" --no-git");
                }
                if cli.limit > 0.0 {
                    cmd.push_str(&format!(" --limit {}", cli.limit));
                }
                let result = std::process::Command::new("tmux")
                    .args(["new-session", "-d", "-s", &session_name, &cmd])
                    .status();
                match result {
                    Ok(status) if status.success() => {
                        info!(session = %session_name, "Launched in tmux session");
                        info!("Attach with: tmux attach -t {}", session_name);
                        std::process::exit(0);
                    }
                    Ok(status) => {
                        warn!(code = ?status.code(), "tmux new-session failed, continuing without tmux");
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to launch tmux, continuing without tmux");
                    }
                }
            }
        }
    }

    // Handle --no-git override
    if cli.no_git {
        if cli.git || cli.git_branch || cli.git_commit {
            warn!("--no-git specified: all git features disabled for this run");
        }
        config.git.enabled = false;
        config.git.auto_branch = false;
        config.git.auto_commit = false;
    }

    // Check git identity before enabling git features
    if (config.git.enabled || config.git.auto_branch || config.git.auto_commit)
        && git::is_git_repo(&working_dir).await.unwrap_or(false)
    {
            match git::check_git_identity(&working_dir).await {
                Ok(git::GitIdentityStatus::Configured { name, email }) => {
                    info!(name = %name, email = %email, "Git identity configured");
                }
                Ok(git::GitIdentityStatus::Missing { name, email }) => {
                    let missing_name = name.is_none();
                    let missing_email = email.is_none();

                    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                        warn!("Git identity not configured and stdin is not a terminal; disabling git features");
                        config.git.enabled = false;
                        config.git.auto_branch = false;
                        config.git.auto_commit = false;
                    } else {
                        match git::prompt_git_identity(missing_name, missing_email) {
                            Ok(git::GitIdentityAction::Configure {
                                name: new_name,
                                email: new_email,
                            }) => {
                                let final_name = if missing_name {
                                    &new_name
                                } else {
                                    name.as_ref().unwrap()
                                };
                                let final_email = if missing_email {
                                    &new_email
                                } else {
                                    email.as_ref().unwrap()
                                };
                                if let Err(e) =
                                    git::set_git_identity(&working_dir, final_name, final_email)
                                        .await
                                {
                                    warn!(error = %e, "Failed to set git identity, disabling git features");
                                    config.git.enabled = false;
                                    config.git.auto_branch = false;
                                    config.git.auto_commit = false;
                                }
                            }
                            Ok(git::GitIdentityAction::DisableGit) => {
                                warn!("Git features disabled by user choice (missing identity)");
                                config.git.enabled = false;
                                config.git.auto_branch = false;
                                config.git.auto_commit = false;
                            }
                            Err(e) => {
                                warn!(error = %e, "Failed to prompt for git identity, disabling git features");
                                config.git.enabled = false;
                                config.git.auto_branch = false;
                                config.git.auto_commit = false;
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Failed to check git identity, continuing (commits may fail)");
                }
            }
    }

    // Capture git state and set up branch if git features are enabled
    let mut state = state;
    if config.git.enabled || config.git.auto_branch || config.git.auto_commit {
        info!("Git features enabled, capturing initial git state");

        match GitState::capture(&working_dir).await {
            Ok(mut git_state) => {
                if git_state.enabled {
                    if !git_state.pre_existing_dirty_files.is_empty() {
                        warn!(
                            count = git_state.pre_existing_dirty_files.len(),
                            "Found pre-existing dirty files that will be excluded from unauthorized checks"
                        );
                    }

                    // Create task branch if requested
                    if config.git.auto_branch && git_state.task_branch.is_none() {
                        match git::create_task_branch(&working_dir, &task_id).await {
                            Ok(branch_name) => {
                                git_state.task_branch = Some(branch_name);
                                info!(task_id = %task_id, "Created task branch");
                            }
                            Err(e) => {
                                warn!(error = %e, "Failed to create task branch, continuing without branching");
                            }
                        }
                    }

                    state.set_git_state(git_state);
                    state
                        .save(&state_path)
                        .context("Failed to save state with git info")?;
                }
            }
            Err(e) => {
                warn!(error = %e, "Failed to capture git state, continuing without git features");
            }
        }
    }

    // Dry run: just show what would be done and exit
    if cli.dry_run {
        let summary = state.get_summary();
        info!(
            task_id = %task_id,
            state_file = %state_path.display(),
            total_files = summary.total,
            pending = summary.pending,
            completed = summary.completed,
            failed = summary.failed,
            concurrency = config.concurrency,
            "Dry run complete - task created but not executed"
        );
        info!(
            "To run this task, use: claude-loop-runner --resume {}",
            task_id
        );
        return Ok(());
    }

    // Set up shutdown signal handler
    let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);

    let shutdown_tx_clone = shutdown_tx.clone();
    let task_id_clone = task_id.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to listen for Ctrl+C");
        info!(task_id = %task_id_clone, "Received Ctrl+C, shutting down gracefully...");
        let _ = shutdown_tx_clone.send(());
    });

    // Run the task
    let result = runner::run(
        config,
        state,
        state_path.clone(),
        cli.tasks_dir.clone(),
        shutdown_rx,
        if cli.limit > 0.0 { Some(cli.limit) } else { None },
    )
    .await;

    // Check if task completed successfully
    if result.is_ok() {
        let state = State::load(&state_path)?;
        let summary = state.get_summary();
        if summary.pending == 0
            && summary.prompt_in_progress == 0
            && summary.verify_in_progress == 0
        {
            task_list.mark_completed(&task_id);
            task_list.save(&cli.tasks_dir)?;
            info!(task_id = %task_id, "Task marked as completed");
        }
    }

    if let Err(ref e) = result {
        error!(task_id = %task_id, error = %e, "Task failed");
    }

    result
}

/// Build a tmux session name from the working directory and task ID.
///
/// Takes first letter of each path component (lowercased) except the last,
/// joins with `-`, appends the full last component, then `_` + task number.
///
/// Example: `/Users/aaronsaunders/Documents/personal/claude-loop-runner` + `task_3`
///   → `d-p-claude-loop-runner_3`
fn build_tmux_session_name(working_dir: &std::path::Path, task_id: &str) -> String {
    let components: Vec<String> = working_dir
        .components()
        .filter_map(|c| {
            let s = c.as_os_str().to_str()?;
            // Skip root `/` and home-like prefixes (Users, home, username)
            if s == "/" || s == "Users" || s == "home" {
                return None;
            }
            Some(s.to_lowercase())
        })
        .collect();

    // Also skip the username component (first after Users/home)
    let parts = if components.len() > 1 {
        &components[1..]
    } else {
        &components[..]
    };

    let task_num = task_id
        .strip_prefix("task_")
        .unwrap_or(task_id);

    if parts.is_empty() {
        return format!("clr_{}", task_num);
    }

    let last = parts.last().unwrap();
    let prefixes: Vec<&str> = parts[..parts.len() - 1]
        .iter()
        .filter_map(|p| p.get(..1))
        .collect();

    if prefixes.is_empty() {
        format!("{}_{}", last, task_num)
    } else {
        format!("{}-{}_{}", prefixes.join("-"), last, task_num)
    }
}
