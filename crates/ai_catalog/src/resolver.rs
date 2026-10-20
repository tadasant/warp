use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use url::Url;

use crate::fixtures;
use crate::types::{AiCatalog, Discovery, ServerCard};

pub const AI_CATALOG_PATH: &str = "/.well-known/ai-catalog.json";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(6);

/// Turns a normalized domain into the Server Cards and Skills bundles it advertises.
#[async_trait]
pub trait CatalogResolver: Send + Sync {
    /// Returns an empty vec when the domain advertises nothing; callers negative-cache that.
    async fn resolve(&self, domain: &str) -> Vec<Discovery>;
}

#[derive(Debug, Default)]
pub struct FixtureResolver;

#[async_trait]
impl CatalogResolver for FixtureResolver {
    async fn resolve(&self, domain: &str) -> Vec<Discovery> {
        fixtures::fixtures_for(domain)
    }
}

/// Fetches `https://<domain>/.well-known/ai-catalog.json` and the Server Cards it references.
pub struct HttpResolver {
    client: http_client::Client,
    origin_overrides: HashMap<String, String>,
}

impl Default for HttpResolver {
    fn default() -> Self {
        Self::new(http_client::Client::new_external())
    }
}

impl HttpResolver {
    pub fn new(client: http_client::Client) -> Self {
        Self {
            client,
            origin_overrides: HashMap::new(),
        }
    }

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

    async fn fetch_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Option<T> {
        let response = match self.client.get(url).timeout(REQUEST_TIMEOUT).send().await {
            Ok(response) => response,
            Err(err) => {
                log::debug!("AI catalog probe of {url} failed: {err}");
                return None;
            }
        };
        if !response.status().is_success() {
            return None;
        }
        match response.json::<T>().await {
            Ok(document) => Some(document),
            Err(err) => {
                log::debug!("AI catalog document at {url} did not parse: {err}");
                None
            }
        }
    }
}

#[async_trait]
impl CatalogResolver for HttpResolver {
    async fn resolve(&self, domain: &str) -> Vec<Discovery> {
        let catalog_url = self.catalog_url(domain);
        let Some(catalog) = self.fetch_json::<AiCatalog>(&catalog_url).await else {
            return vec![];
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
            if let Some(card) = self.fetch_json::<ServerCard>(card_url.as_str()).await {
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
        discoveries
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
    async fn resolve(&self, domain: &str) -> Vec<Discovery> {
        if let Some(http) = &self.http {
            let live = http.resolve(domain).await;
            if !live.is_empty() {
                return live;
            }
        }
        FixtureResolver.resolve(domain).await
    }
}
