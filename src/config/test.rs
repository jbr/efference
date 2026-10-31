use super::*;

/// Load → apply → sync. The persistence tests below predate [`ConfigFile`]; they exercise the
/// same edit path through a fresh handle, which is also the honest read of what they cover
/// (the on-disk result of one change), independent of the held-document invariant.
fn persist_config_change(path: &Path, change: &ConfigChange) -> std::io::Result<()> {
    let mut file = ConfigFile::load(path)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    file.apply(change)
}

fn parse_str(text: &str) -> Config {
    parse(Path::new("test.toml"), text).expect("valid config")
}

// --- [context] preamble builder (docs/context.md) ---

// --- diff_changes: the fs-watch initiator's file-vs-effective delta ---

mod diff;
mod file;
mod fixture;
mod parse;
mod preamble;
