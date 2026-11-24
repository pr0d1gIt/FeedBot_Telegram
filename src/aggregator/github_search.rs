use crate::models::{NewsItem, Source};
use anyhow::{Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures::future::join_all;
use reqwest::{header, Client};
use serde::Deserialize;
use std::time::Duration;
use tracing::{debug, warn};

const GITHUB_SEARCH_URL: &str = "https://api.github.com/search/repositories";
const GITHUB_ADVISORIES_URL: &str = "https://api.github.com/advisories";
const QUERIES: &[&str] = &["CVE-2025", "CVE-2026", "exploit", "nuclei-template"];
const MAX_RESULTS_PER_QUERY: usize = 10;

#[derive(Debug, Clone)]
pub struct GitHubAggregator {
    client: Client,
    token: Option<String>,
}

impl GitHubAggregator {
    pub fn new(client: Client, token: Option<String>) -> Self {
        Self { client, token }
    }

    pub async fn fetch_all(&self) -> Vec<NewsItem> {
        let since = Utc::now() - ChronoDuration::days(7);
        let date_filter = since.format("%Y-%m-%d").to_string();

        // 1. Búsqueda de repositorios con filtro mínimo de estrellas (evita PoCs falsos)
        let repo_tasks = QUERIES.iter().map(|q| {
            let client = self.client.clone();
            let token = self.token.clone();
            let date_filter = date_filter.clone();
            async move {
                match search_repos(&client, token.as_deref(), q, &date_filter).await {
                    Ok(items) => {
                        debug!(query = q, count = items.len(), "GitHub repo search OK");
                        items
                    }
                    Err(e) => {
                        warn!(query = q, error = %e, "GitHub repo search failed");
                        Vec::new()
                    }
                }
            }
        });

        let (mut items, advisories) = tokio::join!(
            async { join_all(repo_tasks).await.into_iter().flatten().collect::<Vec<_>>() },
            fetch_reviewed_advisories(&self.client, self.token.as_deref())
        );

        match advisories {
            Ok(adv_items) => {
                debug!(count = adv_items.len(), "GitHub Security Advisories OK");
                items.extend(adv_items);
            }
            Err(e) => warn!(error = %e, "GitHub Security Advisories failed"),
        }

        items.sort_by(|a, b| a.url.cmp(&b.url));
        items.dedup_by(|a, b| a.url == b.url);
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

#[derive(Debug, Deserialize)]
struct GlobalAdvisory {
    ghsa_id: String,
    cve_id: Option<String>,
    summary: String,
    severity: String,
    html_url: String,
    published_at: DateTime<Utc>,
}

async fn fetch_reviewed_advisories(client: &Client, token: Option<&str>) -> Result<Vec<NewsItem>> {
    let mut req = client
        .get(GITHUB_ADVISORIES_URL)
        .query(&[("type", "reviewed"), ("per_page", "20")])
        .header(header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .timeout(Duration::from_secs(20));

    if let Some(t) = token {
        req = req.bearer_auth(t);
    }

    let resp = req.send().await.context("GitHub advisories request")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("GitHub advisories returned HTTP {status}: {body}");
    }

    let parsed: Vec<GlobalAdvisory> = resp
        .json()
        .await
        .context("Failed to deserialise GitHub advisories")?;

    Ok(parsed
        .into_iter()
        .map(|a| {
            let id_label = a.cve_id.unwrap_or(a.ghsa_id);
            NewsItem {
                id: a.html_url.clone(),
                title: format!("[{}] {}: {}", a.severity.to_uppercase(), id_label, a.summary),
                url: a.html_url,
                summary: Some(format!("GitHub Reviewed Security Advisory ({id_label})")),
                source_name: "GitHub Advisory DB".to_string(),
                source_kind: Source::GitHub,
                published: Some(a.published_at),
            }
        })
        .collect())
}

async fn search_repos(
    client: &Client,
    token: Option<&str>,
    query: &str,
    since_date: &str,
) -> Result<Vec<NewsItem>> {
    let q = format!("{query} in:name,description created:>={since_date} stars:>=5");

    let mut req = client
        .get(GITHUB_SEARCH_URL)
        .query(&[
            ("q", q.as_str()),
            ("sort", "stars"),
            ("order", "desc"),
            ("per_page", "20"),
        ])
        .header(header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .timeout(Duration::from_secs(20));

    if let Some(t) = token {
        req = req.bearer_auth(t);
    }

    let resp = req.send().await.context("GitHub search request")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("GitHub returned HTTP {status}: {body}");
    }

    let parsed: SearchResponse = resp
        .json()
        .await
        .context("Failed to deserialise GitHub search response")?;

    Ok(parsed
        .items
        .into_iter()
        .take(MAX_RESULTS_PER_QUERY)
        .map(|r| {
            let summary = match (&r.description, r.stargazers_count) {
                (Some(d), n) => Some(format!("{d} (⭐ {n})")),
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
        .collect())
}