# Glossary — discussion words and the identifiers behind them

Where the word used in conversation, docs, or the clients differs from the names in the
code, the pair is listed here. A rename that closes a gap removes its row. Words with more
than one current meaning are listed with each sense, so a reader can tell which is meant.

| word | identifiers | governing doc |
| --- | --- | --- |
| **triage** — the role that judges each tool result before the agent sees it | `src/distill/`, `Distiller`, `ForkPeek`, `peek` (`peek.rs`, `dev peek`), `WireState::Distilled`, `RequestOrigin::Peek` / `"peek"` in `usage.jsonl`; the web's "distilled" labels | redaction-fork.md |
| **triage verdict** — what triage returns: which lines to keep, and a note | `Distilled { keep, note }`, the `retain` tool | redaction-fork.md |
| **triage primer** — a capped request that plants the triage model's cache | `ForkPeek::prime`, `RequestOrigin::TriagePrimer` | transport.md |
| **fold** — replacing earlier history with a written account | `POST /loop/fold`, `fold.rs`, `consolidate` (the agent's tool), `Payload::Mark` with `MarkKind::Boundary { upto }`, "summary head", "narrator" (`narrate::narrate`) | consolidate.md, summary-head.md |
| **gate** (1) — a tool call held for a permission decision | `Approval`, `/state`'s `gate`, the loop's `Gated` position | permissions.md |
| **gate** (2) — *older sense, being retired*: the triage pass over one completion's results | `src/agent/gate.rs` (the triage hold), `Patience::GATE`, `PEEK_FORKS` | redaction-fork.md |
| **upstream** (1) — the model API endpoint | `--upstream`, `api::Upstream`, `upstream_failure`, `upstream_retry` | transport.md |
| **upstream** (2) — the session that reviews this one's gates | `ReviewLevel::Upstream`, `resolved_review_level: "upstream"` | permissions.md |
| **seam-drain** — queued user turns delivered between a completion's results and the next request | | control-channel.md |
| **fork**, **branch** — *reserved* for a future session concept (a new variant of an existing session); not to be used for anything else | `TitlePeek`, `ForkSpot` still use *fork*/*peek* for side requests | |
