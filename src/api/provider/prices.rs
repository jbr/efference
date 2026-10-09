//! Per-token prices the keepalive prices a ping against (docs/transport.md).

/// What a prompt cache saves and what output costs, in USD per token — the three
/// prices the keepalive's affordability check needs (`daemon/keepalive.rs`).
/// Transport-only: carried on [`ModelCapabilities`], never logged or projected.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenPrices {
    /// An uncached prompt token.
    pub prompt: f64,
    /// An output token (reasoning included).
    pub completion: f64,
    /// A cache-read prompt token.
    pub cache_read: f64,
}

impl TokenPrices {
    /// What reading one prefix token cold costs beyond reading it warm: the premium
    /// a ping avoids. Never negative — an endpoint pricing the cache above its
    /// prompt saves nothing by being kept warm.
    pub fn premium(&self) -> f64 {
        (self.prompt - self.cache_read).max(0.0)
    }
}

/// One serving endpoint's listed prices, as the provider spells them: decimal USD
/// per token in strings.
pub(super) struct ListedPrices<'a> {
    pub(super) prompt: Option<&'a str>,
    pub(super) completion: Option<&'a str>,
    pub(super) cache_read: Option<&'a str>,
}

impl ListedPrices<'_> {
    /// All three prices, or `None` when any is absent, unparseable, or negative
    /// (`-1` is the provider's "varies"). An endpoint that prices no cache read has
    /// no cold-read premium to weigh, so it contributes nothing.
    fn parse(&self) -> Option<TokenPrices> {
        let per_token = |text: Option<&str>| -> Option<f64> {
            let value: f64 = text?.trim().parse().ok()?;
            (value.is_finite() && value >= 0.0).then_some(value)
        };
        Some(TokenPrices {
            prompt: per_token(self.prompt)?,
            completion: per_token(self.completion)?,
            cache_read: per_token(self.cache_read)?,
        })
    }
}

/// The prices one session is checked against, from the endpoints that may serve it
/// (docs/transport.md). Routing is unpinned, so any listed endpoint might take the
/// ping, and the check errs toward switching the keepalive off rather than paying
/// for a ping that saves less than it costs: of the endpoints that price a cache
/// read, the one with the **smallest** cold-read premium supplies `prompt` and
/// `cache_read`, and the **largest** completion price among them supplies
/// `completion`. `None` when no endpoint prices all three, which leaves the check on
/// its token-ratio rule.
pub(super) fn conservative<'a>(
    endpoints: impl IntoIterator<Item = ListedPrices<'a>>,
) -> Option<TokenPrices> {
    let priced: Vec<TokenPrices> = endpoints.into_iter().filter_map(|e| e.parse()).collect();
    let narrowest = priced
        .iter()
        .min_by(|a, b| a.premium().total_cmp(&b.premium()))?;
    let completion = priced.iter().map(|p| p.completion).fold(0.0, f64::max);
    Some(TokenPrices {
        completion,
        ..*narrowest
    })
}
