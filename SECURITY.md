# Security model — `wyrtloom-provider-nous`

This crate is an out-of-tree [`LlmProvider`] plugin for the **Nous Portal**
hosted inference API (Nous Research, Hermes-4 models), an OpenAI-compatible chat
endpoint at `https://inference-api.nousresearch.com/v1`. It speaks HTTP(S) to a
remote endpoint and returns model-generated text into the Wyrtloom host process,
so its trust boundary sits between a *partially trusted network endpoint* and the
*host application's terminal/output path*.

All claims below are enforced in `src/lib.rs`; line references are to that file.

---

## Threat model & scope

**Assets**

- The host process's memory and availability.
- The host's terminal / downstream output sink (text printed or rendered).
- The Nous API key (a bearer credential).
- The host's network position (ability to reach internal services).

**Adversaries considered**

1. **A hostile, buggy, or compromised inference endpoint** — returns oversized,
   malformed, or malicious response bodies (including terminal-injection escape
   sequences).
2. **A network MITM** — tampers with responses or attempts to redirect requests.
3. **An attacker who can influence the configured `base_url`** — tries to coerce
   the client into making requests to internal/unintended hosts (SSRF).

**Out of scope**

- The confidentiality of the API key *at rest in the environment* — the key is
  read from process env / passed in by the operator; protecting the environment
  is the operator's responsibility (see Gotchas).
- The correctness or safety of the *content* of model output beyond control-
  sequence stripping (e.g. prompt-injection of downstream agents, harmful text).
- Auditing the `reqwest`/`rustls`/`url`/`serde_json` dependency tree itself.
- The Wyrtloom host's handling of the returned `GenerationResponse` after it
  leaves this crate.

---

## Security mechanisms

### SSRF defense — `validate_base_url` (lib.rs:181–208)

The base URL is **parsed with the `url` crate**, not matched with string
prefixes. The policy is:

```
(scheme == http  && host is EXACTLY "localhost" or "127.0.0.1")  OR  (scheme == https)
```

and **any URL carrying userinfo (`user:pass@`) is rejected outright**
(lib.rs:187–191) before the scheme/host check. Real parsing plus the exact-host
rule closes the classic bypasses that a naive `starts_with("http://localhost")`
would wave through:

- `http://localhost@evil.com` — `localhost` is *userinfo*, the real host is
  `evil.com`. Rejected by the userinfo check.
- `http://localhost.evil.com` — subdomain trick; host is not exactly `localhost`.
  Rejected by the exact-match `host == "localhost"`.
- `http://127.0.0.1.evil/` — suffix trick; host is not exactly `127.0.0.1`.
  Rejected likewise.

Any non-`http`/`https` scheme (e.g. `ftp://`) falls through to the `_ => false`
arm and is rejected. Validation runs in the constructor (`new`, lib.rs:55), so an
invalid/abusive URL fails before any client is built or request sent. Covered by
the `ssrf_bypass_strings_are_rejected` test (lib.rs:400–418).

### Response body cap — `read_bounded_body` (lib.rs:151–172)

The response body is bounded to **8 MiB** (`MAX_RESPONSE_BYTES`, lib.rs:38)
**before deserialization**, defending against a memory-DoS from a hostile or
MITM'd endpoint:

- If a `Content-Length` is declared above the cap, the request is rejected up
  front without reading the body (lib.rs:154–158).
- Crucially, the body is then read through a streaming `Read::take(limit)` where
  `limit = MAX_RESPONSE_BYTES + 1` (lib.rs:163–167). Because the bound is applied
  to the *stream*, it holds even for **chunked, length-less, or
  `Content-Length`-spoofing** responses — at most `MAX_RESPONSE_BYTES + 1` bytes
  are ever allocated before the call bails with `Transport("response too large")`
  (lib.rs:168–170).

This cap protects both `generate()` and `models()`, which share the helper.

### Output sanitization — `strip_control` (lib.rs:288, 362–366)

Model output is passed through `wyrtloom_core::util::strip_control` before being
returned (lib.rs:288). This strips ANSI CSI/OSC/DCS escape sequences and Unicode
bidi/format control characters (while preserving `\n`, `\r`, `\t`), defending the
host terminal against **terminal-injection / Trojan-Source** attacks from a
compromised provider. The function is *re-exported* from `wyrtloom-core` rather
than vendored, so every plugin shares the same audited implementation
(lib.rs:362–366). Behaviour is pinned by the `strip_control_*` test (lib.rs:572–576).

### Transport hardening (lib.rs:60–67, 99–107, 241–256)

- **30-second timeout** on the blocking client (lib.rs:61).
- **Redirects disabled** (`redirect::Policy::none()`, lib.rs:63) — an LLM API
  never legitimately redirects, and following one would be an SSRF vector.
- **`rustls-tls`**, not native-tls: `reqwest` is built with
  `default-features = false` and the `rustls-tls` feature (Cargo.toml:16), so the
  crate has no dependency on system OpenSSL / pkg-config.
- **Opaque error mapping**: transport failures are categorised into
  `Transport("connection timed out" | "connection refused" | "network error")`
  (lib.rs:99–107), and HTTP status errors into opaque categories — no response
  body or socket/host detail is ever echoed (lib.rs:238, 466–473).
- **`3xx` is an explicit error** (lib.rs:251–253): since redirects are disabled,
  an unexpected redirect surfaces as `Provider("unexpected redirect …")` rather
  than silently falling through to a decode failure.

### Secrets (lib.rs:29, 70–79, 95)

The API key is supplied via `from_env` (reading `NOUS_API_KEY`, lib.rs:29/75–79)
or an explicit constructor (`new`/`hosted`, lib.rs:53/70). It is sent only as an
`Authorization: Bearer` header (`.bearer_auth`, lib.rs:95/122) and is **never
logged** — error messages are opaque and never include the key. Empty/whitespace
keys are rejected at construction (lib.rs:57–59).

### Budget enforcement (lib.rs:86–88, 222–223)

- A **zero output budget is rejected before any network call** with
  `BudgetExceeded` (lib.rs:86–88) — the API floor is 1 token and silently raising
  the budget would violate the contract.
- `max_tokens` is **clamped to the API window** `1..=32000` (`MAX_TOKENS_CEILING`,
  lib.rs:32/223). The clamp only ever *lowers* the caller's budget to what the API
  accepts; it never raises it.

---

## Key decisions & rationale

- **Parse, don't prefix-match.** SSRF allowlists built on `starts_with` are a
  well-known footgun (userinfo/subdomain/suffix bypasses). Delegating authority
  parsing to the `url` crate and matching on the parsed `scheme`/`host` is the
  whole point of the guard.
- **Bound the stream, not the header.** A `Content-Length` check alone is
  defeated by chunked or lying responses, so the real defense is the
  `Read::take`-bounded read. The `Content-Length` short-circuit is only an early
  optimisation.
- **Fail closed on the unexpected.** `3xx` and unknown statuses become explicit
  `Provider` errors rather than being treated as success or decoded blindly.
- **Opaque errors.** Categorising transport/HTTP failures without echoing bodies
  or socket detail avoids leaking endpoint internals into host logs.
- **Defensive parsing of optional fields.** The Nous `usage` object is read with
  `unwrap_or(0)` defaults (lib.rs:275–283) so a missing/partial accounting object
  never errors a successful generation.
- **Shared sanitizer.** Re-exporting `strip_control` from core keeps the audited
  terminal-injection defense identical across all provider plugins.

---

## Gotchas / watch-outs

- **The `https://` branch allows ANY https host — by design.** The Nous API is a
  remote HTTPS endpoint, so the guard is **not** a strict single-host allowlist
  (lib.rs:195). If `base_url` is sourced from an attacker-controlled config value,
  an `https://` URL pointing at an **internal HTTPS service** (e.g. a cloud
  metadata endpoint over TLS, or an internal admin API) would pass validation and
  be reached. **The base URL must be operator-trusted** — treat it as a trusted
  configuration input, never as untrusted user data.

- **Token accounting is best-effort.** The Nous `usage` object is **not** in the
  published OpenAPI spec. It is parsed defensively and **defaults to 0** when
  absent or malformed (lib.rs:275–283). Cost is then derived from a **built-in
  pricing table** (`price_per_million`, lib.rs:300–309) keyed on canonical model
  substrings; an unrecognised model yields `None` cost (no wrong cost), so
  reported usage/cost should be treated as an estimate, not an authoritative bill.

- **The API key lives in process env / memory.** The key sits in the environment
  (`NOUS_API_KEY`) and in process memory for the lifetime of the provider.
  Protecting the environment — env var exposure, core dumps, process inspection,
  CI secret handling — is the operator's responsibility and outside this crate's
  control.

- **`models()` swallows errors by design.** The catalog call returns an empty
  `Vec` on any transport/decode/status failure (lib.rs:120–135) rather than
  surfacing an error. This is intentional (a missing catalog is non-fatal) but
  means catalog problems are silent.
