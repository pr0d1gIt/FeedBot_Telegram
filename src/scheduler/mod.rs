//! Periodic scheduler that pushes a digest to all subscribers.
//!
//! The scheduler runs forever on a [`tokio::time::interval`] tick, every
//! `config.schedule_interval` (12 hours by default). The first tick happens
//! immediately on startup so subscribers receive something at boot.

use crate::aggregator::Aggregator;
use crate::bot::handlers::send_digest;
use crate::persistence::State;
use anyhow::Result;
use std::time::Duration;
use teloxide::prelude::*;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{error, info, warn};

/// Spawn the scheduler. Returns the join handle so `main` can await graceful
/// shutdown if it wishes; in practice we just let it run for the lifetime of
/// the process.
pub fn spawn(
    bot: Bot,
    state: State,
    aggregator: Aggregator,
    every: Duration,
    max_items_per_category: usize,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        run(bot, state, aggregator, every, max_items_per_category).await;
    })
}

async fn run(
    bot: Bot,
    state: State,
    aggregator: Aggregator,
    every: Duration,
    max_items_per_category: usize,
) {
    info!(interval_secs = every.as_secs(), "Scheduler started");

    let mut ticker = interval(every);
    // If a tick is delayed (e.g. system suspended), skip backlog and resume.
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        ticker.tick().await;
        if let Err(e) = run_once(&bot, &state, &aggregator, max_items_per_category).await {
            error!(error = %e, "Scheduler tick failed");
        }
    }
}

/// Run a single aggregation/distribution cycle. Public so `main` can trigger
/// it manually if needed.
pub async fn run_once(
    bot: &Bot,
    state: &State,
    aggregator: &Aggregator,
    max_items_per_category: usize,
) -> Result<()> {
    let subs = state.subscribers().await;
    if subs.is_empty() {
        info!("Scheduler tick skipped: no subscribers");
        return Ok(());
    }

    info!(subscribers = subs.len(), "Scheduler tick: aggregating");
    let items = aggregator.collect().await;
    if items.is_empty() {
        warn!("Aggregator returned no items; skipping broadcast");
        return Ok(());
    }

    for chat_id in subs {
        let cid = ChatId(chat_id);
        if let Err(e) = send_digest(bot, cid, &items, max_items_per_category, Some(state)).await {
            warn!(chat_id = chat_id, error = %e, "Failed to send digest to subscriber");
        }
    }

    Ok(())
}