# wyrtloom-provider-nous

A [Wyrtloom](https://github.com/codepilots/wyrtloom) `LlmProvider` plugin for the
**Nous Portal** hosted inference API ([Nous Research](https://nousresearch.com), Hermes-4 models).

It implements the locked `wyrtloom_core::provider::LlmProvider` contract against Nous's
OpenAI-compatible chat endpoint, so any Wyrtloom installation can use Hermes-4 as a cloud
provider alongside the in-tree local Ollama provider. It lives in its own repository to
demonstrate Wyrtloom's out-of-tree plugin model: a plugin depends only on the stable core
trait surface, nothing else.

## API

- **Base URL:** `https://inference-api.nousresearch.com/v1` (the *inference* host)
- **Auth:** `Authorization: Bearer <NOUS_API_KEY>` — get a key at <https://portal.nousresearch.com>
- **Models:** `Hermes-4-405B`, `Hermes-4-70B` (the `GET /v1/models` catalog also exposes
  OpenRouter-backed models with their own per-token pricing)

## Usage

```rust
use wyrtloom_core::provider::{GenerationRequest, LlmProvider, Message};
use wyrtloom_provider_nous::NousProvider;

let provider = NousProvider::from_env()?;            // reads NOUS_API_KEY
// or: NousProvider::hosted("sk-...")?;               // explicit key
// or: NousProvider::new("sk-...", "https://...")?;   // explicit key + base URL

let resp = provider.generate(GenerationRequest {
    messages: vec![
        Message::system("Be terse."),
        Message::user("What is a mycelial network?"),
    ],
    max_output_tokens: 64,
    model: "Hermes-4-405B".into(),
})?;

println!("{}", resp.full_text());
println!("tokens in/out: {}/{}", resp.usage.input_tokens, resp.usage.output_tokens);
```

## Wiring `wyrtloom-core`

By default this crate uses a **path dependency** on a sibling checkout of the Wyrtloom monorepo:

```toml
wyrtloom-core = { path = "../wyrtloom/crates/core" }
```

For out-of-tree builds, swap it for the pinned git dependency (commented in `Cargo.toml`):

```toml
wyrtloom-core = { git = "https://github.com/codepilots/wyrtloom.git", rev = "<sha>" }
```

## Security

Mirrors the hardening of the in-tree `plugin-provider-ollama`:

- HTTPS-only base URL allowlist (SSRF guard); 30s timeout; redirects disabled.
- HTTP/transport errors mapped to opaque `ProviderError` categories — no response body is echoed.
- Model output is stripped of ANSI/control sequences before return (terminal-injection guard).

## Cost accounting

Token usage is parsed defensively (the Nous `usage` object is not in the published OpenAPI
spec — missing counts default to `0`). Per-call cost is computed from a built-in Hermes pricing
table: `Hermes-4-405B` $0.09 in / $0.37 out per 1M tokens; `Hermes-4-70B` $0.05 / $0.20. Unknown
models report `cost: None`. The `GET /v1/models` catalog's own per-token pricing is surfaced in
each `ModelDescriptor`.

## Build & test

```bash
cargo build
cargo test           # all tests run offline — no network or API key needed
cargo clippy --all-targets
```

Manual live check (needs a funded key):

```bash
NOUS_API_KEY=sk-... cargo run --example smoke
```

## License

Apache-2.0 (matching Wyrtloom).
