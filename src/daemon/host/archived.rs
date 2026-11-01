//! The sessions on disk that nothing here is running, as rows (docs/control-api.md).
//!
//! The rows come from [`crate::inventory`]'s filesystem scan, held on the [`Host`] so an
//! unchanged log is not read again. The row is its own type: an archived row has no
//! status or processes to report, which [`ListedSession`] carries. The single read
//! answers a [`DormantSession`], which adds what reading the whole log and the usage
//! record learns; the listing reads neither. The routes that serve them are
//! `host_http/archived.rs`.

use super::*;
use crate::inventory::sort_rows;
use crate::protocol::inventory::InventoryRow;
use crate::record::session_dir::SESSION_BASE;
use crate::session::USAGE_NAME;
use crate::session::UsageLine;
use crate::session::last_turn_usage;
use std::collections::HashSet;

impl DormantSession {
    /// `archived`, with what its log and usage record say. Blocking: reads the log
    /// whole, since a resume's switch can record selections anywhere in it.
    pub(crate) fn read(archived: ArchivedSession) -> Self {
        let dir = archived.row.cwd.join(SESSION_BASE).join(&archived.row.id);
        let selections =
            crate::record::persist::load_recording(dir.join(crate::record::session_dir::LOG_NAME))
                .ok()
                .map(|recording| crate::boot::recorded_selections(&recording))
                .filter(|selections| !selections.is_empty());
        let context_tokens = UsageLine::read(&dir.join(USAGE_NAME))
            .ok()
            .and_then(|lines| last_turn_usage(&lines))
            .map(|usage| usage.prompt_tokens());
        Self {
            archived,
            selections,
            context_tokens,
        }
    }
}

impl Host {
    /// The archived rows for `roots`, most recently active first, excluding
    /// sessions this host holds (same id and same working directory).
    ///
    /// Blocking: stats and reads logs. Callers off the executor.
    pub(crate) fn archived(&self, roots: &[PathBuf]) -> Vec<ArchivedSession> {
        let live: HashSet<(String, String)> = self
            .listing()
            .into_iter()
            .filter_map(|summary| Some((summary.id, summary.cwd?)))
            .collect();
        let mut rows: Vec<InventoryRow> = roots
            .iter()
            .flat_map(|root| self.inventory.refresh(root))
            .filter(|row| !live.contains(&(row.id.clone(), row.cwd.to_string_lossy().into_owned())))
            .collect();
        sort_rows(&mut rows);
        rows.into_iter()
            .map(|row| ArchivedSession {
                resume_failure: self.resume_failure(&row.cwd, &row.id),
                row,
                state: SessionState::Archived,
            })
            .collect()
    }
}
