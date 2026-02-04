use crate::cli::Cli;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Configuration for the runner, persisted in state file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Path to the input JSON file
    pub input_file: PathBuf,
    /// Main prompt for Claude
    pub prompt: String,
    /// Fixup prompt when verification fails
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixup_prompt: Option<String>,
    /// Verification command template
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_cmd: Option<String>,
    /// File allowlist pattern
    pub allowlist_pattern: String,
    /// Number of workers per pool
    pub concurrency: usize,
    /// Maximum files to process
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_files: Option<usize>,
    /// Maximum fixup retry attempts
    pub max_retries: u32,
}

impl Config {
    /// Create a new config from CLI arguments
    pub fn from_cli(cli: &Cli) -> anyhow::Result<Self> {
        let input_file = cli
            .input
            .clone()
            .ok_or_else(|| anyhow::anyhow!("--input is required"))?;
        let prompt = cli
            .prompt
            .clone()
            .ok_or_else(|| anyhow::anyhow!("--prompt is required"))?;

        Ok(Self {
            input_file,
            prompt,
            fixup_prompt: cli.fixup.clone(),
            verification_cmd: cli.verify.clone(),
            allowlist_pattern: cli.allowlist.clone(),
            concurrency: cli.concurrency,
            max_files: cli.max_files,
            max_retries: cli.max_retries,
        })
    }

    /// Merge CLI args over saved config
    /// CLI args win if explicitly provided
    pub fn merge_with_cli(mut self, cli: &Cli) -> Self {
        if let Some(ref input) = cli.input {
            self.input_file = input.clone();
        }
        if let Some(ref prompt) = cli.prompt {
            self.prompt = prompt.clone();
        }
        if let Some(ref fixup) = cli.fixup {
            self.fixup_prompt = Some(fixup.clone());
        }
        if let Some(ref verify) = cli.verify {
            self.verification_cmd = Some(verify.clone());
        }
        // Only override allowlist if not default
        if cli.allowlist != "{file_stem}*" {
            self.allowlist_pattern = cli.allowlist.clone();
        }
        // Only override concurrency if not default
        if cli.concurrency != 5 {
            self.concurrency = cli.concurrency;
        }
        if let Some(max_files) = cli.max_files {
            self.max_files = Some(max_files);
        }
        // Only override max_retries if not default
        if cli.max_retries != 3 {
            self.max_retries = cli.max_retries;
        }
        self
    }
}
