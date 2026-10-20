//! Opt-in AI Catalog / Server Card discovery (SEP-2127) for agent sessions.
//!
//! Watches the domains agent conversations touch, probes them in the background for
//! `/.well-known/ai-catalog.json`, nudges the user with a toast when a domain advertises an MCP
//! server or Skills bundle, and installs a chosen server into the running app session so the next
//! agent request can call its tools.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ai_catalog::{
    CatalogResolver, DefaultResolver, Discovery, DiscoveryKind, DiscoveryMode, DiscoveryState,
    DomainObservation, HttpResolver, InstallError, InstallPlan, domains_to_probe, install_plan_for,
};
use itertools::Itertools as _;
use uuid::Uuid;
use warpui::windowing::WindowManager;
use warpui::{Entity, ModelContext, SingletonEntity};

use crate::ai::agent::{
    AIAgentAction, AIAgentActionType, AIAgentExchange, AIAgentOutputMessageType, WebFetchStatus,
    WebSearchStatus,
};
use crate::ai::blocklist::{BlocklistAIHistoryEvent, BlocklistAIHistoryModel};
use crate::ai::mcp::templatable_manager::TemplatableMCPServerManagerEvent;
use crate::ai::mcp::{
    MCPServerState, ParsedTemplatableMCPServerResult, TemplatableMCPServerInstallation,
    TemplatableMCPServerManager,
};
use crate::settings::{AISettings, AiCatalogDiscoveryMode};
use crate::view_components::{DismissibleToast, ToastLink};
use crate::workspace::{ToastStack, WorkspaceAction};

/// Comma-separated `domain=origin` pairs that make the resolver probe a local origin (e.g. a
/// fixture server) in place of `https://<domain>`.
const ORIGIN_OVERRIDES_ENV: &str = "WARP_AI_CATALOG_ORIGIN_OVERRIDES";
/// When set, only the built-in demo fixtures are consulted and no live catalog is fetched.
const DISABLE_HTTP_ENV: &str = "WARP_AI_CATALOG_DISABLE_HTTP";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryInstallError {
    UnknownDiscovery(String),
    Plan(InstallError),
    InvalidServerConfig(String),
}

impl std::fmt::Display for DiscoveryInstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiscoveryInstallError::UnknownDiscovery(id) => write!(f, "unknown discovery: {id}"),
            DiscoveryInstallError::Plan(err) => write!(f, "{err}"),
            DiscoveryInstallError::InvalidServerConfig(err) => {
                write!(f, "invalid MCP server config: {err}")
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum AiCatalogDiscoveryEvent {
    Discovered(Discovery),
    InstallStarted {
        discovery_id: String,
        installation_uuid: Uuid,
    },
    Installed {
        discovery_id: String,
        installation_uuid: Uuid,
        tool_names: Vec<String>,
    },
    InstallFailed {
        discovery_id: String,
        message: String,
    },
}

struct InFlightInstall {
    discovery: Discovery,
    server_name: String,
}

pub struct AiCatalogDiscoveryModel {
    resolver: Arc<dyn CatalogResolver>,
    state: DiscoveryState,
    /// Keys of agent output messages already scanned for domains.
    observed_messages: HashSet<String>,
    installs: HashMap<Uuid, InFlightInstall>,
}

impl AiCatalogDiscoveryModel {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        Self::new_with_resolver(Arc::new(resolver_from_env()), ctx)
    }

    pub fn new_with_resolver(
        resolver: Arc<dyn CatalogResolver>,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        ctx.subscribe_to_model(
            &BlocklistAIHistoryModel::handle(ctx),
            |me, _, event, ctx| {
                me.handle_history_event(event, ctx);
            },
        );
        ctx.subscribe_to_model(
            &TemplatableMCPServerManager::handle(ctx),
            |me, _, event, ctx| {
                if let TemplatableMCPServerManagerEvent::StateChanged { uuid, state } = event {
                    me.handle_server_state_changed(*uuid, *state, ctx);
                }
            },
        );
        Self {
            resolver,
            state: DiscoveryState::default(),
            observed_messages: HashSet::new(),
            installs: HashMap::new(),
        }
    }

    pub fn pending(&self) -> &[Discovery] {
        self.state.pending()
    }

    pub fn discovery(&self, discovery_id: &str) -> Option<&Discovery> {
        self.state.get(discovery_id)
    }

    /// Probes the domains `observation` references, per the current discovery mode. Never blocks:
    /// each new domain is resolved on a background task.
    pub fn observe(&mut self, observation: &DomainObservation, ctx: &mut ModelContext<Self>) {
        let mode = current_mode(ctx);
        if mode == DiscoveryMode::Off {
            return;
        }
        for domain in domains_to_probe(mode, observation) {
            if !self.state.begin_probe(&domain) {
                continue;
            }
            let resolver = self.resolver.clone();
            let probed_domain = domain.clone();
            ctx.spawn(
                async move { resolver.resolve(&probed_domain).await },
                move |me, discoveries, ctx| me.handle_probe_result(&domain, discoveries, ctx),
            );
        }
    }

    fn handle_probe_result(
        &mut self,
        domain: &str,
        discoveries: Vec<Discovery>,
        ctx: &mut ModelContext<Self>,
    ) {
        for discovery in self.state.finish_probe(domain, discoveries) {
            log::info!(
                "AI catalog discovery on {domain}: {} (fixture: {})",
                discovery.id,
                discovery.fixture
            );
            show_toast(nudge_toast(&discovery), true, ctx);
            ctx.emit(AiCatalogDiscoveryEvent::Discovered(discovery));
        }
    }

    /// Installs a discovered MCP server into the running app session. `secret` is required when
    /// the card declares header auth; OAuth cards run the browser flow on connect instead.
    pub fn install(
        &mut self,
        discovery_id: &str,
        secret: Option<&str>,
        ctx: &mut ModelContext<Self>,
    ) -> Result<Uuid, DiscoveryInstallError> {
        let discovery = self
            .state
            .get(discovery_id)
            .cloned()
            .ok_or_else(|| DiscoveryInstallError::UnknownDiscovery(discovery_id.to_string()))?;
        let plan = install_plan_for(&discovery, secret).map_err(DiscoveryInstallError::Plan)?;
        let installation = installation_for_plan(&plan, discovery.description())?;
        let installation_uuid = installation.uuid();

        self.state.mark_installed(&discovery.id);
        self.installs.insert(
            installation_uuid,
            InFlightInstall {
                discovery: discovery.clone(),
                server_name: plan.server_name.clone(),
            },
        );
        TemplatableMCPServerManager::handle(ctx).update(ctx, |manager, ctx| {
            manager.spawn_discovered_server(installation, ctx);
        });
        let connecting_message = if plan.uses_oauth {
            format!(
                "Connecting to {}… complete sign-in in your browser.",
                discovery.title()
            )
        } else {
            format!("Connecting to {}…", discovery.title())
        };
        show_toast(DismissibleToast::default(connecting_message), false, ctx);
        ctx.emit(AiCatalogDiscoveryEvent::InstallStarted {
            discovery_id: discovery.id,
            installation_uuid,
        });
        Ok(installation_uuid)
    }

    pub fn dismiss(&mut self, discovery_id: &str) {
        self.state.mark_dismissed(discovery_id);
    }

    fn handle_server_state_changed(
        &mut self,
        installation_uuid: Uuid,
        server_state: MCPServerState,
        ctx: &mut ModelContext<Self>,
    ) {
        match server_state {
            MCPServerState::Running => {
                let Some(install) = self.installs.remove(&installation_uuid) else {
                    return;
                };
                let tool_names = TemplatableMCPServerManager::as_ref(ctx)
                    .tools_for_server(installation_uuid)
                    .into_iter()
                    .map(|tool| tool.name.to_string())
                    .collect_vec();
                let mut message = format!("{} is now active in this session", install.server_name);
                if !tool_names.is_empty() {
                    message.push_str(&format!(" · tools: {}", tool_names.join(", ")));
                }
                show_toast(DismissibleToast::success(message), false, ctx);
                ctx.emit(AiCatalogDiscoveryEvent::Installed {
                    discovery_id: install.discovery.id,
                    installation_uuid,
                    tool_names,
                });
            }
            MCPServerState::FailedToStart => {
                let Some(install) = self.installs.remove(&installation_uuid) else {
                    return;
                };
                let message = TemplatableMCPServerManager::as_ref(ctx)
                    .get_server_error_message(installation_uuid)
                    .unwrap_or("the server failed to start")
                    .to_string();
                show_toast(
                    DismissibleToast::error(format!(
                        "Couldn't install {}: {message}",
                        install.discovery.title()
                    )),
                    false,
                    ctx,
                );
                let discovery_id = install.discovery.id.clone();
                self.state.requeue(install.discovery);
                ctx.emit(AiCatalogDiscoveryEvent::InstallFailed {
                    discovery_id,
                    message,
                });
            }
            MCPServerState::NotRunning
            | MCPServerState::Starting
            | MCPServerState::Authenticating
            | MCPServerState::ShuttingDown => {}
        }
    }

    fn handle_history_event(
        &mut self,
        event: &BlocklistAIHistoryEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        let (exchange_id, conversation_id) = match event {
            BlocklistAIHistoryEvent::AppendedExchange {
                exchange_id,
                conversation_id,
                ..
            }
            | BlocklistAIHistoryEvent::UpdatedStreamingExchange {
                exchange_id,
                conversation_id,
                ..
            } => (*exchange_id, *conversation_id),
            _ => return,
        };
        if current_mode(ctx) == DiscoveryMode::Off {
            return;
        }
        let Some(exchange) = BlocklistAIHistoryModel::as_ref(ctx)
            .conversation(&conversation_id)
            .and_then(|conversation| conversation.exchange_with_id(exchange_id))
        else {
            return;
        };
        let observations = unobserved_observations(exchange, &mut self.observed_messages);
        for observation in &observations {
            self.observe(&observation.as_observation(), ctx);
        }
    }
}

impl Entity for AiCatalogDiscoveryModel {
    type Event = AiCatalogDiscoveryEvent;
}

impl SingletonEntity for AiCatalogDiscoveryModel {}

fn current_mode(ctx: &ModelContext<AiCatalogDiscoveryModel>) -> DiscoveryMode {
    let mode: AiCatalogDiscoveryMode = AISettings::as_ref(ctx).ai_catalog_discovery_mode;
    mode.as_discovery_mode()
}

fn resolver_from_env() -> DefaultResolver {
    if std::env::var_os(DISABLE_HTTP_ENV).is_some() {
        return DefaultResolver::new(None);
    }
    let mut http = HttpResolver::default();
    if let Ok(overrides) = std::env::var(ORIGIN_OVERRIDES_ENV) {
        http = http.with_origin_overrides_from_str(&overrides);
    }
    DefaultResolver::new(Some(http))
}

/// Owned form of [`DomainObservation`] so observations can outlive the history model borrow.
enum OwnedObservation {
    WebFetch(Vec<String>),
    WebSearch(Vec<String>),
    ShellCommand(String),
    McpToolCall {
        tool_name: String,
        arguments: String,
    },
    OtherToolCall(String),
}

impl OwnedObservation {
    fn as_observation(&self) -> DomainObservation<'_> {
        match self {
            OwnedObservation::WebFetch(urls) => DomainObservation::WebFetch { urls },
            OwnedObservation::WebSearch(result_urls) => {
                DomainObservation::WebSearch { result_urls }
            }
            OwnedObservation::ShellCommand(command) => DomainObservation::ShellCommand { command },
            OwnedObservation::McpToolCall {
                tool_name,
                arguments,
            } => DomainObservation::McpToolCall {
                tool_name,
                arguments,
            },
            OwnedObservation::OtherToolCall(arguments) => {
                DomainObservation::OtherToolCall { arguments }
            }
        }
    }
}

fn unobserved_observations(
    exchange: &AIAgentExchange,
    observed_messages: &mut HashSet<String>,
) -> Vec<OwnedObservation> {
    let Some(output) = exchange.output_status.output() else {
        return vec![];
    };
    let output = output.get();
    let mut observations = Vec::new();
    for message in &output.messages {
        let (stage, observation) = match &message.message {
            AIAgentOutputMessageType::WebFetch(WebFetchStatus::Fetching { urls }) => {
                ("fetching", OwnedObservation::WebFetch(urls.clone()))
            }
            AIAgentOutputMessageType::WebFetch(WebFetchStatus::Success { pages }) => (
                "fetched",
                OwnedObservation::WebFetch(pages.iter().map(|(url, ..)| url.clone()).collect()),
            ),
            AIAgentOutputMessageType::WebSearch(WebSearchStatus::Searching {
                query: Some(query),
            }) => (
                "searching",
                OwnedObservation::McpToolCall {
                    tool_name: "web_search".to_string(),
                    arguments: query.clone(),
                },
            ),
            AIAgentOutputMessageType::WebSearch(WebSearchStatus::Success { pages, .. }) => (
                "searched",
                OwnedObservation::WebSearch(pages.iter().map(|(url, _)| url.clone()).collect()),
            ),
            AIAgentOutputMessageType::Action(action) => ("action", observation_for_action(action)),
            _ => continue,
        };
        let message_id: &str = &message.id;
        if observed_messages.insert(format!("{message_id}:{stage}")) {
            observations.push(observation);
        }
    }
    observations
}

fn observation_for_action(action: &AIAgentAction) -> OwnedObservation {
    match &action.action {
        AIAgentActionType::RequestCommandOutput { command, .. } => {
            OwnedObservation::ShellCommand(command.clone())
        }
        AIAgentActionType::WriteToLongRunningShellCommand { input, .. } => {
            OwnedObservation::ShellCommand(String::from_utf8_lossy(input).into_owned())
        }
        AIAgentActionType::CallMCPTool { name, input, .. } => OwnedObservation::McpToolCall {
            tool_name: name.clone(),
            arguments: input.to_string(),
        },
        other => OwnedObservation::OtherToolCall(other.to_string()),
    }
}

/// Wraps a resolved install plan as an ephemeral installation. Header values are templatized by
/// the parser so secrets never pass through template rendering.
fn installation_for_plan(
    plan: &InstallPlan,
    description: &str,
) -> Result<TemplatableMCPServerInstallation, DiscoveryInstallError> {
    let mut config = serde_json::Map::new();
    config.insert(
        plan.server_name.clone(),
        serde_json::json!({
            "url": plan.url,
            "headers": plan.headers,
            "description": description,
        }),
    );
    let config = serde_json::Value::Object(config);
    ParsedTemplatableMCPServerResult::from_user_json(&config.to_string())
        .map_err(|err| DiscoveryInstallError::InvalidServerConfig(err.to_string()))?
        .into_iter()
        .next()
        .and_then(|parsed| parsed.templatable_mcp_server_installation)
        .ok_or_else(|| {
            DiscoveryInstallError::InvalidServerConfig(
                "server config is missing variable values".to_string(),
            )
        })
}

fn nudge_toast(discovery: &Discovery) -> DismissibleToast<WorkspaceAction> {
    let fixture_label = if discovery.fixture {
        " (demo fixture)"
    } else {
        ""
    };
    match &discovery.kind {
        DiscoveryKind::Server(_) => {
            let auth_label = match discovery.auth() {
                Some(auth) if auth.is_oauth() => " · OAuth",
                Some(_) => " · auth required",
                None => "",
            };
            let link_text = if discovery.requires_secret() {
                "Connect…"
            } else {
                "Install"
            };
            DismissibleToast::default(format!(
                "MCP server discovered on {}{fixture_label}: {}{auth_label}",
                discovery.source_domain,
                discovery.title()
            ))
            .with_object_id(discovery.id.clone())
            .with_link(ToastLink::new(link_text.to_string()).with_onclick_action(
                WorkspaceAction::InstallDiscoveredMcpServer {
                    discovery_id: discovery.id.clone(),
                },
            ))
        }
        DiscoveryKind::Skills(bundle) => DismissibleToast::default(format!(
            "Skills bundle discovered on {}{fixture_label}: {} ({})",
            discovery.source_domain,
            bundle.name,
            bundle.skills.iter().map(|skill| &skill.name).join(", ")
        ))
        .with_object_id(discovery.id.clone()),
    }
}

/// Shows `toast` in the active window. A no-op when no window or toast stack exists (e.g. headless).
fn show_toast(
    toast: DismissibleToast<WorkspaceAction>,
    persistent: bool,
    ctx: &mut ModelContext<AiCatalogDiscoveryModel>,
) {
    if !ctx.has_singleton_model::<ToastStack>() || !ctx.has_singleton_model::<WindowManager>() {
        return;
    }
    let Some(window_id) = WindowManager::as_ref(ctx).active_window() else {
        return;
    };
    ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
        if persistent {
            toast_stack.add_persistent_toast(toast, window_id, ctx);
        } else {
            toast_stack.add_ephemeral_toast(toast, window_id, ctx);
        }
    });
}

#[cfg(test)]
#[path = "ai_catalog_discovery_tests.rs"]
mod tests;
