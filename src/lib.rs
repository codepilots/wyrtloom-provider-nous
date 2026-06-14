//! Wyrtloom `LlmProvider` plugin for the **Nous Portal** hosted inference API
//! (Nous Research — Hermes-4 models), an OpenAI-compatible chat endpoint at
//! `https://inference-api.nousresearch.com/v1`.
//!
//! This crate lives outside the Wyrtloom monorepo and depends only on the locked
//! `wyrtloom-core` trait surface — a demonstration of the out-of-tree plugin model.
//!
//! Security posture mirrors the in-tree `plugin-provider-ollama` reference:
//!   * HTTPS-only base URL allowlist (SSRF guard) — cf. Ollama finding 006
//!   * 30s request timeout, redirects disabled — finding 006
//!   * transport/HTTP errors mapped to opaque [`ProviderError`] categories — finding 021
//!   * model output stripped of ANSI/control sequences before return — finding 007
//!
//! All wire encoding/decoding lives in pure, side-effect-free helpers so the test
//! suite exercises the full contract with zero network access.

use std::time::Duration;

use wyrtloom_core::provider::{
    ContentBlock, GenerationRequest, GenerationResponse, LlmProvider, MessageRole, ModelDescriptor,
    ProviderError, Usage,
};
use wyrtloom_core::types::{ModelId, Money};

/// Default Nous Portal inference base URL (note: the *inference* host, not the portal).
pub const NOUS_BASE_URL: &str = "https://inference-api.nousresearch.com/v1";

/// Environment variable read by [`NousProvider::from_env`].
pub const API_KEY_ENV: &str = "NOUS_API_KEY";

/// Nous API hard limit on `max_tokens` (OpenAPI: 1–32000).
const MAX_TOKENS_CEILING: u32 = 32_000;

/// A Wyrtloom LLM provider backed by the Nous Portal inference API.
pub struct NousProvider {
    base_url: String,
    api_key: String,
    client: reqwest::blocking::Client,
}

impl NousProvider {
    /// Construct a provider for an explicit `api_key` and `base_url`.
    ///
    /// `base_url` must be `https://…` (or `http://localhost`/`http://127.0.0.1` for a
    /// local mock server in tests). The HTTP client is hardened: 30s timeout, no
    /// redirect following.
    pub fn new(api_key: impl Into<String>, base_url: impl Into<String>) -> Result<Self, String> {
        let base_url = base_url.into();
        validate_base_url(&base_url)?;
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err("Nous API key must not be empty".to_string());
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            // LLM APIs never redirect; following one would enable SSRF (cf. finding 006).
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;
        Ok(Self { base_url, api_key, client })
    }

    /// Construct against the hosted Nous Portal endpoint with an explicit key.
    pub fn hosted(api_key: impl Into<String>) -> Result<Self, String> {
        Self::new(api_key, NOUS_BASE_URL)
    }

    /// Construct from the `NOUS_API_KEY` environment variable, targeting the hosted endpoint.
    pub fn from_env() -> Result<Self, String> {
        let key = std::env::var(API_KEY_ENV)
            .map_err(|_| format!("environment variable {API_KEY_ENV} is not set"))?;
        Self::hosted(key)
    }
}

impl LlmProvider for NousProvider {
    fn generate(&self, req: GenerationRequest) -> Result<GenerationResponse, ProviderError> {
        // A zero output budget cannot be honoured (the API floor is 1 token), and
        // silently raising it would violate the contract's "must respect the budget".
        if req.max_output_tokens == 0 {
            return Err(ProviderError::BudgetExceeded);
        }
        let body = build_chat_body(&req);
        let url = format!("{}/chat/completions", self.base_url);

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            // Categorise transport failures without leaking host/internal detail (finding 021).
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::Transport("connection timed out".into())
                } else if e.is_connect() {
                    ProviderError::Transport("connection refused".into())
                } else {
                    ProviderError::Transport("network error".into())
                }
            })?;

        if let Some(err) = map_status(resp.status().as_u16()) {
            return Err(err);
        }

        let json: serde_json::Value = resp
            .json()
            .map_err(|_| ProviderError::Transport("response decode failed".into()))?;

        parse_chat_response(&json, &req.model)
    }

    fn models(&self) -> Vec<ModelDescriptor> {
        let url = format!("{}/models", self.base_url);
        let Ok(resp) = self.client.get(&url).bearer_auth(&self.api_key).send() else {
            return vec![];
        };
        if map_status(resp.status().as_u16()).is_some() {
            return vec![];
        }
        let Ok(json) = resp.json::<serde_json::Value>() else {
            return vec![];
        };
        parse_models(&json)
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (no I/O) — the entire wire contract, unit-testable offline.
// ---------------------------------------------------------------------------

/// Validate the base URL: HTTPS only, with localhost/127.0.0.1 permitted for test mock servers.
fn validate_base_url(url: &str) -> Result<(), String> {
    if url.starts_with("https://")
        || url.starts_with("http://localhost")
        || url.starts_with("http://127.0.0.1")
    {
        Ok(())
    } else {
        Err(format!(
            "base_url '{url}' is not permitted: must be https://, or http://localhost / http://127.0.0.1 for tests"
        ))
    }
}

/// Map an internal [`MessageRole`] to the Nous/OpenAI wire role string.
fn role_str(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
    }
}

/// Build the `/chat/completions` request body. `max_tokens` is clamped into the
/// API's accepted `1..=32000` window — this never *raises* the caller's output
/// budget, only caps it to what the API will accept.
fn build_chat_body(req: &GenerationRequest) -> serde_json::Value {
    let max_tokens = req.max_output_tokens.clamp(1, MAX_TOKENS_CEILING);
    let messages: Vec<serde_json::Value> = req
        .messages
        .iter()
        .map(|m| serde_json::json!({ "role": role_str(&m.role), "content": m.content }))
        .collect();
    serde_json::json!({
        "model": req.model,
        "messages": messages,
        "max_tokens": max_tokens,
        "stream": false,
    })
}

/// Map an HTTP status to a [`ProviderError`], or `None` for success (`2xx`).
/// Error messages are opaque — no response body is ever echoed (finding 021).
/// A `3xx` is an error here: redirects are deliberately disabled (SSRF guard), so
/// an unexpected redirect must surface rather than fall through to a decode failure.
fn map_status(status: u16) -> Option<ProviderError> {
    match status {
        200..=299 => None,
        // 401 invalid/blocked/out-of-funds key; 403 revoked/forbidden — both credential issues.
        401 | 403 => Some(ProviderError::Unauthorized),
        429 => Some(ProviderError::RateLimited),
        // Nous returns 402 for the (unsupported) x402 Solana payment flow when no key is sent.
        402 => Some(ProviderError::Provider(
            "payment required (x402 not supported)".into(),
        )),
        300..=399 => Some(ProviderError::Provider(format!(
            "unexpected redirect (HTTP {status})"
        ))),
        s => Some(ProviderError::Provider(format!("server returned HTTP {s}"))),
    }
}

/// Parse a chat-completion response into the core [`GenerationResponse`].
///
/// The Nous `usage` object is not part of the published OpenAPI spec, so it is
/// read defensively: missing token counts default to 0 and never error. Cost is
/// computed from the built-in pricing table when the model is recognised.
fn parse_chat_response(
    json: &serde_json::Value,
    model: &ModelId,
) -> Result<GenerationResponse, ProviderError> {
    let content = json
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .ok_or_else(|| ProviderError::Provider("malformed response from provider".into()))?;

    let usage_obj = json.get("usage");
    let input_tokens = usage_obj
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = usage_obj
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let cost = cost_for(input_tokens, output_tokens, model);
    let usage = Usage { input_tokens, output_tokens, cost };

    let clean = strip_control(content);
    Ok(GenerationResponse { content: vec![ContentBlock::Text(clean)], usage })
}

/// Built-in Hermes pricing in **USD per 1,000,000 tokens** `(input, output)`.
/// `None` for any model not in the table — cost is then reported as `None`.
///
/// Matched on the specific canonical substrings (which appear in both the native
/// names like `Hermes-4-405B` and the OpenRouter slugs like
/// `nousresearch/hermes-4-405b`), so an id that merely contains a stray `"70b"`
/// or `"405b"` token is not mis-priced, and an unknown future size falls through
/// to `None` (no cost) rather than a wrong cost.
fn price_per_million(model: &ModelId) -> Option<(f64, f64)> {
    let m = model.to_lowercase();
    if m.contains("hermes-4-405b") {
        Some((0.09, 0.37))
    } else if m.contains("hermes-4-70b") {
        Some((0.05, 0.20))
    } else {
        None
    }
}

/// Compute the [`Money`] cost of a call from token counts and the model's pricing.
fn cost_for(input_tokens: u64, output_tokens: u64, model: &ModelId) -> Option<Money> {
    let (in_per_m, out_per_m) = price_per_million(model)?;
    let dollars = (input_tokens as f64) * in_per_m / 1_000_000.0
        + (output_tokens as f64) * out_per_m / 1_000_000.0;
    Some(Money::usd(dollars))
}

/// Parse the `GET /v1/models` catalog (OpenRouter-shaped `data[]`) into
/// [`ModelDescriptor`]s, reading per-token `pricing.{prompt,completion}` (USD/token).
fn parse_models(json: &serde_json::Value) -> Vec<ModelDescriptor> {
    let Some(data) = json.get("data").and_then(|d| d.as_array()) else {
        return vec![];
    };
    data.iter()
        .filter_map(|m| {
            let id = m.get("id").and_then(|v| v.as_str())?.to_string();
            let pricing = m.get("pricing");
            let per_token = |field: &str| -> Option<Money> {
                let dollars = pricing.and_then(|p| p.get(field)).and_then(price_field_to_f64)?;
                let money = Money::usd(dollars);
                // Per-single-token prices are typically below one microdollar (Money's
                // granularity). Report None rather than a misleading $0.00 in that case;
                // the per-call cost in `generate()` is computed at per-million scale and
                // stays accurate.
                (money.amount_microdollars > 0).then_some(money)
            };
            let description = m
                .get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            Some(ModelDescriptor {
                id,
                description,
                cost_per_input_token: per_token("prompt"),
                cost_per_output_token: per_token("completion"),
            })
        })
        .collect()
}

/// OpenRouter pricing values arrive as strings (e.g. `"0.0000000900"`); accept
/// either a JSON string or number.
fn price_field_to_f64(v: &serde_json::Value) -> Option<f64> {
    if let Some(s) = v.as_str() {
        s.parse::<f64>().ok()
    } else {
        v.as_f64()
    }
}

/// Strip ANSI escape sequences and control characters (preserving `\n`, `\r`, `\t`)
/// from provider output, preventing terminal-injection from a compromised provider
/// (cf. Ollama finding 007). Vendored from the in-tree Ollama plugin; a candidate
/// to upstream into `wyrtloom-core` once a shared util module exists.
pub fn strip_control(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip until the terminating ASCII letter of the escape sequence.
            while let Some(&nc) = chars.peek() {
                chars.next();
                if nc.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if c.is_control() && c != '\n' && c != '\r' && c != '\t' {
            // Drop other control characters.
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use wyrtloom_core::provider::Message;

    fn req(model: &str, max: u32) -> GenerationRequest {
        GenerationRequest {
            messages: vec![Message::system("be terse"), Message::user("hello")],
            max_output_tokens: max,
            model: model.to_string(),
        }
    }

    // 1 — base URL validation (SSRF guard)
    #[test]
    fn https_base_url_is_accepted() {
        assert!(validate_base_url(NOUS_BASE_URL).is_ok());
        assert!(validate_base_url("https://example.com/v1").is_ok());
    }

    #[test]
    fn localhost_base_url_is_accepted_for_tests() {
        assert!(validate_base_url("http://localhost:8080/v1").is_ok());
        assert!(validate_base_url("http://127.0.0.1:8080/v1").is_ok());
    }

    #[test]
    fn plain_http_public_base_url_is_rejected() {
        assert!(validate_base_url("http://evil.example.com/v1").is_err());
        assert!(validate_base_url("ftp://example.com").is_err());
    }

    #[test]
    fn empty_api_key_is_rejected() {
        assert!(NousProvider::new("   ", NOUS_BASE_URL).is_err());
        assert!(NousProvider::new("sk-real", NOUS_BASE_URL).is_ok());
    }

    // 2 — from_env missing key
    #[test]
    fn from_env_errors_without_key() {
        // The test process does not set NOUS_API_KEY.
        std::env::remove_var(API_KEY_ENV);
        assert!(NousProvider::from_env().is_err());
    }

    // 3 — unreachable host yields a Transport error (no live server needed)
    #[test]
    fn generate_against_unreachable_host_is_transport_error() {
        let p = NousProvider::new("sk-test", "https://127.0.0.1:19999/v1").unwrap();
        let err = p.generate(req("Hermes-4-405B", 16)).unwrap_err();
        assert!(matches!(err, ProviderError::Transport(_)), "got {err:?}");
    }

    // 4 — status mapping + opacity
    #[test]
    fn status_mapping_is_correct() {
        assert!(map_status(200).is_none());
        assert!(map_status(204).is_none());
        assert!(matches!(map_status(401), Some(ProviderError::Unauthorized)));
        assert!(matches!(map_status(403), Some(ProviderError::Unauthorized)));
        assert!(matches!(map_status(429), Some(ProviderError::RateLimited)));
        assert!(matches!(map_status(402), Some(ProviderError::Provider(_))));
        // 3xx is an error (redirects disabled), not a silent success.
        assert!(matches!(map_status(302), Some(ProviderError::Provider(_))));
        match map_status(500) {
            Some(ProviderError::Provider(m)) => assert_eq!(m, "server returned HTTP 500"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn error_messages_do_not_leak_bodies() {
        // No mapped error should echo a response body — only status/category text.
        for code in [400u16, 403, 418, 500, 503] {
            if let Some(ProviderError::Provider(m)) = map_status(code) {
                assert!(m.starts_with("server returned HTTP") || m.contains("payment required"));
            }
        }
    }

    // 5 — request body construction
    #[test]
    fn build_body_maps_roles_and_disables_stream() {
        let body = build_chat_body(&req("Hermes-4-70B", 100));
        assert_eq!(body["model"], "Hermes-4-70B");
        assert_eq!(body["stream"], false);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "hello");
    }

    #[test]
    fn build_body_clamps_max_tokens_to_api_ceiling() {
        assert_eq!(
            build_chat_body(&req("Hermes-4-70B", 999_999))["max_tokens"],
            MAX_TOKENS_CEILING
        );
        assert_eq!(build_chat_body(&req("Hermes-4-70B", 256))["max_tokens"], 256);
        assert_eq!(build_chat_body(&req("Hermes-4-70B", 1))["max_tokens"], 1);
    }

    #[test]
    fn generate_rejects_zero_output_budget() {
        // A zero budget is rejected before any network call (uses BudgetExceeded).
        let p = NousProvider::new("sk-test", NOUS_BASE_URL).unwrap();
        let err = p.generate(req("Hermes-4-405B", 0)).unwrap_err();
        assert!(matches!(err, ProviderError::BudgetExceeded), "got {err:?}");
    }

    // 6 — happy-path response parsing with usage + cost
    #[test]
    fn parse_response_extracts_text_usage_and_cost() {
        let json = serde_json::json!({
            "choices": [ { "index": 0, "message": { "role": "assistant", "content": "hi there" }, "finish_reason": "stop" } ],
            "usage": { "prompt_tokens": 1_000_000, "completion_tokens": 0, "total_tokens": 1_000_000 }
        });
        let resp = parse_chat_response(&json, &"Hermes-4-405B".to_string()).unwrap();
        assert_eq!(resp.full_text(), "hi there");
        assert_eq!(resp.usage.input_tokens, 1_000_000);
        assert_eq!(resp.usage.output_tokens, 0);
        // 1M input tokens of 405B at $0.09/1M = $0.09 = 90_000 microdollars.
        assert_eq!(resp.usage.cost.unwrap().amount_microdollars, 90_000);
    }

    // 7 — usage absent degrades to zero/None
    #[test]
    fn parse_response_without_usage_defaults_to_zero() {
        let json = serde_json::json!({
            "choices": [ { "message": { "content": "ok" } } ]
        });
        let resp = parse_chat_response(&json, &"Hermes-4-405B".to_string()).unwrap();
        assert_eq!(resp.usage.input_tokens, 0);
        assert_eq!(resp.usage.output_tokens, 0);
        // Zero tokens still yields a $0.00 cost for a priced model.
        assert_eq!(resp.usage.cost.unwrap().amount_microdollars, 0);
    }

    #[test]
    fn parse_response_malformed_is_provider_error() {
        let json = serde_json::json!({ "choices": [] });
        assert!(matches!(
            parse_chat_response(&json, &"Hermes-4-70B".to_string()),
            Err(ProviderError::Provider(_))
        ));
    }

    // 8 — cost table
    #[test]
    fn cost_for_known_models_is_exact() {
        // 405B: 1M output tokens at $0.37/1M = 370_000 microdollars.
        assert_eq!(
            cost_for(0, 1_000_000, &"Hermes-4-405B".to_string()).unwrap().amount_microdollars,
            370_000
        );
        // 70B: 2M input at $0.05/1M = $0.10 = 100_000 microdollars.
        assert_eq!(
            cost_for(2_000_000, 0, &"hermes-4-70b".to_string()).unwrap().amount_microdollars,
            100_000
        );
    }

    #[test]
    fn cost_for_unknown_model_is_none() {
        assert!(cost_for(1000, 1000, &"gpt-4o".to_string()).is_none());
        assert!(cost_for(1000, 1000, &"moonshotai/kimi-k2".to_string()).is_none());
    }

    // 9 — output sanitisation
    #[test]
    fn strip_control_removes_ansi_keeps_whitespace() {
        assert_eq!(strip_control("\x1b[31mred\x1b[0m text"), "red text");
        assert_eq!(strip_control("line1\nline2\tcol\r\n"), "line1\nline2\tcol\r\n");
        assert_eq!(strip_control("bell\x07here"), "bellhere");
    }

    // 10 — models catalog parsing
    #[test]
    fn parse_models_reads_ids_and_pricing() {
        let json = serde_json::json!({
            "data": [
                { "id": "nousresearch/hermes-4-405b", "name": "Hermes 4 405B",
                  "pricing": { "prompt": "0.0000000900", "completion": "0.0000003700" } },
                { "id": "expensive/model", "name": "Pricey",
                  "pricing": { "prompt": "0.0000050000", "completion": "0.0000100000" } },
                { "id": "no-pricing-model" }
            ]
        });
        let models = parse_models(&json);
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].id, "nousresearch/hermes-4-405b");
        assert_eq!(models[0].description.as_deref(), Some("Hermes 4 405B"));
        // $0.00000009/token rounds below one microdollar → reported as None, not a fake $0.00.
        assert!(models[0].cost_per_input_token.is_none());
        // $0.000005/token = 5 microdollars — representable, so surfaced.
        assert_eq!(
            models[1].cost_per_input_token.as_ref().unwrap().amount_microdollars,
            5
        );
        assert_eq!(
            models[1].cost_per_output_token.as_ref().unwrap().amount_microdollars,
            10
        );
        assert!(models[2].cost_per_input_token.is_none());
    }
}
