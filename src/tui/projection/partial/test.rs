use super::*;

fn settled_keys(input: &PartialInput) -> Vec<&str> {
    input.settled.iter().map(|(k, _)| k.as_str()).collect()
}

/// The whole point: the identifying argument is settled — and therefore renderable
/// as identity — long before the big one finishes arriving.
#[test]
fn a_closed_pair_settles_while_the_next_is_still_arriving() {
    let input = PartialInput::parse(r#"{"path":"src/api.rs","contents":"use st"#).unwrap();
    assert_eq!(settled_keys(&input), ["path"]);
    assert_eq!(
        input.settled[0].1.as_str(),
        Some("src/api.rs"),
        "a closed value is complete, not truncated"
    );
    assert_eq!(input.arriving_text(), Some(("contents", "use st")));
}

/// **The claim the honesty rule turns on.** A half-arrived path must never reach
/// the settled set, because `write src/api.r` is well-formed, plausible, and false.
/// It is the last key, so it is in flight, so it renders as content or not at all.
#[test]
fn a_half_arrived_value_is_never_settled() {
    let input = PartialInput::parse(r#"{"path":"src/api.r"#).unwrap();
    assert!(
        input.settled.is_empty(),
        "nothing has closed yet: {:?}",
        input.settled
    );
    assert_eq!(input.arriving_text(), Some(("path", "src/api.r")));
}

/// A value that has closed but whose object has not is settled: the bytes for it
/// arrived and the closing quote landed, which is the whole test.
#[test]
fn a_closed_value_settles_before_the_object_closes() {
    let input = PartialInput::parse(r#"{"path":"src/api.rs""#).unwrap();
    assert_eq!(settled_keys(&input), ["path"]);
    assert_eq!(input.arriving, None);
}

/// A complete document has nothing in flight — the state the live block hands over
/// to the committed one.
#[test]
fn a_complete_document_has_nothing_arriving() {
    let input = PartialInput::parse(r#"{"path":"a.rs","contents":"fn main() {}"}"#).unwrap();
    assert_eq!(settled_keys(&input), ["path", "contents"]);
    assert_eq!(input.arriving, None);
}

/// Arrival order is preserved, not sorted. `serde_json::Value` sorts its keys, so
/// going through it would put `contents` before `path` and destroy the very
/// ordering the last-key rule reads.
#[test]
fn pairs_keep_arrival_order() {
    let input =
        PartialInput::parse(r#"{"intent":"write it","path":"a.rs","contents":"x"}"#).unwrap();
    assert_eq!(settled_keys(&input), ["intent", "path", "contents"]);
}

/// Before any structure has arrived there is nothing to claim, and the caller falls
/// back to rendering the raw characters.
#[test]
fn a_buffer_with_no_structure_yet_yields_nothing() {
    for buffer in ["", "{", "{\"pa"] {
        assert_eq!(
            PartialInput::parse(buffer).map(|i| i.settled.len()),
            PartialInput::parse(buffer).map(|_| 0),
            "sanity"
        );
        let parsed = PartialInput::parse(buffer);
        assert!(
            parsed.as_ref().is_none_or(|i| i.settled.is_empty()),
            "{buffer:?} claimed a settled pair"
        );
    }
}

/// Feeding the buffer one byte at a time, no prefix ever reports a settled value
/// that the complete document disagrees with. This is the invariant that matters
/// across the whole stream, not just at the two or three offsets a hand-written
/// case happens to pick.
#[test]
fn no_prefix_ever_settles_a_value_the_whole_document_contradicts() {
    let whole = r#"{"intent":"write the renderer","path":"src/tui/render.rs","append":false,"contents":"fn main() {\n    println!(\"hi\");\n}\n"}"#;
    let complete = PartialInput::parse(whole).expect("the whole document parses");
    assert_eq!(complete.arriving, None);

    for end in 1..=whole.len() {
        let Some(prefix) = PartialInput::parse(&whole[..end]) else {
            continue;
        };
        for (key, value) in &prefix.settled {
            let final_value = complete
                .settled
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v);
            assert_eq!(
                final_value,
                Some(value),
                "at byte {end}, {key:?} settled as {value:?} but the document says \
                 {final_value:?} — a settled value must be final"
            );
        }
        // And the arriving key, when there is one, is always the *next* key the
        // document has after the settled ones — never an earlier one revisited.
        if let Some((key, _)) = &prefix.arriving {
            assert_eq!(
                complete.settled.get(prefix.settled.len()).map(|(k, _)| k),
                Some(key),
                "at byte {end}, {key:?} is arriving out of order"
            );
        }
    }
}

/// Escapes decode rather than showing through: a rendered `\n` must be a newline,
/// since turning the escaped contents back into lines is what makes the streamed
/// text renderable as text at all.
#[test]
fn escapes_in_the_arriving_value_decode() {
    let input = PartialInput::parse(r#"{"path":"a.rs","contents":"fn a() {\n    b();\n"#).unwrap();
    let (_, text) = input.arriving_text().unwrap();
    assert_eq!(text, "fn a() {\n    b();\n");
    assert_eq!(text.lines().count(), 2, "decoded into real lines");
}

/// The settled value drops the arriving pair — the per-tool renderer must only ever
/// see closed values, since what it produces is an identity claim.
#[test]
fn the_settled_value_excludes_what_is_still_arriving() {
    let input = PartialInput::parse(r#"{"path":"a.rs","contents":"partial"#).unwrap();
    let value = input.settled_value();
    assert_eq!(value.get("path").and_then(|v| v.as_str()), Some("a.rs"));
    assert!(
        value.get("contents").is_none(),
        "an in-flight value reached the identity renderer"
    );
}
