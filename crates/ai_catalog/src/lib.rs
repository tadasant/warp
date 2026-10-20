//! AI Catalog / Server Card auto-discovery (SEP-2127).
//!
//! Probes domains an agent session touches for `/.well-known/ai-catalog.json` and the Server
//! Cards / Skills bundles it references, and turns a discovered card into a remote MCP server
//! install plan. UI and scheduling live in the app; this crate holds no global state.

pub mod domains;
pub mod fixtures;
pub mod install;
pub mod reasoner;
pub mod resolver;
pub mod state;
#[cfg(any(test, feature = "test-util"))]
pub mod test_support;
pub mod trust;
pub mod types;

pub use domains::{DiscoveryMode, DomainObservation, domains_to_probe, extract_hosts};
pub use install::{InstallError, InstallPlan, RemoteTransportKind, install_plan_for};
pub use resolver::{
    CatalogResolver, DefaultResolver, FixtureResolver, HttpResolver, ProbeRecord, TracedResolution,
};
pub use state::DiscoveryState;
pub use types::{
    AiCatalog, AuthRequirement, Discovery, DiscoveryKind, RemoteTransport, ServerCard, SkillRef,
    SkillsBundle,
};

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
