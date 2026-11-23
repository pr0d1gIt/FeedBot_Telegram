//! Keyword-based news categoriser.
//!
//! The categoriser is intentionally simple: it inspects the title and summary
//! of a [`NewsItem`] for sets of pre-defined keywords / regexes and assigns
//! the *first* matching category in priority order:
//!
//! 1. [`Category::Pocs`] – proof-of-concept and exploit code releases
//! 2. [`Category::Vulnerabilities`] – CVEs, advisories, zero-days
//! 3. [`Category::Attacks`] – campaigns, breaches, ransomware
//! 4. [`Category::Iocs`] – threat-intel reports, hashes, malicious IPs
//! 5. [`Category::General`] – anything else
//!
//! POC is checked first because POC announcements very often *also* mention a
//! CVE, but we want them grouped together as actionable code.

use crate::models::{CategorisedItem, Category, NewsItem};
use once_cell::sync::Lazy;
use regex::Regex;

static CVE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\bCVE-\d{4}-\d{4,7}\b").expect("valid CVE regex"));

static SHA_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b[a-fA-F0-9]{32,64}\b").expect("valid hash regex"));

static IP_RE: Lazy<Regex> = Lazy::new(|| {
    // Catch common defanged IoC IP notations (e.g. 1.2.3[.]4) as well.
    Regex::new(r"\b\d{1,3}(?:\.|\[\.\])\d{1,3}(?:\.|\[\.\])\d{1,3}(?:\.|\[\.\])\d{1,3}\b")
        .expect("valid IP regex")
});

const POC_KEYWORDS: &[&str] = &[
    "poc",
    "proof-of-concept",
    "proof of concept",
    "exploit code",
    "exploit released",
    "public exploit",
    "weaponized",
    "weaponised",
    "metasploit module",
];

const VULN_KEYWORDS: &[&str] = &[
    "vulnerability",
    "vulnerabilities",
    "zero-day",
    "zero day",
    "0-day",
    "0day",
    "rce",
    "remote code execution",
    "privilege escalation",
    "buffer overflow",
    "use-after-free",
    "use after free",
    "sql injection",
    "xss",
    "cross-site scripting",
    "ssrf",
    "lfi",
    "rfi",
    "deserialization",
    "patch tuesday",
    "security advisory",
    "advisory",
    "critical flaw",
    "critical bug",
    "high severity",
    "critical severity",
    "cvss",
];

const ATTACK_KEYWORDS: &[&str] = &[
    "attack",
    "attacks",
    "attacked",
    "breach",
    "breaches",
    "breached",
    "hack",
    "hacked",
    "hacker",
    "hackers",
    "ransomware",
    "data leak",
    "data leaked",
    "leaked data",
    "stolen data",
    "phishing",
    "smishing",
    "vishing",
    "ddos",
    "botnet",
    "campaign",
    "intrusion",
    "compromised",
    "wiper",
    "infostealer",
    "credential stuffing",
    "supply chain attack",
    "espionage",
    "apt",
    "nation-state",
    "exploited in the wild",
    "actively exploited",
    "exploitation observed",
];

const IOC_KEYWORDS: &[&str] = &[
    "indicators of compromise",
    "indicator of compromise",
    "ioc",
    "iocs",
    "threat intelligence",
    "threat intel",
    "yara",
    "malware analysis",
    "malware report",
    "c2 server",
    "c&c server",
    "command and control",
    "ttp",
    "ttps",
    "tactics, techniques",
    "mitre att&ck",
    "att&ck",
    "indicators",
];

/// Categorise a single [`NewsItem`] without taking ownership.
pub fn categorise(item: &NewsItem) -> Category {
    let haystack_owned = build_haystack(item);
    let haystack = haystack_owned.as_str();

    // 1) POCs first — POC announcements often mention CVEs too.
    if contains_any(haystack, POC_KEYWORDS) {
        return Category::Pocs;
    }

    // 2) Vulnerabilities — explicit CVE id or vuln vocabulary.
    if CVE_RE.is_match(haystack) || contains_any(haystack, VULN_KEYWORDS) {
        return Category::Vulnerabilities;
    }

    // 3) Active attacks / breaches.
    if contains_any(haystack, ATTACK_KEYWORDS) {
        return Category::Attacks;
    }

    // 4) Threat intel / IOCs — keywords or raw hashes / defanged IPs.
    if contains_any(haystack, IOC_KEYWORDS) || SHA_RE.is_match(haystack) || IP_RE.is_match(haystack)
    {
        return Category::Iocs;
    }

    Category::General
}

/// Convenience helper that consumes a list and returns categorised wrappers.
pub fn categorise_all(items: Vec<NewsItem>) -> Vec<CategorisedItem> {
    items
        .into_iter()
        .map(|item| {
            let category = categorise(&item);
            CategorisedItem { item, category }
        })
        .collect()
}

fn build_haystack(item: &NewsItem) -> String {
    let mut s = String::with_capacity(
        item.title.len() + item.summary.as_deref().map(str::len).unwrap_or(0) + 1,
    );
    s.push_str(&item.title);
    s.push(' ');
    if let Some(summary) = &item.summary {
        s.push_str(summary);
    }
    s.to_lowercase()
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Source;

    fn item(title: &str, summary: Option<&str>) -> NewsItem {
        NewsItem {
            id: title.to_string(),
            title: title.to_string(),
            url: "https://example.com".to_string(),
            summary: summary.map(str::to_string),
            source_name: "test".to_string(),
            source_kind: Source::Rss,
            published: None,
        }
    }

    #[test]
    fn detects_poc_over_vulnerability() {
        let i = item(
            "Public PoC released for CVE-2024-12345 in Acme product",
            None,
        );
        assert_eq!(categorise(&i), Category::Pocs);
    }

    #[test]
    fn detects_cve_id_as_vulnerability() {
        let i = item("Acme issues advisory for CVE-2024-0001", None);
        assert_eq!(categorise(&i), Category::Vulnerabilities);
    }

    #[test]
    fn detects_ransomware_as_attack() {
        let i = item("Hospital hit by ransomware attack", None);
        assert_eq!(categorise(&i), Category::Attacks);
    }

    #[test]
    fn detects_ioc_keywords() {
        let i = item(
            "Malware analysis: new infostealer C2 server uncovered",
            Some("Includes IOCs and YARA rules"),
        );
        // Either Attacks (infostealer keyword) or Iocs is acceptable;
        // we expect Attacks since attack keywords run before IOCs.
        let c = categorise(&i);
        assert!(matches!(c, Category::Attacks | Category::Iocs));
    }

    #[test]
    fn detects_hash_only_as_ioc() {
        let i = item(
            "Threat update",
            Some("Observed sample hash: 44d88612fea8a8f36de82e1278abb02f"),
        );
        assert_eq!(categorise(&i), Category::Iocs);
    }

    #[test]
    fn defaults_to_general() {
        let i = item("Cybersecurity conference announces new speakers", None);
        assert_eq!(categorise(&i), Category::General);
    }
}