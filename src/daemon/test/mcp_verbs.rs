//! The agent verbs over the control channel, as `efference mcp` and the command line
//! send them (docs/tools.md, docs/control-api.md): an operator client drives every
//! session, and a turn from an agent answers a question only by naming it.

use super::*;
use crate::control_tools::{ClientExecutor, Control, Decide, Decision, Executor, Message};
use crate::log::Telemetry;
use mcplease::traits::Tool as _;
use mcplease::types::RequestContext;

/// A session named `child`, as a host names it.
fn child() -> Arc<Session> {
    let (shared, _turns) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_identity(SessionIdentity {
        id: "child".into(),
        title: None,
        model: "m".into(),
        cwd: "/repo".into(),
        inference: None,
        successor: None,
    });
    shared
}

/// The tools over `shared`'s own router, as an MCP mount bound to it.
async fn mounted(shared: &Arc<Session>) -> Control {
    let client = test_client(shared).await;
    Control::new(
        Executor::Client(ClientExecutor::defaulting_to(client)),
        false,
    )
}

/// The context a call from Claude Code carries.
fn claude_code() -> RequestContext {
    RequestContext::from_params(Some(&serde_json::json!({
        "_meta": {
            mcplease::types::meta_keys::CLIENT_INFO:
                mcplease::types::Implementation::new("claude-code", "1.0"),
        }
    })))
}

/// Log `call` as an assistant turn, so it has a handle; returns it.
fn issue(shared: &Session, call: &ToolCall) -> String {
    shared
        .append(
            1,
            Payload::Assistant {
                output: crate::log::AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![],
                    vec![call.clone()],
                ),
                excluded: None,
            },
        )
        .unwrap()
        .to_string()
}

fn message(text: &str, reply_to: Option<&str>) -> Message {
    Message {
        agent: None,
        text: text.into(),
        words: Vec::new(),
        reply_to: reply_to.map(str::to_string),
    }
}

/// `decide` over MCP maps the seq to the held call's id from the status, records the
/// mounting client as the decider, and refuses a seq that is not the held call.
#[tokio::test]
async fn decide_maps_the_seq_and_records_the_client() {
    let shared = child();
    let call = ToolCall::new(
        crate::bash::NAME,
        "toolu_curl",
        serde_json::json!({ "command": "curl -sI https://example.com" }),
    );
    let seq = issue(&shared, &call);
    let gate = shared.register_pending(&call, None);
    let control = mounted(&shared).await;

    let err = Decide {
        agent: None,
        seq: "99".into(),
        decision: Decision::Allow,
        message: None,
    }
    .execute(control.clone(), claude_code())
    .await
    .unwrap_err()
    .to_string();
    assert_eq!(
        err,
        format!(
            "gate moved: 99 is not waiting for a decision; now child: waiting for your decision \
             on bash `curl -sI https://example.com` (seq {seq})"
        )
    );

    let said = Decide {
        agent: None,
        seq: seq.clone(),
        decision: Decision::Allow,
        message: None,
    }
    .execute(control, claude_code())
    .await
    .unwrap();
    assert_eq!(
        said,
        "allowed: child runs bash `curl -sI https://example.com`"
    );
    assert_eq!(gate.await, Ok(crate::frontend::Approval::Approve));
    let decided: Vec<_> = shared
        .snapshot()
        .into_iter()
        .filter_map(|e| match e.payload {
            Payload::Telemetry(Telemetry::GateDecision(decision)) => Some(decision),
            _ => None,
        })
        .collect();
    assert_eq!(decided[0].source.as_deref(), Some("mcp[claude-code]"));
}

/// A message while the session asks is refused with the question, over MCP and from
/// the command line alike; naming the question answers it.
#[tokio::test]
async fn every_sender_names_the_question() {
    let shared = child();
    let ask = ToolCall::new(
        crate::process_monitor::ASK,
        "toolu_q",
        serde_json::json!({ "question": "Which branch?" }),
    );
    let seq = issue(&shared, &ask);
    let mut wait = shared.begin_ask("toolu_q", "Which branch?".into(), 1, 1);
    let control = mounted(&shared).await;

    let err = message("main", None)
        .execute(control.clone(), claude_code())
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        format!(
            "not delivered: child is asking ({seq}) \"Which branch?\" — reply with reply_to: \
             \"{seq}\""
        )
    );
    let said = message("main", Some(&seq))
        .execute(control.clone(), claude_code())
        .await
        .unwrap();
    assert_eq!(
        said,
        format!("answered: child has your answer to {seq} \"Which branch?\" and goes on")
    );
    let reply = wait.reply().await.expect("answered");
    assert_eq!(reply.source.as_deref(), Some("mcp[claude-code]"));
    drop(wait);

    let mut wait = shared.begin_ask("toolu_q", "Which branch?".into(), 1, 1);
    let err = message("main", None)
        .execute(control.clone(), RequestContext::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.starts_with("not delivered: child is asking"), "{err}");
    let said = message("main", Some(&seq))
        .execute(control, RequestContext::default())
        .await
        .unwrap();
    assert!(
        said.starts_with("answered: child has your answer"),
        "{said}"
    );
    assert_eq!(wait.reply().await.expect("answered").source, None);
}

/// `POST /turns` answers a pending question only by naming it, whoever sends it:
/// otherwise `409 not_delivered`, the pending ask in `detail`. A turn naming a question
/// that is not pending is refused the same way.
#[tokio::test]
async fn post_turns_holds_every_client_to_naming_its_question() {
    let shared = child();
    shared
        .append(1, Payload::User(vec![ContentBlock::Text("go".into())]))
        .unwrap();
    let ask = ToolCall::new(
        crate::process_monitor::ASK,
        "toolu_q",
        serde_json::json!({ "question": "Proceed?" }),
    );
    let seq = issue(&shared, &ask);
    let _wait = shared.begin_ask("toolu_q", "Proceed?".into(), 1, 1);
    let app = test_server(&shared).await;

    let refused = app
        .post("/v1/turns")
        .with_json_body(&serde_json::json!({ "text": "yes", "source": "session[parent]" }))
        .await;
    refused.assert_status(409);
    let body: Value = serde_json::from_str(refused.body()).unwrap();
    assert_eq!(body["code"], "not_delivered");
    assert_eq!(body["detail"]["call_id"], "toolu_q");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains(&format!("reply with reply_to: \"{seq}\"")),
        "{body}"
    );

    // The human clients too: the TUI (no source) and the web client.
    for body in [
        serde_json::json!({ "text": "yes" }),
        serde_json::json!({ "text": "yes", "source": "web" }),
    ] {
        let refused = app.post("/v1/turns").with_json_body(&body).await;
        refused.assert_status(409);
        let body: Value = serde_json::from_str(refused.body()).unwrap();
        assert_eq!(body["detail"]["call_id"], "toolu_q");
    }

    let wrong = app
        .post("/v1/turns")
        .with_json_body(&serde_json::json!({ "text": "yes", "answers": "0" }))
        .await;
    wrong.assert_status(409);

    let answered = app
        .post("/v1/turns")
        .with_json_body(&serde_json::json!({ "text": "yes", "answers": seq }))
        .await;
    answered.assert_status(202);
    let body: Value = serde_json::from_str(answered.body()).unwrap();
    assert_eq!(body["delivery"], "answered");
}
