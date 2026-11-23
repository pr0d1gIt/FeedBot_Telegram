//! `telegram-cybersec-bot` — entry point.
//!
//! Wires together configuration, persistence, aggregator, the Telegram
//! command dispatcher and the periodic scheduler, then runs until the
//! process is terminated (Ctrl-C is handled by teloxide).

mod aggregator;
mod bot;
mod config;
mod models;
mod persistence;
mod scheduler;

use crate::aggregator::Aggregator;
use crate::config::Config;
use crate::persistence::State;
use anyhow::{Context, Result};
use teloxide::Bot;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    info!(
        version = env!("CARGO_PKG_VERSION"),
        "Starting telegram-cybersec-bot"
    );

    let config = Config::from_env().context("Failed to load configuration")?;
    info!(
        interval_secs = config.schedule_interval.as_secs(),
        state_path = %config.state_path.display(),
        github_auth = config.github_token.is_some(),
        "Configuration loaded"
    );

    let state = State::load(&config.state_path)
        .await
        .context("Failed to initialise persistent state")?;

    let aggregator = Aggregator::new(&config).context("Failed to build aggregator")?;

    let bot = Bot::new(&config.telegram_token);

    // Kick off the scheduler in the background.
    let _scheduler = scheduler::spawn(
        bot.clone(),
        state.clone(),
        aggregator.clone(),
        config.schedule_interval,
        config.max_items_per_category,
    );

    // Run the Telegram dispatcher in the foreground. Returns when the bot is
    // stopped (e.g. Ctrl-C handled by teloxide).
    bot::run(bot, state, aggregator, config.max_items_per_category).await?;

    info!("Shutdown complete");
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info,telegram_cybersec_bot=info,teloxide=warn,reqwest=warn")
    });

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_level(true)
        .compact()
        .init();
}