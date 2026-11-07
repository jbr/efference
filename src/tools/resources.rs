//! Virtual reads, dispatched before filesystem tools (docs/context.md).
use super::Toolbox;
use crate::config::guide;
use crate::record::log::{RawResult, ToolCall};
use std::{future::Future, pin::Pin};

/// A host-backed content reader, owned by a session's toolbox, not part of tools[].
pub trait ResourceReader: Send + Sync {
    fn read<'a>(
        &'a self,
        uri: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>>;
}

impl Toolbox {
    /// Install dynamic content independently of enabled session-control verbs.
    pub fn mount_resources(&mut self, reader: Box<dyn ResourceReader>) {
        self.resources = Some(reader);
    }

    /// Resolve reserved URI batches as one ordinary tool result. Disk/URI batches
    /// remain refused whole, as with the compiled guide alone.
    pub async fn read_resources(&self, call: &ToolCall) -> Option<RawResult> {
        if call.name != "read" {
            return None;
        }
        let targets = guide::targets(call);
        if !targets.iter().any(|t| guide::is_virtual(t)) {
            return None;
        }
        if let Some(real) = targets.iter().find(|t| !guide::is_virtual(t)) {
            return Some(RawResult::error(format!(
                "this `read` mixes harness resources with a file on disk (`{real}`). Read them in separate calls."
            )));
        }
        let mut bodies = Vec::new();
        for target in targets {
            let body = if let Some(document) = guide::lookup(target) {
                Ok(document.body.trim_end().to_string())
            } else if target.split_once('?').map_or(target, |(base, _)| base)
                == crate::inference::catalog::resource::URI
            {
                match &self.resources {
                    Some(reader) => reader.read(target).await,
                    None => Err("the model catalog requires a hosted session; no host resource reader is mounted".into()),
                }
            } else {
                Err(guide::unknown(target))
            };
            match body {
                Ok(body) => bodies.push(body),
                Err(error) => return Some(RawResult::error(error)),
            }
        }
        Some(RawResult::text(bodies.join("\n\n---\n\n")))
    }
}

#[cfg(test)]
mod test;
