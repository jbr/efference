//! A project config assembled entry by entry, for a test whose subject is what a config
//! does rather than how its file is written (docs/permissions.md).
//!
//! Each method writes the entry an operator would write into a TOML document — a tool
//! grant, a path, or the scope through [`apply_to_document`], the writer a committed
//! config edit uses — and [`ConfigFixture::build`] reads the document with the parser a
//! config file goes through. A fixture is therefore always a config some file could hold,
//! and a change to how the file spells an entry is made here rather than in each test.

use super::*;

/// The document a fixture config is read from; [`ConfigFixture::build`] parses it.
#[derive(Default)]
pub(crate) struct ConfigFixture {
    doc: DocumentMut,
}

impl ConfigFixture {
    /// An empty file: [`build`](Self::build) alone yields [`Config::default`].
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Mount every tool of `namespace` (`efference` for the built-ins, `agents`, `lsp`),
    /// as `<namespace> = { enabled = true }` does.
    pub(crate) fn with_namespace(mut self, namespace: &str) -> Self {
        let tools = self
            .doc
            .as_table_mut()
            .entry("tools")
            .or_insert(Item::Table(Table::new()));
        match tools.get_mut(namespace) {
            Some(Item::Value(Value::InlineTable(entry))) => {
                entry.insert("enabled", Value::from(true));
            }
            _ => {
                let mut entry = InlineTable::new();
                entry.insert("enabled", Value::from(true));
                tools[namespace] = value(entry);
            }
        }
        self
    }

    /// Grant the tool `wire_name` by name (`read`, `agents__message`), mounting its
    /// namespace as a file must for the grant to load.
    pub(crate) fn with_tool(self, wire_name: &str) -> Self {
        let mut fixture = self.with_namespace(crate::record::names::tools::namespace_of(wire_name));
        let grant = ConfigChange::Tool {
            name: wire_name.into(),
        };
        apply_to_document(&mut fixture.doc, &grant);
        fixture
    }

    /// Grant `read` and `write` on the subtree `key`, relative to the project as in
    /// `[paths]`.
    pub(crate) fn with_path(mut self, key: &str, read: bool, write: bool) -> Self {
        let grant = ConfigChange::Path {
            path: key.into(),
            read,
            write,
        };
        apply_to_document(&mut self.doc, &grant);
        self
    }

    /// Give bash commands starting with `prefix` the stance `stance`.
    pub(crate) fn with_command(mut self, prefix: &str, stance: CommandStance) -> Self {
        self.doc["commands"][prefix] = value(stance.as_str());
        self
    }

    pub(crate) fn with_scope(mut self, text: &str) -> Self {
        let scope = ConfigChange::Scope { text: text.into() };
        apply_to_document(&mut self.doc, &scope);
        self
    }

    /// Add `item` to the end of the spotter's `watch-for` list.
    pub(crate) fn with_watch_for(mut self, item: &str) -> Self {
        let list = self
            .doc
            .as_table_mut()
            .entry("watch-for")
            .or_insert(value(toml_edit::Array::new()));
        list.as_array_mut().expect("watch-for is a list").push(item);
        self
    }

    /// The config the document parses to. Panics, naming the document, when it does not
    /// load: a fixture no file could hold would test a config that cannot exist.
    pub(crate) fn build(self) -> Config {
        let text = self.doc.to_string();
        Config::from_toml(&text).unwrap_or_else(|error| panic!("{error}\n{text}"))
    }
}
