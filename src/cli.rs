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

    /// Number of workers for prompt pool [default: 5]
    #[arg(short, long)]
    pub concurrency: Option<usize>,

    /// Number of workers for verify pool (defaults to concurrency value)
    #[arg(long)]
    pub verify_concurrency: Option<usize>,

    /// Maximum number of files to process
    #[arg(short, long)]
    pub max_files: Option<usize>,

    /// File allowlist pattern for Claude ({file}, {file_stem}, {file_dir} substituted) [default: {file_stem}*]
    #[arg(short, long)]
    pub allowlist: Option<String>,

    /// Tasks directory for state files and task list
    #[arg(short = 'd', long, default_value = "./claude-loop-tasks")]
    pub tasks_dir: PathBuf,

    /// Resume a specific task by ID, or all incomplete tasks if not specified
    #[arg(long)]
    pub resume: Option<Option<String>>,

    /// Maximum number of fixup retry attempts [default: 3]
    #[arg(long)]
    pub max_retries: Option<u32>,

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

    /// Disable all git features (overrides --git, --git-commit, --git-branch)
    #[arg(long)]
    pub no_git: bool,

    /// Claude CLI binary to run (name on PATH or a path) [default: claude]
    #[arg(long)]
    pub claude_bin: Option<String>,

    /// Don't auto-launch into a tmux session
    #[arg(long)]
    pub no_tmux: bool,

    /// Usage limit percentage (0-100). Pauses workers when Claude API utilization
    /// exceeds this threshold on either the 5-hour or 7-day window, and sleeps
    /// until the quota resets. Set to 0 to disable.
    #[arg(long, value_name = "PERCENT", default_value = "95")]
    pub limit: f64,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("claude-loop-runner").chain(args.iter().copied()))
            .expect("args should parse")
    }

    #[test]
    fn defaults() {
        let cli = parse(&[]);
        assert_eq!(cli.input, None);
        assert_eq!(cli.prompt, None);
        assert_eq!(cli.fixup, None);
        assert_eq!(cli.verify, None);
        // Defaulted options stay None so a resume can tell "not passed" from "passed the default"
        assert_eq!(cli.concurrency, None);
        assert_eq!(cli.verify_concurrency, None);
        assert_eq!(cli.max_files, None);
        assert_eq!(cli.allowlist, None);
        assert_eq!(cli.tasks_dir, PathBuf::from("./claude-loop-tasks"));
        assert_eq!(cli.resume, None);
        assert_eq!(cli.max_retries, None);
        assert_eq!(cli.working_dir, None);
        assert!(!cli.dry_run && !cli.git && !cli.git_branch && !cli.git_commit);
        assert!(!cli.no_git && !cli.no_tmux);
        assert_eq!(cli.git_commit_message, None);
        assert_eq!(cli.claude_bin, None);
        assert_eq!(cli.limit, 95.0);
        assert!(!cli.is_resume());
        assert_eq!(cli.resume_task_id(), None);
    }

    #[test]
    fn short_and_long_flags() {
        let cli = parse(&[
            "-i",
            "in.json",
            "-p",
            "do it",
            "-f",
            "fix it",
            "-v",
            "make test {file}",
            "-c",
            "8",
            "-m",
            "10",
            "-a",
            "*.rs",
            "-d",
            "/tmp/tasks",
            "-w",
            "/work",
            "--verify-concurrency",
            "2",
            "--max-retries",
            "0",
            "--dry-run",
            "--no-tmux",
            "--claude-bin",
            "/opt/fake-claude",
            "--limit",
            "50.5",
            "--git-commit-message",
            "chore: {file}",
        ]);
        assert_eq!(cli.input, Some(PathBuf::from("in.json")));
        assert_eq!(cli.prompt.as_deref(), Some("do it"));
        assert_eq!(cli.fixup.as_deref(), Some("fix it"));
        assert_eq!(cli.verify.as_deref(), Some("make test {file}"));
        assert_eq!(cli.concurrency, Some(8));
        assert_eq!(cli.max_files, Some(10));
        assert_eq!(cli.allowlist.as_deref(), Some("*.rs"));
        assert_eq!(cli.tasks_dir, PathBuf::from("/tmp/tasks"));
        assert_eq!(cli.working_dir, Some(PathBuf::from("/work")));
        assert_eq!(cli.verify_concurrency, Some(2));
        assert_eq!(cli.max_retries, Some(0));
        assert!(cli.dry_run && cli.no_tmux);
        assert_eq!(cli.claude_bin.as_deref(), Some("/opt/fake-claude"));
        assert_eq!(cli.limit, 50.5);
        assert_eq!(cli.git_commit_message.as_deref(), Some("chore: {file}"));
    }

    #[test]
    fn resume_without_value_resumes_all() {
        let cli = parse(&["--resume"]);
        assert!(cli.is_resume());
        assert_eq!(cli.resume, Some(None));
        assert_eq!(cli.resume_task_id(), None);
    }

    #[test]
    fn resume_with_value_targets_task() {
        for args in [&["--resume", "task-123"][..], &["--resume=task-123"][..]] {
            let cli = parse(args);
            assert!(cli.is_resume());
            assert_eq!(cli.resume_task_id(), Some("task-123"));
        }
    }

    #[test]
    fn resume_followed_by_flag_does_not_swallow_it() {
        let cli = parse(&["--resume", "--dry-run"]);
        assert_eq!(cli.resume_task_id(), None);
        assert!(cli.dry_run);
    }

    #[test]
    fn invalid_values_are_rejected() {
        let bad: &[&[&str]] = &[
            &["-c", "many"],
            &["-c", "-1"],
            &["--max-retries", "-1"],
            &["--limit", "lots"],
            &["--unknown-flag"],
            &["-i"],
        ];
        for args in bad {
            let full = std::iter::once("claude-loop-runner").chain(args.iter().copied());
            assert!(Cli::try_parse_from(full).is_err(), "{args:?} should fail");
        }
    }

    #[test]
    fn validate_requires_input_and_prompt_unless_resuming() {
        let err = parse(&["-p", "x"]).validate().unwrap_err().to_string();
        assert!(err.contains("--input"), "{err}");
        let err = parse(&["-i", "in.json"])
            .validate()
            .unwrap_err()
            .to_string();
        assert!(err.contains("--prompt"), "{err}");
        // Input is checked first when both are missing.
        let err = parse(&[]).validate().unwrap_err().to_string();
        assert!(err.contains("--input"), "{err}");

        parse(&["-i", "in.json", "-p", "x"]).validate().unwrap();
        parse(&["--resume"]).validate().unwrap();
        parse(&["--resume", "abc"]).validate().unwrap();
    }
}
