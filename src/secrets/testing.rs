//! A secret store of shell scripts over a temporary directory, for tests of the
//! `[secrets]` command backend and of what runs on it (docs/secrets.md). Each item is
//! a file named for it, and `get` prints the file and then a newline, as most tools
//! print a value. A missing item exits 3, which a table may declare as its
//! `absent_status`.

use std::path::PathBuf;

use super::{SecretCommands, Secrets};

/// A scratch HOME whose user configuration's `[secrets]` table runs the scripts.
pub(crate) struct ScriptStore {
    root: PathBuf,
}

impl ScriptStore {
    /// A store whose table has `get`, `set` and `delete`, and `extra` after them (as in
    /// `absent_status = 3`).
    pub(crate) fn new(label: &str, extra: &str) -> Self {
        use std::hash::{BuildHasher, Hasher, RandomState};
        let n = RandomState::new().build_hasher().finish();
        let root = std::env::temp_dir().join(format!("efference-secrets-{label}-{n:016x}"));
        std::fs::create_dir_all(root.join("items")).unwrap();
        std::fs::create_dir_all(root.join(".efference")).unwrap();
        let store = Self { root };
        store.script(
            "get.sh",
            r#"f="$1/$2"; [ -f "$f" ] || exit 3; cat "$f"; echo"#,
        );
        store.script(
            "set.sh",
            r#"printf '%s\n' "$*" >> "$1.argv"; cat > "$1/$2""#,
        );
        store.script("delete.sh", r#"f="$1/$2"; [ -f "$f" ] || exit 3; rm "$f""#);
        let table = format!(
            "get = {}\nset = {}\ndelete = {}\n{extra}",
            store.argv("get.sh"),
            store.argv("set.sh"),
            store.argv("delete.sh"),
        );
        store.configure(&table);
        store
    }

    /// Replace the `[secrets]` table's body with `table`.
    pub(crate) fn configure(&self, table: &str) {
        std::fs::write(
            self.root.join(".efference/config.toml"),
            format!("[secrets]\n{table}\n"),
        )
        .unwrap();
    }

    /// Write `body` as the script `name`, run by `sh`.
    pub(crate) fn script(&self, name: &str, body: &str) {
        std::fs::write(self.root.join(name), body).unwrap();
    }

    /// The TOML argv that runs the script `name` on the items directory and `{name}`.
    pub(crate) fn argv(&self, name: &str) -> String {
        format!(
            "[\"sh\", {:?}, {:?}, \"{{name}}\"]",
            self.root.join(name).display().to_string(),
            self.items().display().to_string(),
        )
    }

    /// The store the configuration's `[secrets]` table names: the table read from the
    /// file and checked as the user configuration's parse checks it, then handed to
    /// [`Secrets::configured`] as a process hands it.
    pub(crate) fn secrets(&self) -> Secrets {
        #[derive(serde::Deserialize)]
        struct File {
            secrets: SecretCommands,
        }
        let text = std::fs::read_to_string(self.root.join(".efference/config.toml")).unwrap();
        let file: File = toml_edit::de::from_str(&text).unwrap();
        file.secrets.validate().unwrap();
        Secrets::configured(Some(file.secrets))
    }

    /// The directory holding one file per item.
    pub(crate) fn items(&self) -> PathBuf {
        self.root.join("items")
    }

    /// What `set` has written for `name`, read from its file, or `None`.
    pub(crate) fn item(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.items().join(name)).ok()
    }

    /// Each `set`'s arguments, one line per call.
    pub(crate) fn set_arguments(&self) -> String {
        std::fs::read_to_string(self.root.join("items.argv")).unwrap_or_default()
    }
}

impl Drop for ScriptStore {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
