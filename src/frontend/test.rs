use super::*;

/// A session's source names its project only when it is another than the
/// recipient's; every reader takes the id from either form, and the wire label names
/// the project when there is one.
#[test]
fn a_session_source_names_another_project() {
    let here = session_source("calm-finch", None);
    let there = session_source("calm-finch", Some("~/code/mcplease"));
    assert_eq!(here, "session[calm-finch]");
    assert_eq!(there, "session[calm-finch in ~/code/mcplease]");

    for source in [&here, &there] {
        assert_eq!(source_session(source), Some("calm-finch"));
        assert_eq!(source_name(source), "calm-finch");
        assert!(agent_source(Some(source)));
    }
    assert_eq!(wire_label(&here), "calm-finch (agent)");
    assert_eq!(wire_label(&there), "calm-finch (agent in ~/code/mcplease)");

    assert_eq!(source_name("mcp[claude-code]"), "claude-code");
    assert_eq!(source_name("agent[parent]"), "parent");
    assert_eq!(source_name("calm-finch"), "calm-finch");
    assert_eq!(wire_label("calm-finch"), "calm-finch");
}
