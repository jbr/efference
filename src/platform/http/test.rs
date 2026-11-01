use super::*;

/// The `User-Agent` names this harness first and the transport second — the
/// conventional product-token list, most-specific first. Pinned on shape rather
/// than on the exact string so a dependency bump doesn't fail a test for
/// reporting the truth.
#[test]
fn the_user_agent_names_the_harness_then_the_transport() {
    let ua = user_agent();
    let (efference, trillium) = ua.split_once(' ').expect("two product tokens");
    assert_eq!(
        efference,
        format!("efference/{}", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(trillium, trillium_client::USER_AGENT);
    assert!(trillium.starts_with("trillium-client/"), "{ua}");
}

/// Every clone carries the `User-Agent` as a default header, which is what makes
/// it reach hosts a caller never thinks about — the dev-proxy, an MCP server, a
/// push service.
#[test]
fn the_shared_client_sends_the_user_agent_by_default() {
    assert_eq!(
        client()
            .default_headers()
            .get_str(KnownHeaderName::UserAgent),
        Some(user_agent())
    );
}

/// Nothing else is a default header. Anything identifying *who* is asking —
/// credentials, the sticky-routing key, OpenRouter's attribution — is set per
/// request by a caller that knows the destination, so a header meant for one
/// provider cannot ride to another host on a shared clone.
#[test]
fn the_shared_client_defaults_to_nothing_that_identifies_the_caller() {
    let client = client();
    let names: Vec<String> = client
        .default_headers()
        .iter()
        .map(|(name, _)| name.to_string().to_ascii_lowercase())
        .collect();
    assert!(
        names.iter().all(|n| n == "user-agent" || n == "accept"),
        "unexpected default header among {names:?}"
    );
}
