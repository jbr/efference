//! What a scripted upstream sends back for one request (`upstream.rs`), and the
//! turnstile a test holds an answer at.

use futures_lite::AsyncRead;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::sync::Semaphore;
use trillium::{Body, Conn};

/// One scripted answer: a status, headers and a body, which can wait at a
/// [`Turnstile`] before anything is sent, or stay open once its body is out.
#[derive(Clone, Debug)]
pub(crate) struct Canned {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
    /// Send the body as chunks and never end it: a stream that neither finalizes nor
    /// closes.
    open: bool,
    /// Send nothing, ever.
    silent: bool,
    held: Option<Turnstile>,
}

impl Canned {
    /// A 200 event stream carrying `body`.
    pub(crate) fn stream(body: impl Into<String>) -> Self {
        Self::typed(200, "text/event-stream", body)
    }

    /// `status` with the JSON `body`: a refusal, or a ping's answer, which is not
    /// streamed.
    pub(crate) fn json(status: u16, body: impl Into<String>) -> Self {
        Self::typed(status, "application/json", body)
    }

    /// No answer at all: the request stays open until the client gives up on it.
    pub(crate) fn silent() -> Self {
        Self {
            silent: true,
            ..Self::typed(200, "text/event-stream", "")
        }
    }

    fn typed(status: u16, content_type: &str, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), content_type.into())],
            body: body.into(),
            open: false,
            silent: false,
            held: None,
        }
    }

    /// This answer with the header `name` set to `value` as well.
    pub(crate) fn with_header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// This answer with no `content-type` header.
    pub(crate) fn without_content_type(mut self) -> Self {
        self.headers
            .retain(|(name, _)| !name.eq_ignore_ascii_case("content-type"));
        self
    }

    /// This answer's body sent as chunks and then left open: the response neither
    /// finalizes nor closes until the client lets it go.
    pub(crate) fn left_open(mut self) -> Self {
        self.open = true;
        self
    }

    /// This answer held, status line and all, until `turnstile` lets it through.
    pub(crate) fn held(mut self, turnstile: &Turnstile) -> Self {
        self.held = Some(turnstile.clone());
        self
    }

    /// Send this answer on `conn`.
    pub(super) async fn answer(self, conn: Conn) -> Conn {
        if let Some(turnstile) = &self.held {
            turnstile.pass().await;
        }
        if self.silent {
            std::future::pending::<()>().await;
        }
        let conn = self
            .headers
            .into_iter()
            .fold(conn.with_status(self.status), |conn, (name, value)| {
                conn.with_response_header(name, value)
            });
        let body = if self.open {
            Body::new_streaming(
                OpenEnded {
                    bytes: self.body.into_bytes(),
                    sent: 0,
                },
                None,
            )
        } else {
            Body::from(self.body)
        };
        conn.with_body(body).halt()
    }
}

/// Holds the answers that wait at it ([`Canned::held`]) until the test lets them
/// through: one at a time with [`admit`](Self::admit), or every one, now and later,
/// with [`open`](Self::open). Clones are the same turnstile.
#[derive(Clone, Debug)]
pub(crate) struct Turnstile(Arc<Semaphore>);

impl Default for Turnstile {
    fn default() -> Self {
        Self(Arc::new(Semaphore::new(0)))
    }
}

impl Turnstile {
    /// A turnstile holding every answer that reaches it.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Let one held answer through: the one that has waited longest, or else the next
    /// to arrive.
    pub(crate) fn admit(&self) {
        self.0.add_permits(1);
    }

    /// Let every held answer through, now and later.
    pub(crate) fn open(&self) {
        // A closed semaphore answers every acquire at once, with the error `pass` reads
        // as passage.
        self.0.close();
    }

    async fn pass(&self) {
        if let Ok(permit) = self.0.acquire().await {
            permit.forget();
        }
    }
}

/// A response body that yields its bytes and then never ends.
struct OpenEnded {
    bytes: Vec<u8>,
    sent: usize,
}

impl AsyncRead for OpenEnded {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let rest = &self.bytes[self.sent..];
        if rest.is_empty() {
            // Nothing wakes this read: the response stays open until the client drops
            // it or the test's runtime ends.
            return Poll::Pending;
        }
        let n = rest.len().min(buf.len());
        buf[..n].copy_from_slice(&rest[..n]);
        self.sent += n;
        Poll::Ready(Ok(n))
    }
}
