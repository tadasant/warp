//! Document shapes from SEP-2127 ("Server Cards"): the AI Catalog served at
//! `/.well-known/ai-catalog.json` and the Server Cards / Skills bundles it references.
//!
//! Cards are advisory and must not be treated as authoritative for access control. Tool listings
//! are intentionally absent; they are discovered over MCP once a server is connected.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// The `/.well-known/ai-catalog.json` document. Fields are permissive so partial real-world
/// catalogs still parse.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AiCatalog {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub servers: Vec<CatalogServerRef>,
    #[serde(default)]
    pub skills: Vec<SkillsBundle>,
    /// Typed entries, as served by e.g. `https://github.com/.well-known/ai-catalog.json`.
    #[serde(default)]
    pub entries: Vec<CatalogEntry>,
}

pub const SERVER_CARD_MEDIA_TYPE: &str = "application/mcp-server-card+json";

/// A typed catalog entry. Only entries whose `type` is [`SERVER_CARD_MEDIA_TYPE`] are resolved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    #[serde(default)]
    pub identifier: Option<String>,
    #[serde(default, rename = "type")]
    pub media_type: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

impl CatalogEntry {
    pub fn is_server_card(&self) -> bool {
        self.media_type.as_deref() == Some(SERVER_CARD_MEDIA_TYPE)
    }
}

/// A catalog entry pointing at a Server Card, either inline or by URL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogServerRef {
    #[serde(default)]
    pub name: Option<String>,
    /// URL of an `application/mcp-server-card+json` document. May be relative to the catalog.
    #[serde(default, alias = "serverCard", alias = "card_url", alias = "url")]
    pub server_card: Option<String>,
    #[serde(default)]
    pub card: Option<ServerCard>,
}

/// Advisory metadata for a single MCP server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerCard {
    /// Reverse-DNS identity, e.g. `io.github.github/github-mcp-server`.
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub icons: Option<Icons>,
    /// A URL string or a `{ "url": ... }` object.
    #[serde(default)]
    pub repository: Option<serde_json::Value>,
    #[serde(default, alias = "websiteUrl")]
    pub website: Option<String>,
    #[serde(default)]
    pub remote: Option<RemoteTransport>,
    #[serde(default)]
    pub remotes: Vec<RemoteTransport>,
    #[serde(default, rename = "_meta")]
    pub meta: serde_json::Value,
}

impl ServerCard {
    /// The single `remote` block, or the first of `remotes`.
    pub fn primary_remote(&self) -> Option<&RemoteTransport> {
        self.remote.as_ref().or_else(|| self.remotes.first())
    }
}

/// Values may be `http(s)` URLs or `data:` URIs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Icons {
    #[serde(default)]
    pub light: Option<String>,
    #[serde(default)]
    pub dark: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteTransport {
    /// `streamable-http` or `sse`.
    #[serde(default = "default_transport_type", rename = "type")]
    pub transport_type: String,
    pub url: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default, alias = "protocolVersions")]
    pub protocol_versions: Vec<String>,
    /// Absent means the server is auth-less.
    #[serde(default)]
    pub auth: Option<AuthRequirement>,
}

fn default_transport_type() -> String {
    "streamable-http".to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthRequirement {
    /// `header` (a static secret sent in a request header) or `oauth`.
    pub kind: String,
    /// Header name for `header` auth. Defaults to `Authorization`.
    #[serde(default)]
    pub header: Option<String>,
    /// Value prefix for `header` auth, e.g. `Bearer `.
    #[serde(default)]
    pub prefix: Option<String>,
    /// Logical name of the secret to collect, e.g. `github_token`.
    #[serde(default)]
    pub variable: Option<String>,
    /// Guidance shown to the user when collecting the secret.
    #[serde(default)]
    pub instructions: Option<String>,
}

impl AuthRequirement {
    pub fn is_oauth(&self) -> bool {
        self.kind.eq_ignore_ascii_case("oauth")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillsBundle {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Where the skills can be loaded from (site URL or MCP endpoint).
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub skills: Vec<SkillRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillRef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub uri: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DiscoveryKind {
    // Boxed because `ServerCard` is much larger than `SkillsBundle`.
    Server(Box<ServerCard>),
    Skills(SkillsBundle),
}

/// A Server Card or Skills bundle found by probing a domain the agent touched.
#[derive(Debug, Clone, PartialEq)]
pub struct Discovery {
    /// Stable `<source_domain>#<name>` id used to install or dismiss.
    pub id: String,
    pub source_domain: String,
    pub kind: DiscoveryKind,
    /// True when this came from the built-in demo fixtures rather than a live
    /// `/.well-known/ai-catalog.json` fetch.
    pub fixture: bool,
}

impl Discovery {
    pub fn new_server(source_domain: &str, card: ServerCard, fixture: bool) -> Self {
        Self {
            id: format!("{source_domain}#{}", card.name),
            source_domain: source_domain.to_string(),
            kind: DiscoveryKind::Server(Box::new(card)),
            fixture,
        }
    }

    pub fn new_skills(source_domain: &str, bundle: SkillsBundle, fixture: bool) -> Self {
        Self {
            id: format!("{source_domain}#skills:{}", bundle.name),
            source_domain: source_domain.to_string(),
            kind: DiscoveryKind::Skills(bundle),
            fixture,
        }
    }

    pub fn title(&self) -> &str {
        match &self.kind {
            DiscoveryKind::Server(card) => card.title.as_deref().unwrap_or(&card.name),
            DiscoveryKind::Skills(bundle) => &bundle.name,
        }
    }

    pub fn description(&self) -> &str {
        match &self.kind {
            DiscoveryKind::Server(card) => &card.description,
            DiscoveryKind::Skills(bundle) => &bundle.description,
        }
    }

    pub fn server_card(&self) -> Option<&ServerCard> {
        match &self.kind {
            DiscoveryKind::Server(card) => Some(card),
            DiscoveryKind::Skills(_) => None,
        }
    }

    pub fn is_server(&self) -> bool {
        self.server_card().is_some()
    }

    /// Host the server's remote endpoint lives on, shown wherever the user decides to trust it.
    pub fn remote_host(&self) -> Option<String> {
        self.server_card()
            .and_then(ServerCard::primary_remote)
            .and_then(|remote| url::Url::parse(&remote.url).ok())
            .and_then(|url| url.host_str().map(str::to_string))
    }

    pub fn auth(&self) -> Option<&AuthRequirement> {
        self.server_card()
            .and_then(ServerCard::primary_remote)
            .and_then(|remote| remote.auth.as_ref())
    }

    /// True if installing requires the user to paste a secret (OAuth runs in the browser instead).
    pub fn requires_secret(&self) -> bool {
        self.auth().is_some_and(|auth| !auth.is_oauth())
    }
}
