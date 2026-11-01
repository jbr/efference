use super::*;

/// The blend is the whole of the "code stays distinguishable" decision, so it is
/// pinned directly: endpoints exact, midpoint on the line between them.
#[test]
fn blend_moves_a_channel_toward_the_target() {
    assert_eq!(blend(0, 200, 0.0), 0, "t=0 leaves the source untouched");
    assert_eq!(blend(0, 200, 1.0), 200, "t=1 lands on the target");
    assert_eq!(blend(0, 200, 0.5), 100);
    // Downward too — a code color brighter than the dim register.
    assert_eq!(blend(200, 0, 0.5), 100);
}

/// Two code colors that differ before dimming still differ after: that is what
/// "keeps hue separation" has to mean for the choice to be worth anything. If
/// this fails, the blend has collapsed the palette and the register is really
/// the flat one.
#[test]
fn dimmed_code_colors_stay_distinct_from_each_other() {
    let dim = (0x80u8, 0x80u8, 0x80u8);
    let keyword = (0xff, 0x40, 0x40);
    let type_name = (0x40, 0x40, 0xff);
    let blend_all = |c: (u8, u8, u8)| {
        (
            blend(c.0, dim.0, CODE_DIM_BLEND),
            blend(c.1, dim.1, CODE_DIM_BLEND),
            blend(c.2, dim.2, CODE_DIM_BLEND),
        )
    };
    let (a, b) = (blend_all(keyword), blend_all(type_name));
    assert_ne!(a, b, "distinct code colors collapsed to one");
    // And each has moved toward the register rather than staying put.
    assert_ne!(a, keyword);
    assert_ne!(b, type_name);
}

/// Prose dims the whole way and code does not: the two halves of the register.
#[test]
fn only_code_styles_keep_their_hue() {
    for style in [
        SpanStyle::Keyword,
        SpanStyle::TypeName,
        SpanStyle::FunctionName,
        SpanStyle::InlineCode,
    ] {
        assert!(is_code_style(style), "{style:?} should keep its hue");
    }
    for style in [
        SpanStyle::Plain,
        SpanStyle::Strong,
        SpanStyle::Emphasis,
        SpanStyle::Comment,
        SpanStyle::Punctuation,
    ] {
        assert!(!is_code_style(style), "{style:?} should dim flat");
    }
}
