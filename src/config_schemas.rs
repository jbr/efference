//! The JSON schemas published for the two config files (docs/config-schemas.md):
//! `user` for `~/.efference/config.toml` and `project` for a project's
//! `efference.toml` and the session snapshot of it. Both are generated from the
//! types the files deserialize into, tidied for TOML editors ([`tidy`]), and
//! committed under `assets/schemas/`, where a test pins them to the generator.

use schemars::{JsonSchema, Schema, generate::SchemaSettings};
use serde_json::{Map, Value};

/// Where the committed schemas live, relative to the crate root.
pub const DIR: &str = "assets/schemas";

/// Where efference.net serves [`DIR`]; a schema's `$id`, and the URL a config
/// file's `#:schema` line names.
pub const BASE_URL: &str = "https://efference.net/schemas/";

/// The user configuration's file name under [`DIR`] and [`BASE_URL`].
pub const USER: &str = "user.v1.json";

/// The project configuration's file name under [`DIR`] and [`BASE_URL`].
pub const PROJECT: &str = "project.v1.json";

/// The schema for `~/.efference/config.toml`.
pub fn user() -> Schema {
    generate::<crate::user_config::InferenceConfig>(USER, "efference user configuration")
}

/// The schema for a project's `efference.toml`.
pub fn project() -> Schema {
    generate::<crate::config::RawConfig>(PROJECT, "efference project configuration")
}

/// Each published schema beside its file name.
pub fn all() -> [(&'static str, Schema); 2] {
    [(USER, user()), (PROJECT, project())]
}

/// The comment a config file opens with to name its schema, which Taplo and Tombi
/// read.
pub fn directive(file: &str) -> String {
    format!("#:schema {BASE_URL}{file}")
}

/// `edited` with the header directives `original` opened with (`#:schema …`) back on its
/// first lines, where Taplo and Tombi read them. toml_edit keeps a leading comment as decor
/// on the first item, so an edit that renders a new item ahead of that one — a top-level
/// key before the first table, or the first item of a file that held only comments — would
/// carry the directive down the file.
pub fn keep_directives_first(original: &str, edited: String) -> String {
    let directives: Vec<&str> = original
        .lines()
        .take_while(|line| line.starts_with("#:"))
        .collect();
    if edited
        .lines()
        .take(directives.len())
        .eq(directives.iter().copied())
    {
        return edited;
    }
    let mut rest: Vec<&str> = edited.lines().collect();
    for directive in &directives {
        if let Some(at) = rest.iter().position(|line| line == directive) {
            rest.remove(at);
        }
    }
    let mut text = directives
        .into_iter()
        .chain(rest)
        .collect::<Vec<_>>()
        .join("\n");
    if edited.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// The bytes committed for `schema`: pretty JSON with a trailing newline.
pub fn render(schema: &Schema) -> String {
    let mut text = serde_json::to_string_pretty(schema).expect("a schema serializes");
    text.push('\n');
    text
}

/// A string `enum` with a hover line per value, for a closed vocabulary held as a
/// `String` and checked by hand: the shape [`tidy`] gives a derived enum.
pub fn documented_enum<'a>(values: impl IntoIterator<Item = (&'a str, &'a str)>) -> Schema {
    let (values, docs): (Vec<&str>, Vec<&str>) = values.into_iter().unzip();
    schemars::json_schema!({
        "type": "string",
        "enum": values,
        "x-taplo": { "docs": { "enumValues": docs } },
    })
}

fn generate<T: JsonSchema>(file: &str, title: &str) -> Schema {
    let mut value = SchemaSettings::default()
        .for_deserialize()
        .into_generator()
        .into_root_schema_for::<T>()
        .to_value();
    tidy(&mut value);
    let mut schema = Schema::try_from(value).expect("a tidied schema is still an object");
    schema.insert("$id".into(), format!("{BASE_URL}{file}").into());
    schema.insert("title".into(), title.into());
    schema
}

/// Rewrite what schemars emits into what a TOML editor reads well, at every depth:
///
/// - TOML has no null, so an `Option` field loses the `null` schemars adds to its
///   type and the `default: null` its `#[serde(default)]` produces.
/// - A numeric `format` (`uint64`, `double`) is a Rust width, not a JSON Schema
///   format either editor checks.
/// - An enum whose variants carry docs comes out as a `oneOf` of documented
///   `const`s. A bad value then reads as one failure per variant (Tombi) or as a
///   generic `oneOf` failure (Taplo), and Taplo's hover prints every variant's
///   doc at once. It becomes a plain `enum`, with the variant docs moved to
///   Taplo's `x-taplo.docs.enumValues` (matched by index) for its completion list.
fn tidy(value: &mut Value) {
    match value {
        Value::Object(object) => {
            drop_null(object);
            if matches!(object.get("type"), Some(Value::String(t)) if t == "integer" || t == "number")
            {
                object.remove("format");
            }
            consts_to_enum(object);
            object.values_mut().for_each(tidy);
        }
        Value::Array(items) => items.iter_mut().for_each(tidy),
        _ => {}
    }
}

fn drop_null(object: &mut Map<String, Value>) {
    if object.get("default") == Some(&Value::Null) {
        object.remove("default");
    }
    if let Some(Value::Array(types)) = object.get_mut("type") {
        types.retain(|t| t != "null");
        if let [only] = types.as_slice() {
            let only = only.clone();
            object.insert("type".into(), only);
        }
    }
    if let Some(Value::Array(branches)) = object.get_mut("anyOf") {
        branches.retain(|branch| branch.get("type") != Some(&Value::from("null")));
        if branches.len() == 1 {
            let Some(Value::Object(only)) = branches.pop() else {
                return;
            };
            object.remove("anyOf");
            for (key, value) in only {
                object.entry(key).or_insert(value);
            }
        }
    }
}

fn consts_to_enum(object: &mut Map<String, Value>) {
    let Some(Value::Array(branches)) = object.get("oneOf") else {
        return;
    };
    let only_documented_consts = branches.iter().all(|branch| {
        branch.as_object().is_some_and(|b| {
            b.contains_key("const")
                && b.keys()
                    .all(|k| matches!(k.as_str(), "const" | "type" | "description"))
        })
    });
    if !only_documented_consts {
        return;
    }
    let Some(Value::Array(branches)) = object.remove("oneOf") else {
        return;
    };
    let types: Vec<&Value> = branches.iter().filter_map(|b| b.get("type")).collect();
    if let Some(first) = types.first()
        && types.len() == branches.len()
        && types.iter().all(|t| t == first)
    {
        object.insert("type".into(), (*first).clone());
    }
    let docs: Vec<Value> = branches
        .iter()
        .map(|b| b.get("description").cloned().unwrap_or(Value::Null))
        .collect();
    let values: Vec<Value> = branches
        .into_iter()
        .filter_map(|mut b| b.as_object_mut()?.remove("const"))
        .collect();
    object.insert("enum".into(), values.into());
    if docs.iter().any(|d| !d.is_null()) {
        object.insert(
            "x-taplo".into(),
            serde_json::json!({ "docs": { "enumValues": docs } }),
        );
    }
}

#[cfg(test)]
mod test;
