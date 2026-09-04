//! The per-request output reservation — what goes on the wire as `max_tokens`
//! (docs/transport.md).
//!
//! `max_tokens` is a reservation against the context window, not a prediction
//! of the response: the provider sets the tokens aside before generating and
//! rejects a request whose prompt plus reservation does not fit. So the value
//! sent is the model's per-turn output cap, shrunk to whatever the window can
//! spare after the prompt. The prompt is estimated from the last response's
//! exact usage plus the bytes appended since, and a 400 naming the window is
//! the backstop: the reservation is halved and the request retried once.

use std::sync::{Arc, Mutex};

/// Bytes per token assumed for the part of the prompt no response has counted
/// yet. Measured 3.5–4.4 across the corpus's models; 3 is the conservative side
/// (it over-estimates the tail, which shrinks the reservation rather than the
/// window).
const TAIL_BYTES_PER_TOKEN: usize = 3;

/// Flat allowance between the estimated prompt and the reservation, covering
/// the estimate's rounding and the provider's own accounting of the envelope.
const MARGIN: u64 = 2048;

/// The smallest reservation ever sent when the window is nearly full. Below
/// this the request is going to fail either way — on the reservation or on
/// the prompt — and a tiny cap cuts a turn mid-call for nothing.
const RESERVATION_MIN: u64 = 1024;

/// What the last successful response taught about the prompt: the provider's
/// exact prompt-token count for a body of a known byte size. The next request's
/// prompt is that count plus the bytes it differs by, at
/// [`TAIL_BYTES_PER_TOKEN`].
#[derive(Debug, Clone, Copy, Default)]
struct Calibration {
    prompt_tokens: u64,
    body_bytes: usize,
}

/// The reservation's shared state: the calibration the client writes after
/// each response, and the reservation it last sent — what `GET /state`
/// subtracts from the window to report the limit the session will actually
/// hit (docs/transport.md).
#[derive(Debug, Default)]
pub struct Reservation {
    calibration: Option<Calibration>,
    last_sent: Option<u64>,
}

/// Shared between the client that writes it and the daemon view that reads it,
/// same shape as the upstream-failure cell (`api/diagnostics.rs`).
pub type ReservationState = Arc<Mutex<Reservation>>;

pub fn reservation_state() -> ReservationState {
    Arc::new(Mutex::new(Reservation::default()))
}

impl Reservation {
    /// The `max_tokens` to send for a body of `body_bytes` bytes, given the
    /// model's output cap and the window. Without a window (no capability
    /// lookup — the spotter's client, tests) the cap is sent as is.
    pub fn compute(&mut self, cap: u64, context_length: Option<u64>, body_bytes: usize) -> u64 {
        let reserved = match context_length {
            None => cap,
            Some(window) => {
                let prompt = self.estimate_prompt(body_bytes);
                fit(cap, window, prompt)
            }
        };
        self.last_sent = Some(reserved);
        reserved
    }

    /// Estimated prompt tokens for a body of `body_bytes` bytes: the calibrated
    /// count plus the byte difference at the tail ratio, or the whole body at
    /// that ratio when nothing has been counted yet.
    fn estimate_prompt(&self, body_bytes: usize) -> u64 {
        match self.calibration {
            Some(Calibration {
                prompt_tokens,
                body_bytes: known,
            }) => {
                let grown = body_bytes.saturating_sub(known) / TAIL_BYTES_PER_TOKEN;
                let shrunk = known.saturating_sub(body_bytes) / TAIL_BYTES_PER_TOKEN;
                (prompt_tokens + grown as u64).saturating_sub(shrunk as u64)
            }
            None => (body_bytes / TAIL_BYTES_PER_TOKEN) as u64,
        }
    }

    /// Record a response's exact prompt count for the body that produced it.
    pub fn calibrate(&mut self, prompt_tokens: u64, body_bytes: usize) {
        self.calibration = Some(Calibration {
            prompt_tokens,
            body_bytes,
        });
    }

    /// The reservation a window-overflow retry sends: half the last one, or
    /// `None` when it is already at the floor and a retry cannot help.
    pub fn halve(&mut self) -> Option<u64> {
        let last = self.last_sent?;
        if last <= RESERVATION_MIN {
            return None;
        }
        let halved = (last / 2).max(RESERVATION_MIN);
        self.last_sent = Some(halved);
        Some(halved)
    }

    /// The reservation most recently sent, if any request has gone out.
    pub fn last_sent(&self) -> Option<u64> {
        self.last_sent
    }
}

/// The reservation that fits: the cap, or the window less the prompt and the
/// margin, whichever is smaller, never below [`RESERVATION_MIN`].
fn fit(cap: u64, window: u64, prompt: u64) -> u64 {
    let spare = window.saturating_sub(prompt + MARGIN);
    cap.min(spare).max(RESERVATION_MIN)
}

/// Whether a 4xx body is the provider refusing the request because prompt plus
/// `max_tokens` overran the window — the failure a smaller reservation fixes.
/// Matched on the two known phrasings: Anthropic's "input length and
/// `max_tokens` exceed context limit" and OpenRouter's "maximum context length
/// is N tokens. However, you requested about M tokens". A prompt that is too
/// long on its own says so differently ("prompt is too long") and is not
/// matched: no reservation fixes that.
pub fn is_window_overflow(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    body.contains("context")
        && (body.contains("max_tokens") || body.contains("you requested about"))
}

#[cfg(test)]
mod test;
