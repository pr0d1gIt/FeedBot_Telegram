//! GitHub repository search for newly-published POCs / exploits.
//!
//! Uses the public REST search endpoint (`GET /search/repositories`) which is
//! free to call without authentication (60 req/h unauthenticated, 5000 req/h
//! with a PAT). Each query is scoped to repositories created in the last 7
//! days and sorted by creation date so we surface fresh material.

use crate::models::{NewsItem, Source};
use anyhow::{Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures::future::join_all;
use reqwest::{header, Client};
use serde::Deserialize;
use std::time::Duration;
use tracing::{debug, warn};

const GITHUB_SEARCH_URL: &str = "https://api.github.com/search/repositories";
const QUERIES: &[&str] = &["POC", "exploit", "CVE"];
const MAX_RESULTS_PER_QUERY: usize = 10;

/// GitHub repo search aggregator.
#[derive(Debug, Clone)]
pub struct GitHubAggregator {
    client: Client,
    token: Option<String>,
}

impl GitHubAggregator {
    pub fn new(client: Client, token: Option<String>) -> Self {
        Self { client, token }
    }

    /// Run all configured queries concurrently and merge their results.
    pub async fn fetch_all(&self) -> Vec<NewsItem> {
        let since = Utc::now() - ChronoDuration::days(7);
        let date_filter = since.format("%Y-%m-%d").to_string();

        let tasks = QUERIES.iter().map(|q| {
            let client = self.client.clone();
            let token = self.token.clone();
            let date_filter = date_filter.clone();
            async move {
                match search(&client, token.as_deref(), q, &date_filter).await {
                    Ok(items) => {
                        debug!(query = q, count = items.len(), "GitHub search OK");
                        items
                    }
                    Err(e) => {
                        warn!(query = q, error = %e, "GitHub search failed");
                        Vec::new()
                    }
                }
            }
        });

        let mut items: Vec<NewsItem> = join_all(tasks).await.into_iter().flatten().collect();

        // Deduplicate by URL (different queries can match the same repo).
        items.sort_by(|a, b| a.url.cmp(&b.url));
        items.dedup_by(|a, b| a.url == b.url);

        // Sort newest-first.
        items.sort_by_key(|item| std::cmp::Reverse(item.published));
        items
    }
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    items: Vec<Repo>,
}

#[derive(Debug, Deserialize)]
struct Repo {
    full_name: String,
    html_url: String,
    description: Option<String>,
    created_at: DateTime<Utc>,
    #[serde(default)]
    stargazers_count: u64,
}

async fn search(
    client: &Client,
    token: Option<&str>,
    query: &str,
    since_date: &str,
) -> Result<Vec<NewsItem>> {
    // `in:name,description created:>=YYYY-MM-DD` => fresh repos mentioning the term.
    let q = format!("{query} in:name,description created:>={since_date}");

    let mut req = client
        .get(GITHUB_SEARCH_URL)
        .query(&[
            ("q", q.as_str()),
            ("sort", "updated"),
            ("order", "desc"),
            ("per_page", "30"),
        ])
        .header(header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .timeout(Duration::from_secs(20));

    if let Some(t) = token {
        req = req.bearer_auth(t);
    }

    let resp = req.send().await.context("GitHub search request")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("GitHub returned HTTP {status}: {body}");
    }

    let parsed: SearchResponse = resp
        .json()
        .await
        .context("Failed to deserialise GitHub search response")?;

    let items = parsed
        .items
        .into_iter()
        .take(MAX_RESULTS_PER_QUERY)
        .map(|r| {
            let summary = match (&r.description, r.stargazers_count) {
                (Some(d), 0) => Some(d.clone()),
                (Some(d), n) => Some(format!("{d} (⭐ {n})")),
                (None, 0) => None,
                (None, n) => Some(format!("⭐ {n}")),
            };
            NewsItem {
                id: r.html_url.clone(),
                title: format!("GitHub: {}", r.full_name),
                url: r.html_url,
                summary,
                source_name: "GitHub".to_string(),
                source_kind: Source::GitHub,
                published: Some(r.created_at),
            }
        })
        .collect();

    Ok(items)
}