//! A scripted upstream (docs/transport.md): a provider stand-in whose answers a test
//! writes as a script over arrival order. It is a trillium handler, served in process
//! through `trillium_testing::ServerConnector` to the client the endpoints it hands out
//! carry, so a request never leaves the test's runtime and no two tests share a port;
//! trillium's own HTTP/1.1 server reads each request whole. Every request is recorded —
//! method, target, headers, body — before it is answered, and each arrival raises a
//! signal a wait arms (`Session::wait_until_with`).
//!
//! A path that resolves its HTTP client from configuration rather than from an
//! [`Endpoint`] — a described connection's base URL, an MCP server's — reaches the same
//! handler over a loopback listener ([`ScriptedUpstream::bind`]).

mod canned;
mod messages;

pub(crate) use canned::{Canned, Turnstile};
pub(crate) use messages::{end_turn, events, tool_call};

use crate::inference::api::Endpoint;
use crate::record::log::Provider;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio::sync::futures::Notified;
use trillium::{Conn, Handler, Headers, KnownHeaderName};
use trillium_testing::ServerConnector;
use trillium_tokio::Swansong;

/// The origin every endpoint an upstream hands out names. Under `.test`, which is
/// reserved for testing (RFC 2606), so a request that went to the network instead of
/// the upstream fails to resolve rather than finding a host.
const ORIGIN: &str = "http://upstream.test";

/// The characters of one request's body that [`ScriptedUpstream::describe`] prints.
const BRIEF: usize = 160;

/// A provider stand-in answering by arrival order (the module docs).
pub(crate) struct ScriptedUpstream {
    shared: Arc<Shared>,
    /// The in-process client the endpoints carry ([`Endpoint::with_http`]).
    http: trillium_client::Client,
    /// Shut down when the upstream drops, which closes a listener [`Self::bind`] opened.
    swansong: Swansong,
}

/// What the handler and the test share.
struct Shared {
    script: Box<dyn Fn(usize) -> Canned + Send + Sync>,
    /// Whether a title ask goes to the script like any request, or is answered aside.
    scripts_title_asks: bool,
    /// Every request the script was asked to answer, in arrival order.
    requests: Mutex<Vec<Request>>,
    /// Raised after each request is recorded.
    arrived: Notify,
}

/// One request as the upstream read it.
#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub(crate) method: String,
    /// The path and the query.
    pub(crate) target: String,
    pub(crate) headers: Headers,
    pub(crate) body: String,
}

impl Request {
    /// The value of the header `name`, matched without regard to case.
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.get_str(name)
    }

    /// The body, read as JSON.
    pub(crate) fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("the request body is not JSON ({e}): {}", self.body))
    }
}

impl ScriptedUpstream {
    /// An upstream answering the `n`th request it is sent, counting from zero, with
    /// `script(n)`. A title ask (docs/session-title.md) rides its own detached task and
    /// lands between a session's requests, so it is answered aside with [`end_turn`],
    /// whose text line the title takes, and is neither recorded nor counted: the script
    /// numbers the session's own requests.
    pub(crate) fn new(script: impl Fn(usize) -> Canned + Send + Sync + 'static) -> Self {
        Self::serving(script, false)
    }

    /// An upstream answering each request with the next of `answers`. A request past
    /// the last has no answer, and the upstream panics naming it.
    pub(crate) fn answering(answers: impl IntoIterator<Item = Canned>) -> Self {
        let answers: Vec<Canned> = answers.into_iter().collect();
        Self::new(move |n| {
            answers.get(n).cloned().unwrap_or_else(|| {
                panic!(
                    "the upstream was sent request {n}, and its script answers {}",
                    answers.len()
                )
            })
        })
    }

    /// [`new`](Self::new) for a test of the title ask itself: a title ask goes to the
    /// script like any other request.
    pub(crate) fn scripting_title_asks(
        script: impl Fn(usize) -> Canned + Send + Sync + 'static,
    ) -> Self {
        Self::serving(script, true)
    }

    fn serving(
        script: impl Fn(usize) -> Canned + Send + Sync + 'static,
        scripts_title_asks: bool,
    ) -> Self {
        let shared = Arc::new(Shared {
            script: Box::new(script),
            scripts_title_asks,
            requests: Mutex::default(),
            arrived: Notify::new(),
        });
        // The connector spawns each connection's server task on the runtime current
        // here, the test's own.
        let connector = ServerConnector::new(Serve(Arc::clone(&shared)));
        Self {
            http: crate::platform::http::client_over(connector),
            shared,
            swansong: Swansong::new(),
        }
    }

    /// An endpoint for `provider` whose requests go to this upstream at `path`.
    pub(crate) fn endpoint(&self, provider: Provider, path: &str) -> Endpoint {
        Endpoint::at(provider, Self::url(path)).with_http(self.http())
    }

    /// The URL of `path` at the origin every upstream answers on, for an endpoint built
    /// another way. A URL names no upstream: a request reaches one through its client,
    /// [`Self::http`].
    pub(crate) fn url(path: &str) -> String {
        format!("{ORIGIN}{path}")
    }

    /// The client that reaches this upstream in process.
    pub(crate) fn http(&self) -> trillium_client::Client {
        self.http.clone()
    }

    /// Serve this upstream on a loopback port as well, for a client resolved from
    /// configuration rather than handed an endpoint, and answer the port's origin.
    /// Each response there closes its connection, so the process-wide client pools
    /// none: the port goes back to the kernel when the test ends, and a later test bound
    /// to it must not be handed a connection whose runtime is gone (`http.rs`).
    pub(crate) fn bind(&self) -> String {
        let listener =
            std::net::TcpListener::bind(("127.0.0.1", 0)).expect("a loopback port to bind");
        let port = listener.local_addr().expect("the bound address").port();
        listener
            .set_nonblocking(true)
            .expect("a non-blocking listener");
        let listener = tokio::net::TcpListener::from_std(listener).expect("a tokio listener");
        let close = |conn: Conn| async move {
            conn.with_response_header(KnownHeaderName::Connection, "close")
        };
        trillium_tokio::config()
            .without_signals()
            .with_swansong(self.swansong.clone())
            .listeners()
            .bind_server(listener)
            .spawn((close, Serve(Arc::clone(&self.shared))));
        format!("http://127.0.0.1:{port}")
    }

    /// Every request the script was asked to answer, in arrival order.
    pub(crate) fn requests(&self) -> Vec<Request> {
        self.shared.requests.lock().unwrap().clone()
    }

    /// How many requests the script was asked to answer.
    pub(crate) fn count(&self) -> usize {
        self.shared.requests.lock().unwrap().len()
    }

    /// The signal the next recorded request raises. A wait creates it before it reads
    /// what the upstream holds, so an arrival in between still wakes it.
    pub(crate) fn arrived(&self) -> Notified<'_> {
        self.shared.arrived.notified()
    }

    /// What a wait that gave up prints about the upstream: each request's method,
    /// target, and the start of its body.
    pub(crate) fn describe(&self) -> String {
        let requests = self.shared.requests.lock().unwrap();
        let lines: Vec<String> = requests
            .iter()
            .enumerate()
            .map(|(n, request)| {
                let mut body: String = request.body.chars().take(BRIEF).collect();
                if request.body.chars().nth(BRIEF).is_some() {
                    body.push('…');
                }
                format!("  [{n}] {} {} {body}", request.method, request.target)
            })
            .collect();
        format!("{} requests:\n{}", requests.len(), lines.join("\n"))
    }
}

impl Drop for ScriptedUpstream {
    fn drop(&mut self) {
        drop(self.swansong.shut_down());
    }
}

impl Shared {
    /// Record `request` and wake whoever waits on an arrival; answers its index.
    fn record(&self, request: Request) -> usize {
        let n = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request);
            requests.len() - 1
        };
        self.arrived.notify_waiters();
        n
    }
}

/// The upstream as a trillium handler.
#[derive(Clone)]
struct Serve(Arc<Shared>);

impl Handler for Serve {
    async fn run(&self, mut conn: Conn) -> Conn {
        // The whole body is read before anything is answered: a request is recorded as
        // it was sent, and a refusal never leaves a body unread.
        let Ok(body) = conn.request_body_string().await else {
            // The client went away mid-body; there is nobody to answer.
            return conn.with_status(400).halt();
        };
        let request = Request {
            method: conn.method().to_string(),
            target: conn.path_and_query().to_string(),
            headers: conn.request_headers().clone(),
            body,
        };
        let canned = if !self.0.scripts_title_asks && messages::is_title_ask(&request.body) {
            Canned::stream(end_turn())
        } else {
            let n = self.0.record(request);
            (self.0.script)(n)
        };
        canned.answer(conn).await
    }
}
