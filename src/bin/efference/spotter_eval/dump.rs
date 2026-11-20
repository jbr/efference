//! `--dump-states`: the decision rung's request state per case, written out and nothing
//! sent (docs/permissions.md). The state carries the resolved `move.paths` only this
//! code computes, so an experiment that varies what the state holds, or asks it other
//! questions, starts from the shipping state rather than a re-implementation of it.

use super::*;

/// The corpus lines in `cases_files`, in order: those whose id contains `filter`, at
/// most `limit` of them. Labels are not required: every case is read.
fn read_cases(
    cases_files: &[PathBuf],
    filter: Option<&str>,
    limit: Option<usize>,
) -> Result<Vec<CaseIn>, CliError> {
    let mut cases = Vec::new();
    for path in cases_files {
        for line in std::fs::read_to_string(path)?.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let case: CaseIn = serde_json::from_str(line)
                .map_err(|e| format!("{}: bad case line: {e}", path.display()))?;
            if filter.is_some_and(|f| !case.id.contains(f)) {
                continue;
            }
            if limit.is_some_and(|l| cases.len() >= l) {
                return Ok(cases);
            }
            cases.push(case);
        }
    }
    Ok(cases)
}

/// The directories a case is judged in: its recorded cwd, which stands as both the
/// project and the working directory; `None` without one.
fn directories(case: &CaseIn) -> Option<efference::config::policy::Directories> {
    case.cwd
        .as_deref()
        .map(|cwd| efference::config::policy::Directories::at(Path::new(cwd)))
}

/// One dumped line. The state keeps its wire order in the file.
#[derive(Serialize)]
struct Line<'a> {
    id: &'a str,
    state: &'a efference::curation::spotter::jev::State,
    /// Whether the case was rebuilt from its session log.
    log_read: bool,
    /// What the log could not supply.
    notes: &'a [String],
}

/// Write the decision rung's request `state` for every case, one `{id, state, log_read,
/// notes}` line each: the state the rung sends for the case as the eval replays it
/// (`records.rs`). A case whose log is readable is rebuilt from it — the move its log
/// records (an allowing `agents__decide` as its leaf, with the approval chain), the
/// promotion probe its verdict records, and the utterances before its verdict as
/// `conversation` — unless `no_session_log`; the rest are dumped from their corpus line
/// alone, with `log_read: false` and no `conversation`. The brief comes from the
/// built-in tools or the server the case's session declares.
pub(crate) async fn dump_states(
    cases_files: &[PathBuf],
    filter: Option<&str>,
    limit: Option<usize>,
    no_session_log: bool,
    out: &Path,
) -> CliResult {
    let cases: Vec<Labeled> = read_cases(cases_files, filter, limit)?
        .into_iter()
        .map(|case| (case, String::new(), None))
        .collect();
    let records = records::records(&cases, no_session_log).await?;
    for note in &records.notes {
        eprintln!("{note}");
    }
    let mut lines = String::new();
    for (index, (case, ..)) in cases.iter().enumerate() {
        let call = records.call(index, case);
        let brief = records.brief(index, &call.name);
        let recorded = records.moves[index].as_ref();
        let circumstances = match recorded {
            Some(recorded) => recorded.circumstances(brief),
            None => Circumstances::default().with_brief(brief),
        };
        let scratch = case.scratch.then(|| PathBuf::from(SCRATCH_PLACEHOLDER));
        let state = efference::curation::spotter::jev::requests(
            &role(case),
            &call,
            &circumstances,
            directories(case).as_ref(),
            scratch.as_deref(),
        )
        .state;
        lines.push_str(&serde_json::to_string(&Line {
            id: &case.id,
            state: &state,
            log_read: recorded.is_some(),
            notes: recorded.map_or(&[][..], |r| r.notes.as_slice()),
        })?);
        lines.push('\n');
    }
    std::fs::write(out, lines)?;
    eprintln!("{} states -> {}", cases.len(), out.display());
    Ok(())
}
