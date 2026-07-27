
use super::*;

#[test]
fn test_color_for_style() {
    let scheme = ColorScheme::default();

    // Should return colors for semantic styles
    let keyword_color = scheme.color_for(SpanStyle::Keyword);
    let type_color = scheme.color_for(SpanStyle::TypeName);

    // Colors should be different from default (theme should apply styling)
    assert!(keyword_color.r != 0 || keyword_color.g != 0 || keyword_color.b != 0);
    assert!(type_color.r != 0 || type_color.g != 0 || type_color.b != 0);
}

#[test]
fn test_default_colors() {
    let scheme = ColorScheme::default();
    let fg = scheme.default_foreground();
    let bg = scheme.default_background();

    // Should have valid RGB values
    assert!(fg.a == 255);
    assert!(bg.a == 255);
}
