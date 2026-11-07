use super::*;
use crate::config::guide::{lookup, path};
use serde_json::json;
fn call(name: &str, input: serde_json::Value) -> ToolCall {
    ToolCall::new(name, "resource-test", input)
}

#[tokio::test]
async fn a_read_of_a_document_is_served_from_the_binary() {
    let result = Toolbox::new(Vec::new())
        .read_resources(&call("read", json!({ "paths": [path("config-format")] })))
        .await
        .expect("a virtual read is intercepted");
    assert!(!result.is_error);
    assert_eq!(
        result.content,
        lookup(&path("config-format")).unwrap().body.trim_end()
    );

    // Several at once concatenate, in the order asked.
    let both = Toolbox::new(Vec::new())
        .read_resources(&call(
            "read",
            json!({ "paths": [path("editing"), path("new-project")] }),
        ))
        .await
        .expect("intercepted");
    assert!(
        both.content
            .starts_with(lookup(&path("editing")).unwrap().body.trim_end())
    );
    assert!(
        both.content
            .ends_with(lookup(&path("new-project")).unwrap().body.trim_end())
    );

    // A read that names no virtual path is not this module's business.
    assert!(
        Toolbox::new(Vec::new())
            .read_resources(&call("read", json!({ "paths": ["src/lib.rs"] })))
            .await
            .is_none()
    );
}

#[tokio::test]
async fn an_unknown_slug_lists_the_documents() {
    let result = Toolbox::new(Vec::new())
        .read_resources(&call(
            "read",
            json!({ "paths": ["efference://guide/how-to-fly"] }),
        ))
        .await
        .expect("intercepted");
    assert!(result.is_error);
    assert!(result.content.contains(&path("config-format")));
}

#[tokio::test]
async fn a_mixed_batch_is_refused() {
    let result = Toolbox::new(Vec::new())
        .read_resources(&call(
            "read",
            json!({ "paths": [path("editing"), "src/lib.rs"] }),
        ))
        .await
        .expect("intercepted");
    assert!(result.is_error);
    assert!(result.content.contains("src/lib.rs"));
}

#[tokio::test]
async fn a_model_read_without_a_host_does_not_fall_through_to_disk() {
    let toolbox = Toolbox::new(Vec::new());
    let result = toolbox
        .read_resources(&call(
            "read",
            json!({"paths": ["efference://models?q=some-model"]}),
        ))
        .await
        .unwrap();
    assert!(result.is_error);
    assert!(result.content.contains("requires a hosted session"));
    assert!(
        toolbox
            .read_resources(&call("write", json!({"path": "efference://models"})))
            .await
            .is_none()
    );
    let result = toolbox
        .read_resources(&call(
            "read",
            json!({"paths": ["efference://models", "local.txt"]}),
        ))
        .await
        .unwrap();
    assert!(result.is_error);
    assert!(result.content.contains("local.txt"));
}
