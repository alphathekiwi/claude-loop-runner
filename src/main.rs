mod claude;
mod cli;
mod config;
mod pools;
mod process;
mod runner;
mod state;
mod types;

use anyhow::{Context, Result};
use clap::Parser;
use cli::Cli;
use config::Config;
use state::State;
use tokio::sync::broadcast;
use tracing::{error, info, Level};
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

    // Load or create config and state
    let (config, mut state) = if cli.resume {
        // Load existing state
        let state = State::load(&cli.state)
            .with_context(|| format!("Failed to load state file: {}", cli.state.display()))?;

        info!(state_file = %cli.state.display(), "Resuming from state file");

        // Merge CLI args over saved config
        let config = state.config.clone().merge_with_cli(&cli);

        (config, state)
    } else {
        // Create new config and state
        let config = Config::from_cli(&cli)?;
        let state = State::new(config.clone());

        info!("Starting new run");

        (config, state)
    };

    // Merge input file if provided
    if let Some(ref input) = cli.input {
        state
            .merge_input_file(input)
            .with_context(|| format!("Failed to load input file: {}", input.display()))?;

        info!(input = %input.display(), files = state.files.len(), "Loaded input file");
    }

    // Update config in state and save initial state
    state.config = config.clone();
    state
        .save(&cli.state)
        .context("Failed to save initial state")?;

    // Set up shutdown signal handler
    let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);

    // Handle Ctrl+C
    let shutdown_tx_clone = shutdown_tx.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to listen for Ctrl+C");
        info!("Received Ctrl+C, shutting down gracefully...");
        let _ = shutdown_tx_clone.send(());
    });

    // Run the main loop
    let result = runner::run(config, state, cli.state.clone(), shutdown_rx).await;

    if let Err(ref e) = result {
        error!(error = %e, "Run failed");
    }

    result
}
