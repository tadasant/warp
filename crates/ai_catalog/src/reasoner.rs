//! Background "dual-track" reasoning: infers what the user is trying to do from the conversation
//! and which official domains would publish an AI Catalog for it, so those catalogs can be probed
//! before the agent ever touches the domain.

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use crate::domains::normalize_host;
use crate::resolver::ProbeRecord;

pub const API_KEY_ENV: &str = "AI_CATALOG_REASONER_API_KEY";
pub const MODEL_ENV: &str = "AI_CATALOG_REASONER_MODEL";
pub const BASE_URL_ENV: &str = "AI_CATALOG_REASONER_BASE_URL";

pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const SERVER_SIDE_FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(90);
/// Most domains the reasoner may ask to probe per run.
pub const MAX_CANDIDATE_DOMAINS: usize = 4;

pub const SYSTEM_PROMPT: &str = "\
You run in the background next to a terminal coding agent. You never talk to the user and never \
take actions. Your only job is to notice when an MCP server or Skills bundle published by an \
official vendor would help with what the user is trying to do.

Vendors publish these at https://<domain>/.well-known/ai-catalog.json on their official domain \
(for example a GitHub task points at github.com, a Linear task at linear.app, a Stripe task at \
stripe.com). Infer the user's intent from the conversation, possibly across several turns, then \
list the official domains whose catalog is worth probing. Only list domains you are confident \
are the vendor's real, official domain; never guess look-alike or third-party domains. If \
nothing external is involved, list none.

Reply with only a JSON object, no prose and no code fences:
{\"intent\": \"<one sentence>\", \"candidates\": [{\"domain\": \"<bare host>\", \"reason\": \
\"<why>\"}]}";

/// What the reasoner concluded for one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasonerVerdict {
    pub intent: String,
    pub candidates: Vec<CandidateDomain>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CandidateDomain {
    pub domain: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Deserialize)]
struct RawVerdict {
    #[serde(default)]
    intent: String,
    #[serde(default)]
    candidates: Vec<CandidateDomain>,
}

/// Builds the user turn: recent conversation plus the domains already probed this session.
pub fn build_user_prompt(transcript: &str, already_probed: &[String]) -> String {
    let probed = if already_probed.is_empty() {
        "(none)".to_string()
    } else {
        already_probed.join(", ")
    };
    format!(
        "Conversation so far (oldest first):\n{transcript}\n\nDomains already probed this \
         session: {probed}"
    )
}

/// Parses the model's reply, tolerating surrounding prose or code fences, and normalizes the
/// candidate domains (dropping private hosts and duplicates).
pub fn parse_verdict(raw: &str) -> Result<ReasonerVerdict, String> {
    let start = raw
        .find('{')
        .ok_or_else(|| "reply contained no JSON object".to_string())?;
    let end = raw
        .rfind('}')
        .filter(|end| *end > start)
        .ok_or_else(|| "reply contained no JSON object".to_string())?;
    let parsed: RawVerdict = serde_json::from_str(&raw[start..=end])
        .map_err(|err| format!("reply was not the expected JSON: {err}"))?;
    let mut candidates: Vec<CandidateDomain> = Vec::new();
    for candidate in parsed.candidates {
        let host = candidate
            .domain
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or_default()
            .to_string();
        let Some(domain) = normalize_host(&host) else {
            continue;
        };
        if candidates.iter().any(|existing| existing.domain == domain) {
            continue;
        }
        candidates.push(CandidateDomain {
            domain,
            reason: candidate.reason,
        });
        if candidates.len() == MAX_CANDIDATE_DOMAINS {
            break;
        }
    }
    Ok(ReasonerVerdict {
        intent: parsed.intent,
        candidates,
    })
}

/// A language model the reasoner can ask for a verdict.
#[async_trait]
pub trait ReasonerLlm: Send + Sync {
    /// Short label for the trace, e.g. the model id and endpoint.
    fn describe(&self) -> String;

    async fn complete(&self, system: &str, user: &str) -> Result<String, String>;
}

/// Claude via the Messages API. Configured from `AI_CATALOG_REASONER_*` env vars; the key is
/// always user-supplied.
pub struct AnthropicLlm {
    client: http_client::Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl AnthropicLlm {
    pub fn new(api_key: String, model: String, base_url: String) -> Self {
        Self {
            client: http_client::Client::new_external(),
            api_key,
            model,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// `None` when no API key is configured.
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var(API_KEY_ENV)
            .ok()
            .filter(|key| !key.trim().is_empty())?;
        let model = std::env::var(MODEL_ENV)
            .ok()
            .filter(|model| !model.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        let base_url = std::env::var(BASE_URL_ENV)
            .ok()
            .filter(|url| !url.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        Some(Self::new(api_key, model, base_url))
    }

    fn request_body(&self, system: &str, user: &str) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": 4096,
            "system": system,
            "messages": [{ "role": "user", "content": user }],
        });
        // Effort and server-side fallback aren't accepted by every model; send them only to the
        // models that take them.
        if self.model.starts_with("claude-opus-5")
            || self.model.starts_with("claude-sonnet-5")
            || self.model.starts_with("claude-fable-5")
        {
            body["output_config"] = serde_json::json!({ "effort": "low" });
            body["fallbacks"] = serde_json::json!("default");
        }
        body
    }
}

#[derive(Deserialize)]
struct MessagesResponse {
    #[serde(default)]
    content: Vec<ContentBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    text: Option<String>,
}

#[async_trait]
impl ReasonerLlm for AnthropicLlm {
    fn describe(&self) -> String {
        format!("{} via {}/v1/messages", self.model, self.base_url)
    }

    async fn complete(&self, system: &str, user: &str) -> Result<String, String> {
        let body = self.request_body(system, user);
        let mut request = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .timeout(REQUEST_TIMEOUT)
            .header("x-api-key", self.api_key.as_str())
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body);
        if body.get("fallbacks").is_some() {
            request = request.header("anthropic-beta", SERVER_SIDE_FALLBACK_BETA);
        }
        let response = request
            .send()
            .await
            .map_err(|err| format!("request failed: {err}"))?;
        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            return Err(format!(
                "HTTP {}: {}",
                status.as_u16(),
                detail.chars().take(300).collect::<String>()
            ));
        }
        let response: MessagesResponse = response
            .json()
            .await
            .map_err(|err| format!("unparseable response: {err}"))?;
        if response.stop_reason.as_deref() == Some("refusal") {
            return Err("the model declined this request".to_string());
        }
        let text: String = response
            .content
            .into_iter()
            .filter(|block| block.block_type == "text")
            .filter_map(|block| block.text)
            .collect();
        if text.trim().is_empty() {
            return Err("the model returned no text".to_string());
        }
        Ok(text)
    }
}

/// One entry in a background run's trace, in the order it happened.
#[derive(Debug, Clone, PartialEq)]
pub enum TraceStep {
    Trigger {
        user_query: String,
    },
    Prompt {
        llm: String,
        system: String,
        user: String,
    },
    LlmOutput {
        raw: String,
    },
    Intent {
        text: String,
    },
    Candidates {
        domains: Vec<CandidateDomain>,
    },
    Probe {
        domain: String,
        probes: Vec<ProbeRecord>,
    },
    Decision {
        text: String,
    },
    Error {
        text: String,
    },
}

/// Renders a trace as Markdown for the trace view.
pub fn trace_to_markdown(title: &str, steps: &[TraceStep]) -> String {
    let mut out = format!("# {title}\n\n");
    for step in steps {
        match step {
            TraceStep::Trigger { user_query } => {
                out.push_str(&format!("## Trigger\n\nUser turn:\n\n> {user_query}\n\n"));
            }
            TraceStep::Prompt { llm, system, user } => {
                out.push_str(&format!(
                    "## Reasoner prompt\n\nModel: `{llm}`\n\n### System\n\n```\n{system}\n```\n\n\
                     ### User\n\n```\n{user}\n```\n\n"
                ));
            }
            TraceStep::LlmOutput { raw } => {
                out.push_str(&format!("## Reasoner output\n\n```\n{raw}\n```\n\n"));
            }
            TraceStep::Intent { text } => {
                out.push_str(&format!("## Inferred intent\n\n{text}\n\n"));
            }
            TraceStep::Candidates { domains } => {
                out.push_str("## Candidate domains\n\n");
                if domains.is_empty() {
                    out.push_str("(none)\n");
                }
                for candidate in domains {
                    out.push_str(&format!("- `{}`: {}\n", candidate.domain, candidate.reason));
                }
                out.push('\n');
            }
            TraceStep::Probe { domain, probes } => {
                out.push_str(&format!("## Probe `{domain}`\n\n"));
                for probe in probes {
                    out.push_str(&format!("- `GET {}` → {}\n", probe.url, probe.outcome));
                }
                out.push('\n');
            }
            TraceStep::Decision { text } => {
                out.push_str(&format!("## Decision\n\n{text}\n\n"));
            }
            TraceStep::Error { text } => {
                out.push_str(&format!("## Error\n\n{text}\n\n"));
            }
        }
    }
    out
}
