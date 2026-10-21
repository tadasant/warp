use std::collections::HashMap;
use std::time::Duration;

use async_compat::Compat;
use async_trait::async_trait;
use url::Url;

use crate::domains::is_public_https_url;
use crate::fixtures;
use crate::types::{AiCatalog, Discovery, ServerCard};

pub const AI_CATALOG_PATH: &str = "/.well-known/ai-catalog.json";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(6);

/// Turns a normalized domain into the Server Cards and Skills bundles it advertises.
#[async_trait]
pub trait CatalogResolver: Send + Sync {
    /// Returns an empty vec when the domain advertises nothing; callers negative-cache that.
    async fn resolve(&self, domain: &str) -> Vec<Discovery> {
        self.resolve_traced(domain).await.discoveries
    }

    /// Like [`Self::resolve`], also recording each document fetched and its outcome.
    async fn resolve_traced(&self, domain: &str) -> TracedResolution;
}

/// One document fetched while resolving a domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeRecord {
    pub url: String,
    /// Human-readable result, e.g. `200 OK` or `HTTP 404`.
    pub outcome: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TracedResolution {
    pub discoveries: Vec<Discovery>,
    pub probes: Vec<ProbeRecord>,
}

#[derive(Debug, Default)]
pub struct FixtureResolver;

#[async_trait]
impl CatalogResolver for FixtureResolver {
    async fn resolve_traced(&self, domain: &str) -> TracedResolution {
        let discoveries = fixtures::fixtures_for(domain);
        let outcome = if discoveries.is_empty() {
            "no built-in demo fixture".to_string()
        } else {
            format!("{} built-in demo fixture(s)", discoveries.len())
        };
        TracedResolution {
            discoveries,
            probes: vec![ProbeRecord {
                url: format!("fixture:{domain}"),
                outcome,
            }],
        }
    }
}

/// Fetches `https://<domain>/.well-known/ai-catalog.json` and the Server Cards it references.
///
/// Catalogs are untrusted input, so redirects are not followed and referenced cards are only
/// fetched from public `https` hosts.
pub struct HttpResolver {
    client: reqwest::Client,
    origin_overrides: HashMap<String, String>,
}

impl Default for HttpResolver {
    fn default() -> Self {
        let client = reqwest::Client::builder()
            .user_agent("warp-ai-catalog-discovery/0.1")
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .build()
            .unwrap_or_default();
        Self {
            client,
            origin_overrides: HashMap::new(),
        }
    }
}

impl HttpResolver {
    /// Probes `origin` (e.g. `http://127.0.0.1:8765`) instead of `https://<domain>` for `domain`.
    /// Lets local fixture servers stand in for real domains in tests and demos.
    pub fn with_origin_override(mut self, domain: &str, origin: &str) -> Self {
        self.origin_overrides
            .insert(domain.to_string(), origin.trim_end_matches('/').to_string());
        self
    }

    /// Parses `domain=origin` pairs separated by commas, ignoring malformed entries.
    pub fn with_origin_overrides_from_str(mut self, overrides: &str) -> Self {
        for (domain, origin) in overrides
            .split(',')
            .filter_map(|pair| pair.trim().split_once('='))
        {
            self = self.with_origin_override(domain.trim(), origin.trim());
        }
        self
    }

    pub fn catalog_url(&self, domain: &str) -> String {
        let origin = self
            .origin_overrides
            .get(domain)
            .cloned()
            .unwrap_or_else(|| format!("https://{domain}"));
        format!("{origin}{AI_CATALOG_PATH}")
    }

    async fn fetch_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        probes: &mut Vec<ProbeRecord>,
    ) -> Option<T> {
        // reqwest needs a Tokio reactor; the app's executors don't provide one.
        let (document, outcome) = Compat::new(async {
            match self.client.get(url).timeout(REQUEST_TIMEOUT).send().await {
                Err(err) => (None, format!("network error: {err}")),
                Ok(response) if !response.status().is_success() => {
                    (None, format!("HTTP {}", response.status().as_u16()))
                }
                Ok(response) => match response.json::<T>().await {
                    Ok(document) => (Some(document), "200 OK".to_string()),
                    Err(err) => (None, format!("unparseable document: {err}")),
                },
            }
        })
        .await;
        log::debug!("AI catalog probe outcome: {outcome}");
        probes.push(ProbeRecord {
            url: url.to_string(),
            outcome,
        });
        document
    }
}

#[async_trait]
impl CatalogResolver for HttpResolver {
    async fn resolve_traced(&self, domain: &str) -> TracedResolution {
        let mut probes = Vec::new();
        let catalog_url = self.catalog_url(domain);
        let Some(catalog) = self
            .fetch_json::<AiCatalog>(&catalog_url, &mut probes)
            .await
        else {
            return TracedResolution {
                discoveries: vec![],
                probes,
            };
        };
        let mut cards = Vec::new();
        let mut card_urls = Vec::new();
        for server in catalog.servers {
            match (server.card, server.server_card) {
                (Some(card), _) => cards.push(card),
                (None, Some(card_url)) => card_urls.push(card_url),
                (None, None) => {}
            }
        }
        card_urls.extend(
            catalog
                .entries
                .into_iter()
                .filter(|entry| entry.is_server_card())
                .filter_map(|entry| entry.url),
        );
        for card_url in card_urls {
            let Ok(card_url) = Url::parse(&catalog_url).and_then(|base| base.join(&card_url))
            else {
                continue;
            };
            // Local fixture origins stand in for real domains, so only their cards may be local.
            if !self.origin_overrides.contains_key(domain) && !is_public_https_url(&card_url) {
                probes.push(ProbeRecord {
                    url: card_url.to_string(),
                    outcome: "skipped: not https on a public host".to_string(),
                });
                continue;
            }
            if let Some(card) = self
                .fetch_json::<ServerCard>(card_url.as_str(), &mut probes)
                .await
            {
                cards.push(card);
            }
        }
        let mut discoveries: Vec<_> = cards
            .into_iter()
            .map(|card| Discovery::new_server(domain, card, false))
            .collect();
        discoveries.extend(
            catalog
                .skills
                .into_iter()
                .map(|bundle| Discovery::new_skills(domain, bundle, false)),
        );
        TracedResolution {
            discoveries,
            probes,
        }
    }
}

/// Probes the live catalog first, then falls back to the built-in demo fixtures for domains that
/// do not publish one yet.
pub struct DefaultResolver {
    http: Option<HttpResolver>,
}

impl DefaultResolver {
    pub fn new(http: Option<HttpResolver>) -> Self {
        Self { http }
    }
}

#[async_trait]
impl CatalogResolver for DefaultResolver {
    async fn resolve_traced(&self, domain: &str) -> TracedResolution {
        let mut probes = Vec::new();
        if let Some(http) = &self.http {
            let live = http.resolve_traced(domain).await;
            if !live.discoveries.is_empty() {
                return live;
            }
            probes = live.probes;
        }
        let mut fixtures = FixtureResolver.resolve_traced(domain).await;
        probes.append(&mut fixtures.probes);
        TracedResolution {
            discoveries: fixtures.discoveries,
            probes,
        }
    }
}
