//! Asking the prose rung's model for a verdict (docs/permissions.md): the output cap and
//! the effort its requests carry, and which requests are sent again before the verdict
//! fails closed.

use std::sync::atomic::Ordering;
use std::time::Duration;

use super::*;
use crate::inference::api::{
    Completion, Error, FailureClass, ModelCapabilities, ToolChoice, error_class,
    is_request_rejection,
};
use crate::record::log::WireDialect;

/// The output cap a spotter request is sent with, or the model's own when that is
/// smaller (docs/permissions.md). The verdict is one short call, but a reasoning model
/// thinks first and the thinking counts against the cap, so the client's default of
/// 4096 can end the response before the call. Not the model's whole cap, which a
/// provider may set aside against a rate limit before generating.
pub(super) const SPOT_MAX_OUTPUT: u64 = 16_384;

/// The effort a spotter request asks for on the Responses dialect (docs/permissions.md),
/// whose `reasoning` field has no spelling of off that every model accepts: `none` is
/// refused by models that take `low`, and leaving the field out lets the model reason
/// at its own default, which is the level that ran past the cap and ended without the
/// call.
const SPOT_EFFORT: &str = "low";

/// Requests per verdict at most (docs/permissions.md): a transient upstream failure is
/// sent again after a pause doubling from one second, and an answer with no usable
/// `spot` call is sent again once, at once.
const SPOT_REQUESTS: u32 = 3;

/// The effort the spotter's requests carry on `dialect`: [`SPOT_EFFORT`] on Responses,
/// and none where the forced call's thinking off is how the request asks for no
/// reasoning.
pub(super) fn spot_effort(dialect: WireDialect) -> Option<&'static str> {
    match dialect {
        WireDialect::Responses => Some(SPOT_EFFORT),
        WireDialect::Messages | WireDialect::Completions | WireDialect::SystemOne => None,
    }
}

/// The cap a request is sent again at when the cap cut it short: twice `cap`, or the
/// model's own cap when that is smaller.
pub(super) fn raised_cap(cap: u64, model_cap: Option<u64>) -> u64 {
    let raised = cap.saturating_mul(2);
    model_cap.map_or(raised, |model_cap| raised.min(model_cap))
}

impl ForkSpot {
    /// Apply what the model's metadata says (`Endpoint::spotter_capabilities`): its
    /// thinking answer, which spells the forced call's thinking off
    /// (docs/transport.md); its output cap, when it is below [`SPOT_MAX_OUTPUT`]; and
    /// its context window, which the cap is reserved against.
    pub fn with_capabilities(mut self, capabilities: &ModelCapabilities) -> Self {
        let cap = capabilities
            .max_output_tokens
            .map_or(SPOT_MAX_OUTPUT, |model_cap| model_cap.min(SPOT_MAX_OUTPUT));
        self.client = self
            .client
            .with_thinking(capabilities.thinking)
            .with_max_output(cap)
            .with_context_window(capabilities.context_length);
        self.retry_cap = raised_cap(cap, capabilities.max_output_tokens);
        self
    }

    /// Ask for a verdict until one arrives or [`SPOT_REQUESTS`] are spent, and fail
    /// closed otherwise. An answer without a usable `spot` call — the model ended its turn
    /// without the call, or the cap cut it before the call — is asked again once, at once,
    /// and at a raised cap when the cap cut it. A transient upstream failure is asked
    /// again after a pause. Every request's usage is in the returned one.
    pub(super) async fn verdict(&self, turns: &[WireTurn]) -> Spotting {
        let mut usage: Option<RequestUsage> = None;
        let mut cap = None;
        let mut asked_again = false;
        let mut transient = 0;
        for request in 1..=SPOT_REQUESTS {
            let retry = match self.ask(turns, cap).await {
                Ok(completion) => {
                    // One ledger entry with every request's tokens, under the id and
                    // model of the request that answered last.
                    let mut answered = completion.request_usage();
                    if let Some(earlier) = &usage {
                        answered.usage.add(&earlier.usage);
                    }
                    usage = Some(answered);
                    if completion.upstream_failure().is_none() {
                        if let Some(read) = parse_spot(&completion) {
                            return Spotting {
                                read,
                                origin: Origin::Model,
                                usage,
                                axes: None,
                            };
                        }
                        if asked_again {
                            return Spotting::fail_closed(usage);
                        }
                        asked_again = true;
                        if completion.hit_output_cap() {
                            cap = Some(self.retry_cap);
                        }
                        log::debug!(
                            "spotter answer without a usable `spot` call ({}); asking again",
                            completion
                                .finish
                                .as_ref()
                                .map_or("no finish", |finish| finish.raw.as_str())
                        );
                        Retry::Now
                    } else if completion.terminal_failure().is_none()
                        && !completion.operator_abort()
                    {
                        // An unfinalized stream (docs/transport.md) is not a verdict.
                        transient += 1;
                        Retry::After(transient)
                    } else {
                        return Spotting::fail_closed(usage);
                    }
                }
                Err(e) if error_class(&e) == FailureClass::Transient => {
                    transient += 1;
                    Retry::After(transient)
                }
                Err(_) => return Spotting::fail_closed(usage),
            };
            if request == SPOT_REQUESTS {
                break;
            }
            // A transient failure is retried briefly before failing closed: the
            // fallback is a human gate, which an operator at the keyboard answers in
            // less time than a long backoff, and which leaves an unattended run
            // waiting — so a short burst is worth absorbing and an outage is not
            // worth waiting out here.
            if let Retry::After(failures) = retry {
                tokio::time::sleep(Duration::from_secs(1 << (failures - 1))).await;
            }
        }
        Spotting::fail_closed(usage)
    }

    /// One request for a verdict, at `cap` when a retry raised it. A request carrying
    /// the spotter's effort that the endpoint rejects ([`is_request_rejection`]) is sent
    /// once more without it; when that succeeds, this rung sends no effort from then
    /// on, as the client learns a refused forced choice (docs/transport.md). When it
    /// fails too, the rejection had another cause and the first error is returned.
    async fn ask(&self, turns: &[WireTurn], cap: Option<u64>) -> Result<Completion, Error> {
        let client = match cap {
            Some(cap) => self.client.clone().with_max_output(cap),
            None => self.client.clone(),
        };
        let sends_effort = self.effort.is_some() && !self.effort_refused.load(Ordering::Relaxed);
        if !sends_effort {
            return complete(&client.with_effort(None), turns).await;
        }
        let refusal = match complete(&client, turns).await {
            Err(e) if is_request_rejection(&e) => e,
            answered => return answered,
        };
        let model = client.model().to_string();
        match complete(&client.with_effort(None), turns).await {
            Ok(completion) => {
                log::warn!(
                    "{model} refused the spotter's effort ({refusal}); this spotter sends \
                     none from here on"
                );
                self.effort_refused.store(true, Ordering::Relaxed);
                Ok(completion)
            }
            Err(_) => Err(refusal),
        }
    }
}

/// When a request that did not return a verdict is sent again.
enum Retry {
    /// Straight away: the endpoint answered, without a usable call.
    Now,
    /// After a pause: the nth transient failure.
    After(u32),
}

/// The forced `spot` request over `turns`. No-op sink: the spotter's tokens are
/// internal and must never surface on the control stream (mirrors the triage).
async fn complete(client: &Client, turns: &[WireTurn]) -> Result<Completion, Error> {
    client
        .complete(turns, &[spot_tool()], ToolChoice::force(SPOT), |_| {})
        .await
}

#[cfg(test)]
mod test;
