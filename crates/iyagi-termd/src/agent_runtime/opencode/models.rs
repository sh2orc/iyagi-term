//! Local model listing for OpenCode bindings.
//!
//! `opencode models [provider]` prints one `provider/model` per line and
//! exits — a read-only local listing, so it runs on the same footing as the
//! version probe (explicit argv, null stdin, killed process group on timeout,
//! bounded output) instead of standing up a run-owned server. The adapter's
//! `/config/providers` route needs an authenticated loopback server that only
//! the mission path can build, which is far more than a picker hint is worth.
//!
//! This is a picker hint, never capability evidence: nothing here changes a
//! binding's grade or which roles it may take.

use std::time::Duration;

use term_contracts::mission::rpc::ProbeModel;

use crate::agent_runtime::installation;

/// Listing argv, verified against `opencode --help` (`opencode models
/// [provider]  list all available models`).
const MODELS_ARGV: [&str; 1] = ["models"];

/// The listing is one short line per model. A published catalogue is a few
/// hundred entries, so this admits a generous catalogue while still bounding
/// a CLI that decides to stream.
const OUTPUT_LIMIT: usize = 256 * 1024;

/// Bound: the listing is local and prints immediately. Kept at the version
/// probe's budget so a hung CLI cannot stall an RPC.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Untrusted CLI output caps (same footing as the Codex listing caps).
const MAX_MODELS: usize = 256;
const MAX_MODEL_ID_BYTES: usize = 128;

/// Models the local OpenCode CLI advertises, optionally narrowed to one
/// provider. Best effort: any failure (missing CLI, timeout, unparsable
/// output) yields an empty list rather than an error, because a missing
/// picker hint must never block detection or a probe.
pub fn list(program: &str, provider_id: Option<&str>) -> Vec<ProbeModel> {
    let Ok(text) = installation::capture_stdout(program, &MODELS_ARGV, OUTPUT_LIMIT, TIMEOUT)
    else {
        return Vec::new();
    };
    parse(&text, provider_id)
}

/// Parse `provider/model` lines. Pure so tests never spawn the CLI.
///
/// A binding stores the provider and the model separately, so the provider
/// prefix is stripped and only the model id is kept. Without a provider
/// filter every listed model is kept, first occurrence wins.
pub fn parse(text: &str, provider_id: Option<&str>) -> Vec<ProbeModel> {
    let mut models: Vec<ProbeModel> = Vec::new();
    for line in text.lines() {
        if models.len() >= MAX_MODELS {
            break;
        }
        let line = line.trim();
        // The listing is exactly `provider/model`; anything else (banner art,
        // a warning, a nested path) is not a model row.
        let Some((provider, model)) = line.split_once('/') else {
            continue;
        };
        if model.contains('/') || provider.is_empty() || model.is_empty() {
            continue;
        }
        if provider_id.is_some_and(|wanted| wanted != provider) {
            continue;
        }
        if model.len() > MAX_MODEL_ID_BYTES || model.chars().any(char::is_control) {
            continue;
        }
        if models.iter().any(|known| known.id == model) {
            continue;
        }
        // The listing carries no reasoning-effort information.
        models.push(ProbeModel {
            id: model.to_owned(),
            efforts: Vec::new(),
        });
    }
    models
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = "opencode/big-pickle\n\
                           openai/gpt-5.6-luna\n\
                           openai/gpt-6-astra\n\
                           zai-coding-plan/glm-5.3\n";

    fn ids(models: &[ProbeModel]) -> Vec<&str> {
        models.iter().map(|model| model.id.as_str()).collect()
    }

    #[test]
    fn listing_keeps_model_ids_in_order_without_the_provider_prefix() {
        assert_eq!(
            ids(&parse(LISTING, None)),
            ["big-pickle", "gpt-5.6-luna", "gpt-6-astra", "glm-5.3"]
        );
        assert!(parse(LISTING, None).iter().all(|m| m.efforts.is_empty()));
    }

    #[test]
    fn a_provider_filter_keeps_only_that_providers_models() {
        assert_eq!(
            ids(&parse(LISTING, Some("openai"))),
            ["gpt-5.6-luna", "gpt-6-astra"]
        );
        assert!(parse(LISTING, Some("anthropic")).is_empty());
    }

    #[test]
    fn non_model_rows_and_unusable_ids_are_dropped() {
        let long = format!("openai/{}", "m".repeat(MAX_MODEL_ID_BYTES + 1));
        let text = format!(
            "\u{2800}  banner art\n\
             openai\n\
             /leading\n\
             trailing/\n\
             a/b/c\n\
             openai/gpt\u{1b}[m-5\n\
             {long}\n\
             openai/gpt-6-astra\n\
             openai/gpt-6-astra\n"
        );
        assert_eq!(ids(&parse(&text, None)), ["gpt-6-astra"]);
    }

    #[test]
    fn the_model_cap_bounds_an_endless_listing() {
        let text = (0..MAX_MODELS + 50)
            .map(|index| format!("openai/model-{index}\n"))
            .collect::<String>();
        assert_eq!(parse(&text, None).len(), MAX_MODELS);
    }
}
