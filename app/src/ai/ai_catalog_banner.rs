//! Strip rendered just above the agent input that offers to install MCP servers discovered from a
//! domain's AI Catalog (SEP-2127).

use std::collections::HashMap;

use ai_catalog::{Discovery, DiscoveryKind};
use itertools::Itertools as _;
use warp_core::ui::theme::color::internal_colors;
use warpui::elements::{
    Container, CornerRadius, CrossAxisAlignment, Empty, Flex, Hoverable, MainAxisAlignment,
    MainAxisSize, MouseStateHandle, ParentElement, Radius, Shrinkable, Text,
};
use warpui::platform::Cursor;
use warpui::{AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext};

use crate::Appearance;
use crate::ai::ai_catalog_discovery::{
    AiCatalogDiscoveryEvent, AiCatalogDiscoveryModel, InstallStatusKind,
};
use crate::ai::ai_catalog_reasoner::{
    AiCatalogReasonerEvent, AiCatalogReasonerModel, ReasonerRunStatus,
};
use crate::workspace::WorkspaceAction;

/// Rows shown at once; older discoveries wait until newer ones are handled.
const MAX_VISIBLE_DISCOVERIES: usize = 3;

#[derive(Clone, Debug)]
pub enum AiCatalogBannerAction {
    Dismiss(String),
    DismissStatus,
    DismissRun(usize),
}

#[derive(Default)]
struct RowMouseStates {
    primary: MouseStateHandle,
    dismiss: MouseStateHandle,
}

pub struct AiCatalogBanner {
    row_mouse_states: HashMap<String, RowMouseStates>,
    status_dismiss_mouse_state: MouseStateHandle,
    trace_mouse_state: MouseStateHandle,
    run_dismiss_mouse_state: MouseStateHandle,
}

impl AiCatalogBanner {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        ctx.subscribe_to_model(
            &AiCatalogDiscoveryModel::handle(ctx),
            |me, _, event, ctx| match event {
                AiCatalogDiscoveryEvent::Changed => {
                    me.sync_mouse_states(ctx);
                    ctx.notify();
                }
            },
        );
        if ctx.has_singleton_model::<AiCatalogReasonerModel>() {
            ctx.subscribe_to_model(&AiCatalogReasonerModel::handle(ctx), |_, _, event, ctx| {
                match event {
                    AiCatalogReasonerEvent::RunsChanged => ctx.notify(),
                }
            });
        }
        let mut banner = Self {
            row_mouse_states: HashMap::new(),
            status_dismiss_mouse_state: MouseStateHandle::default(),
            trace_mouse_state: MouseStateHandle::default(),
            run_dismiss_mouse_state: MouseStateHandle::default(),
        };
        banner.sync_mouse_states(ctx);
        banner
    }

    fn sync_mouse_states(&mut self, ctx: &ViewContext<Self>) {
        let pending = AiCatalogDiscoveryModel::as_ref(ctx).pending();
        self.row_mouse_states
            .retain(|id, _| pending.iter().any(|discovery| &discovery.id == id));
        for discovery in pending {
            self.row_mouse_states
                .entry(discovery.id.clone())
                .or_default();
        }
    }

    fn render_row(
        &self,
        discovery: &Discovery,
        needs_secret: bool,
        suggestion_reason: Option<&str>,
        appearance: &Appearance,
    ) -> Option<Box<dyn Element>> {
        let mouse_states = self.row_mouse_states.get(&discovery.id)?;
        let theme = appearance.theme();
        let font_family = appearance.ui_font_family();
        let font_size = appearance.ui_font_size();
        let foreground = theme.foreground().into_solid();
        let muted = internal_colors::neutral_5(theme);
        let accent = theme.accent().into_solid();

        let (kind_label, detail) = match &discovery.kind {
            DiscoveryKind::Server(_) => ("MCP server", None),
            DiscoveryKind::Skills(bundle) => (
                "Skills",
                Some(bundle.skills.iter().map(|skill| &skill.name).join(", ")),
            ),
        };
        let mut qualifiers = vec![format!("on {}", discovery.source_domain)];
        if let Some(host) = discovery.remote_host() {
            qualifiers.push(format!("connects to {host}"));
        }
        if discovery.fixture {
            qualifiers.push("demo fixture".to_string());
        }
        if needs_secret {
            qualifiers.push("auth required".to_string());
        } else if discovery.auth().is_some_and(|auth| auth.is_oauth()) {
            qualifiers.push("OAuth".to_string());
        }
        if let Some(reason) = suggestion_reason {
            qualifiers.push(reason.to_string());
        }
        if let Some(detail) = detail {
            qualifiers.push(detail);
        }

        let label = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                Text::new_inline(format!("{kind_label} discovered: "), font_family, font_size)
                    .with_color(muted)
                    .finish(),
            )
            .with_child(
                Text::new_inline(discovery.title().to_string(), font_family, font_size)
                    .with_color(foreground)
                    .finish(),
            )
            .with_child(
                Shrinkable::new(
                    1.,
                    Text::new_inline(
                        format!("  ·  {}", qualifiers.join(" · ")),
                        font_family,
                        font_size - 1.,
                    )
                    .with_color(muted)
                    .finish(),
                )
                .finish(),
            )
            .finish();

        let mut actions = Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Center);
        if discovery.is_server() {
            let discovery_id = discovery.id.clone();
            let install_label = if needs_secret {
                "Connect…"
            } else {
                "Install"
            };
            actions.add_child(
                Container::new(link(
                    install_label,
                    accent,
                    font_family,
                    font_size,
                    mouse_states.primary.clone(),
                    move |ctx| {
                        ctx.dispatch_typed_action(WorkspaceAction::InstallDiscoveredMcpServer {
                            discovery_id: discovery_id.clone(),
                        })
                    },
                ))
                .with_margin_right(12.)
                .finish(),
            );
        }
        let discovery_id = discovery.id.clone();
        actions.add_child(link(
            "Dismiss",
            muted,
            font_family,
            font_size,
            mouse_states.dismiss.clone(),
            move |ctx| {
                ctx.dispatch_typed_action(AiCatalogBannerAction::Dismiss(discovery_id.clone()))
            },
        ));

        Some(
            Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(Shrinkable::new(1., label).finish())
                .with_child(actions.finish())
                .finish(),
        )
    }
}

impl AiCatalogBanner {
    fn render_status_row(
        &self,
        model: &AiCatalogDiscoveryModel,
        appearance: &Appearance,
    ) -> Option<Box<dyn Element>> {
        let status = model.install_status()?;
        let theme = appearance.theme();
        let font_family = appearance.ui_font_family();
        let font_size = appearance.ui_font_size();
        let color = match status.kind {
            InstallStatusKind::InProgress => internal_colors::neutral_5(theme),
            InstallStatusKind::Succeeded => theme.ansi_fg_green(),
            InstallStatusKind::Failed => theme.ansi_fg_red(),
        };
        let mut row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                Shrinkable::new(
                    1.,
                    Text::new(status.message.clone(), font_family, font_size)
                        .soft_wrap(true)
                        .with_color(color)
                        .finish(),
                )
                .finish(),
            );
        // Always dismissable: an abandoned OAuth flow can leave an install in progress forever.
        row.add_child(link(
            "Dismiss",
            internal_colors::neutral_5(theme),
            font_family,
            font_size,
            self.status_dismiss_mouse_state.clone(),
            |ctx| ctx.dispatch_typed_action(AiCatalogBannerAction::DismissStatus),
        ));
        Some(row.finish())
    }
}

impl AiCatalogBanner {
    /// The background reasoner's latest run: a running indicator, or its outcome, with a link to
    /// the full trace.
    fn render_reasoner_row(
        &self,
        app: &AppContext,
        appearance: &Appearance,
    ) -> Option<Box<dyn Element>> {
        if !app.has_singleton_model::<AiCatalogReasonerModel>() {
            return None;
        }
        let run = AiCatalogReasonerModel::as_ref(app).visible_run()?;
        let theme = appearance.theme();
        let font_family = appearance.ui_font_family();
        let font_size = appearance.ui_font_size();
        let muted = internal_colors::neutral_5(theme);
        let (text, color) = match run.status {
            ReasonerRunStatus::Running => (
                "◌ Background discovery is reasoning about your request…".to_string(),
                theme.accent().into_solid(),
            ),
            ReasonerRunStatus::Finished | ReasonerRunStatus::Failed => (
                format!(
                    "Background discovery: {}",
                    run.summary.as_deref().unwrap_or("done")
                ),
                muted,
            ),
        };
        let run_id = run.id;
        let mut actions = Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Center);
        actions.add_child(
            Container::new(link(
                "View trace",
                theme.accent().into_solid(),
                font_family,
                font_size,
                self.trace_mouse_state.clone(),
                move |ctx| {
                    ctx.dispatch_typed_action(WorkspaceAction::OpenAiCatalogDiscoveryTrace {
                        run_id,
                    })
                },
            ))
            .with_margin_right(12.)
            .finish(),
        );
        if run.status != ReasonerRunStatus::Running {
            actions.add_child(link(
                "Dismiss",
                muted,
                font_family,
                font_size,
                self.run_dismiss_mouse_state.clone(),
                move |ctx| ctx.dispatch_typed_action(AiCatalogBannerAction::DismissRun(run_id)),
            ));
        }
        Some(
            Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(
                    Shrinkable::new(
                        1.,
                        Text::new(text, font_family, font_size)
                            .soft_wrap(true)
                            .with_color(color)
                            .finish(),
                    )
                    .finish(),
                )
                .with_child(actions.finish())
                .finish(),
        )
    }
}

fn link(
    text: &'static str,
    color: pathfinder_color::ColorU,
    font_family: warpui::fonts::FamilyId,
    font_size: f32,
    mouse_state: MouseStateHandle,
    on_click: impl Fn(&mut warpui::EventContext) + 'static,
) -> Box<dyn Element> {
    Hoverable::new(mouse_state, move |_| {
        Text::new_inline(text.to_string(), font_family, font_size)
            .with_color(color)
            .finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| on_click(ctx))
    .finish()
}

impl Entity for AiCatalogBanner {
    type Event = ();
}

impl View for AiCatalogBanner {
    fn ui_name() -> &'static str {
        "AiCatalogBanner"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let model = AiCatalogDiscoveryModel::as_ref(app);
        let appearance = Appearance::as_ref(app);
        let mut rows = self
            .render_reasoner_row(app, appearance)
            .into_iter()
            .chain(self.render_status_row(model, appearance))
            .collect_vec();
        rows.extend(
            model
                .pending()
                .iter()
                .rev()
                .take(MAX_VISIBLE_DISCOVERIES)
                .filter_map(|discovery| {
                    self.render_row(
                        discovery,
                        model.needs_secret(&discovery.id),
                        model.suggestion_reason(&discovery.id),
                        appearance,
                    )
                }),
        );
        if rows.is_empty() {
            return Empty::new().finish();
        }

        let theme = appearance.theme();
        Container::new(
            Flex::column()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_spacing(6.)
                .with_children(rows)
                .finish(),
        )
        .with_background(internal_colors::fg_overlay_1(theme))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
        .with_horizontal_padding(12.)
        .with_vertical_padding(8.)
        .with_margin_bottom(6.)
        .finish()
    }
}

impl TypedActionView for AiCatalogBanner {
    type Action = AiCatalogBannerAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            AiCatalogBannerAction::Dismiss(discovery_id) => {
                AiCatalogDiscoveryModel::handle(ctx).update(ctx, |model, ctx| {
                    model.dismiss(discovery_id, ctx);
                });
            }
            AiCatalogBannerAction::DismissRun(run_id) => {
                AiCatalogReasonerModel::handle(ctx).update(ctx, |model, ctx| {
                    model.dismiss_run(*run_id, ctx);
                });
            }
            AiCatalogBannerAction::DismissStatus => {
                AiCatalogDiscoveryModel::handle(ctx).update(ctx, |model, ctx| {
                    model.clear_install_status(ctx);
                });
            }
        }
    }
}
