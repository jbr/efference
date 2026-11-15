//! Composer discovery (docs/control-api.md): candidates for a typed path prefix and a
//! typed agent prefix. Read-only and one directory level deep, from names and
//! metadata; the route writes no log entry and consults no policy.

use super::*;
use crate::config::policy::lexical_normalize;
use crate::protocol::discovery::{AgentCandidates, PathCandidate, PathCandidates};
use crate::record::log::ReferenceTargetKind;
use crate::routes::host_http::host_of;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// The `limit` a request without one gets.
const DEFAULT_LIMIT: usize = 50;

/// The largest `limit` accepted; a larger value is clamped.
const MAX_LIMIT: usize = 200;

/// The longest `prefix` accepted, in bytes.
const MAX_PREFIX: usize = 4096;

/// The reason a `~` prefix has no directory to read.
const HOME_UNKNOWN: &str = "the host's home directory is not known";

/// The reason a directory is not listed: its own path has no UTF-8 spelling, so a
/// candidate path could not be returned unchanged.
const NON_UTF8: &str = "the directory path is not valid UTF-8";

/// A prefix resolved to the one directory to read and the fragment to match.
#[derive(Debug, PartialEq, Eq)]
struct Resolution {
    /// The prefix as sent, echoed on the answer.
    prefix: String,
    /// The directory spelling as typed, with its trailing `/` when it has one. It is
    /// what `original` joins an entry name onto.
    spelling: String,
    /// Absolute and lexically normalized, without resolving a link or mapping a
    /// worktree path.
    directory: PathBuf,
    /// The fragment after the last `/`, matched against entry names.
    filter: String,
    /// [`HOME_UNKNOWN`] when `~` was typed and this process has no home.
    problem: Option<String>,
}

/// A prefix split at its last `/`: the directory spelling (trailing `/` kept) and the
/// name fragment. A bare `~`, `.`, or `..` names a directory rather than a fragment.
fn split(prefix: &str) -> (&str, &str) {
    if matches!(prefix, "~" | "." | "..") {
        return (prefix, "");
    }
    match prefix.rfind('/') {
        Some(slash) => prefix.split_at(slash + 1),
        None => ("", prefix),
    }
}

/// The one directory `spelling` names: `~` and `~/…` against the host's home, an
/// absolute path as itself, and anything else (an empty spelling included) against the
/// working directory. A `~` followed by anything other than `/` is not a home
/// spelling; it stays an ordinary relative component, as [`crate::platform::home`]
/// documents.
fn directory(spelling: &str, working: &Path, home: Option<&Path>) -> Result<PathBuf, String> {
    let spelled = if spelling == "~" {
        home.map(Path::to_path_buf)
            .ok_or_else(|| HOME_UNKNOWN.to_string())?
    } else if let Some(rest) = spelling.strip_prefix("~/") {
        home.map(|home| home.join(rest))
            .ok_or_else(|| HOME_UNKNOWN.to_string())?
    } else {
        PathBuf::from(spelling)
    };
    let joined = if spelled.is_absolute() {
        spelled
    } else {
        working.join(spelled)
    };
    Ok(lexical_normalize(&joined))
}

/// The spelling a submission keeps for `name`: the typed directory spelling with the
/// name joined on, and a separator added when the spelling has none.
fn original(spelling: &str, name: &str) -> String {
    if spelling.is_empty() {
        name.to_string()
    } else if spelling.ends_with('/') {
        format!("{spelling}{name}")
    } else {
        format!("{spelling}/{name}")
    }
}

/// One candidate for `name`, or `None` when it is not a file or directory, its path
/// has no UTF-8 spelling, or its metadata cannot be read.
fn candidate(resolution: &Resolution, name: &str) -> Option<PathCandidate> {
    let path = resolution.directory.join(name);
    let link = std::fs::symlink_metadata(&path).ok()?;
    let symlink = link.file_type().is_symlink();
    // The kind follows a link, as the `/files` listing's does; the link itself is one
    // stat and its target another.
    let metadata = if symlink {
        std::fs::metadata(&path).ok()?
    } else {
        link
    };
    let target = if metadata.is_dir() {
        ReferenceTargetKind::Directory
    } else if metadata.is_file() {
        ReferenceTargetKind::File
    } else {
        return None;
    };
    Some(PathCandidate {
        name: name.to_string(),
        original: original(&resolution.spelling, name),
        path: path.to_str()?.to_string(),
        target,
        symlink,
    })
}

/// Why a directory could not be read, as one fixed phrase: the platform's message is
/// not stable across hosts and a client shows this beside the composer.
fn problem(error: &std::io::Error) -> String {
    match error.kind() {
        ErrorKind::NotFound => "no such directory",
        ErrorKind::NotADirectory => "not a directory",
        ErrorKind::PermissionDenied => "permission denied",
        _ => "the directory could not be read",
    }
    .to_string()
}

/// Resolve `prefix` against `working` and `home` without touching the filesystem.
fn resolve(prefix: &str, working: &Path, home: Option<&Path>) -> Resolution {
    let (spelling, filter) = split(prefix);
    let (directory, problem) = match directory(spelling, working, home) {
        Ok(directory) => (directory, None),
        Err(problem) => (working.to_path_buf(), Some(problem)),
    };
    Resolution {
        prefix: prefix.to_string(),
        spelling: spelling.to_string(),
        directory,
        filter: filter.to_string(),
        problem,
    }
}

/// Read one directory level, blocking: every entry name is examined and the matching
/// names are sorted, then candidates are statted in name order only until one past
/// `limit` exists. `limit` bounds the stats and the answer, not the read or the sort.
fn listing(resolution: &Resolution, limit: usize) -> PathCandidates {
    let mut answer = PathCandidates {
        prefix: resolution.prefix.clone(),
        directory: resolution.directory.to_str().map_or_default(str::to_owned),
        filter: resolution.filter.clone(),
        truncated: false,
        problem: resolution.problem.clone(),
        entries: Vec::new(),
    };
    if answer.problem.is_none() && resolution.directory.to_str().is_none() {
        answer.problem = Some(NON_UTF8.to_string());
    }
    if answer.problem.is_some() {
        return answer;
    }
    let mut names = Vec::new();
    match std::fs::read_dir(&resolution.directory) {
        Ok(entries) => {
            for entry in entries {
                let Ok(entry) = entry else {
                    continue;
                };
                // A name that is not UTF-8 is left out: a lossy rewrite would not
                // resolve back to the same entry.
                let file_name = entry.file_name();
                let Some(name) = file_name.to_str() else {
                    continue;
                };
                if name.starts_with(&resolution.filter) {
                    names.push(name.to_string());
                }
            }
        }
        Err(error) => {
            answer.problem = Some(problem(&error));
            return answer;
        }
    }
    names.sort_unstable();
    let mut candidates = Vec::new();
    for name in names {
        let Some(entry) = candidate(resolution, &name) else {
            continue;
        };
        candidates.push(entry);
        if candidates.len() > limit {
            answer.truncated = true;
            break;
        }
    }
    candidates.truncate(limit);
    answer.entries = candidates;
    answer
}

/// `prefix` and `limit` from the query. An absent `limit` is [`DEFAULT_LIMIT`], one
/// above [`MAX_LIMIT`] is clamped, and anything that is not a non-negative integer
/// (an empty value included) is a `400`.
fn query(query: &str) -> Result<(String, usize), String> {
    let prefix = query_value(query, "prefix").unwrap_or_default();
    if prefix.len() > MAX_PREFIX {
        return Err(format!("prefix is longer than {MAX_PREFIX} bytes"));
    }
    let limit = match query_value(query, "limit") {
        None => DEFAULT_LIMIT,
        Some(raw) => raw
            .parse::<usize>()
            .map_err(|_| "limit must be a non-negative integer".to_string())?
            .min(MAX_LIMIT),
    };
    Ok((prefix, limit))
}

/// The directory a conn's session works in, hosted or still starting. `None` when
/// the conn has neither (a wiring bug).
fn working_of(conn: &Conn) -> Option<PathBuf> {
    if let Some(shared) = shared_of(conn) {
        return Some(shared.directories().working().to_path_buf());
    }
    starting_of(conn).map(|starting| starting.working().to_path_buf())
}

/// `GET /discovery/paths?prefix=<text>&limit=<n>` (docs/control-api.md): one
/// directory's entries whose names start with the prefix's last fragment, sorted by
/// name.
pub(super) async fn discover_paths(conn: Conn) -> Conn {
    let (prefix, limit) = match query(conn.querystring()) {
        Ok(request) => request,
        Err(message) => return fail(conn, ErrorCode::BadRequest, message),
    };
    let Some(working) = working_of(&conn) else {
        return missing_state(conn);
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let resolution = resolve(&prefix, &working, home.as_deref());
    // Reading a directory and statting its entries are blocking syscalls; they run
    // off the runtime's worker threads.
    match tokio::task::spawn_blocking(move || listing(&resolution, limit)).await {
        Ok(answer) => views::answer(conn, 200, answer),
        Err(error) => fail(
            conn,
            ErrorCode::Internal,
            format!("the discovery read did not finish: {error}"),
        ),
    }
}

/// `GET /discovery/agents?prefix=<text>&limit=<n>` (docs/control-api.md): the host's
/// session rows whose ids start with the prefix, `@` ignored. The universe is
/// `Host::listing()`, the same one a submission's agent target is checked against.
pub(super) async fn discover_agents(conn: Conn) -> Conn {
    let (prefix, limit) = match query(conn.querystring()) {
        Ok(request) => request,
        Err(message) => return fail(conn, ErrorCode::BadRequest, message),
    };
    let Some(host) = host_of(&conn) else {
        return views::answer(
            conn,
            200,
            AgentCandidates {
                prefix,
                truncated: false,
                problem: Some(
                    "this listener serves one session; agent candidates need a host".to_string(),
                ),
                agents: Vec::new(),
            },
        );
    };
    let typed = prefix.strip_prefix('@').unwrap_or(&prefix);
    let mut agents = Vec::new();
    let mut truncated = false;
    for listed in host.listing() {
        if !listed.id.starts_with(typed) {
            continue;
        }
        if agents.len() >= limit {
            truncated = true;
            break;
        }
        agents.push(listed);
    }
    views::answer(
        conn,
        200,
        AgentCandidates {
            prefix,
            truncated,
            problem: None,
            agents,
        },
    )
}

#[cfg(test)]
mod test;
