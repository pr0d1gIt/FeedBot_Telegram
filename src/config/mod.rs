//! Application configuration loaded from environment variables.

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

/// Centralised, immutable runtime configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Telegram bot token issued by `@BotFather`.
    pub telegram_token: String,

    /// Telegram user/chat IDs allowed to run admin commands and exempt from rate limits.
    pub admin_ids: HashSet<i64>,

    /// Optional GitHub Personal Access Token.
    pub github_token: Option<String>,

    /// Interval between automatic digest runs.
    pub schedule_interval: Duration,

    /// Maximum number of items per category included in a single digest.
    pub max_items_per_category: usize,

    /// Path to the JSON file used for persistence (subscribers + dedup state).
    pub state_path: PathBuf,

    /// HTTP request timeout for outbound RSS / GitHub calls.
    pub http_timeout: Duration,

    /// User-Agent header used in outbound HTTP requests.
    pub user_agent: String,
}

impl Config {
    /// Load configuration from the process environment.
    pub fn from_env() -> Result<Self> {
        let _ = dotenvy::dotenv();

        let telegram_token = std::env::var("TELEGRAM_BOT_TOKEN").context(
            "TELEGRAM_BOT_TOKEN is not set. Create a bot via @BotFather and put the token in .env",
        )?;

        if telegram_token.trim().is_empty() {
            anyhow::bail!("TELEGRAM_BOT_TOKEN is empty");
        }

        let admin_ids = parse_admin_ids("ADMIN_IDS")?;

        let github_token = std::env::var("GITHUB_TOKEN")
            .ok()
            .filter(|t| !t.trim().is_empty());

        let schedule_interval =
            Duration::from_secs(parse_env_u64("SCHEDULE_INTERVAL_SECS", 12 * 60 * 60)?);

        let max_items_per_category = parse_env_usize("MAX_ITEMS_PER_CATEGORY", 8)?;

        let http_timeout = Duration::from_secs(parse_env_u64("HTTP_TIMEOUT_SECS", 20)?);

        let state_path: PathBuf = std::env::var("STATE_PATH")
            .unwrap_or_else(|_| "state.json".to_string())
            .into();

        let user_agent = std::env::var("USER_AGENT").unwrap_or_else(|_| {
            concat!(
                "telegram-cybersec-bot/",
                env!("CARGO_PKG_VERSION"),
                " (+https://github.com/)",
            )
            .to_string()
        });

        Ok(Self {
            telegram_token,
            admin_ids,
            github_token,
            schedule_interval,
            max_items_per_category,
            state_path,
            http_timeout,
            user_agent,
        })
    }
}

fn parse_admin_ids(key: &str) -> Result<HashSet<i64>> {
    let mut ids = HashSet::new();
    let Ok(raw) = std::env::var(key) else {
        return Ok(ids);
    };
    for part in raw.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        let id = trimmed
            .parse::<i64>()
            .with_context(|| format!("{key} contains an invalid integer ID: `{trimmed}`"))?;
        ids.insert(id);
    }
    Ok(ids)
}

fn parse_env_u64(key: &str, default: u64) -> Result<u64> {
    match std::env::var(key) {
        Ok(v) => v
            .parse::<u64>()
            .with_context(|| format!("{key} must be a non-negative integer; got `{v}`")),
        Err(_) => Ok(default),
    }
}

fn parse_env_usize(key: &str, default: usize) -> Result<usize> {
    match std::env::var(key) {
        Ok(v) => v
            .parse::<usize>()
            .with_context(|| format!("{key} must be a non-negative integer; got `{v}`")),
        Err(_) => Ok(default),
    }
}