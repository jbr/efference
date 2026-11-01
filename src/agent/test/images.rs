//! Tests for the loop's side of an image refusal (docs/transport.md): the gate
//! closes once with one record, and a resumed session restores both halves.
use super::*;

use crate::inference::api::Endpoint;
use crate::record::log::Provider;

fn session() -> Arc<Session> {
    let (shared, _rx) = Session::in_memory();
    shared
}

/// A client nothing is sent through.
fn client() -> Client {
    let endpoint = Endpoint::at(Provider::Anthropic, "http://127.0.0.1:9/v1/messages");
    Client::new(endpoint, "m", "sys")
}

fn refusals(shared: &Session) -> Vec<String> {
    shared
        .snapshot()
        .into_iter()
        .filter_map(|entry| match entry.payload {
            Payload::ImagesRefused { refusal } => Some(refusal),
            _ => None,
        })
        .collect()
}

/// A refusal the client learned closes the gate and is recorded once, however
/// many requests follow it.
#[test]
fn a_learned_refusal_closes_the_gate_and_is_recorded_once() {
    let shared = session();
    let client = client();
    let mut toolbox = Toolbox::bash_only();
    record_image_refusal(&shared, &client, &mut toolbox, 1).unwrap();
    assert!(toolbox.image_input());
    assert!(refusals(&shared).is_empty());

    client.refuse_images("400 bad request");
    record_image_refusal(&shared, &client, &mut toolbox, 1).unwrap();
    record_image_refusal(&shared, &client, &mut toolbox, 2).unwrap();
    assert!(!toolbox.image_input());
    assert_eq!(refusals(&shared), ["400 bad request"]);
}

/// A resumed session reads the record back: the client withholds and the gate is
/// closed before the first request, and nothing new is recorded.
#[test]
fn a_recorded_refusal_is_restored_on_resume() {
    let shared = session();
    shared
        .append(
            1,
            Payload::ImagesRefused {
                refusal: "400 bad request".into(),
            },
        )
        .unwrap();
    let client = client();
    let mut toolbox = Toolbox::bash_only();
    restore_image_refusal(&shared, &client, &mut toolbox);
    assert_eq!(client.images_refused(), Some("400 bad request"));
    assert!(!toolbox.image_input());
    record_image_refusal(&shared, &client, &mut toolbox, 2).unwrap();
    assert_eq!(refusals(&shared).len(), 1);
}
