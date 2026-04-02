//! Extended Exchange Market Maker CLI.

use anyhow::Result;
use clap::Parser;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

mod fill_logger;
mod market_bot;
mod orchestrator;
mod state;

#[derive(Parser, Debug)]
#[command(name = "extended-mm", about = "Extended Exchange Market Maker")]
struct Cli {
    /// Force paper trading mode (no live orders)
    #[arg(long)]
    paper: bool,

    /// Smoke mode: connect + log, but never send orders
    #[arg(long)]
    smoke: bool,

    /// Close all positions and exit
    #[arg(long)]
    close: bool,

    /// Config file path (default: config/default)
    #[arg(long, short, default_value = "config/default")]
    config: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    dotenvy::dotenv().ok();

    // Init tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,extended_bot=debug,extended_exchange=debug")),
        )
        .with_target(true)
        .with_thread_ids(false)
        .compact()
        .init();

    info!(
        paper = cli.paper,
        smoke = cli.smoke,
        "Starting extended-mm"
    );

    // Load config
    let mut app_config = load_config(&cli.config)?;

    // Override from env
    if let Ok(key) = std::env::var("EXTENDED_API_KEY") {
        app_config.exchange.api_key = key;
    }
    if let Ok(secret) = std::env::var("EXTENDED_API_SECRET") {
        app_config.exchange.api_secret = secret;
    }

    // CLI overrides
    if cli.paper {
        app_config.exchange.paper_trading = true;
    }

    // Validation
    if !app_config.exchange.paper_trading && app_config.exchange.api_key.is_empty() {
        anyhow::bail!("EXTENDED_API_KEY required for live trading. Set env or use --paper.");
    }
    if !app_config.exchange.paper_trading && app_config.exchange.api_secret.is_empty() {
        anyhow::bail!("EXTENDED_API_SECRET required for live trading. Set env or use --paper.");
    }

    if app_config.exchange.paper_trading {
        info!("*** PAPER TRADING MODE - No live orders will be sent ***");
    }
    if cli.smoke {
        info!("*** SMOKE MODE - Observe only, no orders ***");
    }

    // Close mode: close all positions and exit
    if cli.close {
        info!("*** CLOSE MODE - Closing all positions ***");
        match orchestrator::close_all(app_config).await {
            Ok(()) => info!("Positions closed"),
            Err(e) => error!(error = %e, "Failed to close positions"),
        }
        return Ok(());
    }

    // Run the orchestrator
    match orchestrator::run(app_config, cli.smoke).await {
        Ok(()) => info!("Shutdown complete"),
        Err(e) => {
            error!(error = %e, "Fatal error");
            std::process::exit(1);
        }
    }

    Ok(())
}

fn load_config(config_path: &str) -> Result<extended_types::config::AppConfig> {
    info!(config = %config_path, "Loading config");
    let settings = config::Config::builder()
        .add_source(config::File::with_name(config_path).required(true))
        .add_source(config::Environment::with_prefix("EXTENDED").separator("__"))
        .build()?;

    let app_config: extended_types::config::AppConfig = settings.try_deserialize()?;
    Ok(app_config)
}
