//! The control channel's JSON Schema (docs/web-client.md): every type the channel carries,
//! described as the host serializes it, committed beside the TypeScript generated from it.
//!
//! The schema is draft-07 under schemars' `Serialize` contract. Draft-07 because
//! json-schema-to-typescript reads the draft-2020-12 spelling of a tuple and of a `$ref`
//! with siblings by dropping the fields they carry; the `Serialize` contract because the
//! host's output is what a client reads. The derive is `cfg(test)` on every type here, so
//! the schema is built by this test and the binary does not carry it.
//!
//! The roots are the types a client receives or sends over the channel. A request body is
//! a root as well: under the `Serialize` contract a defaulted field is required, so a
//! client that builds a body from its type sends more than the host needs and never less.
//!
//! A log entry is described as it is stored. The channel serves it through `EntryView`,
//! which leaves out a result image's bytes and adds `selection` and `artifacts`
//! (`entry_view.rs`); the web client declares those three beside the generated types.

use std::path::Path;

use schemars::Schema;
use schemars::generate::SchemaSettings;
use schemars::transform::{Transform, transform_subschemas};
use serde_json::{Map, Value};

use crate::inference::ResolvedOptions;
use crate::inference::catalog::{ConnectionCatalog, ModelRole};
use crate::platform::devices::Enrolled;
use crate::protocol::stream::StreamDelta;
use crate::protocol::{
    answers, attention, files, projects, sessions, sign_in, state, status, turns,
};
use crate::record::log::LogEntry;
use crate::tools::ToolEntry;
use crate::tools::process_monitor::ProcessRecord;

use crate::routes::entry_view::SelectionLines;
use crate::routes::errors::{ErrorBody, ErrorCode};
use crate::session::artifact_links::Link;

/// The committed schema, relative to the crate root.
const PATH: &str = "web/src/api/protocol.schema.json";

/// The name of the root document, which json-schema-to-typescript gives the one type it
/// declares for it and names in the comment on every other.
const TITLE: &str = "ControlChannel";

/// A schema that says nothing but its prose accepts any JSON value, which is what a
/// documented `serde_json::Value` field comes out as. json-schema-to-typescript reads such
/// a schema as an object with any keys and `{}` or `true` as `unknown`, so the first is
/// marked to read as the second (`tsType` is its own keyword; a validator ignores it).
#[derive(Clone)]
struct AnyValueIsUnknown;

impl Transform for AnyValueIsUnknown {
    fn transform(&mut self, schema: &mut Schema) {
        transform_subschemas(self, schema);
        let Some(object) = schema.as_object_mut() else {
            return;
        };
        let prose_only = !object.is_empty()
            && object
                .keys()
                .all(|key| matches!(key.as_str(), "description" | "title" | "default"));
        if prose_only {
            object.insert("tsType".into(), "unknown".into());
        }
    }
}

macro_rules! roots {
    ($generator:expr; $($ty:ty),+ $(,)?) => {
        $( $generator.subschema_for::<$ty>(); )+
    };
}

/// The schema as the generator emits it now.
fn generate() -> Value {
    let mut generator = SchemaSettings::draft07()
        .for_serialize()
        .with_transform(AnyValueIsUnknown)
        .into_generator();
    roots!(generator;
        LogEntry,
        state::StateTopics,
        state::LoopResource,
        sessions::ListedSession,
        sessions::ArchivedSession,
        sessions::DormantSession,
        sessions::CreateSession,
        sessions::SessionOptionsRequest,
        sessions::PatchSession,
        sessions::CreatedSession,
        sessions::SessionResumed,
        sessions::SessionSupervised,
        sessions::SessionArchived,
        ResolvedOptions,
        ConnectionCatalog,
        ModelRole,
        answers::CatalogList,
        projects::ProjectList,
        projects::ForgottenProject,
        files::DirListing,
        sign_in::SignInRequest,
        sign_in::SignInClaim,
        sign_in::SignInLink,
        sign_in::PendingSignInList,
        sign_in::DeviceList,
        sign_in::RemovedDevice,
        Enrolled,
        answers::InventoryList,
        answers::StatusBody,
        answers::FiledNotification,
        answers::PushKey,
        answers::ProposalResolution,
        answers::LocatedToolCall,
        answers::ProcessOutputRead,
        attention::AttentionReport,
        status::StatusView,
        turns::QueuedTurn,
        turns::TurnAccepted,
        turns::WithdrawnTurns,
        StreamDelta,
        ProcessRecord,
        ToolEntry,
        ErrorBody,
        ErrorCode,
        SelectionLines,
        Link,
    );
    let mut root = Map::new();
    let meta = generator.settings().meta_schema.clone();
    root.insert(
        "$schema".into(),
        meta.map_or(Value::Null, |meta| meta.to_string().into()),
    );
    root.insert("title".into(), TITLE.into());
    root.insert(
        "description".into(),
        "The control channel's types. The root carries no value; the definitions are the schema."
            .into(),
    );
    root.insert("type".into(), "object".into());
    root.insert("additionalProperties".into(), false.into());
    root.insert(
        "definitions".into(),
        Value::Object(generator.take_definitions(true)),
    );
    Value::Object(root)
}

fn render(schema: &Value) -> String {
    let mut text = serde_json::to_string_pretty(schema).expect("a schema serializes");
    text.push('\n');
    text
}

/// The committed schema is what the generator emits now. A change to a type the channel
/// carries moves it and fails here; regenerate with `EFFERENCE_WRITE_SCHEMAS=1 cargo test
/// schema`, then `pnpm run types` in `web/` for the TypeScript.
#[test]
fn the_committed_schema_matches_the_generator() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(PATH);
    let generated = render(&generate());
    if std::env::var_os("EFFERENCE_WRITE_SCHEMAS").is_some() {
        std::fs::write(&path, &generated).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    assert!(
        committed == generated,
        "{} differs from the generated schema; regenerate with \
         EFFERENCE_WRITE_SCHEMAS=1 cargo test schema, then `pnpm run types` in web/",
        path.display()
    );
}

/// schemars gives a second type of an already-used name a numeric suffix, in whichever
/// order the roots reach them, so which of two same-named types keeps the plain name could
/// swap with an unrelated change. Every type has a name of its own instead
/// (`schemars(rename = …)` on the type).
#[test]
fn no_two_types_share_a_name() {
    let schema = generate();
    let names: Vec<&str> = schema["definitions"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let suffixed: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| {
            let (stem, digit) = name.split_at(name.len() - 1);
            digit.chars().all(|c| ('2'..='9').contains(&c)) && names.contains(&stem)
        })
        .collect();
    assert!(
        suffixed.is_empty(),
        "types sharing a name were suffixed: {suffixed:?}"
    );
}
