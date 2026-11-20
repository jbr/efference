//! What each corpus case's session held, read once before the runs (docs/permissions.md):
//! the circumstances `dev spotter-request` rebuilds from a log, and the MCP servers whose
//! tools the cases name, mounted once each.

use super::*;
use crate::spotter_request::{builtin_toolbox, mount_servers};
use efference::config::McpServerEntry;
use efference::curation::spotter::recorded::{RecordedMove, held_in};
use efference::inference::api::ModelKind;
use efference::protocol::transcript::Handle;
use efference::record::names::tools::namespace_of;
use std::collections::HashMap;

/// The session record of each case, by case index, and the tools their briefs come from.
pub(super) struct Records {
    /// What each case's log held before its verdict; `None` for a case replayed from its
    /// corpus line alone.
    pub(super) moves: Vec<Option<RecordedMove>>,
    /// The tools every session mounts.
    pub(super) builtin: Toolbox,
    /// One toolbox per distinct server declaration the cases' tools belong to.
    pub(super) servers: Vec<Toolbox>,
    /// The `servers` entry holding each case's tool, when its tool is a declared server's.
    pub(super) server_of: Vec<Option<usize>>,
    /// Lines for the operator: what was read, and what was not mounted.
    pub(super) notes: Vec<String>,
}

impl Records {
    /// The brief for `name`, the tool of case `index`'s call.
    pub(super) fn brief(
        &self,
        index: usize,
        name: &str,
    ) -> Option<efference::tools::ToolBrief<'_>> {
        self.server_of[index]
            .and_then(|server| self.servers[server].brief(name))
            .or_else(|| efference::agent::brief(&self.builtin, name))
    }

    /// The call case `index` replays: the move its log records, or its corpus line's.
    pub(super) fn call(&self, index: usize, case: &CaseIn) -> efference::record::log::ToolCall {
        match &self.moves[index] {
            Some(recorded) => recorded.judged.clone(),
            None => efference::record::log::ToolCall::new(
                case.call.name.clone(),
                "toolu_spotter_eval",
                case.call.input.clone(),
            ),
        }
    }
}

/// Read every case's session: its log, unless `no_session_log`, and the servers its tool
/// needs. Only servers of sessions under this process's project root are mounted, the
/// root the eval runs in; the rest are counted.
pub(super) async fn records(
    cases: &[Labeled],
    kind: ModelKind,
    no_session_log: bool,
) -> Result<Records, CliError> {
    let mut notes = Vec::new();
    let moves = match no_session_log {
        true => {
            notes.push(
                "session logs not read (--no-session-log): every case is replayed from its \
                 corpus line alone"
                    .to_string(),
            );
            cases.iter().map(|_| None).collect()
        }
        false => {
            let moves = session_moves(cases, kind);
            let read = moves.iter().filter(|m| m.is_some()).count();
            notes.push(format!(
                "{read} of {} case(s) replayed with what their session log held before the \
                 verdict; the rest have no readable log or verdict and are replayed from their \
                 corpus line alone",
                cases.len()
            ));
            moves
        }
    };
    let root = std::env::current_dir()?.canonicalize()?;
    let builtin = builtin_toolbox(&root, "spotter-eval")?;

    // Each case's tool, and the declaration of the server it belongs to when its session
    // declares one, read from the session's config file.
    let mut configs: HashMap<PathBuf, Option<efference::config::Config>> = HashMap::new();
    let mut declarations: Vec<(PathBuf, String, McpServerEntry)> = Vec::new();
    let mut wanted: Vec<Option<usize>> = Vec::with_capacity(cases.len());
    let mut elsewhere = 0usize;
    for (index, (case, ..)) in cases.iter().enumerate() {
        let name = match &moves[index] {
            Some(recorded) => recorded.judged.name.clone(),
            None => case.call.name.clone(),
        };
        let namespace = namespace_of(&name).to_string();
        let declared = case_log(case).and_then(|log| {
            let session_dir = log.parent()?.to_path_buf();
            let config = configs.entry(session_dir.clone()).or_insert_with(|| {
                efference::config::load(Some(&session_dir.join(efference::host::boot::CONFIG_NAME)))
                    .ok()
                    .map(|loaded| loaded.role)
            });
            let entry = config.as_ref()?.mcp_servers.get(&namespace)?.clone();
            let project = session_dir.parent()?.parent()?.canonicalize().ok()?;
            Some((project, entry))
        });
        wanted.push(match declared {
            Some((project, entry)) if project == root => {
                let at = declarations
                    .iter()
                    .position(|(_, n, e)| *n == namespace && *e == entry)
                    .unwrap_or_else(|| {
                        declarations.push((project, namespace.clone(), entry));
                        declarations.len() - 1
                    });
                Some(at)
            }
            Some(_) => {
                elsewhere += 1;
                None
            }
            None => None,
        });
    }
    if elsewhere > 0 {
        notes.push(format!(
            "{elsewhere} case(s) from other project roots name a tool of an MCP server their \
             session declares; those servers are not mounted here, so their tools run without \
             a brief"
        ));
    }
    let mut servers = Vec::with_capacity(declarations.len());
    for (project, name, entry) in declarations {
        let mut toolbox = Toolbox::new(Vec::new());
        notes.extend(mount_servers(&mut toolbox, BTreeMap::from([(name, entry)]), &project).await?);
        servers.push(toolbox);
    }
    Ok(Records {
        moves,
        builtin,
        servers,
        server_of: wanted,
        notes,
    })
}

/// What each case's log held before its verdict, keeping only what the rung `kind`
/// reads: the prose rung reads the utterances, the decision rung the precedents it
/// compares the move against. Each log is loaded once for all its cases.
pub(super) fn session_moves(cases: &[Labeled], kind: ModelKind) -> Vec<Option<RecordedMove>> {
    let mut by_log: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
    for (index, (case, ..)) in cases.iter().enumerate() {
        if case.seq.is_some()
            && let Some(log) = case_log(case)
        {
            by_log.entry(log).or_default().push(index);
        }
    }
    let mut moves: Vec<Option<RecordedMove>> = cases.iter().map(|_| None).collect();
    for (log, indices) in by_log {
        let Ok(entries) = efference::record::persist::load(&log) else {
            continue;
        };
        // The project the session ran in, whose session base holds a bound leaf's log.
        let project = log
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .map_or_default(Path::to_path_buf);
        // The session's id: its directory's name, as the live session knows it.
        let session = log
            .parent()
            .and_then(Path::file_name)
            .map_or_default(|name| name.to_string_lossy().into_owned());
        for index in indices {
            let seq = cases[index].0.seq.expect("grouped only with a seq");
            let found = RecordedMove::find(
                &session,
                &entries,
                Some(Handle { seq, call: None }),
                None,
                held_in(&project),
            );
            moves[index] = found.ok().map(|mut recorded| {
                match kind {
                    ModelKind::Text => recorded.precedents.clear(),
                    ModelKind::Decisions => {
                        recorded.utterances.said.clear();
                        recorded.precedents =
                            efference::curation::spotter::jev::compared_precedents(
                                &recorded.judged,
                                &recorded.precedents,
                            );
                    }
                }
                recorded
            });
        }
    }
    moves
}

/// The session log a case names: its `source`, relative to where the corpus was
/// extracted, else the same session's log under the case's `cwd`.
pub(super) fn case_log(case: &CaseIn) -> Option<PathBuf> {
    let source = Path::new(case.source.as_deref()?);
    if source.is_file() {
        return Some(source.to_path_buf());
    }
    let session = source.parent()?.file_name()?;
    let under_cwd = Path::new(case.cwd.as_deref()?)
        .join(SESSION_BASE)
        .join(session)
        .join(LOG_NAME);
    under_cwd.is_file().then_some(under_cwd)
}
