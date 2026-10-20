//! Background "dual-track" AI Catalog discovery: running indicator, suggestion above the input,
//! trace view and the settings switch.
//!
//! The reasoner talks to a local stub of the Messages API with a scripted verdict (no API key is
//! used), and the run is started directly rather than by an agent prompt, which needs a Warp
//! login. The probe of `https://github.com/.well-known/ai-catalog.json` and the GitHub Server
//! Card it references is live, so this needs network access. Run manually with a real display to
//! capture screenshots:
//!
//! ```sh
//! WARPUI_USE_REAL_DISPLAY_IN_INTEGRATION_TESTS=1 \
//!   cargo run -p integration --bin integration -- test_ai_catalog_background_discovery
//! ```

use std::time::Duration;

use ai_catalog::reasoner::{API_KEY_ENV, BASE_URL_ENV};
use ai_catalog::test_support::StubLlmServer;
use warp::integration_testing::ai_catalog::{
    open_latest_background_trace, set_background_discovery_only, set_inline_discovery_only,
    start_background_discovery, wait_for_background_discovery,
};
use warp::integration_testing::settings::{open_settings_page, type_settings_search};
use warp::integration_testing::terminal::wait_until_bootstrapped_single_pane_for_tab;
use warp::settings_view::SettingsSection;
use warpui_core::integration::TestStep;

use crate::Builder;

const GITHUB_VERDICT: &str = r#"{"intent": "Triage the user's open pull requests on GitHub.", "candidates": [{"domain": "github.com", "reason": "GitHub publishes the official GitHub MCP server at its own domain"}]}"#;

fn capture(name: &str, screenshot: &str) -> TestStep {
    TestStep::new(name)
        .set_timeout(Duration::from_secs(30))
        .set_post_step_pause(Duration::from_secs(2))
        .with_take_screenshot(screenshot)
}

pub fn test_ai_catalog_background_discovery() -> Builder {
    Builder::new()
        .with_setup(|utils| {
            // Slow enough that the running indicator is on screen for the first screenshot.
            let stub = StubLlmServer::start(0, GITHUB_VERDICT.to_string(), Duration::from_secs(8))
                .expect("stub reasoner should start");
            utils.set_env(
                API_KEY_ENV,
                Some("stub-key-not-a-real-credential".to_string()),
            );
            utils.set_env(BASE_URL_ENV, Some(stub.base_url().to_string()));
            // Serves for the lifetime of the test process.
            std::mem::forget(stub);
        })
        .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
        .with_step(set_background_discovery_only())
        .with_step(start_background_discovery(
            "go to my github and triage my open pull requests",
        ))
        .with_step(capture(
            "Background discovery running above the input",
            "07-background-running.png",
        ))
        .with_step(
            wait_for_background_discovery("github.com#io.github.github/github-mcp-server")
                .set_timeout(Duration::from_secs(60)),
        )
        .with_step(capture(
            "Live GitHub card suggested above the input",
            "08-background-suggestion.png",
        ))
        .with_step(open_latest_background_trace())
        .with_step(capture("Full background discovery trace", "09-trace.png"))
        .with_step(open_settings_page(SettingsSection::WarpAgent))
        .with_step(type_settings_search("Server Card"))
        .with_step(capture(
            "Inline and background switches",
            "10-settings-modes.png",
        ))
        .with_step(set_inline_discovery_only())
        .with_step(capture("Switched to inline only", "11-settings-inline.png"))
        .with_step(set_background_discovery_only())
        .with_step(capture(
            "Switched back to background only",
            "12-settings-background.png",
        ))
}
