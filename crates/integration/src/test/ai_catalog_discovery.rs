//! AI Catalog / Server Card discovery (SEP-2127): nudge, install, auth entry and settings.
//!
//! Agent turns need warp-server, so each demo prompt is typed into the real input and then
//! rendered with a synthetic agent response through the same restore path real conversations use;
//! discovery is fed the turn's web fetch. Catalog resolution (live for github.com), the nudge above
//! the input, the secret-entry modal, the MCP server spawn and the request tool list are all real.
//! It needs network access to github.com and the live `https://modelcontextprotocol.io/mcp`
//! endpoint, and is meant to run manually with a real display to capture screenshots:
//!
//! ```sh
//! WARPUI_USE_REAL_DISPLAY_IN_INTEGRATION_TESTS=1 \
//!   cargo run -p integration --bin integration -- test_ai_catalog_discovery
//! ```

use std::time::Duration;

use ai_catalog::test_support::{FIXTURE_TOOL_NAME, FixtureCatalogServer};
use warp::integration_testing::ai_catalog::{
    SyntheticAgentTurn, assert_tool_in_next_agent_request, install_discovered_server,
    render_synthetic_agent_turns, set_ai_catalog_discovery_mode_full,
};
use warp::integration_testing::settings::{open_settings_page, type_settings_search};
use warp::integration_testing::step::new_step_with_default_assertions;
use warp::integration_testing::terminal::wait_until_bootstrapped_single_pane_for_tab;
use warp::settings_view::SettingsSection;
use warpui_core::integration::TestStep;

use crate::Builder;

const FIXTURE_DOMAIN: &str = "docs.fixture-catalog.dev";
const FIXTURE_TOKEN: &str = "demo-token";

const SERVERS_TURN: SyntheticAgentTurn = SyntheticAgentTurn {
    prompt: "Read https://github.com/modelcontextprotocol/servers and summarise what it is",
    fetched_page: Some((
        "https://github.com/modelcontextprotocol/servers",
        "modelcontextprotocol/servers: Model Context Protocol Servers",
    )),
    reply: "(synthetic agent reply) It's the reference collection of Model Context Protocol \
            servers maintained by the MCP project.",
};
const ELICITATION_TURN: SyntheticAgentTurn = SyntheticAgentTurn {
    prompt: "Now check the MCP spec for how elicitation works",
    fetched_page: Some((
        "https://modelcontextprotocol.io/specification/2025-06-18/client/elicitation",
        "Elicitation - Model Context Protocol",
    )),
    reply: "(synthetic agent reply) Elicitation lets a server ask the user for structured input \
            through the client.",
};
const FIXTURE_TURN: SyntheticAgentTurn = SyntheticAgentTurn {
    prompt: "Open the guide at https://docs.fixture-catalog.dev/guide",
    fetched_page: Some((
        "https://docs.fixture-catalog.dev/guide",
        "Fixture Docs guide",
    )),
    reply: "(synthetic agent reply) Fetched the fixture guide.",
};

static FIRST_TURN: [SyntheticAgentTurn; 1] = [SERVERS_TURN];
static FIRST_TWO_TURNS: [SyntheticAgentTurn; 2] = [SERVERS_TURN, ELICITATION_TURN];
static ALL_TURNS: [SyntheticAgentTurn; 3] = [SERVERS_TURN, ELICITATION_TURN, FIXTURE_TURN];

fn pause(name: &str, screenshot: &str) -> TestStep {
    TestStep::new(name)
        .set_timeout(Duration::from_secs(30))
        .set_post_step_pause(Duration::from_secs(2))
        .with_take_screenshot(screenshot)
}

/// Types `prompt` into the input a few words at a time and holds it on screen long enough to read.
fn type_prompt(mut builder: Builder, prompt: &str) -> Builder {
    let words: Vec<&str> = prompt.split_inclusive(' ').collect();
    for chunk in words.chunks(3) {
        let chunk = chunk.concat();
        builder = builder.with_step(
            new_step_with_default_assertions(&format!("Type {chunk:?}"))
                .with_typed_characters(&[chunk.as_str()])
                .set_post_step_pause(Duration::from_millis(250)),
        );
    }
    builder.with_step(
        TestStep::new("Hold the typed prompt on screen")
            .set_timeout(Duration::from_secs(10))
            .set_post_step_pause(Duration::from_millis(2500)),
    )
}

pub fn test_ai_catalog_discovery() -> Builder {
    let builder = Builder::new()
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
        .with_step(set_ai_catalog_discovery_mode_full());
    let builder = type_prompt(builder, SERVERS_TURN.prompt)
        .with_step(render_synthetic_agent_turns(&FIRST_TURN, 1))
        .with_step(pause(
            "Live GitHub card suggested above the input",
            "01-nudges.png",
        ));
    let builder = type_prompt(builder, ELICITATION_TURN.prompt)
        .with_step(render_synthetic_agent_turns(&FIRST_TWO_TURNS, 3))
        .with_step(pause(
            "MCP docs server and Skills suggested above the input",
            "01b-docs-nudge.png",
        ))
        .with_step(install_discovered_server(
            "modelcontextprotocol.io#io.modelcontextprotocol/docs-server",
        ))
        .with_step(
            assert_tool_in_next_agent_request("search_model_context_protocol")
                .set_timeout(Duration::from_secs(60)),
        )
        .with_step(pause("Live docs server is active", "02-installed.png"));
    type_prompt(builder, FIXTURE_TURN.prompt)
        .with_step(render_synthetic_agent_turns(&ALL_TURNS, 4))
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
