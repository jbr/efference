//! The composer's staged attachments (docs/tui.md, docs/inbox.md).
//!
//! A file the operator attaches — a clipboard image, a paste past the inline limit, a file
//! dragged onto the terminal — is uploaded to the daemon the moment it is staged
//! (`POST /attachments`), so the chip shows what the daemon made of it while it can still be
//! removed. The turn that sends it names the upload. This module is the bookkeeping; the upload
//! itself runs on the tokio side ([`AttachCmd`]) and its answer comes home as
//! [`TuiEvent::Uploaded`](super::TuiEvent::Uploaded).

use std::collections::HashSet;
use std::path::PathBuf;

use crate::attachments::Arrival;

/// What the UI asks the tokio side to do with an attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AttachCmd {
    /// Upload `bytes` under a suggested `name`; the answer is keyed by `key`.
    Upload {
        key: u64,
        name: String,
        bytes: Vec<u8>,
    },
    /// Remove an upload the operator took back before sending.
    Remove { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Status {
    Uploading,
    Ready(Arrival),
    Failed(String),
}

/// One staged file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Staged {
    /// Local identity: the same file can be staged twice.
    pub key: u64,
    /// The name it was staged under, until the daemon answers with the stored one.
    pub label: String,
    pub status: Status,
}

impl Staged {
    /// The stored name once uploaded, the staged label before.
    pub fn name(&self) -> &str {
        match &self.status {
            Status::Ready(arrival) => &arrival.name,
            _ => &self.label,
        }
    }
}

/// The composer's staged row, in staging order.
#[derive(Debug, Default)]
pub(crate) struct StagedAttachments {
    items: Vec<Staged>,
    next_key: u64,
    /// Keys removed while their upload was in flight: when the upload lands, what it stored is
    /// removed rather than resurrecting the chip.
    dropped: HashSet<u64>,
}

impl StagedAttachments {
    pub fn items(&self) -> &[Staged] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Stage a file for upload, returning the command that uploads it.
    pub fn stage(&mut self, name: String, bytes: Vec<u8>) -> AttachCmd {
        self.next_key += 1;
        let key = self.next_key;
        self.items.push(Staged {
            key,
            label: name.clone(),
            status: Status::Uploading,
        });
        AttachCmd::Upload { key, name, bytes }
    }

    /// Stage attachments a recall handed back: already stored, so ready at once.
    pub fn restore(&mut self, arrivals: Vec<Arrival>) {
        for arrival in arrivals {
            self.next_key += 1;
            self.items.push(Staged {
                key: self.next_key,
                label: arrival.name.clone(),
                status: Status::Ready(arrival),
            });
        }
    }

    /// An upload answered. A chip removed meanwhile gets its upload removed instead.
    pub fn landed(&mut self, key: u64, result: Result<Arrival, String>) -> Option<AttachCmd> {
        if self.dropped.remove(&key) {
            return result
                .ok()
                .map(|arrival| AttachCmd::Remove { name: arrival.name });
        }
        let item = self.items.iter_mut().find(|s| s.key == key)?;
        item.status = match result {
            Ok(arrival) => Status::Ready(arrival),
            Err(e) => Status::Failed(e),
        };
        None
    }

    /// Take a chip back. An uploaded file is removed from the store; one still uploading is
    /// removed when its upload lands.
    pub fn remove(&mut self, key: u64) -> Option<AttachCmd> {
        let at = self.items.iter().position(|s| s.key == key)?;
        match self.items.remove(at).status {
            Status::Ready(arrival) => Some(AttachCmd::Remove { name: arrival.name }),
            Status::Uploading => {
                self.dropped.insert(key);
                None
            }
            Status::Failed(_) => None,
        }
    }

    /// The most recently staged chip's key — what `⌫` on an empty composer takes back.
    pub fn last_key(&self) -> Option<u64> {
        self.items.last().map(|s| s.key)
    }

    pub fn uploading(&self) -> bool {
        self.items.iter().any(|s| s.status == Status::Uploading)
    }

    /// What a send carries: every uploaded file, in staging order.
    pub fn ready(&self) -> Vec<Arrival> {
        self.items
            .iter()
            .filter_map(|s| match &s.status {
                Status::Ready(arrival) => Some(arrival.clone()),
                _ => None,
            })
            .collect()
    }

    /// Empty the row after a send, deleting nothing: the sent turn names the files.
    pub fn clear(&mut self) {
        self.items.clear();
    }
}

/// The files a paste names, when it is exactly a drag onto the terminal: one or more paths,
/// shell-escaped the way terminals type them (`/Users/me/My\ Shot.png`, `'/tmp/a b.txt'`) or
/// as `file://` URLs, each an existing regular file. `None` for anything else, which pastes as
/// text.
///
/// Only absolute paths (or `~/`, or `file://`) count. A drag always produces one; a relative
/// path in a paste is far more likely something the operator is typing *about*.
pub(crate) fn dropped_paths(text: &str) -> Option<Vec<PathBuf>> {
    let words = shell_words(text.trim())?;
    if words.is_empty() {
        return None;
    }
    words
        .into_iter()
        .map(|word| {
            let path = if let Some(rest) = word.strip_prefix("file://") {
                PathBuf::from(
                    percent_encoding::percent_decode_str(rest)
                        .decode_utf8()
                        .ok()?
                        .into_owned(),
                )
            } else if let Some(rest) = word.strip_prefix("~/") {
                std::env::home_dir()?.join(rest)
            } else {
                PathBuf::from(word)
            };
            (path.is_absolute() && path.is_file()).then_some(path)
        })
        .collect()
}

/// Split a line the way a shell would for the escapes terminals use on a drop: whitespace
/// separates, a backslash escapes the next character, and single or double quotes group. `None`
/// on an unterminated quote.
fn shell_words(text: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                word.push(chars.next()?);
                in_word = true;
            }
            '\'' | '"' => {
                loop {
                    match chars.next()? {
                        q if q == c => break,
                        '\\' if c == '"' => word.push(chars.next()?),
                        other => word.push(other),
                    }
                }
                in_word = true;
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            c => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Some(words)
}

#[cfg(test)]
mod test;
