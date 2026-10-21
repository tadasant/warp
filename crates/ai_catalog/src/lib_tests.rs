use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::thread;

use futures::executor::block_on;

use super::*;
use crate::domains::normalize_host;
use crate::fixtures::MCP_DOCS_ENDPOINT;
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
        server.server_card().unwrap().primary_remote().unwrap().url,
        MCP_DOCS_ENDPOINT
    );
    assert!(!server.requires_secret());
    assert!(
        found
            .iter()
            .any(|d| matches!(d.kind, DiscoveryKind::Skills(_)))
    );
}

/// Verbatim shape of `https://github.com/.well-known/ai-catalog.json` (fetched 2026-10-20).
const GITHUB_CATALOG: &str = r#"{"specVersion":"1.0","entries":[{"identifier":"urn:air:github.com:mcp:github-mcp-server","type":"application/mcp-server-card+json","url":"__CARD_URL__"}]}"#;

/// Verbatim shape of `https://api.githubcopilot.com/mcp/server-card` (fetched 2026-10-20).
const GITHUB_CARD: &str = r#"{"$schema":"https://static.modelcontextprotocol.io/schemas/v1/server-card.schema.json","name":"io.github.github/github-mcp-server","version":"github-mcp-server/remote-c91af86fab613fa50f4f34159186e6a61e89c381","description":"Connect AI assistants to GitHub - manage repos, issues, PRs, and workflows through natural language.","title":"GitHub","websiteUrl":"https://github.com/github/github-mcp-server","repository":{"url":"https://github.com/github/github-mcp-server","source":"github","id":"942771284"},"remotes":[{"type":"streamable-http","url":"https://api.githubcopilot.com/mcp/"}]}"#;

fn header_auth_card() -> ServerCard {
    serde_json::from_value(serde_json::json!({
        "name": "dev.example/secured",
        "remote": {
            "type": "streamable-http",
            "url": "https://example.dev/mcp",
            "auth": { "kind": "header", "header": "X-Api-Key" }
        }
    }))
    .unwrap()
}

#[test]
fn header_auth_card_requires_secret_and_injects_header() {
    let card = header_auth_card();
    let expected_error = Err(InstallError::SecretRequired {
        header: "X-Api-Key".to_string(),
    });
    assert_eq!(install_plan_for_card(&card, None), expected_error);
    assert_eq!(install_plan_for_card(&card, Some("  ")), expected_error);
    let plan = install_plan_for_card(&card, Some("k3y")).unwrap();
    assert_eq!(plan.server_name, "secured");
    assert_eq!(
        plan.headers.get("X-Api-Key").map(String::as_str),
        Some("k3y")
    );
}

#[test]
fn oauth_card_installs_without_secret() {
    let mut card = header_auth_card();
    card.remote.as_mut().unwrap().auth.as_mut().unwrap().kind = "oauth".to_string();
    let plan = install_plan_for_card(&card, None).unwrap();
    assert!(plan.uses_oauth);
    assert!(plan.headers.is_empty());
}

#[test]
fn parses_github_card_and_sends_token_as_bearer() {
    let card: ServerCard = serde_json::from_str(GITHUB_CARD).unwrap();
    assert_eq!(card.title.as_deref(), Some("GitHub"));
    let plan = install_plan_for_card(&card, None).unwrap();
    assert_eq!(plan.url, "https://api.githubcopilot.com/mcp/");
    assert_eq!(plan.server_name, "github-mcp-server");
    assert!(plan.headers.is_empty());
    let plan = install_plan_for_card(&card, Some("ghp_demo")).unwrap();
    assert_eq!(
        plan.headers.get("Authorization").map(String::as_str),
        Some("Bearer ghp_demo")
    );
}

#[test]
fn http_resolver_follows_typed_catalog_entries() {
    let card_origin = serve_fixture_catalog(vec![("/mcp/server-card", GITHUB_CARD.to_string())]);
    let catalog = GITHUB_CATALOG.replace("__CARD_URL__", &format!("{card_origin}/mcp/server-card"));
    let origin = serve_fixture_catalog(vec![("/.well-known/ai-catalog.json", catalog)]);
    let resolver = HttpResolver::default().with_origin_override("github.com", &origin);
    let found = block_on(resolver.resolve("github.com"));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "github.com#io.github.github/github-mcp-server");
    assert_eq!(found[0].title(), "GitHub");
    assert!(!found[0].fixture);
}

/// Hits the real `https://github.com/.well-known/ai-catalog.json`. Opt in with `AI_CATALOG_LIVE=1`.
#[test]
fn live_github_catalog_resolves_github_mcp_server() {
    if std::env::var_os("AI_CATALOG_LIVE").is_none() {
        eprintln!("skipping live test; set AI_CATALOG_LIVE=1 to run it");
        return;
    }
    let found = block_on(DefaultResolver::new(Some(HttpResolver::default())).resolve("github.com"));
    let server = found
        .iter()
        .find(|d| d.id == "github.com#io.github.github/github-mcp-server")
        .expect("github.com should advertise the GitHub MCP server");
    assert!(!server.fixture);
    let plan = install_plan_for(server, None).unwrap();
    assert_eq!(plan.url, "https://api.githubcopilot.com/mcp/");
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
    let resolver = HttpResolver::default().with_origin_override("example.dev", &origin);

    let found = block_on(resolver.resolve("example.dev"));
    assert_eq!(found.len(), 3);
    assert!(found.iter().all(|discovery| !discovery.fixture));
    assert_eq!(found[1].title(), "Example Docs");
    let plan = install_plan_for(&found[0], None).unwrap();
    assert_eq!(plan.transport, RemoteTransportKind::Sse);
    assert!(plan.uses_oauth);
    assert!(matches!(found[2].kind, DiscoveryKind::Skills(_)));
}

#[test]
fn http_resolver_returns_nothing_without_catalog() {
    let origin = serve_fixture_catalog(vec![]);
    let resolver = HttpResolver::default()
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
    let resolver = HttpResolver::default().with_origin_override("fixture.example", server.origin());
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

#[test]
fn rejects_ports_ip_literals_and_private_suffixes() {
    for host in [
        "127.0.0.1:8080",
        "foo.localhost:3000",
        "[::ffff:127.0.0.1]",
        "metadata.google.internal",
        "nas.lan",
        "router.home.arpa",
        "10.0.0.1",
    ] {
        assert_eq!(normalize_host(host), None, "{host} should be rejected");
    }
    assert_eq!(
        normalize_host("API.GitHub.com."),
        Some("api.github.com".to_string())
    );
}

#[test]
fn refuses_to_install_servers_on_non_public_or_plaintext_hosts() {
    let mut card = fixtures::modelcontextprotocol_card();
    for url in [
        "http://modelcontextprotocol.io/mcp",
        "https://169.254.169.254/mcp",
        "https://metadata.google.internal/mcp",
    ] {
        card.remote.as_mut().unwrap().url = url.to_string();
        assert_eq!(
            install_plan_for_card(&card, None),
            Err(InstallError::UnsafeRemoteUrl(url.to_string()))
        );
    }
    card.remote.as_mut().unwrap().url = "http://127.0.0.1:9000/mcp".to_string();
    assert!(install_plan_for_card(&card, None).is_ok());
}

#[test]
fn only_public_https_urls_count_as_public() {
    for url in [
        "http://127.0.0.1:1/card",
        "https://169.254.169.254/latest/meta-data",
    ] {
        assert!(!crate::domains::is_public_https_url(
            &url::Url::parse(url).unwrap()
        ));
    }
    assert!(crate::domains::is_public_https_url(
        &url::Url::parse("https://api.githubcopilot.com/mcp/server-card").unwrap()
    ));
}

#[test]
fn http_resolver_does_not_follow_redirects() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 2 {
                line.clear();
            }
            let _ = stream.write_all(
                b"HTTP/1.1 302 Found\r\nlocation: http://169.254.169.254/\r\ncontent-length: 0\r\n\r\n",
            );
        }
    });
    let resolver = HttpResolver::default().with_origin_override("redirect.dev", &origin);
    assert!(block_on(resolver.resolve("redirect.dev")).is_empty());
}

mod reasoner_tests {
    use std::time::Duration;

    use futures::executor::block_on;

    use crate::reasoner::{
        AnthropicLlm, ReasonerLlm, SYSTEM_PROMPT, TraceStep, build_user_prompt, parse_verdict,
        trace_to_markdown,
    };
    use crate::resolver::CatalogResolver;
    use crate::test_support::StubLlmServer;
    use crate::trust::{NudgeOnlyPolicy, SuggestionOrigin, TrustPolicy};
    use crate::{DefaultResolver, HttpResolver};

    const GITHUB_VERDICT: &str = r#"{"intent": "Triage the user's open GitHub pull requests.", "candidates": [{"domain": "github.com", "reason": "GitHub publishes the official GitHub MCP server"}]}"#;

    #[test]
    fn parses_verdict_with_fences_and_normalizes_domains() {
        let raw = format!(
            "Sure:\n```json\n{}\n```",
            r#"{"intent":"x","candidates":[{"domain":"https://www.GitHub.com/","reason":"a"},{"domain":"github.com","reason":"dup"},{"domain":"localhost","reason":"private"},{"domain":"linear.app"}]}"#
        );
        let verdict = parse_verdict(&raw).unwrap();
        let domains: Vec<_> = verdict
            .candidates
            .iter()
            .map(|c| c.domain.as_str())
            .collect();
        assert_eq!(domains, ["github.com", "linear.app"]);
        assert!(parse_verdict("no json here").is_err());
    }

    #[test]
    fn user_prompt_includes_transcript_and_probed_domains() {
        let prompt =
            build_user_prompt("User: go to my github", &["modelcontextprotocol.io".into()]);
        assert!(prompt.contains("User: go to my github"));
        assert!(prompt.contains("modelcontextprotocol.io"));
        assert!(build_user_prompt("x", &[]).contains("(none)"));
    }

    #[test]
    fn anthropic_client_sends_messages_request_and_reads_text() {
        let stub =
            StubLlmServer::start(0, GITHUB_VERDICT.to_string(), Duration::from_millis(0)).unwrap();
        let llm = AnthropicLlm::new(
            "test-key".to_string(),
            "claude-opus-5-5".to_string(),
            stub.base_url().to_string(),
        );
        let reply = block_on(llm.complete(SYSTEM_PROMPT, "User: go to my github")).unwrap();
        assert_eq!(reply, GITHUB_VERDICT);
        let request = &stub.requests()[0];
        assert_eq!(request["model"], "claude-opus-5-5");
        assert_eq!(request["system"], SYSTEM_PROMPT);
        assert_eq!(request["messages"][0]["content"], "User: go to my github");
        assert_eq!(request["output_config"]["effort"], "low");
        assert!(llm.describe().contains(stub.base_url()));
    }

    #[test]
    fn nudge_only_policy_never_auto_installs() {
        let discovery = crate::fixtures::fixtures_for("modelcontextprotocol.io").remove(0);
        assert!(!NudgeOnlyPolicy.may_auto_install(&discovery, SuggestionOrigin::Background));
        assert!(!NudgeOnlyPolicy.may_auto_install(&discovery, SuggestionOrigin::Inline));
    }

    #[test]
    fn trace_markdown_lists_every_step() {
        let markdown = trace_to_markdown(
            "run",
            &[
                TraceStep::Intent {
                    text: "triage PRs".into(),
                },
                TraceStep::Decision {
                    text: "nothing to suggest".into(),
                },
            ],
        );
        assert!(markdown.contains("## Inferred intent\n\ntriage PRs"));
        assert!(markdown.contains("## Decision\n\nnothing to suggest"));
    }

    /// The background path end to end against the real
    /// `https://github.com/.well-known/ai-catalog.json`, with the reasoner's reply scripted by
    /// the stub. Opt in with `AI_CATALOG_LIVE=1`.
    #[test]
    fn stubbed_reasoner_verdict_resolves_live_github_catalog() {
        if std::env::var_os("AI_CATALOG_LIVE").is_none() {
            eprintln!("skipping live test; set AI_CATALOG_LIVE=1 to run it");
            return;
        }
        let stub =
            StubLlmServer::start(0, GITHUB_VERDICT.to_string(), Duration::from_millis(0)).unwrap();
        let llm = AnthropicLlm::new(
            "test-key".to_string(),
            "claude-opus-5-5".to_string(),
            stub.base_url().to_string(),
        );
        let reply = block_on(llm.complete(
            SYSTEM_PROMPT,
            &build_user_prompt("User: go to my github and triage my open PRs", &[]),
        ))
        .unwrap();
        let verdict = parse_verdict(&reply).unwrap();
        assert_eq!(verdict.candidates[0].domain, "github.com");

        let resolver = DefaultResolver::new(Some(HttpResolver::default()));
        let traced = block_on(resolver.resolve_traced("github.com"));
        assert_eq!(
            traced.probes[0].url,
            "https://github.com/.well-known/ai-catalog.json"
        );
        assert_eq!(traced.probes[0].outcome, "200 OK");
        assert!(
            traced
                .discoveries
                .iter()
                .any(|d| d.id == "github.com#io.github.github/github-mcp-server" && !d.fixture)
        );
    }
}
