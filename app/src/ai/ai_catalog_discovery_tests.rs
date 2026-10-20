use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use ai::skills::SkillPathOrigin;
use ai_catalog::test_support::{FIXTURE_TOOL_NAME, FixtureCatalogServer};
use ai_catalog::{CatalogResolver, DefaultResolver, HttpResolver, InstallError};
use chrono::Local;
use instant::Instant;
use settings::Setting as _;
use warp_core::features::FeatureFlag;
use warp_multi_agent_api as api;
use warp_multi_agent_api::client_action::{Action, AddMessagesToTask, CreateTask};
use warpui::r#async::Timer;
use warpui::{App, EntityId, SingletonEntity as _};

use super::*;
use crate::ai::agent::api::mcp_context_for_request;
use crate::ai::agent::task::TaskId;
use crate::ai::agent::{AIAgentInput, UserQueryMode};
use crate::ai::blocklist::{RequestInput, ResponseStreamId};
use crate::ai::llms::LLMId;
use crate::test_util::ai_agent_tasks::create_api_task;
use crate::test_util::terminal::initialize_app_for_terminal_view;

const FIXTURE_DOMAIN: &str = "docs.fixture-catalog.dev";

fn setup(app: &mut App, mode: AiCatalogDiscoveryMode, resolver: Arc<dyn CatalogResolver>) {
    initialize_app_for_terminal_view(app);
    app.add_singleton_model(|_| simple_logger::manager::LogManager::new());
    AISettings::handle(app).update(app, |settings, ctx| {
        settings
            .ai_catalog_discovery_mode
            .set_value(mode, ctx)
            .expect("discovery mode should be settable");
    });
    app.add_singleton_model(|ctx| AiCatalogDiscoveryModel::new_with_resolver(resolver, ctx));
}

fn fixture_resolver(server: &FixtureCatalogServer) -> Arc<dyn CatalogResolver> {
    Arc::new(
        HttpResolver::new(http_client::Client::new_external())
            .with_origin_override(FIXTURE_DOMAIN, server.origin()),
    )
}

fn record_events(app: &mut App) -> Rc<RefCell<Vec<AiCatalogDiscoveryEvent>>> {
    let events = Rc::new(RefCell::new(Vec::new()));
    let recorded = events.clone();
    app.update(|ctx| {
        ctx.subscribe_to_model(&AiCatalogDiscoveryModel::handle(ctx), move |_, event, _| {
            recorded.borrow_mut().push(event.clone());
        });
    });
    events
}

async fn wait_until(description: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}"
        );
        Timer::after(Duration::from_millis(20)).await;
    }
}

fn pending_ids(app: &App) -> Vec<String> {
    AiCatalogDiscoveryModel::handle(app).read(app, |model, _| {
        model.pending().iter().map(|d| d.id.clone()).collect()
    })
}

fn web_fetch_message(id: &str, urls: &[&str]) -> api::Message {
    use api::message::web_fetch::{Status, status};
    api::Message {
        id: id.to_string(),
        task_id: "root".to_string(),
        request_id: "req-1".to_string(),
        message: Some(api::message::Message::WebFetch(api::message::WebFetch {
            status: Some(Status {
                r#type: Some(status::Type::Fetching(status::Fetching {
                    urls: urls.iter().map(|url| url.to_string()).collect(),
                })),
            }),
        })),
        ..Default::default()
    }
}

fn shell_command_message(id: &str, command: &str) -> api::Message {
    api::Message {
        id: id.to_string(),
        task_id: "root".to_string(),
        request_id: "req-1".to_string(),
        message: Some(api::message::Message::ToolCall(api::message::ToolCall {
            tool_call_id: format!("call-{id}"),
            tool: Some(api::message::tool_call::Tool::RunShellCommand(
                api::message::tool_call::RunShellCommand {
                    command: command.to_string(),
                    ..Default::default()
                },
            )),
            ..Default::default()
        })),
        ..Default::default()
    }
}

/// Streams `messages` into a new agent conversation through the same client-action path the
/// server response stream uses.
fn stream_agent_messages(app: &mut App, messages: Vec<api::Message>) {
    let terminal_surface_id = EntityId::new();
    BlocklistAIHistoryModel::handle(app).update(app, |history, ctx| {
        let conversation_id =
            history.start_new_conversation(terminal_surface_id, false, false, false, ctx);
        let conversation = history
            .conversation_mut(&conversation_id)
            .expect("conversation exists");
        let stream_id = ResponseStreamId::new_for_test();
        let input = RequestInput {
            conversation_id,
            input_messages: Vec::from([(
                conversation.get_root_task_id().clone(),
                vec![AIAgentInput::UserQuery {
                    query: "what does modelcontextprotocol.io say about elicitation?".to_string(),
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
            .update_for_new_request_input(input, stream_id.clone(), terminal_surface_id, ctx)
            .expect("request input should apply");
        conversation
            .initialize_output_for_response_stream(
                &stream_id,
                api::response_event::StreamInit {
                    request_id: "req-1".to_string(),
                    conversation_id: "server-conversation".to_string(),
                    run_id: String::new(),
                },
                terminal_surface_id,
                ctx,
            )
            .expect("stream init should apply");
        conversation
            .apply_client_action(
                &stream_id,
                terminal_surface_id,
                Action::CreateTask(CreateTask {
                    task: Some(create_api_task("root", vec![])),
                }),
                &SkillPathOrigin::Unavailable,
                ctx,
            )
            .expect("create task should apply");
        conversation
            .apply_client_action(
                &stream_id,
                terminal_surface_id,
                Action::AddMessagesToTask(AddMessagesToTask {
                    task_id: "root".to_string(),
                    messages,
                }),
                &SkillPathOrigin::Unavailable,
                ctx,
            )
            .expect("add messages should apply");
    });
}

fn request_tool_names(app: &App) -> Vec<String> {
    app.read(|ctx| {
        mcp_context_for_request(None, ctx)
            .map(|context| {
                context
                    .servers
                    .iter()
                    .flat_map(|server| server.tools.iter().map(|tool| tool.name.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// The headline flow: a web fetch streamed from the agent touches a domain, discovery probes its
/// AI Catalog over HTTP, one install spawns the advertised MCP server, and its tool is part of
/// the MCP context sent with the next agent request.
#[test]
fn web_fetch_discovery_installs_server_into_next_request() {
    let _grouped = FeatureFlag::MCPGroupedServerContext.override_enabled(true);
    App::test((), |mut app| async move {
        let server = FixtureCatalogServer::start(0, None).unwrap();
        setup(
            &mut app,
            AiCatalogDiscoveryMode::LightTouch,
            fixture_resolver(&server),
        );
        let events = record_events(&mut app);
        assert!(!request_tool_names(&app).contains(&FIXTURE_TOOL_NAME.to_string()));

        stream_agent_messages(
            &mut app,
            vec![web_fetch_message(
                "fetch-1",
                &[&format!("https://{FIXTURE_DOMAIN}/guide/elicitation")],
            )],
        );
        wait_until("discovery", || pending_ids(&app).len() == 2).await;
        let server_id = format!("{FIXTURE_DOMAIN}#dev.warp.fixture/docs");
        assert!(pending_ids(&app).contains(&server_id));

        let installation_uuid = AiCatalogDiscoveryModel::handle(&app)
            .update(&mut app, |model, ctx| model.install(&server_id, None, ctx))
            .expect("auth-less server installs without a secret");

        wait_until("server to run", || {
            events
                .borrow()
                .iter()
                .any(|event| matches!(event, AiCatalogDiscoveryEvent::Installed { .. }))
        })
        .await;
        let installed = events.borrow().iter().find_map(|event| match event {
            AiCatalogDiscoveryEvent::Installed {
                installation_uuid,
                tool_names,
                ..
            } => Some((*installation_uuid, tool_names.clone())),
            _ => None,
        });
        assert_eq!(
            installed,
            Some((installation_uuid, vec![FIXTURE_TOOL_NAME.to_string()]))
        );
        assert!(request_tool_names(&app).contains(&FIXTURE_TOOL_NAME.to_string()));
        assert!(!pending_ids(&app).contains(&server_id));
    });
}

#[test]
fn light_touch_ignores_shell_commands_but_full_probes_them() {
    for (mode, expected) in [
        (AiCatalogDiscoveryMode::Off, 0),
        (AiCatalogDiscoveryMode::LightTouch, 0),
        (AiCatalogDiscoveryMode::Full, 2),
    ] {
        App::test((), |mut app| async move {
            setup(&mut app, mode, Arc::new(DefaultResolver::new(None)));
            stream_agent_messages(
                &mut app,
                vec![shell_command_message(
                    "shell-1",
                    "curl -s -o /dev/null -w '%{http_code}' https://modelcontextprotocol.io/mcp",
                )],
            );
            if expected > 0 {
                wait_until("fixture discovery", || pending_ids(&app).len() == expected).await;
            } else {
                Timer::after(Duration::from_millis(200)).await;
            }
            assert_eq!(pending_ids(&app).len(), expected, "mode {mode:?}");
        });
    }
}

#[test]
fn off_mode_never_probes_web_fetches() {
    App::test((), |mut app| async move {
        setup(
            &mut app,
            AiCatalogDiscoveryMode::Off,
            Arc::new(DefaultResolver::new(None)),
        );
        stream_agent_messages(
            &mut app,
            vec![web_fetch_message(
                "fetch-1",
                &["https://modelcontextprotocol.io"],
            )],
        );
        Timer::after(Duration::from_millis(200)).await;
        assert!(pending_ids(&app).is_empty());
    });
}

/// Header-auth cards need a secret; it is sent as the card's header, and a rejected secret
/// returns the discovery to the pending list so the user can retry.
#[test]
fn header_auth_server_requires_and_sends_secret() {
    let _grouped = FeatureFlag::MCPGroupedServerContext.override_enabled(true);
    App::test((), |mut app| async move {
        let server = FixtureCatalogServer::start(0, Some("s3cret".to_string())).unwrap();
        setup(
            &mut app,
            AiCatalogDiscoveryMode::Full,
            fixture_resolver(&server),
        );
        let events = record_events(&mut app);
        stream_agent_messages(
            &mut app,
            vec![shell_command_message(
                "shell-1",
                &format!("git clone https://{FIXTURE_DOMAIN}/repo.git"),
            )],
        );
        wait_until("discovery", || pending_ids(&app).len() == 2).await;
        let server_id = format!("{FIXTURE_DOMAIN}#dev.warp.fixture/docs");

        let missing_secret = AiCatalogDiscoveryModel::handle(&app)
            .update(&mut app, |model, ctx| model.install(&server_id, None, ctx));
        assert_eq!(
            missing_secret,
            Err(DiscoveryInstallError::Plan(InstallError::SecretRequired {
                header: "Authorization".to_string()
            }))
        );

        AiCatalogDiscoveryModel::handle(&app)
            .update(&mut app, |model, ctx| {
                model.install(&server_id, Some("wrong"), ctx)
            })
            .expect("install starts");
        wait_until("failed install", || {
            events
                .borrow()
                .iter()
                .any(|event| matches!(event, AiCatalogDiscoveryEvent::InstallFailed { .. }))
        })
        .await;
        assert!(pending_ids(&app).contains(&server_id));

        AiCatalogDiscoveryModel::handle(&app)
            .update(&mut app, |model, ctx| {
                model.install(&server_id, Some("s3cret"), ctx)
            })
            .expect("install starts");
        wait_until("authenticated install", || {
            events
                .borrow()
                .iter()
                .any(|event| matches!(event, AiCatalogDiscoveryEvent::Installed { .. }))
        })
        .await;
        assert!(
            server
                .authorization_headers()
                .contains(&"Bearer s3cret".to_string())
        );
        assert!(request_tool_names(&app).contains(&FIXTURE_TOOL_NAME.to_string()));
    });
}
