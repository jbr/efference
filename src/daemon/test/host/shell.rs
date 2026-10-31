use super::*;

#[tokio::test]
async fn missing_session_pages_reach_the_shell_but_content_stays_missing() {
    let host = Host::new(crate::inference::InferenceRegistry::empty(), baseline_env());
    let (server, bearer) = network_host_server(&host).await;
    for path in ["/sessions/gone/", "/sessions/gone/tree"] {
        server
            .get(path)
            .await
            .assert_status(200)
            .assert_header_with(KnownHeaderName::ContentType, |content_type| {
                assert!(
                    content_type
                        .as_str()
                        .is_some_and(|value| value.starts_with("text/html"))
                );
            });
    }
    server
        .build(trillium::Method::Head, "/sessions/gone/")
        .await
        .assert_status(200)
        .assert_body("");
    // `/files` is priced like the control paths beside it: the bearer reaches the
    // missing-session lookup, and no credential stops at the gate.
    for path in [
        "/sessions/gone/v1/log?tail=200",
        "/sessions/gone/v1/events",
        "/sessions/gone/unknown",
        "/sessions/gone/files/",
    ] {
        server
            .get(path)
            .with_request_header("authorization", bearer.clone())
            .await
            .assert_status(404)
            .assert_body_contains(r#""code":"not_found""#);
    }
    server.get("/sessions/gone/v1/log").await.assert_status(401);
    server.get("/sessions/gone/files/").await.assert_status(401);
    let socket = host_server(&host).await;
    socket.get("/sessions/gone/").await.assert_status(404);
    socket
        .get("/sessions/gone/files/")
        .await
        .assert_status(404)
        .assert_body_contains(r#""code":"not_found""#);
}
