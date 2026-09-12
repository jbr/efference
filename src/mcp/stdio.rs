//! The stdio transport (docs/tools.md "connected providers"): spawn the
//! declared command and speak newline-delimited JSON-RPC over its pipes.
//!
//! Framing only. Which message to send, what an arriving message means, and
//! when an exchange is finished all belong to `mcplease::client`
//! (docs/tools.md "the client protocol"); this module reads and writes lines.
//!
//! The framing lives in [`JsonRpcPipe`], generic over the reader/writer so the
//! loopback tests exercise it over an in-memory duplex instead of a
//! subprocess. Requests are strictly serialized — the agent loop issues one
//! tool call at a time — so there is no demux task: one pipe carries every
//! exchange, and the caller reads frames until the one it wants arrives.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;

use anyhow::Context;
use mcplease::types::JsonRpcMessage;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// The pipe to a spawned child's stdin/stdout.
pub(super) type ChildPipe = JsonRpcPipe<BufReader<ChildStdout>, ChildStdin>;

/// A spawned stdio server. The child is killed on drop (session teardown or a
/// failed mount); its stderr — the spec's designated logging channel — drains
/// to our `log` at debug level, never to the model.
pub struct StdioTransport {
    pipe: ChildPipe,
    // Held for lifetime (kill_on_drop), not otherwise consulted.
    _child: Child,
}

impl StdioTransport {
    pub fn spawn(
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        cwd: &Path,
        child_env: &crate::env_table::ChildEnv,
    ) -> anyhow::Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Its own group so a Ctrl-C at the daemon's terminal doesn't
            // reach the server (mirrors bash.rs).
            .process_group(0)
            .kill_on_drop(true);
        // The table first, then the stanza's own `env` — a server explicitly
        // handed a variable in its own config is being given it deliberately, and
        // the table's blanket strip should not silently undo that (docs/env.md).
        child_env.apply_tokio(&mut cmd);
        cmd.envs(env);
        let mut child = crate::spawn::tokio(&mut cmd)
            .with_context(|| format!("could not spawn `{command}`"))?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let command = command.to_string();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log::debug!("mcp `{command}` stderr: {line}");
            }
        });
        Ok(Self {
            pipe: JsonRpcPipe::new(BufReader::new(stdout), stdin),
            _child: child,
        })
    }

    pub(super) fn pipe(&mut self) -> &mut ChildPipe {
        &mut self.pipe
    }
}

/// Newline-delimited JSON-RPC over any async read/write pair.
pub(super) struct JsonRpcPipe<R, W> {
    reader: R,
    writer: W,
}

impl<R, W> JsonRpcPipe<R, W>
where
    R: AsyncBufRead + AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    pub(super) fn new(reader: R, writer: W) -> Self {
        Self { reader, writer }
    }

    /// Write one message as a line.
    pub(super) async fn send(&mut self, message: &JsonRpcMessage) -> anyhow::Result<()> {
        let mut line = serde_json::to_string(message)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.flush().await?;
        Ok(())
    }

    /// The next message the server sent. Blank and unparseable lines are
    /// framing noise and are skipped; EOF is an error, because the server
    /// exited or closed its end and no message is ever coming.
    pub(super) async fn next_frame(&mut self) -> anyhow::Result<JsonRpcMessage> {
        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line).await? == 0 {
                anyhow::bail!("server closed the connection");
            }
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(&line) {
                Ok(message) => return Ok(message),
                Err(e) => log::debug!("ignoring unparseable line from server: {e}"),
            }
        }
    }
}

#[cfg(test)]
mod test;
