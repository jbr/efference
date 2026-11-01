//! Detecting a prompt the server cut short without saying so (docs/transport.md).
//!
//! A server whose context window is smaller than the request may drop the start of
//! the prompt and answer 200 (Ollama at its default `num_ctx`, measured
//! 2026-10-09: ~40 KB sent, 2,050 tokens read). The model then answers a
//! conversation it never saw, and the spotter clears calls against a scope it never
//! read. Nothing in the response says so except its own usage: the prompt tokens it
//! reports are far fewer than the request's text could encode to.

/// The most text bytes any tokenizer is assumed to pack into one token. Measured
/// 3.5–4.4 across the corpus's models (`api/reservation.rs`); 8 leaves about twice
/// that, so a dense tokenizer is never mistaken for a truncating server. The
/// measured truncation read ~19.7.
const MAX_BYTES_PER_TOKEN: u64 = 8;

/// Requests whose text is smaller than this are not judged: a short body's
/// envelope and the server's own accounting dominate the ratio.
const MIN_TEXT_BYTES: u64 = 8 * 1024;

/// Reported prompts smaller than this are not judged. No server serving this
/// harness has a window that small, since the preamble alone is larger, so a count
/// under it is a test double or an accounting that means something else, not a
/// window the prompt was cut to.
const MIN_REPORTED_TOKENS: u64 = 1024;

/// Why a response's reported prompt shows the server read only part of the
/// request, or `None` when it plausibly read all of it. `text_bytes` is
/// [`text_bytes`]'s lower bound. A response that reported fewer than
/// [`MIN_REPORTED_TOKENS`] prompt tokens is not judged.
pub(crate) fn truncated(prompt_tokens: u64, text_bytes: u64) -> Option<String> {
    if prompt_tokens < MIN_REPORTED_TOKENS || text_bytes < MIN_TEXT_BYTES {
        return None;
    }
    let least = text_bytes / MAX_BYTES_PER_TOKEN;
    (prompt_tokens < least).then(|| {
        format!(
            "the server read {prompt_tokens} prompt tokens of a request with {text_bytes} \
             bytes of text, which is at least {least} tokens: it truncated the prompt \
             without saying so, so the reply answered a conversation it did not see. A \
             local server's context length is the usual cause (Ollama's `num_ctx` or \
             `OLLAMA_CONTEXT_LENGTH`)"
        )
    })
}

/// The bytes of text every server must read for this request: the preamble, the
/// tool definitions, user prose, tool results, and the assistant's visible text and
/// call arguments. Left out, so the bound stays a floor: images (an image costs far
/// fewer tokens than its base64), and reasoning and its replay credentials, which a
/// provider may drop from the count (Anthropic does for earlier turns' thinking).
pub(crate) fn text_bytes(
    system: &str,
    tools: &[serde_json::Value],
    turns: &[crate::record::project::WireTurn],
) -> u64 {
    use crate::record::project::{UserPart, WireTurn};
    let tools: usize = tools.iter().map(|tool| tool.to_string().len()).sum();
    let turns: usize = turns
        .iter()
        .map(|turn| match turn {
            WireTurn::User(parts) => parts
                .iter()
                .map(|part| match part {
                    UserPart::Text(text) => text.len(),
                    UserPart::ToolResult { content, .. } => content.len(),
                    UserPart::Breakpoint => 0,
                })
                .sum(),
            WireTurn::Assistant(output) => {
                let text: usize = output.text().iter().map(|text| text.len()).sum();
                let calls: usize = output
                    .tool_calls()
                    .unwrap_or_default()
                    .iter()
                    .map(|call| call.input.to_string().len())
                    .sum();
                text + calls
            }
            WireTurn::Breakpoint => 0,
        })
        .sum();
    (system.len() + tools + turns) as u64
}

#[cfg(test)]
mod test;
