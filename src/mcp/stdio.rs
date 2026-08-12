//! The stdio transport (docs/tools.md "connected providers"): spawn the
//! declared command and speak newline-delimited JSON-RPC over its pipes.
//!
//! The framing lives in [`JsonRpcPipe`], generic over the reader/writer so
//! the loopback tests exercise it over an in-memory duplex instead of a
//! subprocess. Requests are strictly serialized — the agent loop issues one
//! tool call at a time — so there is no demux task: a request writes one
//! line, then reads lines until its response id arrives, answering or
//! discarding whatever else the server sends in between (notifications, its
//! own requests back at us, stale responses to an abandoned call).

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;

use anyhow::Context;
use mcplease::types::{
    JsonRpcError, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, RequestId,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// A spawned stdio server. The child is killed on drop (session teardown or a
/// failed mount); its stderr — the spec's designated logging channel — drains
/// to our `log` at debug level, never to the model.
pub struct StdioTransport {
    pipe: JsonRpcPipe<BufReader<ChildStdout>, ChildStdin>,
    // Held for lifetime (kill_on_drop), not otherwise consulted.
    _child: Child,
}

impl StdioTransport {
    pub fn spawn(
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        cwd: &Path,
    ) -> anyhow::Result<Self> {
        let mut child = Command::new(command)
            .args(args)
            .envs(env)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Its own group so a Ctrl-C at the daemon's terminal doesn't
            // reach the server (mirrors bash.rs).
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
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

    pub async fn request(&mut self, method: &str, params: Option<Value>) -> anyhow::Result<Value> {
        self.pipe.request(method, params).await
    }

    pub async fn notify(&mut self, method: &str, params: Option<Value>) -> anyhow::Result<()> {
        self.pipe.notify(method, params).await
    }
}

/// Newline-delimited JSON-RPC over any async read/write pair.
pub(super) struct JsonRpcPipe<R, W> {
    reader: R,
    writer: W,
    next_id: i64,
}

impl<R, W> JsonRpcPipe<R, W>
where
    R: AsyncBufRead + AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    pub(super) fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            next_id: 0,
        }
    }

    /// Write one request, then read until its response arrives. Interleaved
    /// traffic is handled inline: notifications are logged and skipped,
    /// responses to other ids (a call the operator abandoned) are skipped,
    /// and server-initiated requests are answered — `ping` with an empty
    /// result, anything else with method-not-found (efference offers no
    /// client capabilities in the handshake, so a server asking anyway gets
    /// the honest refusal).
    pub(super) async fn request(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> anyhow::Result<Value> {
        self.next_id += 1;
        let id = RequestId::Integer(self.next_id);
        let request = JsonRpcRequest::new(id.clone(), method, params);
        self.send(&JsonRpcMessage::Request(request)).await?;
        loop {
            let line = self.read_line().await?;
            match serde_json::from_str::<JsonRpcMessage>(&line) {
                Ok(JsonRpcMessage::Response(response)) if response.id.as_ref() == Some(&id) => {
                    return response
                        .into_result()
                        .map_err(|e| anyhow::anyhow!("{method}: {e}"));
                }
                Ok(JsonRpcMessage::Response(stale)) => {
                    log::debug!("skipping response to abandoned request {:?}", stale.id);
                }
                Ok(JsonRpcMessage::Notification(notification)) => {
                    log::debug!("ignoring server notification {}", notification.method);
                }
                Ok(JsonRpcMessage::Request(request)) => {
                    let response = if request.method == "ping" {
                        JsonRpcResponse::success(request.id, json!({}))
                    } else {
                        JsonRpcResponse::error(
                            request.id,
                            JsonRpcError::method_not_found(&request.method),
                        )
                    };
                    self.send(&JsonRpcMessage::Response(response)).await?;
                }
                Err(e) => {
                    log::debug!("ignoring unparseable line from server: {e}");
                }
            }
        }
    }

    pub(super) async fn notify(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> anyhow::Result<()> {
        self.send(&JsonRpcMessage::Notification(JsonRpcNotification::new(
            method, params,
        )))
        .await
    }

    async fn send(&mut self, message: &JsonRpcMessage) -> anyhow::Result<()> {
        let mut line = serde_json::to_string(message)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.flush().await?;
        Ok(())
    }

    /// One non-empty line, or an error on EOF (the server exited or closed
    /// its end — either way the connection is gone).
    async fn read_line(&mut self) -> anyhow::Result<String> {
        loop {
            let mut line = String::new();
            let read = self.reader.read_line(&mut line).await?;
            if read == 0 {
                anyhow::bail!("server closed the connection");
            }
            if !line.trim().is_empty() {
                return Ok(line);
            }
        }
    }
}
