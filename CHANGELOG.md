# Changelog

## v0.1.0 — 2026-06-14

Initial release: a Wyrtloom `LlmProvider` plugin for the Nous Portal inference API.

- Implements `wyrtloom_core::provider::LlmProvider` (`generate`, `models`) against the
  OpenAI-compatible Nous chat endpoint (`https://inference-api.nousresearch.com/v1`).
- `NousProvider::{new, hosted, from_env}` constructors; Bearer-token auth via `NOUS_API_KEY`.
- Security hardening mirrored from the in-tree Ollama provider: HTTPS-only base-URL allowlist
  (SSRF guard), 30s timeout, redirects disabled, opaque HTTP/transport error mapping, and
  ANSI/control-character stripping of model output.
- `max_tokens` clamped to the API's `1..=32000` window (never raises the caller's budget).
- Defensive token-usage parsing (the `usage` object is absent from the published OpenAPI spec)
  and per-call cost computed from a built-in Hermes-4 pricing table.
- `models()` surfaces the `GET /v1/models` catalog with per-token pricing; returns empty on failure.
- All wire encode/decode logic factored into pure helpers; the full test suite runs offline.
- `examples/smoke.rs` for manual live verification with a funded key.

### Not yet supported
- The x402 (Solana USDC) payment flow — `402` responses are surfaced as an error.
- Streaming responses (`stream` is always `false`).
