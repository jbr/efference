# Glossary — discussion words and the identifiers behind them

Where the word used in conversation, docs, or the clients differs from the names in the
code, the pair is listed here. A rename that closes a gap removes its row. Words with more
than one current meaning are listed with each sense, so a reader can tell which is meant.

| word | identifiers | governing doc |
| --- | --- | --- |
| **triage primer** — a capped request that plants the triage model's cache | `Triager::prime`, `RequestOrigin::TriagePrimer` | transport.md |
| **summary** — the text that replaces a range of the transcript, and the log entry that carries it | `Payload::Summary(Summary { from, upto, text, made_by, audience })` | summary.md |
| **consolidation** — the agent replaces a region of its own transcript with a summary, having seen that region in full | the `consolidate` tool, `MadeBy::Consolidation`, `RequestOrigin::Consolidation` | consolidate.md |
| **compaction** — the harness writes a summary from the digest, without the full transcript | `MadeBy::Compaction`, `src/compact.rs` (`compact::summarize`), `src/daemon/compact.rs`, `POST /loop/compact`, the `compaction` state topic, `efference dev compact`, `RequestOrigin::Compaction` | summary.md |
| **reasoning summary** — a provider's account of its own reasoning (OpenAI's term); not a *summary* in the sense above | `ResponsesReasoningSummary`, `WireSummaryPart` | transport.md |
| **session listing** — one row per hosted session in `GET /sessions` | `ListedSession`, `ListingEvent`, `Host::listing`, `src/daemon/host/listing.rs` | control-api.md |
| **gate** — a tool call held for a permission decision | `Approval`, `/state`'s `gate`, the loop's `Gated` position | permissions.md |
| **process monitor** — background-process supervision: spawning, tails, exit notifications, `wait`, `read_process`, `kill_process` | `ProcessMonitor`, `src/process_monitor.rs`, `/processes` | process-monitor.md |
| **upstream** — the model API endpoint | `--upstream`, `api::Upstream`, `upstream_failure`, `upstream_retry` | transport.md |
| **supervisor** — the session that reviews this one's gates and may drive it; by default the creating session, settable after creation | `ReviewLevel::Supervisor`, `resolved_review_level: "supervisor"`, `/state`'s `supervisor`, `Session::supervisor`, `Hosted::supervisor` (the pointer), `Host::set_supervisor`, `PATCH /sessions/:id {"supervisor"}`, the `supervisor` control verb, `telemetry.supervisor_set` | permissions.md |
| **fork**, **branch** — *reserved* for a future session concept (a new variant of an existing session); not to be used for anything else | `TitlePeek`, `ForkSpot` still use *fork*/*peek* for side requests | |
