use std::collections::BTreeMap;

use super::assembler::ResponseMeta;
use super::client::record_upstream_failure;
use super::thinking::ThinkingSupport;
use super::*;
use crate::log::{CallId, FRAME_CAPTURE_CAP, is_captured_header};
use crate::project::ApiBlock;

/// Mirror of `Client::complete`'s read loop (docs/transport.md): an event the
/// assembler refuses stops the feed and is recorded as the stream's failure, which
/// `finish` folds into the completion's `upstream_failure` — the partial is never
/// discarded.
fn feed(events: &[(&str, &str)]) -> Result<Completion, Error> {
    let mut a = Assembler::new();
    for (ty, data) in events {
        if let Err(e) = a.apply(Some(ty), data) {
            a.fail(e.to_string());
            break;
        }
    }
    a.finish()
}

/// Everything the sink sees, in order — the live channel, as distinct from the assembled
/// [`Completion`] that `feed` returns.
fn live(events: &[(&str, &str)]) -> Vec<Streamed> {
    let mut a = Assembler::new();
    let mut out = Vec::new();
    for (ty, data) in events {
        if let Some(s) = a.apply(Some(ty), data).unwrap() {
            out.push(s);
        }
    }
    out
}

mod assembly;
mod blocks;
mod cache_chain;
mod capabilities;
mod codex;
mod completions;
mod custom;
mod effort;
mod endpoint;
mod envelope;
mod forced_choice;
mod images;
mod live;
mod partials;
mod retry;
mod stages;
mod triage_ping;
