//! What is kept of a session's record for the model, and what it may do: per-result triage,
//! the spotter's permission verdicts, the ambient-notification inbox, compaction,
//! consolidation and the trajectory digest
//! (docs/README.md, *Dependency direction*).

pub mod compact;
pub mod consolidate;
pub mod digest;
pub mod inbox;
pub mod spotter;
pub mod triage;
