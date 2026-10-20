//! Steps for driving AI Catalog / Server Card discovery from integration tests.

use ai_catalog::DomainObservation;
use settings::Setting as _;
use warpui::integration::TestStep;
use warpui::{App, SingletonEntity as _, async_assert};

use crate::ai::agent::api::mcp_context_for_request;
use crate::ai::ai_catalog_discovery::AiCatalogDiscoveryModel;
use crate::integration_testing::step::new_step_with_default_assertions;
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
