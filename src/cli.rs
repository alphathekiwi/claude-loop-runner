use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug, Clone)]
#[command(name = "claude-loop-runner")]
#[command(about = "Run multiple Claude CLI instances in parallel to process files")]
#[command(
    long_about = "Run multiple Claude CLI instances in parallel to process files.

This tool is designed for long-running batch operations that may take hours to complete.
It is strongly recommended to run this in tmux or screen to avoid timeouts and
disconnections. Use --resume to continue from where you left off if interrupted."
)]
#[command(version)]
pub struct Cli {
    /// Input JSON file mapping filepaths to metadata
    #[arg(short, long)]
    pub input: Option<PathBuf>,

    /// Main prompt for Claude CLI
    #[arg(short, long)]
    pub prompt: Option<String>,

    /// Fixup prompt when verification fails
    #[arg(short, long)]
    pub fixup: Option<String>,

    /// Verification command (substitutions: {file}, {file_stem}, {file_dir}, {all_files}, {test_files}, {created_files})
    #[arg(short, long)]
    pub verify: Option<String>,

    /// Number of workers for prompt pool
    #[arg(short, long, default_value = "5")]
    pub concurrency: usize,

    /// Number of workers for verify pool (defaults to concurrency value)
    #[arg(long)]
    pub verify_concurrency: Option<usize>,

    /// Maximum number of files to process
    #[arg(short, long)]
    pub max_files: Option<usize>,

    /// File allowlist pattern for Claude ({file}, {file_stem}, {file_dir} substituted)
    #[arg(short, long, default_value = "{file_stem}*")]
    pub allowlist: String,

    /// Tasks directory for state files and task list
    #[arg(short = 'd', long, default_value = "./claude-loop-tasks")]
    pub tasks_dir: PathBuf,

    /// Resume a specific task by ID, or all incomplete tasks if not specified
    #[arg(long)]
    pub resume: Option<Option<String>>,

    /// Maximum number of fixup retry attempts
    #[arg(long, default_value = "3")]
    pub max_retries: u32,

    /// Working directory for the task (defaults to current directory)
    #[arg(short = 'w', long)]
    pub working_dir: Option<PathBuf>,

    /// Dry run: create task config but don't run any Claude CLIs
    #[arg(long)]
    pub dry_run: bool,

    /// Enable git features (capture dirty files, optionally branch/commit)
    #[arg(long)]
    pub git: bool,

    /// Automatically create a branch for this task (implies --git)
    #[arg(long)]
    pub git_branch: bool,

    /// Automatically commit after each file completes (implies --git)
    #[arg(long)]
    pub git_commit: bool,

    /// Custom commit message template (supports {file}, {file_stem}, {task_id})
    #[arg(long)]
    pub git_commit_message: Option<String>,
}

impl Cli {
    /// Check if we're in resume mode
    pub fn is_resume(&self) -> bool {
        self.resume.is_some()
    }

    /// Get the specific task ID to resume, if any
    pub fn resume_task_id(&self) -> Option<&str> {
        self.resume.as_ref().and_then(|o| o.as_deref())
    }

    /// Validate that required arguments are present when not resuming
    pub fn validate(&self) -> anyhow::Result<()> {
        if !self.is_resume() {
            if self.input.is_none() {
                anyhow::bail!("--input is required when not using --resume");
            }
            if self.prompt.is_none() {
                anyhow::bail!("--prompt is required when not using --resume");
            }
        }
        Ok(())
    }
}
