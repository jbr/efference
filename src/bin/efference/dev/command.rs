//! The clap tree of `efference dev` (docs/operating.md): one variant per verb, and the
//! `--name` flag the reader verbs share.

use super::*;

/// The recorded session a reader verb opens, as its positional names it: the flag
/// is the same answer spelled `--name`, for the id of a session this project ran.
#[derive(clap::Args)]
pub(crate) struct ReadName {
    /// The session to read, by id; what the positional would name.
    #[arg(long)]
    pub(crate) name: Option<String>,
}

#[derive(Subcommand)]
pub(crate) enum DevCommand {
    /// Fixed-data new-session form; no host, credentials, or session creation.
    SessionForm {
        /// Open inside the offline gallery's Sessions screen instead of inline.
        #[arg(long)]
        embedded: bool,
    },
    /// Mint a fresh sign-in link against a running daemon's TCP bind and open it —
    /// the re-auth path when at the host, and the answer to lost scrollback.
    /// The credential routes answer only on the Unix socket: possession of the host machine is the enrollment authority. Lives under `dev`
    /// because it mints credentials for the HTTP surface: it is not a session-control
    /// tool and must not be reachable through `efference mcp`.
    // docs/web-bind.md
    Open {
        /// Print the link without launching a browser.
        #[arg(long)]
        print: bool,
    },
    /// Report a session's prompt-cache read rate from its `usage.jsonl` — the empirical half of the prefix oracle the
    /// byte pins cover statically. Offline: no daemon, no network. Reads warm/cold
    /// per `RequestOrigin`, separates the cold reads that need no explanation
    /// (session boot, a model change, an idle gap past the cache TTL) from the ones
    /// that do, and reports the longest run of the latter — a long run means the
    /// provider stopped matching a prefix the pins say did not change.
    // docs/log-and-projection.md
    Cache {
        /// Which session to read: a session name (`.efference/<name>/usage.jsonl`), a
        /// session directory, or a path to a `usage.jsonl`. Omit for the most
        /// recently written session.
        session: Option<String>,
        #[command(flatten)]
        read: ReadName,
        /// Sweep every session under `.efference/` instead of one — the corpus view,
        /// which is where a rate is worth comparing against.
        #[arg(long, conflicts_with_all = ["session", "name"])]
        all: bool,
    },
    /// Report what a session's turn endings bought. A turn ends when the model
    /// emits no tool call, which is the harness's only way to put a question *and*
    /// its only way to say the work is done. This counts which it turned out to be,
    /// by what the next turn carried: a **barren** ending asked nothing and was
    /// answered with nothing, so it bought a round trip and no information.
    /// Offline: no daemon, no network.
    // docs/operating.md
    Stops {
        /// Which session to read: a session name (`.efference/<name>/log.jsonl`), a
        /// session directory, or a path to a `log.jsonl`. Omit for the most
        /// recently written session.
        session: Option<String>,
        #[command(flatten)]
        read: ReadName,
        /// Sweep every session under `.efference/` instead of one — the corpus view,
        /// which is where a rate is worth comparing against.
        #[arg(long, conflicts_with_all = ["session", "name"])]
        all: bool,
    },
    /// Print a session's trajectory digest: one line per tool call —
    /// its tool, paths, intent and triaged note — plus verbatim operator turns and
    /// the agent's turn-ending replies. A deterministic fold over the log; offline, no
    /// daemon, no model.
    // docs/digest.md
    Digest {
        /// Which session to read: a session name (`.efference/<name>/log.jsonl`), a
        /// session directory, or a path to a `log.jsonl`. Omit to use `--name`.
        session: Option<String>,
        #[command(flatten)]
        read: ReadName,
        /// Fold only entries with seq at or below this one.
        #[arg(long)]
        upto: Option<u64>,
        /// Label each block with the seq of the entry it came from.
        #[arg(long)]
        seqs: bool,
    },
    /// Write a compaction summary for a session: the session's own
    /// model reads its digest and writes the summary a compaction would splice in.
    /// Prints it; appends nothing. One live request on the session's connection.
    // docs/summary.md
    Compact {
        /// Which session to read: a session name (`.efference/<name>/log.jsonl`), a
        /// session directory, or a path to a `log.jsonl`. Omit to use `--name`.
        session: Option<String>,
        #[command(flatten)]
        read: ReadName,
        /// Treat the log as ending at this seq, as if the session had gone cold there.
        #[arg(long)]
        upto: Option<u64>,
        /// End the summary at this seq: the model reads past it but summarizes only up
        /// to it, as for a compaction that keeps a verbatim tail. Omit to cover everything.
        #[arg(long)]
        cut: Option<u64>,
    },
    /// What a configured connection's provider lists, read live with that
    /// connection's credential. Resolves the connection as a
    /// session would, GETs the listing route, and prints the status and entries
    /// as they arrived — the other OpenAI arm's route beside it, so one run says
    /// whether each answers the other's credential. Live API calls; no daemon,
    /// no session.
    // docs/transport.md
    Models {
        /// The connection to read, by name from `~/.efference/config.toml`.
        connection: String,
        /// An extra route to read with the same credential, repeatable — a
        /// listing URL the provider does not declare.
        #[arg(long = "url")]
        urls: Vec<String>,
        /// Print whole bodies, including ones this build recognizes a shape for.
        #[arg(long)]
        raw: bool,
    },
    /// What past sessions picked, decayed: the recent menu
    /// and the omitted-branch default the history store would answer, read
    /// offline from `~/.efference/state.db`. No daemon, no network.
    // docs/transport.md
    History {
        /// The project root to scope to, defaulting to this process's. Scoped
        /// like creation scopes it: an explicit `--config` names the config
        /// key, else the checkout's `efference.toml` when present.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// An explicit `--config` path, resolved against the root like boot
        /// resolves it.
        #[arg(long)]
        config: Option<PathBuf>,
        /// A connection to narrow the recent menu to, omitting the default.
        #[arg(long)]
        connection: Option<String>,
    },
    /// Read a recorded session in the TUI, inert: the same transcript rendering as
    /// an attached TUI, over a session's `log.jsonl` instead of a live daemon. Nothing can be
    /// sent — no composer, no approvals, no loop verbs — and the log is read once,
    /// so a session that is still running shows as of the moment it was opened.
    /// q closes, ? for help, t for the theme picker.
    View {
        /// Which session to read: a session name (`.efference/<name>/log.jsonl`), a
        /// session directory, or a path to a `log.jsonl`. Omit to use `--name`;
        /// with neither, the available session names are listed.
        session: Option<String>,
        #[command(flatten)]
        read: ReadName,
        /// Syntax/UI theme — a built-in name or a `.tmTheme` path. Overrides
        /// `[tui] theme`.
        #[arg(long)]
        theme: Option<String>,
        /// Result-image rendering: "auto" queries the terminal, "kitty"/"iterm2"
        /// force a protocol detection missed (tmux), "off" disables. Overrides
        /// `[tui] images`.
        #[arg(long, value_enum)]
        images: Option<ImagesArg>,
    },
    /// Element gallery: launch the TUI on a fixed "one of each" transcript (rich
    /// markdown, a shell call, a colored diff, a collapsed long result, an error, a
    /// triaged result, a summary) — no daemon. A live design page for
    /// iterating on transcript rendering. q quits, ? for help, t for the theme picker.
    Gallery {
        /// Syntax/UI theme — a built-in name or a `.tmTheme` path. Overrides
        /// `[tui] theme`.
        #[arg(long)]
        theme: Option<String>,
        /// Result-image rendering: "auto" queries the terminal, "kitty"/"iterm2"
        /// force a protocol detection missed (tmux), "off" disables. Overrides
        /// `[tui] images`.
        #[arg(long, value_enum)]
        images: Option<ImagesArg>,
        /// Also play a scripted live stream into the gallery on a timer: a long
        /// file write whose JSON input arrives fragment by fragment, then a command
        /// whose output streams. Drives the render behaviours that only exist at
        /// animation rate.
        #[arg(long)]
        stream: bool,
        /// Bytes per streamed tool-input fragment (with `--stream`).
        #[arg(long, default_value_t = 24, requires = "stream")]
        chunk: usize,
        /// Milliseconds between streamed fragments (with `--stream`). Below the
        /// 30 ms paint tick, fragments fold into one frame.
        #[arg(long, default_value_t = 16, requires = "stream")]
        interval: u64,
        /// Time each paint and print the distribution (p50/p90/p99/max, and how
        /// many frames ran over the 30ms tick) on exit, as `[tui] frame-cost`
        /// does for every TUI.
        #[arg(long)]
        frame_cost: bool,
    },
    /// Replay a recorded session's triages and report **first-attempt** compliance — did the triage
    /// answer with a verdict, or reach for a work tool? The live triage retries, so a
    /// session's own record shows that a first attempt failed but can't score one ask
    /// against another. Each triage carries its own prefix at its own session depth, and
    /// replaying one is a single warm prefix read, so a whole session is an eval set.
    /// Sends real requests unless `--dry-run`.
    // docs/triage.md
    Triage(TriageArgs),
    /// Plant triage primers at controlled gaps against one recorded triage and replay
    /// the recorded triage per rung, reporting cache reads. Decides eviction vs
    /// write-visibility lag vs envelope keying: a back-to-back primer pair that
    /// chains proves truncated primers plant; a gapped pair that misses bounds the
    /// lifetime; a triage after a forced primer is the v1.1 verdict live. Sends real
    /// requests unless `--dry-run`. An experiment verb; remove it once the question
    /// it answers is settled.
    // docs/transport.md
    PrimerLadder(PrimerLadderArgs),
    /// Point it at a session JSONL log and print the wire request — the deterministic projection
    /// the assistant actually sees. Reconstructs the full envelope (`system` +
    /// `tools` + `messages`) exactly as a real turn POSTs it: `project(log)` with
    /// the trailing cache breakpoint placed and `tool_choice: auto`. The recorded
    /// boot identity supplies the historical model, provider, and placement; the
    /// recorded `tools[]` is read back; a log that never recorded one gets it rebuilt
    /// from the current code and cwd — flagged on stderr, since the recorded session
    /// may have run elsewhere.
    Wire {
        /// Path to a session JSONL log (e.g. `.efference/session-<millis>.jsonl`).
        path: PathBuf,
        /// Print only the projected `messages` array — `project(log)` with the
        /// trailing cache breakpoint placed — skipping the `system`/`tools[]`
        /// envelope entirely. A lean, config-independent view of what the model
        /// actually sees: it uses the provider/model identity recorded at boot,
        /// so it works even when a session's current config won't parse. Composes
        /// with `--format`.
        #[arg(long)]
        messages: bool,
        /// Rendering: `json` (true-to-bytes, pipeable to `jq`) or `yaml` (a lossy,
        /// read-only pseudo-YAML that unfolds escaped/multi-line strings for
        /// readability — not parseable back).
        #[arg(long, value_enum, default_value = "json")]
        format: WireFormat,
        /// Render in another dialect than the one the session was recorded in.
        /// `completions` translates a Messages-recorded session into Chat
        /// Completions (thinking as `reasoning_details`); the view sends nothing,
        /// and always renders JSON. Omit to render the recorded dialect.
        #[arg(long, value_enum)]
        dialect: Option<ViewDialect>,

        /// Config file to fold into the reconstructed preamble, same resolution as
        /// boot (defaults to probing `efference.toml`). The config file isn't recorded
        /// in the log, so — like `system`/`tools[]` — it's rebuilt from the current
        /// environment; pass `--config` to match the session that produced the log.
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
    },
    /// Run the spotter permission rung on a single move — a bash command, or a structured tool
    /// call with `--tool` — and print its verdict; no daemon, no session. A live API
    /// call to the spotter model, for iterating on the persona and config against real
    /// cases. With `--config`, the declared MCP servers are mounted first, so a
    /// `--tool` move is judged against the server's real tool definition (the same
    /// brief a session's spotter would see). Requires ANTHROPIC_API_KEY.
    // docs/permissions.md
    Spotter(SpotterArgs),
    /// Survey the recorded spotter verdicts across sessions — read every `Payload::Spot` back out
    /// of the session logs and print a health summary (stance distribution,
    /// fail-closed rate) followed by each verdict: the tool call it judged, its
    /// stance, and its one-line reason. Pure offline log read — no daemon, no
    /// network, config-independent (works even when a session's config won't
    /// parse). The companion to the live `spotter` tester: `spotter` runs one fresh
    /// check, `spotter-log` reviews the ones that already ran.
    // docs/permissions.md
    SpotterLog {
        /// A session dir (`.efference/<id>`) or a `log.jsonl` to survey. Omit to
        /// scan every `.efference/*/log.jsonl` under the cwd — the cross-session view.
        path: Option<PathBuf>,
        /// Show only verdicts with this stance (repeatable): `clear`, `clear-always`,
        /// `watch`, or `gate`. Omit for all. E.g. `--stance gate --stance watch` to
        /// read just the calls the spotter didn't wave through.
        #[arg(long = "stance", value_enum)]
        stances: Vec<StanceArg>,
        /// Print only the aggregate summary (stance distribution + fail-closed rate),
        /// skipping the per-verdict listing.
        #[arg(long, conflicts_with = "list")]
        summary: bool,
        /// Print only the per-verdict listing, skipping the aggregate summary.
        #[arg(long, conflicts_with = "summary")]
        list: bool,
        /// Hide the fail-closed fallback verdicts (`origin: Fallback` — a failed
        /// spotter call that defaulted to the gate), leaving only real model
        /// verdicts. Excludes them from both the summary and the listing.
        #[arg(long = "no-fallback")]
        no_fallback: bool,
    },
    /// Run the labeled spotter corpus against a model. Each case's config + call renders through the
    /// production `assess` path, given what the case's session log held before its
    /// verdict when the log is readable — what was said, the promotion probe, a bound
    /// leaf — and the brief of its tool, with the MCP servers
    /// of sessions under this project mounted. Verdicts score against the case's `expected`
    /// on the binary the system acts on (approve vs route-to-human). Reports
    /// per-class rates (human-miss = safety, false-gate = friction), the flip
    /// rate across repeated runs, latency percentiles, token totals, and — on
    /// OpenRouter — the provider-billed cost per generation id. Per-attempt
    /// detail lands in a results jsonl for offline cross-tabs.
    // docs/permissions.md
    SpotterEval(SpotterEvalArgs),
    /// Print the request a spotter rung was sent for a recorded call, as the rung's own
    /// renderer builds it: the session's log supplies the call, the promotion probe, a
    /// bound leaf and what was said before it; the session's
    /// config file, with later recorded edits taken back out, supplies the scope and
    /// grants; and a structured call's brief comes from the session's tools, mounted as
    /// the session mounted them. Name the call by the seq `spotter-log` lists for its
    /// verdict, or by its id; a call that was never spotted renders what the spotter
    /// would have been sent. No network unless the call's tool is a declared MCP
    /// server's, which is mounted (stored credentials only, never a sign-in), or
    /// `--json` is given. Notes on stderr say what may differ from the request the
    /// session sent.
    // docs/permissions.md
    SpotterRequest(SpotterRequestArgs),
}

// The flags of `dev spotter-request`; the variant that holds them carries the command's help
// text.
#[derive(clap::Args)]
pub(crate) struct SpotterRequestArgs {
    /// Which session to read: a session name (`.efference/<name>/log.jsonl`), a
    /// session directory, or a path to a `log.jsonl`. Omit to use `--name`.
    pub(crate) session: Option<String>,
    #[command(flatten)]
    pub(crate) read: ReadName,
    /// The spotter verdict's seq, as `spotter-log` lists it, or the handle of the call
    /// itself, spotted or not: its assistant entry's seq, or `seq.n` for that entry's
    /// n-th call.
    #[arg(long, required_unless_present = "call")]
    pub(crate) seq: Option<String>,
    /// The call's id, spotted or not. With `--seq`, the two must name the same call.
    #[arg(long)]
    pub(crate) call: Option<String>,
    /// Which rung's request to render. Defaults to the rung that judged the call: the
    /// decision rung when its verdict records axes, otherwise the prose rung. With
    /// `--json`, the session's own spotter decides, and naming the other rung is refused.
    #[arg(long, value_enum)]
    pub(crate) rung: Option<RungArg>,
    /// Print the prose rung's system prompt, its persona, before the turn.
    #[arg(long)]
    pub(crate) system: bool,
    /// Print the request body as the client for the session's spotter connection
    /// serializes it, instead of the text view. Resolves that connection from
    /// `~/.efference/config.toml` and asks its provider about the model, so it needs the
    /// connection's credentials.
    #[arg(long)]
    pub(crate) json: bool,
}

// The flags of `dev triage`; the variant that holds them carries the command's help text.
#[derive(clap::Args)]
pub(crate) struct TriageArgs {
    /// Path to a session dir or a `log.jsonl` whose triages to replay.
    pub(crate) path: PathBuf,
    /// Replay only the triage covering this result `seq` (as `Triage.for_result`
    /// names it, and as the listing prints it); repeat the flag for several. Omit to
    /// sweep every triage.
    #[arg(long)]
    pub(crate) seq: Vec<u64>,
    /// Draws per triage. A triage is sampled, not deterministic, so one draw per triage
    /// resolves only large differences; repeats buy a per-triage rate instead of a
    /// coin flip.
    #[arg(long, default_value = "3")]
    pub(crate) repeat: usize,
    /// Sample this many triages instead of sweeping all of them — for pricing a new
    /// model before committing to a full run. Sampled **evenly across the session**,
    /// never the first N: first-attempt failures climb with session depth, so a
    /// prefix of the triages measures the easy end and reads as a better rate than the
    /// model earns.
    #[arg(long)]
    pub(crate) limit: Option<usize>,
    /// Print the triage's trailing message instead of sending it — what makes an
    /// instruction change reviewable without spending a request.
    #[arg(long)]
    pub(crate) dry_run: bool,
    /// Model to replay against. Match the session's own model or the numbers
    /// aren't comparable to its record.
    #[arg(long, default_value = "claude-haiku-4-5")]
    pub(crate) model: String,
    /// Replay under the separate-model triage instead: this model on its own client,
    /// the same shape `--triage-model` boots. Every draw's verdict is
    /// compared line-for-line against the session's recorded verdict — what the
    /// agent chose to see — and the summary reports kept fractions, over- and
    /// under-retention, and a table of the triages that disagreed most.
    #[arg(long)]
    pub(crate) triage_model: Option<String>,
    /// With --triage-model: let it think.
    #[arg(long)]
    pub(crate) triage_thinking: bool,
    /// With --triage-model: `tool_choice: auto` instead of forcing the triage tool.
    #[arg(long)]
    pub(crate) triage_auto_choice: bool,
    /// With --triage-model: its reasoning level (implies thinking on).
    #[arg(long, value_name = "LEVEL")]
    pub(crate) triage_effort: Option<String>,
    /// Compare shared-lineage effort levels without changing the model or tools.
    /// Use `default` to omit effort with the same model capabilities as a named level.
    #[arg(long, value_name = "LEVEL", conflicts_with = "triage_model")]
    pub(crate) shared_effort: Option<String>,
    /// Compare the labeled synthetic idle cases with this candidate effort.
    /// The baseline is --shared-effort (usually `default`).
    #[arg(
        long,
        value_name = "LEVEL",
        requires = "shared_effort",
        conflicts_with = "triage_model"
    )]
    pub(crate) idle_candidate_effort: Option<String>,
    /// The history the triage request reads: `transcript` (the shared triage's view),
    /// `text` (the separate triage's text view: digest, triaged tail, current step),
    /// or an experiment arm: `tail:N` (the digest of the last N assistant turns before
    /// the triage's own), `intent` (none). Omitted, the view the replayed
    /// configuration reads live: `text` with --triage-model, else `transcript`.
    #[arg(long)]
    pub(crate) view: Option<String>,
    /// Write every compared result-draw as a JSON line to this file.
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
    /// Rows in the disagreement table.
    #[arg(long, default_value = "15")]
    pub(crate) table: usize,
}

// The flags of `dev primer-ladder`; the variant that holds them carries the command's help text.
#[derive(clap::Args)]
pub(crate) struct PrimerLadderArgs {
    /// Path to a session dir or a `log.jsonl` holding the triage to plant before.
    pub(crate) path: PathBuf,
    /// Plant before the triage covering this result `seq`. Omit for the deepest
    /// triage, where a cold prefix hurts most.
    #[arg(long)]
    pub(crate) seq: Option<u64>,
    /// Seconds between primer pairs per ladder rung, comma-separated.
    #[arg(long, default_value = "0,120,300,480")]
    pub(crate) gaps: String,
    /// Print the planned arms instead of sending them.
    #[arg(long)]
    pub(crate) dry_run: bool,
    /// Model to replay against. Match the session's own model or the numbers
    /// aren't comparable to its record.
    #[arg(long, default_value = "claude-haiku-4-5")]
    pub(crate) model: String,
    /// The separate triage model whose lineage the ladder plants: this model on
    /// its own client, the same shape `--triage-model` boots. Required — the
    /// ladder is meaningless on a shared triage.
    #[arg(long)]
    pub(crate) triage_model: Option<String>,
    /// With --triage-model: let it think.
    #[arg(long)]
    pub(crate) triage_thinking: bool,
    /// With --triage-model: `tool_choice: auto` on the replayed triage instead of
    /// forcing the triage tool.
    #[arg(long)]
    pub(crate) triage_auto_choice: bool,
    /// With --triage-model: its reasoning level (implies thinking on).
    #[arg(long, value_name = "LEVEL")]
    pub(crate) triage_effort: Option<String>,
    /// Write every report line as JSON to this file instead of stdout.
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
}

// The flags of `dev spotter`; the variant that holds them carries the command's help text.
#[derive(clap::Args)]
pub(crate) struct SpotterArgs {
    /// The scope (perimeter prose) to judge against. Omit to test the no-scope frame.
    #[arg(long)]
    pub(crate) scope: Option<String>,
    /// A note for the spotter about this project. Repeatable.
    // `--watch-for`, the older spelling, is still accepted and not listed in help.
    #[arg(long = "spotter-note", alias = "watch-for")]
    pub(crate) spotter_notes: Vec<String>,
    /// An allowed path grant, repeatable. Format: `<path>:rw` (read+write),
    /// `<path>:r` (read), or bare `<path>` (no access). E.g. `--path .:rw --path /refs:r`.
    #[arg(long = "path")]
    pub(crate) paths: Vec<String>,
    /// Spotter model, as `<connection>/<native model>`. Defaults to the configured spotter.
    #[arg(long)]
    pub(crate) model: Option<String>,
    /// Simulate a prefix-promotion probe riding this call: ask whether `<prefix>` is safe for all
    /// arguments. The verdict's `clear_always` stance is
    /// the safe-vote; the tester reports whether it would queue a promotion.
    // docs/permissions.md
    #[arg(long)]
    pub(crate) probe: Option<String>,
    /// Judge a structured tool call instead of a bash command. Takes the tool name
    /// in either spelling (`ferritin.get` or `ferritin__get`); its input comes from
    /// `--input`. The tool must be one the toolbox mounts — a built-in, or a tool
    /// from a server declared in `--config` — because the point is to judge it
    /// against its *real* definition.
    // docs/permissions.md
    #[arg(long, conflicts_with = "command")]
    pub(crate) tool: Option<String>,
    /// The `--tool` call's input, as JSON. Defaults to no arguments.
    #[arg(long, default_value = "{}")]
    pub(crate) input: String,
    /// A config file to take the mission from, and — the reason this exists — to
    /// mount the `[mcp.servers.<name>]` declarations from, so `--tool` can name a
    /// real connected tool. `--scope`/`--spotter-note`/`--path` override what it
    /// supplies. Omit to judge against the flags alone (no servers mounted).
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,
    /// Print the exact move block sent to the spotter before the verdict. Implied by
    /// `--tool`, where seeing the rendered brief is usually the point.
    #[arg(long = "show-move")]
    pub(crate) show_move: bool,
    /// Withhold the tool's brief, judging a `--tool` move on its name and input alone
    /// — what the spotter saw before briefs existed. The A/B: run the same call with
    /// and without, and the stance difference is the brief's effect.
    #[arg(long = "no-brief")]
    pub(crate) no_brief: bool,
    /// The bash command to judge. Multiple words are joined with spaces.
    #[arg(required_unless_present = "tool", num_args = 1..)]
    pub(crate) command: Vec<String>,
}

// The flags of `dev spotter-eval`; the variant that holds them carries the command's help text.
// Each bool is one command-line switch the operator sets independently of the others, so they
// are not alternatives of one state.
#[allow(clippy::struct_excessive_bools)]
#[derive(clap::Args)]
pub(crate) struct SpotterEvalArgs {
    /// A corpus file (jsonl), repeatable. A case missing an inline
    /// `expected` is joined against `--labels` by id; still-unlabeled cases
    /// are skipped and counted.
    #[arg(long = "cases", required = true)]
    pub(crate) cases: Vec<PathBuf>,
    /// A labels file (jsonl of `{id, expected, …}`) joined onto `--cases`
    /// rows that carry no inline label.
    #[arg(long)]
    pub(crate) labels: Option<PathBuf>,
    /// Spotter model under test, as `<connection>/<native model>`. Defaults to
    /// the spotter a session would get: `[defaults].spotter`, else the chain
    /// ending in the newest model in the main connection's cheap band.
    #[arg(long)]
    pub(crate) model: Option<String>,
    /// Attempts per case — the spotter is sampled behavior, so per-case
    /// consistency (the flip rate) is its own quality axis.
    #[arg(long, default_value_t = 3)]
    pub(crate) runs: usize,
    /// Results path. Defaults to `eval/spotter/results-<model>.jsonl`.
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
    /// Run only the first N cases (after `--filter`) — the smoke-test knob.
    #[arg(long)]
    pub(crate) limit: Option<usize>,
    /// Run only cases whose id contains this substring (e.g. `syn/` for the
    /// synthetics, a session id for one session's cases).
    #[arg(long)]
    pub(crate) filter: Option<String>,
    /// Concurrent in-flight requests.
    #[arg(long, default_value_t = 4)]
    pub(crate) concurrency: usize,
    /// A replacement persona file — the ablation knob: measure a persona
    /// edit against the same corpus instead of by ad-hoc replays.
    #[arg(long)]
    pub(crate) persona: Option<PathBuf>,
    /// Skip the per-generation cost lookup.
    #[arg(long = "no-cost")]
    pub(crate) no_cost: bool,
    /// Compare against what the spotter of the day actually said, rather
    /// than against a label, and print only the cases that diverge. Runs
    /// unlabeled cases too — every case carrying a `recorded` stance is in.
    /// Not a score: the recorded stance is a prior, so the dump is a review
    /// queue.
    // eval/spotter/README.md
    #[arg(long = "against-recorded")]
    pub(crate) against_recorded: bool,
    /// Write the decision rung's request state for every case to this path, labeled
    /// or not, and send nothing: one `{id, state, log_read, notes}` line per case, the
    /// state as the rung sends it for the case as it would be replayed — rebuilt from
    /// its session log when the log is readable, with what was said before the verdict
    /// as `conversation`. The starting point for experiments that vary what the state
    /// carries or what it is asked.
    // docs/permissions.md
    #[arg(long = "dump-states")]
    pub(crate) dump_states: Option<PathBuf>,
    /// Replay each case from its corpus line alone: its session log is not read, so the
    /// spotter is given no utterances, promotion probe or bound leaf — the
    /// comparison arm for what the session's record adds. Without it, a case whose log
    /// is readable is replayed with what the log held before its verdict.
    #[arg(long = "no-session-log")]
    pub(crate) no_session_log: bool,
}
