use super::*;

/// The visible text of the hyperlink is the URL itself, so a terminal that
/// ignores OSC 8 still prints something copy-pasteable — which is the whole
/// point in the no-tty case, where the escape is least likely to be honored.
#[test]
fn an_osc8_link_still_reads_as_a_url_when_the_escape_is_stripped() {
    let url = "https://mcp.sentry.dev/oauth/authorize?code_challenge=abc&state=xyz";
    let rendered = osc8(url);
    assert!(rendered.contains(url));
    // Strip the escapes the way a dumb log capture effectively does.
    let stripped: String = rendered.replace('\x1b', "");
    assert!(stripped.contains(url), "{stripped}");
    // The URL appears as both link target and visible label.
    assert_eq!(rendered.matches(url).count(), 2);
}
