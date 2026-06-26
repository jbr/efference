//! Distillation — what a raw tool result projects to on the canonical wire
//! (DESIGN §4-realized, §10 Phase 1/2).
//!
//! Every tool result is distilled at append; the durable wire never carries raw
//! output, and a result's `wire` is fixed once and never mutated (so caching is
//! safe by construction). The distiller's job is now to produce a **verdict** —
//! which lines survive (`keep`) plus a `note` — not rendered bytes: the wire form is
//! re-rendered from `raw` + the verdict at projection time
//! ([`crate::project::render_distilled`], DESIGN §4), so the kept bytes are never
//! stored twice and the structured decision doubles as the audit record. The
//! distiller is the pluggable policy (§5) behind a trait:
//!
//! - [`HeadTail`] — a deterministic stub (first `head` + last `tail` lines).
//! - [`ForkPeek`] — the §4 universal triage gate: fork a throwaway branch off the
//!   cached prefix, show the model the full line-numbered output, and let it call
//!   `retain` to choose what the canonical wire keeps. One tool call per turn
//!   (§8), so the fork is always single-result. On any failure it keeps
//!   everything — never loses data.
//!
//! Each call returns a [`Distillation`]: the [`WireState`] plus the audit pieces the
//! loop stamps into a [`Triage`](crate::log::Triage) entry (`origin`, `raw_keep`, and
//! — from Slice C — the inbox dispositions). Two distinct renderings stay split:
//!
//! - [`number_lines`] — every line `cat -n`-numbered. The coordinate system the peek
//!   addresses with `retain(lines=…)`, shown ONLY in the throwaway fork.
//! - [`render_distilled`](crate::project::render_distilled) — what lands on the
//!   **canonical wire**: kept ranges as diff-style `@@ lines a-b @@` hunks, the lines
//!   verbatim. It lives in `crate::project` now (the wire form is re-rendered from
//!   `raw` + the stored verdict, DESIGN §4); the distiller only produces the verdict.
//!   Per-line numbering proved heavy enough to make distillates *fatter* than the raw
//!   they replaced (measured live), so the wire uses one header per *range*; gaps are
//!   implied by the jump in the headers.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::{Client, Completion, ToolChoice, Usage};
use crate::log::{Disposition, Keep, LogEntry, Origin, Payload, RawResult, Seq, ToolCall, WireState};
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
    /// Per-notification dispositions this peek chose (DESIGN §6). Empty until Slice C.
    pub inbox: Vec<(Seq, Disposition)>,
    /// The fork request's [`Usage`], if a model round-trip happened: it reads the
    /// shared canonical prefix (sliding its cache TTL), so the loop folds it into the
    /// §3 telemetry. `None` for a model-free distiller.
    pub usage: Option<Usage>,
}

/// Decide the wire form of a tool result (DESIGN §4). `prefix` is the log through the
/// assistant turn that made `call` (the fork's cached head); `client` lets a
/// model-driven distiller run the fork.
#[allow(async_fn_in_trait)] // single-threaded loop; no Send bound needed.
pub trait Distiller {
    async fn distill(
        &self,
        client: &Client,
        prefix: &[LogEntry],
        call: &ToolCall,
        raw: &RawResult,
    ) -> Distillation;
}

/// Deterministic stub: keep the first `head` + last `tail` lines. Ignores the fork
/// context — for tests and as the model-free fallback.
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
    ) -> Distillation {
        let n = raw.content.lines().count();
        let keep = if n <= self.head + self.tail {
            Keep::All
        } else {
            Keep::Ranges(vec![(1, self.head), (n - self.tail + 1, n)])
        };
        // Model-free: a `Fallback` verdict, no usage to fold into the telemetry.
        Distillation {
            wire: WireState::Distilled { keep, note: None },
            origin: Origin::Fallback,
            raw_keep: String::new(),
            inbox: Vec::new(),
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
    ) -> Distillation {
        // Cap what the peek sees before any model call (the full raw still goes to the
        // log; only the peek works over the first N lines). `render_distilled` re-applies
        // the same cap, so the verdict's line coordinates stay aligned with what the
        // model saw.
        let all: Vec<&str> = raw.content.lines().collect();
        let lines = &all[..all.len().min(PEEK_LINE_CAP)];
        let dropped = all.len() - lines.len();
        // The peek sees the full (capped) output with a number on every line — the
        // coordinate system `retain(lines=…)` addresses. This per-line numbering lives
        // ONLY in the throwaway fork; the canonical wire uses the leaner diff-hunks.
        let mut peek_view = number_lines(lines);
        if dropped > 0 {
            peek_view.push('\n');
            peek_view.push_str(&format!("       … [{dropped} more lines truncated] …"));
        }

        // Fork: cached prefix-through-tool_use ┃ + full result + retain prompt. The
        // prefix hits cache; only the raw + instruction tail is fresh, and it is
        // discarded — the canonical lineage never sees raw (DESIGN §4).
        let mut messages = project(prefix);
        place_trailing_cache_breakpoint(&mut messages);
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
                    text: RETAIN_INSTRUCTION.to_string(),
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
            .complete(&messages, &[retain_tool()], ToolChoice::force(RETAIN), |_| {})
            .await
        {
            Ok(completion) => (parse_retain(&completion), Some(completion.usage)),
            Err(_) => (None, None),
        };

        match parsed {
            Some(p) => Distillation {
                wire: WireState::Distilled { keep: p.keep, note: p.note },
                origin: Origin::Model,
                raw_keep: p.raw_keep,
                inbox: Vec::new(),
                usage,
            },
            // Graceful default (DESIGN §4/§8): keep everything the peek saw rather than
            // drop data when the peek fails. `render_distilled` re-adds the truncation
            // breadcrumb from `raw`, so the fallback stays honest about the cap.
            None => Distillation {
                wire: WireState::Distilled { keep: Keep::All, note: None },
                origin: Origin::Fallback,
                raw_keep: String::new(),
                inbox: Vec::new(),
                usage,
            },
        }
    }
}

fn retain_tool() -> Value {
    json!({
        "name": RETAIN,
        "description": "Choose what of the tool output to keep on the record going forward. The output is line-numbered. Whatever you drop is gone from the working context, so keep anything that might matter — but stay lean.",
        "input_schema": {
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
        }
    })
}

// ---- the verdict ----

/// A parsed `retain` call: the structured `keep`, the optional `note`, and the
/// model's exact pre-parse `keep` string (recorded on the [`Triage`] audit entry,
/// DESIGN §4).
///
/// [`Triage`]: crate::log::Triage
struct ParsedRetain {
    keep: Keep,
    note: Option<String>,
    raw_keep: String,
}

#[derive(Deserialize)]
struct RetainInput {
    keep: String,
    #[serde(default)]
    note: Option<String>,
}

/// Pull the `retain` verdict out of the peek's completion. `None` when the turn
/// didn't call `retain` (or called something else) — the caller then falls back to
/// keep-all (DESIGN §4/§8).
fn parse_retain(completion: &Completion) -> Option<ParsedRetain> {
    let Payload::Assistant { calls, .. } = &completion.assistant else {
        return None;
    };
    let retain = calls.iter().find(|c| c.name == RETAIN)?;
    let RetainInput { keep, note } = RetainInput::deserialize(&retain.input).ok()?;
    let parsed = parse_keep(&keep);
    Some(ParsedRetain { keep: parsed, note, raw_keep: keep })
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

        // note is optional; ranges parse through, raw_keep is preserved verbatim.
        let p = parse_retain(&retain_completion(RETAIN, json!({"keep": "40-52,310-318"})))
            .expect("ranges parse");
        assert_eq!(p.keep, Keep::Ranges(vec![(40, 52), (310, 318)]));
        assert_eq!(p.raw_keep, "40-52,310-318");
        assert_eq!(p.note, None);
    }

    #[test]
    fn parse_retain_is_none_without_a_retain_call() {
        // The peek is force-called to `retain`, but guard anyway: a turn that called
        // something else yields no verdict → keep-all fallback.
        assert!(parse_retain(&retain_completion("bash", json!({"command": "ls"}))).is_none());
    }
}
