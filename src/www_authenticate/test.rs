use super::*;

/// Build `(scheme, [(param, value)])` for terse comparison.
fn shape(challenges: &Challenges) -> Vec<(&str, Vec<(&str, &str)>)> {
    challenges
        .iter()
        .map(|c| (c.scheme(), c.params().collect()))
        .collect()
}

#[test]
fn the_mcp_401_challenge() {
    // The example from the spec's authorization overview.
    let challenges = Challenges::parse(
        r#"Bearer resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource", scope="files:read""#,
    );
    let bearer = challenges.scheme("Bearer").unwrap();
    assert_eq!(
        bearer.param("resource_metadata"),
        Some("https://mcp.example.com/.well-known/oauth-protected-resource")
    );
    assert_eq!(bearer.param("scope"), Some("files:read"));
    assert_eq!(challenges.len(), 1);
}

#[test]
fn the_mcp_403_insufficient_scope_challenge() {
    let challenges = Challenges::parse(
        r#"Bearer error="insufficient_scope", scope="files:write", resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource", error_description="File write permission required for this operation""#,
    );
    let bearer = challenges.scheme("Bearer").unwrap();
    assert_eq!(bearer.param("error"), Some("insufficient_scope"));
    assert_eq!(bearer.param("scope"), Some("files:write"));
    assert_eq!(
        bearer.param("error_description"),
        Some("File write permission required for this operation")
    );
}

#[test]
fn two_challenges_each_with_params() {
    let challenges = Challenges::parse(r#"Bearer realm="a", scope="x", Basic realm="c""#);
    assert_eq!(
        shape(&challenges),
        [
            ("Bearer", vec![("realm", "a"), ("scope", "x")]),
            ("Basic", vec![("realm", "c")]),
        ]
    );
}

/// The case that makes a naive `split(',')` wrong: the separator between two
/// challenges and the separator between two params are the same character, and
/// a quoted value may contain it too.
#[test]
fn a_comma_inside_a_quoted_value_is_data() {
    let challenges = Challenges::parse(r#"Bearer realm="a, b", scope="x", Basic realm="c""#);
    assert_eq!(
        shape(&challenges),
        [
            ("Bearer", vec![("realm", "a, b"), ("scope", "x")]),
            ("Basic", vec![("realm", "c")]),
        ]
    );
}

#[test]
fn quoted_string_escapes_are_resolved() {
    let challenges = Challenges::parse(r#"Bearer realm="say \"hi\"", scope="x""#);
    let bearer = challenges.scheme("Bearer").unwrap();
    assert_eq!(bearer.param("realm"), Some(r#"say "hi""#));
    // The escaped quotes must not have ended the string early, so `scope`
    // is still a parameter of this challenge rather than a new scheme.
    assert_eq!(bearer.param("scope"), Some("x"));
}

#[test]
fn an_escaped_backslash_before_a_quote_still_terminates() {
    let challenges = Challenges::parse(r#"Bearer realm="back\\", scope="x""#);
    let bearer = challenges.scheme("Bearer").unwrap();
    assert_eq!(bearer.param("realm"), Some(r"back\"));
    assert_eq!(bearer.param("scope"), Some("x"));
}

#[test]
fn a_scheme_with_no_parameters() {
    let challenges = Challenges::parse("Basic");
    assert_eq!(shape(&challenges), [("Basic", vec![])]);
    assert_eq!(challenges.scheme("Basic").unwrap().token68(), None);
}

#[test]
fn a_scheme_with_no_parameters_beside_one_that_has_them() {
    let challenges = Challenges::parse(r#"Bearer realm="a", Basic"#);
    assert_eq!(
        shape(&challenges),
        [("Bearer", vec![("realm", "a")]), ("Basic", vec![])]
    );
}

/// A bare token right after a scheme is a token68, not the next scheme — the
/// same syntax means different things at the two positions.
#[test]
fn token68_without_padding() {
    let challenges = Challenges::parse("Negotiate a87421bf");
    let negotiate = challenges.scheme("Negotiate").unwrap();
    assert_eq!(negotiate.token68(), Some("a87421bf"));
    assert_eq!(negotiate.params().count(), 0);
}

#[test]
fn token68_padding_is_not_a_parameter() {
    let challenges = Challenges::parse("Negotiate dGVzdA==");
    let negotiate = challenges.scheme("Negotiate").unwrap();
    assert_eq!(negotiate.token68(), Some("dGVzdA=="));
    assert_eq!(negotiate.params().count(), 0);
}

#[test]
fn a_single_trailing_equals_is_padding_not_an_empty_value() {
    let challenges = Challenges::parse("Negotiate dGVzdDE=");
    assert_eq!(
        challenges.scheme("Negotiate").unwrap().token68(),
        Some("dGVzdDE=")
    );
}

/// `/` is a token68 character but not a `tchar`, so the blob has to be taken
/// whole rather than as a leading token plus leftovers.
#[test]
fn token68_containing_non_token_characters() {
    let challenges = Challenges::parse("Negotiate ab+c/def=");
    assert_eq!(
        challenges.scheme("Negotiate").unwrap().token68(),
        Some("ab+c/def=")
    );
}

#[test]
fn a_token68_challenge_followed_by_another_challenge() {
    let challenges = Challenges::parse(r#"Negotiate dGVzdA==, Bearer realm="a""#);
    assert_eq!(
        challenges.scheme("Negotiate").unwrap().token68(),
        Some("dGVzdA==")
    );
    assert_eq!(
        challenges.scheme("Bearer").unwrap().param("realm"),
        Some("a")
    );
}

#[test]
fn an_unquoted_token_value() {
    let challenges = Challenges::parse("Bearer max_age=3600, scope=openid");
    let bearer = challenges.scheme("Bearer").unwrap();
    assert_eq!(bearer.param("max_age"), Some("3600"));
    assert_eq!(bearer.param("scope"), Some("openid"));
}

#[test]
fn bad_whitespace_around_the_equals_is_tolerated() {
    let challenges = Challenges::parse(r#"Bearer realm = "a" , scope =x"#);
    assert_eq!(
        shape(&challenges),
        [("Bearer", vec![("realm", "a"), ("scope", "x")])]
    );
}

#[test]
fn schemes_and_parameter_names_are_case_insensitive() {
    let challenges = Challenges::parse(r#"bEaReR ReAlM="MixedCase""#);
    let bearer = challenges.scheme("BEARER").unwrap();
    assert!(bearer.is_scheme("bearer"));
    // The value is not case-folded — only the name is.
    assert_eq!(bearer.param("realm"), Some("MixedCase"));
    assert_eq!(bearer.scheme(), "bEaReR", "the scheme is kept as written");
}

#[test]
fn a_repeated_parameter_keeps_both_and_reads_the_first() {
    // Forbidden by the grammar; a parser should still report what arrived.
    let challenges = Challenges::parse(r#"Bearer realm="first", realm="second""#);
    let bearer = challenges.scheme("Bearer").unwrap();
    assert_eq!(bearer.param("realm"), Some("first"));
    assert_eq!(bearer.params().count(), 2);
}

#[test]
fn empty_list_elements_are_skipped() {
    let challenges = Challenges::parse(r#", ,Bearer realm="a", ,, Basic,"#);
    assert_eq!(
        shape(&challenges),
        [("Bearer", vec![("realm", "a")]), ("Basic", vec![])]
    );
}

#[test]
fn several_header_values_form_one_list() {
    let challenges = Challenges::parse_all([r#"Bearer realm="a""#, r#"Basic realm="c""#]);
    assert_eq!(
        shape(&challenges),
        [
            ("Bearer", vec![("realm", "a")]),
            ("Basic", vec![("realm", "c")]),
        ]
    );
}

/// A challenge split across header lines still assembles: the continuation
/// parameter attaches to the challenge already in progress.
#[test]
fn a_challenge_continued_on_the_next_header_value() {
    let challenges = Challenges::parse_all([r#"Bearer realm="a""#, r#"scope="x""#]);
    assert_eq!(
        shape(&challenges),
        [("Bearer", vec![("realm", "a"), ("scope", "x")])]
    );
}

/// The spec writes its examples with the header wrapped across lines. Nothing
/// should hinge on a stray newline surviving into the value.
#[test]
fn line_folded_whitespace_is_tolerated() {
    let challenges = Challenges::parse(
        "Bearer resource_metadata=\"https://mcp.example.com/.well-known/oauth-protected-resource\",\r\n                         scope=\"files:read\"",
    );
    assert_eq!(
        shape(&challenges),
        [(
            "Bearer",
            vec![
                (
                    "resource_metadata",
                    "https://mcp.example.com/.well-known/oauth-protected-resource"
                ),
                ("scope", "files:read")
            ]
        )]
    );
}

#[test]
fn an_absent_or_empty_header_yields_nothing() {
    assert!(Challenges::parse("").is_empty());
    assert!(Challenges::parse("   ").is_empty());
    assert!(Challenges::parse(",,,").is_empty());
    assert_eq!(Challenges::parse("Bearer").scheme("Basic"), None);
}

/// Leniency is the contract: a client that refuses to read a slightly-broken
/// challenge just fails to authenticate. Nothing here may panic.
#[test]
fn malformed_input_does_not_panic() {
    for input in [
        r#"Bearer realm=""#,
        r#"Bearer realm="unterminated"#,
        r#"Bearer ="#,
        r#"Bearer realm=, scope="x""#,
        "Bearer =====",
        r#"=,="#,
        r#"Bearer realm="\"#,
        "Bearer réalm=x",
        "Bearer realm=\"ünïcödé, value\", scope=x",
    ] {
        let _ = Challenges::parse(input);
    }
    // Multi-byte characters inside a quoted value must not desync the splitter.
    let challenges = Challenges::parse("Bearer realm=\"ünïcödé, value\", scope=x");
    let bearer = challenges.scheme("Bearer").unwrap();
    assert_eq!(bearer.param("realm"), Some("ünïcödé, value"));
    assert_eq!(bearer.param("scope"), Some("x"));
}

/// Challenges captured verbatim from deployed MCP servers. Every one is the
/// multi-parameter continuation case — a scheme, then three or four params
/// separated by the same commas that separate challenges — which is precisely
/// what a `split(',')` gets wrong.
#[test]
fn challenges_from_real_mcp_servers() {
    for (server, header, metadata) in [
        (
            "linear",
            r#"Bearer realm="OAuth", resource_metadata="https://mcp.linear.app/.well-known/oauth-protected-resource/mcp", error="invalid_token", error_description="Missing or invalid access token""#,
            Some("https://mcp.linear.app/.well-known/oauth-protected-resource/mcp"),
        ),
        (
            // Sentry orders the parameters differently — `error` before
            // `resource_metadata`. Order must not matter to a lookup.
            "sentry",
            r#"Bearer realm="OAuth", error="invalid_token", error_description="Missing or invalid access token", resource_metadata="https://mcp.sentry.dev/.well-known/oauth-protected-resource/mcp""#,
            Some("https://mcp.sentry.dev/.well-known/oauth-protected-resource/mcp"),
        ),
        (
            "github",
            r#"Bearer error="invalid_request", error_description="No access token was provided in this request", resource_metadata="https://api.githubcopilot.com/.well-known/oauth-protected-resource/mcp""#,
            Some("https://api.githubcopilot.com/.well-known/oauth-protected-resource/mcp"),
        ),
        (
            "openrouter",
            r#"Bearer resource_metadata="https://mcp.openrouter.ai/mcp/.well-known/oauth-protected-resource""#,
            Some("https://mcp.openrouter.ai/mcp/.well-known/oauth-protected-resource"),
        ),
        (
            // Atlassian advertises no resource_metadata at all, so discovery
            // has to fall back to probing the well-known URIs.
            "atlassian",
            r#"Bearer realm="OAuth", error="invalid_token", error_description="Missing or invalid access token""#,
            None,
        ),
    ] {
        let challenges = Challenges::parse(header);
        let bearer = challenges
            .scheme("Bearer")
            .unwrap_or_else(|| panic!("{server}: no Bearer challenge in {header}"));
        assert_eq!(bearer.param("resource_metadata"), metadata, "{server}");
        assert_eq!(challenges.len(), 1, "{server}: expected one challenge");
    }
}
