//! What the triage lineage's keepalive re-sends (docs/transport.md): on a lineage the
//! primer plants, the primer; on any other own triage client with a prompt cache, the
//! last result triage's request as sent, ending at its last breakpoint.

use super::*;
use crate::api::SentRequest;
use crate::log::Timestamp;

/// A request the triage lineage sent, for its keepalive to re-send (docs/transport.md).
pub struct SentTriage {
    /// The request as its client sent it, ending at its last breakpoint.
    pub request: SentRequest,
    /// When it was sent, taken before the send: a compaction stamped after it moved the
    /// prefix the request read, so the stored request stops describing the live entry.
    pub at: Timestamp,
    /// The request's usage, whose prompt tokens stand for what a re-send reads until a
    /// ping measures it.
    pub usage: RequestUsage,
}

impl Triager {
    /// The client the triage lineage's keepalive pings on (docs/transport.md): the own
    /// client, where its model reports a prompt cache — the same check the main keepalive
    /// arms on. `None` on the shared triage, whose requests ride the main prefix.
    pub fn keepalive_client(&self) -> Option<Client> {
        let own = self.own.as_ref()?;
        own.client
            .prompt_cache()
            .is_supported()
            .then(|| own.client.clone())
    }

    /// Whether a result triage's request is what the keepalive re-sends: an own client
    /// with a prompt cache that plants no primer. Where the primer plants, the keepalive
    /// re-sends the primer instead.
    pub(super) fn resends_results(&self) -> bool {
        self.keepalive_client()
            .is_some_and(|client| !client.plants_primer())
    }

    /// The triage request that went out over `turns` on `client`, ending at its last
    /// breakpoint (the end of the text view's history, docs/triage.md), when it is the one
    /// the keepalive re-sends and a response came back for it. `at` was taken before the
    /// send. Read after the round trip, so a request the forced-choice fallback sent is
    /// stored in that form ([`Client::sent_form`]).
    pub(super) fn sent_triage(
        &self,
        client: &Client,
        turns: Vec<WireTurn>,
        tools: &[Value],
        choice: ToolChoice,
        at: Timestamp,
        record: &TriageRecord,
    ) -> Option<SentTriage> {
        let usage = record.usage.last()?.clone();
        let request = client
            .sent_form(turns, tools, choice)
            .through_last_breakpoint()?;
        Some(SentTriage { request, at, usage })
    }
}
