use super::*;

fn shown(title: &mut TerminalTitle, text: &str) -> String {
    let mut out = Vec::new();
    title.show(&mut out, text).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn show_writes_osc_0() {
    let mut title = TerminalTitle::default();
    assert_eq!(
        shown(&mut title, "Kitty tab titles · amber-heron"),
        "\x1b]0;Kitty tab titles · amber-heron\x07"
    );
}

#[test]
fn an_unchanged_title_is_not_rewritten() {
    let mut title = TerminalTitle::default();
    shown(&mut title, "one");
    assert_eq!(shown(&mut title, "one"), "");
    assert_eq!(shown(&mut title, "two"), "\x1b]0;two\x07");
}

#[test]
fn control_characters_cannot_end_the_sequence() {
    let mut title = TerminalTitle::default();
    // ESC, BEL, and the 8-bit ST and OSC (C1) would each end or open a sequence.
    assert_eq!(
        shown(&mut title, "a\x07b\x1b]0;c\u{9c}d\u{9d}e\nf"),
        "\x1b]0;ab]0;cdef\x07"
    );
}

#[test]
fn save_and_restore_use_the_title_stack() {
    let mut out = Vec::new();
    save(&mut out).unwrap();
    restore(&mut out).unwrap();
    assert_eq!(out, b"\x1b[22;0t\x1b[23;0t");
}

#[test]
fn session_title_leads_its_id() {
    assert_eq!(
        for_session("amber-heron", Some("Kitty tab titles")),
        "Kitty tab titles · amber-heron"
    );
    assert_eq!(for_session("amber-heron", None), "efference · amber-heron");
}

#[test]
fn host_counts_its_sessions() {
    assert_eq!(for_host(0), "efference · 0 sessions");
    assert_eq!(for_host(1), "efference · 1 session");
    assert_eq!(for_host(3), "efference · 3 sessions");
}
