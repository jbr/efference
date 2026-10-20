//! Flag values the `dev` verbs parse: the `--stance`, `--format` and `--view-dialect`
//! choices and the spotter tester's `--path` grant spec.

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

/// A dialect the `wire` view can translate a recorded session into.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum ViewDialect {
    /// Chat Completions (backlog/inbox/completions-dialect.md).
    Completions,
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
