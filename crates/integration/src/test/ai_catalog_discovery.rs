//! AI Catalog / Server Card discovery (SEP-2127): nudge, install, auth entry and settings.
//!
//! Agent web fetches normally stream from warp-server, so this test feeds discovery a synthetic
//! web-fetch observation instead. Catalog resolution, the toast nudge, the secret-entry modal, the
//! MCP server spawn and the request tool list are all real. It needs network access to the live
//! `https://modelcontextprotocol.io/mcp` endpoint and is meant to run manually with a real display
//! to capture screenshots:
//!
//! ```sh
//! WARPUI_USE_REAL_DISPLAY_IN_INTEGRATION_TESTS=1 \
//!   cargo run -p integration --bin integration -- test_ai_catalog_discovery
//! ```

use std::time::Duration;

use ai_catalog::test_support::{FIXTURE_TOOL_NAME, FixtureCatalogServer};
use warp::integration_testing::ai_catalog::{
    assert_tool_in_next_agent_request, install_discovered_server, observe_agent_web_fetch,
    set_ai_catalog_discovery_mode_full,
};
use warp::integration_testing::settings::{open_settings_page, type_settings_search};
use warp::integration_testing::step::new_step_with_default_assertions;
use warp::integration_testing::terminal::wait_until_bootstrapped_single_pane_for_tab;
use warp::settings_view::SettingsSection;
use warpui_core::integration::TestStep;

use crate::Builder;

const FIXTURE_DOMAIN: &str = "docs.fixture-catalog.dev";
const FIXTURE_TOKEN: &str = "demo-token";

fn pause(name: &str, screenshot: &str) -> TestStep {
    TestStep::new(name)
        .set_timeout(Duration::from_secs(30))
        .set_post_step_pause(Duration::from_secs(2))
        .with_take_screenshot(screenshot)
}

pub fn test_ai_catalog_discovery() -> Builder {
    Builder::new()
        .with_setup(|utils| {
            let server = FixtureCatalogServer::start(0, Some(FIXTURE_TOKEN.to_string()))
                .expect("fixture catalog server should start");
            utils.set_env(
                "WARP_AI_CATALOG_ORIGIN_OVERRIDES",
                Some(format!("{FIXTURE_DOMAIN}={}", server.origin())),
            );
            // Serves for the lifetime of the test process.
            std::mem::forget(server);
        })
        .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
        .with_step(set_ai_catalog_discovery_mode_full())
        .with_step(observe_agent_web_fetch(
            "https://modelcontextprotocol.io/specification/2025-06-18",
            2,
        ))
        .with_step(pause(
            "Nudges for the MCP docs server and its Skills",
            "01-nudges.png",
        ))
        .with_step(install_discovered_server(
            "modelcontextprotocol.io#io.modelcontextprotocol/docs-server",
        ))
        .with_step(
            assert_tool_in_next_agent_request("search_model_context_protocol")
                .set_timeout(Duration::from_secs(60)),
        )
        .with_step(pause("Live docs server is active", "02-installed.png"))
        .with_step(observe_agent_web_fetch(
            "https://docs.fixture-catalog.dev/guide",
            3,
        ))
        .with_step(pause(
            "Nudge for the auth-required server",
            "03-auth-nudge.png",
        ))
        .with_step(install_discovered_server(
            "docs.fixture-catalog.dev#dev.warp.fixture/docs",
        ))
        .with_step(pause("Secret entry modal", "04-auth-modal.png"))
        .with_step(
            new_step_with_default_assertions("Enter the token and connect")
                .with_typed_characters(&[FIXTURE_TOKEN])
                .with_keystrokes(&["enter"]),
        )
        .with_step(
            assert_tool_in_next_agent_request(FIXTURE_TOOL_NAME)
                .set_timeout(Duration::from_secs(30)),
        )
        .with_step(pause(
            "Authenticated server is active",
            "05-auth-installed.png",
        ))
        .with_step(open_settings_page(SettingsSection::WarpAgent))
        .with_step(type_settings_search("Server Card"))
        .with_step(pause("Discovery settings", "06-settings.png"))
}
