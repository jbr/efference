# Glossary — discussion words and the identifiers behind them

Where the word used in conversation, docs, or the clients differs from the names in the
code, the pair is listed here. A rename that closes a gap removes its row. Words with more
than one current meaning are listed with each sense, so a reader can tell which is meant.

| word | identifiers | governing doc |
| --- | --- | --- |
| **triage primer** — a capped request that plants the triage model's cache | `Triager::prime`, `RequestOrigin::TriagePrimer` | transport.md |
| **stored request** (triage keepalive) — what the triage lineage's keepalive re-sends: the last primer, or the last result triage's request ending at its last breakpoint (its cache point) | `api::SentRequest`, `SentRequest::through_last_breakpoint`, `triage::SentTriage`, `Session::store_triage_request`, `daemon::keepalive::StoredRequest` | transport.md |
| **text view** — what a separate triage model reads: the history (head summaries, the digest up to the view cut, the triaged tail), then the current step and the ask | `TriageView::Text`, `Triager::view`, `src/triage/text_view.rs`, `efference dev triage --view text` | triage.md |
| **triaged tail** — the text view's recent steps in full, each result as the agent read it; not a triage request's throwaway tail | `TRIAGED_TAIL_HIGH_WATER`, `TRIAGED_TAIL_LOW_WATER`, `digest::Options::verbatim` | triage.md |
| **cut** (1), compaction cut — where a compaction's account ends, the record after it staying verbatim | `digest::CUT`, `digest::Options::cut`, `compaction_cut` | summary.md |
| **cut** (2), view cut — where the text view's digest ends and its triaged tail begins | `TextViewLayout::cuts`, `text_view::history` | triage.md |
| **summary** — the text that replaces a range of the transcript, and the log entry that carries it | `Payload::Summary(Summary { from, upto, text, made_by, audience })` | summary.md |
| **consolidation** — the agent replaces a region of its own transcript with a summary, having seen that region in full | the `consolidate` tool, `MadeBy::Consolidation`, `RequestOrigin::Consolidation` | consolidate.md |
| **compaction** — the harness writes a summary from the digest, without the full transcript | `MadeBy::Compaction`, `src/compact.rs` (`compact::summarize`), `src/daemon/compact.rs`, `POST /loop/compact`, `current_operations.compaction` and the `compaction_failure` state topic, `efference dev compact`, `RequestOrigin::Compaction` | summary.md |
| **reasoning summary** — a provider's account of its own reasoning (OpenAI's term); not a *summary* in the sense above | `ResponsesReasoningSummary`, `WireSummaryPart` | transport.md |
| **session listing** — one row per hosted session in `GET /sessions` | `ListedSession`, `ListingEvent`, `Host::listing`, `src/daemon/host/listing.rs` | control-api.md |
| **gate** — a tool call held for a permission decision | `Approval`, `/state`'s `gate`, the `gated` loop phase | permissions.md |
| **gate decision**, **decider** — the record of one accepted decision on a held call, and who made it | `Telemetry::GateDecision` (`GateDecision { call_id, decision, source }`), `telemetry.gate_decision`, `PUT /approvals/:call_id`'s `source` | control-api.md |
| **grain** — which render a transcript read answers with: the conversation (the default) or the entry-by-entry audit | `daemon::transcript::Grain`, `GET /transcript?grain=audit`, `client::TranscriptRead::audit` | control-api.md |
| **zoom** (1) — a transcript read of one entry or call in full | `GET /transcript?seq=8.2`, `daemon::transcript::zoom`, `transcript::Handle` | control-api.md |
| **zoom** (2) — the web client's image viewer scaling and panning an image | `web/src/ui/zoom.ts` | web-client.md |
| **loop phase** — where the loop is, as one exclusive value | `LoopPhase`, `BlockedReason`, the `loop_phase` state topic, `Session::loop_phase`, `src/daemon/phase.rs` | control-api.md |
| **verb** (loop verb) — an action a client offers on a loop route; whether each route would accept it now is published | the `verbs` state topic, `Session::continue_verdict`, `Session::compaction_verdict`, `Session::turn_delivery`, the TUI's strip and keys | control-api.md, tui.md |
| **process monitor** — background-process supervision: spawning, tails, exit notifications, `wait`, `read_process`, `kill_process` | `ProcessMonitor`, `src/process_monitor.rs`, `/processes` | process-monitor.md |
| **upstream** — the model API endpoint | `--upstream`, `api::Upstream`, `upstream_failure`, `upstream_retry` | transport.md |
| **supervisor** — the session that reviews this one's gates and may drive it; by default the creating session, settable after creation | `ReviewLevel::Supervisor`, `resolved_review_level: "supervisor"`, `/state`'s `supervisor`, `Session::supervisor`, `Hosted::supervisor` (the pointer), `Host::set_supervisor`, `PATCH /sessions/:id {"supervisor"}`, the `supervisor` control verb, `telemetry.supervisor_set` | permissions.md |
| **fork**, **branch** — *reserved* for a future session concept (a new variant of an existing session); not to be used for anything else | `TitlePeek`, `ForkSpot` still use *fork*/*peek* for side requests | |
