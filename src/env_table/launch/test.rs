//! The environment a launch establishes, and how a hook ends.
//!
//! The hooks are `/bin/sh` scripts written as explicit argv arrays. That is the
//! shape the feature asks an operator to configure — a shell *in* the argv,
//! never one the harness supplies — so the tests exercise the real path rather
//! than a stand-in, and a `sh -c` recipe is one a user would actually write.
//!
//! The tests are in three sibling files: `basic` for what a launch establishes,
//! `parser` for the dump grammar, and `lifecycle` for how a hook ends. The
//! lifecycle ones are written against the orderings that matter — EOF without an
//! exit, an exit without EOF, a descendant outliving the hook, a future aborted
//! rather than awaited — and each asserts the outcome, because the earlier
//! lifecycle reported success for several of these.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;

mod basic;
mod lifecycle;
mod parser;

/// A scratch directory that removes itself. Tests need a real absolute cwd (the
/// hook is run in one) and somewhere for scripts to leave markers.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("efference-launch-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// Write an executable `/bin/sh` script and return the argv that runs it.
    fn script(&self, name: &str, body: &str) -> Vec<String> {
        let path = self.0.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("script");
        vec!["/bin/sh".to_string(), path.to_string_lossy().into_owned()]
    }

    /// A path inside the scratch directory, for a script to leave a marker in.
    fn marker(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A host environment to inherit from and to replace.
fn host_env(entries: &[(&str, &str)]) -> EnvSnapshot {
    let values: BTreeMap<String, String> = entries
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect();
    EnvSnapshot::new(values).expect("valid environment")
}

/// The entries of a snapshot, for comparison against a [`host_env`].
fn entries(snapshot: &EnvSnapshot) -> BTreeMap<String, String> {
    snapshot
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

/// An argv whose stdout is exactly this `printf` format string — the shape
/// `env -0` produces, written out so a test can also write malformed output.
///
/// Double quotes, so a format may still interpolate (`PWD=$(pwd)`); the
/// backslashes reach printf either way and it decodes them. The delimiter is
/// `\000` because this `printf` treats a bare `\0` as two characters.
fn dumping_hook(format: &str) -> Vec<String> {
    vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!("printf \"{format}\""),
    ]
}

/// Run with the documented bounds.
async fn launch(
    cwd: &Path,
    hook: Option<&[String]>,
    host: &EnvSnapshot,
) -> Result<EnvSnapshot, LaunchError> {
    environment_at(cwd, hook, host, std::future::pending()).await
}

/// Run with bounds a test can wait for.
async fn launch_with(
    cwd: &Path,
    hook: Option<&[String]>,
    host: &EnvSnapshot,
    limits: HookLimits,
) -> Result<EnvSnapshot, LaunchError> {
    run_hook(cwd, hook, host, std::future::pending(), limits).await
}

/// Bounds short enough to keep a lifecycle test fast.
fn quick(timeout_ms: u64) -> HookLimits {
    HookLimits {
        timeout: Duration::from_millis(timeout_ms),
        max_output: HOOK_OUTPUT_LIMIT,
    }
}

/// Wait, bounded, for a marker to appear.
///
/// This is what makes a negative assertion downstream provable: concluding
/// "nothing wrote the marker" only means something once the process that would
/// have written it is known to have started. An elapsed time is not that
/// knowledge, so the subject announces itself and this waits for the
/// announcement.
async fn await_marker(path: &Path, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if path.exists() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what} never appeared at {}",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Long enough for a process polling for its gate every 50ms to notice.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(1500)).await;
}

/// An argv whose hook backgrounds a descendant **in its own process group**, and
/// prints `body` on the way out either immediately or once `gate` appears.
///
/// The descendant redirects its stdio, so it does not hold the hook's stdout open;
/// writes `ready`; then polls for `gate` before writing `marker`. The parent waits
/// for `ready` before it prints, so a caller that has returned from the launch
/// knows the descendant was alive and past the point where it could have been
/// missed.
///
/// Opening `gate` afterwards is what makes the negative assertion provable: a
/// descendant the group killed never reaches it, and one that survived writes
/// `marker`.
fn descendant_hook(
    scratch: &Scratch,
    name: &str,
    body: &str,
    ready: &Path,
    gate: &Path,
    marker: &Path,
    parent_waits: bool,
) -> Vec<String> {
    let parent = if parent_waits {
        // Still in flight while the caller decides what to do with it, which is
        // what a cancellation test needs to abort something.
        format!(
            "while [ ! -f {gate} ]; do sleep 0.05; done\n",
            gate = gate.display()
        )
    } else {
        String::new()
    };
    scratch.script(
        name,
        &format!(
            "/bin/sh -c 'exec >/dev/null 2>&1; : > {ready}; \
             while [ ! -f {gate} ]; do sleep 0.05; done; : > {marker}' &\n\
             while [ ! -f {ready} ]; do sleep 0.05; done\n\
             {parent}\
             printf \"{body}\"\n\
             exit 0\n",
            ready = ready.display(),
            gate = gate.display(),
            marker = marker.display(),
        ),
    )
}

/// A hook that prints and exits as soon as its descendant has announced itself.
fn exiting_hook(
    scratch: &Scratch,
    name: &str,
    body: &str,
    ready: &Path,
    gate: &Path,
    marker: &Path,
) -> Vec<String> {
    descendant_hook(scratch, name, body, ready, gate, marker, false)
}

/// The same, except the hook stays in flight until `gate` appears.
fn waiting_hook(
    scratch: &Scratch,
    name: &str,
    body: &str,
    ready: &Path,
    gate: &Path,
    marker: &Path,
) -> Vec<String> {
    descendant_hook(scratch, name, body, ready, gate, marker, true)
}
