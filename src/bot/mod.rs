//! Telegram bot wiring (command parser + dispatcher).

pub mod commands;
pub mod handlers;

use crate::aggregator::Aggregator;
use crate::persistence::State;
use anyhow::Result;
use commands::Command;
use handlers::RateLimiter;
use std::collections::HashSet;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;
use tracing::error;

/// Build the Telegram dispatcher and run it until shutdown.
pub async fn run(
    bot: Bot,
    state: State,
    aggregator: Aggregator,
    max_items_per_category: usize,
    admin_ids: HashSet<i64>,
) -> Result<()> {
    // Publish only public commands to the Telegram client menu.
    let public_commands: Vec<_> = Command::bot_commands()
        .into_iter()
        .filter(|c| !matches!(c.command.as_str(), "/users" | "/adduser" | "/deluser" | "/unban"))
        .collect();

    if let Err(e) = bot.set_my_commands(public_commands).await {
        error!(error = %e, "Failed to publish bot commands");
    }

    let rate_limiter = RateLimiter::default();
    let admin_ids = Arc::new(admin_ids);

    let handler = Update::filter_message().branch(
        dptree::entry()
            .filter_command::<Command>()
            .endpoint(on_command),
    );

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![
            state,
            aggregator,
            max_items_per_category,
            admin_ids,
            rate_limiter
        ])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    Ok(())
}

async fn on_command(
    bot: Bot,
    msg: Message,
    cmd: Command,
    state: State,
    aggregator: Aggregator,
    max_items_per_category: usize,
    admin_ids: Arc<HashSet<i64>>,
    rate_limiter: RateLimiter,
) -> Result<(), teloxide::RequestError> {
    if let Err(e) = handlers::handle_command(
        bot.clone(),
        msg.clone(),
        cmd,
        state,
        aggregator,
        max_items_per_category,
        admin_ids,
        rate_limiter,
    )
    .await
    {
        error!(error = %e, "Command handler failed");
        let _ = bot
            .send_message(
                msg.chat.id,
                "⚠️ Se ha producido un error procesando tu comando. Inténtalo de nuevo en unos minutos.",
            )
            .await;
    }
    Ok(())
}