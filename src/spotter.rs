//! The spotter — the independent permission rung (docs/permissions.md,
//! backlog/permissions/spotter.md).
//!
//! When the deterministic fast-path ([`crate::policy`]) can't `Clear` a tool call,
//! it `Gate`s — and the spotter looks at the residual before the human does. It is a
//! one-shot, **fresh-context** model call (Haiku) that reads the mission and the
//! proposed move and returns a graded [`Stance`]: `Clear` (auto-approve), `Watch`
//! (uncertain / one-way — a human beat), or `Gate` (out of scope or
//! unsanctioned-irreversible — stop and ask). Today `Watch` resolves to the human
//! gate alongside `Gate`; the watch-window that gives it its own latency is a later
//! unit.
//!
//! ## Independent, not a fork
//!
//! The independence *is* the product. The acting session can't check its own folly
//! because it shares its own trajectory; the spotter can, precisely because it does
//! not. So its request is **not** `project(log)` — it is built from scratch:
//!
//! ```text
//! [ persona (system, cached) | scope + watch-for (cached) | the move (fresh) ]
//! ```
//!
//! The persona lives in `assets/prompts/spotter.md` (`include_str!`d as the client's
//! byte-stable system prompt) and is sized to clear Haiku's cache floor on its own,
//! so even a minimal role caches. The per-role context (the config's path grants +
//! cwd, the scope prose, the spotter-only watch-for) rides one cached block;
//! only the move is fresh each call. The persona is universal, so when a fleet shares
//! a process the same cached segment serves every role (docs/permissions.md).
//!
//! ## Fail closed
//!
//! Any failure — transport error, no `spot` call — yields `Stance::Gate` with
//! [`Origin::Fallback`]. Uncertainty routes to the human; the spotter never
//! auto-clears on a failure (contrast the distiller, which fails *open* to keep-all
//! because dropping output loses data, whereas wrongly clearing a move does harm).

use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::{Client, Endpoint, Usage};
use crate::bash;
use crate::config::Config;
use crate::log::{Origin, Stance, ToolCall};
use crate::project::{ApiBlock, ApiMessage, CacheControl, Role as ApiRole};

/// The forced tool name.
const SPOT: &str = "spot";

/// The spotter persona — the universal half of the prompt (docs/permissions.md).
/// Edited as markdown; `include_str!`d as the spotter client's byte-stable system
/// prompt so it sits in the cached prefix. Sized to clear Haiku's cache floor alone.
const PERSONA: &str = include_str!("../assets/prompts/spotter.md");

/// The spotter's verdict on one move: the graded [`Stance`] plus a one-line `reason` addressed to
/// the acting session (what it reads to self-correct or petition). When a prefix-promotion probe
/// rode the call (the loop passed a candidate prefix to [`Spotter::assess`]), the **stance itself**
/// is the probe's answer: [`Stance::ClearAlways`] = clear the move *and* vote that prefix blanket-safe
/// (backlog/permissions/prefix-promotion.md). Folding the answer into the stance — not a separate
/// field — makes the contradictory combinations (`watch`/`gate` + "always safe") unrepresentable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read {
    pub stance: Stance,
    pub reason: String,
}

/// The outcome of one spotter assessment: the [`Read`], its [`Origin`] (model vs the
/// fail-closed fallback — the audit signal the stance alone erases), and the request
/// [`Usage`] if a round-trip happened (folded into the docs/substrate.md cache
/// telemetry, since it reads the shared spotter prefix). `None` usage for a
/// model-free spotter or a transport error.
pub struct Spotting {
    pub read: Read,
    pub origin: Origin,
    pub usage: Option<Usage>,
}

impl Spotting {
    /// The fail-closed verdict (docs/permissions.md): route to the human gate, never
    /// auto-clear. `usage` carried separately because a transport error has none.
    fn fail_closed(usage: Option<Usage>) -> Self {
        Spotting {
            read: Read {
                stance: Stance::Gate,
                reason: "spotter unavailable — defaulting to the human gate.".into(),
            },
            origin: Origin::Fallback,
            usage,
        }
    }
}

/// The permission-rung seam. Used generically by the loop (`<S: Spotter>`); not
/// object-safe and needn't be (one mounted rung per session).
#[allow(async_fn_in_trait)] // single-threaded loop; no Send bound needed, like Distiller.
pub trait Spotter {
    /// Assess one gated tool call against the config. The config supplies the mission
    /// (scope), the boundaries (path grants + cwd), and the spotter-only watch-for list.
    ///
    /// `probe` is an optional command prefix the prefix-promotion descent
    /// (backlog/permissions/prefix-promotion.md) wants judged *for promotion* alongside this move:
    /// when `Some`, the move prompt asks whether that prefix is safe for **every** argument, and a
    /// [`Stance::ClearAlways`] is the safe-vote. `None` on most calls (no probe rides along).
    async fn assess(&self, role: &Config, call: &ToolCall, probe: Option<&str>) -> Spotting;
}

/// The model spotter — the independent rung realized. Owns a dedicated [`Client`]
/// whose system prompt is the [`PERSONA`] and whose model is the spotter band
/// (Haiku), independent of the acting session's model.
pub struct ForkSpot {
    client: Client,
}

impl ForkSpot {
    /// Build the spotter over a fresh client pinned to the spotter `model` (Haiku)
    /// with the persona as its byte-stable system prompt. Shares the acting session's
    /// [`Endpoint`], so it authenticates and routes the same way (docs/transport.md).
    pub fn new(endpoint: Endpoint, model: impl Into<String>) -> Self {
        ForkSpot {
            client: Client::new(endpoint, model, PERSONA),
        }
    }
}

impl Spotter for ForkSpot {
    async fn assess(&self, role: &Config, call: &ToolCall, probe: Option<&str>) -> Spotting {
        // Cached block: the per-session-stable scope + watch-for. Fresh
        // block: the move (plus the optional promotion probe — it rides the fresh tail, so it never
        // perturbs the cached prefix, docs/substrate.md). The persona (system) + the cached block are
        // the prefix; only the move is rewritten each call (docs/permissions.md).
        let messages = vec![ApiMessage {
            role: ApiRole::User,
            content: vec![
                ApiBlock::Text {
                    text: render_context(role),
                    cache_control: Some(CacheControl::Ephemeral),
                },
                ApiBlock::Text {
                    text: render_move(call, probe),
                    cache_control: None,
                },
            ],
        }];

        // No-op sink: the spotter's tokens are internal and must never surface on
        // `GET /stream` (mirrors the fork peek).
        match self
            .client
            .complete(
                &messages,
                &[spot_tool()],
                crate::api::ToolChoice::force(SPOT),
                |_| {},
            )
            .await
        {
            Ok(completion) => {
                let usage = Some(completion.usage.clone());
                match parse_spot(&completion) {
                    Some(read) => Spotting {
                        read,
                        origin: Origin::Model,
                        usage,
                    },
                    None => Spotting::fail_closed(usage),
                }
            }
            Err(_) => Spotting::fail_closed(None),
        }
    }
}

/// A deterministic stub: returns a fixed verdict, ignoring the role and call. For
/// tests and as a model-free rung.
pub struct StaticSpotter {
    pub stance: Stance,
    pub reason: String,
}

impl Spotter for StaticSpotter {
    async fn assess(&self, _role: &Config, _call: &ToolCall, _probe: Option<&str>) -> Spotting {
        Spotting {
            read: Read {
                stance: self.stance,
                reason: self.reason.clone(),
            },
            origin: Origin::Fallback,
            usage: None,
        }
    }
}

/// Render the per-config context block: the mission (scope), the boundaries (cwd + the
/// config's path grants — so the spotter judges "outside scope" without the scope prose
/// having to restate them), and the spotter-only watch-for list. Stable
/// for the whole session, so it caches.
fn render_context(role: &Config) -> String {
    let mut s = String::from("Mission:\n");
    match role
        .scope
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        Some(scope) => s.push_str(scope),
        None => s.push_str("(no scope specified — judge by the reversibility default.)"),
    }

    s.push_str("\n\nScope for this session:\n");
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".into());
    s.push_str(&format!("- working directory: {cwd}\n"));
    if role.paths.is_empty() {
        s.push_str("- allowed paths: none configured\n");
    } else {
        s.push_str("- allowed paths (and everything beneath them):\n");
        for (path, perms) in &role.paths {
            let label = if perms.write {
                "read, write"
            } else if perms.read {
                "read only"
            } else {
                "no access"
            };
            s.push_str(&format!("  - {path} ({label})\n"));
        }
    }

    let watch: Vec<&str> = role
        .watch_for
        .iter()
        .map(|w| w.trim())
        .filter(|w| !w.is_empty())
        .collect();
    if !watch.is_empty() {
        s.push_str("\nThings to watch for on this mission:\n");
        for item in watch {
            s.push_str(&format!("- {item}\n"));
        }
    }
    s
}

/// Render the move the session is about to make. Bash shows its command verbatim; a
/// structured tool shows its name and input. The instruction to call `spot` rides
/// here (the fresh tail), not in the cached context. When `probe` is `Some`, a
/// promotion question for that prefix rides along too (backlog/permissions/prefix-promotion.md) —
/// the harness names the prefix; the model answers it via the `clear_always` stance.
fn render_move(call: &ToolCall, probe: Option<&str>) -> String {
    let body = if call.name == bash::NAME {
        match call.input.get("command").and_then(Value::as_str) {
            Some(cmd) => format!("A shell command:\n\n    {cmd}"),
            None => format!(
                "A `{}` call with no string command (malformed): {}",
                call.name, call.input
            ),
        }
    } else {
        let input =
            serde_json::to_string_pretty(&call.input).unwrap_or_else(|_| call.input.to_string());
        format!("A `{}` tool call with input:\n\n{input}", call.name)
    };
    let promotion = match probe {
        Some(prefix) => format!(
            "\n\nAlso being considered for promotion: the command prefix `{prefix}`. If — and only if \
             — you'd clear this move AND `{prefix}` is safe for *every* argument it could take under \
             this mission (not just this one), use the `clear_always` stance to vouch for the whole \
             prefix. If the prefix could take a dangerous argument, or you're unsure, use plain \
             `clear` (or `watch`/`gate`) — being asked again is cheap; a bad promotion is not."
        ),
        None => String::new(),
    };
    format!(
        "---\n\nThe session is about to make this move:\n\n{body}{promotion}\n\n\
         Call `spot` with your stance and a one-line reason."
    )
}

/// The forced `spot` tool: a required `stance` enum + one-line `reason`. The stance is 4-valued —
/// `clear_always` is the promotion safe-vote, meaningful only when a probe rode the call
/// (backlog/permissions/prefix-promotion.md); the enum is fixed (never varied per call) so the tool
/// schema stays byte-stable and the spotter's cache holds.
fn spot_tool() -> Value {
    json!({
        "name": SPOT,
        "description": "Record your read of the proposed move: a stance and a one-line reason.",
        "input_schema": {
            "type": "object",
            "properties": {
                "stance": {
                    "type": "string",
                    "enum": ["clear", "clear_always", "watch", "gate"],
                    "description": "clear = reversible and in-scope (or scope-sanctioned) — auto-approve; clear_always = clear AND (only when a prefix is offered for promotion) that prefix is safe for every argument under this mission, vouching to add it to the allowlist; watch = probably fine but uncertain, or a one-way move worth a human beat; gate = out of scope or unsanctioned-irreversible — stop and ask the human."
                },
                "reason": {
                    "type": "string",
                    "description": "One sentence to the session as a teammate: what crosses the line (and the in-scope alternative if you see one), or a brief note on a clean clear."
                }
            },
            "required": ["stance", "reason"]
        }
    })
}

/// The `spot` tool input as the model returns it. `stance` is read as a string and
/// mapped leniently so a capitalized or padded value still parses.
#[derive(Deserialize)]
struct SpotInput {
    stance: String,
    reason: String,
}

/// Pull the spotter verdict out of the completion. `None` when the turn didn't call
/// `spot` (or called something else) — the caller then fails closed.
fn parse_spot(completion: &crate::api::Completion) -> Option<Read> {
    let crate::log::Payload::Assistant { calls, .. } = &completion.assistant else {
        return None;
    };
    let call = calls.iter().find(|c| c.name == SPOT)?;
    let SpotInput { stance, reason } = SpotInput::deserialize(&call.input).ok()?;
    Some(Read {
        stance: parse_stance(&stance),
        reason,
    })
}

/// Map a stance string to a [`Stance`], leniently. An unrecognized value fails
/// closed to `Gate` rather than discarding the model's reason.
fn parse_stance(s: &str) -> Stance {
    match s.trim().to_ascii_lowercase().as_str() {
        "clear" => Stance::Clear,
        // Accept a couple of spellings for the promotion safe-vote — the model occasionally drops
        // the underscore or rewords it; all unambiguously mean "clear and vouch for the prefix".
        "clear_always" | "clear always" | "always" => Stance::ClearAlways,
        "watch" => Stance::Watch,
        _ => Stance::Gate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Completion, Usage};
    use crate::config::{Config, Perms};
    use crate::log::{CallId, Payload};

    fn spot_completion(name: &str, input: Value) -> Completion {
        Completion {
            assistant: Payload::Assistant {
                thinking: vec![],
                text: vec![],
                calls: vec![ToolCall {
                    id: CallId("s".into()),
                    name: name.into(),
                    input,
                }],
            },
            model: "test".into(),
            stop_reason: None,
            usage: Usage {
                input_tokens: 0,
                output_tokens: 0,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
        }
    }

    #[test]
    fn parse_stance_is_lenient_and_fails_closed() {
        assert_eq!(parse_stance("clear"), Stance::Clear);
        assert_eq!(parse_stance("  WATCH "), Stance::Watch);
        assert_eq!(parse_stance("Gate"), Stance::Gate);
        // The promotion safe-vote, in its accepted spellings.
        assert_eq!(parse_stance("clear_always"), Stance::ClearAlways);
        assert_eq!(parse_stance("Clear Always"), Stance::ClearAlways);
        assert_eq!(parse_stance("always"), Stance::ClearAlways);
        // Anything unrecognized routes to the human, not to auto-clear.
        assert_eq!(parse_stance("yolo"), Stance::Gate);
        assert_eq!(parse_stance(""), Stance::Gate);
    }

    #[test]
    fn parse_spot_reads_stance_and_reason() {
        let read = parse_spot(&spot_completion(
            SPOT,
            json!({ "stance": "clear", "reason": "in-scope build, reversible" }),
        ))
        .expect("a spot call parses");
        assert_eq!(read.stance, Stance::Clear);
        assert_eq!(read.reason, "in-scope build, reversible");
    }

    #[test]
    fn parse_spot_reads_the_promotion_safe_vote() {
        let read = parse_spot(&spot_completion(
            SPOT,
            json!({ "stance": "clear_always", "reason": "every cargo subcommand is reversible" }),
        ))
        .expect("a spot call parses");
        assert_eq!(read.stance, Stance::ClearAlways);
    }

    #[test]
    fn render_move_asks_the_promotion_question_only_when_a_prefix_is_probed() {
        let call = ToolCall {
            id: CallId("x".into()),
            name: bash::NAME.into(),
            input: json!({ "command": "git log --oneline" }),
        };
        // No probe → no promotion question.
        let plain = render_move(&call, None);
        assert!(!plain.contains("clear_always"), "{plain}");
        assert!(!plain.contains("promotion"), "{plain}");
        // Probe → the named prefix and the clear_always instruction ride the move.
        let probed = render_move(&call, Some("git log"));
        assert!(probed.contains("git log"), "{probed}");
        assert!(probed.contains("clear_always"), "{probed}");
    }

    #[test]
    fn parse_spot_is_none_without_a_spot_call() {
        // A turn that called something else yields no verdict → caller fails closed.
        assert!(parse_spot(&spot_completion("bash", json!({ "command": "ls" }))).is_none());
        // Missing required field → no verdict.
        assert!(parse_spot(&spot_completion(SPOT, json!({ "stance": "clear" }))).is_none());
    }

    #[test]
    fn render_move_shows_bash_command_and_structured_input() {
        let bash_call = ToolCall {
            id: CallId("x".into()),
            name: bash::NAME.into(),
            input: json!({ "command": "git push" }),
        };
        let rendered = render_move(&bash_call, None);
        assert!(rendered.contains("git push"), "{rendered}");
        assert!(rendered.contains("spot"), "instruction present: {rendered}");

        let write_call = ToolCall {
            id: CallId("y".into()),
            name: "write".into(),
            input: json!({ "path": "/etc/hosts" }),
        };
        let rendered = render_move(&write_call, None);
        assert!(rendered.contains("write"), "{rendered}");
        assert!(rendered.contains("/etc/hosts"), "{rendered}");
    }

    #[test]
    fn render_context_includes_scope_and_watch_for() {
        let role = Config {
            scope: Some("Fix the build.".into()),
            watch_for: vec!["Don't touch the deploy keys.".into()],
            paths: [
                (
                    ".".to_string(),
                    Perms {
                        read: true,
                        write: true,
                    },
                ),
                (
                    "/refs".to_string(),
                    Perms {
                        read: true,
                        write: false,
                    },
                ),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let ctx = render_context(&role);
        assert!(ctx.contains("Fix the build."), "{ctx}");
        assert!(ctx.contains(". (read, write)"), "{ctx}");
        assert!(ctx.contains("/refs (read only)"), "{ctx}");
        assert!(ctx.contains("Don't touch the deploy keys."), "{ctx}");
    }

    #[test]
    fn render_context_handles_empty_role() {
        // No scope, no paths, no watch-for: the spotter still gets a usable frame.
        let ctx = render_context(&Config::default());
        assert!(ctx.contains("no scope specified"), "{ctx}");
        assert!(ctx.contains("none configured"), "{ctx}");
        assert!(
            !ctx.contains("Things to watch for"),
            "no empty watch-for section: {ctx}"
        );
    }
}
