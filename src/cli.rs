use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug, Clone)]
#[command(name = "claude-loop-runner")]
#[command(about = "Run multiple Claude CLI instances in parallel to process files")]
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

    /// Verification command ({file} will be substituted)
    #[arg(short, long)]
    pub verify: Option<String>,

    /// Number of workers per pool
    #[arg(short, long, default_value = "5")]
    pub concurrency: usize,

    /// Maximum number of files to process
    #[arg(short, long)]
    pub max_files: Option<usize>,

    /// File allowlist pattern for Claude ({file}, {file_stem}, {file_dir} substituted)
    #[arg(short, long, default_value = "{file_stem}*")]
    pub allowlist: String,

    /// State file path for persistence and resume
    #[arg(short, long, default_value = "./claude-loop-runner.state.json")]
    pub state: PathBuf,

    /// Resume from existing state file
    #[arg(long)]
    pub resume: bool,

    /// Maximum number of fixup retry attempts
    #[arg(long, default_value = "3")]
    pub max_retries: u32,
}

impl Cli {
    /// Validate that required arguments are present when not resuming
    pub fn validate(&self) -> anyhow::Result<()> {
        if !self.resume {
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
