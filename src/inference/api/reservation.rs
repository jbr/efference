//! The per-request output reservation — what goes on the wire as `max_tokens`
//! (docs/transport.md).
//!
//! `max_tokens` is a reservation against the context window, not a prediction
//! of the response: the provider sets the tokens aside before generating and
//! rejects a request whose prompt plus reservation does not fit. So the value
//! sent is the model's per-turn output cap, shrunk to whatever the window can
//! spare after the prompt. The prompt is estimated from the last response's
//! exact usage plus the bytes appended since, and a refusal naming the window —
//! a 400 at open, or an `error` event inside a 200 stream — is the backstop: the
//! reservation is halved and the request retried, once per halving down to
//! [`RESERVATION_MIN`].

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

/// The reservation sent when the capability source reports no output cap
/// (docs/transport.md). A number is unavoidable: Messages **requires**
/// `max_tokens`, so an unreported cap cannot be answered by omitting the field,
/// and an asymmetry between the dialects would cost more than one rule.
///
/// 65536, chosen by how many models **accept** it rather than by how many
/// declare it. Surveyed across the 327 non-variant models OpenRouter reports a
/// cap for: 61% sit at or above 65536, against 38% for 128000 — which is the
/// single most common *value* but a popular ceiling rather than a popular floor,
/// so defaulting there is refused by two thirds of the catalog. The mode is the
/// wrong statistic; the survival function is the right one. 65536 is also the
/// median, and the minimum across the models this project runs, so in practice it
/// is refused by none of them.
///
/// The floor matters more than the ceiling here, which is why this is not simply
/// set as high as [`Reservation::clamp_to`] can recover from. A cap set too low is
/// not a short turn: on a model whose reasoning is mandatory the whole budget can
/// go to thinking and the response arrives **empty**, reading as a broken model
/// rather than as a misconfigured reservation. A cap set too high costs one
/// refused request, once — [`Reservation::learned_cap`] keeps it from recurring —
/// and gains nothing, since a turn wanting more than 65536 output tokens is not a
/// case this harness has. So the asymmetry runs one way only down to the empty-
/// response cliff, and flat above it.
pub const UNKNOWN_OUTPUT_CAP: u64 = 65_536;

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
/// each response, and the reservation it last sent — the number the two 400
/// retries work from, [`Self::clamp_to`] comparing against it and [`Self::halve`]
/// halving it (docs/transport.md).
#[derive(Debug, Default)]
pub struct Reservation {
    calibration: Option<Calibration>,
    last_sent: Option<u64>,
    /// A ceiling the provider stated when it refused one ([`Self::clamp_to`]),
    /// kept for the rest of the session.
    ///
    /// Without this the correction would last exactly one request: [`Self::compute`]
    /// reads the client's configured cap fresh each time, so the next turn would
    /// send the refused number again and buy the same 400. A wrong guess would
    /// then cost a wasted round trip *per turn* rather than once — which is what
    /// makes a guessed cap ([`UNKNOWN_OUTPUT_CAP`]) affordable at all.
    learned_cap: Option<u64>,
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
        // A ceiling this endpoint has already refused above is not offered again.
        let cap = self.learned_cap.map_or(cap, |learned| cap.min(learned));
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

    /// The reservation to retry with when the provider **named** the cap it
    /// accepts (`max_tokens: 65536 > 8192, the maximum`): that number, rather
    /// than a search toward it. `None` when the stated maximum is not below what
    /// was last sent — that is not a reduction, so the rejection was about
    /// something else and the caller falls back to [`Self::halve`].
    ///
    /// Worth parsing rather than halving blind: halving from
    /// [`UNKNOWN_OUTPUT_CAP`] to [`RESERVATION_MIN`] is six round trips against a
    /// small model, and each one is a full request. A stated maximum converges in
    /// one.
    pub fn clamp_to(&mut self, stated_max: u64) -> Option<u64> {
        let clamped = stated_max.max(RESERVATION_MIN);
        if clamped >= self.last_sent? {
            return None;
        }
        self.learned_cap = Some(clamped);
        self.last_sent = Some(clamped);
        Some(clamped)
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
}

/// The reservation that fits: the cap, or the window less the prompt and the
/// margin, whichever is smaller, never below [`RESERVATION_MIN`].
fn fit(cap: u64, window: u64, prompt: u64) -> u64 {
    let spare = window.saturating_sub(prompt + MARGIN);
    cap.min(spare).max(RESERVATION_MIN)
}

/// Whether a refusal's body is the provider saying prompt plus `max_tokens`
/// overran the window — the failure a smaller reservation fixes. The body is a
/// 4xx's, or the text of an in-stream `error` event on a 200
/// (`Client::in_stream_reservation_retry`).
///
/// Matched on the known phrasings: Anthropic's "input length and `max_tokens`
/// exceed context limit", OpenRouter's "maximum context length is N tokens.
/// However, you requested about M tokens", and DeepInfra's (through OpenRouter)
/// "maximum context length is N tokens. However, you requested M output tokens
/// and your prompt contains at least P input tokens". A prompt that is too long
/// on its own says so differently ("prompt is too long") and is not matched: no
/// reservation fixes that.
pub fn is_window_overflow(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    body.contains("context") && (body.contains("max_tokens") || body.contains("you requested"))
}

/// Why a 4xx says the **output reservation** is too large, as distinct from the
/// prompt-plus-reservation overflow [`is_window_overflow`] matches. The two want
/// different retries: an overflowing window is answered by halving toward a fit,
/// while a rejected cap has one right answer the provider usually states.
///
/// Distinguished from window overflow by the absence of the window: every
/// overflow phrasing names the *context*, and a cap rejection names only the
/// output parameter and its ceiling. `is_window_overflow` is checked first, so
/// this arm sees only what that one declined.
///
/// The stated maximum is read from the two shapes providers use for it — the
/// comparison (`max_tokens: N > M`) and the phrase (`at most M`). A rejection
/// that names no number is still recognized, as [`OutputCapRejection::Unstated`],
/// so the caller halves instead of treating a known-fixable 400 as terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputCapRejection {
    /// The provider named the ceiling it accepts.
    Max(u64),
    /// The reservation was refused without a usable number.
    Unstated,
}

pub fn output_cap_rejection(body: &str) -> Option<OutputCapRejection> {
    let body = body.to_ascii_lowercase();
    // The window's business, not this one's.
    if body.contains("context") {
        return None;
    }
    let names_output_cap = ["max_tokens", "max_output_tokens", "max_completion_tokens"]
        .iter()
        .any(|name| body.contains(name));
    let names_a_ceiling = ["maximum", "at most", "too large", "exceeds", "less than"]
        .iter()
        .any(|phrase| body.contains(phrase));
    if !(names_output_cap && names_a_ceiling) {
        return None;
    }
    Some(
        first_number_after(&body, "<=")
            .or_else(|| first_number_after(&body, ">"))
            .or_else(|| first_number_after(&body, "at most"))
            .map_or(OutputCapRejection::Unstated, OutputCapRejection::Max),
    )
}

/// The first run of digits following `marker`, with `_` and `,` separators
/// tolerated because providers write both.
fn first_number_after(body: &str, marker: &str) -> Option<u64> {
    let tail = &body[body.find(marker)? + marker.len()..];
    let digits: String = tail
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '_' || *c == ',')
        .filter(char::is_ascii_digit)
        .collect();
    digits.parse().ok().filter(|n| *n > 0)
}

#[cfg(test)]
mod test;
