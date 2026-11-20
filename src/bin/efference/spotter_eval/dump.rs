//! `--dump-states`: the decision rung's request state per case, written out and nothing
//! sent (docs/permissions.md). The state carries the resolved `move.paths` only this
//! code computes, so an experiment that varies what *else* the state holds — history,
//! precedent, the conversation — starts from the shipping state rather than a
//! re-implementation of it.

use super::*;
use efference::record::utterances::{Speaker, Utterances};

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

/// Write the decision rung's request `state` for every case, one `{id, state}` line
/// each, from the corpus line alone: the built-in tools' briefs, no session log.
pub(crate) fn dump_states(
    cases_files: &[PathBuf],
    filter: Option<&str>,
    limit: Option<usize>,
    out: &Path,
) -> CliResult {
    let toolbox = spotter_request::builtin_toolbox(&std::env::current_dir()?, "spotter-eval")?;
    let mut lines = String::new();
    let cases = read_cases(cases_files, filter, limit)?;
    for case in &cases {
        let call = efference::record::log::ToolCall::new(
            case.call.name.clone(),
            "toolu_spotter_eval",
            case.call.input.clone(),
        );
        let brief = efference::agent::brief(&toolbox, &call.name);
        let scratch = case.scratch.then(|| PathBuf::from(SCRATCH_PLACEHOLDER));
        let state = efference::curation::spotter::jev::preview_state(
            &role(case),
            &call,
            brief.as_ref(),
            None,
            directories(case).as_ref(),
            scratch.as_deref(),
        );
        let state: Value = serde_json::from_str(&state)?;
        lines.push_str(&serde_json::json!({ "id": case.id, "state": state }).to_string());
        lines.push('\n');
    }
    std::fs::write(out, lines)?;
    eprintln!("{} states -> {}", cases.len(), out.display());
    Ok(())
}

/// `--with-conversation`: every case's state as the session log rebuilds it, with the
/// session's conversation added, one `{id, state, log_read, notes}` line each. Not what
/// the decision rung sends: the input to an experiment that gives it what the prose
/// rung reads.
///
/// Each case is read the way the eval replays it (`records.rs`): the move its log
/// records (an allowing `agents__decide` as its leaf, with the approval chain), the
/// brief from the built-in tools or the server its session declares, and the
/// utterances before its verdict as `state.conversation`, `[{writer, text}]` whole and
/// oldest first. The promotion probe is left out, since no question here reads it. A
/// case whose log is not readable has `log_read: false` and no `conversation`.
pub(crate) async fn dump_conversation_states(
    cases_files: &[PathBuf],
    filter: Option<&str>,
    limit: Option<usize>,
    out: &Path,
) -> CliResult {
    let cases: Vec<Labeled> = read_cases(cases_files, filter, limit)?
        .into_iter()
        .map(|case| (case, String::new(), None))
        .collect();
    // The prose rung's reading keeps the utterances, which is what this dump adds.
    let records =
        records::records(&cases, efference::inference::api::ModelKind::Text, false).await?;
    for note in &records.notes {
        eprintln!("{note}");
    }
    let mut lines = String::new();
    for (index, (case, ..)) in cases.iter().enumerate() {
        lines.push_str(&conversation_state(&records, index, case).to_string());
        lines.push('\n');
    }
    std::fs::write(out, lines)?;
    eprintln!(
        "{} states with their conversation -> {}",
        cases.len(),
        out.display()
    );
    Ok(())
}

/// Case `index`'s dump line: its state with the conversation, whether its log was read,
/// and the notes on what the log could not supply.
fn conversation_state(records: &records::Records, index: usize, case: &CaseIn) -> Value {
    let call = records.call(index, case);
    let brief = records.brief(index, &call.name);
    let recorded = records.moves[index].as_ref();
    let circumstances = Circumstances::default()
        .with_brief(brief)
        .with_chain(recorded.map_or(&[], |r| r.chain.as_slice()));
    let scratch = case.scratch.then(|| PathBuf::from(SCRATCH_PLACEHOLDER));
    let mut state = efference::curation::spotter::jev::requests(
        &role(case),
        &call,
        &circumstances,
        directories(case).as_ref(),
        scratch.as_deref(),
    )
    .state;
    if let Some(recorded) = recorded {
        state
            .as_object_mut()
            .expect("the state is built as an object")
            .insert("conversation".into(), conversation(&recorded.utterances));
    }
    serde_json::json!({
        "id": case.id,
        "state": state,
        "log_read": recorded.is_some(),
        "notes": recorded.map_or(&[][..], |r| r.notes.as_slice()),
    })
}

/// The utterances as `[{writer, text}]`, oldest first, each text whole. The writer is
/// `operator`, `supervisor:<id>` for a supervising agent, or `agent`.
pub(super) fn conversation(utterances: &Utterances) -> Value {
    utterances
        .said
        .iter()
        .map(|utterance| {
            let writer = match &utterance.speaker {
                Speaker::Operator => "operator".to_string(),
                Speaker::Supervisor(id) => format!("supervisor:{id}"),
                Speaker::Agent => "agent".to_string(),
            };
            serde_json::json!({ "writer": writer, "text": utterance.text })
        })
        .collect()
}
