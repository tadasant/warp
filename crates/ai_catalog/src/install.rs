use std::collections::HashMap;

use crate::domains::is_permitted_remote_url;
use crate::types::{Discovery, DiscoveryKind, ServerCard};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteTransportKind {
    StreamableHttp,
    Sse,
}

/// A ready-to-spawn remote MCP server derived from a Server Card.
#[derive(Debug, Clone, PartialEq)]
pub struct InstallPlan {
    pub server_name: String,
    pub url: String,
    pub transport: RemoteTransportKind,
    pub headers: HashMap<String, String>,
    /// The card requires OAuth, which the MCP client runs on connect.
    pub uses_oauth: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    NotAServer,
    NoRemoteTransport,
    UnsupportedTransport(String),
    /// The remote isn't `https` on a public host (or a loopback server).
    UnsafeRemoteUrl(String),
    SecretRequired {
        header: String,
    },
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::NotAServer => write!(f, "skills bundles are not MCP servers"),
            InstallError::NoRemoteTransport => write!(f, "server card has no remote transport"),
            InstallError::UnsupportedTransport(kind) => {
                write!(f, "unsupported transport type: {kind}")
            }
            InstallError::UnsafeRemoteUrl(url) => {
                write!(
                    f,
                    "refusing to connect to {url}: not https on a public host"
                )
            }
            InstallError::SecretRequired { header } => {
                write!(f, "auth required: provide a value for {header}")
            }
        }
    }
}

impl std::error::Error for InstallError {}

/// A short, key-safe server name from a card's reverse-DNS identity, e.g.
/// `io.github.github/github-mcp-server` becomes `github-mcp-server`.
pub fn server_name_for(card: &ServerCard) -> String {
    let base = card.name.rsplit('/').next().unwrap_or(&card.name);
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "discovered-server".to_string()
    } else {
        cleaned
    }
}

/// Builds an install plan, injecting `secret` as the card's auth header when it requires one.
pub fn install_plan_for_card(
    card: &ServerCard,
    secret: Option<&str>,
) -> Result<InstallPlan, InstallError> {
    let remote = card
        .primary_remote()
        .ok_or(InstallError::NoRemoteTransport)?;
    if !url::Url::parse(&remote.url).is_ok_and(|url| is_permitted_remote_url(&url)) {
        return Err(InstallError::UnsafeRemoteUrl(remote.url.clone()));
    }
    let transport = match remote.transport_type.to_ascii_lowercase().as_str() {
        "streamable-http" | "streamable_http" | "http" => RemoteTransportKind::StreamableHttp,
        "sse" => RemoteTransportKind::Sse,
        other => return Err(InstallError::UnsupportedTransport(other.to_string())),
    };

    let mut headers = remote.headers.clone();
    let mut uses_oauth = false;
    if let Some(auth) = &remote.auth {
        if auth.is_oauth() {
            uses_oauth = true;
        } else {
            let header = auth
                .header
                .clone()
                .unwrap_or_else(|| "Authorization".to_string());
            let secret = secret
                .map(str::trim)
                .filter(|secret| !secret.is_empty())
                .ok_or_else(|| InstallError::SecretRequired {
                    header: header.clone(),
                })?;
            let prefix = auth.prefix.as_deref().unwrap_or_default();
            headers.insert(header, format!("{prefix}{secret}"));
        }
    } else if let Some(secret) = secret.map(str::trim).filter(|secret| !secret.is_empty()) {
        // Cards often omit their auth requirements (GitHub's does), so a user-supplied token
        // for an auth-less card is sent as a bearer token.
        headers.insert("Authorization".to_string(), format!("Bearer {secret}"));
    }

    Ok(InstallPlan {
        server_name: server_name_for(card),
        url: remote.url.clone(),
        transport,
        headers,
        uses_oauth,
    })
}

pub fn install_plan_for(
    discovery: &Discovery,
    secret: Option<&str>,
) -> Result<InstallPlan, InstallError> {
    match &discovery.kind {
        DiscoveryKind::Server(card) => install_plan_for_card(card, secret),
        DiscoveryKind::Skills(_) => Err(InstallError::NotAServer),
    }
}
