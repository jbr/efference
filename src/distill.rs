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

// ---- the verdict ----

/// Triaged dispositions: inbox items (seq-keyed) and background processes (handle-keyed).
type Dispositions = (Vec<(Seq, Disposition)>, Vec<(String, BgVerdict)>);

// ---- the peek view ----

#[cfg(test)]
mod test;

mod parse;
mod prompt;
mod schema;

use parse::*;
use prompt::*;
use schema::*;
