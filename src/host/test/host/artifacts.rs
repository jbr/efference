//! Artifact links and documents through a host (docs/artifacts.md): the entry view
//! signs them, the web origin serves them, and they stop resolving when their
//! session leaves the host.

use super::*;
use crate::record::log::{AssistantOutput, ContentBlock};

fn assistant(text: Vec<&str>) -> Payload {
    Payload::assistant(AssistantOutput::from_legacy_messages(
        Vec::new(),
        text.into_iter()
            .map(|text| ContentBlock::Text(text.to_string()))
            .collect(),
        Vec::new(),
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_adopted_sessions_artifact_links_resolve_until_it_ends() {
    let roots = Roots::new();
    let host = Host::new(one_connection_registry(), baseline_env());
    host.adopt(
        "one".into(),
        runtime::started(runtime::boot(&roots.0.join("one")), host.swansong()).await,
    )
    .unwrap();
    let session = host.session("one").unwrap();
    let seq = session
        .append(
            1,
            assistant(vec![
                "```html-artifact\n<main>first</main>\n```\n",
                "```rust\nlet a = 1;\n```\n\n```html-artifact\n<main>second</main>\n```\n",
            ]),
        )
        .unwrap();
    let socket = host_server(&host).await;

    // The entry view carries one link per fence, signed for this session.
    let view: Value = serde_json::from_str(
        socket
            .get(&format!("/sessions/one/v1/log/{seq}"))
            .await
            .body(),
    )
    .unwrap();
    let links = view["payload"]["Assistant"]["artifacts"]
        .as_array()
        .unwrap();
    assert_eq!(
        links
            .iter()
            .map(|link| (link["text_block"].as_u64(), link["fence"].as_u64()))
            .collect::<Vec<_>>(),
        [(Some(0), Some(0)), (Some(1), Some(0))]
    );
    let url = links[1]["url"].as_str().unwrap().to_owned();
    assert!(
        url.starts_with(&format!("/artifacts/one/{seq}/1/0/")),
        "{url}"
    );

    // The socket stack serves the document with the artifact policy.
    let document = socket.get(&url).await;
    document.assert_status(200);
    assert!(document.body().contains("<main>second</main>"));
    assert!(!document.body().contains("<main>first</main>"));
    assert!(document.body().starts_with(
        "<!doctype html><link rel=\"stylesheet\" href=\"/artifacts/_efference/theme.css\">"
    ));
    assert_eq!(
        document
            .response_headers()
            .get_str(KnownHeaderName::ContentSecurityPolicy),
        Some(crate::listeners::artifacts::ARTIFACT_CSP)
    );

    // The gated web stack serves it with no credential, as a sandboxed frame asks.
    let (web, _bearer, _state) = network_host_server(&host).await;
    let anonymous = web.get(&url).await;
    anonymous.assert_status(200);
    assert!(anonymous.body().contains("<main>second</main>"));
    web.get("/artifacts/_efference/runtime.js")
        .await
        .assert_status(200);

    // A tag is good for its own coordinates only.
    let first = links[0]["url"].as_str().unwrap();
    let (_, first_tag) = first.rsplit_once('/').unwrap();
    let (second_path, _) = url.rsplit_once('/').unwrap();
    socket
        .get(&format!("{second_path}/{first_tag}"))
        .await
        .assert_status(404);

    // When the session leaves the host, its links stop resolving: there is nothing
    // else to clean up.
    host.end("one").expect("a hosted session").await;
    socket.get(&url).await.assert_status(404);
}
