//! Background "dual-track" AI Catalog discovery.
//!
//! Alongside the main agent, a reasoner reads the conversation whenever the user submits a prompt,
//! infers what they are trying to do, guesses which official vendor domains would publish an AI
//! Catalog for it, probes those catalogs, and suggests what it finds above the agent input. It
//! never blocks the agent and never installs anything on its own.

use std::collections::HashSet;
use std::sync::Arc;

use ai_catalog::reasoner::{
    AnthropicLlm, CandidateDomain, ReasonerLlm, SYSTEM_PROMPT, TraceStep, build_user_prompt,
    parse_verdict, trace_to_markdown,
};
use ai_catalog::trust::{NudgeOnlyPolicy, SuggestionOrigin, TrustPolicy};
use ai_catalog::{CatalogResolver, Discovery, TracedResolution};
use itertools::Itertools as _;
use warpui::{Entity, ModelContext, SingletonEntity};

use crate::ai::agent::{
    AIAgentExchange, AIAgentExchangeId, AIAgentInput, AIAgentOutputMessageType,
};
use crate::ai::ai_catalog_discovery::AiCatalogDiscoveryModel;
use crate::ai::blocklist::{BlocklistAIHistoryEvent, BlocklistAIHistoryModel};
use crate::settings::AISettings;

/// Most recent exchanges included in the reasoner's view of the conversation.
const TRANSCRIPT_EXCHANGES: usize = 6;
/// Per-message cap so one long agent reply can't crowd out the rest of the conversation.
const TRANSCRIPT_MESSAGE_CHARS: usize = 600;
/// Runs kept for the trace view.
const MAX_RUNS: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasonerRunStatus {
    Running,
    Finished,
    Failed,
}

#[derive(Debug, Clone)]
pub struct ReasonerRun {
    pub id: usize,
    pub status: ReasonerRunStatus,
    /// One-line outcome, e.g. "Suggested GitHub" or "Nothing to suggest".
    pub summary: Option<String>,
    pub steps: Vec<TraceStep>,
}

impl ReasonerRun {
    pub fn trace_markdown(&self) -> String {
        trace_to_markdown(
            &format!("Background AI Catalog discovery, run #{}", self.id),
            &self.steps,
        )
    }
}

#[derive(Debug, Clone)]
pub enum AiCatalogReasonerEvent {
    RunsChanged,
}

pub struct AiCatalogReasonerModel {
    llm: Option<Arc<dyn ReasonerLlm>>,
    resolver: Arc<dyn CatalogResolver>,
    trust_policy: Box<dyn TrustPolicy>,
    runs: Vec<ReasonerRun>,
    next_run_id: usize,
    /// Exchanges whose user prompt already triggered a run.
    triggered_exchanges: HashSet<AIAgentExchangeId>,
    probed_domains: Vec<String>,
    dismissed_run: Option<usize>,
}

impl AiCatalogReasonerModel {
    /// Uses `AI_CATALOG_REASONER_*` env vars for the model and the discovery model's resolver
    /// settings for probing.
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let llm = AnthropicLlm::from_env().map(|llm| Arc::new(llm) as Arc<dyn ReasonerLlm>);
        Self::new_with(
            llm,
            crate::ai::ai_catalog_discovery::default_resolver(),
            ctx,
        )
    }

    pub fn new_with(
        llm: Option<Arc<dyn ReasonerLlm>>,
        resolver: Arc<dyn CatalogResolver>,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        ctx.subscribe_to_model(
            &BlocklistAIHistoryModel::handle(ctx),
            |me, _, event, ctx| me.handle_history_event(event, ctx),
        );
        Self {
            llm,
            resolver,
            trust_policy: Box::new(NudgeOnlyPolicy),
            runs: Vec::new(),
            next_run_id: 1,
            triggered_exchanges: HashSet::new(),
            probed_domains: Vec::new(),
            dismissed_run: None,
        }
    }

    pub fn runs(&self) -> &[ReasonerRun] {
        &self.runs
    }

    pub fn run(&self, run_id: usize) -> Option<&ReasonerRun> {
        self.runs.iter().find(|run| run.id == run_id)
    }

    /// The run to show above the input: the newest one, unless the user dismissed it.
    pub fn visible_run(&self) -> Option<&ReasonerRun> {
        self.runs
            .last()
            .filter(|run| Some(run.id) != self.dismissed_run)
    }

    pub fn dismiss_run(&mut self, run_id: usize, ctx: &mut ModelContext<Self>) {
        self.dismissed_run = Some(run_id);
        ctx.emit(AiCatalogReasonerEvent::RunsChanged);
    }

    fn is_enabled(ctx: &ModelContext<Self>) -> bool {
        *AISettings::as_ref(ctx).ai_catalog_background_discovery_enabled
    }

    fn handle_history_event(
        &mut self,
        event: &BlocklistAIHistoryEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        let BlocklistAIHistoryEvent::AppendedExchange {
            exchange_id,
            conversation_id,
            is_hidden: false,
            ..
        } = event
        else {
            return;
        };
        if !Self::is_enabled(ctx) {
            return;
        }
        let Some(conversation) = BlocklistAIHistoryModel::as_ref(ctx).conversation(conversation_id)
        else {
            return;
        };
        let Some(exchange) = conversation.exchange_with_id(*exchange_id) else {
            return;
        };
        let Some(query) = user_query(exchange) else {
            return;
        };
        if !self.triggered_exchanges.insert(*exchange_id) {
            return;
        }
        let transcript = transcript_for(conversation.root_task_exchanges().collect_vec());
        self.start_run(query, transcript, ctx);
    }

    /// Starts a background run for the user's latest prompt. Returns the run id.
    pub fn start_run(
        &mut self,
        user_query: String,
        transcript: String,
        ctx: &mut ModelContext<Self>,
    ) -> usize {
        let run_id = self.next_run_id;
        self.next_run_id += 1;
        let mut run = ReasonerRun {
            id: run_id,
            status: ReasonerRunStatus::Running,
            summary: None,
            steps: vec![TraceStep::Trigger { user_query }],
        };

        let Some(llm) = self.llm.clone() else {
            run.status = ReasonerRunStatus::Failed;
            run.summary = Some(format!(
                "No reasoner configured; set {}",
                ai_catalog::reasoner::API_KEY_ENV
            ));
            run.steps.push(TraceStep::Error {
                text: format!(
                    "Background discovery needs a model. Set {} (and optionally {} / {}) and \
                     restart Warp.",
                    ai_catalog::reasoner::API_KEY_ENV,
                    ai_catalog::reasoner::MODEL_ENV,
                    ai_catalog::reasoner::BASE_URL_ENV
                ),
            });
            self.push_run(run, ctx);
            return run_id;
        };

        let user_prompt = build_user_prompt(&transcript, &self.probed_domains);
        run.steps.push(TraceStep::Prompt {
            llm: llm.describe(),
            system: SYSTEM_PROMPT.to_string(),
            user: user_prompt.clone(),
        });
        self.push_run(run, ctx);

        ctx.spawn(
            async move { llm.complete(SYSTEM_PROMPT, &user_prompt).await },
            move |me, reply, ctx| me.handle_reasoner_reply(run_id, reply, ctx),
        );
        run_id
    }

    fn push_run(&mut self, run: ReasonerRun, ctx: &mut ModelContext<Self>) {
        self.runs.push(run);
        if self.runs.len() > MAX_RUNS {
            self.runs.remove(0);
        }
        ctx.emit(AiCatalogReasonerEvent::RunsChanged);
    }

    fn update_run(
        &mut self,
        run_id: usize,
        ctx: &mut ModelContext<Self>,
        update: impl FnOnce(&mut ReasonerRun),
    ) {
        if let Some(run) = self.runs.iter_mut().find(|run| run.id == run_id) {
            update(run);
            ctx.emit(AiCatalogReasonerEvent::RunsChanged);
        }
    }

    fn handle_reasoner_reply(
        &mut self,
        run_id: usize,
        reply: Result<String, String>,
        ctx: &mut ModelContext<Self>,
    ) {
        let verdict = reply.and_then(|raw| {
            self.update_run(run_id, ctx, |run| {
                run.steps.push(TraceStep::LlmOutput { raw: raw.clone() })
            });
            parse_verdict(&raw)
        });
        let verdict = match verdict {
            Ok(verdict) => verdict,
            Err(error) => {
                self.update_run(run_id, ctx, |run| {
                    run.steps.push(TraceStep::Error {
                        text: error.clone(),
                    });
                    run.status = ReasonerRunStatus::Failed;
                    run.summary = Some(format!("Reasoner failed: {error}"));
                });
                return;
            }
        };

        let candidates = verdict.candidates.clone();
        self.update_run(run_id, ctx, |run| {
            run.steps.push(TraceStep::Intent {
                text: verdict.intent.clone(),
            });
            run.steps.push(TraceStep::Candidates {
                domains: candidates.clone(),
            });
        });
        if candidates.is_empty() {
            self.finish_run(
                run_id,
                "Nothing to suggest: no vendor catalog is relevant".to_string(),
                ctx,
            );
            return;
        }

        for candidate in &candidates {
            if !self.probed_domains.contains(&candidate.domain) {
                self.probed_domains.push(candidate.domain.clone());
            }
        }
        let resolver = self.resolver.clone();
        ctx.spawn(
            async move {
                let mut results = Vec::new();
                for candidate in candidates {
                    let resolution = resolver.resolve_traced(&candidate.domain).await;
                    results.push((candidate, resolution));
                }
                results
            },
            move |me, results, ctx| me.handle_probe_results(run_id, results, ctx),
        );
    }

    fn handle_probe_results(
        &mut self,
        run_id: usize,
        results: Vec<(CandidateDomain, TracedResolution)>,
        ctx: &mut ModelContext<Self>,
    ) {
        let mut suggested: Vec<Discovery> = Vec::new();
        for (candidate, resolution) in results {
            self.update_run(run_id, ctx, |run| {
                run.steps.push(TraceStep::Probe {
                    domain: candidate.domain.clone(),
                    probes: resolution.probes.clone(),
                })
            });
            for discovery in &resolution.discoveries {
                // The hook a trustManifest check would use to install without a click; the
                // default policy keeps every background suggestion a nudge.
                if self
                    .trust_policy
                    .may_auto_install(discovery, SuggestionOrigin::Background)
                {
                    log::info!("trust policy would allow auto-install of {}", discovery.id);
                }
            }
            let fresh = AiCatalogDiscoveryModel::handle(ctx).update(ctx, |model, ctx| {
                model.add_suggested_discoveries(
                    &candidate.domain,
                    resolution.discoveries,
                    format!("background: {}", candidate.reason),
                    ctx,
                )
            });
            suggested.extend(fresh);
        }
        let summary = if suggested.is_empty() {
            "Nothing to suggest: no new catalog entries on the candidate domains".to_string()
        } else {
            format!(
                "Suggested {} (nudge only; never auto-installed)",
                suggested.iter().map(Discovery::title).join(", ")
            )
        };
        self.finish_run(run_id, summary, ctx);
    }

    fn finish_run(&mut self, run_id: usize, summary: String, ctx: &mut ModelContext<Self>) {
        self.update_run(run_id, ctx, |run| {
            run.steps.push(TraceStep::Decision {
                text: summary.clone(),
            });
            run.status = ReasonerRunStatus::Finished;
            run.summary = Some(summary);
        });
    }
}

impl Entity for AiCatalogReasonerModel {
    type Event = AiCatalogReasonerEvent;
}

impl SingletonEntity for AiCatalogReasonerModel {}

fn user_query(exchange: &AIAgentExchange) -> Option<String> {
    exchange.input.iter().find_map(|input| match input {
        AIAgentInput::UserQuery { query, .. } if !query.trim().is_empty() => Some(query.clone()),
        _ => None,
    })
}

/// The last few exchanges as `User:` / `Agent:` lines, oldest first.
fn transcript_for(exchanges: Vec<&AIAgentExchange>) -> String {
    let start = exchanges.len().saturating_sub(TRANSCRIPT_EXCHANGES);
    let mut lines = Vec::new();
    for exchange in &exchanges[start..] {
        if let Some(query) = user_query(exchange) {
            lines.push(format!("User: {}", truncate(&query)));
        }
        if let Some(output) = exchange.output_status.output() {
            for message in &output.get().messages {
                if let AIAgentOutputMessageType::Text(_) = &message.message {
                    lines.push(format!("Agent: {}", truncate(&message.to_string())));
                }
            }
        }
    }
    lines.join("\n")
}

fn truncate(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= TRANSCRIPT_MESSAGE_CHARS {
        return text.to_string();
    }
    let truncated: String = text.chars().take(TRANSCRIPT_MESSAGE_CHARS).collect();
    format!("{truncated}…")
}

#[cfg(test)]
#[path = "ai_catalog_reasoner_tests.rs"]
mod tests;
