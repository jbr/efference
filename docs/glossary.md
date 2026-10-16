# Glossary — discussion words and the identifiers behind them

Where the word used in conversation, docs, or the clients differs from the names in the
code, the pair is listed here. A rename that closes a gap removes its row. Words with more
than one current meaning are listed with each sense, so a reader can tell which is meant.

| word | identifiers | governing doc |
| --- | --- | --- |
| **triage primer** — a capped request that plants the triage model's cache | `Triager::prime`, `RequestOrigin::TriagePrimer` | transport.md |
| **fold** — replacing earlier history with a written account | `POST /loop/fold`, `fold.rs`, `consolidate` (the agent's tool), `Payload::Mark` with `MarkKind::Boundary { upto }`, "summary head", "narrator" (`narrate::narrate`) | consolidate.md, summary-head.md |
| **gate** — a tool call held for a permission decision | `Approval`, `/state`'s `gate`, the loop's `Gated` position | permissions.md |
| **process monitor** — background-process supervision: spawning, tails, exit notifications, `wait`, `read_process`, `kill_process` | `ProcessMonitor`, `src/process_monitor.rs`, `/processes` | process-monitor.md |
| **upstream** — the model API endpoint | `--upstream`, `api::Upstream`, `upstream_failure`, `upstream_retry` | transport.md |
| **supervisor** — the session that reviews this one's gates and may drive it; by default the creating session, settable after creation | `ReviewLevel::Supervisor`, `resolved_review_level: "supervisor"`, `/state`'s `supervisor`, `Session::supervisor`, `Hosted::supervisor` (the pointer), `Host::set_supervisor`, `PATCH /sessions/:id {"supervisor"}`, the `supervisor` control verb, `telemetry.supervisor_set` | permissions.md |
| **fork**, **branch** — *reserved* for a future session concept (a new variant of an existing session); not to be used for anything else | `TitlePeek`, `ForkSpot` still use *fork*/*peek* for side requests | |
