//! Reference validity, plus the two dispatch hooks.
//!
//! The point of compiling the documents in is that they are versioned with the harness and
//! can therefore be *checked* against it. These tests are that check: a config key a
//! document shows, a tool name it tells the reader to call, and a path it points at all
//! have to still exist, so a rename breaks the build rather than the reader.

use super::*;
use crate::config::testing::ConfigFixture;
use serde_json::json;
use std::path::Path;

fn call(name: &str, input: serde_json::Value) -> ToolCall {
    ToolCall::new(name, "toolu_test", input)
}

/// Every fenced ```toml block in a guide has to parse and validate as a real config. This
/// is the config-key half of the reference check, and it needs no separate list of keys:
/// the schema rejects unknown fields, so a renamed key fails here the moment the document
/// still shows the old spelling. A fence titled ```toml ~/.efference/config.toml is the
/// host's inference config and validates against that schema instead.
///
/// The operator's document is not served, but its config examples are held to the same
/// check.
#[test]
fn every_toml_block_in_a_guide_is_a_valid_config() {
    let operating = (
        "docs/operating.md".to_string(),
        include_str!("../../../docs/operating.md"),
    );
    let documents = DOCUMENTS
        .iter()
        .map(|d| (path(d.slug), d.body))
        .chain(std::iter::once(operating));
    let mut checked = 0;
    for (name, body) in documents {
        for (title, block) in toml_blocks(body) {
            checked += 1;
            let parsed = match title {
                "" => crate::config::try_parse_text(&block)
                    .map(drop)
                    .map_err(|e| e.to_string()),
                "~/.efference/config.toml" => {
                    crate::config::user_config::InferenceConfig::from_toml(&block)
                        .map(drop)
                        .map_err(|e| e.to_string())
                }
                other => panic!("unknown toml fence title {other:?} in `{name}`"),
            };
            if let Err(e) = parsed {
                panic!("a ```toml block in `{name}` is not a valid config: {e}\n---\n{block}");
            }
        }
    }
    assert!(
        checked >= 6,
        "expected the guide to show real config; found {checked} toml blocks"
    );
}

/// Every tool name a document tells the reader to call is a tool a turn advertises. The
/// declared list is what a document *claims*; this is the claim measured against
/// `agent::tool_definitions`, which is the same array the request carries — a hosted
/// session's, so the `agents__*` verbs are in it (docs/tools.md).
#[test]
fn every_tool_a_guide_names_is_mounted() {
    let mut toolbox = crate::tools::Toolbox::bash_and_mcps(std::env::current_dir().unwrap(), None)
        .expect("toolbox mounts");
    let host = crate::daemon::Host::new(
        crate::inference::InferenceRegistry::empty(),
        crate::config::env_table::EnvSnapshot::new(Default::default())
            .expect("an empty environment is valid"),
    );
    toolbox.mount_sessions(crate::control_tools::Control::in_session(
        &host,
        "reader".into(),
    ));
    let mounted = crate::agent::callable_names(&toolbox);
    for document in DOCUMENTS {
        for tool in document.tools {
            assert!(
                mounted.iter().any(|m| m == tool),
                "`{}` names the tool `{tool}`, which no turn advertises. Mounted: {}",
                path(document.slug),
                mounted.join(", ")
            );
        }
        // The declaration is only worth having if it tracks the prose, so require that a
        // named tool is actually mentioned — a stale entry left behind after a rewrite
        // would otherwise keep passing the check above forever.
        for tool in document.tools {
            assert!(
                document.body.contains(tool),
                "`{}` declares the tool `{tool}` but never mentions it",
                path(document.slug)
            );
        }
    }
}

/// A document that points at another document points at one that exists — the cross-link
/// half of the check, and the reason a slug rename cannot quietly strand a reader.
#[test]
fn every_guide_link_resolves() {
    for document in DOCUMENTS {
        for link in virtual_links(document.body) {
            assert!(
                lookup(&link).is_some(),
                "`{}` links to `{link}`, which is not a document",
                path(document.slug)
            );
        }
    }
    // And the listing the harness offers on a wrong slug names only real documents.
    for document in DOCUMENTS {
        assert!(listing().contains(&path(document.slug)));
    }
}

/// The stub listing renders as an ordinary `[context]` group: a heading wrapper, the
/// group description, and one `<name path="…">` stub per document (docs/context.md).
#[test]
fn the_guide_renders_as_a_context_group() {
    let resolved = crate::record::log::context::Resolved {
        sections: vec![section(DEFAULT_HEADING)],
    };
    let rendered = resolved.render().expect("a non-empty section renders");
    assert!(rendered.contains(&format!("<{DEFAULT_HEADING}>")));
    assert!(rendered.contains(&format!("</{DEFAULT_HEADING}>")));
    assert!(rendered.contains(&format!("<description>{GROUP_DESCRIPTION}</description>")));
    for document in DOCUMENTS {
        assert!(rendered.contains(&format!(
            "<{slug} path=\"{}\">",
            path(document.slug),
            slug = document.slug
        )));
    }
    // Bodies never ride in the preamble: the whole point of the mount is that these are
    // progressively revealed, so the listing costs one line each.
    assert!(!rendered.contains("## A starter template"));
}

/// A `builtin` entry resolves without touching the disk — the born-resolved path. The
/// operator's key is the heading; their `description`, when they write one, replaces the
/// mount's.
#[test]
fn a_builtin_context_entry_resolves_to_the_listing() {
    let config = crate::config::parse_text("[context]\nHarness = { builtin = \"guide\" }\n");
    let resolved = crate::config::context::resolve(&config.context, Path::new("/nonexistent"))
        .expect("a builtin entry reads no files");
    assert_eq!(resolved.sections.len(), 1);
    assert_eq!(resolved.sections[0].heading.as_deref(), Some("Harness"));
    assert_eq!(resolved.sections[0].items.len(), DOCUMENTS.len());
    assert_eq!(
        resolved.sections[0].description.as_deref(),
        Some(GROUP_DESCRIPTION)
    );

    let custom = crate::config::parse_text(
        "[context]\nHarness = { builtin = \"guide\", description = \"mine\" }\n",
    );
    let resolved =
        crate::config::context::resolve(&custom.context, Path::new("/nonexistent")).unwrap();
    assert_eq!(resolved.sections[0].description.as_deref(), Some("mine"));
}

/// The shapes that must fail the whole file rather than render something empty or
/// ambiguous (docs/context.md).
#[test]
fn a_bad_builtin_entry_fails_the_file() {
    for bad in [
        "[context]\nX = { builtin = \"nosuchthing\" }\n",
        "[context]\nX = { builtin = \"guide\", path = \"docs/\" }\n",
        "[context]\nX = { builtin = \"guide\", exec = \"date\" }\n",
        "[context]\nX = { builtin = \"guide\", update = true }\n",
        "[context]\nX = { builtin = \"guide\", sort = \"name asc\" }\n",
    ] {
        assert!(
            crate::config::try_parse_text(bad).is_err(),
            "expected `{bad}` to be rejected"
        );
    }
}

/// A `read` of a document path is answered from the binary, verbatim.
#[test]
fn a_read_of_a_document_is_served_from_the_binary() {
    let result = read_documents(&call("read", json!({ "paths": [path("config-format")] })))
        .expect("a virtual read is intercepted");
    assert!(!result.is_error);
    assert_eq!(
        result.content,
        lookup(&path("config-format")).unwrap().body.trim_end()
    );

    // Several at once concatenate, in the order asked.
    let both = read_documents(&call(
        "read",
        json!({ "paths": [path("editing"), path("new-project")] }),
    ))
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
    assert!(read_documents(&call("read", json!({ "paths": ["src/lib.rs"] }))).is_none());
}

/// A wrong slug is answered by the overlay, not handed to the filesystem, and the answer
/// says what does exist.
#[test]
fn an_unknown_slug_lists_the_documents() {
    let result = read_documents(&call(
        "read",
        json!({ "paths": ["efference://guide/how-to-fly"] }),
    ))
    .expect("intercepted");
    assert!(result.is_error);
    assert!(result.content.contains(&path("config-format")));
}

/// A batch mixing a document and a file is refused whole, naming the file — the stated
/// edge, answered in one corrective turn rather than half-served.
#[test]
fn a_mixed_batch_is_refused() {
    let result = read_documents(&call(
        "read",
        json!({ "paths": [path("editing"), "src/lib.rs"] }),
    ))
    .expect("intercepted");
    assert!(result.is_error);
    assert!(result.content.contains("src/lib.rs"));
}

/// Only `read` resolves on the prefix; everything else is refused ahead of the permission
/// cascade, with the paths that do work.
#[test]
fn only_read_resolves_on_the_prefix() {
    for (name, input) in [
        ("write", json!({ "path": path("editing"), "contents": "x" })),
        ("edit", json!({ "file_path": path("editing") })),
        ("list", json!({ "path": MOUNT })),
        ("search", json!({ "paths": [MOUNT] })),
    ] {
        let result = reject_non_read(&call(name, input))
            .unwrap_or_else(|| panic!("`{name}` on the prefix should be refused"));
        assert!(result.is_error);
        assert!(result.content.contains(&path("new-project")));
    }
    // A read passes through this hook to the one that serves it, and a real path is
    // untouched by either.
    assert!(reject_non_read(&call("read", json!({ "paths": [path("editing")] }))).is_none());
    assert!(reject_non_read(&call("write", json!({ "path": "src/lib.rs" }))).is_none());
}

/// The prefix is read-granted with no config at all — that is what makes the documents the
/// recovery path for a session whose config knows nothing else — and is never
/// write-granted (docs/permissions.md).
#[test]
fn the_prefix_is_read_granted_and_never_write_granted() {
    let empty = crate::config::Config::default();
    assert_eq!(
        crate::config::policy::classify(
            &crate::config::policy::Directories::at(&std::env::current_dir().unwrap()),
            &empty,
            &call("read", json!({ "paths": [path("new-project")] }))
        ),
        crate::config::policy::Verdict::Clear
    );
    // A batch mixing a granted file and a document clears; the ungranted file still
    // poisons it, exactly as any other mixed batch.
    let granted = ConfigFixture::new()
        .with_tool("read")
        .with_path("/work", true, true)
        .build();
    assert_eq!(
        crate::config::policy::classify(
            &crate::config::policy::Directories::at(&std::env::current_dir().unwrap()),
            &granted,
            &call("read", json!({ "paths": [path("editing"), "/work/x.rs"] }))
        ),
        crate::config::policy::Verdict::Clear
    );
    assert_eq!(
        crate::config::policy::classify(
            &crate::config::policy::Directories::at(&std::env::current_dir().unwrap()),
            &granted,
            &call(
                "read",
                json!({ "paths": [path("editing"), "/elsewhere/x.rs"] })
            )
        ),
        crate::config::policy::Verdict::Gate
    );
    // No config can grant a write to the mount; there is nothing on disk to write.
    let permissive = ConfigFixture::new()
        .with_tool("write")
        .with_path("/", true, true)
        .build();
    assert_eq!(
        crate::config::policy::classify(
            &crate::config::policy::Directories::at(&std::env::current_dir().unwrap()),
            &permissive,
            &call("write", json!({ "path": path("editing"), "contents": "x" }))
        ),
        crate::config::policy::Verdict::Gate
    );
}

/// Extract the contents of every ```toml fence in a document.
/// Each ```toml fence with its title (the text after `toml`, trimmed; empty for
/// a plain fence) and its body.
fn toml_blocks(body: &str) -> Vec<(&str, String)> {
    let mut blocks = Vec::new();
    let mut current: Option<(&str, Vec<&str>)> = None;
    for line in body.lines() {
        let line = line.trim_end();
        match &mut current {
            None => {
                if let Some(title) = line.strip_prefix("```toml")
                    && (title.is_empty() || title.starts_with(' '))
                {
                    current = Some((title.trim(), Vec::new()));
                }
            }
            Some(_) if line == "```" => {
                let (title, lines) = current.take().unwrap();
                blocks.push((title, lines.join("\n")));
            }
            Some((_, lines)) => lines.push(line),
        }
    }
    blocks
}

/// Every `efference://…` path a document's prose points at, stripped of trailing
/// punctuation and markup.
fn virtual_links(body: &str) -> Vec<String> {
    body.match_indices(SCHEME)
        .map(|(at, _)| {
            body[at..]
                .split(|c: char| c.is_whitespace() || matches!(c, '`' | ')' | '"' | '>'))
                .next()
                .unwrap_or_default()
                .trim_end_matches(['.', ','])
                .to_string()
        })
        .collect()
}

/// `guide/<slug>` rides one document in full under the operator's label, and an
/// unknown slug fails the file rather than rendering nothing.
#[test]
fn a_builtin_slug_inlines_that_document() {
    let config = crate::config::parse_text(
        "[context]\n\"Setting up\" = { builtin = \"guide/new-project\" }\n",
    );
    let resolved = crate::config::context::resolve(&config.context, Path::new("/nonexistent"))
        .expect("a builtin entry reads no files");
    let rendered = resolved.render().expect("renders");
    assert!(rendered.contains("<Setting up>"), "{rendered}");
    assert!(
        rendered.contains("## A starter template"),
        "the body rides in full"
    );
    let err = crate::config::Config::from_toml(
        "[context]\nX = { builtin = \"guide/no-such-document\" }\n",
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("guide/<slug>"), "{err}");
}

/// The variables the artifacts guide tells an agent to use are ones every artifact's
/// stylesheet defines (docs/artifacts.md): a renamed token fails here rather than leaving
/// an agent's colours unset.
#[test]
fn every_variable_the_artifacts_guide_names_is_defined() {
    let tokens = include_str!("../../../web/src/theme/tokens.css");
    let guide = DOCUMENTS
        .iter()
        .find(|document| document.slug == "artifacts")
        .expect("the artifacts guide is listed");
    let mut named = Vec::new();
    let mut rest = guide.body;
    while let Some(start) = rest.find("`--") {
        let name = &rest[start + 1..];
        let end = name.find('`').expect("a closed code span");
        named.push(&name[..end]);
        rest = &name[end..];
    }
    assert!(named.len() > 10, "{named:?}");
    for name in named {
        assert!(
            tokens.contains(&format!("{name}:")),
            "the artifacts guide names {name}, which web/src/theme/tokens.css does not define"
        );
    }
}

/// The artifacts guide lists exactly the libraries `efference.load` serves, each with
/// the version it is: an agent writes against the API the guide names
/// (docs/artifacts.md).
#[test]
fn the_artifacts_guide_lists_the_built_in_libraries() {
    let guide = DOCUMENTS
        .iter()
        .find(|document| document.slug == "artifacts")
        .expect("the artifacts guide is listed")
        .body;
    let mut listed: Vec<&str> = guide
        .match_indices("efference.load(\"")
        .map(|(at, prefix)| {
            let rest = &guide[at + prefix.len()..];
            &rest[..rest.find('"').expect("a closed name")]
        })
        .collect();
    listed.sort_unstable();
    listed.dedup();
    let built_in: Vec<&str> = crate::daemon::artifacts::libraries::LIBRARIES
        .iter()
        .map(|library| library.name)
        .collect();
    assert_eq!(listed, built_in, "efference.load names in the guide");
    // A version may wrap across the guide's lines.
    let flat = guide.split_whitespace().collect::<Vec<_>>().join(" ");
    for library in crate::daemon::artifacts::libraries::LIBRARIES {
        assert!(
            flat.contains(library.version),
            "the guide names {} {}",
            library.name,
            library.version
        );
    }
}

/// The session preamble says a `mermaid` fence renders and with which version, so an
/// agent writing one without reading the guide knows which diagram types and syntax it
/// has (docs/artifacts.md).
#[test]
fn the_preamble_names_the_built_in_mermaid_version() {
    let mermaid = crate::daemon::artifacts::libraries::LIBRARIES
        .iter()
        .find(|library| library.name == "mermaid")
        .expect("Mermaid is built in");
    let preamble = include_str!("../../../assets/prompts/session.md");
    let flat = preamble.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains(&format!("Mermaid {}", mermaid.version)),
        "the preamble names Mermaid {}",
        mermaid.version
    );
}
