//! Hand-authored catalogs standing in for `/.well-known/ai-catalog.json` documents that no public
//! domain serves yet. Every discovery built here has `fixture = true` so the UI can label it.
//!
//! `modelcontextprotocol.io` maps to a minimal card for the real, auth-less MCP server at
//! `https://modelcontextprotocol.io/mcp`: installing it connects to the live endpoint. The GitHub
//! card (from SEP-2127 issue #34) is mocked and drives the auth-entry install flow.

use std::collections::HashMap;

use crate::types::{
    AuthRequirement, Discovery, RemoteTransport, ServerCard, SkillRef, SkillsBundle,
};

pub const MCP_DOCS_DOMAIN: &str = "modelcontextprotocol.io";
pub const MCP_DOCS_ENDPOINT: &str = "https://modelcontextprotocol.io/mcp";
pub const GITHUB_ENDPOINT: &str = "https://api.githubcopilot.com/mcp/";

const FIXTURE_DOMAINS: &[&str] = &[
    MCP_DOCS_DOMAIN,
    "github.com",
    "api.githubcopilot.com",
    "api.github.com",
];

pub fn modelcontextprotocol_card() -> ServerCard {
    ServerCard {
        name: "io.modelcontextprotocol/docs-server".to_string(),
        version: Some("1.0.0".to_string()),
        title: Some("Model Context Protocol Docs".to_string()),
        description: "Search and retrieval over the Model Context Protocol site. Exposes \
            search_model_context_protocol and a read-only docs filesystem."
            .to_string(),
        icons: None,
        repository: Some(
            "https://github.com/modelcontextprotocol/modelcontextprotocol".to_string(),
        ),
        website: Some("https://modelcontextprotocol.io".to_string()),
        remote: Some(RemoteTransport {
            transport_type: "streamable-http".to_string(),
            url: MCP_DOCS_ENDPOINT.to_string(),
            headers: HashMap::new(),
            protocol_versions: vec!["2025-06-18".to_string(), "2025-03-26".to_string()],
            auth: None,
        }),
        meta: serde_json::json!({
            "io.modelcontextprotocol/sep": 2127,
            "demo": "auth-less headline demo; endpoint is live"
        }),
    }
}

/// Mirrors the `mintlify://skills/*` resources the docs server exposes.
pub fn modelcontextprotocol_skills() -> SkillsBundle {
    SkillsBundle {
        name: "MCP Authoring Skills".to_string(),
        description: "Guided skills for working with the Model Context Protocol: drafting SEPs \
            and searching MCP GitHub activity."
            .to_string(),
        source: Some(MCP_DOCS_ENDPOINT.to_string()),
        skills: vec![
            SkillRef {
                name: "draft-sep".to_string(),
                description: "Research and draft a Specification Enhancement Proposal following \
                    the MCP SEP governance process."
                    .to_string(),
                uri: Some("mintlify://skills/draft-sep".to_string()),
            },
            SkillRef {
                name: "search-mcp-github".to_string(),
                description: "Search MCP PRs, issues, and discussions across the \
                    modelcontextprotocol GitHub org."
                    .to_string(),
                uri: Some("mintlify://skills/search-mcp-github".to_string()),
            },
        ],
    }
}

pub fn github_card() -> ServerCard {
    ServerCard {
        name: "io.github.github/github-mcp-server".to_string(),
        version: Some("1.4.0".to_string()),
        title: Some("GitHub MCP Server".to_string()),
        description: "GitHub's official MCP server: repositories, issues, pull requests, \
            Actions, and code search over the GitHub API."
            .to_string(),
        icons: None,
        repository: Some("https://github.com/github/github-mcp-server".to_string()),
        website: Some("https://github.com".to_string()),
        remote: Some(RemoteTransport {
            transport_type: "streamable-http".to_string(),
            url: GITHUB_ENDPOINT.to_string(),
            headers: HashMap::new(),
            protocol_versions: vec!["2025-06-18".to_string()],
            auth: Some(AuthRequirement {
                kind: "header".to_string(),
                header: Some("Authorization".to_string()),
                prefix: Some("Bearer ".to_string()),
                variable: Some("github_token".to_string()),
                instructions: Some(
                    "Paste a GitHub Personal Access Token with repo scope. It is sent as the \
                     Authorization header to api.githubcopilot.com."
                        .to_string(),
                ),
            }),
        }),
        meta: serde_json::json!({
            "io.modelcontextprotocol/sep": 2127,
            "demo": "mocked card from SEP-2127 issue #34; drives the auth-entry flow"
        }),
    }
}

/// Fixture discoveries for a normalized domain, if any.
pub fn fixtures_for(domain: &str) -> Vec<Discovery> {
    match domain {
        MCP_DOCS_DOMAIN => vec![
            Discovery::new_server(domain, modelcontextprotocol_card(), true),
            Discovery::new_skills(domain, modelcontextprotocol_skills(), true),
        ],
        "github.com" | "api.githubcopilot.com" | "api.github.com" => {
            vec![Discovery::new_server(domain, github_card(), true)]
        }
        _ => vec![],
    }
}

/// Domains the fixtures recognize, so a bare hostname (no scheme) in a tool call still probes.
pub fn fixture_domains() -> &'static [&'static str] {
    FIXTURE_DOMAINS
}
