//! Tests for image refusal (docs/transport.md): a 400 on a request carrying images
//! is retried once with them withheld, and the client remembers when that cures it.

use super::forced_choice::{STREAM, scripted};
use super::*;
use crate::log::ResultImage;
use crate::project::{UserPart, WireTurn};

/// Some 400 whose wording names nothing; the retry is the diagnosis, not the text.
const REFUSAL: &str =
    r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad request"}}"#;

/// A history whose one tool result carries an image.
fn with_image() -> Vec<WireTurn> {
    vec![WireTurn::User(vec![UserPart::ToolResult {
        call_id: CallId("c".into()),
        seq: Some(3),
        content: "an image".into(),
        is_error: false,
        images: vec![ResultImage {
            media_type: "image/png".into(),
            data: "aGk=".into(),
            width: 1,
            height: 1,
        }],
    }])]
}

fn carries_image(body: &Value) -> bool {
    body["messages"].to_string().contains(r#""type":"image""#)
}

/// A refusal the image-free retry cures teaches the client: the retry and every
/// later request go with a placeholder where the image was, one request each.
#[tokio::test]
async fn a_refusal_cured_by_withholding_images_is_remembered() {
    let (client, server) = scripted(vec![(400, REFUSAL), (200, STREAM), (200, STREAM)]);
    let turns = with_image();
    client
        .complete(&turns, &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    assert!(
        client
            .images_refused()
            .is_some_and(|r| r.contains("bad request"))
    );
    client
        .complete(&turns, &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    let bodies = server.join().unwrap();

    assert!(carries_image(&bodies[0]));
    for body in &bodies[1..] {
        assert!(!carries_image(body), "{body}");
        assert!(
            body["messages"]
                .to_string()
                .contains("1 image block(s) withheld"),
            "{body}"
        );
    }
}

/// When the retry fails too, the images were not the cause: the first error comes
/// back and nothing is learned.
#[tokio::test]
async fn a_400_withholding_does_not_cure_is_returned_and_not_learned() {
    let (client, server) = scripted(vec![(400, REFUSAL), (400, REFUSAL), (200, STREAM)]);
    let turns = with_image();
    assert!(
        client
            .complete(&turns, &[], ToolChoice::auto(), |_| {})
            .await
            .is_err()
    );
    assert_eq!(client.images_refused(), None);
    client
        .complete(&turns, &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    let bodies = server.join().unwrap();
    assert!(carries_image(&bodies[2]));
}

/// Once a request carrying images has succeeded, the model reads them: a later
/// 400 is returned as-is, so the cached history's images are never stripped.
#[tokio::test]
async fn a_400_after_images_were_accepted_is_not_retried() {
    let (client, server) = scripted(vec![(200, STREAM), (400, REFUSAL)]);
    let turns = with_image();
    client
        .complete(&turns, &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    assert!(
        client
            .complete(&turns, &[], ToolChoice::auto(), |_| {})
            .await
            .is_err()
    );
    assert_eq!(server.join().unwrap().len(), 2);
    assert_eq!(client.images_refused(), None);
}

/// OpenRouter's answer to an image sent to a text-only model, captured 2026-10-09:
/// a 404, not a 400. Any request rejection is retried, whatever its status.
#[tokio::test]
async fn openrouters_404_for_an_unroutable_image_is_recovered_from() {
    const NO_ENDPOINTS: &str = r#"{"type":"error","error":{"type":"not_found_error","message":"No endpoints found that support image input","error_type":"not_found"}}"#;
    let (client, server) = scripted(vec![(404, NO_ENDPOINTS), (200, STREAM)]);
    client
        .complete(&with_image(), &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    assert!(
        client
            .images_refused()
            .is_some_and(|r| r.contains("No endpoints"))
    );
    assert!(!carries_image(&server.join().unwrap()[1]));
}

/// A rate limit is about the caller, not the content: nothing is withheld.
#[tokio::test]
async fn a_429_is_not_a_reason_to_withhold_images() {
    let limited = r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#;
    let (client, server) = scripted(vec![(429, limited); 6]);
    assert!(
        client
            .complete(&with_image(), &[], ToolChoice::auto(), |_| {})
            .await
            .is_err()
    );
    assert_eq!(client.images_refused(), None);
    drop(server);
}

/// A 400 on a request with no images has nothing to withhold.
#[tokio::test]
async fn a_400_without_images_is_not_retried() {
    let (client, server) = scripted(vec![(400, REFUSAL)]);
    let turns = [WireTurn::text("hi")];
    assert!(
        client
            .complete(&turns, &[], ToolChoice::auto(), |_| {})
            .await
            .is_err()
    );
    assert_eq!(server.join().unwrap().len(), 1);
}

/// A refusal restored from the log withholds images from the first request.
#[tokio::test]
async fn a_restored_refusal_withholds_from_the_first_request() {
    let (client, server) = scripted(vec![(200, STREAM)]);
    client.refuse_images("recorded");
    client
        .complete(&with_image(), &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    assert!(!carries_image(&server.join().unwrap()[0]));
}
