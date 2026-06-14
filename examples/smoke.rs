//! Manual end-to-end smoke test against the live Nous Portal API.
//!
//! Requires a funded API key:
//!     NOUS_API_KEY=sk-... cargo run --example smoke
//!
//! Optionally override the model:  NOUS_MODEL=Hermes-4-70B cargo run --example smoke
//!
//! Prints the model's reply, the reported token usage, and the computed cost.
//! This is also the way to confirm the live `usage` field names (which are absent
//! from the published OpenAPI spec) match the OpenAI-standard assumption.

use wyrtloom_core::provider::{GenerationRequest, LlmProvider, Message};
use wyrtloom_provider_nous::NousProvider;

fn main() {
    let provider = match NousProvider::from_env() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("setup error: {e}");
            eprintln!("set NOUS_API_KEY to a funded Nous Portal key and retry.");
            std::process::exit(1);
        }
    };

    let model = std::env::var("NOUS_MODEL").unwrap_or_else(|_| "Hermes-4-405B".to_string());
    println!("model: {model}");
    println!("catalog entries: {}", provider.models().len());

    let req = GenerationRequest {
        messages: vec![
            Message::system("You are terse. Answer in one short sentence."),
            Message::user("In one sentence, what is a mycelial network?"),
        ],
        max_output_tokens: 64,
        model,
    };

    match provider.generate(req) {
        Ok(resp) => {
            println!("\n--- reply ---\n{}", resp.full_text());
            println!(
                "\n--- usage ---\ninput_tokens={} output_tokens={}",
                resp.usage.input_tokens, resp.usage.output_tokens
            );
            match resp.usage.cost {
                Some(m) => println!("cost=${:.6} ({} microdollars)", m.as_dollars(), m.amount_microdollars),
                None => println!("cost=unknown (model not in pricing table)"),
            }
        }
        Err(e) => {
            eprintln!("generate failed: {e}");
            std::process::exit(1);
        }
    }
}
