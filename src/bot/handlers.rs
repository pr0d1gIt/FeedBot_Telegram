//! Telegram message handlers and digest rendering helpers.

use crate::aggregator::Aggregator;
use crate::bot::commands::Command;
use crate::models::{CategorisedItem, Category};
use crate::persistence::State;
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use teloxide::prelude::*;
use teloxide::types::{LinkPreviewOptions, ParseMode};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

/// Telegram hard limit on message size (in characters).
const TELEGRAM_MAX_CHARS: usize = 4000;

/// Minimum time a non-admin user must wait between `/latest` requests.
const LATEST_COOLDOWN: Duration = Duration::from_secs(15);

/// Minimum time between any two commands to detect rapid button/command flooding.
const GENERAL_COOLDOWN: Duration = Duration::from_secs(2);

/// Number of cooldown violations before automatically banning the user.
const MAX_SPAM_STRIKES: u8 = 3;

#[derive(Debug, Default)]
struct UserRateEntry {
    last_command: Option<Instant>,
    last_latest: Option<Instant>,
    strikes: u8,
    warned_in_window: bool,
}

#[derive(Debug, Clone, Default)]
pub struct RateLimiter {
    inner: Arc<Mutex<HashMap<i64, UserRateEntry>>>,
}

enum RateDecision {
    Allow,
    Warn(String),
    AutoBan,
}

impl RateLimiter {
    async fn check(&self, chat_id: i64, is_latest: bool) -> RateDecision {
        let mut map = self.inner.lock().await;
        let entry = map.entry(chat_id).or_default();
        let now = Instant::now();

        if is_latest {
            if let Some(prev) = entry.last_latest {
                let elapsed = now.duration_since(prev);
                if elapsed < LATEST_COOLDOWN {
                    entry.strikes += 1;
                    if entry.strikes >= MAX_SPAM_STRIKES {
                        return RateDecision::AutoBan;
                    }
                    let remaining = (LATEST_COOLDOWN - elapsed).as_secs().max(1);
                    return RateDecision::Warn(format!(
                        "⏳ Espera {remaining}s antes de volver a pedir /latest. El spam repetido conlleva bloqueo automático."
                    ));
                }
            }
            entry.last_latest = Some(now);
            entry.last_command = Some(now);
            entry.strikes = 0;
            entry.warned_in_window = false;
            return RateDecision::Allow;
        }

        if let Some(prev) = entry.last_command {
            if now.duration_since(prev) < GENERAL_COOLDOWN {
                entry.strikes += 1;
                if entry.strikes >= MAX_SPAM_STRIKES + 2 {
                    return RateDecision::AutoBan;
                }
                if !entry.warned_in_window {
                    entry.warned_in_window = true;
                    return RateDecision::Warn(
                        "⏳ Por favor, no envíes comandos tan rápido.".to_string(),
                    );
                }
                return RateDecision::Warn(String::new());
            }
        }

        entry.last_command = Some(now);
        entry.strikes = 0;
        entry.warned_in_window = false;
        RateDecision::Allow
    }
}

/// Entry point dispatched by teloxide for every incoming command.
#[allow(clippy::too_many_arguments)]
pub async fn handle_command(
    bot: Bot,
    msg: Message,
    cmd: Command,
    state: State,
    aggregator: Aggregator,
    max_items: usize,
    admin_ids: Arc<HashSet<i64>>,
    rate_limiter: RateLimiter,
) -> Result<()> {
    let chat_id = msg.chat.id;
    let is_admin = admin_ids.contains(&chat_id.0);

    // Silently ignore banned non-admin users so they cannot consume bot resources.
    if !is_admin && state.is_banned(chat_id.0).await {
        warn!(chat = chat_id.0, "Ignored command from banned user");
        return Ok(());
    }

    // Record user metadata (username / first_name) for admin listings.
    let (username, first_name) = msg
        .from
        .as_ref()
        .map(|u| (u.username.clone(), Some(u.first_name.clone())))
        .unwrap_or((None, None));
    let _ = state.upsert_profile(chat_id.0, username, first_name).await;

    // Enforce anti-spam rate limits for non-admin users.
    if !is_admin {
        let is_latest = matches!(cmd, Command::Latest);
        match rate_limiter.check(chat_id.0, is_latest).await {
            RateDecision::Allow => {}
            RateDecision::Warn(warning_msg) => {
                warn!(chat = chat_id.0, command = ?cmd, "Rate limited user");
                if !warning_msg.is_empty() {
                    let _ = bot.send_message(chat_id, warning_msg).await;
                }
                return Ok(());
            }
            RateDecision::AutoBan => {
                warn!(chat = chat_id.0, "Auto-banning user due to spam");
                state.ban_user(chat_id.0).await?;
                let label = state.user_label(chat_id.0).await;
                let _ = bot
                    .send_message(
                        chat_id,
                        "🚫 Has sido bloqueado automáticamente por uso abusivo / spam.",
                    )
                    .await;
                notify_admins(
                    &bot,
                    &admin_ids,
                    &format!("🚨 Usuario bloqueado automáticamente por spam:\n• {label}"),
                )
                .await;
                return Ok(());
            }
        }
    }

    info!(chat = chat_id.0, admin = is_admin, command = ?cmd, "Received command");

    match cmd {
        Command::Start | Command::Help => {
            let text = welcome_message(is_admin);
            send_markdown_v2(&bot, chat_id, &text).await?;
        }
        Command::Latest => {
            let _ = bot
                .send_message(
                    chat_id,
                    "⏳ Buscando las últimas publicaciones de investigación, dame unos segundos...",
                )
                .await;

            let items = aggregator.collect().await;
            if items.is_empty() {
                let _ = bot
                    .send_message(
                        chat_id,
                        "⚠️ No se han podido recuperar publicaciones en este momento. Inténtalo de nuevo en unos minutos.",
                    )
                    .await;
                return Ok(());
            }

            send_digest(&bot, chat_id, &items, max_items, None).await?;
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
        Command::Users => {
            if !ensure_admin(&bot, chat_id, is_admin).await {
                return Ok(());
            }
            let subs = state.subscribers().await;
            let banned = state.banned_users().await;

            let mut out = format!("👥 Usuarios suscritos ({}):\n", subs.len());
            if subs.is_empty() {
                out.push_str("  (ninguno)\n");
            } else {
                for id in subs {
                    let label = state.user_label(id).await;
                    out.push_str(&format!("  • {label}\n"));
                }
            }

            out.push_str(&format!("\n🚫 Usuarios bloqueados ({}):\n", banned.len()));
            if banned.is_empty() {
                out.push_str("  (ninguno)\n");
            } else {
                for id in banned {
                    let label = state.user_label(id).await;
                    out.push_str(&format!("  • {label}\n"));
                }
            }

            bot.send_message(chat_id, out).await?;
        }
        Command::Adduser(arg) => {
            if !ensure_admin(&bot, chat_id, is_admin).await {
                return Ok(());
            }
            let Some(target_id) = parse_target_id(&arg) else {
                bot.send_message(chat_id, "Uso: /adduser <id_numerico>").await?;
                return Ok(());
            };
            state.unban_user(target_id).await?;
            let added = state.add_subscriber(target_id).await?;
            let label = state.user_label(target_id).await;
            let reply = if added {
                format!("✅ Usuario añadido y suscrito: {label}")
            } else {
                format!("ℹ️ El usuario ya estaba suscrito (y desbloqueado si aplicaba): {label}")
            };
            bot.send_message(chat_id, reply).await?;
        }
        Command::Deluser(arg) => {
            if !ensure_admin(&bot, chat_id, is_admin).await {
                return Ok(());
            }
            let Some(target_id) = parse_target_id(&arg) else {
                bot.send_message(chat_id, "Uso: /deluser <id_numerico>").await?;
                return Ok(());
            };
            if admin_ids.contains(&target_id) {
                bot.send_message(chat_id, "⚠️ No puedes bloquear a otro administrador.")
                    .await?;
                return Ok(());
            }
            state.ban_user(target_id).await?;
            let label = state.user_label(target_id).await;
            bot.send_message(
                chat_id,
                format!("🗑️ Usuario eliminado de suscripciones y bloqueado: {label}"),
            )
            .await?;
        }
        Command::Unban(arg) => {
            if !ensure_admin(&bot, chat_id, is_admin).await {
                return Ok(());
            }
            let Some(target_id) = parse_target_id(&arg) else {
                bot.send_message(chat_id, "Uso: /unban <id_numerico>").await?;
                return Ok(());
            };
            let unbanned = state.unban_user(target_id).await?;
            let label = state.user_label(target_id).await;
            let reply = if unbanned {
                format!("🔓 Usuario desbloqueado: {label}\n(Puede volver a suscribirse con /subscribe)")
            } else {
                format!("ℹ️ Ese usuario no estaba en la lista de bloqueados: {label}")
            };
            bot.send_message(chat_id, reply).await?;
        }
    }

    Ok(())
}

async fn ensure_admin(bot: &Bot, chat_id: ChatId, is_admin: bool) -> bool {
    if !is_admin {
        let _ = bot
            .send_message(chat_id, "⛔ No tienes permisos para ejecutar este comando.")
            .await;
    }
    is_admin
}

fn parse_target_id(raw: &str) -> Option<i64> {
    raw.split_whitespace().next()?.parse::<i64>().ok()
}

async fn notify_admins(bot: &Bot, admin_ids: &HashSet<i64>, text: &str) {
    for &admin_id in admin_ids {
        let _ = bot.send_message(ChatId(admin_id), text).await;
    }
}

/// Welcome / help text used by `/start` and `/help`.
pub fn welcome_message(is_admin: bool) -> String {
    let mut text = "*Bot de Inteligencia y Ciberseguridad*\n\n\
     Te mantengo al día con publicaciones técnicas de equipos de investigación, CERTs y laboratorios: \
     0\\-days, vulnerabilidades, POCs verificados e indicadores de compromiso\\.\n\n\
     *Comandos disponibles*\n\
     /latest \\– Resumen bajo demanda\\.\n\
     /subscribe \\– Recibir resúmenes automáticos cada 12 h\\.\n\
     /unsubscribe \\– Detener los resúmenes automáticos\\.\n\
     /status \\– Ver el estado de tu suscripción\\.\n\
     /help \\– Mostrar esta ayuda\\.\n\n\
     *Fuentes*: CISA, NVD, CERT/CC, INCIBE\\-CERT, Google Project Zero, PortSwigger, Trail of Bits, \
     watchTowr, ZDI, Unit 42, Cisco Talos, Mandiant, Securelist, SentinelLabs, SANS ISC, The DFIR Report \
     y GitHub Advisory DB\\."
        .to_string();

    if is_admin {
        text.push_str(
            "\n\n*Comandos de Administración*\n\
             /users \\– Listar usuarios suscritos y bloqueados\\.\n\
             /adduser `<id>` \\– Suscribir manualmente a un usuario\\.\n\
             /deluser `<id>` \\– Eliminar y bloquear a un usuario\\.\n\
             /unban `<id>` \\– Desbloquear a un usuario\\.",
        );
    }

    text.push_str(
        "\n\n⚠️ Este bot reúne información técnica pública\\. Verifica siempre las fuentes originales antes de actuar\\.",
    );
    text
}

pub async fn send_digest(
    bot: &Bot,
    chat_id: ChatId,
    items: &[CategorisedItem],
    max_items_per_category: usize,
    mark_sent: Option<&State>,
) -> Result<()> {
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

pub fn render_digest(items: &[&CategorisedItem], max_per_category: usize) -> Vec<String> {
    let now: DateTime<Utc> = Utc::now();
    let header = format!(
        "*Cybersec Digest*\n_{}_\n",
        escape_md(&now.format("%Y-%m-%d %H:%M UTC").to_string()),
    );

    let mut by_cat: HashMap<Category, Vec<&CategorisedItem>> = HashMap::new();
    for ci in items {
        by_cat.entry(ci.category).or_default().push(ci);
    }

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

fn strip_markdown(s: &str) -> String {
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