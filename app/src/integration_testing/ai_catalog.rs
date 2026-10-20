//! Steps for driving AI Catalog / Server Card discovery from integration tests.

use ai_catalog::DomainObservation;
use settings::Setting as _;
use warp_multi_agent_api as api;
use warpui::integration::TestStep;
use warpui::{App, SingletonEntity as _, async_assert};

use crate::ai::agent::api::mcp_context_for_request;
use crate::ai::ai_catalog_discovery::AiCatalogDiscoveryModel;
use crate::ai::ai_catalog_reasoner::{AiCatalogReasonerModel, ReasonerRunStatus};
use crate::integration_testing::step::new_step_with_default_assertions;
use crate::integration_testing::view_getters::single_terminal_view_for_tab;
use crate::settings::{AISettings, AiCatalogDiscoveryMode};
use crate::workspace::{Workspace, WorkspaceAction};

pub fn set_ai_catalog_discovery_mode_full() -> TestStep {
    new_step_with_default_assertions("Set Server Card discovery to Full").with_action(
        |app, _, _| {
            AISettings::handle(app).update(app, |settings, ctx| {
                settings
                    .ai_catalog_discovery_mode
                    .set_value(AiCatalogDiscoveryMode::Full, ctx)
                    .expect("discovery mode should be settable");
            });
        },
    )
}

/// Feeds a synthetic agent web fetch of `url` to discovery, as if the agent had fetched it, and
/// waits until `expected_pending` discoveries are pending.
pub fn observe_agent_web_fetch(url: &'static str, expected_pending: usize) -> TestStep {
    new_step_with_default_assertions(&format!("Agent web fetch of {url}"))
        .with_action(move |app, _, _| {
            let urls = vec![url.to_string()];
            AiCatalogDiscoveryModel::handle(app).update(app, |model, ctx| {
                model.observe(&DomainObservation::WebFetch { urls: &urls }, ctx);
            });
        })
        .add_named_assertion(
            format!("{expected_pending} discoveries pending"),
            move |app, _| {
                let pending = pending_discovery_ids(app);
                async_assert!(
                    pending.len() == expected_pending,
                    "expected {expected_pending} pending discoveries, got {pending:?}"
                )
            },
        )
}

/// Clicks the nudge's install action for `discovery_id`.
pub fn install_discovered_server(discovery_id: &'static str) -> TestStep {
    new_step_with_default_assertions(&format!("Install {discovery_id}")).with_action(
        move |app, window_id, _| {
            let workspace_view_id = app
                .views_of_type::<Workspace>(window_id)
                .and_then(|views| views.first().map(|view| view.id()))
                .expect("no workspace view");
            app.dispatch_typed_action(
                window_id,
                &[workspace_view_id],
                &WorkspaceAction::InstallDiscoveredMcpServer {
                    discovery_id: discovery_id.to_string(),
                },
            );
        },
    )
}

/// Asserts the next agent request's MCP context advertises `tool_name`.
pub fn assert_tool_in_next_agent_request(tool_name: &'static str) -> TestStep {
    new_step_with_default_assertions(&format!("{tool_name} is in the next agent request"))
        .add_named_assertion(format!("{tool_name} advertised"), move |app, _| {
            let tool_names = request_tool_names(app);
            async_assert!(
                tool_names.iter().any(|name| name == tool_name),
                "expected {tool_name} in request tools, got {tool_names:?}"
            )
        })
}

fn pending_discovery_ids(app: &App) -> Vec<String> {
    AiCatalogDiscoveryModel::handle(app).read(app, |model, _| {
        model
            .pending()
            .iter()
            .map(|discovery| discovery.id.clone())
            .collect()
    })
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

/// One synthetic agent turn for demos: the user's prompt, the page the agent fetched (if any), and
/// a short agent reply.
pub struct SyntheticAgentTurn {
    pub prompt: &'static str,
    /// `(url, title)` of a page the agent fetched.
    pub fetched_page: Option<(&'static str, &'static str)>,
    pub reply: &'static str,
}

/// Replaces the prompt typed into the input with a rendered conversation of `turns` (through the
/// same restore path real conversations use), then feeds the last turn's fetch (if any) to
/// discovery and waits for `expected_pending` discoveries. Agent turns need warp-server, so the responses are
/// synthetic.
pub fn render_synthetic_agent_turns(
    turns: &'static [SyntheticAgentTurn],
    expected_pending: usize,
) -> TestStep {
    new_step_with_default_assertions("Render the synthetic agent turn")
        .with_action(move |app, window_id, _| {
            let terminal_view = single_terminal_view_for_tab(app, window_id, 0);
            terminal_view.update(app, |view, ctx| {
                view.input().update(ctx, |input, ctx| {
                    input.clear_buffer_and_reset_undo_stack(ctx);
                });
                view.load_conversation_from_tasks(synthetic_conversation(turns), ctx);
            });
            if let Some((url, _)) = turns.last().and_then(|turn| turn.fetched_page) {
                let urls = vec![url.to_string()];
                AiCatalogDiscoveryModel::handle(app).update(app, |model, ctx| {
                    model.observe(&DomainObservation::WebFetch { urls: &urls }, ctx);
                });
            }
        })
        .add_named_assertion(
            format!("{expected_pending} discoveries pending"),
            move |app, _| {
                let pending = pending_discovery_ids(app);
                async_assert!(
                    pending.len() == expected_pending,
                    "expected {expected_pending} pending discoveries, got {pending:?}"
                )
            },
        )
}

fn synthetic_conversation(turns: &[SyntheticAgentTurn]) -> api::ConversationData {
    use api::message::web_fetch::{Status, status};

    let task_id = "synthetic-demo-task";
    let mut messages = Vec::new();
    for (index, turn) in turns.iter().enumerate() {
        let request_id = format!("synthetic-demo-request-{index}");
        let wrap = |suffix: &str, message| api::Message {
            id: format!("synthetic-demo-{index}-{suffix}"),
            task_id: task_id.to_string(),
            request_id: request_id.clone(),
            message: Some(message),
            ..Default::default()
        };
        messages.push(wrap(
            "query",
            api::message::Message::UserQuery(api::message::UserQuery {
                query: turn.prompt.to_string(),
                ..Default::default()
            }),
        ));
        if let Some((url, title)) = turn.fetched_page {
            messages.push(wrap(
                "fetch",
                api::message::Message::WebFetch(api::message::WebFetch {
                    status: Some(Status {
                        r#type: Some(status::Type::Success(status::Success {
                            pages: vec![status::success::FetchedPage {
                                url: url.to_string(),
                                title: title.to_string(),
                                success: true,
                            }],
                        })),
                    }),
                }),
            ));
        }
        messages.push(wrap(
            "output",
            api::message::Message::AgentOutput(api::message::AgentOutput {
                text: turn.reply.to_string(),
            }),
        ));
    }
    api::ConversationData {
        tasks: vec![api::Task {
            id: task_id.to_string(),
            messages,
            dependencies: None,
            description: String::new(),
            summary: String::new(),
            server_data: String::new(),
        }],
        ..Default::default()
    }
}

/// Turns inline discovery off and background discovery on, so the two can be demoed separately.
pub fn set_background_discovery_only() -> TestStep {
    new_step_with_default_assertions("Switch Server Card discovery to background only").with_action(
        |app, _, _| {
            AISettings::handle(app).update(app, |settings, ctx| {
                settings
                    .ai_catalog_discovery_mode
                    .set_value(AiCatalogDiscoveryMode::Off, ctx)
                    .expect("discovery mode should be settable");
                settings
                    .ai_catalog_background_discovery_enabled
                    .set_value(true, ctx)
                    .expect("background discovery should be settable");
            });
        },
    )
}

/// Starts a background run for `user_query` as if the user had just submitted it, and waits for
/// the running indicator.
pub fn start_background_discovery(user_query: &'static str) -> TestStep {
    new_step_with_default_assertions(&format!("Background discovery for {user_query:?}"))
        .with_action(move |app, _, _| {
            AiCatalogReasonerModel::handle(app).update(app, |model, ctx| {
                model.start_run(user_query.to_string(), format!("User: {user_query}"), ctx);
            });
        })
        .add_named_assertion("background run is in progress", |app, _| {
            let status = latest_run_status(app);
            async_assert!(
                status == Some(ReasonerRunStatus::Running),
                "expected a running background run, got {status:?}"
            )
        })
}

pub fn wait_for_background_discovery(expected_pending_id: &'static str) -> TestStep {
    new_step_with_default_assertions("Background discovery finishes")
        .add_named_assertion("background run finished", |app, _| {
            let status = latest_run_status(app);
            async_assert!(
                status == Some(ReasonerRunStatus::Finished),
                "expected a finished background run, got {status:?}"
            )
        })
        .add_named_assertion(format!("{expected_pending_id} suggested"), move |app, _| {
            let pending = pending_discovery_ids(app);
            async_assert!(
                pending.iter().any(|id| id == expected_pending_id),
                "expected {expected_pending_id} pending, got {pending:?}"
            )
        })
}

/// Clicks "View trace" for the latest background run.
pub fn open_latest_background_trace() -> TestStep {
    new_step_with_default_assertions("Open the background discovery trace").with_action(
        |app, window_id, _| {
            let Some(run_id) = AiCatalogReasonerModel::handle(app)
                .read(app, |model, _| model.runs().last().map(|run| run.id))
            else {
                return;
            };
            let workspace_view_id = app
                .views_of_type::<Workspace>(window_id)
                .and_then(|views| views.first().map(|view| view.id()))
                .expect("no workspace view");
            app.dispatch_typed_action(
                window_id,
                &[workspace_view_id],
                &WorkspaceAction::OpenAiCatalogDiscoveryTrace { run_id },
            );
        },
    )
}

fn latest_run_status(app: &App) -> Option<ReasonerRunStatus> {
    AiCatalogReasonerModel::handle(app)
        .read(app, |model, _| model.runs().last().map(|run| run.status))
}
