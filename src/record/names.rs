//! The names a recorded entry carries and its readers match on: tool and namespace names
//! ([`tools`], docs/tools.md) and the labels a turn's `source` is written in ([`sources`],
//! docs/log-and-projection.md).
//!
//! The tool backends, the control channel's handlers and the loop write these names. The
//! log's readers, the projection, the permission config and the clients match on them. The
//! module imports nothing from the crate, so every one of them can import it
//! (docs/README.md, *Dependency direction*).

pub mod sources;
pub mod tools;

#[cfg(test)]
mod test;
