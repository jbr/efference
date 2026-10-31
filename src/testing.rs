//! Scratch space for tests that need a real filesystem.

use tempfile::TempDir;

/// A directory of its own under the OS temp dir, named `efference-<label>-` and a random
/// suffix, that is removed when the returned guard drops, a panicking test's included.
/// The directory is created before the name is returned and a taken name is retried, so
/// two tests never hold the same one. The guard has to live as long as anything the test
/// opens inside it, such as a database.
pub(crate) fn temp_dir(label: &str) -> TempDir {
    TempDir::with_prefix(format!("efference-{label}-")).expect("a scratch directory")
}
