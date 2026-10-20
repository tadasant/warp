use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::thread;

use futures::executor::block_on;

use super::*;
use crate::domains::normalize_host;
use crate::fixtures::{GITHUB_ENDPOINT, MCP_DOCS_ENDPOINT};
use crate::install::install_plan_for_card;

#[test]
fn extracts_scheme_qualified_hosts() {
    let hosts = extract_hosts(r#"{"command":"curl -s https://modelcontextprotocol.io/llms.txt"}"#);
    assert_eq!(hosts, vec!["modelcontextprotocol.io".to_string()]);
}

#[test]
fn extracts_bare_fixture_domain() {
    let hosts = extract_hosts(r#"{"url":"modelcontextprotocol.io"}"#);
    assert_eq!(hosts, vec!["modelcontextprotocol.io".to_string()]);
}

#[test]
fn strips_www_port_and_userinfo() {
    assert_eq!(
        extract_hosts("see https://www.github.com:443/foo and http://user@Example.COM./x"),
        vec!["github.com".to_string(), "example.com".to_string()]
    );
}

#[test]
fn never_probes_private_hosts() {
    assert_eq!(normalize_host("localhost"), None);
    assert_eq!(normalize_host("127.0.0.1"), None);
    assert_eq!(normalize_host("printer.local"), None);
    assert!(extract_hosts("curl http://10.0.0.1:8080/ http://localhost:3000").is_empty());
}

#[test]
fn off_mode_captures_nothing() {
    let urls = vec!["https://modelcontextprotocol.io/docs".to_string()];
    assert!(
        domains_to_probe(
            DiscoveryMode::Off,
            &DomainObservation::WebFetch { urls: &urls }
        )
        .is_empty()
    );
    assert!(
        domains_to_probe(
            DiscoveryMode::Off,
            &DomainObservation::ShellCommand {
                command: "curl https://modelcontextprotocol.io"
            }
        )
        .is_empty()
    );
}

#[test]
fn light_touch_captures_web_fetch_and_search_but_ignores_shell() {
    let urls = vec!["https://modelcontextprotocol.io/docs".to_string()];
    assert_eq!(
        domains_to_probe(
            DiscoveryMode::LightTouch,
            &DomainObservation::WebFetch { urls: &urls }
        ),
        vec!["modelcontextprotocol.io".to_string()]
    );
    let results = vec!["https://github.com/modelcontextprotocol".to_string()];
    assert_eq!(
        domains_to_probe(
            DiscoveryMode::LightTouch,
            &DomainObservation::WebSearch {
                result_urls: &results
            }
        ),
        vec!["github.com".to_string()]
    );
    assert!(
        domains_to_probe(
            DiscoveryMode::LightTouch,
            &DomainObservation::ShellCommand {
                command: "curl https://modelcontextprotocol.io"
            }
        )
        .is_empty()
    );
}

#[test]
fn light_touch_captures_fetch_like_mcp_tools_only() {
    let arguments = r#"{"url":"https://modelcontextprotocol.io"}"#;
    assert_eq!(
        domains_to_probe(
            DiscoveryMode::LightTouch,
            &DomainObservation::McpToolCall {
                tool_name: "fetch",
                arguments
            }
        ),
        vec!["modelcontextprotocol.io".to_string()]
    );
    assert!(
        domains_to_probe(
            DiscoveryMode::LightTouch,
            &DomainObservation::McpToolCall {
                tool_name: "create_issue",
                arguments
            }
        )
        .is_empty()
    );
}

#[test]
fn full_mode_captures_shell_commands_and_any_tool_call() {
    assert_eq!(
        domains_to_probe(
            DiscoveryMode::Full,
            &DomainObservation::ShellCommand {
                command: "curl -s -o /dev/null -w '%{http_code}' https://modelcontextprotocol.io/mcp"
            }
        ),
        vec!["modelcontextprotocol.io".to_string()]
    );
    assert_eq!(
        domains_to_probe(
            DiscoveryMode::Full,
            &DomainObservation::OtherToolCall {
                arguments: "git clone https://github.com/warpdotdev/warp"
            }
        ),
        vec!["github.com".to_string()]
    );
}

#[test]
fn fixture_resolver_returns_mcp_server_and_skills() {
    let found = block_on(FixtureResolver.resolve("modelcontextprotocol.io"));
    assert_eq!(found.len(), 2, "expected one server and one skills bundle");
    assert!(found.iter().all(|discovery| discovery.fixture));
    let server = found.iter().find(|d| d.is_server()).unwrap();
    assert_eq!(
        server.server_card().unwrap().remote.as_ref().unwrap().url,
        MCP_DOCS_ENDPOINT
    );
    assert!(!server.requires_secret());
    assert!(
        found
            .iter()
            .any(|d| matches!(d.kind, DiscoveryKind::Skills(_)))
    );
}

#[test]
fn github_fixture_requires_secret_and_injects_header() {
    let found = block_on(FixtureResolver.resolve("github.com"));
    let server = found.iter().find(|d| d.is_server()).unwrap();
    assert!(server.requires_secret());
    assert_eq!(
        install_plan_for(server, None),
        Err(InstallError::SecretRequired {
            header: "Authorization".to_string()
        })
    );
    assert_eq!(
        install_plan_for(server, Some("  ")),
        Err(InstallError::SecretRequired {
            header: "Authorization".to_string()
        })
    );
    let plan = install_plan_for(server, Some("ghp_demo")).unwrap();
    assert_eq!(plan.url, GITHUB_ENDPOINT);
    assert_eq!(plan.server_name, "github-mcp-server");
    assert_eq!(plan.transport, RemoteTransportKind::StreamableHttp);
    assert_eq!(
        plan.headers.get("Authorization").map(String::as_str),
        Some("Bearer ghp_demo")
    );
}

#[test]
fn oauth_card_installs_without_secret() {
    let mut card = fixtures::github_card();
    card.remote.as_mut().unwrap().auth.as_mut().unwrap().kind = "oauth".to_string();
    let plan = install_plan_for_card(&card, None).unwrap();
    assert!(plan.uses_oauth);
    assert!(plan.headers.is_empty());
}

#[test]
fn skills_bundle_is_not_installable_as_server() {
    let bundle =
        Discovery::new_skills("example.com", fixtures::modelcontextprotocol_skills(), true);
    assert_eq!(
        install_plan_for(&bundle, None),
        Err(InstallError::NotAServer)
    );
}

#[test]
fn state_dedupes_domains_and_negative_caches() {
    let mut state = DiscoveryState::default();
    assert!(state.begin_probe("example.com"));
    assert!(!state.begin_probe("example.com"));
    assert!(state.finish_probe("example.com", vec![]).is_empty());
    assert!(state.is_negative("example.com"));

    assert!(state.begin_probe("modelcontextprotocol.io"));
    let fresh = state.finish_probe(
        "modelcontextprotocol.io",
        fixtures::fixtures_for("modelcontextprotocol.io"),
    );
    assert_eq!(fresh.len(), 2);
    assert_eq!(state.pending().len(), 2);

    let server_id = fresh.iter().find(|d| d.is_server()).unwrap().id.clone();
    state.mark_installed(&server_id);
    assert!(state.is_installed(&server_id));
    assert_eq!(state.pending().len(), 1);

    let server = fixtures::fixtures_for("modelcontextprotocol.io").remove(0);
    state.requeue(server.clone());
    assert!(!state.is_installed(&server_id));
    assert_eq!(state.pending().len(), 2);
    state.mark_installed(&server_id);

    let skills_id = state.pending()[0].id.clone();
    state.mark_dismissed(&skills_id);
    assert!(state.pending().is_empty());

    // Re-probing (e.g. after a restart of the probe) never resurfaces installed/dismissed items.
    let fresh = state.finish_probe(
        "modelcontextprotocol.io",
        fixtures::fixtures_for("modelcontextprotocol.io"),
    );
    assert!(fresh.is_empty());
}

/// Serves `routes` (path -> JSON body) over plain HTTP on an ephemeral port; 404 otherwise.
fn serve_fixture_catalog(routes: Vec<(&'static str, String)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                    break;
                }
            }
            let path = request_line.split_whitespace().nth(1).unwrap_or("/");
            let response = match routes.iter().find(|(route, _)| *route == path) {
                Some((_, body)) => format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                ),
                None => "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    .to_string(),
            };
            let _ = stream.write_all(response.as_bytes());
        }
    });
    origin
}

#[test]
fn http_resolver_fetches_catalog_and_referenced_server_card() {
    let card = serde_json::json!({
        "name": "dev.example/docs",
        "title": "Example Docs",
        "description": "Docs search",
        "remote": { "type": "streamable-http", "url": "https://example.dev/mcp" }
    });
    let catalog = serde_json::json!({
        "version": "1",
        "servers": [
            { "serverCard": "/mcp/server-card" },
            { "card": {
                "name": "dev.example/inline",
                "description": "Inline card",
                "remote": { "type": "sse", "url": "https://example.dev/sse",
                            "auth": { "kind": "oauth" } }
            } }
        ],
        "skills": [ { "name": "Example Skills", "skills": [ { "name": "do-thing" } ] } ]
    });
    let origin = serve_fixture_catalog(vec![
        ("/.well-known/ai-catalog.json", catalog.to_string()),
        ("/mcp/server-card", card.to_string()),
    ]);
    let resolver = HttpResolver::new(http_client::Client::new_external())
        .with_origin_override("example.dev", &origin);

    let found = block_on(resolver.resolve("example.dev"));
    assert_eq!(found.len(), 3);
    assert!(found.iter().all(|discovery| !discovery.fixture));
    assert_eq!(found[0].title(), "Example Docs");
    let plan = install_plan_for(&found[1], None).unwrap();
    assert_eq!(plan.transport, RemoteTransportKind::Sse);
    assert!(plan.uses_oauth);
    assert!(matches!(found[2].kind, DiscoveryKind::Skills(_)));
}

#[test]
fn http_resolver_returns_nothing_without_catalog() {
    let origin = serve_fixture_catalog(vec![]);
    let resolver = HttpResolver::new(http_client::Client::new_external())
        .with_origin_overrides_from_str(&format!("nothing.dev={origin}, malformed"));
    assert!(block_on(resolver.resolve("nothing.dev")).is_empty());
}

#[test]
fn default_resolver_prefers_fixtures_and_skips_http_when_disabled() {
    let resolver = DefaultResolver::new(None);
    assert_eq!(
        block_on(resolver.resolve("modelcontextprotocol.io")).len(),
        2
    );
    assert!(block_on(resolver.resolve("example.dev")).is_empty());
}

#[test]
fn fixture_catalog_server_resolves_over_http() {
    let server = test_support::FixtureCatalogServer::start(0, Some("s3cret".to_string())).unwrap();
    let resolver = HttpResolver::new(http_client::Client::new_external())
        .with_origin_override("fixture.example", server.origin());
    let found = block_on(resolver.resolve("fixture.example"));
    assert_eq!(found.len(), 2);
    let server_discovery = found.iter().find(|d| d.is_server()).unwrap();
    assert!(server_discovery.requires_secret());
    let plan = install_plan_for(server_discovery, Some("s3cret")).unwrap();
    assert_eq!(plan.url, server.mcp_url());
    assert_eq!(
        plan.headers.get("Authorization").map(String::as_str),
        Some("Bearer s3cret")
    );
}
