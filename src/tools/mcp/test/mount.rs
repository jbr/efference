use super::*;

#[tokio::test(start_paused = true)]
async fn a_mount_timeout_names_the_pending_method() {
    // The scripted server's requests in order: server/discover, initialize, then
    // two tools/list pages.
    for (stall_at, method) in [(1, "server/discover"), (2, "initialize"), (4, "tools/list")] {
        let mut count = 0;
        let mut script = scripted_server();
        let (transport, _) = loopback(Box::new(move |request| {
            count += 1;
            if count == stall_at {
                Vec::new()
            } else {
                script(request)
            }
        }));
        let error = ConnectedProvider::mount_transport("stalled", transport)
            .await
            .err()
            .expect("mount should time out");
        let message = format!("{error:#}");
        assert!(message.contains("within 30s"), "{message}");
        assert!(
            message.contains(&format!("(waiting on {method})")),
            "{message}"
        );
    }
}

#[tokio::test]
async fn an_http_mount_error_names_the_method_and_keeps_its_type() {
    use crate::inference::testing::upstream::{Canned, ScriptedUpstream};

    for (status, body) in [(200, "not JSON"), (503, "unavailable"), (401, "")] {
        let upstream = ScriptedUpstream::new(move |_| Canned::json(status, body));
        let origin = upstream.bind();
        let error = ConnectedProvider::mount(
            "broken",
            &McpTransport::Http {
                url: format!("{origin}/mcp"),
            },
            Path::new("."),
            None,
            &crate::config::env_table::ChildEnv::default(),
        )
        .await
        .err()
        .expect("mount should fail");
        let message = format!("{error:#}");
        assert!(message.starts_with("during server/discover: "), "{message}");
        // Boot's mount runs the authorization dance only when this downcast
        // succeeds through the added context.
        assert_eq!(
            error
                .downcast_ref::<auth::AuthorizationRequired>()
                .is_some(),
            status == 401
        );
    }
}
