use super::*;
use mcplease::types::Implementation;

/// A call over MCP is named from the caller's `clientInfo`, without the caller
/// saying anything (docs/control-api.md); an explicit label wins over it, and no
/// caller name (the command line) means the operator.
#[test]
fn source_comes_from_client_info_unless_explicit() {
    let context = RequestContext::from_params(Some(&serde_json::json!({
        "_meta": {
            mcplease::types::meta_keys::CLIENT_INFO: Implementation::new("claude-code", "1.0"),
        }
    })));
    assert_eq!(
        turn_source(None, &context).as_deref(),
        Some("mcp[claude-code]")
    );
    assert_eq!(
        turn_source(Some("agent[abc]".into()), &context).as_deref(),
        Some("agent[abc]")
    );
    assert_eq!(turn_source(None, &RequestContext::default()), None);
}
