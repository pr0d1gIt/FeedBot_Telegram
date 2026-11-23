//! Shared domain types used across aggregation, categorisation and bot layers.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

/// The four high-level buckets reported to users.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Category {
    /// Active campaigns, breaches, ransomware, defacements, etc.
    Attacks,
    /// CVEs, zero-days, advisories, RCEs, critical bugs.
    Vulnerabilities,
    /// Proof-of-concept code, exploit releases, technical write-ups.
    Pocs,
    /// Indicators of compromise: hashes, IPs, threat-intel reports.
    Iocs,
    /// Anything cyber-security-related that did not match a more specific bucket.
    General,
}

impl Category {
    /// Human-readable header used in digest messages.
    pub fn header(&self) -> &'static str {
        match self {
            Category::Attacks => "🚨 Ataques recientes",
            Category::Vulnerabilities => "🛡️ Vulnerabilidades",
            Category::Pocs => "💥 POCs publicados",
            Category::Iocs => "🧬 IOCs / Threat Intel",
            Category::General => "📰 Otras noticias",
        }
    }

    /// Iteration order used when rendering a digest.
    pub fn ordered() -> [Category; 5] {
        [
            Category::Attacks,
            Category::Vulnerabilities,
            Category::Pocs,
            Category::Iocs,
            Category::General,
        ]
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.header())
    }
}

/// Where a piece of intel came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    Rss,
    GitHub,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Rss => f.write_str("RSS"),
            Source::GitHub => f.write_str("GitHub"),
        }
    }
}

/// A single normalised piece of cybersecurity intel.
///
/// Produced by the aggregator regardless of source (RSS feed or GitHub repo)
/// and consumed by the categoriser and the bot's render layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsItem {
    /// Stable identifier used for deduplication. Typically the canonical URL,
    /// or `<feed>::<guid>` when no URL is provided.
    pub id: String,
    pub title: String,
    pub url: String,
    pub summary: Option<String>,
    pub source_name: String,
    pub source_kind: Source,
    pub published: Option<DateTime<Utc>>,
}

impl NewsItem {
    /// Returns the publication date or the current UTC time as a fallback.
    #[allow(dead_code)]
    pub fn published_or_now(&self) -> DateTime<Utc> {
        self.published.unwrap_or_else(Utc::now)
    }
}

/// A `NewsItem` together with its assigned category.
#[derive(Debug, Clone)]
pub struct CategorisedItem {
    pub item: NewsItem,
    pub category: Category,
}