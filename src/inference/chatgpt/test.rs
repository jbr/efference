use super::*;

/// The three values the issuer exact-matches. A redirect URI that says
/// `127.0.0.1` — which is what the listener actually binds — is refused by the
/// issuer, so the spelling is asserted rather than left to a helper's default.
#[test]
fn the_redirect_uri_is_spelled_the_way_it_was_registered() {
    let uri = format!("http://{REDIRECT_HOST}:{}{CALLBACK_PATH}", PORTS[0]);
    assert_eq!(uri, "http://localhost:1455/auth/callback");
    assert_eq!(PORTS[1], 1457, "the other registered port");
}

/// The authorization query, including the two parameters beyond the standard
/// set. Asserted whole: a missing `codex_cli_simplified_flow` or a narrowed
/// scope produces a grant the registered redirect URIs are not for, and the
/// failure arrives at the browser rather than here.
#[test]
fn the_authorize_url_carries_what_the_issuer_requires() {
    let pkce = pkce::Pkce::generate();
    let url = authorize_url("http://localhost:1455/auth/callback", &pkce, "the-state");

    let (base, query) = url.split_once('?').expect("a query");
    assert_eq!(base, "https://auth.openai.com/oauth/authorize");
    let pairs: std::collections::BTreeMap<String, String> =
        trillium_client::url::form_urlencoded::parse(query.as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();

    assert_eq!(pairs["response_type"], "code");
    assert_eq!(pairs["client_id"], "app_EMoamEEZ73f0CkXaXp7hrann");
    assert_eq!(pairs["redirect_uri"], "http://localhost:1455/auth/callback");
    assert_eq!(
        pairs["scope"],
        "openid profile email offline_access api.connectors.read api.connectors.invoke"
    );
    assert_eq!(pairs["code_challenge_method"], "S256");
    assert_eq!(pairs["code_challenge"], pkce.challenge());
    assert_eq!(pairs["id_token_add_organizations"], "true");
    assert_eq!(pairs["codex_cli_simplified_flow"], "true");
    assert_eq!(pairs["state"], "the-state");
}

/// A JWT segment, signed with nothing: the claims are read, never verified.
fn jwt(claims: serde_json::Value) -> String {
    use base64::Engine as _;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
    format!("header.{payload}.signature")
}

#[test]
fn the_account_and_plan_come_off_the_access_token() {
    let token = jwt(serde_json::json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "228f6909-0000-0000-0000-000000000000",
            "chatgpt_plan_type": "plus",
        }
    }));
    let (account, plan) = account_of(&token).expect("readable claims");
    assert_eq!(account, "228f6909-0000-0000-0000-000000000000");
    assert_eq!(plan.as_deref(), Some("plus"));
}

/// The account is required — it rides `ChatGPT-Account-ID` on every request, so
/// a token without it cannot address the backend and must not be stored as if
/// it could.
#[test]
fn a_token_without_an_account_is_refused() {
    let token = jwt(serde_json::json!({ "https://api.openai.com/auth": { "amr": ["pop"] } }));
    let error = account_of(&token).expect_err("no account id");
    assert!(error.contains("chatgpt_account_id"), "{error}");
}

#[test]
fn a_token_that_is_not_a_jwt_says_so() {
    let error = account_of("not-a-jwt").expect_err("unreadable");
    assert!(error.contains("JWT"), "{error}");
}

/// A grant as stored: the serde shape is the storage format, so this is what a
/// record written by an earlier build has to keep parsing as.
fn stored(expires_at: Option<u64>) -> Grant {
    serde_json::from_value(serde_json::json!({
        "issuer": ISSUER,
        "tokens": {
            "access_token": "at",
            "token_type": "Bearer",
            "refresh_token": "rt",
            "scope": SCOPE,
            "expires_at": expires_at,
        },
        "account_id": "acct-1",
        "plan": "plus",
    }))
    .expect("the stored shape")
}

fn seconds_from_now(offset: i64) -> u64 {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    now.saturating_add_signed(offset)
}

/// Refresh is scheduled off the token's own `exp`, so it happens before a
/// request is rejected rather than after one is.
#[test]
fn a_grant_is_refreshed_before_it_expires_not_after() {
    assert!(
        !stored(Some(seconds_from_now(60 * 60 * 24))).needs_refresh(),
        "a day out is current"
    );
    assert!(
        stored(Some(seconds_from_now(60 * 30))).needs_refresh(),
        "inside the window, before any rejection"
    );
    assert!(
        stored(Some(seconds_from_now(-1))).needs_refresh(),
        "already past"
    );
    assert!(
        !stored(None).needs_refresh(),
        "no stated expiry is taken as current, not refreshed on every request"
    );
}

/// A grant survives the round trip through the store, under a name that says
/// it is a record rather than a bare secret.
#[test]
fn a_grant_round_trips_through_the_secret_store() {
    let secrets = Secrets::in_memory();
    assert!(
        Grant::load(&secrets, "codex")
            .expect("no grant is not an error")
            .is_none()
    );

    stored(Some(seconds_from_now(60 * 60 * 24)))
        .save(&secrets, "codex")
        .expect("saved");
    let loaded = Grant::load(&secrets, "codex")
        .expect("readable")
        .expect("present");
    assert_eq!(loaded.account_id, "acct-1");
    assert_eq!(loaded.plan.as_deref(), Some("plus"));
    assert_eq!(loaded.issuer, ISSUER);
    assert!(
        secrets.get("oauth:codex").unwrap().is_some(),
        "stored under the record name, not `inference:`"
    );
}

/// An unreadable record is an error rather than "no grant": treating it as
/// absent would start a browser dance and overwrite whatever was there.
#[test]
fn an_unreadable_record_is_not_read_as_a_missing_one() {
    let secrets = Secrets::in_memory();
    secrets.set("oauth:codex", "{not json").expect("stored");
    let error = Grant::load(&secrets, "codex").expect_err("refused");
    assert!(error.contains("oauth:codex"), "{error}");
}

/// The tokens never reach a log line through Debug.
#[test]
fn a_grant_does_not_debug_print_its_tokens() {
    let printed = format!("{:?}", stored(None));
    assert!(!printed.contains("\"at\""), "{printed}");
    assert!(!printed.contains("rt"), "{printed}");
    assert!(
        printed.contains("acct-1"),
        "the account is not a secret: {printed}"
    );
}
