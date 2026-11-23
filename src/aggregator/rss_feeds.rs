//! RSS / Atom feed aggregation.
//!
//! Fetches a fixed list of well-known cybersecurity feeds concurrently with
//! [`reqwest`] and parses the bytes with [`feed_rs`]. Failures from individual
//! feeds never abort the batch: they are logged and skipped.

use crate::models::{NewsItem, Source};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use feed_rs::parser;
use futures::future::join_all;
use reqwest::Client;
use std::time::Duration;
use tracing::{debug, warn};

/// A single RSS/Atom feed source.
#[derive(Debug, Clone, Copy)]
pub struct FeedSource {
    pub name: &'static str,
    pub url: &'static str,
}

/// Default cybersecurity feed list.
pub const DEFAULT_FEEDS: &[FeedSource] = &[
    FeedSource {
        name: "The Hacker News",
        url: "https://feeds.feedburner.com/TheHackersNews",
    },
    FeedSource {
        name: "Bleeping Computer",
        url: "https://www.bleepingcomputer.com/feed/",
    },
    FeedSource {
        name: "SecurityWeek",
        url: "https://feeds.feedburner.com/securityweek",
    },
    FeedSource {
        name: "Krebs on Security",
        url: "https://krebsonsecurity.com/feed/",
    },
    FeedSource {
        name: "CISA Alerts",
        url: "https://www.cisa.gov/cybersecurity-advisories/all.xml",
    },
    FeedSource {
        name: "US-CERT (CISA)",
        url: "https://www.cisa.gov/news.xml",
    },
    FeedSource {
        name: "NVD Recent CVEs",
        url: "https://nvd.nist.gov/pao/nist-rss-feeds/cybersecurity/rss.xml",

    },
    FeedSource {
        name: "NVD Recent Analysis",
        url: "https://www.nist.gov/blogs/cybersecurity-insights/rss.xml",
    },

    FeedSource { name: "Dark Reading", url: "https://www.darkreading.com/rss.xml" },  
    FeedSource { name: "The Record", url: "https://therecord.media/feed" },  
    FeedSource { name: "Ars Technica Security", url: "https://arstechnica.com/security/feed/" },  
    FeedSource { name: "Unit 42 (Palo Alto)", url: "https://unit42.paloaltonetworks.com/feed/" },  
    FeedSource { name: "Talos Intelligence", url: "https://blog.talosintelligence.com/rss/" },  
    FeedSource { name: "Graham Cluley", url: "https://grahamcluley.com/feed/" },
];

/// Aggregator that fetches all configured RSS feeds concurrently.
#[derive(Debug, Clone)]
pub struct RssAggregator {
    client: Client,
    feeds: Vec<FeedSource>,
}

impl RssAggregator {
    /// Construct a new aggregator with the default feed list.
    pub fn new(client: Client) -> Self {
        Self {
            client,
            feeds: DEFAULT_FEEDS.to_vec(),
        }
    }

    /// Override the feed list (mainly for tests).
    #[allow(dead_code)]
    pub fn with_feeds(client: Client, feeds: Vec<FeedSource>) -> Self {
        Self { client, feeds }
    }

    /// Fetch every configured feed concurrently. Items are returned newest-first.
    pub async fn fetch_all(&self) -> Vec<NewsItem> {
        let tasks = self.feeds.iter().map(|feed| {
            let client = self.client.clone();
            let feed = *feed;
            async move {
                match fetch_single(&client, feed).await {
                    Ok(items) => {
                        debug!(feed = feed.name, count = items.len(), "Fetched feed");
                        items
                    }
                    Err(e) => {
                        warn!(feed = feed.name, error = %e, "Failed to fetch feed");
                        Vec::new()
                    }
                }
            }
        });

        let mut items: Vec<NewsItem> = join_all(tasks).await.into_iter().flatten().collect();

        // Sort newest-first; items without a date sink to the bottom.
        items.sort_by(|a, b| {
            b.published
                .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_else(Utc::now))
                .cmp(&a.published.unwrap_or_else(|| {
                    DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_else(Utc::now)
                }))
        });
        items
    }
}

async fn fetch_single(client: &Client, source: FeedSource) -> Result<Vec<NewsItem>> {
    let resp = client
        .get(source.url)
        .header(
            reqwest::header::ACCEPT,
            "application/rss+xml, application/atom+xml, application/xml, text/xml;q=0.9, */*;q=0.5",
        )
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .with_context(|| format!("GET {}", source.url))?;

    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("{} returned HTTP {}", source.url, status);
    }

    let bytes = resp
        .bytes()
        .await
        .with_context(|| format!("read body for {}", source.url))?;

    let feed =
        parser::parse(bytes.as_ref()).with_context(|| format!("parse feed {}", source.url))?;

    let items = feed
        .entries
        .into_iter()
        .filter_map(|entry| convert_entry(entry, &source))
        .collect();

    Ok(items)
}

fn convert_entry(entry: feed_rs::model::Entry, source: &FeedSource) -> Option<NewsItem> {
    // Pick the first usable link.
    let url = entry
        .links
        .into_iter()
        .map(|l| l.href)
        .find(|h| !h.trim().is_empty())?;

    let title = entry.title.map(|t| t.content).unwrap_or_default();
    let title = clean_text(&title);
    if title.is_empty() {
        return None;
    }

    let summary = entry
        .summary
        .map(|s| s.content)
        .or_else(|| entry.content.and_then(|c| c.body))
        .map(|raw| clean_text(&raw))
        .filter(|s| !s.is_empty());

    let published = entry.published.or(entry.updated);

    // Prefer canonical URL as ID; fall back to feed-provided id.
    let id = if !entry.id.is_empty() && entry.id != url {
        format!("{}::{}", source.name, entry.id)
    } else {
        url.clone()
    };

    Some(NewsItem {
        id,
        title,
        url,
        summary,
        source_name: source.name.to_string(),
        source_kind: Source::Rss,
        published,
    })
}

/// Strip HTML tags and collapse whitespace. Cheap and deliberately non-strict;
/// the goal is human-readable preview text, not perfect HTML parsing.
fn clean_text(input: &str) -> String {
    // Decode HTML entities first.
    let decoded = html_escape::decode_html_entities(input);

    let mut out = String::with_capacity(decoded.len());
    let mut in_tag = false;
    for ch in decoded.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }

    // Collapse whitespace.
    let mut result = String::with_capacity(out.len());
    let mut prev_space = false;
    for ch in out.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                result.push(' ');
            }
            prev_space = true;
        } else {
            result.push(ch);
            prev_space = false;
        }
    }
    result.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_text_strips_html_and_entities() {
        let raw = "<p>Hello <b>world</b> &amp; everyone</p>";
        assert_eq!(clean_text(raw), "Hello world & everyone");
    }

    #[test]
    fn clean_text_collapses_whitespace() {
        let raw = "Line\n\nwith   lots\tof   space";
        assert_eq!(clean_text(raw), "Line with lots of space");
    }
}