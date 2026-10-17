//! `efference control status --follow`: the status blocks of the followed agents on a
//! writer, one per decision point (docs/operating.md).
//!
//! The same follower and the same key as `wait` and the mount's status streams
//! (`watch.rs`): each agent's status at connect, then each status whose decision point
//! differs from the last one printed, nothing for a working loop. It returns once every
//! followed agent has ended, after printing its end, and errors when the host stops
//! answering.

use std::collections::{BTreeSet, HashMap};
use std::io::Write;
use std::ops::ControlFlow;

use anyhow::{Result, anyhow};
use tokio::task::JoinSet;

use super::status::operator_reader;
use super::watch::{Followed, Mark, ended, follow, mark};
use crate::client::Client;
use crate::daemon::status::render;

/// Print the status blocks of `agents` on `out` as they arrive, blocks separated by a
/// blank line and `out` flushed after each. `host` is a client at the host's socket
/// root; `me` names the reading client, as `status` does.
pub(crate) async fn follow_status(
    host: &Client,
    agents: &[String],
    me: Option<&str>,
    out: &mut (impl Write + Send),
) -> Result<()> {
    let held = host.list_sessions().await?;
    for agent in agents {
        if !held.contains(agent) {
            return Err(match held.is_empty() {
                true => anyhow!("no agent {agent:?} on this host, which holds none"),
                false => anyhow!(
                    "no agent {agent:?} on this host; it holds {}",
                    held.join(", ")
                ),
            });
        }
    }
    let (readings, mut arrivals) = tokio::sync::mpsc::unbounded_channel();
    // Aborts every follower when this returns.
    let mut followers = JoinSet::new();
    for agent in agents {
        let (host, agent, readings) = (host.clone(), agent.clone(), readings.clone());
        followers.spawn(async move {
            follow(host, &agent.clone(), |followed| {
                match readings.send((agent.clone(), followed)) {
                    Ok(()) => ControlFlow::Continue(()),
                    Err(_) => ControlFlow::Break(()),
                }
            })
            .await
        });
    }
    drop(readings);
    let mut open: BTreeSet<&str> = agents.iter().map(String::as_str).collect();
    // The mark of the last block printed of each agent.
    let mut printed: HashMap<String, Option<Mark>> = HashMap::new();
    let mut first = true;
    while !open.is_empty() {
        let Some((agent, followed)) = arrivals.recv().await else {
            break;
        };
        if !open.contains(agent.as_str()) {
            continue;
        }
        let view = match followed {
            Followed::Status(view) => *view,
            Followed::Left => ended(&agent),
            Followed::Unreachable => {
                return Err(anyhow!("the host is unreachable; stopped following"));
            }
        };
        let now = mark(&view);
        let new = match printed.get(&agent) {
            None => true,
            Some(last) => now.is_some() && *last != now,
        };
        if !new {
            continue;
        }
        let block = render(&view, &operator_reader(&view, me), super::now());
        if !first {
            writeln!(out)?;
        }
        first = false;
        writeln!(out, "{block}")?;
        out.flush()?;
        if now == Some(Mark::Ended) {
            open.remove(agent.as_str());
        }
        printed.insert(agent, now);
    }
    Ok(())
}
