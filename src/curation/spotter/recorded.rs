//! A call in a session log and the circumstances the spotter judged it in, rebuilt from
//! the log as it stood when the spotter was asked (docs/permissions.md).
//!
//! `efference dev spotter-request` renders a rung's request from it, and `efference dev
//! spotter-eval` replays a corpus case with it. Everything here is read from the log, so
//! it is what the loop passed: the judged call, the promotion probe recorded on the
//! verdict, the bound leaf of an allowing `agents__decide`, the consented precedents, the
//! utterances, and the committed config edits. The supervision facts are host state the
//! log does not hold, and the tool's brief comes from a mounted toolbox; both are the
//! caller's.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

use crate::config::Config;
use crate::protocol::agents::allows;
use crate::protocol::transcript::Handle;
use crate::record::log::{
    CallId, ConfigChange, EditState, LogEntry, Payload, Seq, Spot, Telemetry, ToolCall,
};
use crate::record::utterances::{Utterances, utterances};
use crate::tools::ToolBrief;

use super::Circumstances;
use super::precedent::consented;

/// A recorded call and what the spotter was given beside it.
pub struct RecordedMove {
    /// The call as the session made it: for a bound leaf, the allowing `agents__decide`.
    pub call: ToolCall,
    /// The move the spotter judged: `call` itself, or the leaf an allowing
    /// `agents__decide` authorizes, under `call`'s id.
    pub judged: ToolCall,
    /// The spotter's verdict on the call and its entry's seq; `None` for a call that was
    /// never spotted.
    pub spot: Option<(Seq, Spot)>,
    /// The first entry the request could not have read: the verdict, or for a call
    /// never spotted, the first later entry about the call. `None` when no later entry
    /// is about it, and the log as it ends was read.
    pub cut: Option<Seq>,
    /// What was written in the session before the cut.
    pub utterances: Utterances,
    /// The consented calls before the cut, oldest first; empty for a bound leaf, whose
    /// history is the leaf session's.
    pub precedents: Vec<ToolCall>,
    /// A bound leaf's approval chain: the session the decision names, down to the session
    /// holding the leaf. Empty for an ordinary move.
    pub chain: Vec<String>,
    /// The committed config edits before the cut, in log order.
    edits: Vec<ConfigChange>,
    /// The committed config edits at or after the cut, with their seqs.
    later_edits: Vec<(Seq, ConfigChange)>,
    /// What the log could not supply, as sentences for the operator.
    pub notes: Vec<String>,
}

impl RecordedMove {
    /// The call `seq` or `call` names in `entries`, with its circumstances.
    ///
    /// `seq` is a verdict's entry, or a transcript handle naming an assistant entry's
    /// call (`8`, or `8.2` for its second); `call` is a call id. Either names the call,
    /// and both may be given when they agree. A call named by its handle or id is shown
    /// with its verdict when it has one, and as it stood before its first later entry
    /// when it was never spotted. `held` reads another session's log by id, to follow an
    /// allowing `agents__decide` to the leaf it authorizes ([`held_in`] reads them from
    /// a project's session base); `None` when that log is not at hand. `session` is the
    /// session's id, which the utterances' tag id is derived from.
    pub fn find(
        session: &str,
        entries: &[LogEntry],
        seq: Option<Handle>,
        call: Option<&str>,
        mut held: impl FnMut(&str) -> Option<Vec<LogEntry>>,
    ) -> Result<Self, String> {
        let (call_id, named_spot) = named_call(entries, seq, call)?;
        let (made_at, made) = entries
            .iter()
            .enumerate()
            .find_map(|(at, entry)| match &entry.payload {
                Payload::Assistant { output, .. } => output
                    .tool_calls()
                    .ok()?
                    .into_iter()
                    .find(|c| c.id == call_id)
                    .map(|c| (at, c)),
                _ => None,
            })
            .ok_or_else(|| format!("the log has no call `{}`", call_id.0))?;

        let mut notes = Vec::new();
        let spots: Vec<usize> = (made_at + 1..entries.len())
            .filter(|&at| matches!(&entries[at].payload, Payload::Spot(s) if s.call_id == call_id))
            .collect();
        let spot_at = named_spot.or_else(|| spots.first().copied());
        if named_spot.is_none()
            && let [first, _, ..] = spots.as_slice()
        {
            notes.push(format!(
                "the call was spotted {} times; the first, seq {}, is shown — name another \
                 by its seq",
                spots.len(),
                entries[*first].seq
            ));
        }
        let cut_at = spot_at
            .or_else(|| (made_at + 1..entries.len()).find(|&at| concerns(&entries[at], &call_id)));
        let before = &entries[..cut_at.unwrap_or(entries.len())];

        let bound = allows(&made);
        let (judged, chain) = match bound {
            false => (made.clone(), Vec::new()),
            true => match leaf(&made, &mut held) {
                Ok((leaf, chain)) => (ToolCall::new(leaf.name, made.id.clone(), leaf.input), chain),
                Err(why) => {
                    notes.push(format!(
                        "the call is an allowing `agents__decide`, and the leaf it authorized \
                         could not be named ({why}); the decision itself is shown as the move"
                    ));
                    (made.clone(), Vec::new())
                }
            },
        };

        let mut edits = Vec::new();
        let mut later_edits = Vec::new();
        for (at, entry) in entries.iter().enumerate() {
            if let Payload::ConfigEdit(edit) = &entry.payload
                && edit.state == EditState::Committed
            {
                match cut_at.is_none_or(|cut| at < cut) {
                    true => edits.push(edit.change.clone()),
                    false => later_edits.push((entry.seq, edit.change.clone())),
                }
            }
        }

        Ok(Self {
            call: made,
            judged,
            spot: spot_at.and_then(|at| match &entries[at].payload {
                Payload::Spot(spot) => Some((entries[at].seq, spot.clone())),
                _ => None,
            }),
            cut: cut_at.map(|at| entries[at].seq),
            utterances: utterances(session, before),
            // A bound leaf's history is the leaf session's, so the loop passes it none
            // (docs/permissions.md).
            precedents: match bound {
                true => Vec::new(),
                false => consented(before),
            },
            chain,
            edits,
            later_edits,
            notes,
        })
    }

    /// The promotion probe the verdict records, which was asked alongside the move.
    pub fn probe(&self) -> Option<&str> {
        self.spot
            .as_ref()
            .and_then(|(_, spot)| spot.probed_prefix.as_deref())
    }

    /// The circumstances the loop passed with the move, `brief` added. The supervision
    /// facts are host state the log does not record, so none are given.
    pub fn circumstances<'a>(&'a self, brief: Option<ToolBrief<'a>>) -> Circumstances<'a> {
        Circumstances::default()
            .with_brief(brief)
            .with_chain(&self.chain)
            .with_probe(self.probe())
            .with_precedents(&self.precedents)
            .with_utterances(Some(&self.utterances))
    }

    /// The session config as the spotter read it at the move, rebuilt from `file`, the
    /// session's config as its file holds it now, with sentences on what may still
    /// differ.
    ///
    /// The file holds every committed edit, later ones included: an approved grant is
    /// written back to it. So each path grant an edit at or after the cut introduced is
    /// taken back out, and the edits before the cut are applied over the result, which
    /// restores a key a later edit changed. A later scope edit cannot be taken back,
    /// since the text it replaced is not recorded. Only what the spotter reads is
    /// rebuilt: command and tool grants decide whether a call reaches the spotter, not
    /// what it is sent.
    pub fn config(&self, file: &Config) -> (Config, Vec<String>) {
        let mut role = file.clone();
        let mut notes = Vec::new();
        let earlier_paths: BTreeSet<&str> = self
            .edits
            .iter()
            .filter_map(|change| match change {
                ConfigChange::Path { path, .. } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        let earlier_scope = self
            .edits
            .iter()
            .any(|change| matches!(change, ConfigChange::Scope { .. }));
        let mut scope_noted = false;
        for (seq, change) in &self.later_edits {
            match change {
                ConfigChange::Path { path, .. }
                    if !earlier_paths.contains(path.as_str())
                        && role.paths.remove(path).is_some() =>
                {
                    notes.push(format!(
                        "the path grant `{path}` was recorded at seq {seq}, after the move, and \
                         is left out; a grant the file held on `{path}` before then is not \
                         recorded"
                    ));
                }
                ConfigChange::Scope { .. } if !earlier_scope && !scope_noted => {
                    scope_noted = true;
                    notes.push(format!(
                        "the scope was replaced at seq {seq}, after the move, and the text it \
                         replaced is not recorded: the scope shown is the file's"
                    ));
                }
                _ => {}
            }
        }
        for change in &self.edits {
            crate::config::policy::apply_config_change(&mut role, change);
        }
        (role, notes)
    }
}

/// A reader of the session logs under `project`'s session base, by session id, for
/// [`RecordedMove::find`]. An id that is not a plain path segment, or a log that does
/// not load, reads as not at hand.
pub fn held_in(project: &Path) -> impl FnMut(&str) -> Option<Vec<LogEntry>> + '_ {
    move |id| {
        if !crate::record::session_dir::plain_id(id) {
            return None;
        }
        let log = project
            .join(crate::record::session_dir::SESSION_BASE)
            .join(id)
            .join(crate::record::session_dir::LOG_NAME);
        crate::record::persist::load(log).ok()
    }
}

/// The call id `seq` or `call` names, and the position of the verdict `seq` names when
/// it names one.
fn named_call(
    entries: &[LogEntry],
    seq: Option<Handle>,
    call: Option<&str>,
) -> Result<(CallId, Option<usize>), String> {
    let Some(handle) = seq else {
        return call
            .map(|id| (CallId::from(id), None))
            .ok_or_else(|| "name a verdict's seq or a call id".to_string());
    };
    let at = entries
        .iter()
        .position(|entry| entry.seq == handle.seq)
        .ok_or_else(|| format!("the log has no entry {}", handle.seq))?;
    let (id, spot_at) = match &entries[at].payload {
        Payload::Spot(spot) if handle.call.is_none() => (spot.call_id.clone(), Some(at)),
        Payload::Spot(_) => {
            return Err(format!(
                "entry {} is a spotter verdict, which holds no calls to number",
                handle.seq
            ));
        }
        Payload::Assistant { output, .. } => {
            let calls = output.tool_calls().unwrap_or_default();
            let n = handle.call.unwrap_or(1);
            let picked = calls.get(n - 1).ok_or_else(|| {
                format!(
                    "entry {} holds {} call(s), so it has no call {n}",
                    handle.seq,
                    calls.len()
                )
            })?;
            (picked.id.clone(), None)
        }
        other => {
            let kind: &'static str = other.into();
            return Err(format!(
                "entry {} is a `{kind}` entry, not a spotter verdict or a call",
                handle.seq
            ));
        }
    };
    match call {
        Some(call) if call != id.0 => Err(format!(
            "entry {handle} is about the call `{}`, not `{call}`",
            id.0
        )),
        _ => Ok((id, spot_at)),
    }
}

/// Whether `entry` records something about the call `id` that happens once the cascade
/// has judged it: its verdict, a decision on it, its start, or its result.
fn concerns(entry: &LogEntry, id: &CallId) -> bool {
    match &entry.payload {
        Payload::Spot(spot) => spot.call_id == *id,
        Payload::ToolResult { call_id, .. } => call_id == id,
        Payload::Telemetry(telemetry) => match telemetry {
            Telemetry::Tool(tool) => tool.call_id == *id,
            Telemetry::CallStarted { call_id } => call_id == id,
            Telemetry::GateDecision(decision) => decision.call_id == *id,
            Telemetry::Escalation(escalation) => escalation.call_id == *id,
            _ => false,
        },
        _ => false,
    }
}

/// The leaf an allowing `agents__decide` authorizes, and the chain of sessions down to
/// the one holding it, read from the held sessions' logs. While the held call is itself
/// an allowing decision, its own target is followed (docs/permissions.md).
fn leaf(
    decide: &ToolCall,
    held: &mut impl FnMut(&str) -> Option<Vec<LogEntry>>,
) -> Result<(ToolCall, Vec<String>), String> {
    let mut chain: Vec<String> = Vec::new();
    let mut current = decide.clone();
    loop {
        let target = current
            .input
            .get("agent")
            .and_then(Value::as_str)
            .ok_or("the decision names no agent")?
            .to_string();
        let handle = match current.input.get("seq") {
            Some(Value::String(handle)) => handle.clone(),
            Some(Value::Number(seq)) => seq.to_string(),
            _ => return Err("the decision names no seq".into()),
        };
        let handle = Handle::parse(&handle)?;
        if chain.contains(&target) {
            return Err(format!("the chain revisits `{target}`"));
        }
        let entries =
            held(&target).ok_or_else(|| format!("the log of `{target}` is not at hand"))?;
        let entry = entries
            .iter()
            .find(|entry| entry.seq == handle.seq)
            .ok_or_else(|| format!("the log of `{target}` has no entry {}", handle.seq))?;
        let Payload::Assistant { output, .. } = &entry.payload else {
            return Err(format!("entry {handle} of `{target}` is not a call"));
        };
        let held_call = output
            .tool_calls()
            .unwrap_or_default()
            .into_iter()
            .nth(handle.call.unwrap_or(1) - 1)
            .ok_or_else(|| format!("entry {} of `{target}` has no call {handle}", handle.seq))?;
        chain.push(target);
        if !allows(&held_call) {
            return Ok((held_call, chain));
        }
        current = held_call;
    }
}

#[cfg(test)]
mod test;
