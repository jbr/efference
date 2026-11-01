//! The resume at boot: a restarted host reopens the sessions that were open when the last
//! one went away (docs/resumption.md, docs/control-api.md).
//!
//! The pass reads the remembered roots (the ones behind `GET /sessions?state=archived`)
//! and resumes every session whose last lifecycle entry is not `Archived`, through
//! [`Host::resume`] — the path `PATCH {"state":"active"}` takes — so each is reserved as
//! `starting` at once and prepared in the background while the host serves. A session
//! that fails to resume stays dormant with its `resume_failure`; nothing here stops the
//! host.
//!
//! The pass runs only on a host that is not an island. An island reopens nothing: its
//! exit archives what it hosts (`island_exit.rs`), so a session it reopened, another
//! host's or a crashed island's, would be archived when the island's owner went.

use super::resume::ResumeRefused;
use super::*;
use crate::record::session_dir::{SESSION_BASE, plain_id};
use std::collections::{HashMap, HashSet};
use tokio::sync::{Semaphore, watch};

/// How many sessions prepare at once during the boot resume. Preparation runs each
/// session's env hook and mounts its MCP servers, which spawns processes and may reach
/// the network; the pass takes this many at a time and starts the next when one has
/// started or failed.
pub(super) const BOOT_PREPARING: usize = 4;

/// One session the boot pass will resume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Reopening {
    pub(super) id: String,
    pub(super) root: PathBuf,
    /// The creator the header names, whether or not this pass resumes it.
    pub(super) created_by: Option<String>,
}

impl Host {
    /// Resume the sessions the last host left open, creators before the children that
    /// end with them, and return the ids that started. Waits for the host-start pass
    /// (the id backfill) first, so every session it resumes holds its id. Returns once every session has started or failed to; on an island,
    /// at once, having resumed nothing.
    pub async fn resume_open_sessions(self: &Arc<Self>) -> Vec<String> {
        if self.is_island() {
            return Vec::new();
        }
        let mut passed = self.start_pass.clone();
        if passed.wait_for(|done| *done).await.is_err() {
            log::warn!("boot resume skipped: the host-start pass did not finish");
            return Vec::new();
        }
        let scanning = Arc::clone(self);
        let reopening = match tokio::task::spawn_blocking(move || scanning.open_sessions()).await {
            Ok(found) => found,
            Err(e) => {
                log::warn!("boot resume skipped: the scan failed: {e}");
                return Vec::new();
            }
        };
        if reopening.is_empty() {
            return Vec::new();
        }
        log::info!("boot resume: {} open session(s)", reopening.len());
        let reopening = without_cycles(reopening);
        let permits = Arc::new(Semaphore::new(BOOT_PREPARING));
        // Each session's outcome, for the children that wait on it: `true` once hosted.
        // The sender dropping without it means the session did not start.
        let mut hosted: HashMap<String, watch::Receiver<bool>> = HashMap::new();
        let mut tasks = tokio::task::JoinSet::new();
        // Every receiver exists before any task runs, so a child finds its creator's
        // whatever the spawn order.
        let mut senders = Vec::new();
        for session in &reopening {
            let (tx, rx) = watch::channel(false);
            hosted.insert(session.id.clone(), rx);
            senders.push(tx);
        }
        for (session, outcome) in reopening.into_iter().zip(senders) {
            let creator = session
                .created_by
                .as_ref()
                .and_then(|creator| hosted.get(creator).cloned());
            let host = Arc::clone(self);
            let permits = Arc::clone(&permits);
            tasks.spawn(async move { host.reopen(session, creator, permits, outcome).await });
        }
        let mut started = Vec::new();
        while let Some(done) = tasks.join_next().await {
            if let Ok(Some(id)) = done {
                started.push(id);
            }
        }
        started
    }

    /// One session's turn in the pass: wait for the creator to be hosted (or to have
    /// failed), take a preparation slot, resume, and hold the slot until it has started.
    async fn reopen(
        self: Arc<Self>,
        session: Reopening,
        creator: Option<watch::Receiver<bool>>,
        permits: Arc<Semaphore>,
        outcome: watch::Sender<bool>,
    ) -> Option<String> {
        if let Some(mut creator) = creator {
            // `recorded_lineage` keeps a creator only while it is hosted, not merely
            // starting. A creator that failed leaves the child standing on its own,
            // as a child whose creator was archived does.
            let _ = creator.wait_for(|hosted| *hosted).await;
        }
        let Ok(_slot) = permits.acquire().await else {
            return None;
        };
        if !self.swansong.state().is_running() {
            return None;
        }
        let Reopening { id, root, .. } = session;
        let mut patch = PatchSession::activate(root.clone());
        patch.refuse_authorization = true;
        match self.resume(&id, patch) {
            Ok(_) => {}
            Err(ResumeRefused::AlreadyActive) => return None,
            Err(ResumeRefused::Conflict(reason) | ResumeRefused::Refused(reason)) => {
                log::warn!("session {id}: not resumed at boot: {reason}");
                self.resume_failures
                    .lock()
                    .unwrap()
                    .insert((root, id), reason);
                return None;
            }
        }
        let Some(Entry::Starting(starting)) = self.entry(&id) else {
            return None;
        };
        match starting.finished().await {
            Ok(_) => {
                let _ = outcome.send(true);
                Some(id)
            }
            // `preparation_failed` has recorded the reason and logged it.
            Err(_) => None,
        }
    }

    /// The sessions under the remembered roots that were open when the last host went
    /// away, in the order the pass takes them: each root most recently used first, its
    /// sessions most recently active first. Blocking: stats and reads logs.
    pub(super) fn open_sessions(&self) -> Vec<Reopening> {
        let mut found = Vec::new();
        let mut seen = HashSet::new();
        for root in self.remembered.roots() {
            for row in self.inventory.refresh(&root) {
                if !row.interrupted
                    || row.problem.is_some()
                    || !plain_id(&row.id)
                    || !seen.insert(row.id.clone())
                {
                    continue;
                }
                let created_by = header_creator(&root, &row.id);
                found.push(Reopening {
                    id: row.id,
                    root: root.clone(),
                    created_by,
                });
            }
        }
        found
    }
}

/// The creator the session's header records, read from line 1 alone.
fn header_creator(root: &std::path::Path, id: &str) -> Option<String> {
    use std::io::BufRead;
    let log = root
        .join(SESSION_BASE)
        .join(id)
        .join(crate::record::session_dir::LOG_NAME);
    let mut line = Vec::new();
    io::BufReader::new(std::fs::File::open(&log).ok()?)
        .read_until(b'\n', &mut line)
        .ok()?;
    let header = crate::record::persist::read_header(&log, line.trim_ascii_end()).ok()?;
    header.metadata()?.created_by.clone()
}

/// Drop the creator from any session whose chain of creators leads back to itself, so
/// no session waits on a cycle. Headers written by this host never form one.
fn without_cycles(mut sessions: Vec<Reopening>) -> Vec<Reopening> {
    let creators: HashMap<String, Option<String>> = sessions
        .iter()
        .map(|s| (s.id.clone(), s.created_by.clone()))
        .collect();
    for session in &mut sessions {
        let mut next = session.created_by.clone();
        let mut cyclic = false;
        for _ in 0..=creators.len() {
            let Some(id) = next else { break };
            if id == session.id {
                cyclic = true;
                break;
            }
            next = creators.get(&id).cloned().flatten();
        }
        if cyclic {
            session.created_by = None;
        }
    }
    sessions
}
