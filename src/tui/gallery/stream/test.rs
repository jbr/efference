use super::*;

/// Every scene's scripted input has to be valid JSON, because `play_scene` parses
/// it to build the committed [`ToolCall`](crate::log::ToolCall) — a scene that
/// serialized badly would only surface as a panic mid-demo.
#[test]
fn every_scene_emits_parseable_json() {
    for scene in [Scene::Write, Scene::Bash] {
        let (_, _, input, _) = scene.parts();
        serde_json::from_str::<serde_json::Value>(&input).unwrap_or_else(|e| {
            panic!(
                "scene input is not JSON: {e}\n{}",
                &input[..80.min(input.len())]
            )
        });
    }
}

/// **The demo emits keys in arrival order, not sorted order.** `serde_json::Value`
/// sorts (for the wire's byte stability, docs/log-and-projection.md), and a demo
/// built through it would put `contents` first — the one ordering from which the
/// identifying field can never be recovered early. The whole streaming render rests
/// on `path` arriving before `contents`, so pin it here rather than eyeballing it.
#[test]
fn the_identifying_key_arrives_before_the_long_one() {
    let (_, _, input, _) = Scene::Write.parts();
    let path_at = input.find("\"path\"").expect("a path key");
    let contents_at = input.find("\"contents\"").expect("a contents key");
    assert!(
        path_at < contents_at,
        "the write scene emitted contents before path, which is not what a model does"
    );
}

/// Fragments partition the input with no gaps and no duplication — the render is
/// fed by concatenating them, so a splitter that dropped or repeated bytes would
/// produce a document that never parses however long it streamed.
#[test]
fn fragments_reassemble_into_the_input() {
    let (_, _, input, _) = Scene::Write.parts();
    for chunk in [1, 7, 24, 4096] {
        let pieces = fragments(&input, chunk);
        assert_eq!(pieces.concat(), input, "chunk size {chunk} lost bytes");
    }
}

/// A multi-byte character must not be split mid-codepoint: fragments are `&str`,
/// so a naive byte split would panic rather than merely look wrong.
#[test]
fn fragments_split_on_char_boundaries() {
    let text = "❯ αβγ — 日本語 ❬rust❭";
    for chunk in 1..=8 {
        assert_eq!(fragments(text, chunk).concat(), text);
    }
}
