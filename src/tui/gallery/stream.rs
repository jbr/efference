//! A scripted emitter for the streaming render path (`efference gallery --stream`).
//!
//! The gallery runs the real [`ui_loop`](crate::tui::ui_loop) over a fixed offline
//! log, holding the [`TuiEvent`] sender for the session's life. This pushes live
//! events into that same channel on a timer, so the behaviours that only exist at
//! animation rate — a tool call's JSON arriving fragment by fragment, a file's
//! contents accumulating, the handoff from live tail to committed entry — can be
//! watched and measured without a daemon or an upstream.
//!
//! Fragment size and interval are knobs because the interesting cases sit at both
//! ends: small fragments at the frame interval are what a real stream looks like,
//! and large ones arriving faster than the 30 ms paint tick are what provokes the
//! per-frame costs (backlog/tui-streaming-lifecycle.md).

#[cfg(test)]
mod test;

use std::time::Duration;

use crossbeam_channel::Sender;

use super::{assistant, call, result};
use crate::log::WireState;
use crate::tui::TuiEvent;

/// A real source file, included at build time: the streaming render is about
/// syntax highlighting under a growing buffer, and generated repetitive code lexes
/// nothing like a file with strings, comments, lifetimes, and nested blocks in it.
const WRITTEN_FILE: &str = include_str!("../render_code_block.rs");

/// The path the demo claims to be writing. Named separately from
/// [`WRITTEN_FILE`] because the header's job is to show it *before* the contents
/// finish arriving, which is the property under test.
const WRITTEN_PATH: &str = "src/tui/render_code_block.rs";

/// How the scripted stream is paced.
#[derive(Debug, Clone, Copy)]
pub struct Script {
    /// Bytes per `tool_call_input` fragment.
    pub chunk: usize,
    /// Delay between fragments.
    pub interval: Duration,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            chunk: 24,
            interval: Duration::from_millis(16),
        }
    }
}

impl Script {
    /// Play the scripted scenes into `tx`, forever. Returns when the receiver is
    /// gone (the UI exited), so the caller can simply let the task run.
    pub(crate) async fn play(self, tx: Sender<TuiEvent>) {
        // Past the fixed gallery log's last seq, and monotonic across repeats so a
        // replayed scene appends rather than overwriting.
        let mut seq = 100;
        loop {
            for scene in [Scene::Write, Scene::Bash] {
                if self.play_scene(scene, &tx, &mut seq).await.is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }

    /// One scene: think, call, stream, commit. `Err(())` means the UI is gone.
    async fn play_scene(
        self,
        scene: Scene,
        tx: &Sender<TuiEvent>,
        seq: &mut u64,
    ) -> Result<(), ()> {
        let (name, prose, input, output) = scene.parts();

        // A thinking phase first, so the phase line's replacement by the tool call
        // is exercised rather than assumed.
        for word in "weighing the render path against the layout pass ".split_inclusive(' ') {
            self.send(tx, TuiEvent::ThinkingDelta(word.to_string()))
                .await?;
        }

        let call_id = format!("stream-{seq}");
        self.send(
            tx,
            TuiEvent::ToolCallStart {
                name: name.to_string(),
            },
        )
        .await?;
        for fragment in fragments(&input, self.chunk) {
            self.send(
                tx,
                TuiEvent::ToolCallInput {
                    fragment: fragment.to_string(),
                },
            )
            .await?;
        }

        // The committed assistant turn supersedes the live phase; the result
        // follows, so the handoff from streaming render to committed render is
        // visible in both directions.
        *seq += 1;
        let input_value: serde_json::Value = serde_json::from_str(&input).expect("scripted JSON");
        self.send(
            tx,
            TuiEvent::Entry(assistant(
                *seq,
                prose,
                vec![call(name, &call_id, input_value)],
            )),
        )
        .await?;

        // A tool with streaming output streams it after the call commits, which is
        // where the tail-follow question lives for everything that isn't a write.
        for chunk in output.split_inclusive('\n') {
            self.send(
                tx,
                TuiEvent::ToolOutput {
                    call_id: call_id.clone(),
                    chunk: chunk.to_string(),
                },
            )
            .await?;
        }

        *seq += 1;
        self.send(
            tx,
            TuiEvent::Entry(result(*seq, &call_id, &output, false, WireState::Raw)),
        )
        .await
    }

    /// Send one event and wait out the interval. `Err(())` when the UI has gone —
    /// the receiver is dropped on exit, so a failed send is how this task learns to
    /// stop rather than spinning against a dead channel.
    async fn send(self, tx: &Sender<TuiEvent>, event: TuiEvent) -> Result<(), ()> {
        tx.send(event).map_err(|_| ())?;
        tokio::time::sleep(self.interval).await;
        Ok(())
    }
}

/// Split `s` into `chunk`-byte pieces on char boundaries — the API splits a tool
/// call's JSON at arbitrary points, so the demo must too.
fn fragments(s: &str, chunk: usize) -> Vec<&str> {
    let chunk = chunk.max(1);
    let mut out = Vec::new();
    let mut at = 0;
    while at < s.len() {
        let mut end = (at + chunk).min(s.len());
        while !s.is_char_boundary(end) {
            end += 1;
        }
        out.push(&s[at..end]);
        at = end;
    }
    out
}

/// The scripted scenes.
#[derive(Clone, Copy)]
enum Scene {
    /// The target interaction: a long file write, whose path arrives long before
    /// its contents finish.
    Write,
    /// A call with a short input and a long *output* — the other half of the
    /// streaming lifecycle, where the tail-follow question is about tool output.
    Bash,
}

impl Scene {
    /// `(tool name, assistant prose, JSON input, tool output)`.
    fn parts(self) -> (&'static str, &'static str, String, String) {
        match self {
            Scene::Write => (
                "write",
                "Writing the code block renderer.",
                object(&[
                    ("intent", json_str("write the code block renderer")),
                    ("path", json_str(WRITTEN_PATH)),
                    ("contents", json_str(WRITTEN_FILE)),
                ]),
                format!("Wrote {} bytes to {WRITTEN_PATH}", WRITTEN_FILE.len()),
            ),
            Scene::Bash => (
                crate::bash::NAME,
                "Running the test suite.",
                object(&[
                    ("intent", json_str("run the test suite")),
                    ("command", json_str("cargo test --all")),
                ]),
                (1..=120)
                    .map(|n| format!("test tui::render::case_{n} ... ok\n"))
                    .collect(),
            ),
        }
    }
}

/// A JSON object built **in the given key order**, which `serde_json::Value` cannot
/// do — it sorts keys (deliberately, for the wire's byte stability,
/// docs/log-and-projection.md). Arrival order is the whole subject here: a model
/// emits `path` before `contents`, and a demo that emitted them sorted would test
/// the one case the render can't do anything useful with.
fn object(pairs: &[(&str, String)]) -> String {
    let body = pairs
        .iter()
        .map(|(k, v)| format!("{}:{v}", json_str(k)))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{body}}}")
}

/// One JSON string literal, escaped as the wire would carry it.
fn json_str(s: &str) -> String {
    serde_json::to_string(s).expect("a string always serializes")
}
