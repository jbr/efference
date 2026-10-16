use super::*;

/// The refusal text fs-mcp's `write` returns when the path already exists, verbatim
/// down to the backticks around the token — the string the shape rule has to
/// recognize in production.
const REFUSAL: &str = "File src/main.rs already exists, use \"overwrite\": true if you intend \
to replace it, or \"append\": true if you intend to add content to the end of the file.\n\n\
Your 8772 bytes of contents are preserved under `write-retry-9f3a1c04be27d5e8`: repeat the \
call with \"overwrite\": true and \"contents\": \"write-retry-9f3a1c04be27d5e8\" to confirm \
this exact write.";

#[test]
fn a_write_retry_token_in_a_refusal_is_a_verbatim_handle() {
    assert_eq!(verbatim_handle(REFUSAL), Some("fs-mcp write retry token"));
    // Position is not part of the match: the token appears mid-sentence, quoted, and
    // again as a JSON value.
    assert_eq!(
        verbatim_handle("write-retry-0123456789abcdef"),
        Some("fs-mcp write retry token")
    );
}

/// The sentinel alone is ordinary text — a file that discusses the feature, a
/// changelog line, this test. Only sentinel-plus-digest is a handle.
#[test]
fn the_sentinel_without_a_digest_is_not_a_handle() {
    for text in [
        "write-retry- is mentioned in these notes\n",
        "see write-retry-<hex> in the refusal",
        "write-retry-0123456789abcde",         // 15 hex digits
        "write-retry-0123456789abcdefa",       // 17 — a longer token, not this one
        "write-retry-0123456789abcdeg",        // not hex
        "the retry token shape is documented", // no sentinel at all
    ] {
        assert_eq!(verbatim_handle(text), None, "{text}");
    }
}

/// An ordinary tool result — a file listing, a build log — carries no handle and
/// stays the fork's to triage.
#[test]
fn ordinary_output_carries_no_handle() {
    assert_eq!(verbatim_handle("src/main.rs\nsrc/lib.rs\n"), None);
    assert_eq!(verbatim_handle(""), None);
}
