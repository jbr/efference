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
//! auto-clears on a failure. The distiller fails the same direction with a different
//! artifact: a failed gate withholds the output as a hard error on the wire
//! (docs/redaction-fork.md), never silently keeps or silently clears.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::{Client, Endpoint, RequestUsage};
use crate::bash;
use crate::config::Config;
use crate::log::{Origin, Stance, ToolCall};
use crate::project::{ApiBlock, ApiMessage, CacheControl, Role as ApiRole};
use crate::tools::ToolBrief;
use mcplease::types::Tool as ToolDefinition;

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
/// (docs/permissions.md). Folding the answer into the stance — not a separate
/// field — makes the contradictory combinations (`watch`/`gate` + "always safe") unrepresentable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read {
    pub stance: Stance,
    pub reason: String,
}

/// The outcome of one spotter assessment: the [`Read`], its [`Origin`] (model vs the
/// fail-closed fallback — the audit signal the stance alone erases), and the request
/// [`RequestUsage`] if a round-trip happened (persisted origin-tagged and folded into
/// the docs/log-and-projection.md cache telemetry, since it reads the shared spotter
/// prefix). `None` usage for a model-free spotter or a transport error.
pub struct Spotting {
    pub read: Read,
    pub origin: Origin,
    pub usage: Option<RequestUsage>,
}

impl Spotting {
    /// The fail-closed verdict (docs/permissions.md): route to the human gate, never
    /// auto-clear. `usage` carried separately because a transport error has none.
    fn fail_closed(usage: Option<RequestUsage>) -> Self {
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

/// The permission-rung interface. Used generically by the loop (`<S: Spotter>`); not
/// object-safe and needn't be (one mounted rung per session).
#[allow(async_fn_in_trait)] // single-threaded loop; no Send bound needed, like Distiller.
pub trait Spotter {
    /// Assess one gated tool call against the config. The config supplies the mission
    /// (scope), the boundaries (path grants + cwd), and the spotter-only watch-for list.
    ///
    /// `brief` is what the mounted tool table knows about a structured call's tool
    /// — its description, its input schema, and the server's own behavior hints
    /// (docs/permissions.md). `None` for bash (judged as command text) and when no
    /// toolbox is at hand (the `efference spotter` dev command).
    ///
    /// `probe` is an optional command prefix the prefix-promotion descent
    /// (docs/permissions.md) wants judged *for promotion* alongside this move:
    /// when `Some`, the move prompt asks whether that prefix is safe for **every** argument, and a
    /// [`Stance::ClearAlways`] is the safe-vote. `None` on most calls (no probe rides along).
    async fn assess(
        &self,
        role: &Config,
        call: &ToolCall,
        brief: Option<&ToolBrief<'_>>,
        probe: Option<&str>,
    ) -> Spotting;
}

/// The model spotter — the independent rung realized. Owns a dedicated [`Client`]
/// whose system prompt is the [`PERSONA`] and whose model is the spotter band
/// (Haiku), independent of the acting session's model.
pub struct ForkSpot {
    client: Client,
    /// The session scratch dir (docs/tools.md), when one exists. Rendered into the
    /// scope block as the `$TMPDIR` alias: a bash move writes `$TMPDIR/…` (the
    /// shell expands it in-child, so the spotter sees the *variable*), and without
    /// the alias the granted literal path and the variable never connect.
    scratch: Option<std::path::PathBuf>,
}

impl ForkSpot {
    /// Build the spotter over a fresh client pinned to the spotter `model` (Haiku)
    /// with the persona as its byte-stable system prompt. Shares the acting session's
    /// [`Endpoint`], so it authenticates and routes the same way (docs/transport.md).
    pub fn new(endpoint: Endpoint, model: impl Into<String>) -> Self {
        ForkSpot {
            client: Client::new(endpoint, model, PERSONA),
            scratch: None,
        }
    }

    /// Name the session scratch dir so the scope block can render the `$TMPDIR`
    /// alias (docs/tools.md).
    pub fn with_scratch(mut self, scratch: Option<std::path::PathBuf>) -> Self {
        self.scratch = scratch;
        self
    }
}

impl Spotter for ForkSpot {
    async fn assess(
        &self,
        role: &Config,
        call: &ToolCall,
        brief: Option<&ToolBrief<'_>>,
        probe: Option<&str>,
    ) -> Spotting {
        // Cached block: the per-session-stable scope + watch-for. Fresh
        // block: the move (plus the optional promotion probe — it rides the fresh tail, so it never
        // perturbs the cached prefix, docs/log-and-projection.md). The persona (system) + the cached block are
        // the prefix; only the move is rewritten each call (docs/permissions.md).
        let messages = vec![ApiMessage {
            role: ApiRole::User,
            content: vec![
                ApiBlock::Text {
                    text: render_context(role, self.scratch.as_deref()),
                    cache_control: Some(CacheControl::Ephemeral),
                },
                ApiBlock::Text {
                    text: render_move(call, brief, probe),
                    cache_control: None,
                },
            ],
        }];

        // No-op sink: the spotter's tokens are internal and must never surface on        // the control stream (mirrors the fork peek).
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
                let usage = Some(completion.request_usage());
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
    async fn assess(
        &self,
        _role: &Config,
        _call: &ToolCall,
        _brief: Option<&ToolBrief<'_>>,
        _probe: Option<&str>,
    ) -> Spotting {
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
/// having to restate them), the `$TMPDIR` scratch alias when a scratch dir exists
/// (docs/tools.md), and the spotter-only watch-for list. Stable
/// for the whole session, so it caches.
fn render_context(role: &Config, scratch: Option<&std::path::Path>) -> String {
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
        // A more specific grant overrides a broader one, so a denied subtree carves an
        // exclusion out of an allowed parent (docs/permissions.md). Surfacing every grant —
        // including the denies — is what lets the spotter judge a *bash* move against a
        // carve-out the deterministic fast-path never sees (e.g. a write to the session's own
        // log, or a poke at its own control socket, both under a denied `.efference/`).
        s.push_str("- path grants (a more specific grant overrides a broader one):\n");
        for (path, perms) in &role.paths {
            let label = if perms.write {
                "read, write"
            } else if perms.read {
                "read only"
            } else {
                "no access — denied, even though it sits under an allowed parent"
            };
            s.push_str(&format!("  - {path} ({label})\n"));
        }
    }
    // The scratch alias (docs/tools.md): a bash move writes `$TMPDIR/…` and the
    // shell expands it in-child, so what the spotter sees is the variable — this
    // line is what connects it to the granted literal path above.
    if let Some(scratch) = scratch {
        s.push_str(&format!(
            "- `$TMPDIR` points at `{}` — the session's private scratch directory \
             (read, write; cleaned up when the session ends)\n",
            scratch.display()
        ));
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
/// structured tool shows what the tool *is* — its brief — and then the call. The
/// instruction to call `spot` rides here (the fresh tail), not in the cached context.
/// When `probe` is `Some`, a promotion question for that prefix rides along too
/// (docs/permissions.md) — the harness names the prefix; the model answers it via
/// the `clear_always` stance.
///
/// The brief goes **before** the input for the same reason a reader wants it there:
/// `{"path": "x"}` is uninterpretable until you know what the tool does with a path.
/// Bash needs no equivalent — its command text is self-describing, which is exactly
/// what a bare structured call is not.
fn render_move(call: &ToolCall, brief: Option<&ToolBrief<'_>>, probe: Option<&str>) -> String {
    let body = if call.name == bash::NAME {
        match call.input.get("command").and_then(Value::as_str) {
            Some(cmd) => format!("A shell command:\n\n    {cmd}"),
            None => format!(
                "A `{}` call with no string command (malformed): {}",
                call.name, call.input
            ),
        }
    } else {
        // The harness-injected `intent` (docs/tools.md) is withheld from the spotter,
        // for now, and the reason is not that it would be useless — it is context for
        // an underdetermined call, which is exactly what a bare structured call lacks.
        //
        // It is withheld because the agent authoring it is the party being judged, and
        // it knows the criteria. Any weight the spotter puts on it is a channel for
        // talking past the gate on assertion alone ("this path is scratch" is not
        // evidence that it is). And a gate that reads the field changes the field:
        // once the session learns the spotter sees it, `intent` stops being a cheap
        // honest note about why and becomes partly a plea, which spends the honesty
        // the condensed-transcript use depends on.
        //
        // Both effects are measurable and neither is measured yet
        // (docs/permissions.md); the red team — commands the spotter blocks, intents
        // written to get them through, the flip rate — comes before turning this on.
        // Withheld is the reversible default.
        let mut shown = call.input.clone();
        if let Some(object) = shown.as_object_mut() {
            object.remove(crate::tools::INTENT);
        }
        let input = serde_json::to_string_pretty(&shown).unwrap_or_else(|_| shown.to_string());
        let display = crate::mcp::display_name(&call.name);
        match brief {
            Some(brief) => format!(
                "{}\nThe call being proposed, with input:\n\n{input}",
                render_brief(&display, brief)
            ),
            None => format!("A `{display}` tool call with input:\n\n{input}"),
        }
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

/// The exact move block [`Spotter::assess`] would send for this call — what the
/// spotter model reads, byte for byte. The `efference spotter` tester prints it, so
/// iterating on a tool's brief doesn't mean guessing at what the model saw.
pub fn preview_move(call: &ToolCall, brief: Option<&ToolBrief<'_>>, probe: Option<&str>) -> String {
    render_move(call, brief, probe)
}

/// How much of a description and of an input schema ride into the move block. A
/// verbose server (or a schema with a large enum) would otherwise push the fresh
/// tail past the useful signal and pay for it on every gated call. Clipped at a
/// char boundary, with the clip marked so a truncated schema is never mistaken for
/// the whole one.
const DESCRIPTION_BUDGET: usize = 1200;
const SCHEMA_BUDGET: usize = 2000;

/// Clip to `max` bytes at a char boundary, marking the clip.
fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… (truncated)", &text[..end])
}

/// Describe the tool a structured move would call: where it came from, what it says
/// it does, what it declares about its own behavior, and what it accepts.
///
/// **On the declarations.** `readOnlyHint` / `destructiveHint` / `idempotentHint` /
/// `openWorldHint` are the server's claims about its own tool — the MCP spec warns
/// clients not to make trust decisions on them for untrusted servers. They are
/// rendered anyway, labelled as self-reported, because this harness's threat model
/// is **error, not deception** (docs/permissions.md): a well-meaning server's hints
/// are true, and a lying one is already out of scope. The asymmetry that survives
/// either way is stated to the spotter: a hint is grounds to *gate*, never
/// sufficient on its own to *clear*.
fn render_brief(display: &str, brief: &ToolBrief<'_>) -> String {
    let definition = brief.definition;
    let mut s = match brief.server {
        Some(server) => format!(
            "The tool `{display}`, from the connected MCP server `{server}` — a server this \
             operator declared, whose tools this harness does not implement or vet.\n"
        ),
        None => format!("The tool `{display}`, built into this harness.\n"),
    };
    if let Some(title) = definition.title.as_deref().filter(|t| !t.trim().is_empty()) {
        s.push_str(&format!("Titled: {title}\n"));
    }

    match definition
        .description
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        Some(description) => s.push_str(&format!(
            "\nWhat it says it does:\n{}\n",
            clip(description, DESCRIPTION_BUDGET)
        )),
        None => s.push_str("\nIt carries no description.\n"),
    }

    let declarations = declarations(definition);
    if !declarations.is_empty() {
        s.push_str(
            "\nWhat the server declares about it (self-reported, not verified here — enough to \
             raise a concern, never enough on its own to settle one):\n",
        );
        for line in declarations {
            s.push_str(&format!("- {line}\n"));
        }
    }

    let schema = serde_json::to_string_pretty(&definition.input_schema)
        .unwrap_or_else(|_| definition.input_schema.to_string());
    s.push_str(&format!(
        "\nWhat it accepts:\n{}\n",
        clip(&schema, SCHEMA_BUDGET)
    ));
    s
}

/// The server's behavior hints, rendered one per declared field. Absent fields are
/// omitted rather than defaulted: "the server said nothing" and "the server said
/// no" are different evidence, and only the first is true of a server that never
/// filled the annotations in.
fn declarations(definition: &ToolDefinition) -> Vec<&'static str> {
    let Some(hints) = definition.annotations.as_ref() else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    if let Some(read_only) = hints.read_only_hint {
        lines.push(match read_only {
            true => "read-only: it does not modify its environment",
            false => "not read-only: it modifies its environment",
        });
    }
    if let Some(destructive) = hints.destructive_hint {
        lines.push(match destructive {
            true => "destructive: its modifications may be irreversible",
            false => "additive: it does not remove or overwrite what is already there",
        });
    }
    if let Some(idempotent) = hints.idempotent_hint {
        lines.push(match idempotent {
            true => "idempotent: repeating it with the same arguments changes nothing further",
            false => "not idempotent: repeating it compounds its effect",
        });
    }
    if let Some(open_world) = hints.open_world_hint {
        lines.push(match open_world {
            true => "open-world: it reaches entities outside this machine (e.g. the network)",
            false => "closed-world: its effects stay inside a closed domain",
        });
    }
    lines
}

/// The forced `spot` tool: a required `stance` enum + one-line `reason`. The stance is 4-valued —
/// `clear_always` is the promotion safe-vote, meaningful only when a probe rode the call
/// (docs/permissions.md); the enum is fixed (never varied per call) so the tool
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
mod test;
