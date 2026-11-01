use super::*;
use serde_json::json;

fn bash(command: &str) -> ToolCall {
    ToolCall::new("bash", "c", json!({ "command": command }))
}

#[test]
fn a_bash_family_is_the_program_after_cd_and_assignments() {
    let family = |command: &str| family(&bash(command));
    assert_eq!(family("cargo test"), "bash:cargo");
    assert_eq!(family("  cargo test --lib"), "bash:cargo");
    assert_eq!(family("cd /work/proj && cargo test"), "bash:cargo");
    assert_eq!(family("cd /work/proj ; git status"), "bash:git");
    assert_eq!(family("RUST_LOG=debug FOO=1 cargo run"), "bash:cargo");
    assert_eq!(family("cd /w && RUST_LOG=debug /usr/bin/rg x"), "bash:rg");
    assert_eq!(family("FOO=1"), "bash:?");
    assert_eq!(family(""), "bash:?");
    // `cd` with no `&&` is the program itself.
    assert_eq!(family("cd /work/proj"), "bash:cd");
}

#[test]
fn any_other_tool_is_its_own_family_by_display_name() {
    let call = ToolCall::new("fs__read", "c", json!({ "path": "a" }));
    assert_eq!(family(&call), "fs.read");
}

#[test]
fn a_bash_call_renders_as_the_raw_command() {
    assert_eq!(plaintext(&bash("ls -la")), "$ ls -la");
    assert_eq!(
        plaintext(&bash("cat <<'EOF'\nhello\nEOF")),
        "$ cat <<'EOF'\n  hello\n  EOF"
    );
}

#[test]
fn another_tool_renders_as_its_name_and_argument_lines() {
    let call = ToolCall::new(
        "fs__edit",
        "c",
        json!({
            "path": "src/lib.rs",
            "count": 3,
            "text": "one\ntwo",
            INTENT: "a plea",
        }),
    );
    assert_eq!(
        plaintext(&call),
        "fs.edit\n  count: 3\n  path: src/lib.rs\n  text: one\n    two"
    );
}

#[test]
fn candidates_are_the_ten_most_recent_of_the_same_family() {
    let consented: Vec<ToolCall> = (0..14)
        .map(|i| {
            let command = if i % 7 == 6 {
                "git status".to_string()
            } else {
                format!("cargo test {i}")
            };
            ToolCall::new("bash", format!("c{i}"), json!({ "command": command }))
        })
        .collect();
    let chosen = candidates(&bash("cargo build"), &consented);
    let ids: Vec<&str> = chosen.iter().map(|c| c.id.0.as_str()).collect();
    // 14 calls, two of them git; the twelve cargo calls are trimmed to the last ten.
    assert_eq!(
        ids,
        [
            "c2", "c3", "c4", "c5", "c7", "c8", "c9", "c10", "c11", "c12"
        ]
    );
    assert!(candidates(&bash("rg x"), &consented).is_empty());
}

#[test]
fn the_state_is_the_move_and_the_working_directory() {
    let state = state(&bash("ls"), Some(Path::new("/work")));
    assert_eq!(
        state,
        json!({ "move": "$ ls", "working_directory": "/work" })
    );
    assert_eq!(
        super::state(&bash("ls"), None)["working_directory"],
        Value::Null
    );
}

/// One Choice per candidate and per option order, the candidate inlined in the
/// instructions object, the options in the order the experiment validated and its
/// reverse.
#[test]
fn each_candidate_is_asked_in_both_option_orders() {
    let earlier = bash("cargo test");
    let asked = questions(&[&earlier]);
    assert_eq!(
        asked.keys().map(String::as_str).collect::<Vec<_>>(),
        ["rel_0", "relr_0"]
    );
    let forward = serde_json::to_string(&asked["rel_0"]).expect("serializes");
    let reversed = serde_json::to_string(&asked["relr_0"]).expect("serializes");
    assert!(forward.starts_with(
        r#"{"type":"choice","instructions":{"question":"How does the move in the state relate to this earlier call?","earlier_call":"[OPERATOR_APPROVED] $ cargo test"},"criteria":{"same":"#
    ));
    assert!(
        forward.contains(r#","wider":"#)
            && forward.ends_with(r#","different":"A different kind of action altogether."}}"#)
    );
    assert!(
        reversed.contains(
            r#""criteria":{"different":"A different kind of action altogether.","wider":"#
        )
    );
    assert!(reversed.contains(r#","same":"The same action on the same kind of target"#));
}

fn relation_answers(a: f64, b: f64) -> Decisions {
    let choice = |same: f64| {
        json!({
            "type": "choice",
            "probabilities": { "same": same, "wider": (1.0 - same) * 0.5, "different": (1.0 - same) * 0.5 }
        })
    };
    serde_json::from_value(json!({
        "answers": {
            "rel_0": choice(a),
            "relr_0": choice(b),
            // Candidate 1 answered in one order only.
            "rel_1": choice(0.9),
        },
        "usage": {},
    }))
    .expect("parses")
}

/// A reading is the mean of the two option orders, so a candidate the model reads
/// as `same` in one order only lands on the threshold, and one it did not read
/// twice counts for nothing.
#[test]
fn a_reading_is_the_mean_of_both_orders() {
    assert_eq!(read(&relation_answers(0.9, 0.9), 2).same, 1);
    assert_eq!(read(&relation_answers(0.9, 0.1), 2).same, 1);
    assert_eq!(read(&relation_answers(0.8, 0.1), 2).same, 0);
    let reading = read(&relation_answers(0.9, 0.9), 2);
    assert_eq!(reading.candidates, 2);
    assert_eq!(reading.wider, 0);
}

#[test]
fn wider_is_counted_apart_from_same() {
    let wider = |w: f64| {
        json!({
            "type": "choice",
            "probabilities": { "same": 0.1, "wider": w, "different": 0.9 - w }
        })
    };
    let decisions: Decisions = serde_json::from_value(json!({
        "answers": { "rel_0": wider(0.7), "relr_0": wider(0.5) },
        "usage": {},
    }))
    .expect("parses");
    let reading = read(&decisions, 1);
    assert_eq!((reading.same, reading.wider), (0, 1));
}

/// The record keeps each candidate's figures under its call id, so a clear can be
/// traced to the approved call it matched and a different cut replayed.
#[test]
fn compared_names_each_candidate_with_its_means() {
    let first = ToolCall::new("bash", "call_a", json!({"command": "cargo test"}));
    let second = ToolCall::new("bash", "call_b", json!({"command": "cargo build"}));
    // The second candidate was answered in one order only, so it reads as zero.
    let decisions = relation_answers(0.9, 0.7);
    let compared = compared(&decisions, &[&first, &second]);
    assert_eq!(
        compared
            .iter()
            .map(|c| c.call_id.0.as_str())
            .collect::<Vec<_>>(),
        ["call_a", "call_b"]
    );
    assert!((compared[0].same - 0.8).abs() < 1e-9);
    assert_eq!(compared[1].same, 0.0);
}
