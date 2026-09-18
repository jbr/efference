//! The `efference dev <verb>` group: the instruments for developing efference
//! itself (docs/operating.md), compiled only under the `dev` cargo feature.
//!
//! None of these drive a session. They read a recorded log, render the wire a turn
//! would send, replay recorded gates, exercise the spotter rung directly, fold
//! `usage.jsonl` into a cache read rate, open a recorded log read-only in the TUI, or
//! mint a bootstrap link for the web bind —
//! the views and testers that make the harness's own behaviour inspectable. The feature
//! is in `default` because a gated feature nobody builds rots; a release build
//! passes `--no-default-features`.

use super::*;

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
        /// Print the program bearer token instead of a link.
        #[arg(long)]
        bearer: bool,
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
        /// Sweep every session under `.efference/` instead of one — the corpus view,
        /// which is where a rate is worth comparing against.
        #[arg(long, conflicts_with = "session")]
        all: bool,
    },
    /// Report what a session's turn endings bought
    /// (backlog/turn-persistence-in-the-preamble.md). A turn ends when the model
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
        /// Sweep every session under `.efference/` instead of one — the corpus view,
        /// which is where a rate is worth comparing against.
        #[arg(long, conflicts_with = "session")]
        all: bool,
    },
    /// Read a recorded session in the TUI, inert: the same transcript rendering as
    /// `tui`, over a session's `log.jsonl` instead of a live daemon. Nothing can be
    /// sent — no composer, no approvals, no loop verbs — and the log is read once,
    /// so a session that is still running shows as of the moment it was opened.
    /// q closes, ? for help, t for the theme picker.
    View {
        /// Which session to read: a session name (`.efference/<name>/log.jsonl`), a
        /// session directory, or a path to a `log.jsonl`. Omit to use `--name`;
        /// with neither, the available session names are listed.
        session: Option<String>,
        /// Syntax/UI theme — a built-in name or a `.tmTheme` path. Defaults to
        /// "Catppuccin Frappe".
        #[arg(long, env = "EFFERENCE_THEME")]
        theme: Option<String>,
        /// Result-image rendering: "auto" queries the terminal, "kitty"/"iterm2"
        /// force a protocol detection missed (tmux), "off" disables.
        #[arg(long, env = "EFFERENCE_IMAGES", value_enum, default_value_t)]
        images: ImagesArg,
    },
    /// Element gallery: launch the TUI on a fixed "one of each" transcript (rich
    /// markdown, a shell call, a colored diff, a collapsed long result, an error, a
    /// distilled result, a consolidation mark) — no daemon. A live design page for
    /// iterating on transcript rendering. q quits, ? for help, t for the theme picker.
    Gallery {
        /// Syntax/UI theme — a built-in name or a `.tmTheme` path. Defaults to
        /// "Catppuccin Frappe".
        #[arg(long, env = "EFFERENCE_THEME")]
        theme: Option<String>,
        /// Result-image rendering: "auto" queries the terminal, "kitty"/"iterm2"
        /// force a protocol detection missed (tmux), "off" disables.
        #[arg(long, env = "EFFERENCE_IMAGES", value_enum, default_value_t)]
        images: ImagesArg,
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
        /// many frames ran over the 30ms tick) on exit. Development
        /// instrumentation for the streaming render.
        #[arg(long)]
        frame_cost: bool,
    },
    /// Replay a recorded session's fork-gate peeks (docs/redaction-fork.md) and report **first-attempt** compliance — did the peek
    /// answer with a verdict, or reach for a work tool? The live gate retries, so a
    /// session's own record shows that a first attempt failed but can't score one ask
    /// against another. Each gate carries its own prefix at its own session depth, and
    /// replaying one is a single warm prefix read, so a whole session is an eval set.
    /// Sends real requests unless `--dry-run`.
    Peek {
        /// Path to a session dir or a `log.jsonl` whose gates to replay.
        path: PathBuf,
        /// Replay only the gate triaging this result `seq` (as `Triage.for_result`
        /// names it, and as the listing prints it). Omit to sweep every gate.
        #[arg(long)]
        seq: Option<u64>,
        /// Draws per gate. A peek is sampled, not deterministic, so one draw per gate
        /// resolves only large differences; repeats buy a per-gate rate instead of a
        /// coin flip.
        #[arg(long, default_value = "3")]
        repeat: usize,
        /// Sample this many gates instead of sweeping all of them — for pricing a new
        /// model before committing to a full run. Sampled **evenly across the session**,
        /// never the first N: first-attempt failures climb with session depth, so a
        /// prefix of the gates measures the easy end and reads as a better rate than the
        /// model earns.
        #[arg(long)]
        limit: Option<usize>,
        /// Print the peek's trailing message instead of sending it — what makes an
        /// instruction change reviewable without spending a request.
        #[arg(long)]
        dry_run: bool,
        /// Model to replay against. Match the session's own model or the numbers
        /// aren't comparable to its record.
        #[arg(long, default_value = "claude-haiku-4-5")]
        model: String,
        /// Replay under the separate-model peek instead: this model on its own client,
        /// the same shape `--triage-model` boots. Every draw's verdict is
        /// compared line-for-line against the session's recorded verdict — what the
        /// agent chose to see — and the summary reports kept fractions, over- and
        /// under-retention, and a table of the gates that disagreed most.
        #[arg(long)]
        triage_model: Option<String>,
        /// With --triage-model: let it think.
        #[arg(long)]
        triage_thinking: bool,
        /// With --triage-model: `tool_choice: auto` instead of forcing the peek tool.
        #[arg(long)]
        triage_auto_choice: bool,
        /// With --triage-model: its reasoning level (implies thinking on).
        #[arg(long, value_name = "LEVEL")]
        triage_effort: Option<String>,
        /// Write every compared result-draw as a JSON line to this file.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Rows in the disagreement table.
        #[arg(long, default_value = "15")]
        table: usize,
    },
    /// Point it at a session JSONL log and print the wire request — the deterministic projection
    /// the assistant actually sees. Reconstructs the full envelope (`system` +
    /// `tools` + `messages`) exactly as a real turn POSTs it: `project(log)` with
    /// the trailing cache breakpoint placed and `tool_choice: auto`. The `system`
    /// block and `tools[]` aren't stored in the log, so they're rebuilt from the
    /// current code and cwd — flagged on stderr, since the recorded session may
    /// have run elsewhere.
    Wire {
        /// Path to a session JSONL log (e.g. `.efference/session-<millis>.jsonl`).
        path: PathBuf,
        /// Print only the projected `messages` array — `project(log)` with the
        /// trailing cache breakpoint placed — skipping the `system`/`tools[]`
        /// envelope entirely. A lean, config-independent view of what the model
        /// actually sees: unlike the full envelope, this needs no config, model
        /// id, or rebuilt tool list, so it works even when a session's config
        /// won't parse. Composes with `--format`.
        #[arg(long)]
        messages: bool,
        /// Rendering: `json` (true-to-bytes, pipeable to `jq`) or `yaml` (a lossy,
        /// read-only pseudo-YAML that unfolds escaped/multi-line strings for
        /// readability — not parseable back).
        #[arg(long, value_enum, default_value = "json")]
        format: WireFormat,
        /// Model to stamp into the envelope, selected like `--model` at creation.
        /// Only cosmetic here (the request isn't sent).
        #[arg(long, default_value = "claude-haiku-4-5")]
        model: String,
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
        /// Spotter model, as `<connection>/<native model>` or a native id on the
        /// host's default connection. Defaults to the configured spotter.
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
        /// Spotter model under test.
        #[arg(long, default_value = "claude-haiku-4-5")]
        model: String,
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
    },
}

/// Run one `dev` verb.
/// `name` is the global `--name`; `client_target` resolves the session on demand,
/// so the offline verbs never trigger discovery.
pub(crate) async fn run<F: Future<Output = Result<ClientTarget, CliError>>>(
    command: DevCommand,
    name: Option<String>,
    client_target: impl FnOnce() -> F,
) -> CliResult {
    match command {
        DevCommand::SessionForm { embedded } => {
            if embedded {
                efference::tui::run_session_form_prototype().await?;
            } else {
                efference::tui::session_form::run()?;
            }
            Ok(())
        }
        DevCommand::Open { print, bearer } => open(client_target().await?, print, bearer).await,
        DevCommand::Cache { session, all } => cache_report::cache(session, name, all),
        DevCommand::Stops { session, all } => stop_report::stops_report(session, name, all),
        DevCommand::View {
            session,
            theme,
            images,
        } => view(session, name, theme, images.into()).await,
        DevCommand::Gallery {
            theme,
            images,
            stream,
            chunk,
            interval,
            frame_cost,
        } => gallery(theme, images.into(), stream, chunk, interval, frame_cost).await,
        DevCommand::Wire {
            path,
            messages,
            format,
            model,
            config,
        } => wire(path, messages, format, &model, config).await,
        DevCommand::Peek {
            path,
            seq,
            repeat,
            limit,
            dry_run,
            model,
            triage_model,
            triage_thinking,
            triage_auto_choice,
            triage_effort,
            out,
            table,
        } => {
            let triage = triage_model.map(|model| efference::distill::TriageModel {
                model,
                thinking: triage_thinking,
                auto_choice: triage_auto_choice,
                effort: triage_effort,
            });
            peek_replay::peek(
                path, seq, repeat, limit, dry_run, &model, triage, out, table,
            )
            .await
        }
        DevCommand::Spotter {
            scope,
            watch_for,
            paths,
            model,
            probe,
            tool,
            input,
            config,
            show_move,
            no_brief,
            command,
        } => {
            serve::spotter_check(
                scope,
                watch_for,
                paths,
                model.as_deref(),
                probe,
                tool,
                input,
                config,
                show_move,
                no_brief,
                command.join(" "),
            )
            .await
        }
        DevCommand::SpotterLog {
            path,
            stances,
            summary,
            list,
            no_fallback,
        } => spot_report::spotter_log(path, stances, summary, list, no_fallback),
        DevCommand::SpotterEval {
            cases,
            labels,
            model,
            runs,
            out,
            limit,
            filter,
            concurrency,
            persona,
            no_cost,
        } => {
            spotter_eval::spotter_eval(
                cases,
                labels,
                model,
                runs,
                out,
                limit,
                filter,
                concurrency,
                persona,
                no_cost,
            )
            .await
        }
    }
}

/// A spotter [`Stance`](efference::log::Stance) as a `--stance` filter value for the
/// `spotter-log` dev survey. Mirrors the log's four verdicts.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum StanceArg {
    Clear,
    ClearAlways,
    Watch,
    Gate,
}

impl StanceArg {
    pub(crate) fn matches(self, stance: efference::log::Stance) -> bool {
        use efference::log::Stance;
        matches!(
            (self, stance),
            (StanceArg::Clear, Stance::Clear)
                | (StanceArg::ClearAlways, Stance::ClearAlways)
                | (StanceArg::Watch, Stance::Watch)
                | (StanceArg::Gate, Stance::Gate)
        )
    }
}

/// Output format for the `wire` dev view.
#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum WireFormat {
    /// Pretty-printed JSON — true to the bytes POSTed; pipeable to `jq`.
    Json,
    /// Lossy, read-only pseudo-YAML that unfolds escaped/multi-line strings.
    Yaml,
}

/// Parse a `--path` grant spec for the spotter tester into a `[paths]` map entry: `<path>:rw`
/// (read+write), `<path>:r` (read), or a bare `<path>` (no access — the honest default). The
/// perm suffix is matched by the letters it contains.
pub(crate) fn parse_path_grant(spec: &str) -> (String, efference::config::Perms) {
    match spec.rsplit_once(':') {
        Some((path, perms)) => (
            path.into(),
            efference::config::Perms {
                read: perms.contains('r'),
                write: perms.contains('w'),
            },
        ),
        None => (
            spec.into(),
            efference::config::Perms {
                read: false,
                write: false,
            },
        ),
    }
}

/// Reconstruct and print the wire request for a recorded session JSONL — the
/// deterministic projection the assistant sees (docs/README.md, docs/log-and-projection.md). No daemon, no
/// socket, no network: load the log, fold it to `messages`, place the trailing
/// cache breakpoint, and rebuild the `system`/`tools` envelope exactly as a real
/// turn would (`tool_choice: auto`, docs/transport.md). `system` and `tools[]` are not in
/// the log; they're rebuilt from the current code + cwd, so the note on stderr
/// warns when the session may have run in a different environment.
pub(crate) async fn wire(
    path: PathBuf,
    messages_only: bool,
    format: WireFormat,
    model: &str,
    config: Option<PathBuf>,
) -> CliResult {
    let entries = efference::persist::load(&path)?;
    // The provider behind `--model`, when the host config resolves it; with none,
    // the view's own defaults.
    let (model, provider) = match crate::inference::main_endpoint(model) {
        Ok((model, endpoint)) => (model, Some(endpoint.provider)),
        Err(_) => (model.to_string(), None),
    };
    let model = model.as_str();
    let placement = provider
        .map(|p| p.image_placement(model))
        .unwrap_or_default();
    // The same turns a live request encodes, trailing breakpoint included; the
    // dialect decides the wire they render on (docs/transport.md).
    let mut turns = efference::project::project_semantic(&entries);
    // The dialect the log was recorded in; with no provider declared it is the view's
    // dialect, and with one declared the two must agree — the other encoder would
    // drop every assistant turn (docs/transport.md).
    let recorded = efference::project::recorded_dialect(&turns)?;
    let dialect = provider
        .map(|p| p.dialect())
        .or(recorded)
        .unwrap_or(efference::api::WireDialect::Messages);
    efference::project::check_dialect(&turns, dialect)?;
    turns.push(efference::project::WireTurn::Breakpoint);
    let messages = efference::project::encode_messages(
        &turns,
        placement,
        provider
            .map(|p| p.breakpoint_placement())
            .unwrap_or_default(),
    );
    // Responses caching is model-specific; the offline view assumes the explicit
    // policy every model in the compiled catalog reports.
    let cache = efference::api::PromptCachePolicy::Explicit;

    // `--messages` prints just the projected array and returns before touching
    // config/system/tools at all — a lean, config-independent view of what the
    // model sees, so it still works when a session's config won't parse.
    if messages_only {
        eprintln!(
            "note: {} log entries → {} wire messages.",
            entries.len(),
            messages.len()
        );
        let rendered = match (dialect, format) {
            (efference::api::WireDialect::Responses, _) => {
                serde_json::to_string_pretty(&efference::api::encode_responses(&turns, "", cache))?
            }
            (_, WireFormat::Json) => efference::api::render_messages_pretty(&messages)?,
            (_, WireFormat::Yaml) => efference::api::render_messages_yaml(&messages)?,
        };
        println!("{rendered}");
        return Ok(());
    }

    // The `system` a session sent is recorded in the log as the boot preamble snapshot
    // (docs/context.md) — prefer it (the last one wins on a multi-boot log) so
    // the view shows exactly what the model saw, not a re-resolve of files that may have
    // drifted on disk since. Only logs that predate the snapshot fall back to a rebuild.
    let recorded_system = entries.iter().rev().find_map(|e| match &e.payload {
        efference::log::Payload::Preamble { system, .. } => Some(system.clone()),
        _ => None,
    });
    let cwd = std::env::current_dir()?;
    let system = if let Some(system) = recorded_system {
        eprintln!(
            "note: {} log entries → {} wire messages. `system` is the recorded boot \
             preamble; `tools[]` is rebuilt from the current code and cwd.",
            entries.len(),
            messages.len()
        );
        system
    } else {
        // No snapshot (a pre-snapshot log) — rebuild the envelope the same way boot does
        // (current cwd seeds both the env block and the MCP servers' session roots; the role
        // config folds its brief into the preamble).
        let loaded = match efference::config::load(config.as_deref()) {
            Ok(l) => l,
            Err(e) => {
                e.eprint();
                return Err("could not load role config".into());
            }
        };
        let role_section = loaded.role.preamble_section();
        // Best-effort for the dev view: if the [context] files don't resolve from the current cwd
        // (e.g. inspecting a session recorded elsewhere), note it and omit the block rather than fail.
        let context_block = match efference::context::resolve(&loaded.role.context, &cwd) {
            Ok(resolved) => resolved.render(),
            Err(e) => {
                eprintln!("note: [context] not resolved ({e}); omitting from the rebuilt envelope");
                None
            }
        };
        eprintln!(
            "note: {} log entries → {} wire messages. This log predates the boot-preamble \
             snapshot: `system`/`tools[]` are rebuilt from the current code and cwd, not the \
             recorded session.",
            entries.len(),
            messages.len()
        );
        // No unmounted-servers section: this offline view mounts no connected
        // providers at all (see the note below), so it has nothing to report
        // about which of them failed.
        // The session's id is its dir's name — the log sits at
        // `.efference/<id>/log.jsonl`, so the rebuilt identity line can name the
        // session this log belongs to (docs/context.md).
        let session_id = path
            .parent()
            .and_then(|dir| dir.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        // No creator: `created_by` is host control state, never logged
        // (docs/tools.md), so an offline rebuild from a log cannot recover it —
        // a child's rebuilt preamble is missing the creator sentence its live
        // one carried. Sessions logged since the boot-preamble snapshot read
        // back the recorded `system` instead and are unaffected.
        efference::env::system_prompt(
            model,
            &session_id,
            None,
            context_block.as_deref(),
            role_section.as_deref(),
            None,
        )
    };
    // The full tools[] a real turn sends — world tools + the harness tools
    // (`consolidate`, the docs/supervisor.md and docs/inbox.md tools) — so the dev view matches.
    // (No scratch dir: the wire view renders, it never executes.) One known gap:
    // config-declared connected providers (docs/tools.md) are not mounted here —
    // mounting spawns processes / opens connections, which an offline view must
    // not — so a session with `[mcp.servers.*]` has more tools[] than this shows.
    let toolbox = Toolbox::bash_and_mcps(cwd, None)?;
    let tools = efference::agent::tool_definitions(&toolbox);

    let choice = efference::api::ToolChoice::auto();
    let rendered = match (dialect, format) {
        (efference::api::WireDialect::Responses, _) => {
            efference::api::render_responses_request_pretty(
                model, &system, &tools, choice, &turns, cache,
            )?
        }
        (_, WireFormat::Json) => {
            efference::api::render_request_pretty(model, &system, &tools, choice, &messages)?
        }
        (_, WireFormat::Yaml) => {
            efference::api::render_request_yaml(model, &system, &tools, choice, &messages)?
        }
    };
    println!("{rendered}");
    Ok(())
}

/// Launch the TUI on the fixed element gallery — no daemon, no socket. With
/// `stream`, a scripted emitter plays live tool-call events over it on a timer.
pub(crate) async fn gallery(
    theme: Option<String>,
    images: efference::tui::ImageMode,
    stream: bool,
    chunk: usize,
    interval: u64,
    frame_cost: bool,
) -> CliResult {
    let script = stream.then(|| efference::tui::StreamScript {
        chunk,
        interval: std::time::Duration::from_millis(interval),
    });
    efference::tui::run_gallery(theme, images, script, frame_cost).await?;
    Ok(())
}

#[cfg(test)]
mod test;

/// Mint and open a fresh bootstrap link against a running daemon's TCP bind
/// (docs/web-bind.md); `--bearer` prints the program token instead. Both come over
/// the Unix socket only.
async fn open(target: ClientTarget, print: bool, bearer: bool) -> CliResult {
    // The credential routes live at the socket root, whatever the session prefix.
    let client = ControlClient::new(target.socket);
    if bearer {
        println!("{}", client.auth_bearer().await?);
        return Ok(());
    }
    let url = client.auth_link().await?;
    println!("{url}");
    if !print && webbrowser::open(&url).is_err() {
        eprintln!("[could not open a browser — use the printed link]");
    }
    Ok(())
}

/// Read a recorded session in the TUI, inert (docs/tui.md): resolve the session to
/// a `log.jsonl`, load it with the same `persist::load` a live session's entries come
/// through, and hand the entries to the read-only TUI. No socket, no network, no
/// writes — and no config, so it works on a session whose config no longer parses or
/// that ran in another directory.
async fn view(
    session: Option<String>,
    name: Option<String>,
    theme: Option<String>,
    images: efference::tui::ImageMode,
) -> CliResult {
    let Some(target) = session.or(name) else {
        return Err(format!(
            "which session? pass a session name, a session dir, or a log.jsonl path.{}",
            session_listing()
        )
        .into());
    };
    let (path, label) = resolve_session_log(&target)?;
    let entries = efference::persist::load(&path)?;
    if entries.is_empty() {
        return Err(format!("{} holds no entries", path.display()).into());
    }
    efference::tui::run_view(entries, label, theme, images).await?;
    Ok(())
}
