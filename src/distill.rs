//! Distillation — what a raw tool result projects to on the canonical wire
//! (DESIGN §4-realized, §10 Phase 1/2), and the inbox triage that rides the same
//! fork peek (DESIGN §6).
//!
//! Every tool result is distilled at append; the durable wire never carries raw
//! output, and a result's `wire` is fixed once and never mutated (so caching is
//! safe by construction). The distiller's job is to produce a **verdict** — which
//! lines survive (`keep`) plus a `note` — not rendered bytes: the wire form is
//! re-rendered from `raw` + the verdict at projection time
//! ([`crate::project::render_distilled`], DESIGN §4), so the kept bytes are never
//! stored twice and the structured decision doubles as the audit record.
//!
//! The same throwaway peek also triages the **inbox** (DESIGN §6): the pending
//! ambient notifications are shown under the output, and the `retain` verdict's
//! `inbox` field pins / dismisses them by `[seq]`. The dispositions ride back on the
//! [`Distillation`] and land on the logged [`Triage`](crate::log::Triage). The
//! distiller is the pluggable policy (§5) behind a trait:
//!
//! - [`HeadTail`] — a deterministic stub (first `head` + last `tail` lines).
//! - [`ForkPeek`] — the §4 universal triage gate: fork a throwaway branch off the
//!   cached prefix, show the model the line-numbered raw output + the inbox, and let
//!   it call `retain` to choose what the canonical wire keeps and which notifications
//!   to pin. One tool call per turn (§8). On any failure it keeps everything and
//!   leaves the inbox untouched — never loses data.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::{Client, Completion, ToolChoice, Usage};
use crate::inbox::ShownNotification;
use crate::log::{Disposition, Keep, LogEntry, Origin, Payload, RawResult, Seq, ToolCall, WireState};
use crate::supervisor::BgView;
use crate::project::{
    ApiBlock, ApiMessage, PEEK_LINE_CAP, Role, place_trailing_cache_breakpoint, project,
};

const RETAIN: &str = "retain";

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

/// A live-process triage disposition (DESIGN §6) — distinct from the seq-keyed
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

/// The outcome of distilling one tool result (DESIGN §4): the wire form plus the
/// audit pieces the loop stamps into a [`Triage`](crate::log::Triage) entry. `raw` is
/// never mutated — it stays in the log.
pub struct Distillation {
    /// The canonical wire form — a [`WireState::Distilled`] verdict (kept ranges +
    /// note), re-rendered from `raw` at projection time (DESIGN §4).
    pub wire: WireState,
    /// Whether the verdict is the model's or the keep-all fallback (DESIGN §4).
    pub origin: Origin,
    /// The model's exact `keep` string, pre-parse — empty for a model-free distiller.
    pub raw_keep: String,
    /// Per-notification dispositions this peek chose, by arrival `seq` (DESIGN §6) —
    /// filtered to notifications actually shown. Empty when nothing was pinned or
    /// dismissed (everything left for the next gate).
    pub inbox: Vec<(Seq, Disposition)>,
    /// Live-process dispositions this peek chose, by `bg#<n>` handle (DESIGN §6) —
    /// filtered to processes actually shown. The loop materializes a `Pin` into a
    /// snapshot `Ambient` and applies a `Mute` to the supervisor.
    pub background: Vec<(String, BgVerdict)>,
    /// The fork request's [`Usage`], if a model round-trip happened: it reads the
    /// shared canonical prefix (sliding its cache TTL), so the loop folds it into the
    /// §3 telemetry. `None` for a model-free distiller.
    pub usage: Option<Usage>,
}

/// Decide the wire form of a tool result and triage the inbox + live background
/// processes (DESIGN §4, §6). `prefix` is the log through the assistant turn that
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
        // Model-free: a `Fallback` verdict, no usage, inbox + background untouched.
        Distillation {
            wire: WireState::Distilled { keep, note: None },
            origin: Origin::Fallback,
            raw_keep: String::new(),
            inbox: Vec::new(),
            background: Vec::new(),
            usage: None,
        }
    }
}

/// The model peek — the §4 universal triage gate realized.
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
        // The pending inbox, each item by its [seq] handle (DESIGN §6) — the
        // coordinate `retain.inbox.{pin,dismiss}` addresses.
        if !inbox.is_empty() {
            peek_view.push_str(&format!("\n\n═══ inbox · {} pending ═══", inbox.len()));
            for n in inbox {
                peek_view.push_str(&format!("\n[{}] {} · {}", n.seq, n.source, n.body));
            }
        }
        // The live background processes, each by its [bg#n] handle with a fresh tail
        // and timing (DESIGN §6) — the coordinate `retain.background.{pin,mute}`
        // addresses. Re-rendered every gate, off the cached prefix, so it never
        // touches the canonical wire (§3).
        if !background.is_empty() {
            peek_view.push_str(&format!("\n\n═══ background · {} running ═══", background.len()));
            for b in background {
                peek_view.push_str(&format!(
                    "\n[{}] {} · running {}s · idle {}s",
                    b.handle, b.command, b.running_secs, b.idle_secs
                ));
                for line in b.tail.lines() {
                    peek_view.push_str("\n    ");
                    peek_view.push_str(line);
                }
            }
        }

        // Fork: cached prefix-through-tool_use ┃ + full result + retain prompt. The
        // prefix hits cache; only the raw + instruction tail is fresh, and it is
        // discarded — the canonical lineage never sees raw (DESIGN §4).
        let mut messages = project(prefix);
        place_trailing_cache_breakpoint(&mut messages);
        let mut instruction = RETAIN_INSTRUCTION.to_string();
        if !inbox.is_empty() {
            instruction.push_str(INBOX_INSTRUCTION);
        }
        if !background.is_empty() {
            instruction.push_str(BACKGROUND_INSTRUCTION);
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
                ApiBlock::Text { text: instruction, cache_control: None },
            ],
        });

        // The peek is a real round-trip: keep its `usage` even when the verdict is
        // unusable (the request happened and slid the prefix's TTL); only a transport
        // error yields no usage.
        let (parsed, usage) = match client
            // No-op sink: the fork peek's triage tokens are internal and must never
            // surface on `GET /stream` — only the main thread's prose streams.
            .complete(&messages, &[retain_tool(inbox, background)], ToolChoice::force(RETAIN), |_| {})
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
                let shown_bg: HashSet<&str> = background.iter().map(|b| b.handle.as_str()).collect();
                let bg = p
                    .background
                    .into_iter()
                    .filter(|(h, _)| shown_bg.contains(h.as_str()))
                    .collect();
                Distillation {
                    wire: WireState::Distilled { keep: p.keep, note: p.note },
                    origin: Origin::Model,
                    raw_keep: p.raw_keep,
                    inbox: dispositions,
                    background: bg,
                    usage,
                }
            }
            // Graceful default (DESIGN §4/§8): keep everything the peek saw rather than
            // drop data when the peek fails, and leave the inbox + background untouched
            // (they re-show next gate). `render_distilled` re-adds the truncation
            // breadcrumb from `raw`.
            None => Distillation {
                wire: WireState::Distilled { keep: Keep::All, note: None },
                origin: Origin::Fallback,
                raw_keep: String::new(),
                inbox: Vec::new(),
                background: Vec::new(),
                usage,
            },
        }
    }
}

/// The forced fork tool. Its `input_schema` gains an `inbox` field only when there are
/// notifications to triage, and a `background` field only when processes are running —
/// each absent case stays lean (and the model isn't prompted to invent dispositions).
fn retain_tool(shown: &[ShownNotification], background: &[BgView]) -> Value {
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
        input_schema["properties"]["inbox"] = json!({
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
        });
    }
    if !background.is_empty() {
        input_schema["properties"]["background"] = json!({
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
        });
    }
    json!({
        "name": RETAIN,
        "description": "Choose what of the tool output to keep on the record going forward. The output is line-numbered. Whatever you drop is gone from the working context, so keep anything that might matter — but stay lean.",
        "input_schema": input_schema
    })
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

/// Pull the `retain` verdict out of the peek's completion. `None` when the turn
/// didn't call `retain` (or called something else) — the caller then falls back to
/// keep-all (DESIGN §4/§8).
fn parse_retain(completion: &Completion) -> Option<ParsedRetain> {
    let Payload::Assistant { calls, .. } = &completion.assistant else {
        return None;
    };
    let retain = calls.iter().find(|c| c.name == RETAIN)?;
    let RetainInput { keep, note, inbox, background } =
        RetainInput::deserialize(&retain.input).ok()?;
    let parsed_keep = parse_keep(&keep);
    let mut dispositions = Vec::new();
    if let Some(ib) = inbox {
        dispositions.extend(ib.pin.into_iter().map(|s| (s, Disposition::Pin)));
        dispositions.extend(ib.dismiss.into_iter().map(|s| (s, Disposition::Dismiss)));
    }
    let mut bg = Vec::new();
    if let Some(b) = background {
        bg.extend(b.pin.into_iter().map(|h| (h, BgVerdict::Pin)));
        bg.extend(b.mute.into_iter().map(|h| (h, BgVerdict::Mute)));
    }
    Some(ParsedRetain { keep: parsed_keep, note, raw_keep: keep, inbox: dispositions, background: bg })
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
/// coordinate system the §4 peek addresses with `retain(lines=…)`, so it is shown
/// ONLY in the throwaway fork — never on the canonical wire, which uses the leaner
/// `@@`-hunks in [`render_distilled`](crate::project::render_distilled).
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
    use crate::log::CallId;

    fn lines(s: &str) -> Vec<&str> {
        s.lines().collect()
    }

    /// A `Completion` whose assistant turn made a single `retain` call carrying
    /// `input` — the shape `ForkPeek` forces and `parse_retain` reads.
    fn retain_completion(name: &str, input: Value) -> Completion {
        Completion {
            assistant: Payload::Assistant {
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
        assert_eq!(parse_keep(" 40-52 , 310-318 "), Keep::Ranges(vec![(40, 52), (310, 318)]));
        assert_eq!(parse_keep("just words"), Keep::None);
    }

    #[test]
    fn parse_retain_reads_keep_note_and_raw_keep() {
        let p = parse_retain(&retain_completion(RETAIN, json!({"keep": "all", "note": "the gist"})))
            .expect("a retain call parses");
        assert_eq!(p.keep, Keep::All);
        assert_eq!(p.note.as_deref(), Some("the gist"));
        assert_eq!(p.raw_keep, "all", "the model's exact string is kept for audit");
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
            vec![(5, Disposition::Pin), (7, Disposition::Pin), (6, Disposition::Dismiss)]
        );
    }

    #[test]
    fn parse_retain_is_none_without_a_retain_call() {
        // The peek is force-called to `retain`, but guard anyway: a turn that called
        // something else yields no verdict → keep-all fallback.
        assert!(parse_retain(&retain_completion("bash", json!({"command": "ls"}))).is_none());
    }
}
