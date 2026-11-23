//! Aggregation layer: pulls news from RSS and GitHub, categorises results.

pub mod categorizer;
pub mod github_search;
pub mod rss_feeds;

use crate::config::Config;
use crate::models::{CategorisedItem, NewsItem};
use anyhow::{Context, Result};
use reqwest::Client;
use std::time::Duration;
use tracing::info;

/// High-level façade combining all aggregation sources.
#[derive(Debug, Clone)]
pub struct Aggregator {
    rss: rss_feeds::RssAggregator,
    github: github_search::GitHubAggregator,
}

impl Aggregator {
    /// Build a new aggregator using the supplied [`Config`].
    pub fn new(config: &Config) -> Result<Self> {
        let client = build_http_client(config).context("Failed to build HTTP client")?;
        Ok(Self {
            rss: rss_feeds::RssAggregator::new(client.clone()),
            github: github_search::GitHubAggregator::new(client, config.github_token.clone()),
        })
    }

    /// Fetch every source concurrently, merge and categorise the results.
    pub async fn collect(&self) -> Vec<CategorisedItem> {
        let (rss_items, gh_items) = tokio::join!(self.rss.fetch_all(), self.github.fetch_all());

        info!(
            rss = rss_items.len(),
            github = gh_items.len(),
            "Aggregation complete"
        );

        let mut items: Vec<NewsItem> = Vec::with_capacity(rss_items.len() + gh_items.len());
        items.extend(rss_items);
        items.extend(gh_items);

        categorizer::categorise_all(items)
    }
}

fn build_http_client(config: &Config) -> Result<Client> {
    Client::builder()
        .user_agent(&config.user_agent)
        .timeout(config.http_timeout)
        .connect_timeout(Duration::from_secs(10))
        .build()
        .context("reqwest client build")
}