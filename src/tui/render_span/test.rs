use super::{display_width, width_upto, wrap_points};

/// Split `text` at the byte boundaries `wrap_points` reports, for legible
/// assertions on the visual rows.
fn rows(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut prev = 0;
    for b in wrap_points(text, width) {
        out.push(text[prev..b].to_string());
        prev = b;
    }
    out.push(text[prev..].to_string());
    out
}

#[test]
fn short_text_is_one_row() {
    assert!(wrap_points("hello", 40).is_empty());
}

#[test]
fn breaks_at_word_boundaries() {
    // 30 cols: the break falls on a space, never mid-word, and the breaking
    // space stays on the preceding row so the next starts clean.
    let r = rows("the quick brown fox jumps over the lazy dog", 30);
    assert!(r.len() >= 2, "wrapped into multiple rows");
    for row in &r {
        assert!(display_width(row) <= 30, "row {row:?} within width");
        assert!(!row.starts_with(' '), "no leading space on {row:?}");
    }
    // Reassembling the rows reproduces the input exactly (no dropped chars) —
    // the property the composer's cursor mapping depends on.
    assert_eq!(r.concat(), "the quick brown fox jumps over the lazy dog");
}

#[test]
fn hard_breaks_a_word_longer_than_the_width() {
    // No whitespace to break on, so it cuts to make progress.
    let r = rows("superlongunbreakabletoken", 10);
    assert!(r.len() >= 3);
    assert_eq!(r.concat(), "superlongunbreakabletoken");
    assert!(r.iter().all(|row| display_width(row) <= 10));
}

#[test]
fn zero_width_never_loops() {
    assert!(wrap_points("anything", 0).is_empty());
}

/// [`width_upto`] agrees with [`display_width`] wherever the answer is wanted —
/// i.e. everywhere at or below the cap. Above it, all a caller ever asks is
/// "wider than the cap?", and `cap + 1` answers that for every comparison the wrap
/// policy makes.
#[test]
fn a_bounded_width_agrees_with_the_full_one_below_its_cap() {
    for text in [
        "",
        "hello",
        "hello world",
        "\ttabbed",
        "αβγ — 日本語",
        "❬rust❭ ❯ ⋯",
        "a̐éö̲ combining",
    ] {
        let full = display_width(text);
        for cap in 0..=full + 2 {
            let bounded = width_upto(text, cap);
            if full <= cap {
                assert_eq!(bounded, full, "{text:?} at cap {cap}: exact below the cap");
            } else {
                assert_eq!(bounded, cap + 1, "{text:?} at cap {cap}: over-cap sentinel");
            }
            // The one property every call site actually relies on.
            assert_eq!(
                bounded <= cap,
                full <= cap,
                "{text:?} at cap {cap}: the fits-in-cap test must not change"
            );
        }
    }
}

/// **Wrapping one long line is linear in its length, not quadratic.**
///
/// `wrap_points` emits a row at a time and each row's decision compares against
/// something no wider than the row, so a row costs the row — not the rest of the
/// line. Measuring the whole remainder per row is quadratic instead: a 16k-column
/// line (a tool call's JSON input mid-stream, a minified file, a base64 blob) costs
/// ~300 ms per wrap in release that way, repeated every frame while it streams.
///
/// A ratio rather than a duration, so it says the same thing on any machine. The
/// margin is wide because the defect it guards against is not: eight times the
/// length costs eight times as much when linear and sixty-four when quadratic.
#[test]
fn wrapping_a_long_line_is_linear_in_its_length() {
    fn line(chars: usize) -> String {
        "let value = compute(state); "
            .chars()
            .cycle()
            .take(chars)
            .collect()
    }
    // Best-of, so an unlucky scheduling slice can't manufacture a failure.
    fn cost(text: &str) -> std::time::Duration {
        (0..5)
            .map(|_| {
                let start = std::time::Instant::now();
                std::hint::black_box(wrap_points(std::hint::black_box(text), 76));
                start.elapsed()
            })
            .min()
            .expect("five samples")
    }

    let (short, long) = (line(2_000), line(16_000));
    cost(&short); // warm the allocator and the instruction cache
    let ratio = cost(&long).as_secs_f64() / cost(&short).as_secs_f64();
    assert!(
        ratio < 24.0,
        "8× the line length cost {ratio:.1}× the wrap — linear is 8×, quadratic is 64×"
    );
}
