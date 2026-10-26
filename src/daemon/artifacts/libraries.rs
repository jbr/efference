//! The libraries built into the binary for artifacts (docs/artifacts.md): the table,
//! and the route that serves each as one script for `efference.load`.

use trillium::{Conn, KnownHeaderName};
use trillium_router::RouterConnExt;

/// A library built into the binary for artifacts, loaded with `efference.load(name)`
/// (docs/artifacts.md). One version each, not the newest: a major version models know
/// well, so an agent writes against an API it has seen. `assets/artifact-libs/README.md`
/// records where each build came from.
pub(crate) struct Library {
    /// What an artifact passes to `efference.load`: the package it gets back.
    pub(crate) name: &'static str,
    /// The version the guide names, packages bundled with it included.
    pub(crate) version: &'static str,
    /// The global the last build defines, which `efference.load` resolves to.
    pub(super) global: &'static str,
    /// UMD builds, served in this order as one script.
    pub(super) builds: &'static [&'static str],
}

pub(crate) const LIBRARIES: &[Library] = &[
    Library {
        name: "chart.js",
        version: "4.4.9",
        global: "Chart",
        builds: &[include_str!(
            "../../../assets/artifact-libs/chart.js/chart.umd.js"
        )],
    },
    Library {
        name: "d3",
        version: "7.9.0",
        global: "d3",
        builds: &[include_str!("../../../assets/artifact-libs/d3/d3.min.js")],
    },
    Library {
        name: "vega-embed",
        version: "6.29.0, with Vega 5.33.1 and Vega-Lite 5.23.0",
        global: "vegaEmbed",
        builds: &[
            include_str!("../../../assets/artifact-libs/vega/vega.min.js"),
            include_str!("../../../assets/artifact-libs/vega-lite/vega-lite.min.js"),
            include_str!("../../../assets/artifact-libs/vega-embed/vega-embed.min.js"),
        ],
    },
];

/// One built-in library as a script the runtime adds to the page: its builds in order,
/// then a call handing `efference.load` the library's global. A name with no library
/// gets a script that rejects the load with the names there are, which the runtime also
/// shows on the page, since the agent that wrote it cannot see it fail. Revalidated by a
/// hash of the script rather than re-sent: a library is the same bytes until an upgrade.
pub(super) async fn library(conn: Conn) -> Conn {
    let name = conn.param("name").unwrap_or_default().to_owned();
    let Some(library) = LIBRARIES.iter().find(|library| library.name == name) else {
        let available: Vec<&str> = LIBRARIES.iter().map(|library| library.name).collect();
        let body = format!(
            "efference.__missing({}, {});\n",
            serde_json::json!(name),
            serde_json::json!(available)
        );
        return conn
            .with_response_header(
                KnownHeaderName::ContentType,
                "text/javascript; charset=utf-8",
            )
            .with_response_header(KnownHeaderName::CacheControl, "no-store")
            .with_response_header("x-content-type-options", "nosniff")
            .with_status(200)
            .with_body(body)
            .halt();
    };
    let body = library_script(library);
    let etag = format!("\"{:016x}\"", {
        let mut hasher = std::hash::DefaultHasher::new();
        std::hash::Hash::hash(&body, &mut hasher);
        std::hash::Hasher::finish(&hasher)
    });
    let conn = conn
        .with_response_header(
            KnownHeaderName::ContentType,
            "text/javascript; charset=utf-8",
        )
        .with_response_header(KnownHeaderName::CacheControl, "no-cache")
        .with_response_header(KnownHeaderName::Etag, etag.clone())
        .with_response_header("x-content-type-options", "nosniff");
    if conn.request_headers().get_str(KnownHeaderName::IfNoneMatch) == Some(etag.as_str()) {
        return conn.with_status(304).halt();
    }
    conn.with_status(200).with_body(body).halt()
}

/// A library's builds as one script, named and versioned in a leading comment for
/// whoever reads it in developer tools, in order, then the call that resolves
/// `efference.load`. Each build loses its `sourceMappingURL` comment: the maps are not
/// vendored, and developer tools fetching one would be refused by `connect-src`.
pub(super) fn library_script(library: &Library) -> String {
    let builds: Vec<&str> = library
        .builds
        .iter()
        .map(|build| without_source_map(build))
        .collect();
    format!(
        "/* {} {}, built into efference (docs/artifacts.md) */\n{}\n;efference.__loaded({}, {});\n",
        library.name,
        library.version,
        builds.join("\n;\n"),
        serde_json::json!(library.name),
        library.global
    )
}

fn without_source_map(build: &str) -> &str {
    let build = build.trim_end();
    match build.rsplit_once('\n') {
        Some((code, last)) if last.starts_with("//# sourceMappingURL=") => code,
        _ => build,
    }
}
