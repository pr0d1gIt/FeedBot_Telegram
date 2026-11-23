//! Telegram bot wiring (command parser + dispatcher).

pub mod commands;
pub mod handlers;

use crate::aggregator::Aggregator;
use crate::persistence::State;
use anyhow::Result;
use commands::Command;
use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;
use tracing::error;

/// Build the Telegram dispatcher and run it until shutdown.
///
/// This function returns when the dispatcher stops (e.g. when the process
/// receives Ctrl-C, since teloxide installs its own ctrl-c handler).
pub async fn run(
    bot: Bot,
    state: State,
    aggregator: Aggregator,
    max_items_per_category: usize,
) -> Result<()> {
    // Publish the command list so Telegram clients show them in the menu.
    if let Err(e) = bot.set_my_commands(Command::bot_commands()).await {
        error!(error = %e, "Failed to publish bot commands");
    }

    let handler = Update::filter_message().branch(
        dptree::entry()
            .filter_command::<Command>()
            .endpoint(on_command),
    );

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![state, aggregator, max_items_per_category])
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
) -> Result<(), teloxide::RequestError> {
    if let Err(e) = handlers::handle_command(
        bot.clone(),
        msg.clone(),
        cmd,
        state,
        aggregator,
        max_items_per_category,
    )
    .await
    {
        error!(error = %e, "Command handler failed");
        // Best-effort user-facing error.
        let _ = bot
            .send_message(
                msg.chat.id,
                "⚠️ Se ha producido un error procesando tu comando. Inténtalo de nuevo en unos minutos.",
            )
            .await;
    }
    Ok(())
}