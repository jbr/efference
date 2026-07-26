//! Distillation — what a raw tool result projects to on the canonical wire
//! (docs/redaction-fork.md, docs/README.md), and the inbox triage that rides the
//! same fork peek (docs/inbox.md).
//!
//! Every tool result is distilled at append; the durable wire never carries raw
//! output, and a result's `wire` is fixed once and never mutated (so caching is
//! safe by construction). The distiller's job is to produce a **verdict** — which
//! lines survive (`keep`) plus a `note` — not rendered bytes: the wire form is
//! re-rendered from `raw` + the verdict at projection time
//! ([`crate::project::render_distilled`], docs/redaction-fork.md), so the kept bytes
//! are never stored twice and the structured decision doubles as the audit record.
//!
//! The same throwaway peek also triages the **inbox** (docs/inbox.md): the pending
//! ambient notifications are shown under the output, and the `retain` verdict's
//! `inbox` field pins / dismisses them by `[seq]`. The dispositions ride back on the
//! [`Distillation`] and land on the logged [`Triage`](crate::log::Triage). The
//! distiller is the pluggable policy (docs/tools.md) behind a trait:
//!
//! - [`HeadTail`] — a deterministic stub (first `head` + last `tail` lines).
//! - [`ForkPeek`] — the universal triage gate (docs/redaction-fork.md): fork a
//!   throwaway branch off the cached prefix, show the model the line-numbered raw
//!   output + the inbox, and let it call `retain` to choose what the canonical wire
//!   keeps and which notifications to pin. One tool call per turn (docs/README.md).
//!   On any failure it keeps everything and leaves the inbox untouched — never loses
//!   data.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::{Client, Completion, ToolChoice, Usage};
use crate::context::Item;
use crate::inbox::ShownNotification;
use crate::log::{
    Disposition, Keep, LogEntry, Origin, Payload, RawResult, Seq, ToolCall, WireState,
};
use crate::project::{
    ApiBlock, ApiMessage, PEEK_LINE_CAP, Role, place_trailing_cache_breakpoint, project,
};
use crate::supervisor::BgView;

const RETAIN: &str = "retain";

/// The forced tool of the **standalone** idle-triage peek (docs/supervisor.md): no `keep`
/// (there's no tool output to retain), only inbox/background dispositions.
const TRIAGE: &str = "triage";

const TRIAGE_INSTRUCTION: &str = "You're between tasks and have been woken to triage \
out-of-band events. Review the pending notifications (and any running background \
processes) above and call `triage`: `pin` what genuinely needs to enter your working \
context and be acted on now, `dismiss` the noise, and leave anything you're deferring \
unlisted. If you pin nothing you simply go back to idle and nothing changes — so pin only \
what truly warrants picking the work back up.";

const RETAIN_INSTRUCTION: &str = "Above is the FULL, line-numbered output of the \
tool call you just made. Call `retain` to choose what stays on the record going \
forward: \"all\", \"none\", or comma-separated 1-based inclusive line ranges like \
\"40-52,310-318\". Whatever you drop is gone from the working context — so keep \
anything that might matter for the rest of the task, while staying as lean as you \
can. Always add a one-line `note` summarizing what the output showed.";

const INBOX_INSTRUCTION: &str = " You also have pending notifications, listed under \
\"inbox\" above with an [id] each. In the `inbox` field, `pin` the ids worth carrying \
into your working context going forward and `dismiss` the noise; any id you don't list \
is left in the inbox and shown again at the next decision point.";

const BACKGROUND_INSTRUCTION: &str = " Background processes are listed under \
\"background\" above, each with a [bg#n] handle and its recent output. In the \
`background` field, `pin` the handles whose current output you want captured onto the \
record now, and `mute` the ones whose tail you no longer need to see (they keep \
running and still notify you when they exit). Anything you don't list keeps streaming \
its latest tail to you at the next decision point.";

const SURFACE_INSTRUCTION: &str = " Under \"context\" above are one-line-summarized \
files available to you but not yet loaded, each with a [name]. In the `surfaced` field, \
name any that genuinely bear on what you're doing right now — each you name will be \
surfaced as a reminder to read it before you rely on memory. Name nothing if none apply.";

/// A live-process triage disposition (docs/supervisor.md) — distinct from the seq-keyed
/// notification [`Disposition`] because a running process is addressed by its live
/// `bg#<n>` handle, not a frozen arrival seq, and its "dismiss" is **mute** (stop
/// surfacing the tail; the process keeps running), not clear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BgVerdict {
    /// Snapshot the current tail onto the canonical wire — the loop materializes it
    /// as a pinned `Ambient` at the `Triage` position (reusing the notification pin
    /// path).
    Pin,
    /// Stop surfacing the running tail; the process runs on and its exit still notifies.
    Mute,
}

/// The outcome of distilling one tool result (docs/redaction-fork.md): the wire form plus the
/// audit pieces the loop stamps into a [`Triage`](crate::log::Triage) entry. `raw` is
/// never mutated — it stays in the log.
pub struct Distillation {
    /// The canonical wire form — a [`WireState::Distilled`] verdict (kept ranges +
    /// note), re-rendered from `raw` at projection time (docs/redaction-fork.md).
    pub wire: WireState,
    /// Whether the verdict is the model's or the keep-all fallback (docs/redaction-fork.md).
    pub origin: Origin,
    /// The model's exact `keep` string, pre-parse — empty for a model-free distiller.
    pub raw_keep: String,
    /// Per-notification dispositions this peek chose, by arrival `seq` (docs/inbox.md) —
    /// filtered to notifications actually shown. Empty when nothing was pinned or
    /// dismissed (everything left for the next gate).
    pub inbox: Vec<(Seq, Disposition)>,
    /// Live-process dispositions this peek chose, by `bg#<n>` handle (docs/supervisor.md) —
    /// filtered to processes actually shown. The loop materializes a `Pin` into a
    /// snapshot `Ambient` and applies a `Mute` to the supervisor.
    pub background: Vec<(String, BgVerdict)>,
    /// Context-item names this peek chose to surface (docs/redaction-fork.md) —
    /// filtered to the roster actually shown (invented names dropped). Stored on the
    /// `Triage` (`surfaced`); `project` re-renders each as a read-me reminder. Empty
    /// when nothing was surfaced.
    pub surfaced: Vec<String>,
    /// The fork request's [`Usage`], if a model round-trip happened: it reads the
    /// shared canonical prefix (sliding its cache TTL), so the loop folds it into the
    /// cache telemetry (docs/log-and-projection.md). `None` for a model-free distiller.
    pub usage: Option<Usage>,
}

/// The verdict of a **standalone** idle-triage peek (docs/supervisor.md) — a gate that rides no
/// tool result, so it has no [`WireState`]; just the inbox/background dispositions (and
/// the audit pieces the loop stamps into a [`Triage`](crate::log::Triage) with
/// `for_result: None`).
pub struct InboxTriage {
    pub inbox: Vec<(Seq, Disposition)>,
    pub background: Vec<(String, BgVerdict)>,
    pub origin: Origin,
    pub usage: Option<Usage>,
}

/// Decide the wire form of a tool result and triage the inbox + live background
/// processes (docs/redaction-fork.md, docs/inbox.md, docs/supervisor.md). `prefix` is the log through the assistant turn that
/// made `call` (the fork's cached head); `inbox` is the pending notifications and
/// `background` the running processes' fresh tails to show the model.
#[allow(async_fn_in_trait)] // single-threaded loop; no Send bound needed.
pub trait Distiller {
    async fn distill(
        &self,
        client: &Client,
        prefix: &[LogEntry],
        call: &ToolCall,
        raw: &RawResult,
        inbox: &[ShownNotification],
        background: &[BgView],
    ) -> Distillation;

    /// Triage the inbox + background with **no tool result to ride** — the
    /// idle-triggered standalone peek (docs/supervisor.md). The loop calls this when a
    /// notification wakes an idle agent; it escalates to a real turn iff this pins
    /// something. Default: no-op (the stub distiller never escalates).
    async fn triage(
        &self,
        _client: &Client,
        _prefix: &[LogEntry],
        _inbox: &[ShownNotification],
        _background: &[BgView],
    ) -> InboxTriage {
        InboxTriage {
            inbox: Vec::new(),
            background: Vec::new(),
            origin: Origin::Fallback,
            usage: None,
        }
    }
}

/// Deterministic stub: keep the first `head` + last `tail` lines. Ignores the fork
/// context, the inbox, and the background processes — for tests and as the
/// model-free fallback.
pub struct HeadTail {
    pub head: usize,
    pub tail: usize,
}

impl Distiller for HeadTail {
    async fn distill(
        &self,
        _client: &Client,
        _prefix: &[LogEntry],
        _call: &ToolCall,
        raw: &RawResult,
        _inbox: &[ShownNotification],
        _background: &[BgView],
    ) -> Distillation {
        let n = raw.content.lines().count();
        let keep = if n <= self.head + self.tail {
            Keep::All
        } else {
            Keep::Ranges(vec![(1, self.head), (n - self.tail + 1, n)])
        };
        // Model-free: a `Fallback` verdict, no usage, inbox + background + surfacing untouched.
        Distillation {
            wire: WireState::Distilled { keep, note: None },
            origin: Origin::Fallback,
            raw_keep: String::new(),
            inbox: Vec::new(),
            background: Vec::new(),
            surfaced: Vec::new(),
            usage: None,
        }
    }
}

/// The model peek — the universal triage gate (docs/redaction-fork.md) realized.
pub struct ForkPeek;

impl Distiller for ForkPeek {
    async fn distill(
        &self,
        client: &Client,
        prefix: &[LogEntry],
        call: &ToolCall,
        raw: &RawResult,
        inbox: &[ShownNotification],
        background: &[BgView],
    ) -> Distillation {
        // Cap what the peek sees before any model call (the full raw still goes to the
        // log; only the peek works over the first N lines). `render_distilled` re-applies
        // the same cap, so the verdict's line coordinates stay aligned with what the
        // model saw.
        let all: Vec<&str> = raw.content.lines().collect();
        let lines = &all[..all.len().min(PEEK_LINE_CAP)];
        let dropped = all.len() - lines.len();
        // The peek sees the full (capped) output with a number on every line — the
        // coordinate system `retain(lines=…)` addresses. Per-line numbering lives ONLY
        // in the throwaway fork; the canonical wire uses the leaner diff-hunks.
        let mut peek_view = number_lines(lines);
        if dropped > 0 {
            peek_view.push('\n');
            peek_view.push_str(&format!("       … [{dropped} more lines truncated] …"));
        }
        // The pending inbox + live background processes, by their `[seq]` / `[bg#n]`
        // handles (docs/inbox.md, docs/supervisor.md) — the coordinates
        // `retain.{inbox,background}` address. Re-rendered every gate off the cached
        // prefix, so they never touch the canonical wire (docs/log-and-projection.md). The same
        // builder feeds the standalone idle triage.
        append_inbox_and_background(&mut peek_view, inbox, background);
        // The surfacing roster (docs/redaction-fork.md): unloaded `[context]` stub
        // files this session hasn't surfaced yet, by `[name]` — the coordinates the
        // `retain.surfaced` field addresses. Folded from the cached prefix, throwaway tail.
        let roster = surfaceable_roster(prefix);
        append_roster(&mut peek_view, &roster);

        // Fork: cached prefix-through-tool_use ┃ + full result + retain prompt. The
        // prefix hits cache; only the raw + instruction tail is fresh, and it is
        // discarded — the canonical lineage never sees raw (docs/redaction-fork.md).
        let mut messages = project(prefix);
        place_trailing_cache_breakpoint(&mut messages);
        let mut instruction = RETAIN_INSTRUCTION.to_string();
        if !inbox.is_empty() {
            instruction.push_str(INBOX_INSTRUCTION);
        }
        if !background.is_empty() {
            instruction.push_str(BACKGROUND_INSTRUCTION);
        }
        if !roster.is_empty() {
            instruction.push_str(SURFACE_INSTRUCTION);
        }
        messages.push(ApiMessage {
            role: Role::User,
            content: vec![
                ApiBlock::ToolResult {
                    tool_use_id: call.id.0.clone(),
                    content: peek_view,
                    is_error: raw.is_error,
                    cache_control: None,
                },
                ApiBlock::Text {
                    text: instruction,
                    cache_control: None,
                },
            ],
        });

        // The peek is a real round-trip: keep its `usage` even when the verdict is
        // unusable (the request happened and slid the prefix's TTL); only a transport
        // error yields no usage.
        let (parsed, usage) = match client
            // No-op sink: the fork peek's triage tokens are internal and must never
            // surface on `GET /stream` — only the main thread's prose streams.
            .complete(
                &messages,
                &[retain_tool(inbox, background, &roster)],
                ToolChoice::force(RETAIN),
                |_| {},
            )
            .await
        {
            Ok(completion) => (parse_retain(&completion), Some(completion.usage)),
            Err(_) => (None, None),
        };

        match parsed {
            Some(p) => {
                // Keep only dispositions for notifications we actually showed (drop any
                // stray id the model invented).
                let shown: HashSet<Seq> = inbox.iter().map(|n| n.seq).collect();
                let dispositions = p
                    .inbox
                    .into_iter()
                    .filter(|(s, _)| shown.contains(s))
                    .collect();
                // Same guard for background handles: only verdicts on processes we
                // actually showed (drop any handle the model invented).
                let shown_bg: HashSet<&str> =
                    background.iter().map(|b| b.handle.as_str()).collect();
                let bg = p
                    .background
                    .into_iter()
                    .filter(|(h, _)| shown_bg.contains(h.as_str()))
                    .collect();
                // Keep only names actually on the roster (drop any the model invented),
                // deduped — the harness-side guard (docs/redaction-fork.md).
                let offered: HashSet<&str> = roster.iter().map(String::as_str).collect();
                let mut kept = HashSet::new();
                let surfaced = p
                    .surfaced
                    .into_iter()
                    .filter(|n| offered.contains(n.as_str()) && kept.insert(n.clone()))
                    .collect();
                Distillation {
                    wire: WireState::Distilled {
                        keep: p.keep,
                        note: p.note,
                    },
                    origin: Origin::Model,
                    raw_keep: p.raw_keep,
                    inbox: dispositions,
                    background: bg,
                    surfaced,
                    usage,
                }
            }
            // Graceful default (docs/redaction-fork.md, docs/README.md): keep everything the peek saw rather than
            // drop data when the peek fails, and leave the inbox + background untouched
            // (they re-show next gate). `render_distilled` re-adds the truncation
            // breadcrumb from `raw`.
            None => {
                // No usable verdict (the peek errored or didn't call `retain`): keep
                // everything rather than drop data. With a large raw result this is
                // exactly the path that puts the full output on the wire — the ingestion
                // cap (`agent::cap_tool_result`) is what bounds it.
                log::debug!(
                    "fork peek produced no verdict for `{}`; falling back to keep-all",
                    call.name
                );
                Distillation {
                    wire: WireState::Distilled {
                        keep: Keep::All,
                        note: None,
                    },
                    origin: Origin::Fallback,
                    raw_keep: String::new(),
                    inbox: Vec::new(),
                    background: Vec::new(),
                    surfaced: Vec::new(),
                    usage,
                }
            }
        }
    }

    /// The standalone idle-triage peek (docs/supervisor.md, docs/inbox.md): fork off
    /// the cached prefix, show the inbox + background with no tool result, and force
    /// the `triage` tool (no
    /// `keep`). The caller wakes this when an event arrives while idle and escalates to
    /// a real turn iff it pins something. On any failure it pins/dismisses nothing —
    /// the agent simply goes back to idle, losing no data.
    async fn triage(
        &self,
        client: &Client,
        prefix: &[LogEntry],
        inbox: &[ShownNotification],
        background: &[BgView],
    ) -> InboxTriage {
        let mut view = String::new();
        append_inbox_and_background(&mut view, inbox, background);
        view.push_str("\n\n");
        view.push_str(TRIAGE_INSTRUCTION);

        let mut messages = project(prefix);
        place_trailing_cache_breakpoint(&mut messages);
        messages.push(ApiMessage {
            role: Role::User,
            content: vec![ApiBlock::Text {
                text: view,
                cache_control: None,
            }],
        });

        let (parsed, usage) = match client
            .complete(
                &messages,
                &[triage_tool(inbox, background)],
                ToolChoice::force(TRIAGE),
                |_| {},
            )
            .await
        {
            Ok(completion) => (
                parse_dispositions(&completion, TRIAGE),
                Some(completion.usage),
            ),
            Err(_) => (None, None),
        };

        match parsed {
            Some((ibx, bg)) => {
                let shown: HashSet<Seq> = inbox.iter().map(|n| n.seq).collect();
                let shown_bg: HashSet<&str> =
                    background.iter().map(|b| b.handle.as_str()).collect();
                InboxTriage {
                    inbox: ibx.into_iter().filter(|(s, _)| shown.contains(s)).collect(),
                    background: bg
                        .into_iter()
                        .filter(|(h, _)| shown_bg.contains(h.as_str()))
                        .collect(),
                    origin: Origin::Model,
                    usage,
                }
            }
            None => InboxTriage {
                inbox: Vec::new(),
                background: Vec::new(),
                origin: Origin::Fallback,
                usage,
            },
        }
    }
}

/// The `inbox` triage schema field (seq-keyed pin/dismiss) — shared by `retain` and
/// the standalone `triage` tool.
fn inbox_schema_field() -> Value {
    json!({
        "type": "object",
        "description": "Triage your pending notifications by [id]. Unlisted ids are left for the next decision point.",
        "properties": {
            "pin": {
                "type": "array",
                "items": { "type": "integer" },
                "description": "Notification ids to pin into your working context going forward."
            },
            "dismiss": {
                "type": "array",
                "items": { "type": "integer" },
                "description": "Notification ids to clear as noise."
            }
        }
    })
}

/// The `background` triage schema field (handle-keyed pin/mute) — shared by `retain`
/// and the standalone `triage` tool.
fn background_schema_field() -> Value {
    json!({
        "type": "object",
        "description": "Triage live background processes by [bg#n] handle. Unlisted handles keep streaming their tail at the next decision point.",
        "properties": {
            "pin": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Handles whose current tail to capture onto the record now (e.g. \"bg#3\")."
            },
            "mute": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Handles whose running tail to stop showing — they keep running and still notify on exit."
            }
        }
    })
}

/// The `surfaced` schema field (name-keyed) — present only when the roster is non-empty.
/// A plain string array: names are validated against the roster in code (invented ones
/// dropped), not by a schema enum, keeping the guard harness-side (docs/redaction-fork.md).
fn surfaced_schema_field() -> Value {
    json!({
        "type": "array",
        "items": { "type": "string" },
        "description": "Names of available context items (listed under \"context\" above) that genuinely bear on what you're doing now — each is surfaced as a reminder to read it. Omit or leave empty if none apply."
    })
}

/// The forced fork tool. Its `input_schema` gains an `inbox` field only when there are
/// notifications to triage, a `background` field only when processes are running, and a
/// `surfaced` field only when there are unloaded context items to offer — each absent case
/// stays lean (and the model isn't prompted to invent dispositions).
fn retain_tool(shown: &[ShownNotification], background: &[BgView], roster: &[String]) -> Value {
    let mut input_schema = json!({
        "type": "object",
        "properties": {
            "keep": {
                "type": "string",
                "description": "\"all\", \"none\", or comma-separated 1-based inclusive line ranges, e.g. \"40-52,310-318\" (a single line is \"7\")."
            },
            "note": {
                "type": "string",
                "description": "One-line summary of what the output showed; the breadcrumb kept when little or nothing is retained."
            }
        },
        "required": ["keep"]
    });
    if !shown.is_empty() {
        input_schema["properties"]["inbox"] = inbox_schema_field();
    }
    if !background.is_empty() {
        input_schema["properties"]["background"] = background_schema_field();
    }
    if !roster.is_empty() {
        input_schema["properties"]["surfaced"] = surfaced_schema_field();
    }
    json!({
        "name": RETAIN,
        "description": "Choose what of the tool output to keep on the record going forward. The output is line-numbered. Whatever you drop is gone from the working context, so keep anything that might matter — but stay lean.",
        "input_schema": input_schema
    })
}

/// The forced tool of the standalone idle-triage peek (docs/supervisor.md, docs/inbox.md):
/// inbox/background dispositions only, no `keep` (there's no tool output). At least one
/// section is always present (the loop skips the peek when both are empty).
fn triage_tool(shown: &[ShownNotification], background: &[BgView]) -> Value {
    let mut properties = serde_json::Map::new();
    if !shown.is_empty() {
        properties.insert("inbox".into(), inbox_schema_field());
    }
    if !background.is_empty() {
        properties.insert("background".into(), background_schema_field());
    }
    json!({
        "name": TRIAGE,
        "description": "Triage your pending notifications and running background processes. Pin what needs to enter your working context and be acted on now; dismiss noise; leave anything you're deferring unlisted (pinning nothing returns you to idle, unchanged).",
        "input_schema": { "type": "object", "properties": properties }
    })
}

/// Render the inbox + background sections of a peek view (docs/inbox.md, docs/supervisor.md) — shared by the
/// tool-result-riding gate and the standalone idle triage. Each item by its `[seq]` /
/// `[bg#n]` handle, with running-process tails indented.
fn append_inbox_and_background(
    view: &mut String,
    inbox: &[ShownNotification],
    background: &[BgView],
) {
    if !inbox.is_empty() {
        view.push_str(&format!("\n\n═══ inbox · {} pending ═══", inbox.len()));
        for n in inbox {
            view.push_str(&format!("\n[{}] {} · {}", n.seq, n.source, n.body));
        }
    }
    if !background.is_empty() {
        view.push_str(&format!(
            "\n\n═══ background · {} running ═══",
            background.len()
        ));
        for b in background {
            view.push_str(&format!(
                "\n[{}] {} · running {}s · idle {}s",
                b.handle, b.command, b.running_secs, b.idle_secs
            ));
            for line in b.tail.lines() {
                view.push_str("\n    ");
                view.push_str(line);
            }
        }
    }
}

/// The fork's surfacing roster (docs/redaction-fork.md): the names of **listed**
/// `[context]` items — stub files carrying a `path`, read on demand — that no prior gate
/// has surfaced yet. A pure fold over the prefix: the boot `Preamble`'s structured listing
/// (last one wins on a multi-boot log) minus every name any prior `Triage.surfaced` already
/// carried (the surfaced-once dedup, `Triage`-only — no path-load tracking). Inlined and
/// exec items are excluded: their whole body is already on the wire, so there is nothing to
/// read on demand. Names double as the constrained output vocabulary.
fn surfaceable_roster(prefix: &[LogEntry]) -> Vec<String> {
    let mut sections: &[crate::context::Section] = &[];
    let mut surfaced: HashSet<&str> = HashSet::new();
    for entry in prefix {
        match &entry.payload {
            Payload::Preamble { context, .. } => sections = &context.sections,
            Payload::Triage(t) => surfaced.extend(t.surfaced.iter().map(String::as_str)),
            _ => {}
        }
    }
    let mut seen = HashSet::new();
    sections
        .iter()
        .flat_map(|s| &s.items)
        .filter_map(|item| match item {
            Item::Listed { name, .. } => Some(name),
            Item::Inline { .. } | Item::Exec { .. } => None,
        })
        .filter(|name| !surfaced.contains(name.as_str()) && seen.insert(name.as_str()))
        .cloned()
        .collect()
}

/// Render the surfacing roster section of a peek view (docs/redaction-fork.md): the
/// available-but-unloaded context items by `[name]`, the coordinates the `retain.surfaced`
/// field addresses. Only in the tool-result gate — the idle triage rides no work in flight.
fn append_roster(view: &mut String, roster: &[String]) {
    if roster.is_empty() {
        return;
    }
    view.push_str(&format!("\n\n═══ context · {} available ═══", roster.len()));
    for name in roster {
        view.push_str(&format!("\n[{name}]"));
    }
}

// ---- the verdict ----

/// A parsed `retain` call: the structured `keep`, the optional `note`, the model's
/// exact pre-parse `keep` string (for the [`Triage`] audit entry), and the inbox
/// dispositions (unfiltered — the caller drops ids it didn't show).
///
/// [`Triage`]: crate::log::Triage
struct ParsedRetain {
    keep: Keep,
    note: Option<String>,
    raw_keep: String,
    inbox: Vec<(Seq, Disposition)>,
    background: Vec<(String, BgVerdict)>,
    /// Names the model surfaced — **unfiltered**; the caller drops any not on the roster.
    surfaced: Vec<String>,
}

#[derive(Deserialize)]
struct RetainInput {
    keep: String,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    inbox: Option<InboxVerdict>,
    #[serde(default)]
    background: Option<BackgroundVerdict>,
    #[serde(default)]
    surfaced: Vec<String>,
}

#[derive(Deserialize, Default)]
struct InboxVerdict {
    #[serde(default)]
    pin: Vec<u64>,
    #[serde(default)]
    dismiss: Vec<u64>,
}

#[derive(Deserialize, Default)]
struct BackgroundVerdict {
    #[serde(default)]
    pin: Vec<String>,
    #[serde(default)]
    mute: Vec<String>,
}

/// The inbox/background fields of the standalone `triage` tool (no `keep`).
#[derive(Deserialize)]
struct TriageInput {
    #[serde(default)]
    inbox: Option<InboxVerdict>,
    #[serde(default)]
    background: Option<BackgroundVerdict>,
}

/// Triaged dispositions: inbox items (seq-keyed) and background processes (handle-keyed).
type Dispositions = (Vec<(Seq, Disposition)>, Vec<(String, BgVerdict)>);

/// Flatten the optional inbox/background verdicts into disposition lists (unfiltered —
/// the caller drops handles/ids it didn't show). Shared by `retain` and `triage`.
fn collect_dispositions(
    inbox: Option<InboxVerdict>,
    background: Option<BackgroundVerdict>,
) -> Dispositions {
    let mut ibx = Vec::new();
    if let Some(ib) = inbox {
        ibx.extend(ib.pin.into_iter().map(|s| (s, Disposition::Pin)));
        ibx.extend(ib.dismiss.into_iter().map(|s| (s, Disposition::Dismiss)));
    }
    let mut bg = Vec::new();
    if let Some(b) = background {
        bg.extend(b.pin.into_iter().map(|h| (h, BgVerdict::Pin)));
        bg.extend(b.mute.into_iter().map(|h| (h, BgVerdict::Mute)));
    }
    (ibx, bg)
}

/// Pull the `retain` verdict out of the peek's completion. `None` when the turn
/// didn't call `retain` (or called something else) — the caller then falls back to
/// keep-all (docs/redaction-fork.md, docs/README.md).
fn parse_retain(completion: &Completion) -> Option<ParsedRetain> {
    let Payload::Assistant { calls, .. } = &completion.assistant else {
        return None;
    };
    let retain = calls.iter().find(|c| c.name == RETAIN)?;
    let RetainInput {
        keep,
        note,
        inbox,
        background,
        surfaced,
    } = RetainInput::deserialize(&retain.input).ok()?;
    let parsed_keep = parse_keep(&keep);
    let (dispositions, bg) = collect_dispositions(inbox, background);
    Some(ParsedRetain {
        keep: parsed_keep,
        note,
        raw_keep: keep,
        inbox: dispositions,
        background: bg,
        surfaced,
    })
}

/// Pull the inbox/background dispositions from a `triage`-tool completion (the
/// standalone idle peek). `None` when the turn didn't call the named tool.
fn parse_dispositions(completion: &Completion, tool: &str) -> Option<Dispositions> {
    let Payload::Assistant { calls, .. } = &completion.assistant else {
        return None;
    };
    let call = calls.iter().find(|c| c.name == tool)?;
    let TriageInput { inbox, background } = TriageInput::deserialize(&call.input).ok()?;
    Some(collect_dispositions(inbox, background))
}

/// Parse a `keep` string: `all` / `none` / range list. Lenient — malformed tokens are
/// dropped; a list with no valid ranges reads as `none`.
fn parse_keep(s: &str) -> Keep {
    match s.trim().to_ascii_lowercase().as_str() {
        "all" => Keep::All,
        "none" | "" => Keep::None,
        _ => {
            let ranges: Vec<(usize, usize)> = s.split(',').filter_map(parse_range).collect();
            if ranges.is_empty() {
                Keep::None
            } else {
                Keep::Ranges(ranges)
            }
        }
    }
}

fn parse_range(tok: &str) -> Option<(usize, usize)> {
    let tok = tok.trim();
    if tok.is_empty() {
        return None;
    }
    match tok.split_once('-') {
        Some((a, b)) => Some((a.trim().parse().ok()?, b.trim().parse().ok()?)),
        None => {
            let v = tok.parse().ok()?;
            Some((v, v))
        }
    }
}

// ---- the peek view ----

/// Render every line `cat -n`-style (1-based, 6-wide, tab-separated). This is the
/// coordinate system the fork-peek gate (docs/redaction-fork.md) addresses with
/// `retain(lines=…)`, so it is shown ONLY in the throwaway fork — never on the
/// canonical wire, which uses the leaner `@@`-hunks in
/// [`render_distilled`](crate::project::render_distilled).
fn number_lines(lines: &[&str]) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>6}\t{}", i + 1, line))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Usage;
    use crate::log::{CallId, Triage};

    fn lines(s: &str) -> Vec<&str> {
        s.lines().collect()
    }

    /// A `Completion` whose assistant turn made a single `retain` call carrying
    /// `input` — the shape `ForkPeek` forces and `parse_retain` reads.
    fn retain_completion(name: &str, input: Value) -> Completion {
        Completion {
            assistant: Payload::Assistant {
                thinking: vec![],
                text: vec![],
                calls: vec![ToolCall {
                    id: CallId("r".into()),
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
    fn peek_view_numbers_every_line() {
        // The throwaway fork's view — per-line numbers are the `retain` coordinate
        // system. (Only here; never on the canonical wire.)
        let ls = lines("alpha\nbeta\ngamma");
        let view = number_lines(&ls);
        assert!(view.contains("     1\talpha"), "{view}");
        assert!(view.contains("     3\tgamma"), "{view}");
    }

    #[test]
    fn parse_keep_handles_keywords_ranges_and_garbage() {
        assert_eq!(parse_keep("all"), Keep::All);
        assert_eq!(parse_keep(" ALL "), Keep::All);
        assert_eq!(parse_keep("none"), Keep::None);
        assert_eq!(parse_keep(""), Keep::None);
        assert_eq!(parse_keep("7"), Keep::Ranges(vec![(7, 7)]));
        assert_eq!(
            parse_keep(" 40-52 , 310-318 "),
            Keep::Ranges(vec![(40, 52), (310, 318)])
        );
        assert_eq!(parse_keep("just words"), Keep::None);
    }

    #[test]
    fn parse_retain_reads_keep_note_and_raw_keep() {
        let p = parse_retain(&retain_completion(
            RETAIN,
            json!({"keep": "all", "note": "the gist"}),
        ))
        .expect("a retain call parses");
        assert_eq!(p.keep, Keep::All);
        assert_eq!(p.note.as_deref(), Some("the gist"));
        assert_eq!(
            p.raw_keep, "all",
            "the model's exact string is kept for audit"
        );
        assert!(p.inbox.is_empty(), "no inbox field → no dispositions");

        let p = parse_retain(&retain_completion(RETAIN, json!({"keep": "40-52,310-318"})))
            .expect("ranges parse");
        assert_eq!(p.keep, Keep::Ranges(vec![(40, 52), (310, 318)]));
        assert_eq!(p.raw_keep, "40-52,310-318");
        assert_eq!(p.note, None);
    }

    #[test]
    fn parse_retain_reads_inbox_dispositions() {
        let p = parse_retain(&retain_completion(
            RETAIN,
            json!({ "keep": "all", "inbox": { "pin": [5, 7], "dismiss": [6] } }),
        ))
        .expect("parses");
        assert_eq!(
            p.inbox,
            vec![
                (5, Disposition::Pin),
                (7, Disposition::Pin),
                (6, Disposition::Dismiss)
            ]
        );
    }

    #[test]
    fn parse_dispositions_reads_triage_inbox_and_background() {
        // The standalone idle-triage tool carries inbox + background, no `keep`.
        let c = retain_completion(
            TRIAGE,
            json!({
                "inbox": { "pin": [5], "dismiss": [6] },
                "background": { "pin": ["bg#1"], "mute": ["bg#2"] }
            }),
        );
        let (ibx, bg) = parse_dispositions(&c, TRIAGE).expect("a triage call parses");
        assert_eq!(ibx, vec![(5, Disposition::Pin), (6, Disposition::Dismiss)]);
        assert_eq!(
            bg,
            vec![
                ("bg#1".to_string(), BgVerdict::Pin),
                ("bg#2".to_string(), BgVerdict::Mute)
            ]
        );
        // A call by a different name yields nothing.
        assert!(parse_dispositions(&c, "something_else").is_none());
    }

    #[test]
    fn parse_retain_is_none_without_a_retain_call() {
        // The peek is force-called to `retain`, but guard anyway: a turn that called
        // something else yields no verdict → keep-all fallback.
        assert!(parse_retain(&retain_completion("bash", json!({"command": "ls"}))).is_none());
    }

    #[test]
    fn parse_retain_reads_surfaced_names() {
        // The `surfaced` field carries context-item names the model judged relevant
        // (docs/redaction-fork.md); parse_retain reads them unfiltered.
        let p = parse_retain(&retain_completion(
            RETAIN,
            json!({ "keep": "all", "surfaced": ["ferritin", "jsonl-prose"] }),
        ))
        .expect("parses");
        assert_eq!(p.surfaced, vec!["ferritin", "jsonl-prose"]);

        // Absent field → no surfacing (the lean default).
        let p = parse_retain(&retain_completion(RETAIN, json!({ "keep": "all" }))).unwrap();
        assert!(p.surfaced.is_empty());
    }

    fn entry(seq: u64, payload: Payload) -> LogEntry {
        use crate::log::Timestamp;
        LogEntry {
            seq,
            turn: 0,
            at: Timestamp(1000 + seq),
            payload,
        }
    }

    fn preamble_with(items: Vec<Item>) -> LogEntry {
        use crate::context::{Resolved, Section};
        entry(
            0,
            Payload::Preamble {
                system: "sys".into(),
                context: Resolved {
                    sections: vec![Section {
                        heading: Some("Skills".into()),
                        items,
                    }],
                },
            },
        )
    }

    fn listed(name: &str) -> Item {
        Item::Listed {
            name: name.into(),
            description: format!("what {name} does"),
            path: format!("skills/{name}.md"),
        }
    }

    #[test]
    fn roster_lists_stub_items_and_excludes_inlined_and_exec() {
        // Only read-on-demand stubs (`Listed`) are surfaceable; inlined and exec bodies
        // are already whole on the wire, so there's nothing to load.
        let prefix = vec![preamble_with(vec![
            listed("ferritin"),
            Item::Inline {
                name: "CLAUDE".into(),
                path: "CLAUDE.md".into(),
                body: "the repo map".into(),
            },
            Item::Exec {
                name: "Env".into(),
                command: "env".into(),
                body: "PWD=/x".into(),
            },
            listed("jsonl-prose"),
        ])];
        assert_eq!(surfaceable_roster(&prefix), vec!["ferritin", "jsonl-prose"]);
    }

    #[test]
    fn roster_excludes_already_surfaced_names() {
        // Surfaced-once dedup (docs/redaction-fork.md): a name any prior
        // `Triage.surfaced` carried drops out of the roster — folded from the log alone.
        let prefix = vec![
            preamble_with(vec![listed("ferritin"), listed("jsonl-prose")]),
            entry(
                1,
                Payload::Triage(Triage {
                    for_result: Some(1),
                    origin: Origin::Model,
                    raw_keep: "all".into(),
                    inbox: Vec::new(),
                    surfaced: vec!["ferritin".into()],
                }),
            ),
        ];
        assert_eq!(
            surfaceable_roster(&prefix),
            vec!["jsonl-prose"],
            "ferritin was surfaced already; only jsonl-prose remains offerable"
        );
    }

    #[test]
    fn roster_is_empty_without_a_preamble() {
        // No listing → nothing to offer (the pre-snapshot / config-less case).
        assert!(surfaceable_roster(&[]).is_empty());
    }
}
