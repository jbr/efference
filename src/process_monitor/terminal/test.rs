use super::*;

/// Vitest's coloured summary, as a TTY-detecting runner writes it.
const VITEST: &str = "\x1b[32m ✓\x1b[39m src/a.test.ts \x1b[2m(3 tests)\x1b[22m \x1b[2m12ms\x1b[22m\n\
\x1b[1m\x1b[7m\x1b[36m RUN \x1b[39m\x1b[27m\x1b[22m \x1b[36mv2.1.0\x1b[39m\n\
\x1b[2m Test Files \x1b[22m \x1b[1m\x1b[32m1 passed\x1b[39m\x1b[22m\x1b[90m (1)\x1b[39m\n";

#[test]
fn colour_sequences_are_removed() {
    assert_eq!(
        strip_formatting(VITEST),
        " ✓ src/a.test.ts (3 tests) 12ms\n RUN  v2.1.0\n Test Files  1 passed (1)\n"
    );
}

#[test]
fn cursor_movement_and_private_modes_are_removed() {
    assert_eq!(
        strip_formatting("\x1b[2K\x1b[1Gbuilding\x1b[?25l\x1b[?25h\x1b[3;4H!"),
        "building!"
    );
    assert_eq!(strip_formatting("a\u{9b}31mb"), "ab", "single-byte CSI");
}

#[test]
fn an_osc_8_hyperlink_keeps_its_visible_text() {
    assert_eq!(
        strip_formatting("see \x1b]8;;https://example.com/a?b=c\x1b\\the docs\x1b]8;;\x1b\\ now"),
        "see the docs now"
    );
    assert_eq!(
        strip_formatting("\x1b]8;id=1;file:///x\x07link\x1b]8;;\x07"),
        "link",
        "BEL-terminated"
    );
    assert_eq!(strip_formatting("\x1b]0;window title\x07text"), "text");
}

#[test]
fn an_unterminated_string_ends_at_its_line() {
    assert_eq!(strip_formatting("a\x1b]8;;https://x\nnext"), "a\nnext");
}

#[test]
fn lone_two_byte_and_charset_escapes_are_removed() {
    assert_eq!(strip_formatting("a\x1bMb\x1b=c\x1b>d"), "abcd");
    assert_eq!(strip_formatting("\x1b(Bplain\x1b)0"), "plain");
    assert_eq!(strip_formatting("end\x1b"), "end");
    assert_eq!(
        strip_formatting("a\x1b\nb"),
        "a\nb",
        "a newline is not consumed"
    );
}

#[test]
fn a_redrawn_line_keeps_its_last_state() {
    assert_eq!(
        strip_formatting("  10%\r  55%\r 100% done\nnext\n"),
        " 100% done\nnext\n"
    );
    assert_eq!(strip_formatting("a\rb\rc"), "c", "no line ending");
    assert_eq!(
        strip_formatting("\x1b[2K\rcompiling 1/9\x1b[2K\rcompiling 9/9\n"),
        "compiling 9/9\n"
    );
}

#[test]
fn a_trailing_carriage_return_does_not_erase_the_line() {
    assert_eq!(strip_formatting("50%\r"), "50%");
    assert_eq!(strip_formatting("50%\r\r"), "50%");
}

#[test]
fn crlf_endings_are_preserved() {
    assert_eq!(strip_formatting("one\r\ntwo\r\n"), "one\r\ntwo\r\n");
    assert_eq!(
        strip_formatting("a\r\n10%\r20%\r\nb"),
        "a\r\n20%\r\nb",
        "a redraw before a CRLF ending keeps the ending"
    );
    assert_eq!(strip_formatting("\r\n\r\n"), "\r\n\r\n");
}

#[test]
fn plain_text_is_returned_byte_identical() {
    for plain in [
        "",
        "hello\n",
        "tabs\tand\nlines\n\n",
        "unicode ✓ ünï — 日本語\n",
        "brackets [31m and ]8;; stay\n",
    ] {
        assert_eq!(strip_formatting(plain), plain);
    }
}
