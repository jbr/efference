//! The inline fenced artifact syntax shared by the control API and TUI.
//!
//! Coordinates count fenced code blocks in the same assistant text block, not only
//! artifact blocks, so a later client cannot disagree about which source to resolve.

use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};

type SourceRange = std::ops::Range<usize>;
type CurrentFence = (usize, String, SourceRange, Vec<SourceRange>);

/// One explicitly closed `html-artifact` fence and its ordinal among all fenced blocks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HtmlArtifactFence {
    pub(crate) ordinal: usize,
    pub(crate) html: String,
}

/// Extract closed fences with the exact `html-artifact` info string, in source order.
///
/// `pulldown-cmark` emits a matching `End` event for an unterminated code fence too,
/// so eligibility also checks the source range for an explicit closing marker. Container
/// prefixes are accepted because Markdown permits fenced blocks inside quotes and lists.
pub(crate) fn closed_html_artifact_fences(markdown: &str) -> Vec<HtmlArtifactFence> {
    let mut ordinal = 0;
    let mut current: Option<CurrentFence> = None;
    let mut artifacts = Vec::new();

    for (event, range) in Parser::new(markdown).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) => {
                let block_ordinal = ordinal;
                ordinal += 1;
                current = (info.as_ref() == "html-artifact")
                    .then(|| (block_ordinal, String::new(), range, Vec::new()));
            }
            Event::Text(text) => {
                if let Some((_, html, _, text_ranges)) = &mut current {
                    html.push_str(&text);
                    text_ranges.push(range);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((ordinal, html, block_range, text_ranges)) = current.take()
                    && has_explicit_closer(markdown, block_range, &text_ranges)
                {
                    artifacts.push(HtmlArtifactFence { ordinal, html });
                }
            }
            _ => {}
        }
    }

    artifacts
}

fn has_explicit_closer(
    markdown: &str,
    range: std::ops::Range<usize>,
    text_ranges: &[std::ops::Range<usize>],
) -> bool {
    let Some(block) = markdown.get(range.clone()) else {
        return false;
    };
    let Some(open_line) = block.lines().next() else {
        return false;
    };
    let Some((marker, open_length, _)) = fence_line(strip_container_prefixes(open_line)) else {
        return false;
    };

    let mut line_start = range.start;
    let mut close_line = None;
    for raw_line in block.split_inclusive('\n') {
        let line = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        if !line.trim().is_empty() {
            close_line = Some((line_start, line_start + raw_line.len(), line));
        }
        line_start += raw_line.len();
    }
    let Some((close_start, close_end, close_line)) = close_line else {
        return false;
    };

    // A fence-like line emitted as code text is content, even when stripping
    // arbitrary quote/list prefixes would make it look like a closing marker.
    if text_ranges
        .iter()
        .any(|text| text.start < close_end && text.end > close_start)
    {
        return false;
    }

    let Some((close_marker, close_length, trailing)) =
        fence_line(strip_container_prefixes(close_line))
    else {
        return false;
    };

    marker == close_marker
        && close_length >= open_length
        && trailing
            .bytes()
            .all(|byte| matches!(byte, b' ' | b'\t' | b'\r'))
}

/// Remove blockquote and list markers before reading a fence line. Source offsets for
/// nested code blocks begin at the fence, but the final source line includes its
/// container prefixes.
fn strip_container_prefixes(mut line: &str) -> &str {
    loop {
        let bytes = line.as_bytes();
        let indent = bytes.iter().take_while(|&&byte| byte == b' ').count();
        let rest = &line[indent..];
        if indent <= 3 && rest.starts_with('>') {
            line = rest[1..].strip_prefix(' ').unwrap_or(&rest[1..]);
            continue;
        }
        if indent <= 3
            && let Some(marker_length) = list_marker_length(rest)
        {
            line = rest[marker_length..].trim_start_matches([' ', '\t']);
            continue;
        }
        return line;
    }
}

fn list_marker_length(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let marker = match bytes.first()? {
        b'-' | b'+' | b'*' if bytes.get(1).is_some_and(u8::is_ascii_whitespace) => 1,
        b'0'..=b'9' => {
            let digits = bytes
                .iter()
                .take_while(|byte| byte.is_ascii_digit())
                .count();
            if digits > 9
                || !matches!(bytes.get(digits), Some(b'.' | b')'))
                || !bytes.get(digits + 1).is_some_and(u8::is_ascii_whitespace)
            {
                return None;
            }
            digits + 1
        }
        _ => return None,
    };
    Some(marker)
}

/// The opening/closing marker, its run length, and the text after the run.
fn fence_line(line: &str) -> Option<(u8, usize, &str)> {
    let bytes = line.as_bytes();
    let indent = bytes.iter().take_while(|&&byte| byte == b' ').count();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let marker = *rest.as_bytes().first()?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let length = rest
        .as_bytes()
        .iter()
        .take_while(|&&byte| byte == marker)
        .count();
    (length >= 3).then_some((marker, length, &rest[length..]))
}

#[cfg(test)]
mod test;
