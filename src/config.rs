use crate::cli::Cli;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Git-related configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GitConfig {
    /// Enable git features (branch creation, commits)
    #[serde(default)]
    pub enabled: bool,
    /// Automatically create a branch for this task
    #[serde(default)]
    pub auto_branch: bool,
    /// Automatically commit after each file completes successfully
    #[serde(default)]
    pub auto_commit: bool,
    /// Custom commit message template (supports {file}, {file_stem}, {task_id})
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_message_template: Option<String>,
}

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
    /// Number of workers for prompt pool
    pub concurrency: usize,
    /// Number of workers for verify pool (defaults to concurrency if not set)
    #[serde(default)]
    pub verify_concurrency: Option<usize>,
    /// Maximum files to process
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_files: Option<usize>,
    /// Maximum fixup retry attempts
    pub max_retries: u32,
    /// Git configuration
    #[serde(default)]
    pub git: GitConfig,
    /// Claude CLI binary to run
    #[serde(default = "default_claude_bin")]
    pub claude_bin: String,
}

pub const DEFAULT_CONCURRENCY: usize = 5;
pub const DEFAULT_MAX_RETRIES: u32 = 3;
pub const DEFAULT_ALLOWLIST: &str = "{file_stem}*";
pub const DEFAULT_CLAUDE_BIN: &str = "claude";

fn default_claude_bin() -> String {
    DEFAULT_CLAUDE_BIN.to_string()
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

        let git = GitConfig {
            enabled: cli.git,
            auto_branch: cli.git_branch,
            auto_commit: cli.git_commit,
            commit_message_template: cli.git_commit_message.clone(),
        };

        Ok(Self {
            input_file,
            prompt,
            fixup_prompt: cli.fixup.clone(),
            verification_cmd: cli.verify.clone(),
            allowlist_pattern: cli
                .allowlist
                .clone()
                .unwrap_or_else(|| DEFAULT_ALLOWLIST.to_string()),
            concurrency: cli.concurrency.unwrap_or(DEFAULT_CONCURRENCY),
            verify_concurrency: cli.verify_concurrency,
            max_files: cli.max_files,
            max_retries: cli.max_retries.unwrap_or(DEFAULT_MAX_RETRIES),
            git,
            claude_bin: cli.claude_bin.clone().unwrap_or_else(default_claude_bin),
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
        if let Some(ref allowlist) = cli.allowlist {
            self.allowlist_pattern = allowlist.clone();
        }
        if let Some(concurrency) = cli.concurrency {
            self.concurrency = concurrency;
        }
        // Override verify_concurrency if provided
        if cli.verify_concurrency.is_some() {
            self.verify_concurrency = cli.verify_concurrency;
        }
        if let Some(max_files) = cli.max_files {
            self.max_files = Some(max_files);
        }
        if let Some(max_retries) = cli.max_retries {
            self.max_retries = max_retries;
        }
        if let Some(ref claude_bin) = cli.claude_bin {
            self.claude_bin = claude_bin.clone();
        }
        // Override git settings if explicitly enabled
        if cli.git {
            self.git.enabled = true;
        }
        if cli.git_branch {
            self.git.auto_branch = true;
        }
        if cli.git_commit {
            self.git.auto_commit = true;
        }
        if let Some(ref msg) = cli.git_commit_message {
            self.git.commit_message_template = Some(msg.clone());
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn cli(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("claude-loop-runner").chain(args.iter().copied()))
            .expect("args should parse")
    }

    /// A saved config whose every field differs from the CLI defaults.
    fn saved() -> Config {
        Config {
            input_file: PathBuf::from("saved.json"),
            prompt: "saved prompt".into(),
            fixup_prompt: Some("saved fixup".into()),
            verification_cmd: Some("saved verify".into()),
            allowlist_pattern: "saved*".into(),
            concurrency: 9,
            verify_concurrency: Some(4),
            max_files: Some(7),
            max_retries: 6,
            git: GitConfig {
                enabled: true,
                auto_branch: true,
                auto_commit: true,
                commit_message_template: Some("saved msg".into()),
            },
            claude_bin: "/saved/claude".into(),
        }
    }

    fn assert_unchanged(c: &Config) {
        let s = saved();
        assert_eq!(c.input_file, s.input_file);
        assert_eq!(c.prompt, s.prompt);
        assert_eq!(c.fixup_prompt, s.fixup_prompt);
        assert_eq!(c.verification_cmd, s.verification_cmd);
        assert_eq!(c.allowlist_pattern, s.allowlist_pattern);
        assert_eq!(c.concurrency, s.concurrency);
        assert_eq!(c.verify_concurrency, s.verify_concurrency);
        assert_eq!(c.max_files, s.max_files);
        assert_eq!(c.max_retries, s.max_retries);
        assert_eq!(c.claude_bin, s.claude_bin);
        assert!(c.git.enabled && c.git.auto_branch && c.git.auto_commit);
        assert_eq!(c.git.commit_message_template, s.git.commit_message_template);
    }

    #[test]
    fn from_cli_requires_input() {
        let err = Config::from_cli(&cli(&["-p", "x"])).unwrap_err();
        assert_eq!(err.to_string(), "--input is required");
    }

    #[test]
    fn from_cli_requires_prompt() {
        let err = Config::from_cli(&cli(&["-i", "in.json"])).unwrap_err();
        assert_eq!(err.to_string(), "--prompt is required");
    }

    #[test]
    fn from_cli_uses_defaults() {
        let c = Config::from_cli(&cli(&["-i", "in.json", "-p", "go"])).unwrap();
        assert_eq!(c.input_file, PathBuf::from("in.json"));
        assert_eq!(c.prompt, "go");
        assert_eq!(c.fixup_prompt, None);
        assert_eq!(c.verification_cmd, None);
        assert_eq!(c.allowlist_pattern, "{file_stem}*");
        assert_eq!(c.concurrency, 5);
        assert_eq!(c.verify_concurrency, None);
        assert_eq!(c.max_files, None);
        assert_eq!(c.max_retries, 3);
        assert_eq!(c.claude_bin, "claude");
        assert!(!c.git.enabled && !c.git.auto_branch && !c.git.auto_commit);
        assert_eq!(c.git.commit_message_template, None);
    }

    #[test]
    fn from_cli_copies_every_option() {
        let c = Config::from_cli(&cli(&[
            "-i",
            "in.json",
            "-p",
            "go",
            "-f",
            "fix",
            "-v",
            "check",
            "-a",
            "a*",
            "-c",
            "2",
            "--verify-concurrency",
            "1",
            "-m",
            "3",
            "--max-retries",
            "1",
            "--claude-bin",
            "cb",
            "--git",
            "--git-branch",
            "--git-commit",
            "--git-commit-message",
            "msg",
        ]))
        .unwrap();
        assert_eq!(c.fixup_prompt.as_deref(), Some("fix"));
        assert_eq!(c.verification_cmd.as_deref(), Some("check"));
        assert_eq!(c.allowlist_pattern, "a*");
        assert_eq!(c.concurrency, 2);
        assert_eq!(c.verify_concurrency, Some(1));
        assert_eq!(c.max_files, Some(3));
        assert_eq!(c.max_retries, 1);
        assert_eq!(c.claude_bin, "cb");
        assert!(c.git.enabled && c.git.auto_branch && c.git.auto_commit);
        assert_eq!(c.git.commit_message_template.as_deref(), Some("msg"));
    }

    #[test]
    fn from_cli_does_not_apply_no_git() {
        // --no-git is honoured in main after the config is built, not in Config.
        let c = Config::from_cli(&cli(&["-i", "i", "-p", "p", "--git", "--no-git"])).unwrap();
        assert!(c.git.enabled);
    }

    #[test]
    fn merge_with_bare_resume_keeps_saved_config() {
        assert_unchanged(&saved().merge_with_cli(&cli(&["--resume"])));
    }

    #[test]
    fn merge_overrides_every_explicit_non_default_value() {
        let c = saved().merge_with_cli(&cli(&[
            "--resume",
            "-i",
            "new.json",
            "-p",
            "new prompt",
            "-f",
            "new fixup",
            "-v",
            "new verify",
            "-a",
            "new*",
            "-c",
            "1",
            "--verify-concurrency",
            "2",
            "-m",
            "3",
            "--max-retries",
            "4",
            "--claude-bin",
            "/new/claude",
            "--git-commit-message",
            "new msg",
        ]));
        assert_eq!(c.input_file, PathBuf::from("new.json"));
        assert_eq!(c.prompt, "new prompt");
        assert_eq!(c.fixup_prompt.as_deref(), Some("new fixup"));
        assert_eq!(c.verification_cmd.as_deref(), Some("new verify"));
        assert_eq!(c.allowlist_pattern, "new*");
        assert_eq!(c.concurrency, 1);
        assert_eq!(c.verify_concurrency, Some(2));
        assert_eq!(c.max_files, Some(3));
        assert_eq!(c.max_retries, 4);
        assert_eq!(c.claude_bin, "/new/claude");
        assert_eq!(c.git.commit_message_template.as_deref(), Some("new msg"));
    }

    #[test]
    fn merge_applies_explicitly_passed_default_values() {
        // Passing the default explicitly (e.g. `-c 5` to go back from 9 workers) must win
        // over the saved value, even though it equals what omitting the flag would give.
        let c = saved().merge_with_cli(&cli(&[
            "--resume",
            "-c",
            "5",
            "--max-retries",
            "3",
            "-a",
            "{file_stem}*",
            "--claude-bin",
            "claude",
        ]));
        assert_eq!(c.concurrency, DEFAULT_CONCURRENCY);
        assert_eq!(c.max_retries, DEFAULT_MAX_RETRIES);
        assert_eq!(c.allowlist_pattern, DEFAULT_ALLOWLIST);
        assert_eq!(c.claude_bin, DEFAULT_CLAUDE_BIN);
    }

    #[test]
    fn merge_git_flags_only_turn_features_on() {
        let mut base = saved();
        base.git = GitConfig::default();
        let c = base.clone().merge_with_cli(&cli(&["--resume", "--git"]));
        assert!(c.git.enabled && !c.git.auto_branch && !c.git.auto_commit);
        let c = base
            .clone()
            .merge_with_cli(&cli(&["--resume", "--git-branch"]));
        assert!(!c.git.enabled && c.git.auto_branch && !c.git.auto_commit);
        let c = base.merge_with_cli(&cli(&["--resume", "--git-commit"]));
        assert!(!c.git.enabled && !c.git.auto_branch && c.git.auto_commit);

        // Omitting the flags (or passing --no-git) never turns saved features off.
        let c = saved().merge_with_cli(&cli(&["--resume", "--no-git"]));
        assert!(c.git.enabled && c.git.auto_branch && c.git.auto_commit);
    }

    #[test]
    fn merge_cannot_clear_optional_fields() {
        // There is no CLI way to unset a saved fixup/verify/max_files/verify_concurrency.
        let c = saved().merge_with_cli(&cli(&["--resume"]));
        assert!(c.fixup_prompt.is_some() && c.verification_cmd.is_some());
        assert!(c.max_files.is_some() && c.verify_concurrency.is_some());
    }

    #[test]
    fn deserialize_old_config_fills_serde_defaults() {
        let json = r#"{
            "input_file": "in.json",
            "prompt": "p",
            "allowlist_pattern": "{file_stem}*",
            "concurrency": 5,
            "max_retries": 3
        }"#;
        let c: Config = serde_json::from_str(json).unwrap();
        assert_eq!(c.claude_bin, "claude");
        assert_eq!(c.verify_concurrency, None);
        assert_eq!(c.fixup_prompt, None);
        assert_eq!(c.verification_cmd, None);
        assert_eq!(c.max_files, None);
        assert!(!c.git.enabled && !c.git.auto_branch && !c.git.auto_commit);
        assert_eq!(c.git.commit_message_template, None);
    }

    #[test]
    fn deserialize_partial_git_section() {
        let json = r#"{
            "input_file": "in.json", "prompt": "p", "allowlist_pattern": "*",
            "concurrency": 1, "max_retries": 0, "git": {"enabled": true}
        }"#;
        let c: Config = serde_json::from_str(json).unwrap();
        assert!(c.git.enabled && !c.git.auto_branch && !c.git.auto_commit);
    }

    #[test]
    fn deserialize_rejects_missing_required_fields() {
        for field in [
            "input_file",
            "prompt",
            "allowlist_pattern",
            "concurrency",
            "max_retries",
        ] {
            let mut v = serde_json::to_value(saved()).unwrap();
            v.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<Config>(v).is_err(),
                "missing {field} should fail"
            );
        }
    }

    #[test]
    fn serialize_round_trips_and_omits_none_fields() {
        let json = serde_json::to_value(saved()).unwrap();
        let back: Config = serde_json::from_value(json).unwrap();
        assert_unchanged(&back);

        let c = Config::from_cli(&cli(&["-i", "i", "-p", "p"])).unwrap();
        let v = serde_json::to_value(&c).unwrap();
        let obj = v.as_object().unwrap();
        for absent in ["fixup_prompt", "verification_cmd", "max_files"] {
            assert!(!obj.contains_key(absent), "{absent} should be skipped");
        }
        assert!(!obj["git"]
            .as_object()
            .unwrap()
            .contains_key("commit_message_template"));
        // verify_concurrency has no skip attribute, so it serializes as null.
        assert_eq!(obj["verify_concurrency"], serde_json::Value::Null);
    }
}
