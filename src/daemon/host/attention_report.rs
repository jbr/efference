//! The host's side of an attention report (docs/web-push.md): the table the report goes
//! into is `attention.rs`, which the host shares with every session it adopts.

use super::*;
use crate::log::PushCancel;
use crate::protocol::attention::AttentionReport;

impl Host {
    /// Take a client's report: record it, and when it is an active report from a
    /// client with a hosted session open, tell that session its pending moment has
    /// been seen ([`Session::cancel_delayed_push`]).
    pub(crate) fn report_attention(&self, report: &AttentionReport) {
        self.attention.report_at(report, Instant::now());
        if report.active
            && let Some(id) = &report.session
            && let Some(session) = self.session(id)
        {
            session.cancel_delayed_push(PushCancel::Seen {
                client: report.client.clone(),
            });
        }
    }
}
