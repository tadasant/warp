//! Install flow for MCP servers discovered from a domain's AI Catalog (SEP-2127).

use std::collections::HashMap;

use ai_catalog::Discovery;
use uuid::Uuid;
use warpui::ui_components::components::{Coords, UiComponentStyles};
use warpui::{SingletonEntity, ViewContext};

use super::Workspace;
use crate::ai::ai_catalog_discovery::AiCatalogDiscoveryModel;
use crate::ai::mcp::{JsonTemplate, TemplatableMCPServer, TemplateVariable, VariableValue};
use crate::modal::{Modal, ModalEvent, ModalViewState};
use crate::settings_view::mcp_servers::installation_modal::{
    InstallationModalBody, InstallationModalBodyEvent,
};
use crate::view_components::DismissibleToast;
use crate::workspace::ToastStack;

const DEFAULT_SECRET_VARIABLE: &str = "token";

/// The secret-entry modal plus the discovery it is collecting a secret for.
pub(super) struct AiCatalogInstallModal {
    modal: ModalViewState<Modal<InstallationModalBody>>,
    discovery_id: Option<String>,
}

impl AiCatalogInstallModal {
    pub(super) fn is_open(&self) -> bool {
        self.modal.is_open()
    }

    pub(super) fn render(&self) -> Box<dyn warpui::Element> {
        self.modal.render()
    }
}

impl Workspace {
    pub(super) fn build_ai_catalog_install_modal(
        ctx: &mut ViewContext<Self>,
    ) -> AiCatalogInstallModal {
        let body = ctx.add_typed_action_view(InstallationModalBody::new_for_secrets);
        ctx.subscribe_to_view(&body, |me, _, event, ctx| {
            me.handle_ai_catalog_install_modal_body_event(event, ctx);
        });
        let modal = ctx.add_typed_action_view(|ctx| {
            Modal::new(None, body, ctx).with_body_style(UiComponentStyles {
                padding: Some(Coords::uniform(0.)),
                ..Default::default()
            })
        });
        ctx.subscribe_to_view(&modal, |me, _, event, ctx| match event {
            ModalEvent::Close => me.close_ai_catalog_install_modal(ctx),
        });
        AiCatalogInstallModal {
            modal: ModalViewState::new(modal),
            discovery_id: None,
        }
    }

    pub(super) fn install_discovered_mcp_server(
        &mut self,
        discovery_id: &str,
        ctx: &mut ViewContext<Self>,
    ) {
        let model = AiCatalogDiscoveryModel::as_ref(ctx);
        let needs_secret = model.needs_secret(discovery_id);
        let Some(discovery) = model.discovery(discovery_id).cloned() else {
            self.show_ai_catalog_error_toast(
                "That discovered MCP server is no longer available.".to_string(),
                ctx,
            );
            return;
        };
        if needs_secret {
            self.open_ai_catalog_install_modal(&discovery, ctx);
        } else {
            self.install_discovery_with_secret(discovery_id, None, ctx);
        }
    }

    fn install_discovery_with_secret(
        &mut self,
        discovery_id: &str,
        secret: Option<&str>,
        ctx: &mut ViewContext<Self>,
    ) {
        let result = AiCatalogDiscoveryModel::handle(ctx)
            .update(ctx, |model, ctx| model.install(discovery_id, secret, ctx));
        if let Err(err) = result {
            self.show_ai_catalog_error_toast(format!("Couldn't install MCP server: {err}"), ctx);
        }
    }

    fn open_ai_catalog_install_modal(
        &mut self,
        discovery: &Discovery,
        ctx: &mut ViewContext<Self>,
    ) {
        let auth = discovery.auth();
        let variable = auth
            .and_then(|auth| auth.variable.clone())
            .unwrap_or_else(|| DEFAULT_SECRET_VARIABLE.to_string());
        // Only the template's name and variables are rendered; the install itself is driven by
        // the discovery's Server Card.
        let server = TemplatableMCPServer {
            uuid: Uuid::new_v4(),
            name: discovery.title().to_string(),
            description: Some(discovery.description().to_string()),
            template: JsonTemplate {
                json: String::new(),
                variables: vec![TemplateVariable {
                    key: variable,
                    allowed_values: None,
                }],
            },
            version: 0,
            gallery_data: None,
        };
        let header = match auth {
            Some(auth) => auth
                .header
                .as_deref()
                .unwrap_or("Authorization")
                .to_string(),
            None => "Authorization: Bearer".to_string(),
        };
        let mut instructions = format!(
            "{}\n\nThis server requires authentication. The value is sent as the `{header}` \
             header and kept in memory for this session only.",
            discovery.description()
        );
        if let Some(auth_instructions) = auth.and_then(|auth| auth.instructions.as_deref()) {
            instructions.push_str(&format!("\n\n{auth_instructions}"));
        }
        let fixture_label = if discovery.fixture {
            " (demo fixture)"
        } else {
            ""
        };
        let source_label = format!("Discovered on {}{fixture_label}", discovery.source_domain);

        self.ai_catalog_install_modal.discovery_id = Some(discovery.id.clone());
        self.ai_catalog_install_modal
            .modal
            .view
            .update(ctx, |modal, ctx| {
                modal.body().update(ctx, |body, ctx| {
                    body.set_templatable_mcp_server(Some(server), Some(instructions), ctx);
                    body.set_source_label(Some(source_label), ctx);
                });
            });
        self.ai_catalog_install_modal.modal.open();
        ctx.focus(&self.ai_catalog_install_modal.modal.view);
        ctx.notify();
    }

    fn handle_ai_catalog_install_modal_body_event(
        &mut self,
        event: &InstallationModalBodyEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            InstallationModalBodyEvent::Install(server, values) => {
                let discovery_id = self.ai_catalog_install_modal.discovery_id.clone();
                let secret = secret_from_values(server, values);
                self.close_ai_catalog_install_modal(ctx);
                if let Some(discovery_id) = discovery_id {
                    self.install_discovery_with_secret(&discovery_id, secret.as_deref(), ctx);
                }
            }
            InstallationModalBodyEvent::Cancel => self.close_ai_catalog_install_modal(ctx),
        }
    }

    fn close_ai_catalog_install_modal(&mut self, ctx: &mut ViewContext<Self>) {
        self.ai_catalog_install_modal.discovery_id = None;
        self.ai_catalog_install_modal.modal.close();
        self.ai_catalog_install_modal
            .modal
            .view
            .update(ctx, |modal, ctx| {
                modal.body().update(ctx, |body, ctx| {
                    body.set_templatable_mcp_server(None, None, ctx);
                    body.set_source_label(None, ctx);
                });
            });
        self.focus_active_tab(ctx);
        ctx.notify();
    }

    fn show_ai_catalog_error_toast(&self, message: String, ctx: &mut ViewContext<Self>) {
        let window_id = ctx.window_id();
        ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
            toast_stack.add_ephemeral_toast(DismissibleToast::error(message), window_id, ctx);
        });
    }
}

fn secret_from_values(
    server: &TemplatableMCPServer,
    values: &HashMap<String, VariableValue>,
) -> Option<String> {
    let variable = server.template.variables.first()?;
    values.get(&variable.key).map(|value| value.value.clone())
}
