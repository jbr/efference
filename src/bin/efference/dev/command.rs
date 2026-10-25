//! The clap tree of `efference dev` (docs/operating.md): one variant per verb, and the
//! `--name` flag the reader verbs share.

use super::*;

/// The recorded session a reader verb opens, as its positional names it: the flag
/// is the same answer spelled `--name`, for the id of a session this project ran.
#[derive(clap::Args)]
pub(crate) struct ReadName {
    /// The session to read, by id; what the positional would name.
    #[arg(long)]
    pub(super) name: Option<String>,
}

#[derive(Subcommand)]
pub(crate) enum DevCommand {
    /// Fixed-data new-session form; no host, credentials, or session creation.
    SessionForm {
        /// Open inside the offline gallery's Sessions screen instead of inline.
        #[arg(long)]
        embedded: bool,
    },
    /// Mint a fresh bootstrap link against a running daemon's TCP bind and open it —
    /// the re-auth path when at the host, and the answer to lost scrollback
    /// (docs/web-bind.md). The credential routes answer only on the Unix socket:
    /// possession of the host machine is the enrollment authority. Lives under `dev`
    /// because it mints credentials for the HTTP surface: it is not a session-control
    /// tool and must not be reachable through `efference mcp`.
    Open {
        /// Print the link without launching a browser.
        #[arg(long)]
        print: bool,
    },
    /// Report a session's prompt-cache read rate from its `usage.jsonl`
    /// (docs/log-and-projection.md) — the empirical half of the prefix oracle the
    /// byte pins cover statically. Offline: no daemon, no network. Reads warm/cold
    /// per `RequestOrigin`, separates the cold reads that need no explanation
    /// (session boot, a model change, an idle gap past the cache TTL) from the ones
    /// that do, and reports the longest run of the latter — a long run means the
    /// provider stopped matching a prefix the pins say did not change.
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
    /// Report what a session's turn endings bought
    /// (docs/operating.md). A turn ends when the model
    /// emits no tool call, which is the harness's only way to put a question *and*
    /// its only way to say the work is done. This counts which it turned out to be,
    /// by what the next turn carried: a **barren** ending asked nothing and was
    /// answered with nothing, so it bought a round trip and no information.
    /// Offline: no daemon, no network.
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
    /// Print a session's trajectory digest (docs/digest.md): one line per tool call —
    /// its tool, paths, intent and triaged note — plus verbatim operator turns and
    /// the agent's turn-ending replies. A deterministic fold over the log; offline, no
    /// daemon, no model.
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
    /// Write a compaction summary for a session (docs/summary.md): the session's own
    /// model reads its digest and writes the summary a compaction would splice in.
    /// Prints it; appends nothing. One live request on the session's connection.
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
    /// connection's credential (docs/transport.md). Resolves the connection as a
    /// session would, GETs the listing route, and prints the status and entries
    /// as they arrived — the other OpenAI arm's route beside it, so one run says
    /// whether each answers the other's credential. Live API calls; no daemon,
    /// no session.
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
    /// What past sessions picked, decayed (docs/transport.md): the recent menu
    /// and the omitted-branch default the history store would answer, read
    /// offline from `~/.efference/state.db`. No daemon, no network.
    History {
        /// The project root to scope to, defaulting to this process's. Scoped
        /// like creation scopes it: an explicit `--config` names the config
        /// key, else the checkout's `efference.toml` when present.
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
        /// An explicit `--config` path, resolved against the root like boot
        /// resolves it.
        #[arg(long)]
        config: Option<std::path::PathBuf>,
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
    /// Replay a recorded session's triages (docs/triage.md) and report **first-attempt** compliance — did the triage
    /// answer with a verdict, or reach for a work tool? The live triage retries, so a
    /// session's own record shows that a first attempt failed but can't score one ask
    /// against another. Each triage carries its own prefix at its own session depth, and
    /// replaying one is a single warm prefix read, so a whole session is an eval set.
    /// Sends real requests unless `--dry-run`.
    Triage {
        /// Path to a session dir or a `log.jsonl` whose triages to replay.
        path: PathBuf,
        /// Replay only the triage covering this result `seq` (as `Triage.for_result`
        /// names it, and as the listing prints it); repeat the flag for several. Omit to
        /// sweep every triage.
        #[arg(long)]
        seq: Vec<u64>,
        /// Draws per triage. A triage is sampled, not deterministic, so one draw per triage
        /// resolves only large differences; repeats buy a per-triage rate instead of a
        /// coin flip.
        #[arg(long, default_value = "3")]
        repeat: usize,
        /// Sample this many triages instead of sweeping all of them — for pricing a new
        /// model before committing to a full run. Sampled **evenly across the session**,
        /// never the first N: first-attempt failures climb with session depth, so a
        /// prefix of the triages measures the easy end and reads as a better rate than the
        /// model earns.
        #[arg(long)]
        limit: Option<usize>,
        /// Print the triage's trailing message instead of sending it — what makes an
        /// instruction change reviewable without spending a request.
        #[arg(long)]
        dry_run: bool,
        /// Model to replay against. Match the session's own model or the numbers
        /// aren't comparable to its record.
        #[arg(long, default_value = "claude-haiku-4-5")]
        model: String,
        /// Replay under the separate-model triage instead: this model on its own client,
        /// the same shape `--triage-model` boots. Every draw's verdict is
        /// compared line-for-line against the session's recorded verdict — what the
        /// agent chose to see — and the summary reports kept fractions, over- and
        /// under-retention, and a table of the triages that disagreed most.
        #[arg(long)]
        triage_model: Option<String>,
        /// With --triage-model: let it think.
        #[arg(long)]
        triage_thinking: bool,
        /// With --triage-model: `tool_choice: auto` instead of forcing the triage tool.
        #[arg(long)]
        triage_auto_choice: bool,
        /// With --triage-model: its reasoning level (implies thinking on).
        #[arg(long, value_name = "LEVEL")]
        triage_effort: Option<String>,
        /// The history the triage request reads: `transcript` (the shared triage's view),
        /// `text` (the separate triage's text view: digest, triaged tail, current step),
        /// or an experiment arm: `tail:N` (the digest of the last N assistant turns before
        /// the triage's own), `intent` (none). Omitted, the view the replayed
        /// configuration reads live: `text` with --triage-model, else `transcript`.
        #[arg(long)]
        view: Option<String>,
        /// Write every compared result-draw as a JSON line to this file.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Rows in the disagreement table.
        #[arg(long, default_value = "15")]
        table: usize,
    },
    /// Plant triage primers at controlled gaps against one recorded triage and replay
    /// the recorded triage per rung, reporting cache reads
    /// (docs/transport.md). Decides eviction vs
    /// write-visibility lag vs envelope keying: a back-to-back primer pair that
    /// chains proves truncated primers plant; a gapped pair that misses bounds the
    /// lifetime; a triage after a forced primer is the v1.1 verdict live. Sends real
    /// requests unless `--dry-run`. An experiment verb; remove it once the question
    /// it answers is settled.
    PrimerLadder {
        /// Path to a session dir or a `log.jsonl` holding the triage to plant before.
        path: PathBuf,
        /// Plant before the triage covering this result `seq`. Omit for the deepest
        /// triage, where a cold prefix hurts most.
        #[arg(long)]
        seq: Option<u64>,
        /// Seconds between primer pairs per ladder rung, comma-separated.
        #[arg(long, default_value = "0,120,300,480")]
        gaps: String,
        /// Print the planned arms instead of sending them.
        #[arg(long)]
        dry_run: bool,
        /// Model to replay against. Match the session's own model or the numbers
        /// aren't comparable to its record.
        #[arg(long, default_value = "claude-haiku-4-5")]
        model: String,
        /// The separate triage model whose lineage the ladder plants: this model on
        /// its own client, the same shape `--triage-model` boots. Required — the
        /// ladder is meaningless on a shared triage.
        #[arg(long)]
        triage_model: Option<String>,
        /// With --triage-model: let it think.
        #[arg(long)]
        triage_thinking: bool,
        /// With --triage-model: `tool_choice: auto` on the replayed triage instead of
        /// forcing the triage tool.
        #[arg(long)]
        triage_auto_choice: bool,
        /// With --triage-model: its reasoning level (implies thinking on).
        #[arg(long, value_name = "LEVEL")]
        triage_effort: Option<String>,
        /// Write every report line as JSON to this file instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
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
    /// Run the spotter permission rung (docs/permissions.md) on a single move — a bash command, or a structured tool
    /// call with `--tool` — and print its verdict; no daemon, no session. A live API
    /// call to the spotter model, for iterating on the persona and config against real
    /// cases. With `--config`, the declared MCP servers are mounted first, so a
    /// `--tool` move is judged against the server's real tool definition (the same
    /// brief a session's spotter would see). Requires ANTHROPIC_API_KEY.
    Spotter {
        /// The scope (perimeter prose) to judge against. Omit to test the no-scope frame.
        #[arg(long)]
        scope: Option<String>,
        /// A spotter-only "thing to watch for". Repeatable.
        #[arg(long = "watch-for")]
        watch_for: Vec<String>,
        /// An allowed path grant, repeatable. Format: `<path>:rw` (read+write),
        /// `<path>:r` (read), or bare `<path>` (no access). E.g. `--path .:rw --path /refs:r`.
        #[arg(long = "path")]
        paths: Vec<String>,
        /// Spotter model, as `<connection>/<native model>`. Defaults to the configured spotter.
        #[arg(long)]
        model: Option<String>,
        /// Simulate a prefix-promotion probe riding this call: ask whether `<prefix>` is safe for all
        /// arguments (docs/permissions.md). The verdict's `clear_always` stance is
        /// the safe-vote; the tester reports whether it would queue a promotion.
        #[arg(long)]
        probe: Option<String>,
        /// Judge a structured tool call instead of a bash command. Takes the tool name
        /// in either spelling (`ferritin.get` or `ferritin__get`); its input comes from
        /// `--input`. The tool must be one the toolbox mounts — a built-in, or a tool
        /// from a server declared in `--config` — because the point is to judge it
        /// against its *real* definition (docs/permissions.md).
        #[arg(long, conflicts_with = "command")]
        tool: Option<String>,
        /// The `--tool` call's input, as JSON. Defaults to no arguments.
        #[arg(long, default_value = "{}")]
        input: String,
        /// A config file to take the mission from, and — the reason this exists — to
        /// mount the `[mcp.servers.<name>]` declarations from, so `--tool` can name a
        /// real connected tool. `--scope`/`--watch-for`/`--path` override what it
        /// supplies. Omit to judge against the flags alone (no servers mounted).
        #[arg(long)]
        config: Option<PathBuf>,
        /// Print the exact move block sent to the spotter before the verdict. Implied by
        /// `--tool`, where seeing the rendered brief is usually the point.
        #[arg(long = "show-move")]
        show_move: bool,
        /// Withhold the tool's brief, judging a `--tool` move on its name and input alone
        /// — what the spotter saw before briefs existed. The A/B: run the same call with
        /// and without, and the stance difference is the brief's effect.
        #[arg(long = "no-brief")]
        no_brief: bool,
        /// The bash command to judge. Multiple words are joined with spaces.
        #[arg(required_unless_present = "tool", num_args = 1..)]
        command: Vec<String>,
    },
    /// Survey the recorded spotter verdicts (docs/permissions.md) across sessions — read every `Payload::Spot` back out
    /// of the session logs and print a health summary (stance distribution,
    /// fail-closed rate) followed by each verdict: the tool call it judged, its
    /// stance, and its one-line reason. Pure offline log read — no daemon, no
    /// network, config-independent (works even when a session's config won't
    /// parse). The companion to the live `spotter` tester: `spotter` runs one fresh
    /// check, `spotter-log` reviews the ones that already ran.
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
    /// Run the labeled spotter corpus against a model
    /// (docs/permissions.md). Each case's config + call renders through the
    /// production `assess` path; verdicts score against the case's `expected`
    /// on the binary the system acts on (approve vs route-to-human). Reports
    /// per-class rates (human-miss = safety, false-gate = friction), the flip
    /// rate across repeated runs, latency percentiles, token totals, and — on
    /// OpenRouter — the provider-billed cost per generation id. Per-attempt
    /// detail lands in a results jsonl for offline cross-tabs.
    SpotterEval {
        /// A corpus file (jsonl), repeatable. A case missing an inline
        /// `expected` is joined against `--labels` by id; still-unlabeled cases
        /// are skipped and counted.
        #[arg(long = "cases", required = true)]
        cases: Vec<PathBuf>,
        /// A labels file (jsonl of `{id, expected, …}`) joined onto `--cases`
        /// rows that carry no inline label.
        #[arg(long)]
        labels: Option<PathBuf>,
        /// Spotter model under test, as `<connection>/<native model>`. Defaults to
        /// the spotter a session would get: `[defaults].spotter`, else the chain
        /// ending in the newest model in the main connection's cheap band.
        #[arg(long)]
        model: Option<String>,
        /// Attempts per case — the spotter is sampled behavior, so per-case
        /// consistency (the flip rate) is its own quality axis.
        #[arg(long, default_value_t = 3)]
        runs: usize,
        /// Results path. Defaults to `eval/spotter/results-<model>.jsonl`.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Run only the first N cases (after `--filter`) — the smoke-test knob.
        #[arg(long)]
        limit: Option<usize>,
        /// Run only cases whose id contains this substring (e.g. `syn/` for the
        /// synthetics, a session id for one session's cases).
        #[arg(long)]
        filter: Option<String>,
        /// Concurrent in-flight requests.
        #[arg(long, default_value_t = 4)]
        concurrency: usize,
        /// A replacement persona file — the ablation knob: measure a persona
        /// edit against the same corpus instead of by ad-hoc replays.
        #[arg(long)]
        persona: Option<PathBuf>,
        /// Skip the per-generation cost lookup.
        #[arg(long = "no-cost")]
        no_cost: bool,
        /// Compare against what the spotter of the day actually said, rather
        /// than against a label, and print only the cases that diverge. Runs
        /// unlabeled cases too — every case carrying a `recorded` stance is in.
        /// Not a score: the recorded stance is a prior, so the dump is a review
        /// queue (eval/spotter/README.md).
        #[arg(long = "against-recorded")]
        against_recorded: bool,
        /// Write the decision rung's request state for every case to this path,
        /// labeled or not, and send nothing — the starting point for experiments
        /// that vary what else the state carries (docs/permissions.md).
        #[arg(long = "dump-states")]
        dump_states: Option<PathBuf>,
    },
}
