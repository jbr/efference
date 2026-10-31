//! The mount's status streams (docs/operating.md): one WebSocket per agent, served by
//! an `efference mcp` mount on loopback for a client that turns each frame into a
//! notification (Claude Code's `Monitor`).
//!
//! The server binds `127.0.0.1:0` when the first stream is asked for and serves until the
//! mount exits. An agent's stream is the path of one random token, minted at the first
//! request for that agent and reused after. Any other path is a 404, and a request to a
//! stream's path that is not a WebSocket upgrade is a 426. Each connection follows the
//! agent's `status` topic with its own follower (`watch.rs`): the first frame is the
//! status at connect, then one frame per new decision point by `wait`'s key, which the
//! connection records for itself, so `wait` and the stream never consume each other's
//! statuses. An end is sent as a frame and closes with 1000; a host that can no longer be
//! listed closes with 1011.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_lite::StreamExt;
use tokio::task::AbortHandle;
use trillium::{Conn, Status};
use trillium_tokio::Swansong;
use trillium_websockets::tungstenite::protocol::CloseFrame;
use trillium_websockets::tungstenite::protocol::frame::coding::CloseCode;
use trillium_websockets::{Message, WebSocket, WebSocketConfig, WebSocketConn};

use super::status::operator_reader;
use super::watch::{Followed, Mark, ended, follow, mark};
use crate::client::Client;
use crate::daemon::status::render;
use crate::protocol::status::StatusView;

/// A token's random bytes: 256 bits, 43 characters of URL-safe base64.
const TOKEN_BYTES: usize = 32;

/// How long a stream that sent its close waits for the client's reply before it drops
/// the connection.
const CLOSE_GRACE: Duration = Duration::from_secs(5);

/// The largest message a client may send. A stream takes no input, so this only bounds
/// what a client can make the mount buffer.
const MAX_INBOUND: usize = 4096;

/// The close reason when the agent ended.
const ENDED: &str = "the agent ended";

/// The close reason when the host cannot be listed. A close reason is at most 123 bytes.
const UNREACHABLE: &str = "the efference host is unreachable, so this agent's status is unknown";

/// A mount's status streams, shared by every clone of its executor. The server is bound
/// on first use.
#[derive(Default)]
pub(super) struct Streams {
    server: tokio::sync::OnceCell<Server>,
}

impl Streams {
    /// `agent`'s stream URL, `ws://127.0.0.1:<port>/<token>`. The first call binds the
    /// server with `host` (a client at the host's socket root) as the host every stream
    /// follows. `reader` is the client a frame names "you", kept from the call that mints
    /// the agent's token.
    pub(super) async fn url(
        &self,
        host: &Client,
        agent: &str,
        reader: Option<&str>,
    ) -> Result<String> {
        let server = self
            .server
            .get_or_try_init(|| async { Server::bind(host.clone()) })
            .await?;
        let token = server.routes.token(agent, reader);
        Ok(format!("ws://127.0.0.1:{}/{token}", server.port))
    }
}

/// The bound server.
struct Server {
    port: u16,
    routes: Arc<Routes>,
    /// Shut down when the mount drops its streams, which ends every connection.
    swansong: Swansong,
}

impl Server {
    /// Bind `127.0.0.1:0` and serve. The listener is bound here rather than by the
    /// server builder so its port is known without awaiting the server's handle, whose
    /// wait is not `Send` and so cannot sit inside a tool call.
    fn bind(host: Client) -> Result<Self> {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
            .context("binding the status stream server on 127.0.0.1")?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let routes = Arc::new(Routes {
            host,
            streams: Mutex::default(),
        });
        let swansong = Swansong::new();
        // The token is the credential: a page that cannot know it gains nothing from
        // opening the socket, so the handshake's origin is not what admits it.
        let upgrade = WebSocket::new(stream)
            .allow_any_origin()
            .with_protocol_config(
                WebSocketConfig::default()
                    .max_message_size(Some(MAX_INBOUND))
                    .max_frame_size(Some(MAX_INBOUND)),
            )
            .required();
        trillium_tokio::config()
            // The mount's process owns its signals; this server ends with it.
            .without_signals()
            .with_swansong(swansong.clone())
            .listeners()
            .bind_server(listener)
            .spawn((route(Arc::clone(&routes)), upgrade));
        Ok(Self {
            port,
            routes,
            swansong,
        })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        drop(self.swansong.shut_down());
    }
}

/// What the server's handler reads: the host each stream follows, and each token's
/// stream.
struct Routes {
    host: Client,
    streams: Mutex<HashMap<String, Route>>,
}

/// One agent's stream.
#[derive(Clone)]
struct Route {
    agent: String,
    /// The client a frame names "you".
    reader: Option<String>,
}

impl Routes {
    fn streams(&self) -> MutexGuard<'_, HashMap<String, Route>> {
        self.streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `agent`'s token: the one already minted for it, else a new one.
    fn token(&self, agent: &str, reader: Option<&str>) -> String {
        let mut streams = self.streams();
        if let Some((token, _)) = streams.iter().find(|(_, route)| route.agent == agent) {
            return token.clone();
        }
        let mut bytes = [0u8; TOKEN_BYTES];
        rand::fill(&mut bytes);
        let token = URL_SAFE_NO_PAD.encode(bytes);
        streams.insert(
            token.clone(),
            Route {
                agent: agent.to_string(),
                reader: reader.map(str::to_string),
            },
        );
        token
    }
}

/// A connection's stream, carried from the route to the upgrade.
struct Following {
    host: Client,
    route: Route,
}

/// The handler in front of the upgrade: a path that names no stream is a 404, and a
/// stream's path carries what it follows on to the upgrade.
fn route(routes: Arc<Routes>) -> impl trillium::Handler {
    move |conn: Conn| {
        let routes = Arc::clone(&routes);
        async move {
            let found = conn
                .path()
                .strip_prefix('/')
                .and_then(|token| routes.streams().get(token).cloned());
            match found {
                Some(route) => conn.with_state(Following {
                    host: routes.host.clone(),
                    route,
                }),
                None => conn.with_status(Status::NotFound).halt(),
            }
        }
    }
}

/// Aborts the follower when the connection it feeds ends.
struct AbortOnDrop(AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// How a connection's stream ends.
enum Close {
    /// The agent ended: 1000.
    Ended,
    /// The host could not be listed: 1011.
    Unreachable,
}

/// One connection: the agent's status at connect, then each new decision point, then a
/// close. The client's frames are read only to answer its pings and hear its close.
async fn stream(mut conn: WebSocketConn) {
    let Some(Following { host, route }) = conn.take_state::<Following>() else {
        return;
    };
    let (sender, mut readings) = tokio::sync::mpsc::unbounded_channel();
    let agent = route.agent.clone();
    let follower = tokio::spawn(async move {
        follow(host, &agent, |followed| match sender.send(followed) {
            Ok(()) => ControlFlow::Continue(()),
            Err(_) => ControlFlow::Break(()),
        })
        .await;
    });
    let _follower = AbortOnDrop(follower.abort_handle());
    // The mark of the last frame sent; `None` before the first.
    let mut sent: Option<Option<Mark>> = None;
    let close = loop {
        tokio::select! {
            inbound = conn.next() => match inbound {
                Some(Ok(Message::Close(_)) | Err(_)) | None => return,
                Some(Ok(_)) => {}
            },
            reading = readings.recv() => {
                let view = match reading {
                    Some(Followed::Status(view)) => *view,
                    // The session left without a status saying it ended (one that did
                    // closed the stream); the follower ends only after this.
                    Some(Followed::Left) | None => {
                        if send(&mut conn, &ended(&route.agent), &route).await.is_err() {
                            return;
                        }
                        break Close::Ended;
                    }
                    Some(Followed::Unreachable) => break Close::Unreachable,
                };
                let now = mark(&view);
                let new = match &sent {
                    None => true,
                    Some(last) => now.is_some() && *last != now,
                };
                if !new {
                    continue;
                }
                if send(&mut conn, &view, &route).await.is_err() {
                    return;
                }
                let over = now == Some(Mark::Ended);
                sent = Some(now);
                if over {
                    break Close::Ended;
                }
            }
        }
    };
    let (code, reason) = match close {
        Close::Ended => (CloseCode::Normal, ENDED),
        Close::Unreachable => (CloseCode::Error, UNREACHABLE),
    };
    let frame = CloseFrame {
        code,
        reason: reason.into(),
    };
    if conn.send(Message::Close(Some(frame))).await.is_err() {
        return;
    }
    // The client answers a close with its own; the connection drops after it, or after
    // the grace period.
    let _ = tokio::time::timeout(CLOSE_GRACE, async {
        while let Some(Ok(message)) = conn.next().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
    })
    .await;
}

/// Send `view` as one frame, rendered for the mount as `status` renders it.
async fn send(
    conn: &mut WebSocketConn,
    view: &StatusView,
    route: &Route,
) -> trillium_websockets::Result<()> {
    let reader = operator_reader(view, route.reader.as_deref());
    conn.send_string(render(view, &reader, super::now())).await
}

#[cfg(test)]
mod test;
