//! Result shapes the gate does not get to decide about (docs/redaction-fork.md).
//!
//! A result whose value is a machine-precise handle the model must reproduce
//! exactly cannot pass through a summarizer: a paraphrase keeps the meaning and
//! destroys the function, and nothing in the result tells the peek that. So the
//! decision is not the fork's to make — a result carrying one of the shapes below
//! is never offered for a verdict, and its raw text reaches the canonical wire
//! byte-for-byte.
//!
//! One entry per known shape in [`SHAPES`]. A second handle-bearing format is one
//! more entry, not another mechanism.

/// A handle format: a sentinel followed by a fixed-width hex digest. The digest
/// width and the hex alphabet are what keep the match narrow — a result that
/// merely mentions the sentinel in prose is not a handle-bearing result.
pub(super) struct Shape {
    /// How the shape is named in the bypass breadcrumb the `Triage` records.
    name: &'static str,
    sentinel: &'static str,
    digits: usize,
}

impl Shape {
    /// Whether `text` carries this shape anywhere in it. The handle is quoted
    /// inside a sentence of refusal prose, so position is not part of the match;
    /// the digest is. A trailing alphanumeric rules out a longer token that
    /// happens to open with the same bytes.
    fn matches(&self, text: &str) -> bool {
        text.match_indices(self.sentinel).any(|(at, _)| {
            let rest = &text.as_bytes()[at + self.sentinel.len()..];
            rest.len() >= self.digits
                && rest[..self.digits].iter().all(u8::is_ascii_hexdigit)
                && rest
                    .get(self.digits)
                    .is_none_or(|b| !b.is_ascii_alphanumeric())
        })
    }
}

/// The known handle-bearing result shapes.
///
/// `fs-mcp write retry token`: a refused `write` (the path exists, or `overwrite`
/// and `append` were both set) preserves the rejected body server-side under
/// `write-retry-<16 hex>` and tells the caller to repeat the call with that token
/// as `contents`. The token is the only way to reach the preserved body, so a
/// refusal whose token was paraphrased away costs the whole body again on the
/// next attempt — measured at ~26 KB across three re-sends of one file.
const SHAPES: &[Shape] = &[Shape {
    name: "fs-mcp write retry token",
    sentinel: "write-retry-",
    digits: 16,
}];

/// The name of the first handle shape `text` carries, or `None` if it carries
/// none — in which case the result is the fork's to distill as usual.
pub(super) fn verbatim_handle(text: &str) -> Option<&'static str> {
    SHAPES
        .iter()
        .find(|shape| shape.matches(text))
        .map(|s| s.name)
}

#[cfg(test)]
mod test;
