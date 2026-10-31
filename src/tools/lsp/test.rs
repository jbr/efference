use super::*;
use crate::config::Config;

#[test]
fn the_namespace_serves_definition_references_and_rename() {
    assert_eq!(
        wire_names().collect::<Vec<_>>(),
        ["lsp__definition", "lsp__references", "lsp__rename"]
    );
}

/// Off unless an entry enables the namespace or one of its tools, so a session that did not
/// ask binds nothing and starts no server.
#[test]
fn the_tools_are_mounted_only_when_the_config_enables_them() {
    let role = |text: &str| Config::from_toml(text).unwrap();
    assert!(!enabled(&role("")));
    assert!(!enabled(&role("[tools]\nefference = { enabled = true }\n")));
    assert!(enabled(&role("[tools]\nlsp = { enabled = true }\n")));
    assert!(enabled(&role(
        "[tools]\n\"lsp.references\" = { enabled = true }\n"
    )));
    assert!(!enabled(&role(
        "[tools]\nlsp = { enabled = true }\n\"lsp.definition\" = { enabled = false }\n\
         \"lsp.references\" = { enabled = false }\n\"lsp.rename\" = { enabled = false }\n"
    )));
}

/// What the tools advertise is read where no repository exists, so it names no path in one.
/// An example's arguments are a caller's project, not a citation (`within: "src/agent"`), so
/// they are left out.
#[test]
fn advertised_tools_name_no_repository_path() {
    fn without_examples(value: &mut Value) {
        match value {
            Value::Object(map) => {
                map.remove("examples");
                map.values_mut().for_each(without_examples);
            }
            Value::Array(items) => items.iter_mut().for_each(without_examples),
            _ => {}
        }
    }
    let paths = regex::Regex::new(r"\b(docs|backlog|journal|process|src)/[\w./-]+").unwrap();
    let mut tools = serde_json::to_value(<Tools as AsToolsList>::tools_list()).unwrap();
    without_examples(&mut tools);
    let advertised = tools.to_string();
    if let Some(found) = paths.find(&advertised) {
        panic!(
            "the lsp tools name the repository path {:?}: {}",
            found.as_str(),
            &advertised[found.start().saturating_sub(120)..found.end()]
        );
    }
}

/// A directory with no project in it lays out no server, so binding starts nothing and a
/// query answers at once with what is missing. The mount, the namespace and the dispatch are
/// efference's; what a server answers is structural's to test.
#[tokio::test]
async fn a_bound_session_mounts_the_tools_and_answers_without_a_server() {
    let dir = std::env::temp_dir().join(format!("eff-lsp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let servers = language_servers();

    let lsp = Lsp::bind(Arc::clone(&servers), "s1", dir.clone())
        .await
        .unwrap();
    let mut toolbox = Toolbox::bash_and_mcps(dir.clone(), None).unwrap();
    toolbox.mount_lsp(lsp);

    let names = toolbox.names();
    let mounted: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|name| name.starts_with("lsp__"))
        .collect();
    assert_eq!(mounted, wire_names().collect::<Vec<_>>());
    let once = serde_json::to_string(&toolbox.definitions()).unwrap();
    let twice = serde_json::to_string(&toolbox.definitions()).unwrap();
    assert_eq!(once, twice);
    let entry = toolbox.tool_entry("lsp__definition").expect("mounted");
    assert_eq!(entry.name, "lsp__definition");
    assert!(entry.input_schema["properties"]["intent"].is_object());

    let call = ToolCall::new(
        "lsp__definition",
        "c1",
        serde_json::json!({ "symbol": "main", "intent": "find main" }),
    );
    let Outcome::Done(result) = toolbox
        .run(&call, std::future::pending(), |_| {}, |_| {}, |_| {})
        .await
    else {
        panic!("an lsp call does not detach");
    };
    let text = &result.content;
    assert!(!text.contains("invalid input"), "{text}");
    assert!(!text.contains("unknown tool"), "{text}");

    drop(toolbox);
    tokio::time::timeout(std::time::Duration::from_secs(30), shut_down(servers))
        .await
        .expect("nothing was running, so stopping is prompt");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A kill abandons a call still waiting, with an error result that pairs its `tool_use`.
#[tokio::test]
async fn a_kill_abandons_the_call() {
    let dir = std::env::temp_dir().join(format!("eff-lsp-kill-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let lsp = Lsp::bind(language_servers(), "s1", dir.clone())
        .await
        .unwrap();
    let call = ToolCall::new(
        "lsp__references",
        "c1",
        serde_json::json!({ "symbol": "x" }),
    );
    let result = lsp.run(&call, async { Interrupt::Kill }).await;
    assert!(result.is_error, "{}", result.content);
    assert!(
        result.content.contains("cancelled by the operator"),
        "{}",
        result.content
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
