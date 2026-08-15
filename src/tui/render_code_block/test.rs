use super::*;
use crate::tui::render_context::RenderContext;

/// A representative block: strings, comments, nesting, and a raw string — the
/// constructs whose lexing spans lines, so continuing from a saved state can
/// actually go wrong.
const CODE: &str = r#"//! A module comment.
use std::fmt::Write;

/// Doc comment with a `code span`.
fn render(name: &str) -> String {
    let mut out = String::new();
    // A line comment mentioning "a string" and a { brace.
    let raw = r"a raw string with \n in it";
    write!(out, "{name}: {raw}").unwrap();
    if name.contains('{') {
        out.push_str("unbalanced {");
    }
    out
}
"#;

/// Highlight `code` from cold, in one pass.
fn cold(ctx: &RenderContext, code: &str) -> Vec<Vec<Segment>> {
    let syntax = ctx.syntax_set().find_syntax_by_token("rust").unwrap();
    let mut memo = HighlightMemo::default();
    memo.highlight_growing(syntax, ctx.theme(), ctx.syntax_set(), 1, code)
        .0
}

/// **The incremental path must colour a block exactly as a cold pass would.**
///
/// It reuses syntect state saved after the last complete line of the previous
/// frame; if that state were saved at the wrong point — after a partial line, say —
/// colours would diverge somewhere past the first multi-line construct and the
/// block would render subtly wrong rather than obviously broken.
#[test]
fn arriving_a_piece_at_a_time_colours_the_same_as_arriving_whole() {
    let ctx = RenderContext::new();
    let syntax = ctx.syntax_set().find_syntax_by_token("rust").unwrap();
    let expected = cold(&ctx, CODE);

    // Feed it the way a stream does: a growing prefix, re-highlighted each frame.
    let mut memo = HighlightMemo::default();
    let mut last = Vec::new();
    for end in 1..=CODE.len() {
        if !CODE.is_char_boundary(end) {
            continue;
        }
        let (lines, _) =
            memo.highlight_growing(syntax, ctx.theme(), ctx.syntax_set(), 1, &CODE[..end]);
        // Every prefix must agree with a cold pass over that same prefix, or the
        // operator is reading colours that a reload would change.
        assert_eq!(
            lines,
            cold(&ctx, &CODE[..end]),
            "prefix of {end} bytes coloured differently when streamed"
        );
        last = lines;
    }
    assert_eq!(last, expected, "the completed block");
}

/// A growing block costs the lines that arrived, not the lines it holds. Without
/// this the memo is missed every frame (its key is the text, which changed) and the
/// block re-parses from line 1 — quadratic over a write, and worst at the end.
#[test]
fn a_growing_block_colours_only_what_arrived() {
    let ctx = RenderContext::new();
    let syntax = ctx.syntax_set().find_syntax_by_token("rust").unwrap();
    let mut memo = HighlightMemo::default();

    let body: String = (1..=400)
        .map(|n| format!("    let value_{n} = compute(&state, {n});\n"))
        .collect();

    // Warm it on everything but the final line.
    let upto = body[..body.len() - 1].rfind('\n').unwrap() + 1;
    memo.highlight_growing(syntax, ctx.theme(), ctx.syntax_set(), 1, &body[..upto]);

    let (_, parsed) = memo.highlight_growing(syntax, ctx.theme(), ctx.syntax_set(), 1, &body);
    assert!(
        parsed <= 2,
        "one more line arrived but {parsed} lines were parsed — the prefix wasn't reused"
    );
}

/// Anything that isn't an append starts over rather than continuing from a state
/// that describes different text. A theme change is the case that would otherwise
/// paint the old theme's colours onto new lines forever.
#[test]
fn a_non_append_starts_over() {
    let ctx = RenderContext::new();
    let syntax = ctx.syntax_set().find_syntax_by_token("rust").unwrap();
    let mut memo = HighlightMemo::default();
    memo.highlight_growing(syntax, ctx.theme(), ctx.syntax_set(), 1, CODE);

    // Same text, different identity (a theme switch) — must not resume.
    let (lines, parsed) = memo.highlight_growing(syntax, ctx.theme(), ctx.syntax_set(), 2, CODE);
    assert!(parsed > 1, "a theme change resumed instead of recomputing");
    assert_eq!(lines, cold(&ctx, CODE));

    // Divergent text under the same identity — an edit, not an append.
    let edited = CODE.replace("fn render", "fn draw");
    let (lines, _) = memo.highlight_growing(syntax, ctx.theme(), ctx.syntax_set(), 2, &edited);
    assert_eq!(lines, cold(&ctx, &edited), "an edited block must recompute");
}
