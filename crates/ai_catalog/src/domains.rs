use std::sync::LazyLock;

use regex::Regex;

use crate::fixtures;

/// How aggressively discovery captures the domains an agent session touches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DiscoveryMode {
    #[default]
    Off,
    /// Only domains from web fetch / web search and fetch-like MCP tool calls.
    LightTouch,
    /// Every domain that appears in any agent tool call, including shell commands.
    Full,
}

/// Something the agent did that may reference a domain.
#[derive(Debug, Clone, PartialEq)]
pub enum DomainObservation<'a> {
    WebFetch {
        urls: &'a [String],
    },
    WebSearch {
        result_urls: &'a [String],
    },
    ShellCommand {
        command: &'a str,
    },
    McpToolCall {
        tool_name: &'a str,
        arguments: &'a str,
    },
    /// Any other tool call, serialized.
    OtherToolCall {
        arguments: &'a str,
    },
}

/// Domains to probe for `observation` under `mode`.
pub fn domains_to_probe(mode: DiscoveryMode, observation: &DomainObservation) -> Vec<String> {
    let light_touch = match observation {
        DomainObservation::WebFetch { urls } => return hosts_from_urls(mode, urls),
        DomainObservation::WebSearch { result_urls } => return hosts_from_urls(mode, result_urls),
        DomainObservation::McpToolCall { tool_name, .. } => is_fetch_like_tool(tool_name),
        DomainObservation::ShellCommand { .. } | DomainObservation::OtherToolCall { .. } => false,
    };
    let captured = match mode {
        DiscoveryMode::Off => false,
        DiscoveryMode::LightTouch => light_touch,
        DiscoveryMode::Full => true,
    };
    if !captured {
        return vec![];
    }
    match observation {
        DomainObservation::ShellCommand { command: text }
        | DomainObservation::McpToolCall {
            arguments: text, ..
        }
        | DomainObservation::OtherToolCall { arguments: text } => extract_hosts(text),
        DomainObservation::WebFetch { .. } | DomainObservation::WebSearch { .. } => vec![],
    }
}

fn hosts_from_urls(mode: DiscoveryMode, urls: &[String]) -> Vec<String> {
    match mode {
        DiscoveryMode::Off => vec![],
        DiscoveryMode::LightTouch | DiscoveryMode::Full => {
            let mut hosts = Vec::new();
            for url in urls {
                for host in extract_hosts(url) {
                    if !hosts.contains(&host) {
                        hosts.push(host);
                    }
                }
            }
            hosts
        }
    }
}

/// Matches fetch/search/browse-style tool names, which may be namespaced (e.g. `fetch__get`).
pub fn is_fetch_like_tool(tool_name: &str) -> bool {
    const HINTS: &[&str] = &[
        "fetch", "search", "scrape", "browse", "crawl", "web", "url", "navigate", "http_get",
        "wget", "curl",
    ];
    let name = tool_name.to_ascii_lowercase();
    HINTS.iter().any(|hint| name.contains(hint))
}

static URL_HOST_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bhttps?://(?:[^/\s@]+@)?([a-z0-9.\-]+)").expect("valid url host regex")
});

/// Scheme-qualified URL hosts in `text`, plus bare mentions of fixture domains.
pub fn extract_hosts(text: &str) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    let mut push = |host: &str| {
        if let Some(host) = normalize_host(host)
            && !hosts.contains(&host)
        {
            hosts.push(host);
        }
    };
    for captures in URL_HOST_RE.captures_iter(text) {
        if let Some(host) = captures.get(1) {
            push(host.as_str());
        }
    }
    let lower = text.to_ascii_lowercase();
    for domain in fixtures::fixture_domains() {
        if lower.contains(domain) {
            push(domain);
        }
    }
    hosts
}

/// Lowercases, strips a trailing dot and a leading `www.`, and rejects non-public hosts (no dot,
/// localhost, or IP literals) so discovery never probes private infrastructure.
pub fn normalize_host(host: &str) -> Option<String> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let is_ip_literal = host.chars().all(|c| c.is_ascii_digit() || c == '.');
    if !host.contains('.')
        || host.starts_with('.')
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || is_ip_literal
    {
        return None;
    }
    Some(host.to_string())
}
