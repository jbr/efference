//! The mask half of the `[env]` table (docs/env.md): needles, the marker, and
//! the single scan every string passes through on its way into the log.
//!
//! The choke point is **log append** ([`crate::persist::Log::append_submission`]),
//! not the tool-result path — so tool results, the boot `Preamble`, `Ambient`
//! notifications, user paste, and harness error text are all covered by one
//! mechanism, and every downstream surface (the wire, the jsonl, the TUI, the
//! `control transcript` and `entries` reads) reads an already-filtered record.

use crate::log::Telemetry;
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::sync::RwLock;

use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use serde_json::Value;

use crate::log::{
    AssistantOutput, ConfigChange, ContentBlock, MessagesOutputBlock, Payload,
    ResponsesMessageContent, ResponsesOutputItem, UpstreamFailureRecord,
};

/// Values shorter than this are not masked (GitLab's "maskability" precedent):
/// `SESSION_KEY=dev` would otherwise shred every occurrence of "dev" in every
/// tool result. The startup report names the variable — never the value.
pub const MASKABILITY_FLOOR: usize = 8;

/// How many leading characters of the value the marker shows. **Fixed**, never
/// proportional: a proportional prefix leaks the value's length.
pub const MARKER_PREFIX_CHARS: usize = 4;

/// The replacement for one masked value: the variable's name and a fixed
/// 4-character prefix, e.g. `⟦ANTHROPIC_API_KEY: sk-a… elided by harness⟧`.
///
/// Deterministic and model-legible on purpose. The accidental-capture pattern
/// that motivates masking is diagnostic ("is it set? does it match what the
/// other side has?"), and both questions are answered by the marker's existence
/// and equality without any value bytes crossing. A bare `[REDACTED]` answers
/// neither, and a blocked diagnostic makes a well-intentioned session work
/// around it (`echo ${KEY:0:8}`, `md5sum`) — each workaround a fresh leak.
pub fn marker(name: &str, value: &str) -> String {
    let prefix: String = value.chars().take(MARKER_PREFIX_CHARS).collect();
    format!("⟦{name}: {prefix}… elided by harness⟧")
}

/// The marker for a value registered by [`Masker::absorb_secret`]: the name
/// alone. A host inference credential is short enough that the ordinary
/// four-character prefix could be most of it.
pub fn secret_marker(name: &str) -> String {
    format!("⟦{name}: elided by harness⟧")
}

/// The session's needle set. Append-only for the session's lifetime: a value
/// that has since rotated out is still a secret, and still sits in files the
/// session may read, so there is never a removal.
#[derive(Debug, Default)]
pub struct Masker {
    needles: RwLock<Needles>,
}

#[derive(Debug)]
struct Needles {
    /// `(needle, marker)`, longest needle first, then lexicographic — a total
    /// order, so the same inputs always produce the same scan.
    entries: Vec<(String, String)>,
    /// Which bytes can start a needle; the scan's early-out.
    first: [bool; 256],
    seen: BTreeSet<String>,
}

impl Default for Needles {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            first: [false; 256],
            seen: BTreeSet::new(),
        }
    }
}

impl Masker {
    /// Add every needle derived from `values` (`(name, value)` pairs already
    /// filtered to the maskable ones). Idempotent, and never removes.
    pub fn absorb(&self, values: &[(String, String)]) {
        self.absorb_with_floor(values, true);
    }

    /// Register secret values that must be redacted even when too short for the
    /// ordinary output-noise threshold. Host-owned inference credentials use this
    /// path: a short credential is still a credential, not a reason to print it.
    pub fn absorb_secret(&self, values: &[(String, String)]) {
        self.absorb_with_floor(values, false);
    }

    fn absorb_with_floor(&self, values: &[(String, String)], apply_floor: bool) {
        let mut needles = self.needles.write().unwrap();
        for (name, value) in values {
            let marker = if apply_floor {
                marker(name, value)
            } else {
                secret_marker(name)
            };
            for needle in needles_for(value, apply_floor) {
                if needles.seen.insert(needle.clone()) {
                    needles.first[needle.as_bytes()[0] as usize] = true;
                    needles.entries.push((needle, marker.clone()));
                }
            }
        }
        needles
            .entries
            .sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    }

    pub fn is_empty(&self) -> bool {
        self.needles.read().unwrap().entries.is_empty()
    }

    /// Scan `text` **once**, left to right, replacing each needle with its
    /// marker. Longest-first ordering settles substring nesting, and advancing
    /// past an emitted marker is what keeps a short needle from matching inside
    /// one (a repeated `replace` could not promise that).
    pub fn mask<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let needles = self.needles.read().unwrap();
        if needles.entries.is_empty() {
            return Cow::Borrowed(text);
        }
        let bytes = text.as_bytes();
        let mut out: Option<String> = None;
        let mut i = 0;
        let mut copied = 0;
        while i < bytes.len() {
            if needles.first[bytes[i] as usize]
                && let Some((needle, marker)) = needles
                    .entries
                    .iter()
                    .find(|(needle, _)| bytes[i..].starts_with(needle.as_bytes()))
            {
                let out = out.get_or_insert_with(String::new);
                out.push_str(&text[copied..i]);
                out.push_str(marker);
                i += needle.len();
                copied = i;
                continue;
            }
            i += 1;
        }
        match out {
            Some(mut out) => {
                out.push_str(&text[copied..]);
                Cow::Owned(out)
            }
            None => Cow::Borrowed(text),
        }
    }

    fn mask_in_place(&self, text: &mut String) {
        if let Cow::Owned(masked) = self.mask(text) {
            *text = masked;
        }
    }

    fn mask_opt(&self, text: &mut Option<String>) {
        if let Some(text) = text {
            self.mask_in_place(text);
        }
    }

    /// A config change's one text field: a command prefix, a path, a tool name, or
    /// the scope prose.
    fn mask_change(&self, change: &mut ConfigChange) {
        match change {
            ConfigChange::Command { command } => self.mask_in_place(command),
            ConfigChange::Path { path, .. } => self.mask_in_place(path),
            ConfigChange::Tool { name } => self.mask_in_place(name),
            ConfigChange::Scope { text } => self.mask_in_place(text),
        }
    }

    /// The record-only mark and, when the empty-stream path attached them, its
    /// captured diagnostics: the header values, the `message_start` body, and the
    /// frames. All three are upstream text that a request's own bytes can echo
    /// back, so all three are scanned (docs/env.md, docs/transport.md).
    fn mask_failure(&self, failure: &mut UpstreamFailureRecord) {
        self.mask_in_place(&mut failure.why);
        if let Some(stream) = &mut failure.stream {
            for value in stream.headers.values_mut() {
                self.mask_in_place(value);
            }
            self.mask_opt(&mut stream.message_start);
            self.mask_in_place(&mut stream.frames);
        }
    }

    fn mask_blocks(&self, blocks: &mut [ContentBlock]) {
        for block in blocks {
            // `Thinking` / `RedactedThinking` are deliberately untouched: the
            // provider mints a signature over the block and rejects a modified
            // one on replay, so masking there would break the session rather
            // than protect it (docs/env.md names this gap).
            if let ContentBlock::Text(text) = block {
                self.mask_in_place(text);
            }
        }
    }

    /// A call's argument string, which is the replay authority
    /// (docs/transport.md): parse, mask the values, and re-serialize only when a
    /// value changed, so the common call keeps the provider's exact bytes. A
    /// string that does not parse is scanned as text: it is record-only, never
    /// dispatched, and still must not carry the secret to disk.
    fn mask_arguments(&self, arguments: &mut String) {
        match serde_json::from_str::<Value>(arguments) {
            Ok(original) => {
                let mut masked = original.clone();
                self.mask_json(&mut masked);
                if masked != original {
                    *arguments = masked.to_string();
                }
            }
            Err(_) => self.mask_in_place(arguments),
        }
    }

    fn mask_json(&self, value: &mut Value) {
        match value {
            Value::String(text) => self.mask_in_place(text),
            Value::Array(items) => items.iter_mut().for_each(|item| self.mask_json(item)),
            // Object *keys* are field names the tool schema defines, not
            // carriers of a value; masking one would rename a field.
            Value::Object(map) => map.values_mut().for_each(|item| self.mask_json(item)),
            _ => {}
        }
    }

    /// Filter every text-bearing field of a payload about to be appended. The
    /// paths, exhaustively: `User` text blocks; `Assistant` text blocks, tool-call
    /// `input` strings, and `upstream_failure` (its `why` and, when present, its
    /// captured stream diagnostics); `ToolResult`'s `raw.content`;
    /// `Summary.text`; `Triage`'s `raw_keep`, `surfaced`, `retries`, and `thinking`;
    /// `Ambient`'s `source`/`body`/`slot`; `Spot`'s `reason` and `probed_prefix`;
    /// `ConfigEdit`'s `note` and its change payload; `Preamble`'s `system` and
    /// its resolved `[context]` listing.
    ///
    /// Not covered, by construction: signed thinking blocks (above) and
    /// `ResultImage.data` (base64 image bytes, where a substitution would
    /// corrupt the image rather than protect anything).
    pub fn mask_payload(&self, payload: &mut Payload) {
        if self.is_empty() {
            return;
        }
        match payload {
            Payload::User(blocks) => self.mask_blocks(blocks),
            Payload::Assistant {
                output,
                upstream_failure,
            } => {
                match output {
                    AssistantOutput::Messages(blocks) => {
                        for block in blocks {
                            match block {
                                MessagesOutputBlock::Text(text) => self.mask_in_place(text),
                                MessagesOutputBlock::ToolUse(call) => {
                                    self.mask_json(&mut call.input)
                                }
                                MessagesOutputBlock::Thinking { .. }
                                | MessagesOutputBlock::RedactedThinking { .. } => {}
                            }
                        }
                    }
                    AssistantOutput::Responses(items) => {
                        for item in items {
                            match item {
                                ResponsesOutputItem::Reasoning {
                                    summary, content, ..
                                } => {
                                    for part in summary {
                                        self.mask_in_place(&mut part.text);
                                    }
                                    for part in content {
                                        self.mask_in_place(&mut part.text);
                                    }
                                }
                                ResponsesOutputItem::Message { content, .. } => {
                                    for part in content {
                                        match part {
                                            ResponsesMessageContent::OutputText {
                                                text, ..
                                            } => self.mask_in_place(text),
                                            ResponsesMessageContent::Refusal { refusal } => {
                                                self.mask_in_place(refusal)
                                            }
                                        }
                                    }
                                }
                                // The argument string is the replay authority
                                // (docs/transport.md): it goes back to the provider
                                // byte for byte. Parse, mask the values, and
                                // re-serialize only when a value changed, so the
                                // common call keeps the provider's exact bytes and a
                                // masked one is the same security override a Messages
                                // `tool_use` input takes (docs/env.md). A string that
                                // does not parse is scanned as text: it is
                                // record-only, never dispatched, and still must not
                                // carry the secret to disk.
                                ResponsesOutputItem::FunctionCall { arguments, .. } => {
                                    self.mask_arguments(arguments)
                                }
                                ResponsesOutputItem::Unsupported(value) => self.mask_json(value),
                            }
                        }
                    }
                    // The defined keys only. Every other key is opaque and may be
                    // signed reasoning, left unmasked for the reason Messages
                    // thinking is (above).
                    AssistantOutput::Completions(message) => {
                        let message = message.as_map_mut();
                        match message.get_mut("content") {
                            Some(Value::String(text)) => self.mask_in_place(text),
                            Some(Value::Array(parts)) => {
                                for part in parts.iter_mut().filter(|part| {
                                    part.get("type").and_then(Value::as_str) == Some("text")
                                }) {
                                    if let Some(Value::String(text)) = part.get_mut("text") {
                                        self.mask_in_place(text);
                                    }
                                }
                            }
                            _ => {}
                        }
                        if let Some(Value::String(refusal)) = message.get_mut("refusal") {
                            self.mask_in_place(refusal);
                        }
                        if let Some(Value::Array(calls)) = message.get_mut("tool_calls") {
                            for call in calls {
                                if let Some(Value::String(arguments)) = call
                                    .get_mut("function")
                                    .and_then(|function| function.get_mut("arguments"))
                                {
                                    self.mask_arguments(arguments);
                                }
                            }
                        }
                    }
                }
                if let Some(failure) = upstream_failure {
                    self.mask_failure(failure);
                }
            }
            Payload::ToolResult { raw, .. } => self.mask_in_place(&mut raw.content),
            Payload::ResultVerdict { wire, .. } => match wire {
                crate::log::WireState::Triaged(verdict) => {
                    self.mask_opt(&mut verdict.note);
                    self.mask_opt(&mut verdict.intent_feedback);
                }
                crate::log::WireState::TriageFailed { why } => self.mask_in_place(why),
                _ => {}
            },
            Payload::Summary(summary) => self.mask_in_place(&mut summary.text),
            Payload::Triage(triage) => {
                self.mask_in_place(&mut triage.raw_keep);
                for name in &mut triage.surfaced {
                    self.mask_in_place(name);
                }
                for retry in &mut triage.retries {
                    self.mask_in_place(retry);
                }
                for thought in &mut triage.thinking {
                    self.mask_in_place(thought);
                }
            }
            Payload::Ambient(note) => {
                self.mask_in_place(&mut note.source);
                self.mask_in_place(&mut note.body);
                self.mask_opt(&mut note.slot);
            }
            Payload::Spot(spot) => {
                self.mask_in_place(&mut spot.reason);
                self.mask_opt(&mut spot.probed_prefix);
            }
            // Per-call telemetry (docs/tools.md) carries no free text a secret
            // could ride in: a tool name, a duration, byte counts, an outcome
            // token, and the call's own `operation` argument — every one of them
            // a value the tool table or the harness produced, not a value from
            // the environment. Nothing to mask (docs/env.md).
            Payload::Telemetry(Telemetry::Tool(_))
            | Payload::Telemetry(Telemetry::CallStarted { .. }) => {}
            // A request stage (docs/log-and-projection.md) is an id, an origin, a
            // status, and — on a failure — the provider's own error text, which
            // can quote what the request carried.
            Payload::Telemetry(Telemetry::RequestStage(stage)) => {
                if let crate::log::RequestStageKind::Failed { reason } = &mut stage.stage {
                    self.mask_in_place(reason);
                }
            }
            // A push record (docs/web-push.md) is a moment number, a kind, client
            // ids, and per-device answers whose free text has the endpoint
            // replaced before it is logged. Nothing from the environment.
            Payload::Telemetry(Telemetry::Push(_)) => {}
            // A cold-check record: a reason, instants, a token count, and a gesture
            // name. Nothing from the environment.
            Payload::Telemetry(Telemetry::Cold(_)) => {}
            // A compaction request is a flag and a seq; its failure is error text that
            // can quote the provider's answer or the request.
            Payload::Telemetry(Telemetry::CompactionRequested { .. }) => {}
            Payload::Telemetry(Telemetry::CompactionFailed { error }) => self.mask_in_place(error),
            // A notification's seq and a count. Nothing from the environment.
            Payload::Telemetry(Telemetry::InboxExpired { .. }) => {}
            // A supervisor change carries a session id. Nothing from the environment.
            Payload::Telemetry(Telemetry::SupervisorSet { .. }) => {}
            // An interrupt's origin is two client-asserted labels.
            Payload::Telemetry(Telemetry::Interrupt(origin)) => {
                if let Some(source) = origin.source.as_mut() {
                    self.mask_in_place(source);
                }
                if let Some(control) = origin.control.as_mut() {
                    self.mask_in_place(control);
                }
            }
            // A gate decision's decider is a client-asserted label, and a redirect's
            // message or a config edit's change and note are text a person wrote.
            Payload::Telemetry(Telemetry::GateDecision(decision)) => {
                self.mask_opt(&mut decision.source);
                match &mut decision.decision {
                    crate::frontend::Approval::Approve | crate::frontend::Approval::Abort => {}
                    crate::frontend::Approval::Redirect(message) => self.mask_in_place(message),
                    crate::frontend::Approval::ConfigEdit { change, note } => {
                        self.mask_in_place(note);
                        self.mask_change(change);
                    }
                }
            }
            // Tool definitions are what the tools advertise, the same bytes the
            // provider receives unmasked; nothing in them is read from the environment.
            Payload::Telemetry(Telemetry::Tools { .. }) => {}
            Payload::Telemetry(Telemetry::Consolidation { outcome, .. }) => {
                self.mask_in_place(outcome)
            }
            // Git's output, including whatever the project's `post-checkout` hook
            // printed, and its error and status text: free text that may quote the
            // environment.
            Payload::Worktree(event) => match &mut event.kind {
                crate::log::WorktreeEventKind::Added(checkout)
                | crate::log::WorktreeEventKind::Restored(checkout) => {
                    self.mask_in_place(&mut checkout.output)
                }
                crate::log::WorktreeEventKind::Kept { reason } => self.mask_in_place(reason),
                crate::log::WorktreeEventKind::RemoveFailed { error } => self.mask_in_place(error),
                crate::log::WorktreeEventKind::Removed { .. } => {}
            },
            // The provider's error text, which may quote the request back.
            Payload::ImagesRefused { refusal } => self.mask_in_place(refusal),
            // A gap, handle ids, and call ids: nothing from the environment.
            Payload::Resumed(_) | Payload::Archived {} => {}
            // A process lifecycle record (docs/process-monitor.md) carries one piece
            // of free text — the command line, which is exactly where an
            // interpolated secret would ride (docs/env.md). The rest is an
            // ordinal, an exit status, and stamps.
            Payload::ProcessEvent(event) => {
                if let crate::log::ProcessEventKind::Spawned(spawn) = &mut event.kind {
                    self.mask_in_place(&mut spawn.command);
                }
            }
            Payload::ConfigEdit(edit) => {
                self.mask_in_place(&mut edit.note);
                self.mask_change(&mut edit.change);
            }
            Payload::Preamble {
                identity,
                body,
                context,
                ..
            } => {
                self.mask_in_place(identity);
                self.mask_in_place(body);
                self.mask_context(context);
            }
            // Model- or operator-written prose about the session; a secret could
            // be quoted into it like any other text.
            Payload::Title(title) => self.mask_in_place(&mut title.text),
        }
    }

    /// Filter a resolved `[context]` listing. Called directly at boot, before
    /// the preamble is rendered — see [`crate::env_table::EnvPolicy::mask_context`].
    pub fn mask_context(&self, resolved: &mut crate::context::Resolved) {
        use crate::context::Item;
        for section in &mut resolved.sections {
            self.mask_opt(&mut section.heading);
            self.mask_opt(&mut section.description);
            for item in &mut section.items {
                match item {
                    Item::Inline { name, path, body } => {
                        self.mask_in_place(name);
                        self.mask_in_place(path);
                        self.mask_in_place(body);
                    }
                    Item::Listed {
                        name,
                        description,
                        path,
                    } => {
                        self.mask_in_place(name);
                        self.mask_in_place(description);
                        self.mask_in_place(path);
                    }
                    Item::Exec {
                        name,
                        command,
                        body,
                    } => {
                        self.mask_in_place(name);
                        self.mask_in_place(command);
                        self.mask_in_place(body);
                    }
                }
            }
        }
    }
}

/// Every needle one value contributes.
///
/// A secret rarely reaches a tool result verbatim, so the raw value alone
/// misses the common cases (GitHub Actions' `add-mask` precedent):
///
/// - **line-wise for a multiline value** — the top real-world miss is a PEM key
///   read back through `cat -n` / `grep` / `diff`, where every line carries a
///   prefix and the whole blob never appears contiguously.
/// - **base64 at three phase offsets** — a value embedded in a larger base64
///   blob encodes differently depending on its byte offset mod 3, so all three
///   alignments are needled.
/// - **percent-encoded and JSON-escaped** — the two shapes a value takes when
///   it rides a URL or a JSON body back out of a tool.
///
/// Ordering here does not matter; [`Masker::absorb`] imposes the total order
/// the scan depends on.
fn needles_for(value: &str, apply_floor: bool) -> Vec<String> {
    let mut out = Vec::new();
    if apply_floor && value.chars().count() < MASKABILITY_FLOOR {
        return out;
    }
    let mut push = |needle: String| {
        if !needle.is_empty() {
            out.push(needle);
        }
    };
    // The whole value, and its trimmed form — an env file's trailing newline
    // must not be the difference between a hit and a miss.
    let mut units: Vec<&str> = vec![value];
    let trimmed = value.trim();
    if trimmed != value {
        units.push(trimmed);
    }
    // Line-wise, for the PEM case.
    if value.contains('\n') {
        for line in value.lines() {
            let line = line.trim();
            if line.chars().count() >= MASKABILITY_FLOOR {
                units.push(line);
            }
        }
    }
    for unit in &units {
        push((*unit).to_string());
        push(percent_encoded(unit));
        push(json_escaped(unit));
    }
    // base64 only for the value as a whole: the encoding of a fragment of a
    // multiline value is not something any tool produces.
    for encoded in base64_phases(trimmed.as_bytes()) {
        push(encoded);
    }
    out.sort();
    out.dedup();
    out
}

/// The three base64 alignments of `value`. Prepending 0/1/2 filler bytes shifts
/// the value into each position within a 3-byte group; the characters that mix
/// filler bits (2 leading for phase 1, 3 for phase 2) and the final character
/// when the group does not close (its low bits are zero-fill, not value bits)
/// are dropped, leaving only characters that depend on the value alone.
fn base64_phases(value: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for phase in 0..3usize {
        let mut buf = vec![b'A'; phase];
        buf.extend_from_slice(value);
        let encoded = STANDARD_NO_PAD.encode(&buf);
        let lead = match phase {
            0 => 0,
            1 => 2,
            _ => 3,
        };
        let trail = usize::from(!(phase + value.len()).is_multiple_of(3));
        if encoded.len() > lead + trail {
            out.push(encoded[lead..encoded.len() - trail].to_string());
        }
    }
    out
}

/// Strict percent-encoding: everything outside the RFC 3986 unreserved set is
/// escaped, uppercase hex.
fn percent_encoded(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

/// The value as it appears **inside** a JSON string literal (the surrounding
/// quotes dropped) — what a tool that echoes a request body back produces.
fn json_escaped(value: &str) -> String {
    let quoted = serde_json::to_string(value).unwrap_or_default();
    if quoted.len() >= 2 {
        quoted[1..quoted.len() - 1].to_string()
    } else {
        String::new()
    }
}
