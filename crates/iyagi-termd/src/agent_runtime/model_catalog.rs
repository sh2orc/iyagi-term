//! Selectable model candidates for mission setup — Claude Code only.
//!
//! What this is NOT: capability evidence. Constants such as
//! [`super::capability_evidence::CODEX_MACOS_MODEL`] name the exact model one
//! recorded live session proved and are pinned next to that recording's
//! sha256 digest; "updating" them to a newer model would silently detach a
//! claim from the run that backs it. Nothing here ever reaches that registry:
//! these ids only fill a picker, and picking an unproven one still leaves the
//! start-gate to the evidence path.
//!
//! Claude only, because the other runtimes answer for themselves — the Codex
//! adapter asks app-server (`model/list`) and OpenCode asks its server, so a
//! static table beside a live listing could only be the stale one. The Claude
//! Code CLI has no listing API, which leaves the two sources that cannot go
//! stale:
//! * the `--model` aliases (`opus`, `sonnet`, `haiku`), which the CLI itself
//!   resolves to whatever the current model of that family is; and
//! * the model ids this machine's own transcripts show answering, which name
//!   a new model the first time it replies — no release-day edit needed.
//!
//! Both sources describe the CLI, not one connection through it, so
//! [`claude_models`] narrows them to the provider the binding will carry: the
//! same executable routes to non-Anthropic backends, and a transcript does not
//! record which one answered.
//!
//! Every lookup is best effort: a missing home, an unreadable directory or a
//! half-written transcript line yields fewer candidates, never an error.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;
use term_contracts::mission::rpc::ProbeModel;

use crate::agent_model;

/// Model-id aliases accepted by `claude --model`; the CLI resolves each to the
/// current model of that family, so the list never needs a release-day edit.
const CLAUDE_ALIASES: [&str; 3] = ["opus", "sonnet", "haiku"];

/// The provider id a Claude Code binding carries when it talks to Anthropic
/// itself — the same string
/// [`super::capability_evidence::evidence_provider_id`] suggests for this
/// runtime, and therefore the one detection's one-click setup saves.
const ANTHROPIC_PROVIDER_ID: &str = "anthropic";

/// Prefix on every model id Anthropic itself serves (`claude-opus-5`,
/// `claude-sonnet-5-20260514`, …).
const ANTHROPIC_ID_PREFIX: &str = "claude-";

/// Effort rungs recorded for Claude Code in the `crate::agent_model` header,
/// which is where this list comes from: a live measurement on 2.1.270
/// (launched with `--effort high`, switched with `/effort xhigh`) plus the
/// values `ModelObservation::effort` is documented to carry. The observed
/// value `auto` is left out: it asks the CLI to choose a rung rather than
/// being one.
///
/// Unverified per model, and knowingly so. The same header records that Claude
/// stores effort per model (`modelSettings.<model>.effortLevel`), so "every
/// model takes all five" is a guess this build has never measured — no CLI
/// output enumerates a model's rungs. It stays a picker hint like the ids
/// themselves: nothing gates on it, the daemon forwards the choice to
/// `--effort` without checking it (`super::claude` rejects only option-shaped
/// values), and the CLI is what judges a rung it does not take.
const CLAUDE_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Claude Code's placeholder in messages it synthesizes itself (interrupts,
/// injected notices). It appears in `message.model` exactly like a real id,
/// but no such model can be selected.
const SYNTHETIC_MODEL: &str = "<synthetic>";

/// Newest transcripts read per call. Each is read as a bounded tail
/// ([`agent_model::TAIL_READ_MAX`], 256 KiB), so one pass stays in the low
/// megabytes; two dozen sessions already cover every model a user has talked
/// to recently.
const MAX_TRANSCRIPTS: usize = 24;

/// Distinct observed ids handed to the picker. A dropdown longer than this is
/// unusable, and the tail of a recency-ranked list is what this machine
/// stopped talking to.
const MAX_OBSERVED: usize = 24;

/// Upper bound on a model id, matching the one `detection` puts on configured
/// ids. Real ids are a few dozen bytes; anything longer is a corrupt line.
const MAX_MODEL_ID_BYTES: usize = 128;

/// `projects/` holds one directory per workspace, so the walk needs its own
/// bound before it ever touches file metadata.
const MAX_PROJECT_DIRS: usize = 256;

/// Candidate transcripts collected before ranking by modification time. A home
/// with years of history has thousands; stopping here keeps the walk finite at
/// the cost of possibly missing a newer session in such a home — a picker hint
/// may be short, it may not be unbounded work.
const MAX_SCANNED_FILES: usize = 512;

/// The rungs offered for every Claude candidate, alias or observed id.
///
/// One helper because the picker must not grow an effort control for `opus`
/// and lose it for `claude-opus-5`: both ids reach the same `--effort` flag on
/// the same executable, and this process knows nothing per model that would
/// justify the difference (see [`CLAUDE_EFFORTS`] — the ladder is a guess for
/// either one). Consistency is the claim here, not accuracy.
fn claude_effort_ladder() -> Vec<String> {
    CLAUDE_EFFORTS.iter().map(|e| (*e).to_owned()).collect()
}

/// Model-id aliases the Claude Code CLI resolves to the current model.
/// These never go stale, which is why no version-pinned id is listed here:
/// a hardcoded `claude-opus-5`-style id is wrong the day the next model ships,
/// while `opus` keeps pointing at whatever is current. Version-pinned ids
/// reach the picker only by observation, from transcripts that actually used
/// them.
pub fn claude_aliases() -> Vec<ProbeModel> {
    CLAUDE_ALIASES
        .iter()
        .map(|id| ProbeModel {
            id: (*id).to_owned(),
            efforts: claude_effort_ladder(),
        })
        .collect()
}

/// Distinct model ids this machine's Claude Code transcripts show in use,
/// most recently seen first. Self-updating: a new model appears as soon as it
/// answers.
///
/// Recency, not frequency: the model this picker exists for is the one that
/// just shipped, and on the day it ships it has answered a handful of times
/// against months of the previous one — a use-ranked list buries it under
/// exactly the ids the user is trying to move off.
///
/// Reads the tail of the most recently written transcripts under
/// `<claude_dir>/projects/<slug>/<session>.jsonl`, where assistant entries
/// carry `{"type":"assistant","message":{"model":"…"}}`. An unreadable home,
/// a line that is not JSON and an entry without `message.model` are all
/// skipped in silence; nothing here panics or propagates an error.
pub fn observed_claude_models(claude_dir: &Path) -> Vec<ProbeModel> {
    // Insertion order is the answer: [`recent_transcripts`] hands over files
    // newest-first, and a transcript is append-only, so walking its lines
    // backwards reaches the newest entry first. First sighting of an id is
    // therefore its most recent one.
    let mut seen: Vec<String> = Vec::new();
    'walk: for path in recent_transcripts(claude_dir) {
        let Some(tail) = agent_model::read_tail(&path, agent_model::TAIL_READ_MAX) else {
            continue;
        };
        for line in tail.lines().rev() {
            // Cheap pre-filter: most transcript lines are user/tool entries.
            if !line.contains("\"model\"") {
                continue;
            }
            let Some(id) = transcript_model_id(line) else {
                continue;
            };
            if seen.iter().any(|known| *known == id) {
                continue;
            }
            seen.push(id);
            if seen.len() >= MAX_OBSERVED {
                break 'walk;
            }
        }
    }
    seen.into_iter()
        .map(|id| ProbeModel {
            id,
            // A transcript records which model answered, never which efforts
            // that model accepts, so this ladder is a guess here. It is the
            // same guess the aliases already carry, which is the whole reason
            // to repeat it: `opus` and `claude-opus-5` are two spellings for
            // one CLI flag, and offering the effort control for the first
            // while hiding it for the second makes the picker flicker without
            // knowing more about either model.
            efforts: claude_effort_ladder(),
        })
        .collect()
}

/// Aliases plus observed ids, de-duplicated, aliases first, narrowed to the
/// provider the binding will carry. The aliases lead because they are the
/// selection that stays correct across releases; an observed id that is itself
/// an alias is not repeated.
///
/// The narrowing is a heuristic on the id, and is one because a transcript
/// records which model answered and never which provider served it. It is
/// needed because Claude Code is not Anthropic-only: `crate::claude_provider`
/// routes the same CLI at other backends (`zai-coding-plan` sets
/// `ANTHROPIC_BASE_URL` and the `ANTHROPIC_DEFAULT_*_MODEL` variables), so one
/// home's transcripts name `claude-*` ids and `glm-*` ids side by side while a
/// binding names exactly one provider. Offering the other provider's ids is
/// offering a pick that fails at launch.
///
/// * `Some("anthropic")` — the aliases plus observed ids starting with
///   `claude-`.
/// * `Some(_)` — observed ids that are neither `claude-*` nor an alias. The
///   aliases are Anthropic-route only: another route does not accept `opus` as
///   an id, it overrides what `opus` resolves to through its own environment.
/// * `None` — everything observed, for a caller that does not yet know which
///   provider the binding will name.
pub fn claude_models(claude_dir: Option<&Path>, provider_id: Option<&str>) -> Vec<ProbeModel> {
    let anthropic_route = provider_id == Some(ANTHROPIC_PROVIDER_ID);
    let mut models = if anthropic_route || provider_id.is_none() {
        claude_aliases()
    } else {
        Vec::new()
    };
    for observed in claude_dir.map(observed_claude_models).unwrap_or_default() {
        // Named provider: keep the ids that read as its own — the Anthropic
        // route wants exactly what the other routes do not.
        let keep = provider_id.is_none() || anthropic_route == anthropic_model_id(&observed.id);
        if !keep || models.iter().any(|known| known.id == observed.id) {
            continue;
        }
        models.push(observed);
    }
    models
}

/// Whether an id reads as one Anthropic itself serves. Deliberately shaped by
/// what Anthropic's own ids look like rather than by a list of foreign
/// prefixes: a route this build has never heard of is then excluded from the
/// Anthropic picker by default instead of being recommended into a launch
/// failure.
fn anthropic_model_id(id: &str) -> bool {
    let base = without_context_suffix(id.trim());
    base.starts_with(ANTHROPIC_ID_PREFIX) || CLAUDE_ALIASES.contains(&base)
}

/// Claude Code's context-window suffix (`opus[1m]`, `claude-fable-5-1[1m]`):
/// the same model at a larger context, never a different one. It must come off
/// before the alias compare, which is exact — otherwise `opus[1m]` reads as
/// foreign, and the split that fills the two provider pickers sends it to the
/// Z.ai route while dropping it from the Anthropic one. A `claude-` id is
/// unaffected either way; the aliases are the ids this rescues.
fn without_context_suffix(id: &str) -> &str {
    match id.rfind('[') {
        Some(open) if id.ends_with(']') => &id[..open],
        _ => id,
    }
}

/// The newest [`MAX_TRANSCRIPTS`] `*.jsonl` files under `<claude_dir>/projects`,
/// most recently modified first. Empty when `projects/` cannot be read.
fn recent_transcripts(claude_dir: &Path) -> Vec<PathBuf> {
    let Ok(projects) = std::fs::read_dir(claude_dir.join("projects")) else {
        return Vec::new();
    };
    let mut files: Vec<(SystemTime, PathBuf)> = Vec::new();
    'walk: for project in projects.flatten().take(MAX_PROJECT_DIRS) {
        let dir = project.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(sessions) = std::fs::read_dir(&dir) else {
            continue;
        };
        for session in sessions.flatten() {
            if files.len() >= MAX_SCANNED_FILES {
                break 'walk;
            }
            let path = session.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            // Metadata first: a FIFO or device left in the directory must not
            // be opened for reading.
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            files.push((modified, path));
        }
    }
    // Newest first; the path breaks ties so one home always ranks the same way.
    files.sort_by(|(a_at, a_path), (b_at, b_path)| b_at.cmp(a_at).then_with(|| a_path.cmp(b_path)));
    files.truncate(MAX_TRANSCRIPTS);
    files.into_iter().map(|(_, path)| path).collect()
}

/// `message.model` of one transcript line, when it is a usable id.
fn transcript_model_id(line: &str) -> Option<String> {
    let entry: Value = serde_json::from_str(line).ok()?;
    clean_model_id(entry.get("message")?.get("model")?.as_str()?)
}

/// A model id fit for a picker: trimmed, non-empty, bounded, free of control
/// characters, and never the synthetic-message placeholder.
fn clean_model_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()
        && trimmed.len() <= MAX_MODEL_ID_BYTES
        && trimmed != SYNTHETIC_MODEL
        && !trimmed.chars().any(char::is_control))
    .then(|| trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript(claude_dir: &Path, project: &str, session: &str, lines: &[&str]) {
        let dir = claude_dir.join("projects").join(project);
        std::fs::create_dir_all(&dir).unwrap();
        let body = lines.join("\n");
        std::fs::write(dir.join(format!("{session}.jsonl")), format!("{body}\n")).unwrap();
    }

    fn assistant(model: &str) -> String {
        format!("{{\"type\":\"assistant\",\"message\":{{\"model\":\"{model}\",\"id\":\"m\"}}}}")
    }

    fn ids(models: &[ProbeModel]) -> Vec<&str> {
        models.iter().map(|model| model.id.as_str()).collect()
    }

    #[test]
    fn observed_models_rank_by_recency_not_by_use() {
        let dir = tempfile::tempdir().unwrap();
        transcript(
            dir.path(),
            "-Users-dev-alpha",
            "s1",
            &[
                &assistant("claude-sonnet-5"),
                &assistant("claude-sonnet-5"),
                &assistant("claude-opus-5"),
                &assistant("claude-sonnet-5"),
                &assistant("claude-opus-6"),
            ],
        );
        let observed = observed_claude_models(dir.path());
        assert_eq!(
            ids(&observed),
            ["claude-opus-6", "claude-sonnet-5", "claude-opus-5"],
            "a model that answered once today leads the one used all week"
        );
        assert!(
            observed.iter().all(|model| model.efforts == CLAUDE_EFFORTS),
            "an observed id carries the alias ladder: a transcript states no \
             efforts, and neither does the alias it sits next to"
        );
    }

    #[test]
    fn observations_span_every_project_directory() {
        let dir = tempfile::tempdir().unwrap();
        transcript(
            dir.path(),
            "-Users-dev-alpha",
            "s1",
            &[&assistant("claude-opus-5")],
        );
        transcript(
            dir.path(),
            "-Users-dev-beta",
            "s2",
            &[&assistant("claude-haiku-4")],
        );
        // Which project ranks first depends on filesystem timestamps, so only
        // the collection is asserted here; ordering has its own test.
        let models = observed_claude_models(dir.path());
        let mut observed = ids(&models);
        observed.sort_unstable();
        assert_eq!(observed, ["claude-haiku-4", "claude-opus-5"]);
    }

    #[test]
    fn synthetic_and_malformed_ids_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let oversize = "c".repeat(MAX_MODEL_ID_BYTES + 1);
        transcript(
            dir.path(),
            "-Users-dev-alpha",
            "s1",
            &[
                &assistant("<synthetic>"),
                &assistant(""),
                &assistant("claude-opus\\u0007-5"),
                &assistant(&oversize),
                &assistant("claude-opus-5"),
            ],
        );
        assert_eq!(ids(&observed_claude_models(dir.path())), ["claude-opus-5"]);
    }

    #[test]
    fn non_json_lines_and_entries_without_a_model_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        transcript(
            dir.path(),
            "-Users-dev-alpha",
            "s1",
            &[
                "not json at all \"model\"",
                "{\"type\":\"user\",\"message\":{\"role\":\"user\"}}",
                "{\"type\":\"assistant\",\"message\":{\"model\":7}}",
                "{\"model\":\"top-level-not-a-message\"}",
                "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-5\"}",
                &assistant("claude-opus-5"),
            ],
        );
        assert_eq!(ids(&observed_claude_models(dir.path())), ["claude-opus-5"]);
    }

    #[test]
    fn missing_projects_directory_yields_no_observations() {
        let dir = tempfile::tempdir().unwrap();
        assert!(observed_claude_models(dir.path()).is_empty());
        assert!(observed_claude_models(&dir.path().join("absent")).is_empty());
    }

    #[test]
    fn claude_models_lists_aliases_first_without_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        transcript(
            dir.path(),
            "-Users-dev-alpha",
            "s1",
            &[
                &assistant("opus"),
                &assistant("opus"),
                &assistant("claude-opus-5"),
            ],
        );
        let models = claude_models(Some(dir.path()), None);
        assert_eq!(ids(&models), ["opus", "sonnet", "haiku", "claude-opus-5"]);
        assert_eq!(models[0].efforts, CLAUDE_EFFORTS, "aliases keep the ladder");
        assert_eq!(
            models[3].efforts, models[0].efforts,
            "one ladder for the whole picker: `--effort` does not care which \
             spelling of the model sits in the field"
        );
        assert_eq!(ids(&claude_models(None, None)), ["opus", "sonnet", "haiku"]);
    }

    #[test]
    fn claude_models_are_narrowed_to_the_connection_provider() {
        let dir = tempfile::tempdir().unwrap();
        transcript(
            dir.path(),
            "-Users-dev-alpha",
            "s1",
            &[
                &assistant("claude-opus-5"),
                &assistant("glm-5.3"),
                &assistant("opus"),
            ],
        );
        assert_eq!(
            ids(&claude_models(Some(dir.path()), Some("anthropic"))),
            ["opus", "sonnet", "haiku", "claude-opus-5"],
            "an id another route answered would fail at launch on this one"
        );
        assert_eq!(
            ids(&claude_models(Some(dir.path()), Some("zai-coding-plan"))),
            ["glm-5.3"],
            "the aliases name Anthropic families, not this route's models"
        );
        assert_eq!(
            ids(&claude_models(Some(dir.path()), None)),
            ["opus", "sonnet", "haiku", "glm-5.3", "claude-opus-5"],
            "a caller without a provider still sees everything observed"
        );
        assert!(
            claude_models(None, Some("zai-coding-plan")).is_empty(),
            "without transcripts there is nothing this route could offer"
        );
    }

    #[test]
    fn a_context_suffixed_alias_stays_on_the_anthropic_route() {
        // `~/.claude/settings.json` stores the 1M-context variant as
        // `opus[1m]`, so it reaches the pickers as `configured_model_id` and
        // as a transcript id. Before the suffix was stripped it matched
        // neither `claude-` nor the exact alias list, so the shape split
        // offered it as a Z.ai Coding Plan model and hid it from Anthropic's.
        let dir = tempfile::tempdir().unwrap();
        transcript(
            dir.path(),
            "-Users-dev-alpha",
            "s1",
            &[
                &assistant("opus[1m]"),
                &assistant("claude-fable-5-1[1m]"),
                &assistant("glm-5.3"),
            ],
        );
        // Bound first: `ids` borrows the listing, which may not be a temporary.
        let listed = claude_models(Some(dir.path()), Some("anthropic"));
        let anthropic = ids(&listed);
        assert!(
            anthropic.contains(&"opus[1m]"),
            "a suffixed alias is the same model at a larger context: {anthropic:?}"
        );
        assert!(anthropic.contains(&"claude-fable-5-1[1m]"));
        assert_eq!(
            ids(&claude_models(Some(dir.path()), Some("zai-coding-plan"))),
            ["glm-5.3"],
            "only the genuinely foreign id belongs to the other route"
        );
    }

    #[test]
    fn context_suffix_comes_off_only_when_it_closes() {
        // A bare `[` is part of the id, not a suffix, and the GLM ids the Z.ai
        // route carries must not be rescued into the Anthropic picker.
        assert_eq!(without_context_suffix("opus[1m]"), "opus");
        assert_eq!(without_context_suffix("opus"), "opus");
        assert_eq!(without_context_suffix("opus[1m"), "opus[1m");
        assert!(!anthropic_model_id("glm-5.3[1m]"));
        assert!(!anthropic_model_id("glm-5.3"));
        assert!(anthropic_model_id(" sonnet[1m] "));
    }

    #[test]
    fn observed_models_stop_at_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let lines: Vec<String> = (0..MAX_OBSERVED + 8)
            .map(|n| assistant(&format!("claude-model-{n}")))
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        transcript(dir.path(), "-Users-dev-alpha", "s1", &refs);
        assert_eq!(observed_claude_models(dir.path()).len(), MAX_OBSERVED);
    }
}
