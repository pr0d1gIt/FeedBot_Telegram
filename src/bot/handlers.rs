//! Telegram message handlers and digest rendering helpers.
//!
//! The handlers convert incoming commands into bot actions, while the
//! [`render_digest`] / [`send_digest`] helpers are reused by both the
//! `/latest` command and the periodic scheduler.

use crate::aggregator::Aggregator;
use crate::bot::commands::Command;
use crate::models::{CategorisedItem, Category};
use crate::persistence::State;
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use teloxide::prelude::*;
use teloxide::types::{LinkPreviewOptions, ParseMode};
use tracing::{error, info, warn};

/// Telegram hard limit on message size (in characters). We keep a small margin.
const TELEGRAM_MAX_CHARS: usize = 4000;

/// Entry point dispatched by teloxide for every incoming command.
pub async fn handle_command(
    bot: Bot,
    msg: Message,
    cmd: Command,
    state: State,
    aggregator: Aggregator,
    max_items: usize,
) -> Result<()> {
    let chat_id = msg.chat.id;
    info!(chat = chat_id.0, command = ?cmd, "Received command");

    match cmd {
        Command::Start | Command::Help => {
            let text = welcome_message();
            send_markdown_v2(&bot, chat_id, &text).await?;
        }
        Command::Latest => {
            // Acknowledge immediately because aggregation may take 10–20s.
            let _ = bot
                .send_message(
                    chat_id,
                    "⏳ Buscando las últimas noticias, dame unos segundos...",
                )
                .await;

            let items = aggregator.collect().await;
            if items.is_empty() {
                let _ = bot
                    .send_message(
                        chat_id,
                        "⚠️ No se han podido recuperar noticias en este momento. Inténtalo de nuevo en unos minutos.",
                    )
                    .await;
                return Ok(());
            }

            // /latest does *not* deduplicate against previously sent items: the
            // user explicitly asked, so we honour the request even with repeats.
            send_digest(&bot, chat_id, &items, max_items, /* mark_sent: */ None).await?;
        }
        Command::Subscribe => {
            let newly_added = state.add_subscriber(chat_id.0).await?;
            let text = if newly_added {
                "Te has *suscrito* a las notificaciones automáticas\\. Recibirás un resumen cada 12 horas\\. Usa /unsubscribe para detenerlas\\."
            } else {
                "Ya estabas suscrito\\. Usa /unsubscribe si quieres detener las notificaciones\\."
            };
            send_markdown_v2(&bot, chat_id, text).await?;
        }
        Command::Unsubscribe => {
            let removed = state.remove_subscriber(chat_id.0).await?;
            let text = if removed {
                "Notificaciones automáticas *desactivadas*\\. Usa /subscribe para volver a activarlas\\."
            } else {
                "No estabas suscrito\\. Usa /subscribe para activar las notificaciones\\."
            };
            send_markdown_v2(&bot, chat_id, text).await?;
        }
        Command::Status => {
            let subs = state.subscribers().await;
            let subscribed = subs.contains(&chat_id.0);
            let text = format!(
                "*Estado de la suscripción*\n\n\
                 • Tu chat: `{}`\n\
                 • Suscrito: {}\n\
                 • Total de usuarios suscritos: *{}*",
                chat_id.0,
                if subscribed { "sí" } else { "no" },
                subs.len()
            );
            send_markdown_v2(&bot, chat_id, &text).await?;
        }
    }

    Ok(())
}

/// Welcome / help text used by `/start` and `/help`.
///
/// Pre-escaped for Telegram MarkdownV2.
pub fn welcome_message() -> String {
    "*Bot de Ciberseguridad*\n\n\
     Te mantengo al día con noticias profesionales de ciberseguridad: \
     ataques recientes, vulnerabilidades, POCs públicos e indicadores de compromiso\\.\n\n\
     *Comandos disponibles*\n\
     /latest \\– Resumen bajo demanda\\.\n\
     /subscribe \\– Recibir resúmenes automáticos cada 12 h\\.\n\
     /unsubscribe \\– Detener los resúmenes automáticos\\.\n\
     /status \\– Ver el estado de tu suscripción\\.\n\
     /help \\– Mostrar esta ayuda\\.\n\n\
     *Fuentes*: The Hacker News, Bleeping Computer, SecurityWeek, Krebs on Security, \
     CISA, NVD y búsqueda en GitHub \\(POCs / exploits\\)\\.\n\n\
     ⚠️ Este bot reúne información pública\\. Verifica siempre las fuentes originales antes de actuar\\."
        .to_string()
}

/// Send a categorised digest to `chat_id`, splitting it into multiple messages
/// if it exceeds Telegram's per-message size limit.
///
/// When `mark_sent` is `Some(state)`, items whose IDs are already in the state
/// are filtered out and the newly delivered IDs are persisted afterwards.
pub async fn send_digest(
    bot: &Bot,
    chat_id: ChatId,
    items: &[CategorisedItem],
    max_items_per_category: usize,
    mark_sent: Option<&State>,
) -> Result<()> {
    // Filter out already-delivered items when a state is supplied.
    let filtered: Vec<&CategorisedItem> = if let Some(state) = mark_sent {
        let mut out = Vec::with_capacity(items.len());
        for ci in items {
            if !state.was_sent(&ci.item.id).await {
                out.push(ci);
            }
        }
        out
    } else {
        items.iter().collect()
    };

    if filtered.is_empty() {
        let _ = bot
            .send_message(
                chat_id,
                "📭 No hay novedades desde el último envío. Estás al día.",
            )
            .await;
        return Ok(());
    }

    let messages = render_digest(&filtered, max_items_per_category);

    let mut sent_ids: Vec<String> = Vec::new();
    for (idx, msg) in messages.iter().enumerate() {
        let res = bot
            .send_message(chat_id, msg)
            .parse_mode(ParseMode::MarkdownV2)
            .link_preview_options(LinkPreviewOptions {
                is_disabled: true,
                url: None,
                prefer_small_media: false,
                prefer_large_media: false,
                show_above_text: false,
            })
            .await;

        if let Err(e) = res {
            warn!(
                error = %e,
                part = idx + 1,
                "Failed to send digest message; falling back to plain text"
            );
            // Last-ditch attempt: send unformatted so the user still gets the info.
            let plain = strip_markdown(msg);
            if let Err(e2) = bot.send_message(chat_id, plain).await {
                error!(error = %e2, "Plain-text fallback failed too; aborting digest");
                return Err(e2.into());
            }
        }
    }

    if let Some(state) = mark_sent {
        for ci in &filtered {
            sent_ids.push(ci.item.id.clone());
        }
        if !sent_ids.is_empty() {
            state.mark_sent_batch(sent_ids).await?;
        }
    }

    Ok(())
}

/// Render a list of categorised items into one or more Telegram-MarkdownV2 strings,
/// each at most [`TELEGRAM_MAX_CHARS`] long.
pub fn render_digest(items: &[&CategorisedItem], max_per_category: usize) -> Vec<String> {
    let now: DateTime<Utc> = Utc::now();
    let header = format!(
        "*Cybersec Digest*\n_{}_\n",
        escape_md(&now.format("%Y-%m-%d %H:%M UTC").to_string()),
    );

    // Group by category, preserving the global newest-first order.
    let mut by_cat: HashMap<Category, Vec<&CategorisedItem>> = HashMap::new();
    for ci in items {
        by_cat.entry(ci.category).or_default().push(ci);
    }

    // Build sections in the canonical order.
    let mut sections: Vec<String> = Vec::new();
    for cat in Category::ordered() {
        let Some(list) = by_cat.get(&cat) else {
            continue;
        };
        if list.is_empty() {
            continue;
        }

        let mut section = String::new();
        section.push_str(&format!("\n*{}*\n", escape_md(cat.header())));

        for ci in list.iter().take(max_per_category) {
            section.push_str(&format_item(ci));
        }

        let extra = list.len().saturating_sub(max_per_category);
        if extra > 0 {
            section.push_str(&format!("_\\.\\.\\.y {} más_\n", extra));
        }
        sections.push(section);
    }

    if sections.is_empty() {
        return vec![format!("{header}\n_Sin novedades._")];
    }

    // Pack sections into messages of at most TELEGRAM_MAX_CHARS chars.
    let mut messages = Vec::new();
    let mut current = header.clone();
    for section in sections {
        if current.chars().count() + section.chars().count() > TELEGRAM_MAX_CHARS {
            messages.push(std::mem::take(&mut current));
            current.push_str(&header);
        }
        current.push_str(&section);
    }
    if !current.is_empty() {
        messages.push(current);
    }

    messages
}

fn format_item(ci: &CategorisedItem) -> String {
    let title = escape_md(&truncate(&ci.item.title, 180));
    let url = escape_md_url(&ci.item.url);
    let source = escape_md(&ci.item.source_name);

    let date = ci
        .item
        .published
        .map(|d| escape_md(&d.format("%Y-%m-%d %H:%M UTC").to_string()))
        .unwrap_or_else(|| escape_md("fecha desconocida"));

    let mut line = format!("• [{}]({}) — _{}_, {}\n", title, url, source, date);

    if let Some(summary) = &ci.item.summary {
        let snippet = truncate(summary, 220);
        if !snippet.trim().is_empty() {
            line.push_str(&format!("  {}\n", escape_md(&snippet)));
        }
    }
    line
}

/// Escape every Telegram MarkdownV2 reserved character.
///
/// Reserved set per the official spec:
/// `_ * [ ] ( ) ~ \` > # + - = | { } . !`
fn escape_md(input: &str) -> String {
    const RESERVED: &[char] = &[
        '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!',
        '\\',
    ];
    let mut out = String::with_capacity(input.len() + 8);
    for ch in input.chars() {
        if RESERVED.contains(&ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Escape special characters that have meaning *inside* a MarkdownV2 link URL,
/// namely `)` and `\`.
fn escape_md_url(url: &str) -> String {
    let mut out = String::with_capacity(url.len() + 4);
    for ch in url.chars() {
        if ch == ')' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Best-effort plain-text fallback if MarkdownV2 sending fails.
fn strip_markdown(s: &str) -> String {
    // Drop unambiguously-formatting characters, then normalise parens for
    // readability. Parens are handled separately because we want them kept,
    // just spaced out.
    let stripped: String = s
        .chars()
        .filter(|c| !matches!(c, '\\' | '*' | '_' | '[' | ']'))
        .collect();
    stripped.replace('(', " (").replace(')', ") ")
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let cut: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// Send a single MarkdownV2-formatted message, with a plain-text fallback if
/// Telegram rejects the formatting.
async fn send_markdown_v2(bot: &Bot, chat_id: ChatId, text: &str) -> Result<()> {
    let res = bot
        .send_message(chat_id, text)
        .parse_mode(ParseMode::MarkdownV2)
        .await;
    if let Err(e) = res {
        warn!(error = %e, "MarkdownV2 send failed; falling back to plain text");
        bot.send_message(chat_id, strip_markdown(text)).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Category, NewsItem, Source};

    fn make_ci(title: &str, cat: Category) -> CategorisedItem {
        CategorisedItem {
            item: NewsItem {
                id: title.to_string(),
                title: title.to_string(),
                url: "https://example.com/post?id=1".to_string(),
                summary: Some("A test summary.".to_string()),
                source_name: "Test".to_string(),
                source_kind: Source::Rss,
                published: None,
            },
            category: cat,
        }
    }

    #[test]
    fn render_digest_produces_at_least_one_message() {
        let items = [
            make_ci("Critical RCE in WidgetOS", Category::Vulnerabilities),
            make_ci("Public PoC for CVE-2024-1234", Category::Pocs),
        ];
        let refs: Vec<&CategorisedItem> = items.iter().collect();
        let msgs = render_digest(&refs, 8);
        assert!(!msgs.is_empty());
        // Each message must be within Telegram's hard limit.
        for m in &msgs {
            assert!(m.chars().count() <= 4096, "message exceeds Telegram limit");
        }
    }

    #[test]
    fn escape_md_escapes_reserved() {
        let escaped = escape_md("Hello (world)!");
        assert_eq!(escaped, "Hello \\(world\\)\\!");
    }
}