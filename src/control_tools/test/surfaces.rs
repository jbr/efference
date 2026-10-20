//! What each set advertises: the names a mounting harness caches, the schemas a model
//! fills in, and the hints an approval UX shows (docs/tools.md).

use super::super::*;
use mcplease::traits::AsToolsList;

/// Each set's advertised names, in order: a mounting harness caches the list, and
/// order is macro-expansion order, so a reorder moves every client's cached list
/// and the in-session `tools[]` (docs/tools.md). The MCP set waits with `wait`, which
/// needs the mount's watched set; the command line, which holds no mount, keeps
/// `settle` (docs/operating.md).
#[test]
fn each_set_advertises_its_verbs_in_order() {
    let names = |tools: Vec<mcplease::types::Tool>| -> Vec<String> {
        tools.into_iter().map(|tool| tool.name).collect()
    };
    let agent = [
        "create",
        "message",
        "decide",
        "interrupt",
        "archive",
        "status",
        "transcript",
    ];
    let with = |extra: &[&str]| -> Vec<String> {
        agent
            .iter()
            .chain(extra)
            .map(|name| name.to_string())
            .collect()
    };
    assert_eq!(names(in_session::Tools::tools_list()), with(&["hand_off"]));
    assert_eq!(names(mcp::Tools::tools_list()), with(&["wait"]));
    assert_eq!(
        names(cli::Tools::tools_list()),
        with(&[
            "settle",
            "state",
            "pace",
            "title",
            "background",
            "proposal",
            "review_level",
            "supervisor",
            "notify",
            "entries",
            "image",
        ])
    );
}

/// An agent surface carries no output schema — every answer is plaintext — and none
/// of the retired verbs or arguments (docs/tools.md).
#[test]
fn the_agent_surfaces_advertise_plaintext_and_no_retired_verbs() {
    for tools in [in_session::Tools::tools_list(), mcp::Tools::tools_list()] {
        for tool in &tools {
            assert!(
                tool.output_schema.is_none(),
                "{} has an output schema",
                tool.name
            );
            assert!(
                ![
                    "turn",
                    "approve",
                    "sessions",
                    "subscribe",
                    "unsubscribe",
                    "notify",
                    "state"
                ]
                .contains(&tool.name.as_str()),
                "{} is off the agent surfaces",
                tool.name
            );
            let properties = tool.input_schema["properties"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            for retired in ["session", "source", "wait", "call_id", "words", "audit"] {
                assert!(
                    !properties.contains_key(retired),
                    "{} advertises `{retired}`",
                    tool.name
                );
            }
        }
    }
    let message = mcp::Tools::tools_list()
        .into_iter()
        .find(|tool| tool.name == "message")
        .unwrap();
    assert_eq!(message.input_schema["properties"]["text"]["type"], "string");
    assert_eq!(
        message.input_schema["required"],
        serde_json::json!(["agent", "text"])
    );
}

/// Only the MCP set's `status` takes `follow`, and nothing a session's toolbox or the
/// command line advertises names the status stream: the stream is served by an
/// `efference mcp` mount, and neither of the others holds one (docs/operating.md).
#[test]
fn only_the_mcp_set_offers_the_status_stream() {
    let status = |tools: Vec<mcplease::types::Tool>| {
        let status = tools
            .into_iter()
            .find(|tool| tool.name == "status")
            .unwrap();
        status.input_schema["properties"].clone()
    };
    assert_eq!(
        status(mcp::Tools::tools_list())["follow"]["type"],
        "boolean"
    );
    for (set, tools) in [
        ("in-session", in_session::Tools::tools_list()),
        ("command-line", cli::Tools::tools_list()),
    ] {
        assert!(
            status(tools).get("follow").is_none(),
            "the {set} status takes `follow`"
        );
    }
    let advertised = serde_json::to_string(&in_session::Tools::tools_list()).unwrap();
    for text in ["follow", "ws://", "Monitor", "WebSocket"] {
        assert!(
            !advertised.contains(text),
            "the in-session set names {text:?}: {advertised}"
        );
    }
}

/// The behaviour hints are what an approval UX shows, and the spec's defaults are
/// pessimistic, so an unset one reads as a destructive open-world mutation.
#[test]
fn the_verbs_are_annotated() {
    let tools = cli::Tools::tools_list();
    let hints = |name: &str| {
        let tool = tools.iter().find(|t| t.name == name).unwrap();
        let annotations = tool
            .annotations
            .as_ref()
            .unwrap_or_else(|| panic!("{name} declares no annotations"));
        (
            annotations.read_only_hint,
            annotations.destructive_hint,
            annotations.idempotent_hint,
        )
    };
    for name in [
        "status",
        "transcript",
        "state",
        "entries",
        "settle",
        "image",
    ] {
        assert_eq!(hints(name).0, Some(true), "{name} is a read");
    }
    // Interrupt kills a process group; allowing runs a call the rungs did not
    // clear; ending a session tears it down; committing a proposal folds a change
    // into the effective config.
    for name in ["interrupt", "decide", "archive", "proposal"] {
        assert_eq!(
            hints(name),
            (Some(false), Some(true), Some(false)),
            "{name}"
        );
    }
    for name in ["message", "background", "pace", "notify", "create"] {
        assert_eq!(
            hints(name),
            (Some(false), Some(false), Some(false)),
            "{name} is a non-idempotent write"
        );
    }
    let wait = mcp::Tools::tools_list()
        .into_iter()
        .find(|tool| tool.name == "wait")
        .unwrap();
    let annotations = wait.annotations.as_ref().unwrap();
    assert_eq!(annotations.read_only_hint, Some(true), "wait is a read");
    assert_eq!(annotations.open_world_hint, Some(false));
    // Every tool declares a closed world: they reach one local host, nothing else.
    for tool in &tools {
        assert_eq!(
            tool.annotations.as_ref().unwrap().open_world_hint,
            Some(false),
            "{}",
            tool.name
        );
    }
}

/// Every verb but `create` and `hand_off` takes `agent`, the mount's whole addressing
/// surface, pinned against a tool that forgets the field. The verbs that act on an
/// agent require it; `status` and `wait` read without one, and the command line's
/// route verbs fall back to the one session live here (docs/operating.md).
#[test]
fn every_agent_verb_takes_agent() {
    let acting = [
        "message",
        "decide",
        "interrupt",
        "archive",
        "transcript",
        "supervisor",
    ];
    for tool in cli::Tools::tools_list()
        .into_iter()
        .chain(in_session::Tools::tools_list())
        .chain(mcp::Tools::tools_list())
    {
        let properties = tool.input_schema["properties"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        match tool.name.as_str() {
            "create" | "hand_off" => {
                assert!(!properties.contains_key("agent"), "{}", tool.name)
            }
            name if acting.contains(&name) => {
                assert!(
                    tool.input_schema["required"]
                        .as_array()
                        .is_some_and(|required| required.contains(&"agent".into())),
                    "{} requires `agent`",
                    tool.name
                )
            }
            _ => assert!(
                properties.contains_key("agent"),
                "{} takes no `agent`",
                tool.name
            ),
        }
    }
}

/// The documented temperature range (0–100 percent) must be what the model
/// sees, not just what the runtime accepts: 255 would otherwise serialize as
/// a 5.1 native temperature. Pinned through the advertised tool list, which is
/// exactly what a mounting harness caches.
#[test]
fn create_temperature_schema_matches_documented_range() {
    let temperature = mcp::Tools::tools_list()
        .into_iter()
        .find(|tool| tool.name == "create")
        .expect("create is mounted")
        .input_schema["properties"]["temperature"]
        .clone();
    assert_eq!(temperature["type"], "integer");
    assert_eq!(temperature["minimum"], serde_json::json!(0));
    assert_eq!(temperature["maximum"], serde_json::json!(100));
}

/// `worktree` and the first message are advertised on `create`, and both default
/// off, so a bare request body still parses.
#[test]
fn create_advertises_worktree_and_a_first_message() {
    let tool = mcp::Tools::tools_list()
        .into_iter()
        .find(|tool| tool.name == "create")
        .expect("create is mounted");
    assert_eq!(
        tool.input_schema["properties"]["worktree"]["type"],
        "boolean"
    );
    assert_eq!(tool.input_schema["properties"]["message"]["type"], "string");
    let create: Create = serde_json::from_value(serde_json::json!({})).unwrap();
    assert!(!create.worktree && create.message.is_none());
    let create: Create =
        serde_json::from_value(serde_json::json!({"worktree": true, "message": "go"})).unwrap();
    assert!(create.worktree);
    assert_eq!(create.message.as_deref(), Some("go"));
}

/// A seq an agent wrote as a number reads as the handle it names.
#[test]
fn a_numeric_seq_reads_as_a_handle() {
    let decide: Decide =
        serde_json::from_value(serde_json::json!({"agent": "child", "seq": 8, "decision": "deny"}))
            .unwrap();
    assert_eq!(decide.seq, "8");
    assert_eq!(decide.decision, Decision::Deny);
    let message: Message = serde_json::from_value(
        serde_json::json!({"agent": "child", "text": "yes", "reply_to": 3.1}),
    )
    .unwrap();
    assert_eq!(message.reply_to.as_deref(), Some("3.1"));
}
