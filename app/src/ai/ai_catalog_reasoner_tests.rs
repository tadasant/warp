use std::sync::Arc;
use std::time::Duration;

use ai_catalog::reasoner::{AnthropicLlm, ReasonerLlm};
use ai_catalog::test_support::{FixtureCatalogServer, StubLlmServer};
use ai_catalog::{CatalogResolver, DefaultResolver, HttpResolver};
use chrono::Local;
use instant::Instant;
use settings::Setting as _;
use warpui::r#async::Timer;
use warpui::{App, EntityId, SingletonEntity as _};

use super::*;
use crate::ai::agent::UserQueryMode;
use crate::ai::blocklist::{RequestInput, ResponseStreamId};
use crate::ai::llms::LLMId;
use crate::ai::mcp::FileMCPWatcher;
use crate::ai::mcp::file_based_manager::FileBasedMCPManager;
use crate::settings::AiCatalogDiscoveryMode;
use crate::test_util::terminal::initialize_app_for_terminal_view;

const GITHUB_VERDICT: &str = r#"{"intent": "Triage the user's open GitHub pull requests.", "candidates": [{"domain": "github.com", "reason": "GitHub publishes the official GitHub MCP server"}]}"#;
const NO_CANDIDATES_VERDICT: &str = r#"{"intent": "Rename a local variable.", "candidates": []}"#;
const GITHUB_SERVER_ID: &str = "github.com#io.github.github/github-mcp-server";

fn setup(
    app: &mut App,
    background_enabled: bool,
    llm: Option<Arc<dyn ReasonerLlm>>,
    resolver: Arc<dyn CatalogResolver>,
) {
    initialize_app_for_terminal_view(app);
    app.add_singleton_model(|_| simple_logger::manager::LogManager::new());
    app.add_singleton_model(|_| FileMCPWatcher::new_inert());
    app.add_singleton_model(FileBasedMCPManager::new);
    AISettings::handle(app).update(app, |settings, ctx| {
        settings
            .ai_catalog_discovery_mode
            .set_value(AiCatalogDiscoveryMode::Off, ctx)
            .unwrap();
        settings
            .ai_catalog_background_discovery_enabled
            .set_value(background_enabled, ctx)
            .unwrap();
    });
    let discovery_resolver = resolver.clone();
    app.add_singleton_model(|ctx| {
        AiCatalogDiscoveryModel::new_with_resolver(discovery_resolver, ctx)
    });
    app.add_singleton_model(|ctx| AiCatalogReasonerModel::new_with(llm, resolver, ctx));
}

fn stub_llm(reply: &str) -> (StubLlmServer, Arc<dyn ReasonerLlm>) {
    let stub = StubLlmServer::start(0, reply.to_string(), Duration::from_millis(50)).unwrap();
    let llm = AnthropicLlm::new(
        "stub-key".to_string(),
        "claude-opus-5-5".to_string(),
        stub.base_url().to_string(),
    );
    (stub, Arc::new(llm))
}

/// Submits `query` as a new user prompt, the way the agent input does.
fn submit_user_query(app: &mut App, query: &str) {
    let terminal_surface_id = EntityId::new();
    BlocklistAIHistoryModel::handle(app).update(app, |history, ctx| {
        let conversation_id =
            history.start_new_conversation(terminal_surface_id, false, false, false, ctx);
        let conversation = history
            .conversation_mut(&conversation_id)
            .expect("conversation exists");
        let input = RequestInput {
            conversation_id,
            input_messages: Vec::from([(
                conversation.get_root_task_id().clone(),
                vec![AIAgentInput::UserQuery {
                    query: query.to_string(),
                    context: Default::default(),
                    static_query_type: None,
                    referenced_attachments: Default::default(),
                    user_query_mode: UserQueryMode::Normal,
                    running_command: None,
                    intended_agent: None,
                    base: None,
                }],
            )]),
            working_directory: None,
            model_id: LLMId::from("test-model"),
            coding_model_id: LLMId::from("test-coding-model"),
            cli_agent_model_id: LLMId::from("test-cli-agent-model"),
            computer_use_model_id: LLMId::from("test-computer-use-model"),
            shared_session_response_initiator: None,
            request_start_ts: Local::now(),
            supported_tools_override: None,
        };
        conversation
            .update_for_new_request_input(
                input,
                ResponseStreamId::new_for_test(),
                terminal_surface_id,
                ctx,
            )
            .expect("request input should apply");
    });
}

fn latest_run(app: &App) -> Option<ReasonerRun> {
    AiCatalogReasonerModel::handle(app).read(app, |model, _| model.runs().last().cloned())
}

async fn wait_for_finished_run(app: &App) -> ReasonerRun {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(run) = latest_run(app)
            && run.status != ReasonerRunStatus::Running
        {
            return run;
        }
        assert!(Instant::now() < deadline, "background run did not finish");
        Timer::after(Duration::from_millis(20)).await;
    }
}

fn pending_ids(app: &App) -> Vec<String> {
    AiCatalogDiscoveryModel::handle(app).read(app, |model, _| {
        model.pending().iter().map(|d| d.id.clone()).collect()
    })
}

/// A user prompt starts a background run: the stubbed reasoner names github.com, the run probes
/// a stand-in for github.com's catalog, and the GitHub server is suggested above the input with
/// the reasoner's rationale, without being installed.
#[test]
fn user_prompt_runs_reasoner_and_suggests_inferred_server() {
    App::test((), |mut app| async move {
        let catalog = FixtureCatalogServer::start(0, None).unwrap();
        let resolver: Arc<dyn CatalogResolver> = Arc::new(
            HttpResolver::new(http_client::Client::new_external())
                .with_origin_override("github.com", catalog.origin()),
        );
        let (stub, llm) = stub_llm(GITHUB_VERDICT);
        setup(&mut app, true, Some(llm), resolver);

        submit_user_query(&mut app, "go to my github and triage my open PRs");
        let running = latest_run(&app).expect("a run starts as soon as the prompt is submitted");
        assert_eq!(running.status, ReasonerRunStatus::Running);

        let run = wait_for_finished_run(&app).await;
        assert_eq!(run.status, ReasonerRunStatus::Finished);
        let request = &stub.requests()[0];
        assert!(
            request["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("User: go to my github and triage my open PRs")
        );
        assert!(run.steps.iter().any(|step| matches!(
            step,
            TraceStep::Probe { domain, probes } if domain == "github.com"
                && probes[0].url.ends_with("/.well-known/ai-catalog.json")
                && probes[0].outcome == "200 OK"
        )));
        assert!(
            run.summary
                .as_deref()
                .unwrap()
                .starts_with("Suggested Fixture Docs")
        );

        let fixture_id = "github.com#dev.warp.fixture/docs";
        assert!(pending_ids(&app).contains(&fixture_id.to_string()));
        let reason = AiCatalogDiscoveryModel::handle(&app).read(&app, |model, _| {
            model.suggestion_reason(fixture_id).map(str::to_string)
        });
        assert_eq!(
            reason.as_deref(),
            Some("background: GitHub publishes the official GitHub MCP server")
        );
        assert!(no_discovered_servers_active(&app));

        let markdown = run.trace_markdown();
        for section in [
            "## Trigger",
            "## Reasoner prompt",
            "## Reasoner output",
            "## Inferred intent",
            "## Candidate domains",
            "## Probe `github.com`",
            "## Decision",
        ] {
            assert!(markdown.contains(section), "trace is missing {section}");
        }
    });
}

fn no_discovered_servers_active(app: &App) -> bool {
    app.read(|ctx| {
        crate::ai::mcp::TemplatableMCPServerManager::as_ref(ctx)
            .get_active_discovered_servers()
            .is_empty()
    })
}

#[test]
fn no_relevant_vendor_records_nothing_to_suggest() {
    App::test((), |mut app| async move {
        let (_stub, llm) = stub_llm(NO_CANDIDATES_VERDICT);
        setup(
            &mut app,
            true,
            Some(llm),
            Arc::new(DefaultResolver::new(None)),
        );
        submit_user_query(&mut app, "rename foo to bar in main.rs");
        let run = wait_for_finished_run(&app).await;
        assert!(
            run.summary
                .as_deref()
                .unwrap()
                .starts_with("Nothing to suggest")
        );
        assert!(pending_ids(&app).is_empty());
    });
}

#[test]
fn disabled_background_discovery_never_runs() {
    App::test((), |mut app| async move {
        let (stub, llm) = stub_llm(GITHUB_VERDICT);
        setup(
            &mut app,
            false,
            Some(llm),
            Arc::new(DefaultResolver::new(None)),
        );
        submit_user_query(&mut app, "go to my github and triage my open PRs");
        Timer::after(Duration::from_millis(200)).await;
        assert!(latest_run(&app).is_none());
        assert!(stub.requests().is_empty());
    });
}

#[test]
fn missing_key_records_actionable_error() {
    App::test((), |mut app| async move {
        setup(&mut app, true, None, Arc::new(DefaultResolver::new(None)));
        submit_user_query(&mut app, "go to my github");
        let run = latest_run(&app).expect("run is recorded");
        assert_eq!(run.status, ReasonerRunStatus::Failed);
        assert!(
            run.summary
                .as_deref()
                .unwrap()
                .contains(ai_catalog::reasoner::API_KEY_ENV)
        );
    });
}

/// Against the real `https://github.com/.well-known/ai-catalog.json`, with the reasoner's reply
/// scripted by the stub. Opt in with `AI_CATALOG_LIVE=1`.
#[test]
fn live_github_catalog_suggests_github_mcp_server() {
    if std::env::var_os("AI_CATALOG_LIVE").is_none() {
        eprintln!("skipping live test; set AI_CATALOG_LIVE=1 to run it");
        return;
    }
    App::test((), |mut app| async move {
        let (_stub, llm) = stub_llm(GITHUB_VERDICT);
        setup(
            &mut app,
            true,
            Some(llm),
            Arc::new(DefaultResolver::new(Some(HttpResolver::default()))),
        );
        submit_user_query(&mut app, "go to my github and triage my open PRs");
        let run = wait_for_finished_run(&app).await;
        assert!(run.steps.iter().any(|step| matches!(
            step,
            TraceStep::Probe { probes, .. }
                if probes[0].url == "https://github.com/.well-known/ai-catalog.json"
                    && probes[0].outcome == "200 OK"
        )));
        assert_eq!(
            run.summary
                .as_deref()
                .map(|s| s.starts_with("Suggested GitHub")),
            Some(true)
        );
        assert!(pending_ids(&app).contains(&GITHUB_SERVER_ID.to_string()));
    });
}
