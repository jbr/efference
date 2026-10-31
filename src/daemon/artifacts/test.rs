use super::*;
use crate::config::testing::ConfigFixture;
use crate::daemon::SessionParts;
use crate::log::{AssistantOutput, ContentBlock, Exclusion};
use crate::persist::Log;
use trillium_testing::TestServer;

fn assistant(text: &str, excluded: Option<Exclusion>) -> Payload {
    let output = AssistantOutput::from_legacy_messages(
        Vec::new(),
        vec![ContentBlock::Text(text.to_string())],
        Vec::new(),
    );
    match excluded {
        Some(why) => Payload::record_only(output, why),
        None => Payload::assistant(output),
    }
}

/// A session with an id to address it by, as a session bind serves one.
fn session(id: Option<&str>) -> Arc<Session> {
    let cwd = std::env::current_dir().unwrap();
    Session::full(SessionParts {
        id: id.map(str::to_owned),
        ..SessionParts::new(crate::policy::Directories::at(&cwd), Log::in_memory())
    })
    .0
}

async fn session_server(session: &Arc<Session>) -> TestServer<impl trillium::Handler> {
    let session = Arc::clone(session);
    TestServer::new((
        trillium::Init::new(move |info: trillium::Info| {
            let session = Arc::clone(&session);
            async move { info.with_shared_state(session) }
        }),
        routes(),
    ))
    .await
}

const TWO_FENCES: &str =
    "```html-artifact\n<main>first</main>\n```\n\n```html-artifact\n<main>second</main>\n```\n";

/// Only a committed assistant entry of an addressable session has links: a user's
/// fence, a record-only entry, and a session with no id have none.
#[test]
fn links_belong_to_committed_assistant_text_of_an_addressable_session() {
    let work = session(Some("work"));
    let assistant_seq = work.append(1, assistant(TWO_FENCES, None)).unwrap();
    let user_seq = work.append(1, Payload::user(TWO_FENCES)).unwrap();
    let excluded_seq = work
        .append(1, assistant(TWO_FENCES, Some(Exclusion::Empty)))
        .unwrap();
    let entry = |seq| work.entry_at(seq).unwrap();

    let links = links(&work, &entry(assistant_seq));
    assert_eq!(links.len(), 2);
    assert!(
        links[1]
            .url
            .starts_with(&format!("/artifacts/work/{assistant_seq}/0/1/"))
    );
    assert_eq!(links[1].external_url, None, "no host, so no listener");
    assert!(super::links(&work, &entry(user_seq)).is_empty());
    assert!(super::links(&work, &entry(excluded_seq)).is_empty());

    let anonymous = session(None);
    let seq = anonymous.append(1, assistant(TWO_FENCES, None)).unwrap();
    assert!(super::links(&anonymous, &anonymous.entry_at(seq).unwrap()).is_empty());
}

/// A document is served only for a tag its session signed over exactly those
/// coordinates; another session's key, another fence's tag, or a different id in
/// the path all answer 404.
#[tokio::test]
async fn a_document_verifies_its_tag_under_the_named_sessions_key() {
    let work = session(Some("work"));
    let seq = work.append(1, assistant(TWO_FENCES, None)).unwrap();
    let links = links(&work, &work.entry_at(seq).unwrap());
    let server = session_server(&work).await;

    let second = server.get(&links[1].url).await;
    second.assert_status(200);
    assert!(second.body().contains("<main>second</main>"));
    assert!(!second.body().contains("<main>first</main>"));
    assert_eq!(
        second
            .response_headers()
            .get_str(KnownHeaderName::ContentSecurityPolicy),
        Some(ARTIFACT_CSP)
    );

    let (_, first_tag) = links[0].url.rsplit_once('/').unwrap();
    let (second_path, second_tag) = links[1].url.rsplit_once('/').unwrap();
    let mut flipped = second_tag.to_owned().into_bytes();
    flipped[0] = if flipped[0] == b'A' { b'B' } else { b'A' };
    for path in [
        format!("{second_path}/{first_tag}"),
        format!("{second_path}/{}", String::from_utf8(flipped).unwrap()),
        links[1]
            .url
            .replacen("/artifacts/work/", "/artifacts/other/", 1),
        format!("/artifacts/work/{}/0/1/{second_tag}", seq + 1),
    ] {
        server.get(&path).await.assert_status(404);
    }

    // The same coordinates in another session sign to another tag.
    let other = session(Some("work"));
    let other_seq = other.append(1, assistant(TWO_FENCES, None)).unwrap();
    assert_eq!(other_seq, seq);
    assert_ne!(
        super::links(&other, &other.entry_at(seq).unwrap())[1].url,
        links[1].url
    );
}

/// A `mermaid` fence is numbered with the `html-artifact` fences around it, its link
/// says so, and its document is a page the host writes: the source as a JSON string
/// no source can break out of, then the script that draws it (docs/artifacts.md).
#[tokio::test]
async fn a_mermaid_fence_is_served_inside_a_page_that_draws_it() {
    let work = session(Some("work"));
    let text = "```html-artifact\n<main>page</main>\n```\n\n\
                ```mermaid\ngraph TD\n  A[\"</script><script>alert(1)</script>\"] --> B\n```\n";
    let seq = work.append(1, assistant(text, None)).unwrap();
    let links = links(&work, &work.entry_at(seq).unwrap());
    assert_eq!(
        links
            .iter()
            .map(|link| (link.fence, link.kind))
            .collect::<Vec<_>>(),
        [(0, Kind::Html), (1, Kind::Mermaid)]
    );
    assert_eq!(serde_json::to_value(&links[1]).unwrap()["kind"], "mermaid");
    let server = session_server(&work).await;

    let page = server.get(&links[1].url).await;
    page.assert_status(200);
    assert_eq!(
        page.body(),
        "<!doctype html>\
         <link rel=\"stylesheet\" href=\"/artifacts/_efference/theme.css\">\
         <script src=\"/artifacts/_efference/runtime.js\"></script>\
         <div id=\"efference-mermaid\"></div>\
         <script type=\"application/json\" id=\"efference-mermaid-source\">\
         \"graph TD\\n  A[\\\"\\u003c/script>\\u003cscript>alert(1)\\u003c/script>\\\"] --> B\\n\"\
         </script>\
         <script src=\"/artifacts/_efference/mermaid.js\"></script>"
    );
    assert_eq!(
        page.response_headers()
            .get_str(KnownHeaderName::ContentSecurityPolicy),
        Some(ARTIFACT_CSP)
    );
    // The page's JSON reads back as the fence's source.
    let json = page
        .body()
        .split_once("id=\"efference-mermaid-source\">")
        .and_then(|(_, rest)| rest.split_once("</script>"))
        .unwrap()
        .0;
    assert_eq!(
        serde_json::from_str::<String>(json).unwrap(),
        "graph TD\n  A[\"</script><script>alert(1)</script>\"] --> B\n"
    );
}

/// A scratch working directory, removed when dropped.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Self {
        use std::hash::{BuildHasher, Hasher, RandomState};
        let nonce = RandomState::new().build_hasher().finish();
        let path = std::env::temp_dir().join(format!("efference-artifact-files-{nonce:016x}"));
        std::fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `efference.file` hands an artifact a file's text only as its session's own `read`
/// would get it without asking, and says why when it does not; a path whose tag does
/// not verify is no artifact at all (docs/artifacts.md).
#[tokio::test]
async fn an_artifact_gets_a_file_only_as_its_session_could_read_it() {
    let scratch = Scratch::new();
    std::fs::write(scratch.0.join("sizes.csv"), "crate,lines\ncore,41\n").unwrap();
    std::fs::create_dir(scratch.0.join("dir")).unwrap();
    std::fs::write(scratch.0.join("binary.dat"), [0xff, 0xfe, 0x00]).unwrap();
    // A role that grants reading the working directory, as a project config would.
    let role = ConfigFixture::new()
        .with_tool("read")
        .with_path(".", true, false)
        .build();
    let work = Session::full(SessionParts {
        id: Some("work".into()),
        role: crate::config::LoadedConfig {
            role,
            ..crate::config::LoadedConfig::default()
        },
        ..SessionParts::new(crate::policy::Directories::at(&scratch.0), Log::in_memory())
    })
    .0;
    let seq = work.append(1, assistant(TWO_FENCES, None)).unwrap();
    let url = links(&work, &work.entry_at(seq).unwrap())[0].url.clone();
    let server = session_server(&work).await;
    let file = |url: &str, path: &str| {
        let query: String =
            trillium_client::url::form_urlencoded::byte_serialize(path.as_bytes()).collect();
        format!("{url}/file?path={query}")
    };

    let served = server.get(&file(&url, "sizes.csv")).await;
    served.assert_status(200);
    assert_eq!(
        served.body(),
        "efference.__file(\"sizes.csv\", \"crate,lines\\ncore,41\\n\");\n"
    );

    for (path, reason) in [
        ("missing.csv", "no such file"),
        ("dir", "not a file"),
        ("binary.dat", "not UTF-8 text"),
        ("", "no path given"),
        ("/etc/hosts", "this session cannot read it without asking"),
    ] {
        let refused = server.get(&file(&url, path)).await;
        refused.assert_status(200);
        assert_eq!(
            refused.body(),
            format!(
                "efference.__fileRefused({}, {});\n",
                serde_json::json!(path),
                serde_json::json!(reason)
            ),
            "{path:?}"
        );
    }

    let (path, tag) = url.rsplit_once('/').unwrap();
    let forged = format!("{path}/{}", tag.chars().rev().collect::<String>());
    server
        .get(&file(&forged, "sizes.csv"))
        .await
        .assert_status(404);
}

#[tokio::test]
async fn the_shared_assets_are_served_without_a_tag() {
    let server = session_server(&session(Some("work"))).await;
    let runtime = server.get("/artifacts/_efference/runtime.js").await;
    runtime.assert_status(200);
    assert_eq!(runtime.body(), RUNTIME_JS);
    let mermaid = server.get("/artifacts/_efference/mermaid.js").await;
    mermaid.assert_status(200);
    assert_eq!(mermaid.body(), MERMAID_JS);
    let theme = server.get("/artifacts/_efference/theme.css").await;
    theme.assert_status(200);
    assert_eq!(theme.body(), THEME_CSS);
    server
        .get("/artifacts/_efference/private.js")
        .await
        .assert_status(404);
}

/// A built-in library is one script: its builds in order, then the call that hands
/// `efference.load` its global. Revalidation by version answers 304, and a name with
/// no library gets a script that names the ones there are (docs/artifacts.md).
#[tokio::test]
async fn built_in_libraries_load_as_one_script_and_a_missing_one_names_the_rest() {
    let server = session_server(&session(Some("work"))).await;

    let d3 = server.get("/artifacts/_efference/lib/d3").await;
    d3.assert_status(200);
    assert!(d3.body().ends_with("\n;efference.__loaded(\"d3\", d3);\n"));
    let etag = d3
        .response_headers()
        .get_str(KnownHeaderName::Etag)
        .unwrap()
        .to_owned();
    server
        .get("/artifacts/_efference/lib/d3")
        .with_request_header(KnownHeaderName::IfNoneMatch, etag)
        .await
        .assert_status(304);

    let vega = server.get("/artifacts/_efference/lib/vega-embed").await;
    vega.assert_status(200);
    // vega-embed's build expects `vega` and `vegaLite` defined before it runs.
    let embed = libraries::LIBRARIES
        .iter()
        .find(|library| library.name == "vega-embed")
        .unwrap();
    let defines: Vec<bool> = ["vega", "vegaLite", "vegaEmbed"]
        .iter()
        .zip(embed.builds)
        .map(|(global, build)| build.contains(&format!(").{global}=")))
        .collect();
    assert_eq!(defines, [true, true, true], "builds in dependency order");
    assert_eq!(vega.body(), libraries::library_script(embed));
    assert!(
        vega.body()
            .ends_with("\n;efference.__loaded(\"vega-embed\", vegaEmbed);\n")
    );
    // The builds name source maps that are not vendored; developer tools would fetch
    // them and be refused by `connect-src`.
    assert!(
        embed
            .builds
            .iter()
            .all(|build| build.contains("sourceMappingURL"))
    );
    assert!(!vega.body().contains("sourceMappingURL"));

    let missing = server.get("/artifacts/_efference/lib/d4").await;
    missing.assert_status(200);
    assert_eq!(
        missing.body(),
        "efference.__missing(\"d4\", [\"chart.js\",\"d3\",\"mermaid\",\"vega-embed\"]);\n"
    );
}

#[test]
fn only_exact_artifact_shapes_are_artifact_routes() {
    let tag = "A".repeat(TAG_LENGTH);
    for (path, artifact) in [
        (format!("/artifacts/brisk-otter/12/0/3/{tag}"), true),
        (format!("/artifacts/1787453029792/0/0/0/{tag}"), true),
        ("/artifacts/_efference/runtime.js".into(), true),
        ("/artifacts/_efference/theme.css".into(), true),
        ("/artifacts/_efference/mermaid.js".into(), true),
        ("/artifacts/_efference/private.js".into(), false),
        ("/artifacts/_efference/lib/d3".into(), true),
        ("/artifacts/_efference/lib/chart.js".into(), true),
        ("/artifacts/_efference/lib/".into(), false),
        ("/artifacts/_efference/lib/..".into(), false),
        ("/artifacts/_efference/lib/d3/extra".into(), false),
        ("/artifacts/_efference/runtime.js/".into(), false),
        (format!("/artifacts/brisk-otter/12/0/3/{tag}/"), false),
        (format!("/artifacts/brisk-otter/12/0/3/{tag}/file"), true),
        (
            format!("/artifacts/brisk-otter/12/0/3/{tag}/file/more"),
            false,
        ),
        (format!("/artifacts/brisk-otter/12/0/3/{tag}/other"), false),
        (
            format!("/artifacts/brisk-otter/12/0/3/{}", &tag[1..]),
            false,
        ),
        (format!("/artifacts/brisk-otter/x/0/3/{tag}"), false),
        (format!("/artifacts/../12/0/3/{tag}"), false),
        (format!("/artifacts/_efference/12/0/3/{tag}"), false),
        (
            format!("/sessions/brisk-otter/artifacts/brisk-otter/12/0/3/{tag}"),
            false,
        ),
        ("/artifacts".into(), false),
    ] {
        assert_eq!(is_artifact_route(&path), artifact, "{path}");
    }
}

/// The runtime opens the document, and a source with no doctype gets one: a fragment
/// would otherwise render in quirks mode, where the root element is stretched to the
/// frame and the frame can never shrink to its content (docs/artifacts.md).
#[test]
fn the_runtime_follows_the_doctype_and_a_missing_doctype_is_added() {
    for (source, served) in [
        (
            "<svg viewBox=\"0 0 4 1\"></svg>",
            format!("<!doctype html>{INJECTED}<svg viewBox=\"0 0 4 1\"></svg>"),
        ),
        (
            "<!DOCTYPE html>\n<html><head><title>t</title></head></html>",
            format!("<!DOCTYPE html>{INJECTED}\n<html><head><title>t</title></head></html>"),
        ),
        // Whitespace and comments may precede a doctype; a byte-order mark is dropped.
        (
            "\u{feff}\n<!-- drawn by an agent -->\n<!doctype html><p>x</p>",
            format!("\n<!-- drawn by an agent -->\n<!doctype html>{INJECTED}<p>x</p>"),
        ),
        // A doctype after content is one the parser ignores, so it does not count.
        (
            "<p>first</p><!doctype html>",
            format!("<!doctype html>{INJECTED}<p>first</p><!doctype html>"),
        ),
    ] {
        assert_eq!(inject_runtime(source), served, "{source:?}");
    }
}
