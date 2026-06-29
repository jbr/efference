# Changelog

All notable changes to efference. Format follows [keep-a-changelog](https://keepachangelog.com);
one line per feature. The project is private and unreleased, so everything lives under
*Unreleased* for now. For how the system works, see [`docs/`](docs/); for the narrative
behind a change, see [`journal/`](journal/).

## [Unreleased]

### Added
- Append-only annotated log + byte-stable deterministic projection — the context substrate.
- Durable JSONL persistence of the log, resumable across restarts.
- Universal redaction fork: every tool result is distilled at append; raw output never reaches the wire.
- `retain` verdict (`all` / line ranges / `none` + note) stored as the wire form and re-rendered as `@@`-hunk output each turn.
- `Triage` audit entries — every fork verdict (and its origin) is reconstructable from the log.
- Visible `[seq]` block handles on tool results for re-read / branch / compare addressing.
- Trajectory consolidation: agent-called `consolidate(summary)` + `Mark::Boundary` projection fold, with branch-from-seq.
- Ambient-notification inbox: out-of-band `POST /notify`, enqueue-then-triage at the fork gate (pin / dismiss / leave), logged at first-show.
- Background-process supervisor: `bash background:true`, live peek tail, directed `list_processes` / `read_process` / `kill_process`, and idle-triggered triage (wake-on-notification).
- Control channel: HTTP-over-Unix-socket daemon (`serve` / `turn` / `approve` / `interrupt` / `state` / `log` / `attach`) with durable `/events` and lossy `/stream`.
- Three-valued approval gate (approve / abort / redirect) over the `Frontend` trait; multi-turn headless via `POST /turn`.
- Upstream SSE streaming with token-level deltas, cache instrumentation, and a `max_tokens:0` keepalive.
- In-process MCP tools (fs-mcp, semantic-edit-mcp) reusing the generated `mcplease` tool enums.
- Interactive TUI (`efference tui`): themed streaming transcript, conversational rendering with per-tool result renderers, emacs-keybind composer, inline approval gates, and a live background-process panel.
- `Screenplay` snapshot harness + `efference gallery` for iterating TUI display without being blind.
- Config (TOML via `toml_edit`): `serve --config <path>` / `efference.toml`, a string-keyed schema (`scope`, `watch-for`, `[commands]`, `[paths]`, `tools`) validated fail-closed (command keys lexed; non-simple keys reject the file), a session `scope` that folds byte-stably into the cached preamble, resolved path reported on `GET /state`.
- Permission fast-path: deterministic, zero-model-cost auto-approval of allowlisted bash — a `yash-syntax` structural-simplicity check (single simple command, no operators/redirection/expansion/glob) then a token-wise `[commands]` prefix match; everything else falls through to the gate.
- Spotter permission rung: an independent, fresh-context model (Haiku band) judges the calls the fast-path can't clear, returning a graded `clear`/`watch`/`gate` stance + one-line reason; verdicts logged as `Spot` audit entries, scope drawn from the config's `[paths]` grants, persona in `assets/prompts/spotter.md`, fail-closed to the human gate. `efference spotter` exercises it on one `(scope, watch-for, command)`.
- Config edits: a `Payload::ConfigEdit` (a `ConfigChange` over `command`/`path`/`tool`/`scope`, `session` or `persist` lifetime) folds into an effective config by replace-at-key (last-write-wins, materialized from the ordered log); the orchestrator commits one at a gate with a fourth `Approval::ConfigEdit` decision, which re-classifies the call against the new policy (it runs only if the changed rule clears it — never an unchecked ride-along) and auto-persists to the TOML file (format-preserving, comment-safe, any key including a `scope` narrowing) when asked.
- Session-visible config edits notify: a committed `scope`/`paths` edit raises an `Ambient` notification (optional tail append, reusing the inbox/triage machinery) so the session learns its perimeter moved — never re-rendering the frozen cached preamble; `commands`/`tools` edits reach the model only through the classifier.

### Changed
- Model selection moved to a per-session `serve --model <id>` flag (default `claude-haiku-4-5` for dev).
- Tool definitions unified into one source shared by the loop, the keepalive ping, and the dev `wire` view (closing a cache-correctness gap).

### Fixed
- Guard against runaway tool results overflowing the request (400 on oversized content).
- Streamed control characters, a turn-loop livelock, and a resume edge case.
